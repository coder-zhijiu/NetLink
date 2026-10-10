//! 运行时：仿真时钟、设备状态缓存、寄存器块编码、连接任务管理。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use serde::Serialize;

use crate::model::{Area, Connection, DataType, Device, FaultConfig, ProjectSpec, Transport, ValueSource};
use crate::protocol::FrameEvent;
use crate::protocol::can::{CanAgg, CanAggSnapshot, CanMsg, CanNodeRuntime};
use crate::protocol::modbus::master::{self, MasterMsg};
use crate::sim;

/// 设备仿真 tick 周期
pub const TICK_INTERVAL: Duration = Duration::from_millis(250);
/// 帧广播通道容量
const FRAME_CHANNEL_CAP: usize = 2048;

// ---------- 仿真时钟（暂停时冻结） ----------

#[derive(Debug, Clone)]
pub struct SimClock {
    inner: Arc<Mutex<ClockInner>>,
}

#[derive(Debug)]
struct ClockInner {
    start: Instant,
    paused: bool,
    pause_at: Option<Instant>,
    paused_total: Duration,
}

impl SimClock {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ClockInner {
                start: Instant::now(),
                paused: false,
                pause_at: None,
                paused_total: Duration::ZERO,
            })),
        }
    }

    /// 当前仿真时刻（秒），暂停期间冻结
    pub fn now_secs(&self) -> f64 {
        let i = self.inner.lock().unwrap();
        let now = i.pause_at.unwrap_or_else(Instant::now);
        (now - i.start).saturating_sub(i.paused_total).as_secs_f64()
    }

    pub fn set_paused(&self, paused: bool) {
        let mut i = self.inner.lock().unwrap();
        if paused && !i.paused {
            i.pause_at = Some(Instant::now());
            i.paused = true;
        } else if !paused && i.paused {
            if let Some(p) = i.pause_at.take() {
                i.paused_total += p.elapsed();
            }
            i.paused = false;
        }
    }

    pub fn is_paused(&self) -> bool {
        self.inner.lock().unwrap().paused
    }
}

impl Default for SimClock {
    fn default() -> Self {
        Self::new()
    }
}

// ---------- 设备运行时 ----------

/// 单台从机的运行时状态（配置快照 + 实时值 + 编码后的块缓存）。
/// 同一结构体服务两个方向：从机侧（仿真值 → 块）/ 主机侧（采集块 → 值）。
pub struct DeviceRuntime {
    pub id: u64,
    pub slave_id: u8,
    pub enabled: bool,
    pub fault: FaultConfig,
    /// 主机侧轮询周期
    pub poll_period_ms: u32,
    /// 采集健康统计（主机侧维护）
    pub last_ok_ms: u64,
    pub timeout_count: u64,
    pub fail_count: u64,
    /// 最近一次采集失败原因（成功采集后清空；仅主机侧维护）
    pub last_error: Option<String>,
    points: Vec<PointRuntime>,
    values: Vec<f64>,
    blocks: Blocks,
}

struct PointRuntime {
    id: u64,
    area: Area,
    address: u16,
    data_type: DataType,
    source: ValueSource,
}

/// 四区块缓存：直接可被协议层读取的原始视图
#[derive(Debug, Default, Clone)]
pub struct Blocks {
    pub coil: Vec<bool>,
    pub discrete: Vec<bool>,
    pub holding: Vec<u16>,
    pub input: Vec<u16>,
}

impl Blocks {
    pub fn area_len(&self, area: Area) -> usize {
        match area {
            Area::Coil => self.coil.len(),
            Area::DiscreteInput => self.discrete.len(),
            Area::HoldingRegister => self.holding.len(),
            Area::InputRegister => self.input.len(),
        }
    }
}

/// f64 值按数据类型编码为若干 16 位字（大端字序，符合 Modbus 惯例）
pub fn encode_words(v: f64, dt: DataType) -> Vec<u16> {
    match dt {
        DataType::U16 => vec![v.clamp(0.0, 65535.0).round() as u16],
        DataType::I16 => vec![(v.clamp(-32768.0, 32767.0).round() as i16) as u16],
        DataType::U32 => {
            let x = v.clamp(0.0, u32::MAX as f64).round() as u32;
            vec![(x >> 16) as u16, (x & 0xFFFF) as u16]
        }
        DataType::I32 => {
            let x = v.clamp(i32::MIN as f64, i32::MAX as f64).round() as i32;
            let u = x as u32;
            vec![(u >> 16) as u16, (u & 0xFFFF) as u16]
        }
        DataType::F32 => {
            let bits = (v as f32).to_bits();
            vec![(bits >> 16) as u16, (bits & 0xFFFF) as u16]
        }
        DataType::U64 => {
            let x = v.max(0.0).round() as u64;
            let w3 = (x >> 48) as u16;
            let w2 = (x >> 32) as u16;
            let w1 = (x >> 16) as u16;
            let w0 = x as u16;
            vec![w3, w2, w1, w0]
        }
        DataType::I64 => {
            let x = v.round() as i64;
            let u = x as u64;
            vec![(u >> 48) as u16, (u >> 32) as u16, (u >> 16) as u16, u as u16]
        }
        DataType::F64 => {
            let bits = v.to_bits();
            vec![
                (bits >> 48) as u16,
                (bits >> 32) as u16,
                (bits >> 16) as u16,
                bits as u16,
            ]
        }
    }
}

impl DeviceRuntime {
    pub fn new(dev: &Device) -> Self {
        let points: Vec<PointRuntime> = dev
            .points
            .iter()
            .map(|p| PointRuntime {
                id: p.id,
                area: p.area,
                address: p.address,
                data_type: p.data_type,
                source: p.source.clone(),
            })
            .collect();

        let mut rt = Self {
            id: dev.id,
            slave_id: dev.slave_id,
            enabled: dev.enabled,
            fault: dev.fault.clone(),
            poll_period_ms: dev.poll_period_ms,
            last_ok_ms: 0,
            timeout_count: 0,
            fail_count: 0,
            last_error: None,
            points,
            values: dev
                .points
                .iter()
                .map(|p| match &p.source {
                    ValueSource::Static { value } => *value,
                    ValueSource::Ramp { min, .. } => *min,
                    _ => 0.0,
                })
                .collect(),
            blocks: Blocks::default(),
        };
        rt.rebuild_blocks();
        rt
    }

    /// 一个仿真 tick：求值全部点位并重建块缓存
    pub fn tick(&mut self, t: f64) {
        for (i, p) in self.points.iter().enumerate() {
            self.values[i] = sim::evaluate(&p.source, t, self.values[i]);
        }
        self.rebuild_blocks();
    }

    fn rebuild_blocks(&mut self) {
        // 先按最大地址重排块尺寸
        for p in &self.points {
            let need = p.address as usize + slot_words(p.area, p.data_type);
            match p.area {
                Area::Coil => grow_bool(&mut self.blocks.coil, need),
                Area::DiscreteInput => grow_bool(&mut self.blocks.discrete, need),
                Area::HoldingRegister => grow_u16(&mut self.blocks.holding, need),
                Area::InputRegister => grow_u16(&mut self.blocks.input, need),
            }
        }
        for (i, p) in self.points.iter().enumerate() {
            let v = self.values[i];
            if p.area.is_bit() {
                let bit = v >= 0.5;
                match p.area {
                    Area::Coil => self.blocks.coil[p.address as usize] = bit,
                    Area::DiscreteInput => self.blocks.discrete[p.address as usize] = bit,
                    _ => unreachable!(),
                }
            } else {
                let words = encode_words(v, p.data_type);
                let base = p.address as usize;
                match p.area {
                    Area::HoldingRegister => {
                        for (k, w) in words.iter().enumerate() {
                            self.blocks.holding[base + k] = *w;
                        }
                    }
                    Area::InputRegister => {
                        for (k, w) in words.iter().enumerate() {
                            self.blocks.input[base + k] = *w;
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    /// 块快照（协议层读取用）
    pub fn snapshot_blocks(&self) -> Blocks {
        self.blocks.clone()
    }

    /// 全部点位实时值（id, value）
    pub fn snapshot_values(&self) -> Vec<(u64, f64)> {
        self.points
            .iter()
            .zip(self.values.iter())
            .map(|(p, v)| (p.id, *v))
            .collect()
    }

    /// 主机写入线圈：更新块缓存；静态源点位同步改值（写入成为新的静态值）
    pub fn write_coils(&mut self, addr: u16, bits: &[bool]) {
        for (k, b) in bits.iter().enumerate() {
            let a = addr as usize + k;
            if a < self.blocks.coil.len() {
                self.blocks.coil[a] = *b;
            }
        }
        let mut updates: Vec<(usize, f64)> = Vec::new();
        for (i, p) in self.points.iter().enumerate() {
            if p.area == Area::Coil && p.source.is_static() {
                let a = p.address as usize;
                if a >= addr as usize && a < addr as usize + bits.len() {
                    updates.push((i, if bits[a - addr as usize] { 1.0 } else { 0.0 }));
                }
            }
        }
        for (i, v) in updates {
            self.values[i] = v;
            if let ValueSource::Static { value } = &mut self.points[i].source {
                *value = v;
            }
        }
    }

    /// 主机写入保持寄存器：同上
    pub fn write_registers(&mut self, addr: u16, words: &[u16]) {
        for (k, w) in words.iter().enumerate() {
            let a = addr as usize + k;
            if a < self.blocks.holding.len() {
                self.blocks.holding[a] = *w;
            }
        }
        // 静态源点位：写入的字重解码为值，且写入成为新的静态值（tick 不再打回）
        let mut updates: Vec<(usize, f64)> = Vec::new();
        for (i, p) in self.points.iter().enumerate() {
            if p.area == Area::HoldingRegister && p.source.is_static() {
                let base = p.address as usize;
                let n = p.data_type.words();
                let end = addr as usize + words.len();
                if base < end && base + n > addr as usize {
                    let mut buf = [0u16; 4];
                    for k in 0..n {
                        let src = base + k;
                        let val = if src >= addr as usize && src < end {
                            words[src - addr as usize]
                        } else {
                            self.blocks.holding.get(src).copied().unwrap_or(0)
                        };
                        buf[k] = val;
                    }
                    updates.push((i, decode_words(&buf, p.data_type)));
                }
            }
        }
        for (i, v) in updates {
            self.values[i] = v;
            if let ValueSource::Static { value } = &mut self.points[i].source {
                *value = v;
            }
        }
    }

    // ---------- 主机侧：采集数据导入与状态 ----------

    /// 主机采集应答导入（寄存器区）：写入块缓存并重解码覆盖到的点位值
    pub fn import_registers(&mut self, area: Area, start: u16, words: &[u16]) {
        {
            let arr: &mut Vec<u16> = match area {
                Area::HoldingRegister => &mut self.blocks.holding,
                Area::InputRegister => &mut self.blocks.input,
                _ => return,
            };
            let end = start as usize + words.len();
            if arr.len() < end {
                arr.resize(end, 0);
            }
            arr[start as usize..end].copy_from_slice(words);
        }
        for (i, p) in self.points.iter().enumerate() {
            if p.area != area {
                continue;
            }
            let base = p.address as usize;
            let n = p.data_type.words();
            if base + n <= start as usize || base >= start as usize + words.len() {
                continue;
            }
            let mut buf = [0u16; 4];
            for k in 0..n {
                buf[k] = self.reg_view(area)[base + k];
            }
            self.values[i] = decode_words(&buf, p.data_type);
        }
    }

    /// 主机采集应答导入（位区）
    pub fn import_bits(&mut self, area: Area, start: u16, bits: &[bool]) {
        {
            let arr: &mut Vec<bool> = match area {
                Area::Coil => &mut self.blocks.coil,
                Area::DiscreteInput => &mut self.blocks.discrete,
                _ => return,
            };
            let end = start as usize + bits.len();
            if arr.len() < end {
                arr.resize(end, false);
            }
            for (k, b) in bits.iter().enumerate() {
                arr[start as usize + k] = *b;
            }
        }
        for (i, p) in self.points.iter().enumerate() {
            if p.area != area {
                continue;
            }
            let a = p.address as usize;
            if a >= start as usize && a < start as usize + bits.len() {
                self.values[i] = if bits[a - start as usize] { 1.0 } else { 0.0 };
            }
        }
    }

    fn reg_view(&self, area: Area) -> &[u16] {
        match area {
            Area::HoldingRegister => &self.blocks.holding,
            Area::InputRegister => &self.blocks.input,
            _ => &[],
        }
    }

    /// 点位布局（主机轮询分段用）：(point_id, area, address, data_type)
    pub fn point_layout(&self) -> Vec<(u64, Area, u16, DataType)> {
        self.points
            .iter()
            .map(|p| (p.id, p.area, p.address, p.data_type))
            .collect()
    }

    /// 按点位 id 查布局（写命令用）
    pub fn point_info(&self, point_id: u64) -> Option<(Area, u16, DataType)> {
        self.points
            .iter()
            .find(|p| p.id == point_id)
            .map(|p| (p.area, p.address, p.data_type))
    }

    /// 本轮采集成功
    pub fn mark_ok(&mut self) {
        self.last_ok_ms = now_ms();
        self.last_error = None;
    }

    pub fn mark_timeout(&mut self) {
        self.timeout_count += 1;
        self.last_error = Some("超时无应答".to_string());
    }

    /// 采集失败：计数并记录原因（悬浮提示展示）
    pub fn mark_fail(&mut self, reason: &str) {
        self.fail_count += 1;
        self.last_error = Some(reason.to_string());
    }
}

impl ValueSource {
    fn is_static(&self) -> bool {
        matches!(self, ValueSource::Static { .. })
    }
}

/// 解码 n 个字为 f64（与 encode_words 互逆；用于主机写回显）
fn decode_words(buf: &[u16; 4], dt: DataType) -> f64 {
    match dt {
        DataType::U16 => buf[0] as f64,
        DataType::I16 => buf[0] as i16 as f64,
        DataType::U32 => ((buf[0] as u32) << 16 | buf[1] as u32) as f64,
        DataType::I32 => (((buf[0] as u32) << 16 | buf[1] as u32) as i32) as f64,
        DataType::F32 => f32::from_bits((buf[0] as u32) << 16 | buf[1] as u32) as f64,
        DataType::U64 => {
            ((buf[0] as u64) << 48 | (buf[1] as u64) << 32 | (buf[2] as u64) << 16 | buf[3] as u64)
                as f64
        }
        DataType::I64 => {
            (((buf[0] as u64) << 48 | (buf[1] as u64) << 32 | (buf[2] as u64) << 16 | buf[3] as u64)
                as i64) as f64
        }
        DataType::F64 => {
            f64::from_bits(
                (buf[0] as u64) << 48 | (buf[1] as u64) << 32 | (buf[2] as u64) << 16 | buf[3] as u64,
            )
        }
    }
}

fn slot_words(area: Area, dt: DataType) -> usize {
    if area.is_bit() {
        1
    } else {
        dt.words()
    }
}

fn grow_bool(v: &mut Vec<bool>, need: usize) {
    if v.len() < need {
        v.resize(need, false);
    }
}

fn grow_u16(v: &mut Vec<u16>, need: usize) {
    if v.len() < need {
        v.resize(need, 0);
    }
}

/// epoch 毫秒
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------- 连接运行时与总管理器 ----------

/// 一条连接（总线）的任务组
struct ConnState {
    devices: Vec<Arc<Mutex<DeviceRuntime>>>,
    /// CAN 连接的节点运行时（Modbus 连接为空）
    can_nodes: Vec<Arc<Mutex<CanNodeRuntime>>>,
    cancel: CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// 主机连接的命令通道（写点/测试帧）；从机连接为 None
    cmd_tx: Option<mpsc::Sender<MasterMsg>>,
    /// CAN 连接的命令通道（手动发送）；非 CAN 连接为 None
    can_cmd_tx: Option<mpsc::Sender<CanMsg>>,
    /// CAN 连接的观测聚合（接收统计/帧率）；非 CAN 连接为 None
    can_agg: Option<Arc<Mutex<CanAgg>>>,
}

/// 主机连接的链路状态（仅主机类连接维护；从机监听不适用）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnStateKind {
    /// 首次连接中
    Connecting,
    /// 已建立（正常采集）
    Connected,
    /// 断开后重连中（含退避等待）
    Reconnecting,
}

/// 一条主机连接的当前链路状态
#[derive(Debug, Clone, Serialize)]
pub struct ConnStatus {
    pub kind: ConnStateKind,
    /// 断开原因等细节（前端悬浮提示展示）
    pub detail: Option<String>,
}

/// 运行时总管理器：跨连接共享仿真时钟与帧广播。
/// Clone 后内部共享同一状态。
#[derive(Clone)]
pub struct RuntimeManager {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    clock: SimClock,
    frame_tx: broadcast::Sender<FrameEvent>,
    conns: Mutex<HashMap<u64, ConnState>>,
    /// 主机连接链路状态（conn_id → 状态；连接停止时清除）
    conn_states: Mutex<HashMap<u64, ConnStatus>>,
    frame_seq: std::sync::atomic::AtomicU64,
}

impl RuntimeManager {
    pub fn new() -> Self {
        let (frame_tx, _) = broadcast::channel(FRAME_CHANNEL_CAP);
        Self {
            inner: Arc::new(RuntimeInner {
                clock: SimClock::new(),
                frame_tx,
                conns: Mutex::new(HashMap::new()),
                conn_states: Mutex::new(HashMap::new()),
                frame_seq: std::sync::atomic::AtomicU64::new(1),
            }),
        }
    }

    pub fn clock(&self) -> &SimClock {
        &self.inner.clock
    }

    pub fn subscribe_frames(&self) -> broadcast::Receiver<FrameEvent> {
        self.inner.frame_tx.subscribe()
    }

    pub fn is_running(&self) -> bool {
        !self.inner.conns.lock().unwrap().is_empty()
    }

    /// 广播一帧（发送失败=无订阅者，忽略）
    pub fn emit_frame(&self, mut ev: FrameEvent) {
        ev.id = self
            .inner
            .frame_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _ = self.inner.frame_tx.send(ev);
    }

    /// 启动工程里全部连接；任一连接启动失败则整体回滚
    pub async fn start(&self, project: &ProjectSpec) -> Result<(), String> {
        let mut started: Vec<u64> = Vec::new();
        for conn in &project.connections {
            match self.start_connection(conn).await {
                Ok(()) => started.push(conn.id),
                Err(e) => {
                    for id in &started {
                        self.stop_connection(*id);
                    }
                    return Err(format!("连接「{}」启动失败: {}", conn.name, e));
                }
            }
        }
        Ok(())
    }

    /// 启动单条连接（已在运行则先停旧实例再以最新配置启动）。
    /// TCP 监听绑定 / 串口打开在此完成，失败即返回错误（端口占用等）。
    pub async fn start_connection(&self, conn: &Connection) -> Result<(), String> {
        self.start_connection_with(conn, None).await
    }

    /// 同上，但可指定启动后在线的设备集合：
    /// Some(ids) = 仅这些设备参与（单台设备 ▶ 首启用，其余在线停用）；None = 按工程配置。
    pub async fn start_connection_with(
        &self,
        conn: &Connection,
        enabled_devices: Option<Vec<u64>>,
    ) -> Result<(), String> {
        let was_running = self.stop_connection(conn.id);
        // 旧实例的监听 socket / 串口句柄释放是异步的，重启前留一点余量
        if was_running {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        let devices: Vec<Arc<Mutex<DeviceRuntime>>> = conn
            .devices
            .iter()
            .map(|d| {
                let mut rt = DeviceRuntime::new(d);
                if let Some(ids) = &enabled_devices {
                    rt.enabled = ids.contains(&d.id);
                }
                Arc::new(Mutex::new(rt))
            })
            .collect();

        // CAN 节点运行时（CAN 连接专用；Modbus 连接恒为空）
        let can_nodes: Vec<Arc<Mutex<CanNodeRuntime>>> = conn
            .can_nodes
            .iter()
            .map(|n| {
                let mut rt = CanNodeRuntime::new(n);
                if let Some(ids) = &enabled_devices {
                    rt.enabled = ids.contains(&n.id);
                }
                Arc::new(Mutex::new(rt))
            })
            .collect();

        let cancel = CancellationToken::new();

        // 仿真 tick 只服务从机连接（波形求值并重建块缓存）；
        // 主机连接的值来自采集导入（import_registers），tick 会把采集值打回静态占位值，必须跳过；
        // CAN 连接无「实时值」概念（发送报文表读工程配置），同样跳过
        let mut tasks = Vec::new();
        if matches!(conn.transport, Transport::TcpServer { .. } | Transport::Serial { .. }) {
            let tick_devs = devices.clone();
            let tick_clock = self.inner.clock.clone();
            let tick_cancel = cancel.clone();
            tasks.push(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(TICK_INTERVAL);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = tick_cancel.cancelled() => break,
                        _ = ticker.tick() => {
                            if tick_clock.is_paused() { continue; }
                            let t = tick_clock.now_secs();
                            for dev in &tick_devs {
                                let mut d = dev.lock().unwrap();
                                if d.enabled { d.tick(t); }
                            }
                        }
                    }
                }
            }));
        }
        let mut cmd_tx: Option<mpsc::Sender<MasterMsg>> = None;
        let mut can_cmd_tx: Option<mpsc::Sender<CanMsg>> = None;
        let mut can_agg: Option<Arc<Mutex<CanAgg>>> = None;

        // CAN 连接帧定义预校验（启动即暴露配置错误，而不是等到发送时静默失败）
        for n in &conn.can_nodes {
            for f in &n.frames {
                f.validate().map_err(|e| format!("节点「{}」报文 id {}: {e}", n.name, f.id))?;
            }
        }

        match &conn.transport {
            Transport::TcpServer { listen_addr, port } => {
                let listener = tokio::net::TcpListener::bind((listen_addr.as_str(), *port))
                    .await
                    .map_err(|e| format!("监听 {listen_addr}:{port} 失败: {e}"))?;
                let ctx = crate::protocol::modbus::ConnCtx {
                    conn_id: conn.id,
                    devices: devices.clone(),
                    clock: self.inner.clock.clone(),
                    manager: self.clone(),
                };
                let t = crate::protocol::modbus::tcp::serve(Arc::new(ctx), listener, cancel.clone());
                tasks.push(tokio::spawn(t));
            }
            Transport::TcpClient { host, port, timeout_ms } => {
                let (tx, rx) = mpsc::channel(64);
                cmd_tx = Some(tx);
                let ctx = crate::protocol::modbus::ConnCtx {
                    conn_id: conn.id,
                    devices: devices.clone(),
                    clock: self.inner.clock.clone(),
                    manager: self.clone(),
                };
                let t = master::serve(
                    Arc::new(ctx),
                    host.clone(),
                    *port,
                    *timeout_ms,
                    cancel.clone(),
                    rx,
                );
                tasks.push(tokio::spawn(t));
            }
            Transport::Serial { serial_port, baud, parity, stop_bits } => {
                let port = crate::protocol::modbus::rtu::open_port(serial_port, *baud, *parity, *stop_bits)?;
                let ctx = crate::protocol::modbus::ConnCtx {
                    conn_id: conn.id,
                    devices: devices.clone(),
                    clock: self.inner.clock.clone(),
                    manager: self.clone(),
                };
                let t = crate::protocol::modbus::rtu::serve(
                    Arc::new(ctx),
                    serial_port.clone(),
                    port,
                    cancel.clone(),
                );
                tasks.push(tokio::spawn(t));
            }
            Transport::SerialMaster { serial_port, baud, parity, stop_bits, timeout_ms } => {
                let port = crate::protocol::modbus::rtu::open_port(serial_port, *baud, *parity, *stop_bits)?;
                let (tx, rx) = mpsc::channel(64);
                cmd_tx = Some(tx);
                let ctx = crate::protocol::modbus::ConnCtx {
                    conn_id: conn.id,
                    devices: devices.clone(),
                    clock: self.inner.clock.clone(),
                    manager: self.clone(),
                };
                let t = master::serve_serial(Arc::new(ctx), port, *timeout_ms, cancel.clone(), rx);
                tasks.push(tokio::spawn(t));
            }
            Transport::UsbCan { backend, device_index: _, channel: _, bitrate, serial_port, serial_baud } => {
                if backend != "slcan" {
                    return Err(format!(
                        "CAN 后端「{backend}」尚未实现（zlgcan/candle 待硬件到货，当前可用：slcan）"
                    ));
                }
                let name = serial_port.as_deref().unwrap_or("");
                if name.is_empty() {
                    return Err("slcan 后端需要配置串口（连接编辑器里选择）".to_string());
                }
                let io = crate::protocol::can::slcan::open(name, *serial_baud, *bitrate)?;
                let (tx, rx) = mpsc::channel(64);
                can_cmd_tx = Some(tx);
                let agg = Arc::new(Mutex::new(CanAgg::default()));
                can_agg = Some(agg.clone());
                let ctx = Arc::new(crate::protocol::can::CanCtx {
                    conn_id: conn.id,
                    nodes: can_nodes.clone(),
                    manager: self.clone(),
                    agg,
                });
                let t = crate::protocol::can::serve_can_with(ctx, Box::new(io), cancel.clone(), rx);
                tasks.push(tokio::spawn(t));
            }
            Transport::CanNet { host, port, channel: _, timeout_ms } => {
                // ZLG CANET 网关（TCP 工作端口，13 字节透传帧）：断线退避重连在引擎层完成
                let (tx, rx) = mpsc::channel(64);
                can_cmd_tx = Some(tx);
                let agg = Arc::new(Mutex::new(CanAgg::default()));
                can_agg = Some(agg.clone());
                let ctx = Arc::new(crate::protocol::can::CanCtx {
                    conn_id: conn.id,
                    nodes: can_nodes.clone(),
                    manager: self.clone(),
                    agg,
                });
                let t = crate::protocol::can::zlg_canet::serve_can_net(
                    ctx,
                    host.clone(),
                    *port,
                    *timeout_ms,
                    cancel.clone(),
                    rx,
                );
                tasks.push(tokio::spawn(t));
            }
        }

        self.inner.conns.lock().unwrap().insert(
            conn.id,
            ConnState {
                devices,
                can_nodes,
                cancel,
                tasks,
                cmd_tx,
                can_cmd_tx,
                can_agg,
            },
        );
        Ok(())
    }

    /// 停止单条连接；返回该连接此前是否在运行
    pub fn stop_connection(&self, id: u64) -> bool {
        self.inner.conn_states.lock().unwrap().remove(&id);
        if let Some(state) = self.inner.conns.lock().unwrap().remove(&id) {
            state.cancel.cancel();
            for t in state.tasks {
                t.abort();
            }
            true
        } else {
            false
        }
    }

    /// 停止全部连接
    pub fn stop_all(&self) {
        let ids: Vec<u64> = self.inner.conns.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.stop_connection(id);
        }
    }

    /// 运行中的连接 id 列表（前端逐连接启停显示用）
    pub fn running_conn_ids(&self) -> Vec<u64> {
        self.inner.conns.lock().unwrap().keys().copied().collect()
    }

    /// 在线（运行中）的设备 id 列表（前端逐设备启停显示用；CAN 节点共用同一 id 空间）
    pub fn running_device_ids(&self) -> Vec<u64> {
        let conns = self.inner.conns.lock().unwrap();
        let mut out = Vec::new();
        for state in conns.values() {
            for dev in &state.devices {
                let d = dev.lock().unwrap();
                if d.enabled {
                    out.push(d.id);
                }
            }
            for node in &state.can_nodes {
                let n = node.lock().unwrap();
                if n.enabled {
                    out.push(n.id);
                }
            }
        }
        out
    }

    /// 在线启停一台设备/CAN 节点（不改工程配置）：
    /// 从机侧停用后该从机地址不再应答（按网关惯例回 0x0B）；主机侧停用即暂停轮询；
    /// CAN 节点停用即暂停其周期报文发送
    pub fn set_device_enabled(&self, device_id: u64, enabled: bool) -> Result<(), String> {
        let conns = self.inner.conns.lock().unwrap();
        for state in conns.values() {
            for dev in &state.devices {
                let mut d = dev.lock().unwrap();
                if d.id == device_id {
                    d.enabled = enabled;
                    return Ok(());
                }
            }
            for node in &state.can_nodes {
                let mut n = node.lock().unwrap();
                if n.id == device_id {
                    n.enabled = enabled;
                    return Ok(());
                }
            }
        }
        Err("设备不在任何运行中的连接里".to_string())
    }

    /// 选中设备的实时值（前端 250ms 拉取）
    pub fn snapshot_values(&self, device_id: u64) -> Vec<(u64, f64)> {
        let conns = self.inner.conns.lock().unwrap();
        for state in conns.values() {
            for dev in &state.devices {
                let d = dev.lock().unwrap();
                if d.id == device_id {
                    return d.snapshot_values();
                }
            }
        }
        Vec::new()
    }

    /// 全部设备的采集健康统计：(device_id, last_ok_ms, timeout_count, fail_count, last_error)
    pub fn snapshot_stats(&self) -> Vec<(u64, u64, u64, u64, Option<String>)> {
        let conns = self.inner.conns.lock().unwrap();
        let mut out = Vec::new();
        for state in conns.values() {
            for dev in &state.devices {
                let d = dev.lock().unwrap();
                out.push((d.id, d.last_ok_ms, d.timeout_count, d.fail_count, d.last_error.clone()));
            }
        }
        out
    }

    /// 设置主机连接链路状态（仅主机类连接调用；从机监听不适用）
    pub fn set_conn_state(&self, conn_id: u64, kind: ConnStateKind, detail: Option<String>) {
        self.inner
            .conn_states
            .lock()
            .unwrap()
            .insert(conn_id, ConnStatus { kind, detail });
    }

    /// 主机连接链路状态快照：(conn_id, kind, detail)；停止的连接不出现
    pub fn snapshot_conn_states(&self) -> Vec<(u64, ConnStateKind, Option<String>)> {
        self.inner
            .conn_states
            .lock()
            .unwrap()
            .iter()
            .map(|(id, s)| (*id, s.kind, s.detail.clone()))
            .collect()
    }

    /// CAN 连接的观测快照（接收聚合 + 帧率）；推送 ID 聚合视图与徽章用
    pub fn snapshot_can_rx(&self) -> Vec<(u64, CanAggSnapshot)> {
        let conns = self.inner.conns.lock().unwrap();
        conns
            .iter()
            .filter_map(|(id, state)| {
                let agg = state.can_agg.as_ref()?;
                Some((*id, agg.lock().unwrap().snapshot()))
            })
            .collect()
    }

    /// CAN 手动发送一帧（下发到所属 CAN 连接的调度线程）
    pub async fn send_can_frame(&self, conn_id: u64, frame: crate::protocol::can::CanFrame) -> Result<(), String> {
        let tx = {
            let conns = self.inner.conns.lock().unwrap();
            match conns.get(&conn_id) {
                Some(state) => state.can_cmd_tx.clone(),
                None => return Err("连接不存在或未启动".to_string()),
            }
        };
        let tx = tx.ok_or("该连接不是 CAN 连接")?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(CanMsg::SendFrame { frame, reply: reply_tx })
            .await
            .map_err(|_| "CAN 连接已停止".to_string())?;
        reply_rx.await.map_err(|_| "发送命令无应答".to_string())?
    }

    /// 主机侧写点位：按点位类型自动拼 FC05/06/10 下发到所属主机连接
    pub async fn write_point(&self, device_id: u64, point_id: u64, value: f64) -> Result<(), String> {
        let (tx, dev) = {
            let conns = self.inner.conns.lock().unwrap();
            let mut found: Option<(mpsc::Sender<MasterMsg>, Arc<Mutex<DeviceRuntime>>)> = None;
            for state in conns.values() {
                for d in &state.devices {
                    if d.lock().unwrap().id == device_id {
                        if let Some(tx) = &state.cmd_tx {
                            found = Some((tx.clone(), d.clone()));
                        }
                    }
                }
            }
            match found {
                Some(v) => v,
                None => return Err("设备不在运行中的主机连接里".to_string()),
            }
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(MasterMsg::Write { device: dev, point_id, value, reply: reply_tx })
            .await
            .map_err(|_| "主机连接已停止".to_string())?;
        reply_rx.await.map_err(|_| "写命令无应答".to_string())?
    }

    /// 主机侧测试帧：向指定主机连接直接下发 PDU，返回应答 PDU
    pub async fn send_test_frame(&self, conn_id: u64, unit: u8, pdu: Vec<u8>) -> Result<Vec<u8>, String> {
        let tx = {
            let conns = self.inner.conns.lock().unwrap();
            match conns.get(&conn_id) {
                Some(state) => state.cmd_tx.clone(),
                None => return Err("连接不存在或未启动".to_string()),
            }
        };
        let tx = tx.ok_or_else(|| "该连接不是主机连接".to_string())?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(MasterMsg::TestFrame { unit, pdu, reply: reply_tx })
            .await
            .map_err(|_| "主机连接已停止".to_string())?;
        reply_rx.await.map_err(|_| "测试帧无应答".to_string())?
    }
}

impl Default for RuntimeManager {
    fn default() -> Self {
        Self::new()
    }
}
