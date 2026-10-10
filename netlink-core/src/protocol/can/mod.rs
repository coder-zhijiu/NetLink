//! CAN 引擎：帧抽象（CanFrame）+ IO 抽象（CanIo）+ 直采直控主循环。
//!
//! 主循环平移 RTU 主机模式：spawn_blocking 单线程（CAN 单通道本就串行）——
//! ① 到期周期报文逐条发送 ② CanIo.recv 兼作调度 tick（等待时长收敛到最近到期帧，
//! 空闲时 50ms 一拍）③ 命令通道非阻塞收取（手动发送插队）④ 每 500ms 聚合采样
//! （接收聚合 + 帧率滑动窗口）。后端差异压在 CanIo 一层（slcan 已实现；
//! zlgcan/candle/can_net 待硬件到货）。接收聚合（CanAgg）是运行时状态，
//! 不进工程文件，经 RuntimeManager 快照推送前端。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::model::CanNode;
use crate::protocol::FrameEvent;
use crate::protocol::hex_str;
use crate::runtime::{now_ms, ConnStateKind, RuntimeManager};

pub mod slcan;
pub mod zlg_canet;

/// 一条 CAN 经典帧
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct CanFrame {
    pub id: u32,
    pub extended: bool,
    pub remote: bool,
    pub data: Vec<u8>,
}

impl CanFrame {
    /// 监视器 note：ID 按域宽补零 + DLC（扩展/远程标注）
    pub fn note(&self) -> String {
        let width = if self.extended { 8 } else { 3 };
        let mut s = format!("ID=0x{:0width$X} DLC={}", self.id, self.data.len(), width = width);
        if self.remote {
            s.push_str(" 远程帧");
        }
        s
    }
}

/// 后端总线状态摘要（错误计数 / bus-off；slcan 无硬件计数恒零，zlgcan 用）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CanBusStatus {
    pub tx_err: u16,
    pub rx_err: u16,
    pub bus_off: bool,
}

/// CAN IO 抽象：直连（USB-CAN）与转以太网后端共用（测试用内存实现替换）
pub trait CanIo: Send {
    fn send(&mut self, f: &CanFrame) -> Result<(), String>;
    /// None = 读超时静默（超时兼作调度 tick）
    fn recv(&mut self, timeout: Duration) -> Result<Option<CanFrame>, String>;
    fn status(&mut self) -> CanBusStatus;
}

// ---------- 节点运行时 ----------

/// 单条周期报文的运行时定义（快照，改定义走连接重启）
#[derive(Debug, Clone)]
pub struct CanFrameDef {
    pub frame_id: u32,
    pub extended: bool,
    pub remote: bool,
    pub data: Vec<u8>,
    pub period_ms: u32,
    pub enabled: bool,
}

/// 单个 CAN 节点的运行时（启停在线可变；帧定义不可变）
pub struct CanNodeRuntime {
    pub id: u64,
    pub enabled: bool,
    pub frames: Vec<CanFrameDef>,
}

impl CanNodeRuntime {
    pub fn new(node: &CanNode) -> Self {
        Self {
            id: node.id,
            enabled: node.enabled,
            frames: node
                .frames
                .iter()
                .map(|f| CanFrameDef {
                    frame_id: f.frame_id,
                    extended: f.extended,
                    remote: f.remote,
                    data: f.data.clone(),
                    period_ms: f.period_ms,
                    enabled: f.enabled,
                })
                .collect(),
        }
    }
}

// ---------- 接收聚合（运行时状态，不进工程文件） ----------

/// 接收聚合行（ID 聚合视图）：同 (frame_id, extended) 合并计数与平均周期
#[derive(Debug, Clone, Serialize)]
pub struct CanRxStat {
    pub frame_id: u32,
    pub extended: bool,
    pub count: u64,
    /// 平均到达间隔（毫秒）；不足两个间隔时为 0
    pub avg_period_ms: f64,
    /// 最近一次数据域（大写空格 hex；远程帧为空串）
    pub last_data: String,
    /// 最近一次到达（epoch 毫秒）
    pub last_ts: u64,
}

/// 一条 CAN 连接的运行时观测（接收聚合 + 帧计数 + 帧率滑动窗口）
#[derive(Default)]
pub struct CanAgg {
    rows: Vec<RxRow>,
    pub tx_total: u64,
    pub rx_total: u64,
    pub err_total: u64,
    /// 帧率窗口采样：(时刻, tx 累计, rx 累计)，约 4s
    samples: VecDeque<(Instant, u64, u64)>,
}

struct RxRow {
    stat: CanRxStat,
    sum_ms: f64,
    gaps: u64,
}

/// 帧率统计窗口
const RATE_WINDOW: Duration = Duration::from_secs(4);

impl CanAgg {
    /// 收到一帧：聚合计数与间隔
    fn on_rx(&mut self, f: &CanFrame, ts_ms: u64) {
        self.rx_total += 1;
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|r| r.stat.frame_id == f.id && r.stat.extended == f.extended)
        {
            let gap = ts_ms.saturating_sub(row.stat.last_ts) as f64;
            row.sum_ms += gap;
            row.gaps += 1;
            row.stat.avg_period_ms = row.sum_ms / row.gaps.max(1) as f64;
            row.stat.count += 1;
            row.stat.last_data = hex_str(&f.data);
            row.stat.last_ts = ts_ms;
        } else {
            self.rows.push(RxRow {
                stat: CanRxStat {
                    frame_id: f.id,
                    extended: f.extended,
                    count: 1,
                    avg_period_ms: 0.0,
                    last_data: hex_str(&f.data),
                    last_ts: ts_ms,
                },
                sum_ms: 0.0,
                gaps: 0,
            });
        }
    }

    /// 帧率窗口采样（主循环每 500ms 调一次）
    fn sample(&mut self) {
        let now = Instant::now();
        self.samples.push_back((now, self.tx_total, self.rx_total));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > RATE_WINDOW {
            self.samples.pop_front();
        }
    }

    /// 快照（含窗口帧率；聚合行按 ID 排序便于对照）
    pub fn snapshot(&self) -> CanAggSnapshot {
        let mut rows: Vec<CanRxStat> = self.rows.iter().map(|r| r.stat.clone()).collect();
        rows.sort_by(|a, b| a.frame_id.cmp(&b.frame_id).then(a.extended.cmp(&b.extended)));
        let (tx_rate, rx_rate) = match (self.samples.front(), self.samples.back()) {
            (Some(&(t0, tx0, rx0)), Some(&(t1, tx1, rx1))) if t1 > t0 => {
                let secs = t1.duration_since(t0).as_secs_f64();
                ((tx1 - tx0) as f64 / secs, (rx1 - rx0) as f64 / secs)
            }
            _ => (0.0, 0.0),
        };
        CanAggSnapshot {
            rows,
            tx_total: self.tx_total,
            rx_total: self.rx_total,
            err_total: self.err_total,
            tx_rate,
            rx_rate,
        }
    }
}

/// CAN 连接观测快照（推送前端 ID 聚合视图 / 徽章帧率）
#[derive(Debug, Clone, Serialize)]
pub struct CanAggSnapshot {
    pub rows: Vec<CanRxStat>,
    pub tx_total: u64,
    pub rx_total: u64,
    pub err_total: u64,
    /// 发送帧率（帧/秒，滑动窗口）
    pub tx_rate: f64,
    /// 接收帧率（帧/秒，滑动窗口）
    pub rx_rate: f64,
}

// ---------- 命令通道 ----------

/// 调度器命令（来自 Manager）
pub enum CanMsg {
    /// 手动发送一帧（插队，立即发出）
    SendFrame {
        frame: CanFrame,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// 一条 CAN 连接的共享上下文
pub struct CanCtx {
    pub conn_id: u64,
    pub nodes: Vec<Arc<Mutex<CanNodeRuntime>>>,
    pub manager: RuntimeManager,
    pub agg: Arc<Mutex<CanAgg>>,
}

// ---------- 主循环 ----------

/// CAN 主机服务（IO 注入版；集成测试用内存 IO 走同一条真实代码路径）
pub async fn serve_can_with(
    ctx: Arc<CanCtx>,
    io: Box<dyn CanIo>,
    cancel: CancellationToken,
    mut rx: mpsc::Receiver<CanMsg>,
) {
    // 设备/串口由启动阶段打开（失败即启动失败），进入主循环即视为链路已建立
    ctx.manager.set_conn_state(ctx.conn_id, ConnStateKind::Connected, None);
    let stop_flag = Arc::new(AtomicBool::new(false));

    // 取消令牌 → 置停止标志（唤醒阻塞读）
    let sf = stop_flag.clone();
    let watcher = tokio::spawn(async move {
        cancel.cancelled().await;
        sf.store(true, Ordering::Release);
    });

    let handle = tokio::task::spawn_blocking(move || {
        let mut io = io;
        let _ = can_loop(&ctx, io.as_mut(), &stop_flag, &mut rx, false);
    });

    let _ = handle.await;
    watcher.abort();
}

/// 周期报文的调度条目（与节点运行时快照对齐）
struct PeriodicSlot {
    node_id: u64,
    def: CanFrameDef,
    next_due: Instant,
}

/// CAN 主循环：到期周期报文发送 + 收帧聚合 + 命令插队 + 500ms 统计采样。
/// `fatal_io = true` 时 IO 错误直接上抛（CanNet 用：断线交给外层重连）；
/// false 时记链路徽章原地重试（slcan 用：串口无连接概念）。
pub(crate) fn can_loop(
    ctx: &Arc<CanCtx>,
    io: &mut dyn CanIo,
    stop_flag: &AtomicBool,
    rx: &mut mpsc::Receiver<CanMsg>,
    fatal_io: bool,
) -> Result<(), String> {
    // 节点在线状态查表（启停不重启，同 Modbus 设备）
    let node_map: std::collections::HashMap<u64, Arc<Mutex<CanNodeRuntime>>> = ctx
        .nodes
        .iter()
        .map(|n| (n.lock().unwrap().id, n.clone()))
        .collect();
    // 周期报文快照（帧定义改动走连接重启）
    let mut periodic: Vec<PeriodicSlot> = ctx
        .nodes
        .iter()
        .flat_map(|n| {
            let g = n.lock().unwrap();
            let node_id = g.id;
            g.frames
                .iter()
                .filter(|f| f.period_ms > 0)
                .map(|f| PeriodicSlot { node_id, def: f.clone(), next_due: Instant::now() })
                .collect::<Vec<_>>()
        })
        .collect();

    let mut last_sample = Instant::now();
    let mut link_error: Option<String> = None;

    loop {
        if stop_flag.load(Ordering::Acquire) {
            return Ok(());
        }
        let now = Instant::now();

        // ① 到期周期报文逐条发送（停用节点/停用报文跳过，但保持各自节拍）
        for slot in periodic.iter_mut() {
            if slot.next_due > now || !slot.def.enabled {
                continue;
            }
            let node_on = node_map.get(&slot.node_id).is_some_and(|n| n.lock().unwrap().enabled);
            if node_on {
                let frame = CanFrame {
                    id: slot.def.frame_id,
                    extended: slot.def.extended,
                    remote: slot.def.remote,
                    data: slot.def.data.clone(),
                };
                match send_frame(ctx, io, &frame, Some(slot.node_id)) {
                    Ok(()) => clear_link_error(ctx, &mut link_error),
                    Err(e) => {
                        if fatal_io {
                            return Err(e);
                        }
                        mark_link_error(ctx, &mut link_error, e);
                    }
                }
            }
            slot.next_due = now + Duration::from_millis(slot.def.period_ms.max(10) as u64);
        }

        // ② 收帧：等待时长收敛到最近到期帧（空闲 50ms 一拍兼作调度 tick）
        let wait = periodic
            .iter()
            .filter(|s| s.def.enabled && s.next_due > now)
            .map(|s| s.next_due - now)
            .min()
            .unwrap_or(Duration::from_millis(50))
            .min(Duration::from_millis(50));
        match io.recv(wait) {
            Ok(Some(f)) => {
                ctx.manager.emit_frame(FrameEvent::new(
                    ctx.conn_id,
                    None,
                    "in",
                    &f.data,
                    Some(f.note()),
                ));
                ctx.agg.lock().unwrap().on_rx(&f, now_ms());
                clear_link_error(ctx, &mut link_error);
            }
            Ok(None) => {}
            Err(e) => {
                if fatal_io {
                    return Err(e);
                }
                mark_link_error(ctx, &mut link_error, e);
                // 错误限速：持久故障（串口被拔等）立即返回会空转烧 CPU
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        // ③ 命令插队（非阻塞收取）
        while let Ok(msg) = rx.try_recv() {
            match msg {
                CanMsg::SendFrame { frame, reply } => {
                    let r = send_frame(ctx, io, &frame, None);
                    let _ = reply.send(r.clone());
                    // 命令级 IO 失败同样触发断线重连（应答已交给调用方）
                    if fatal_io {
                        r?;
                    }
                }
            }
        }

        // ④ 每 500ms 聚合采样（帧率窗口 + 总线状态）
        if last_sample.elapsed() >= Duration::from_millis(500) {
            last_sample = Instant::now();
            ctx.agg.lock().unwrap().sample();
            let st = io.status();
            if st.bus_off {
                mark_link_error(ctx, &mut link_error, "bus-off（总线关闭）".to_string());
            }
        }
    }
}

/// 发送一帧（io + 出帧事件 + 计数）
fn send_frame(
    ctx: &CanCtx,
    io: &mut dyn CanIo,
    frame: &CanFrame,
    device_id: Option<u64>,
) -> Result<(), String> {
    io.send(frame)?;
    {
        let mut agg = ctx.agg.lock().unwrap();
        agg.tx_total += 1;
    }
    ctx.manager
        .emit_frame(FrameEvent::new(ctx.conn_id, device_id, "out", &frame.data, Some(frame.note())));
    Ok(())
}

/// 链路错误：首个错误置徽章（Reconnecting + 原因），重复错误不刷屏
fn mark_link_error(ctx: &CanCtx, link_error: &mut Option<String>, e: String) {
    ctx.agg.lock().unwrap().err_total += 1;
    if link_error.is_none() {
        eprintln!("[netlink] CAN 连接 {} IO 错误: {e}", ctx.conn_id);
        ctx.manager
            .set_conn_state(ctx.conn_id, ConnStateKind::Reconnecting, Some(format!("{e}，持续重试")));
        *link_error = Some(e);
    }
}

/// 链路恢复：清徽章回 Connected
fn clear_link_error(ctx: &CanCtx, link_error: &mut Option<String>) {
    if link_error.take().is_some() {
        ctx.manager.set_conn_state(ctx.conn_id, ConnStateKind::Connected, None);
    }
}
