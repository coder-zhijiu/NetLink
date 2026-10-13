//! Modbus 点位扫描集成测试：
//! - TCP：用本工具自己的从机引擎当被测设备（对越界读回异常 02），主机引擎扫描它；
//! - 哑监听（接受连接但永不读写）：超时型「死设备」→ 探活阶梯全超时 → 中止；
//! - 扫描取消：事务间检测取消令牌，返回部分结果并标「已取消」；
//! - RTU：内存假串口驱动真实主机主循环，扫描走串口事务层。
//! 同时覆盖：扫描帧带 scan 标志（监视器可过滤）、扫描结束句柄清除。

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netlink_core::model::{
    Area, Connection, DataType, Device, FaultConfig, Point, ProjectSpec, Transport, ValueSource,
};
use netlink_core::protocol::modbus::master::{serve_rtu_with, MasterMsg, RtuIo};
use netlink_core::protocol::modbus::pdu;
use netlink_core::protocol::modbus::rtu::crc16;
use netlink_core::protocol::modbus::scan::{ScanParams, ScanProgress};
use netlink_core::protocol::modbus::ConnCtx;
use netlink_core::runtime::{encode_words, DeviceRuntime, RuntimeManager, SimClock};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const PORT: u16 = 15060;
const PORT_DEAD: u16 = 15061;
const PORT_DEAD2: u16 = 15062;

/// 被扫描的从机：holding 0=f32 123.5、holding 2=f32 999.0、coil 0=1
/// （保持区可读窗口 [0,4)，线圈 [0,1)——其余地址从机回异常 02）
fn slave_project() -> ProjectSpec {
    ProjectSpec {
        schema_version: 3,
        name: "slave".into(),
        connections: vec![Connection {
            id: 4,
            name: "被扫从机".into(),
            transport: Transport::TcpServer { listen_addr: "127.0.0.1".into(), port: PORT },
            can_nodes: vec![],
            devices: vec![Device {
                id: 901,
                name: "从机装置".into(),
                slave_id: 1,
                enabled: true,
                fault: FaultConfig::default(),
                poll_period_ms: 1000,
                points: vec![
                    Point {
                        id: 11,
                        name: "设定值".into(),
                        area: Area::HoldingRegister,
                        address: 0,
                        data_type: DataType::F32,
                        source: ValueSource::Static { value: 123.5 },
                        note: None,
                    },
                    Point {
                        id: 12,
                        name: "设定值2".into(),
                        area: Area::HoldingRegister,
                        address: 2,
                        data_type: DataType::F32,
                        source: ValueSource::Static { value: 999.0 },
                        note: None,
                    },
                    Point {
                        id: 13,
                        name: "遥信".into(),
                        area: Area::Coil,
                        address: 0,
                        data_type: DataType::U16,
                        source: ValueSource::Static { value: 1.0 },
                        note: None,
                    },
                ],
            }],
        }],
    }
}

/// 扫描主机：连 PORT，设备点表为空（扫描期间无轮询干扰）
fn master_project(port: u16, timeout_ms: u32) -> ProjectSpec {
    ProjectSpec {
        schema_version: 3,
        name: "master".into(),
        connections: vec![Connection {
            id: 5,
            name: "扫描主机".into(),
            transport: Transport::TcpClient { host: "127.0.0.1".into(), port, timeout_ms },
            can_nodes: vec![],
            devices: vec![Device {
                id: 902,
                name: "扫描装置".into(),
                slave_id: 1,
                enabled: true,
                fault: FaultConfig::default(),
                poll_period_ms: 1000,
                points: vec![],
            }],
        }],
    }
}

async fn wait_until<F: Fn() -> bool>(ms: u64, f: F) -> bool {
    for _ in 0..(ms / 50) {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f()
}

fn scan_params(areas: Vec<Area>, end: u16) -> ScanParams {
    ScanParams { unit: 1, areas, start_addr: 0, end_addr: end }
}

/// TCP 自闭环：扫出保持区 [0,4)（含两个字段的编码值）与线圈 [0,1)，
/// 扫描帧带 scan 标志，完成后句柄清除
#[tokio::test(flavor = "multi_thread")]
async fn tcp_scan_discovers_ranges_and_marks_frames() {
    let slave = RuntimeManager::new();
    slave.start(&slave_project()).await.unwrap();

    let master = RuntimeManager::new();
    let mut frame_rx = master.subscribe_frames();
    master.start(&master_project(PORT, 400)).await.unwrap();

    let r = master.scan_points(5, scan_params(vec![Area::HoldingRegister, Area::Coil], 99)).await.unwrap();
    assert!(r.aborted.is_none(), "{:?}", r.aborted);
    assert!(r.suspicious.is_empty(), "{:?}", r.suspicious);
    assert!(r.rejected.is_empty(), "{:?}", r.rejected);

    // 保持区 [0,4)：两个 f32 的字编码连排
    let holding: Vec<_> = r.ranges.iter().filter(|x| x.area == Area::HoldingRegister).collect();
    assert_eq!(holding.len(), 1);
    assert_eq!((holding[0].start, holding[0].qty), (0, 4));
    let mut expect = encode_words(123.5, DataType::F32);
    expect.extend(encode_words(999.0, DataType::F32));
    assert_eq!(holding[0].values, expect);

    // 线圈 [0,1) = 1
    let coil: Vec<_> = r.ranges.iter().filter(|x| x.area == Area::Coil).collect();
    assert_eq!(coil.len(), 1);
    assert_eq!((coil[0].start, coil[0].qty, coil[0].values.clone()), (0, 1, vec![1]));

    // 扫描帧全部带 scan 标志（该主机无轮询点位，本连接帧只来自扫描）
    let mut scan_frames = 0;
    let mut non_scan = 0;
    while let Ok(ev) = frame_rx.try_recv() {
        if ev.scan {
            scan_frames += 1;
        } else {
            non_scan += 1;
        }
    }
    assert!(scan_frames >= 4, "扫描帧过少: {scan_frames}");
    assert_eq!(non_scan, 0, "无轮询点位的连接不应有非扫描帧");

    // 扫描结束：进度句柄清除
    assert!(master.snapshot_scan_progress().is_empty());

    slave.stop_all();
    master.stop_all();
}

/// 哑监听（接受连接但永不读写）：块读超时 → 探活阶梯全超时 → 中止并给收窄建议
#[tokio::test(flavor = "multi_thread")]
async fn tcp_scan_dead_link_aborts_with_hint() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", PORT_DEAD)).unwrap();
    // 不 accept：连接由内核 backlog 完成，数据无人读 → 全超时

    let master = RuntimeManager::new();
    master.start(&master_project(PORT_DEAD, 120)).await.unwrap();
    let ok = wait_until(3000, || {
        master.snapshot_conn_states().iter().any(|(_, k, _)| matches!(k, netlink_core::runtime::ConnStateKind::Connected))
    })
    .await;
    assert!(ok, "主机应连上哑监听");

    let r = master.scan_points(5, scan_params(vec![Area::HoldingRegister], 9)).await.unwrap();
    let msg = r.aborted.expect("死链路应中止");
    assert!(msg.contains("无应答"), "中止原因应含无应答: {msg}");
    assert!(msg.contains("收窄"), "应给出收窄建议: {msg}");
    assert!(r.ranges.is_empty());
    assert!(master.snapshot_scan_progress().is_empty());

    drop(listener);
    master.stop_all();
}

/// 取消：扫描进行中下发取消令牌 → 事务间检测，返回部分结果并标「已取消」
#[tokio::test(flavor = "multi_thread")]
async fn tcp_scan_cancel_returns_partial_result() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", PORT_DEAD2)).unwrap();
    let master = RuntimeManager::new();
    master.start(&master_project(PORT_DEAD, 200)).await.unwrap();
    let ok = wait_until(3000, || {
        master.snapshot_conn_states().iter().any(|(_, k, _)| matches!(k, netlink_core::runtime::ConnStateKind::Connected))
    })
    .await;
    assert!(ok);

    let m = master.clone();
    let handle = tokio::spawn(async move { m.scan_points(5, scan_params(vec![Area::HoldingRegister], 99)).await });

    // 首块读（200ms 超时）进行中取消；下笔请求前检测到 → 中止
    tokio::time::sleep(Duration::from_millis(60)).await;
    master.scan_cancel(5).unwrap();

    let r = handle.await.unwrap().unwrap();
    assert_eq!(r.aborted.as_deref(), Some("已取消"));
    assert!(master.snapshot_scan_progress().is_empty());

    drop(listener);
    master.stop_all();
}

// ---------- RTU：内存假串口 ----------

/// 内存假串口：master 写入立即被"从机"应答（rtu_master.rs 同款，精简为扫描所需）
struct FakeRtuIo {
    to_master: VecDeque<u8>,
    pending: Vec<u8>,
    slave: Arc<Mutex<DeviceRuntime>>,
    unit: u8,
}

impl RtuIo for FakeRtuIo {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.pending.extend_from_slice(buf);
        self.respond();
        Ok(())
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.to_master.pop_front() {
            Some(b) => {
                buf[0] = b;
                Ok(1)
            }
            None => Ok(0), // 读超时（静默）
        }
    }
}

impl FakeRtuIo {
    fn respond(&mut self) {
        let n = self.pending.len();
        if n < 4 {
            return;
        }
        let frame = std::mem::take(&mut self.pending);
        let addr = frame[0];
        let crc = u16::from_le_bytes([frame[n - 2], frame[n - 1]]);
        if crc != crc16(&frame[..n - 2]) || addr != self.unit {
            return;
        }
        let resp_pdu = {
            let mut g = self.slave.lock().unwrap();
            pdu::process_request(&frame[1..n - 2], &mut g).expect("应答 PDU")
        };
        let mut out = Vec::with_capacity(3 + resp_pdu.len());
        out.push(addr);
        out.extend_from_slice(&resp_pdu);
        out.extend_from_slice(&crc16(&out).to_le_bytes());
        self.to_master.extend(out);
    }
}

fn rtu_slave_device() -> Device {
    Device {
        id: 1,
        name: "从机".into(),
        slave_id: 7,
        enabled: true,
        fault: FaultConfig::default(),
        poll_period_ms: 1000,
        points: vec![Point {
            id: 11,
            name: "设定".into(),
            area: Area::HoldingRegister,
            address: 0,
            data_type: DataType::F32,
            source: ValueSource::Static { value: 123.5 },
            note: None,
        }],
    }
}

/// RTU 扫描：假串口从机（holding [0,2) 可读），扫描走串口事务层，
/// 帧带 scan 标志
#[tokio::test(flavor = "multi_thread")]
async fn rtu_scan_discovers_holding_window() {
    let slave_rt = Arc::new(Mutex::new(DeviceRuntime::new(&rtu_slave_device())));
    let master_rt = Arc::new(Mutex::new(DeviceRuntime::new(&Device {
        id: 2,
        name: "采集".into(),
        slave_id: 7,
        enabled: true,
        fault: FaultConfig::default(),
        poll_period_ms: 1000,
        points: vec![],
    })));

    let manager = RuntimeManager::new();
    let mut frame_rx = manager.subscribe_frames();
    let ctx = Arc::new(ConnCtx {
        conn_id: 9,
        devices: vec![master_rt],
        clock: SimClock::new(),
        manager: manager.clone(),
    });

    let (tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let io = FakeRtuIo { to_master: VecDeque::new(), pending: Vec::new(), slave: slave_rt, unit: 7 };
    let task = tokio::spawn(serve_rtu_with(ctx, Box::new(io), 300, cancel.clone(), rx));

    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(MasterMsg::Scan {
        params: ScanParams { unit: 7, areas: vec![Area::HoldingRegister], start_addr: 0, end_addr: 99 },
        cancel: CancellationToken::new(),
        progress: Arc::new(Mutex::new(ScanProgress::default())),
        reply: reply_tx,
    })
    .await
    .unwrap();
    let r = reply_rx.await.unwrap();

    assert!(r.aborted.is_none(), "{:?}", r.aborted);
    assert_eq!(r.ranges.len(), 1);
    assert_eq!((r.ranges[0].area, r.ranges[0].start, r.ranges[0].qty), (Area::HoldingRegister, 0, 2));
    assert_eq!(r.ranges[0].values, encode_words(123.5, DataType::F32));

    let mut saw_scan_frame = false;
    while let Ok(ev) = frame_rx.try_recv() {
        if ev.scan {
            saw_scan_frame = true;
        }
    }
    assert!(saw_scan_frame, "RTU 扫描帧应带 scan 标志");

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
}
