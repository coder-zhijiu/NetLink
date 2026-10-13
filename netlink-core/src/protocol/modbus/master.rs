//! Modbus 主机：TCP 轮询采集客户端 + RTU 串口轮询（`serve_serial`）。
//!
//! TCP 结构：连接循环（断线退避重连 1s→5s→30s 封顶）→ 轮询调度器
//! （单连接串行：到期设备逐个采集；写点/测试帧/扫描命令插队处理）。
//! RTU 结构：独占串口阻塞线程（同 RTU 从机），帧格式 `[地址][PDU][CRC16]`，
//! 以读超时静默间隙判定帧边界；命令通道复用 MasterMsg。
//! 采集应答经 `import_registers/import_bits` 写回设备运行时，
//! 复用从机侧的值推送与报文监视链路（dir 语义反转：out=请求 / in=应答）。
//! 点位扫描（scan 模块的 ScanEngine 状态机）在两条管道上以同构驱动循环执行：
//! 扫描期间轮询天然让路（同一串行调度链）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::scan::{ScanEngine, ScanParams, ScanProgress, ScanResult};
use super::tcp::{pdu_note, pdu_note_resp};
use super::{find_device_id, ConnCtx};
use crate::model::Area;
use crate::model::DataType;
use crate::protocol::FrameEvent;
use crate::runtime::{ConnStateKind, DeviceRuntime};

/// 调度器命令（来自 Manager）
pub enum MasterMsg {
    /// 写点位：按点位类型自动拼 FC05/06/10
    Write {
        device: Arc<Mutex<DeviceRuntime>>,
        point_id: u64,
        value: f64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// 测试帧：直接下发 PDU（不含 MBAP），返回应答 PDU
    TestFrame {
        unit: u8,
        pdu: Vec<u8>,
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    /// 点位扫描：对 unit 暴力探测可读地址（长任务，进度实时写入 progress）
    Scan {
        params: ScanParams,
        /// 用户取消（面板「停止」按钮）
        cancel: CancellationToken,
        progress: Arc<Mutex<ScanProgress>>,
        reply: oneshot::Sender<ScanResult>,
    },
}

const MBAP_LEN: usize = 7;
/// 请求尝试次数：1 次 + 2 次重试
const ATTEMPTS: usize = 3;
/// 单段最大字数（Modbus 读保持/输入寄存器上限 125）
const MAX_WORDS_PER_SEG: usize = 125;

pub async fn serve(
    ctx: Arc<ConnCtx>,
    host: String,
    port: u16,
    timeout_ms: u32,
    cancel: CancellationToken,
    mut rx: mpsc::Receiver<MasterMsg>,
) {
    let devices = ctx.devices.clone();
    let mut backoff = Duration::from_secs(1);

    loop {
        if cancel.is_cancelled() {
            return;
        }
        ctx.manager.set_conn_state(ctx.conn_id, ConnStateKind::Connecting, None);
        let mut stream = match TcpStream::connect((host.as_str(), port)).await {
            Ok(s) => {
                backoff = Duration::from_secs(1);
                ctx.manager.set_conn_state(ctx.conn_id, ConnStateKind::Connected, None);
                s
            }
            Err(e) => {
                let detail = format!("{e}，{}ms 后重试", backoff.as_millis());
                ctx.manager
                    .set_conn_state(ctx.conn_id, ConnStateKind::Reconnecting, Some(detail));
                eprintln!("[netlink] 主机连接 {host}:{port} 失败（{e}），{}ms 后重试", backoff.as_millis());
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 5).min(Duration::from_secs(30));
                continue;
            }
        };

        // 调度器：出错即返回外层重连
        match dispatch(&mut stream, &ctx, &devices, timeout_ms, &mut rx, &cancel).await {
            Ok(()) => return, // 取消
            Err(e) => {
                ctx.manager.set_conn_state(
                    ctx.conn_id,
                    ConnStateKind::Reconnecting,
                    Some(format!("{e}，重连中")),
                );
                eprintln!("[netlink] 主机连接 {host}:{port} 断开（{e}），重连…");
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                }
            }
        }
    }
}

/// 轮询分段：(区域, 功能码, 起始地址, 数量)
type Segment = (Area, u8, u16, u16);

/// 按设备点表计算采集分段：同区域连续地址合并，超长分片
fn compute_segments(layout: &[(u64, Area, u16, DataType)]) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    for &area in &[Area::Coil, Area::DiscreteInput, Area::HoldingRegister, Area::InputRegister] {
        let mut addrs: Vec<(u16, usize)> = layout
            .iter()
            .filter(|(_, a, _, _)| *a == area)
            .map(|(_, _, addr, dt)| (*addr, if area.is_bit() { 1 } else { dt.words() }))
            .collect();
        addrs.sort();
        let mut i = 0;
        while i < addrs.len() {
            // 找从 i 开始的连续段（重叠或相接都合并）
            let (start, slots) = addrs[i];
            let mut end = start as usize + slots;
            let mut j = i + 1;
            while j < addrs.len() && addrs[j].0 as usize <= end {
                end = end.max(addrs[j].0 as usize + addrs[j].1);
                j += 1;
            }
            // 分片
            let mut s = start as usize;
            while s < end {
                let take = (end - s).min(MAX_WORDS_PER_SEG);
                let fc = match area {
                    Area::Coil => 0x01,
                    Area::DiscreteInput => 0x02,
                    Area::HoldingRegister => 0x03,
                    Area::InputRegister => 0x04,
                };
                out.push((area, fc, s as u16, take as u16));
                s += take;
            }
            i = j;
        }
    }
    out
}

struct Dispatcher {
    txn: u16,
}

impl Dispatcher {
    /// 一条请求-应答事务（1 次 + 2 次重试，非扫描帧）；返回应答 PDU
    async fn transact(
        &mut self,
        stream: &mut TcpStream,
        ctx: &ConnCtx,
        unit: u8,
        pdu: &[u8],
        timeout_ms: u32,
    ) -> Result<Vec<u8>, String> {
        self.transact_with(stream, ctx, unit, pdu, timeout_ms, ATTEMPTS, false).await
    }

    /// 同上，参数化尝试次数与扫描标记（扫描事务不重试/探活自带重试，帧带 scan 标志）
    async fn transact_with(
        &mut self,
        stream: &mut TcpStream,
        ctx: &ConnCtx,
        unit: u8,
        pdu: &[u8],
        timeout_ms: u32,
        attempts: usize,
        scan: bool,
    ) -> Result<Vec<u8>, String> {
        let timeout = Duration::from_millis(timeout_ms.max(50) as u64);
        let dev_id = find_device_id(ctx, unit);
        for _ in 0..attempts.max(1) {
            self.txn = self.txn.wrapping_add(1);
            let txn = self.txn;
            let mut frame = Vec::with_capacity(MBAP_LEN + pdu.len());
            frame.extend_from_slice(&txn.to_be_bytes());
            frame.extend_from_slice(&0u16.to_be_bytes());
            frame.extend_from_slice(&((pdu.len() as u16 + 1).to_be_bytes()));
            frame.push(unit);
            frame.extend_from_slice(pdu);

            let mut ev = FrameEvent::new(ctx.conn_id, dev_id, "out", &frame, pdu_note(pdu));
            ev.scan = scan;
            ctx.manager.emit_frame(ev);
            stream.write_all(&frame).await.map_err(|e| format!("写入失败: {e}"))?;

            let read = read_response(stream, txn);
            match tokio::time::timeout(timeout, read).await {
                Ok(Ok(pdu_resp)) => {
                    let mut ev =
                        FrameEvent::new(ctx.conn_id, dev_id, "in", &pdu_resp.1, pdu_note_resp(&pdu_resp.0));
                    ev.scan = scan;
                    ctx.manager.emit_frame(ev);
                    return Ok(pdu_resp.0);
                }
                Ok(Err(e)) => return Err(e), // IO 错误：交给上层重连
                Err(_) => {
                    // 本次超时，重试
                    continue;
                }
            }
        }
        // 重试全部超时：补一条超时标注事件（与从机侧「(超时不响应)」对称）
        let mut ev = FrameEvent::new(
            ctx.conn_id,
            dev_id,
            "in",
            &[],
            Some("(超时无应答)".to_string()),
        );
        ev.scan = scan;
        ctx.manager.emit_frame(ev);
        Err("超时".to_string())
    }
}

/// 应答读取：返回 (PDU, 完整帧)；事务号不匹配时丢弃陈旧帧继续读
async fn read_response(stream: &mut TcpStream, expect_txn: u16) -> Result<(Vec<u8>, Vec<u8>), String> {
    for _ in 0..10 {
        let mut header = [0u8; MBAP_LEN];
        stream.read_exact(&mut header).await.map_err(|e| format!("读取失败: {e}"))?;
        let txn = u16::from_be_bytes([header[0], header[1]]);
        let proto = u16::from_be_bytes([header[2], header[3]]);
        let len = u16::from_be_bytes([header[4], header[5]]) as usize;
        if proto != 0 || len < 2 || len > 260 {
            return Err("应答帧格式非法".to_string());
        }
        let mut resp = vec![0u8; len - 1];
        stream.read_exact(&mut resp).await.map_err(|e| format!("读取失败: {e}"))?;
        if txn != expect_txn {
            continue; // 丢弃陈旧应答
        }
        let mut full = Vec::with_capacity(MBAP_LEN + resp.len());
        full.extend_from_slice(&header);
        full.extend_from_slice(&resp);
        return Ok((resp, full));
    }
    Err("连续 10 帧事务号不匹配".to_string())
}

async fn dispatch(
    stream: &mut TcpStream,
    ctx: &Arc<ConnCtx>,
    devices: &[Arc<Mutex<DeviceRuntime>>],
    timeout_ms: u32,
    rx: &mut mpsc::Receiver<MasterMsg>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let mut d = Dispatcher { txn: 0 };
    // 每设备下次到期时刻
    let mut next_due: Vec<Option<Instant>> = vec![Some(Instant::now()); devices.len()];

    loop {
        let now = Instant::now();
        // 到期设备逐个采集（串行）
        for (i, dev) in devices.iter().enumerate() {
            let due = next_due[i];
            if due.is_some_and(|t| t <= now) {
                let (enabled, period) = {
                    let g = dev.lock().unwrap();
                    (g.enabled, g.poll_period_ms)
                };
                if enabled {
                    poll_device(stream, ctx, dev, timeout_ms, &mut d).await?;
                }
                next_due[i] = Some(now + Duration::from_millis(period.max(50) as u64));
            }
        }

        // 计算最近的到期时刻
        let next = next_due
            .iter()
            .filter_map(|t| *t)
            .min()
            .unwrap_or(now + Duration::from_millis(500));

        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            msg = rx.recv() => {
                match msg {
                    Some(MasterMsg::Write { device, point_id, value, reply }) => {
                        let r = handle_write(stream, ctx, &device, point_id, value, timeout_ms, &mut d).await;
                        let _ = reply.send(r);
                    }
                    Some(MasterMsg::TestFrame { unit, pdu, reply }) => {
                        let r = d.transact(stream, ctx, unit, &pdu, timeout_ms).await;
                        let _ = reply.send(r);
                    }
                    Some(MasterMsg::Scan { params, cancel: scan_cancel, progress, reply }) => {
                        // 扫描期间轮询天然暂停（本 arm 阻塞到扫描结束）；
                        // 连接级取消/链路 IO 错误由驱动循环检测并中止扫描
                        let r = run_scan_tcp(stream, ctx, &params, &progress, &scan_cancel, cancel, timeout_ms, &mut d).await;
                        let _ = reply.send(r);
                    }
                    None => {
                        // 通道关闭（连接停止中）：等取消
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(next)) => {}
        }
    }
}

/// 扫描驱动（TCP）：执行 ScanEngine 产出的每笔读事务。
/// 连接取消/链路错误 → 引擎中止并保留部分结果；IO 级错误不再上抛
/// （连接本身的断线重连由后续轮询触发，扫描侧只负责干净收尾）
async fn run_scan_tcp(
    stream: &mut TcpStream,
    ctx: &Arc<ConnCtx>,
    params: &ScanParams,
    progress: &Arc<Mutex<ScanProgress>>,
    scan_cancel: &CancellationToken,
    conn_cancel: &CancellationToken,
    timeout_ms: u32,
    d: &mut Dispatcher,
) -> ScanResult {
    let mut eng = ScanEngine::new(params, ctx.conn_id, progress.clone(), scan_cancel.clone());
    while let Some(req) = eng.next_request() {
        if conn_cancel.is_cancelled() {
            eng.abort("连接已停止");
            break;
        }
        let pdu = read_pdu(req.fc, req.addr, req.qty);
        let r = d.transact_with(stream, ctx, params.unit, &pdu, timeout_ms, req.attempts, true).await;
        eng.apply(r);
    }
    eng.into_result()
}

/// 拼读请求 PDU：[fc][起始地址 2B][数量 2B]
fn read_pdu(fc: u8, addr: u16, qty: u16) -> Vec<u8> {
    let mut pdu = Vec::with_capacity(5);
    pdu.push(fc);
    pdu.extend_from_slice(&addr.to_be_bytes());
    pdu.extend_from_slice(&qty.to_be_bytes());
    pdu
}

/// 采集一台设备：分段请求 → 应答导入
async fn poll_device(
    stream: &mut TcpStream,
    ctx: &Arc<ConnCtx>,
    dev: &Arc<Mutex<DeviceRuntime>>,
    timeout_ms: u32,
    d: &mut Dispatcher,
) -> Result<(), String> {
    let (unit, layout) = {
        let g = dev.lock().unwrap();
        (g.slave_id, g.point_layout())
    };
    if layout.is_empty() {
        return Ok(());
    }
    let segments = compute_segments(&layout);

    for (area, fc, start, qty) in segments {
        let mut pdu = Vec::with_capacity(5);
        pdu.push(fc);
        pdu.extend_from_slice(&start.to_be_bytes());
        pdu.extend_from_slice(&qty.to_be_bytes());

        let r = d.transact(stream, ctx, unit, &pdu, timeout_ms).await;
        apply_poll_result(dev, area, start, qty, r)?;
    }
    Ok(())
}

/// 解析采集应答并导入设备运行时；返回应答是否正常
/// 采集应答导入设备运行时（TCP 与 RTU 主机共用）；Err = 应答内容异常（原因入采集统计）
fn apply_response(
    dev: &Arc<Mutex<DeviceRuntime>>,
    area: Area,
    start: u16,
    qty: u16,
    resp: &[u8],
) -> Result<(), String> {
    if resp.len() < 2 {
        return Err("应答过短".to_string());
    }
    let fc = resp[0];
    if fc & 0x80 != 0 {
        return Err(format!("异常应答 code=0x{:02X}", resp[1])); // 异常应答
    }
    let count = resp[1] as usize;
    if resp.len() < 2 + count {
        return Err("应答长度不符".to_string());
    }
    let data = &resp[2..2 + count];
    let mut g = dev.lock().unwrap();
    if area.is_bit() {
        let bits: Vec<bool> = (0..qty as usize)
            .map(|i| data.get(i / 8).map(|b| b & (1 << (i % 8)) != 0).unwrap_or(false))
            .collect();
        g.import_bits(area, start, &bits);
    } else {
        let mut words = Vec::with_capacity(count / 2);
        for k in 0..count / 2 {
            let hi = data[k * 2] as u16;
            let lo = data[k * 2 + 1] as u16;
            words.push((hi << 8) | lo);
        }
        g.import_registers(area, start, &words);
    }
    Ok(())
}

/// 按点位类型拼写帧 PDU（FC05/06/10，TCP 与 RTU 主机共用）；
/// 输入区（离散输入/输入寄存器）协议只读，直接拒绝
fn build_write_pdu(area: Area, addr: u16, dt: DataType, value: f64) -> Result<Vec<u8>, String> {
    if !matches!(area, Area::Coil | Area::HoldingRegister) {
        let name = if area.is_bit() { "离散输入" } else { "输入寄存器" };
        return Err(format!("{name}为只读区，仅线圈/保持寄存器可写"));
    }
    if area.is_bit() {
        // FC05 写单线圈：[功能码][地址2B][值2B]，值域 0xFF00(ON) / 0x0000(OFF)
        let on = value >= 0.5;
        return Ok(vec![0x05, (addr >> 8) as u8, addr as u8, if on { 0xFF } else { 0x00 }, 0x00]);
    }
    let words = crate::runtime::encode_words(value, dt);
    if words.len() == 1 {
        // FC06 写单寄存器
        Ok(vec![0x06, (addr >> 8) as u8, addr as u8, (words[0] >> 8) as u8, words[0] as u8])
    } else {
        // FC10 写多寄存器
        let mut p = Vec::with_capacity(6 + words.len() * 2);
        p.push(0x10);
        p.extend_from_slice(&addr.to_be_bytes());
        p.extend_from_slice(&(words.len() as u16).to_be_bytes());
        p.push((words.len() * 2) as u8);
        for w in &words {
            p.extend_from_slice(&w.to_be_bytes());
        }
        Ok(p)
    }
}

/// 写应答导入本地缓存（下轮轮询自然校准）；TCP 与 RTU 主机共用
fn apply_write_response(
    dev: &Arc<Mutex<DeviceRuntime>>,
    area: Area,
    addr: u16,
    dt: DataType,
    value: f64,
    resp: &[u8],
) -> Result<(), String> {
    if !resp.first().is_some_and(|&c| c & 0x80 == 0) {
        return Err("从机返回异常应答".to_string());
    }
    let mut g = dev.lock().unwrap();
    if area.is_bit() {
        g.import_bits(area, addr, &[value >= 0.5]);
    } else {
        let words = crate::runtime::encode_words(value, dt);
        g.import_registers(area, addr, &words);
        g.mark_ok();
    }
    Ok(())
}

/// 采集应答导入设备运行时（TCP 与 RTU 主机共用）。
/// 超时/异常应答只计入采集统计（设备继续轮询）；IO 级错误先记原因再上抛
/// （TCP：触发重连；RTU：串口保持、下一周期重试）。
fn apply_poll_result(
    dev: &Arc<Mutex<DeviceRuntime>>,
    area: Area,
    start: u16,
    qty: u16,
    result: Result<Vec<u8>, String>,
) -> Result<(), String> {
    match result {
        Ok(resp) => {
            let applied = apply_response(dev, area, start, qty, &resp);
            let mut g = dev.lock().unwrap();
            match applied {
                Ok(()) => g.mark_ok(),
                Err(reason) => g.mark_fail(&reason), // 异常应答等
            }
            Ok(())
        }
        Err(e) => {
            let mut g = dev.lock().unwrap();
            if e == "超时" {
                g.mark_timeout();
                g.mark_fail("超时无应答");
                Ok(())
            } else {
                g.mark_fail(&e);
                Err(e) // IO 级错误：交给上层
            }
        }
    }
}

/// 写点位：按类型自动拼帧
async fn handle_write(
    stream: &mut TcpStream,
    ctx: &Arc<ConnCtx>,
    dev: &Arc<Mutex<DeviceRuntime>>,
    point_id: u64,
    value: f64,
    timeout_ms: u32,
    d: &mut Dispatcher,
) -> Result<(), String> {
    let (unit, info) = {
        let g = dev.lock().unwrap();
        (g.slave_id, g.point_info(point_id))
    };
    let (area, addr, dt) = info.ok_or("点位不存在")?;
    let pdu = build_write_pdu(area, addr, dt, value)?;
    let resp = d.transact(stream, ctx, unit, &pdu, timeout_ms).await?;
    apply_write_response(dev, area, addr, dt, value, &resp)
}

// ---------- Modbus RTU 主机（串口轮询） ----------
//
// 帧格式与从机侧一致：`[地址(1)] [PDU(n)] [CRC16低(1)] [CRC16高(1)]`。
// 串口 IO 为阻塞模式跑在 spawn_blocking 线程（同 RTU 从机）；
// 以读超时静默间隙判定帧边界；命令通道复用 MasterMsg（写点/测试帧与 TCP 主机同构）。

/// RTU 主机串口 IO 抽象（read 返回 0 表示读超时无数据；测试用内存实现替换）
pub trait RtuIo: Send {
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()>;
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
}

/// 真实串口适配（读超时已由 open_port 配置为 50ms，超时归一化为 Ok(0)）
struct SerialRtuIo {
    port: Box<dyn serialport::SerialPort>,
}

impl RtuIo for SerialRtuIo {
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        std::io::Write::write_all(&mut self.port, buf)
    }
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::Read;
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e),
        }
    }
}

/// RTU 主机服务：独占串口轮询（阻塞线程内跑主循环）
pub async fn serve_serial(
    ctx: Arc<ConnCtx>,
    port: Box<dyn serialport::SerialPort>,
    timeout_ms: u32,
    cancel: CancellationToken,
    rx: mpsc::Receiver<MasterMsg>,
) {
    serve_rtu_with(ctx, Box::new(SerialRtuIo { port }), timeout_ms, cancel, rx).await
}

/// RTU 主机服务（IO 注入版；集成测试用内存 IO 走同一条真实代码路径）
pub async fn serve_rtu_with(
    ctx: Arc<ConnCtx>,
    mut io: Box<dyn RtuIo>,
    timeout_ms: u32,
    cancel: CancellationToken,
    mut rx: mpsc::Receiver<MasterMsg>,
) {
    // 串口由启动阶段打开（失败即启动失败），进入主循环即视为链路已建立
    ctx.manager.set_conn_state(ctx.conn_id, ConnStateKind::Connected, None);
    let stop_flag = Arc::new(AtomicBool::new(false));

    // 取消令牌 → 置停止标志（唤醒阻塞读）
    let sf = stop_flag.clone();
    let watcher = tokio::spawn(async move {
        cancel.cancelled().await;
        sf.store(true, Ordering::Release);
    });

    let handle = tokio::task::spawn_blocking(move || {
        let mut io = io.as_mut();
        rtu_master_loop(&ctx, io, timeout_ms, &stop_flag, &mut rx);
    });

    let _ = handle.await;
    watcher.abort();
}

/// RTU 主机主循环：到期设备逐个采集 + 非阻塞处理写点/测试帧命令
fn rtu_master_loop(
    ctx: &Arc<ConnCtx>,
    io: &mut dyn RtuIo,
    timeout_ms: u32,
    stop_flag: &AtomicBool,
    rx: &mut mpsc::Receiver<MasterMsg>,
) {
    let devices = ctx.devices.clone();
    let mut next_due: Vec<Option<Instant>> = vec![Some(Instant::now()); devices.len()];

    loop {
        if stop_flag.load(Ordering::Acquire) {
            return;
        }
        let now = Instant::now();
        // 到期设备逐个采集（串行，同一总线）
        for (i, dev) in devices.iter().enumerate() {
            let due = next_due[i];
            if due.is_some_and(|t| t <= now) {
                let (enabled, period) = {
                    let g = dev.lock().unwrap();
                    (g.enabled, g.poll_period_ms)
                };
                if enabled {
                    if let Err(e) = poll_device_rtu(io, ctx, dev, timeout_ms) {
                        eprintln!("[netlink] RTU 主机采集失败: {e}");
                    }
                }
                next_due[i] = Some(now + Duration::from_millis(period.max(50) as u64));
            }
        }

        // 写点/测试帧/扫描命令插队（非阻塞收取）
        while let Ok(msg) = rx.try_recv() {
            match msg {
                MasterMsg::Write { device, point_id, value, reply } => {
                    let r = handle_write_rtu(io, ctx, &device, point_id, value, timeout_ms);
                    let _ = reply.send(r);
                }
                MasterMsg::TestFrame { unit, pdu, reply } => {
                    let r = rtu_transact_retry(io, ctx, unit, &pdu, timeout_ms, ATTEMPTS, false).map(|(p, _)| p);
                    let _ = reply.send(r);
                }
                MasterMsg::Scan { params, cancel: scan_cancel, progress, reply } => {
                    // 扫描期间轮询天然暂停（本分支阻塞到扫描结束）
                    let r = run_scan_rtu(io, ctx, &params, &progress, &scan_cancel, stop_flag, timeout_ms);
                    let _ = reply.send(r);
                }
            }
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 丢弃串口缓冲里的陈旧数据（RTU 无事务号，超时后迟到应答必须清掉）
fn rtu_drain(io: &mut dyn RtuIo) {
    let mut quiet = 0;
    let mut buf = [0u8; 64];
    while quiet < 2 {
        match io.read(&mut buf) {
            Ok(0) => quiet += 1,
            Ok(_) => quiet = 0,
            Err(_) => return,
        }
    }
}

/// 单次 RTU 事务：发 `[addr][pdu][crc]`，以帧间隔收一帧并校验；返回 (应答 PDU, 完整帧)。
/// scan = 点位扫描事务（帧事件带 scan 标志，监视器默认隐藏）
fn rtu_transact(
    io: &mut dyn RtuIo,
    ctx: &ConnCtx,
    unit: u8,
    pdu: &[u8],
    timeout_ms: u32,
    scan: bool,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut frame = Vec::with_capacity(3 + pdu.len());
    frame.push(unit);
    frame.extend_from_slice(pdu);
    let crc = super::rtu::crc16(&frame);
    frame.extend_from_slice(&crc.to_le_bytes());

    let dev_id = find_device_id(ctx, unit);
    let mut ev = FrameEvent::new(ctx.conn_id, dev_id, "out", &frame, pdu_note(pdu));
    ev.scan = scan;
    ctx.manager.emit_frame(ev);
    io.write_all(&frame).map_err(|e| format!("串口写入失败: {e}"))?;

    // 读到静默间隙（一次读超时）即认为帧完整；总时长超限判超时
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(50) as u64);
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    loop {
        if Instant::now() > deadline {
            return Err("超时".to_string());
        }
        let mut b = [0u8; 1];
        match io.read(&mut b) {
            Ok(0) => {
                if !buf.is_empty() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(_) => {
                buf.push(b[0]);
                if buf.len() >= 256 {
                    buf.clear(); // 无意义长流，丢弃重来
                }
            }
            Err(e) => return Err(format!("串口读取失败: {e}")),
        }
    }

    // 解帧：[addr][pdu][crc]
    let n = buf.len();
    if n < 4 {
        return Err("应答帧过短".to_string());
    }
    let addr = buf[0];
    let resp = buf[1..n - 2].to_vec();
    let crc = u16::from_le_bytes([buf[n - 2], buf[n - 1]]);
    if crc != super::rtu::crc16(&buf[..n - 2]) {
        let mut ev = FrameEvent::new(ctx.conn_id, dev_id, "in", &buf, Some("(CRC 校验失败)".to_string()));
        ev.scan = scan;
        ctx.manager.emit_frame(ev);
        return Err("CRC 校验失败".to_string());
    }
    if addr != unit {
        return Err("应答地址不匹配".to_string());
    }
    let mut ev = FrameEvent::new(ctx.conn_id, dev_id, "in", &buf, pdu_note_resp(&resp));
    ev.scan = scan;
    ctx.manager.emit_frame(ev);
    Ok((resp, buf))
}

/// RTU 事务 + 超时重试（attempts 次；IO 级错误直接上抛）
fn rtu_transact_retry(
    io: &mut dyn RtuIo,
    ctx: &ConnCtx,
    unit: u8,
    pdu: &[u8],
    timeout_ms: u32,
    attempts: usize,
    scan: bool,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    for _ in 0..attempts.max(1) {
        rtu_drain(io);
        match rtu_transact(io, ctx, unit, pdu, timeout_ms, scan) {
            Ok(v) => return Ok(v),
            Err(e) if e == "超时" => continue,
            Err(e) => return Err(e),
        }
    }
    // 重试全部超时：补一条超时标注事件（与 TCP 主机一致）
    let mut ev = FrameEvent::new(
        ctx.conn_id,
        find_device_id(ctx, unit),
        "in",
        &[],
        Some("(超时无应答)".to_string()),
    );
    ev.scan = scan;
    ctx.manager.emit_frame(ev);
    Err("超时".to_string())
}

/// 扫描驱动（RTU）：同 TCP 驱动的事务循环，走串口事务层（同步）
fn run_scan_rtu(
    io: &mut dyn RtuIo,
    ctx: &Arc<ConnCtx>,
    params: &ScanParams,
    progress: &Arc<Mutex<ScanProgress>>,
    scan_cancel: &CancellationToken,
    stop_flag: &AtomicBool,
    timeout_ms: u32,
) -> ScanResult {
    let mut eng = ScanEngine::new(params, ctx.conn_id, progress.clone(), scan_cancel.clone());
    while let Some(req) = eng.next_request() {
        if stop_flag.load(Ordering::Acquire) {
            eng.abort("连接已停止");
            break;
        }
        let pdu = read_pdu(req.fc, req.addr, req.qty);
        let r = rtu_transact_retry(io, ctx, params.unit, &pdu, timeout_ms, req.attempts, true).map(|(p, _)| p);
        eng.apply(r);
    }
    eng.into_result()
}

/// 采集一台设备（RTU）：分段请求 → 应答导入
fn poll_device_rtu(
    io: &mut dyn RtuIo,
    ctx: &Arc<ConnCtx>,
    dev: &Arc<Mutex<DeviceRuntime>>,
    timeout_ms: u32,
) -> Result<(), String> {
    let (unit, layout) = {
        let g = dev.lock().unwrap();
        (g.slave_id, g.point_layout())
    };
    if layout.is_empty() {
        return Ok(());
    }
    let segments = compute_segments(&layout);

    for (area, fc, start, qty) in segments {
        let mut pdu = Vec::with_capacity(5);
        pdu.push(fc);
        pdu.extend_from_slice(&start.to_be_bytes());
        pdu.extend_from_slice(&qty.to_be_bytes());

        let r = rtu_transact_retry(io, ctx, unit, &pdu, timeout_ms, ATTEMPTS, false).map(|(p, _)| p);
        apply_poll_result(dev, area, start, qty, r)?;
    }
    Ok(())
}

/// 写点位（RTU）：拼帧共用，事务走串口
fn handle_write_rtu(
    io: &mut dyn RtuIo,
    ctx: &Arc<ConnCtx>,
    dev: &Arc<Mutex<DeviceRuntime>>,
    point_id: u64,
    value: f64,
    timeout_ms: u32,
) -> Result<(), String> {
    let (unit, info) = {
        let g = dev.lock().unwrap();
        (g.slave_id, g.point_info(point_id))
    };
    let (area, addr, dt) = info.ok_or("点位不存在")?;
    let pdu = build_write_pdu(area, addr, dt, value)?;
    let (resp, _) = rtu_transact_retry(io, ctx, unit, &pdu, timeout_ms, ATTEMPTS, false)?;
    apply_write_response(dev, area, addr, dt, value, &resp)
}
