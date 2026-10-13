//! Modbus 主机侧点位自动检索（扫描）。
//!
//! Modbus 协议没有枚举点表的能力，扫描 = 暴力探测 + 从机应答语义推理：
//! - 正常应答 → 整块可读，记录每个槽位的原始值；
//! - 异常码 02（非法地址）→ 块内定界：二分找最长有效前缀，再二分找有效后缀。
//!   从机普遍实现为「请求范围内任一地址非法即整段拒绝」，二分正好适用；
//! - 超时 → 同样走块内二分定界（超时当「失败」收敛——对「读不到就静默」型
//!   从机，二分对单调语义同样成立，有效窗口照样能以 O(log) 剥离出来）；
//!   超时间隔节流地探活：已知会应答的地址直接回读（3 次尝试）；还不知道任何
//!   会应答的地址时先阶梯采样请求块的首/中/尾（各 1 次）。探活有任何应答
//!   （含异常应答）= 设备活着继续扫；探活也无应答 = 判链路/设备故障中止——
//!   不把活设备扫成空表，也不无限等待；
//! - 异常码 03（非法值）→ 多为设备拒绝大数量读：块宽减半重试（自适应到 1）；
//! - 其他异常码 → 该区域剩余地址拒读，跳过（避免逐块刷屏）。
//!
//! 已知限制：夹在无效地址中间、且不触及块边界/不跨块延续的孤立可读段发现不了
//! （块读探测类工具的固有限制；真实装置点表基本是连续窗口，极少踩中）。
//!
//! 引擎为纯状态机（[`ScanEngine`]）：`next_request()` 产出下一笔读事务 →
//! 驱动方（TCP 异步 / RTU 同步）执行事务 → `apply()` 喂回应答。
//! 与 IO 完全解耦，可直接用内存应答器做单元测试。

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::model::Area;

/// 连续多少次超时后做一次探活（节流：死区密集超时时不每次都白等探活的尝试）
const PROBE_EVERY: u32 = 8;

/// 探活状态机：无已知「会应答的地址」时先阶梯采样（块首/块中/块尾），
/// 任何一点有应答即证明设备存活并升级为回读探活
#[derive(Debug, Clone, Copy, PartialEq)]
enum ProbeState {
    None,
    /// 回读已知会应答的地址（3 次尝试）
    LastOk,
    /// 阶梯采样第 idx 档（0/1/2 = 请求块的 首/中/尾，各 1 次尝试）
    Ladder(u8),
}

/// 阶梯第 idx 档的采样地址（基于超时的那个请求范围）
fn ladder_addr(idx: u8, req: &ScanReq) -> u16 {
    let off = match idx {
        0 => 0,
        1 => req.qty as u32 / 2,
        _ => req.qty as u32 - 1,
    };
    (req.addr as u32 + off).min(u16::MAX as u32) as u16
}

// ---------- 扫描参数与结果（与前端 types.ts 同构） ----------

/// 一次扫描的参数（前端下发）
#[derive(Debug, Clone, Deserialize)]
pub struct ScanParams {
    pub unit: u8,
    /// 参与扫描的区域
    pub areas: Vec<Area>,
    /// 起始地址（含）
    pub start_addr: u16,
    /// 结束地址（含）
    pub end_addr: u16,
}

/// 扫描发现的一个可读区间
#[derive(Debug, Clone, Serialize)]
pub struct ScanRange {
    pub area: Area,
    pub start: u16,
    /// 槽位数量（字区 = 寄存器个数；位区 = 位个数）
    pub qty: u32,
    /// 每槽位原始值（寄存器原值；位区 0/1）
    pub values: Vec<u16>,
}

/// 不可读的一块（存疑 / 拒读）
#[derive(Debug, Clone, Serialize)]
pub struct ScanBlock {
    pub area: Area,
    pub start: u16,
    pub qty: u32,
    pub reason: String,
}

/// 扫描结果
#[derive(Debug, Clone, Serialize, Default)]
pub struct ScanResult {
    pub ranges: Vec<ScanRange>,
    /// 超时未应答但设备存活的块（可缩小范围重扫）
    pub suspicious: Vec<ScanBlock>,
    /// 从机明确拒绝的块
    pub rejected: Vec<ScanBlock>,
    /// 中止原因（取消 / 链路故障）；正常完成为 None
    pub aborted: Option<String>,
}

/// 扫描进度（运行时逐连接推送）
#[derive(Debug, Clone, Serialize, Default)]
pub struct ScanProgress {
    pub conn_id: u64,
    pub unit: u8,
    /// 当前扫描的区域；未开始/结束时为 None
    pub area: Option<Area>,
    pub cur_addr: u16,
    /// 已处理槽位数 / 总槽位数
    pub processed: u64,
    pub total: u64,
    pub ranges: u32,
    pub suspicious: u32,
    pub done: bool,
    pub aborted: Option<String>,
}

/// 一次读事务的请求（驱动方按 fc/addr/qty 拼 PDU 执行）
#[derive(Debug, Clone)]
pub struct ScanReq {
    pub fc: u8,
    pub addr: u16,
    pub qty: u16,
    /// 事务尝试次数（探活 3 次；普通探测 1 次——瞬时抖动由探活兜底）
    pub attempts: usize,
}

/// 扫描的区域顺序（进度展示稳定）
const AREA_ORDER: [Area; 4] = [Area::Coil, Area::DiscreteInput, Area::HoldingRegister, Area::InputRegister];

/// 一区一次读的最大槽位数（协议限制：字区 125 / 位区 2000）
fn block_width(area: Area) -> u32 {
    if area.is_bit() { 2000 } else { 125 }
}

fn area_fc(area: Area) -> u8 {
    match area {
        Area::Coil => 0x01,
        Area::DiscreteInput => 0x02,
        Area::HoldingRegister => 0x03,
        Area::InputRegister => 0x04,
    }
}

// ---------- 应答解析 ----------

enum Parsed {
    Values(Vec<u16>),
    Exc(u8),
    Malformed(String),
}

/// 解析一条读应答 PDU（正常 / 异常码 / 畸形）
fn parse_read_response(fc: u8, qty: u32, resp: &[u8]) -> Parsed {
    if resp.len() < 2 {
        return Parsed::Malformed("应答过短".into());
    }
    let rfc = resp[0];
    if rfc & 0x80 != 0 {
        return Parsed::Exc(resp[1]);
    }
    if rfc != fc {
        return Parsed::Malformed(format!("应答功能码 0x{rfc:02X} 与请求不符"));
    }
    let count = resp[1] as usize;
    if resp.len() < 2 + count {
        return Parsed::Malformed("应答长度不符".into());
    }
    let data = &resp[2..2 + count];
    if fc == 0x01 || fc == 0x02 {
        if count < (qty as usize + 7) / 8 {
            return Parsed::Malformed("位应答字节数不足".into());
        }
        let values = (0..qty as usize)
            .map(|i| u16::from(data[i / 8] & (1 << (i % 8)) != 0))
            .collect();
        Parsed::Values(values)
    } else {
        if count < qty as usize * 2 {
            return Parsed::Malformed("应答字节数不足".into());
        }
        let values = (0..qty as usize)
            .map(|i| ((data[i * 2] as u16) << 8) | data[i * 2 + 1] as u16)
            .collect();
        Parsed::Values(values)
    }
}

// ---------- 扫描引擎（纯状态机） ----------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// 步进读整块
    Block,
    /// 块内二分：找最长有效前缀（[cursor, cursor+k) 全可读的最大 k）
    Prefix,
    /// 块内二分：找最小 j 使 [j, block_end) 全可读
    Suffix,
}

/// 扫描引擎。驱动循环：
/// `while let Some(req) = eng.next_request() { let resp = 执行事务(req); eng.apply(resp); }`
/// 结束后 `eng.into_result()` 取结果（进度在内部实时更新到 progress）。
pub struct ScanEngine {
    areas: Vec<Area>,
    area_idx: usize,
    area: Area,
    start: u32,
    end: u32,
    /// 当前区域自适应块宽（异常 03 时减半）
    width: u32,
    cursor: u32,
    /// 已完成区域的槽位累计（进度计算用）
    base: u64,

    phase: Phase,
    /// 当前定界块的右边界（exclusive）
    block_end: u32,
    /// Prefix 二分：最大已知可读 k / 最小已知失败 k
    ok_k: u32,
    fail_k: u32,
    /// Suffix 二分：最大已知失败 j / 最小已知可读 j
    fail_j: u32,
    ok_j: u32,
    /// 最近一次成功读的应答值（定界完成时入账）
    saved: Option<Vec<u16>>,
    /// 本定界块内出现过超时（收敛后把未定界出的空档记为存疑）
    block_had_timeout: bool,

    /// 探活状态（None = 不在探活）
    probe: ProbeState,
    /// 距上次探活的连续超时数（节流）
    to_since_probe: u32,
    /// 最近已知「会应答的地址」(区域, 地址)——探活回读目标（读到数据或异常应答都算）
    probe_addr: Option<(Area, u32)>,

    last_req: ScanReq,
    cancel: CancellationToken,
    progress: Arc<Mutex<ScanProgress>>,
    result: ScanResult,
}

impl ScanEngine {
    pub fn new(params: &ScanParams, conn_id: u64, progress: Arc<Mutex<ScanProgress>>, cancel: CancellationToken) -> Self {
        let areas: Vec<Area> = AREA_ORDER.iter().copied().filter(|a| params.areas.contains(a)).collect();
        let start = params.start_addr as u32;
        let end = (params.end_addr.max(params.start_addr)) as u32;
        let area = areas.first().copied();
        let total = areas.len() as u64 * (end - start + 1) as u64;
        {
            let mut p = progress.lock().unwrap();
            p.conn_id = conn_id;
            p.unit = params.unit;
            p.total = total;
            p.area = area;
        }
        Self {
            area: area.unwrap_or(Area::HoldingRegister),
            areas,
            area_idx: 0,
            start,
            end,
            width: area.map(block_width).unwrap_or(125),
            cursor: start,
            base: 0,
            phase: Phase::Block,
            block_end: 0,
            ok_k: 0,
            fail_k: 0,
            fail_j: 0,
            ok_j: 0,
            saved: None,
            block_had_timeout: false,
            probe: ProbeState::None,
            to_since_probe: 0,
            probe_addr: None,
            last_req: ScanReq { fc: 0x03, addr: 0, qty: 1, attempts: 1 },
            cancel,
            progress,
            result: ScanResult::default(),
        }
    }

    /// 下一笔读事务；None = 扫描结束（完成 / 取消 / 中止）
    pub fn next_request(&mut self) -> Option<ScanReq> {
        loop {
            if self.cancel.is_cancelled() {
                self.result.aborted.get_or_insert_with(|| "已取消".into());
                return None;
            }
            if self.result.aborted.is_some() {
                return None;
            }
            if self.probe != ProbeState::None {
                return Some(match self.probe {
                    ProbeState::LastOk => {
                        let (area, addr) = self.probe_addr.unwrap_or((self.area, self.last_req.addr as u32));
                        ScanReq { fc: area_fc(area), addr: addr as u16, qty: 1, attempts: 3 }
                    }
                    ProbeState::Ladder(idx) => {
                        ScanReq { fc: area_fc(self.area), addr: ladder_addr(idx, &self.last_req), qty: 1, attempts: 1 }
                    }
                    ProbeState::None => unreachable!(),
                });
            }
            match self.phase {
                Phase::Block => {
                    if self.cursor > self.end {
                        if !self.next_area() {
                            return None; // 全部区域完成
                        }
                        continue;
                    }
                    let qty = (self.end - self.cursor + 1).min(self.width);
                    self.last_req = ScanReq {
                        fc: area_fc(self.area),
                        addr: self.cursor as u16,
                        qty: qty as u16,
                        attempts: 1,
                    };
                    return Some(self.last_req.clone());
                }
                Phase::Prefix => {
                    if self.fail_k - self.ok_k <= 1 {
                        // 前缀定界完成：入账 [cursor, cursor+ok_k)，cursor 推进到首个无效地址
                        if self.ok_k > 0 {
                            let values = self.saved.take().unwrap_or_default();
                            record_range(&mut self.result, self.area, self.cursor, self.ok_k, values);
                        }
                        self.cursor += self.ok_k;
                        self.fail_j = self.cursor;
                        self.ok_j = self.block_end;
                        self.saved = None;
                        self.phase = Phase::Suffix;
                        continue;
                    }
                    let mid = (self.ok_k + self.fail_k) / 2;
                    self.last_req = ScanReq {
                        fc: area_fc(self.area),
                        addr: self.cursor as u16,
                        qty: mid as u16,
                        attempts: 1,
                    };
                    return Some(self.last_req.clone());
                }
                Phase::Suffix => {
                    if self.ok_j - self.fail_j <= 1 {
                        // 后缀定界完成：先入账延续到块尾的可读段，再把未定界出的空档记存疑
                        if self.ok_j < self.block_end {
                            let qty = self.block_end - self.ok_j;
                            let values = self.saved.take().unwrap_or_default();
                            record_range(&mut self.result, self.area, self.ok_j, qty, values);
                        }
                        let gap_end = self.ok_j.min(self.block_end);
                        if self.block_had_timeout && gap_end > self.cursor {
                            self.result.suspicious.push(ScanBlock {
                                area: self.area,
                                start: self.cursor as u16,
                                qty: gap_end - self.cursor,
                                reason: "无应答段（超时型从机或混有无效地址）".into(),
                            });
                        }
                        self.cursor = self.block_end;
                        self.saved = None;
                        self.block_had_timeout = false;
                        self.phase = Phase::Block;
                        continue;
                    }
                    let mid = (self.fail_j + self.ok_j) / 2;
                    let qty = self.block_end - mid;
                    self.last_req = ScanReq {
                        fc: area_fc(self.area),
                        addr: mid as u16,
                        qty: qty as u16,
                        attempts: 1,
                    };
                    return Some(self.last_req.clone());
                }
            }
        }
    }

    /// 喂入 `next_request()` 对应事务的应答（PDU 字节 / Err 原因）
    pub fn apply(&mut self, resp: Result<Vec<u8>, String>) {
        if self.probe != ProbeState::None {
            self.to_since_probe = 0;
            match resp {
                Ok(_) => {
                    // 任何应答（含异常码）都证明链路活着；阶梯命中点升级为回读探活目标
                    if let ProbeState::Ladder(idx) = self.probe {
                        self.probe_addr = Some((self.area, ladder_addr(idx, &self.last_req) as u32));
                    }
                    self.probe = ProbeState::None;
                    let req = self.last_req.clone();
                    self.on_timeout_fail(&req);
                }
                Err(e) if e == "超时" => match self.probe {
                    ProbeState::Ladder(idx) if idx < 2 => {
                        self.probe = ProbeState::Ladder(idx + 1);
                    }
                    _ => {
                        self.probe = ProbeState::None;
                        let msg = if self.probe_addr.is_some() {
                            "设备无应答（探活失败），扫描中止".to_string()
                        } else {
                            "从机无应答（采样块首/块中/块尾均无应答；设备离线，或为「无效地址不吭声」型设备——后者请把范围收窄到已知可读地址附近再扫）".to_string()
                        };
                        self.result.aborted = Some(msg);
                    }
                },
                Err(e) => {
                    self.probe = ProbeState::None;
                    self.result.aborted = Some(format!("链路错误: {e}"));
                }
            }
            self.touch_progress();
            return;
        }
        let req = self.last_req.clone();
        match resp {
            Ok(bytes) => match parse_read_response(req.fc, req.qty as u32, &bytes) {
                Parsed::Values(values) => {
                    self.probe_addr = Some((self.area, req.addr as u32));
                    match self.phase {
                        Phase::Block => {
                            record_range(&mut self.result, self.area, self.cursor, req.qty as u32, values);
                            self.cursor += req.qty as u32;
                        }
                        Phase::Prefix => {
                            self.ok_k = req.qty as u32;
                            self.saved = Some(values);
                        }
                        Phase::Suffix => {
                            self.ok_j = req.addr as u32;
                            self.saved = Some(values);
                        }
                    }
                }
                Parsed::Exc(0x02) => match self.phase {
                    Phase::Block => {
                        // 整块含非法地址 → 块内定界
                        self.enter_refine(req.qty as u32);
                    }
                    Phase::Prefix => self.fail_k = req.qty as u32,
                    Phase::Suffix => self.fail_j = req.addr as u32,
                },
                Parsed::Exc(0x03) if self.phase == Phase::Block && self.width > 1 => {
                    // 从机拒绝大数量读（数量超设备上限）：块宽减半重试，不推进不记录
                    self.width = (self.width / 2).max(1);
                }
                Parsed::Exc(code) => match self.phase {
                    Phase::Block => {
                        // 区域级拒绝（功能码不支持 / 从机故障 / 单点读仍拒绝）：
                        // 记一条并跳过该区域剩余地址，避免逐块刷屏
                        let reason = match code {
                            0x01 => "异常码 0x01（功能码不支持），跳过该区域剩余地址".into(),
                            0x03 => "异常码 0x03（拒绝单点读），跳过该区域剩余地址".into(),
                            0x0B => "异常码 0x0B（网关目标设备无应答），跳过该区域剩余地址".into(),
                            _ => format!("异常码 0x{code:02X}，跳过该区域剩余地址"),
                        };
                        self.skip_area_rest(reason);
                    }
                    Phase::Prefix => self.fail_k = req.qty as u32,
                    Phase::Suffix => self.fail_j = req.addr as u32,
                },
                Parsed::Malformed(reason) => match self.phase {
                    Phase::Block => self.skip_area_rest(format!("应答异常（{reason}），跳过该区域剩余地址")),
                    Phase::Prefix => self.fail_k = req.qty as u32,
                    Phase::Suffix => self.fail_j = req.addr as u32,
                },
            },
            Err(e) if e == "超时" => {
                self.to_since_probe += 1;
                // 探活条件：还没有已知会应答的地址（尽快建立探活锚点/暴露死设备），
                // 或距上次探活已累计足够多超时（节流）
                let need_probe = self.probe_addr.is_none() || self.to_since_probe >= PROBE_EVERY;
                if need_probe {
                    self.probe = if self.probe_addr.is_some() { ProbeState::LastOk } else { ProbeState::Ladder(0) };
                } else {
                    self.on_timeout_fail(&req);
                }
            }
            Err(e) => {
                self.result.aborted = Some(format!("链路错误: {e}"));
            }
        }
        self.touch_progress();
    }

    /// 驱动方在外部判定需要中止时调用（连接停止等）
    pub fn abort(&mut self, reason: impl Into<String>) {
        self.result.aborted.get_or_insert_with(|| reason.into());
    }

    /// 结束扫描，返回结果（并给进度收尾）
    pub fn into_result(self) -> ScanResult {
        {
            let mut p = self.progress.lock().unwrap();
            p.done = true;
            p.processed = p.total;
            p.aborted = self.result.aborted.clone();
            p.ranges = self.result.ranges.len() as u32;
            p.suspicious = self.result.suspicious.len() as u32;
        }
        self.result
    }

    /// 超时请求（探活证实设备活着 / 节流跳过探活）后的推进。
    /// 超时在二分语义里当「失败」收敛：块内定界同样能剥离有效窗口。
    fn on_timeout_fail(&mut self, req: &ScanReq) {
        self.block_had_timeout = true;
        match self.phase {
            Phase::Block => {
                if req.qty > 1 {
                    self.enter_refine(req.qty as u32);
                } else {
                    // 单点读超时：设备活着但这个地址读不出来
                    self.result.suspicious.push(ScanBlock {
                        area: self.area,
                        start: req.addr,
                        qty: 1,
                        reason: "超时无应答（设备存活）".into(),
                    });
                    self.cursor += 1;
                }
            }
            Phase::Prefix => self.fail_k = req.qty as u32,
            Phase::Suffix => self.fail_j = req.addr as u32,
        }
    }

    /// 进入块内定界（异常 02 / 块超时共用）
    fn enter_refine(&mut self, qty: u32) {
        self.block_end = self.cursor + qty;
        self.ok_k = 0;
        self.fail_k = qty;
        self.saved = None;
        self.phase = Phase::Prefix;
    }

    fn next_area(&mut self) -> bool {
        self.base += (self.end - self.start + 1) as u64;
        self.area_idx += 1;
        if self.area_idx >= self.areas.len() {
            return false;
        }
        self.area = self.areas[self.area_idx];
        self.cursor = self.start;
        self.width = block_width(self.area);
        self.phase = Phase::Block;
        self.saved = None;
        self.block_had_timeout = false;
        true
    }

    fn skip_area_rest(&mut self, reason: String) {
        let qty = self.end - self.cursor + 1;
        self.result.rejected.push(ScanBlock { area: self.area, start: self.cursor as u16, qty, reason });
        self.cursor = self.end + 1;
    }

    fn touch_progress(&mut self) {
        let mut p = self.progress.lock().unwrap();
        p.area = Some(self.area);
        p.cur_addr = self.cursor.min(u16::MAX as u32) as u16;
        p.processed = self.base + (self.cursor - self.start) as u64;
        p.ranges = self.result.ranges.len() as u32;
        p.suspicious = self.result.suspicious.len() as u32;
    }
}

/// 记录一个可读区间（与上一条同区且首尾相接则合并：跨块延续的有效段）
fn record_range(result: &mut ScanResult, area: Area, start: u32, qty: u32, values: Vec<u16>) {
    if let Some(last) = result.ranges.last_mut() {
        if last.area == area && last.start as u32 + last.qty == start {
            last.qty += qty;
            last.values.extend(values);
            return;
        }
    }
    result.ranges.push(ScanRange { area, start: start as u16, qty, values });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内存从机应答器。valid 为 (区, 起, 止) 可读区间列表。
    /// 对「范围内任一地址非法」的请求：
    /// - mode=Exc02：回异常 02（标准从机）；
    /// - mode=Silent：不应答（「读不到就静默」型，走超时+探活+二分路径）。
    /// 完全可读的请求正常应答（字区回递增值，位区按地址模 3 的位型）。
    struct FakeSlave {
        valid: Vec<(Area, u32, u32)>,
        mode: InvalidMode,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum InvalidMode {
        Exc02,
        Silent,
    }

    impl FakeSlave {
        fn resp(&self, req: &ScanReq) -> Result<Vec<u8>, String> {
            let area = match req.fc {
                0x01 => Area::Coil,
                0x02 => Area::DiscreteInput,
                0x03 => Area::HoldingRegister,
                _ => Area::InputRegister,
            };
            let hit = |addr: u32| self.valid.iter().any(|(a, s, e)| *a == area && addr >= *s && addr < *e);
            let (s, e) = (req.addr as u32, req.addr as u32 + req.qty as u32);
            if (s..e).all(hit) {
                if area.is_bit() {
                    let mut data = vec![0u8; (req.qty as usize + 7) / 8];
                    for i in 0..req.qty as usize {
                        if (s + i as u32) % 3 == 0 {
                            data[i / 8] |= 1 << (i % 8);
                        }
                    }
                    let mut out = vec![req.fc, data.len() as u8];
                    out.extend(data);
                    Ok(out)
                } else {
                    let mut out = vec![req.fc, (req.qty as usize * 2) as u8];
                    for i in 0..req.qty as usize {
                        let w: u16 = ((s + i as u32) % 65536) as u16;
                        out.extend_from_slice(&w.to_be_bytes());
                    }
                    Ok(out)
                }
            } else if self.mode == InvalidMode::Exc02 {
                Ok(vec![req.fc | 0x80, 0x02])
            } else {
                Err("超时".into())
            }
        }
    }

    fn params(areas: &[Area], start: u16, end: u16) -> ScanParams {
        ScanParams { unit: 1, areas: areas.to_vec(), start_addr: start, end_addr: end }
    }

    fn run(slave: &FakeSlave, params: &ScanParams) -> ScanResult {
        let progress = Arc::new(Mutex::new(ScanProgress::default()));
        let cancel = CancellationToken::new();
        let mut eng = ScanEngine::new(params, 1, progress, cancel);
        while let Some(req) = eng.next_request() {
            eng.apply(slave.resp(&req));
        }
        eng.into_result()
    }

    fn holding(a: u32, b: u32) -> (Area, u32, u32) {
        (Area::HoldingRegister, a, b)
    }

    /// 异常 02 型：有效窗口 [0, 30) 触块首，前缀定界直接命中
    #[test]
    fn exc02_prefix_window_from_zero() {
        let slave = FakeSlave { valid: vec![holding(0, 30)], mode: InvalidMode::Exc02 };
        let r = run(&slave, &params(&[Area::HoldingRegister], 0, 99));
        assert!(r.aborted.is_none());
        assert_eq!(r.suspicious.len(), 0);
        assert_eq!(r.rejected.len(), 0);
        assert_eq!(r.ranges.len(), 1);
        let rg = &r.ranges[0];
        assert_eq!((rg.area, rg.start, rg.qty), (Area::HoldingRegister, 0, 30));
        assert_eq!(rg.values.len(), 30);
        assert_eq!(rg.values[0], 0);
        assert_eq!(rg.values[29], 29);
    }

    /// 异常 02 型：窗口跨多个块，块读命中 + 尾块前缀定界 + 跨块区间合并
    #[test]
    fn exc02_window_spanning_blocks_merges() {
        let slave = FakeSlave { valid: vec![holding(0, 300)], mode: InvalidMode::Exc02 };
        let r = run(&slave, &params(&[Area::HoldingRegister], 0, 999));
        assert_eq!(r.ranges.len(), 1);
        assert_eq!((r.ranges[0].start, r.ranges[0].qty), (0, 300));
        assert_eq!(r.ranges[0].values.len(), 300);
        // 跨块合并后值连续
        assert_eq!(r.ranges[0].values[124], 124);
        assert_eq!(r.ranges[0].values[125], 125);
        assert_eq!(r.ranges[0].values[299], 299);
    }

    /// 异常 02 型：窗口起于块中间、终于块尾——前缀为 0，后缀定界命中
    #[test]
    fn exc02_suffix_window_to_block_end() {
        let slave = FakeSlave { valid: vec![holding(50, 125)], mode: InvalidMode::Exc02 };
        let r = run(&slave, &params(&[Area::HoldingRegister], 0, 124));
        assert_eq!(r.ranges.len(), 1);
        assert_eq!((r.ranges[0].start, r.ranges[0].qty), (50, 75));
    }

    /// 位区（线圈）扫描：位值解包 0/1
    #[test]
    fn exc02_bit_area_scan() {
        let slave = FakeSlave { valid: vec![(Area::Coil, 0, 10)], mode: InvalidMode::Exc02 };
        let r = run(&slave, &params(&[Area::Coil], 0, 99));
        assert_eq!(r.ranges.len(), 1);
        let rg = &r.ranges[0];
        assert_eq!((rg.area, rg.start, rg.qty), (Area::Coil, 0, 10));
        assert_eq!(rg.values.len(), 10);
        assert_eq!(rg.values[0], 1); // addr 0 % 3 == 0 → true
        assert_eq!(rg.values[1], 0);
        assert_eq!(rg.values[3], 1);
    }

    /// 多区域依次扫描；空区（异常 02 全拒）无区间产出、无存疑
    #[test]
    fn exc02_multiple_areas() {
        let slave = FakeSlave {
            valid: vec![holding(0, 5), (Area::InputRegister, 0, 4)],
            mode: InvalidMode::Exc02,
        };
        let r = run(&slave, &params(&[Area::Coil, Area::DiscreteInput, Area::HoldingRegister, Area::InputRegister], 0, 99));
        assert!(r.aborted.is_none());
        let holding_ranges: Vec<_> = r.ranges.iter().filter(|x| x.area == Area::HoldingRegister).collect();
        let input_ranges: Vec<_> = r.ranges.iter().filter(|x| x.area == Area::InputRegister).collect();
        assert_eq!(holding_ranges.len(), 1);
        assert_eq!((holding_ranges[0].start, holding_ranges[0].qty), (0, 5));
        assert_eq!(input_ranges.len(), 1);
        assert_eq!((input_ranges[0].start, input_ranges[0].qty), (0, 4));
        assert_eq!(r.suspicious.len(), 0);
        assert_eq!(r.rejected.len(), 0);
    }

    /// 已知限制（文档声明）：夹在无效地址中间、不触块首也不延续到块尾的
    /// 孤立可读段，块读二分定界发现不了（真实装置点表基本是连续窗口）
    #[test]
    fn known_limitation_isolated_island_missed() {
        let slave = FakeSlave { valid: vec![holding(8, 12)], mode: InvalidMode::Exc02 };
        let r = run(&slave, &params(&[Area::HoldingRegister], 0, 99));
        assert!(r.ranges.is_empty());
    }

    /// 超时型（读不到就静默）：窗口 [0,20) 被二分剥离，静默段收敛为一条存疑
    #[test]
    fn silent_device_refines_window_and_marks_gap() {
        let slave = FakeSlave { valid: vec![holding(0, 20)], mode: InvalidMode::Silent };
        let r = run(&slave, &params(&[Area::HoldingRegister], 0, 99));
        assert!(r.aborted.is_none(), "设备活着不应中止: {:?}", r.aborted);
        assert_eq!(r.ranges.len(), 1);
        assert_eq!((r.ranges[0].start, r.ranges[0].qty), (0, 20));
        assert_eq!(r.ranges[0].values.len(), 20);
        // 静默段 [20, 100) 收敛为一条存疑（无重叠记录）
        assert_eq!(r.suspicious.len(), 1);
        assert_eq!((r.suspicious[0].start, r.suspicious[0].qty), (20, 80));
    }

    /// 超时型：窗口不在块首（[50, 125)）——前缀二分照样剥离（超时当失败收敛）
    #[test]
    fn silent_device_window_mid_block() {
        let slave = FakeSlave { valid: vec![holding(50, 125)], mode: InvalidMode::Silent };
        let r = run(&slave, &params(&[Area::HoldingRegister], 0, 124));
        assert!(r.aborted.is_none(), "{:?}", r.aborted);
        assert_eq!(r.ranges.len(), 1);
        assert_eq!((r.ranges[0].start, r.ranges[0].qty), (50, 75));
        // 块首 [0, 50) 静默 → 存疑
        assert_eq!(r.suspicious.len(), 1);
        assert_eq!((r.suspicious[0].start, r.suspicious[0].qty), (0, 50));
    }

    /// 设备整体无应答：首个块超时 → 探活也无应答 → 中止并给出收窄建议
    #[test]
    fn dead_device_aborts_with_hint() {
        let slave = FakeSlave { valid: vec![], mode: InvalidMode::Silent };
        let r = run(&slave, &params(&[Area::HoldingRegister], 0, 99));
        assert!(r.aborted.is_some());
        let msg = r.aborted.unwrap();
        assert!(msg.contains("无应答"), "中止原因应含无应答: {msg}");
        assert!(msg.contains("收窄"), "应给出收窄建议: {msg}");
        assert!(r.ranges.is_empty());
        assert!(r.suspicious.is_empty());
    }

    /// 取消：下一笔请求前检测到取消 → 中止原因「已取消」
    #[test]
    fn cancel_aborts() {
        let slave = FakeSlave { valid: vec![holding(0, 100)], mode: InvalidMode::Exc02 };
        let progress = Arc::new(Mutex::new(ScanProgress::default()));
        let cancel = CancellationToken::new();
        let mut eng = ScanEngine::new(&params(&[Area::HoldingRegister], 0, 99), 1, progress, cancel.clone());
        let req = eng.next_request().unwrap();
        cancel.cancel();
        eng.apply(slave.resp(&req));
        assert!(eng.next_request().is_none());
        let r = eng.into_result();
        assert_eq!(r.aborted.as_deref(), Some("已取消"));
    }

    /// 区域级拒绝（异常 01：功能码不支持）：记一条拒读并跳过该区域，其余区域正常
    #[test]
    fn area_level_rejection() {
        struct Mixed;
        impl Mixed {
            fn resp(&self, req: &ScanReq) -> Result<Vec<u8>, String> {
                if req.fc == 0x01 {
                    return Ok(vec![0x81, 0x01]);
                }
                // 保持区 [0,5) 可读
                if req.addr == 0 && req.qty <= 5 {
                    let mut out = vec![req.fc, (req.qty as usize * 2) as u8];
                    for i in 0..req.qty as u16 {
                        out.extend_from_slice(&i.to_be_bytes());
                    }
                    Ok(out)
                } else {
                    Ok(vec![req.fc | 0x80, 0x02])
                }
            }
        }
        let m = Mixed;
        let progress = Arc::new(Mutex::new(ScanProgress::default()));
        let cancel = CancellationToken::new();
        let params = params(&[Area::Coil, Area::HoldingRegister], 0, 99);
        let mut eng = ScanEngine::new(&params, 1, progress, cancel);
        while let Some(req) = eng.next_request() {
            eng.apply(m.resp(&req));
        }
        let r = eng.into_result();
        assert!(r.aborted.is_none());
        assert_eq!(r.ranges.len(), 1);
        assert_eq!((r.ranges[0].area, r.ranges[0].start, r.ranges[0].qty), (Area::HoldingRegister, 0, 5));
        assert_eq!(r.rejected.len(), 1);
        assert!(r.rejected[0].reason.contains("0x01"));
        assert_eq!(r.rejected[0].area, Area::Coil);
    }

    /// 收窄范围：只扫 [10, 50)
    #[test]
    fn narrowed_range() {
        let slave = FakeSlave { valid: vec![holding(0, 100)], mode: InvalidMode::Exc02 };
        let r = run(&slave, &params(&[Area::HoldingRegister], 10, 50));
        assert_eq!(r.ranges.len(), 1);
        assert_eq!((r.ranges[0].start, r.ranges[0].qty), (10, 41));
        assert_eq!(r.ranges[0].values[0], 10);
    }

    /// 进度：total 预置、完成后 processed 收口到 total、区间计数入进度
    #[test]
    fn progress_tracks_scan() {
        let slave = FakeSlave { valid: vec![holding(0, 30)], mode: InvalidMode::Exc02 };
        let progress = Arc::new(Mutex::new(ScanProgress::default()));
        let cancel = CancellationToken::new();
        let params = params(&[Area::HoldingRegister, Area::InputRegister], 0, 99);
        let mut eng = ScanEngine::new(&params, 7, progress.clone(), cancel);
        {
            let p = progress.lock().unwrap();
            assert_eq!(p.total, 200);
            assert_eq!(p.conn_id, 7);
            assert_eq!(p.unit, 1);
            assert!(!p.done);
        }
        while let Some(req) = eng.next_request() {
            eng.apply(slave.resp(&req));
        }
        let r = eng.into_result();
        assert!(r.aborted.is_none());
        let p = progress.lock().unwrap();
        assert!(p.done);
        assert_eq!(p.processed, p.total);
        assert_eq!(p.ranges, 1);
        assert_eq!(p.suspicious, 0);
        assert_eq!(p.aborted, None);
    }
}
