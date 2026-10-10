//! Modbus RTU 主机集成测试：
//! 用内存假串口（实现 RtuIo，内置一个从机应答器）驱动真实的主机主循环，
//! 覆盖：轮询采集值导入、写线圈/写寄存器、写输入区被拒、测试帧、CRC 校验。

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netlink_core::model::{Area, Connection, DataType, Device, FaultConfig, Point, ProjectSpec, Transport, ValueSource};
use netlink_core::protocol::modbus::master::{serve_rtu_with, MasterMsg, RtuIo};
use netlink_core::protocol::modbus::rtu::crc16;
use netlink_core::protocol::modbus::pdu;
use netlink_core::protocol::modbus::ConnCtx;
use netlink_core::runtime::{DeviceRuntime, RuntimeManager, SimClock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 内存假串口：master 写入立即被"从机"应答；读空返回 Ok(0) 模拟读超时
struct FakeRtuIo {
    to_master: VecDeque<u8>,
    pending: Vec<u8>,
    slave: Arc<Mutex<DeviceRuntime>>,
    /// 只应答这个从机地址（其他地址静默，模拟总线上无此设备）
    unit: u8,
    /// 从机故障模拟（测主机侧采集统计/监视器标注）
    fault: FakeFault,
}

/// 假从机故障：静默（超时）/ 坏 CRC（脏帧）/ 异常码
#[derive(Clone, Copy)]
enum FakeFault {
    None,
    Silent,
    CorruptCrc,
    Exception(u8),
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
    /// 从机应答器：解析请求帧 → pdu 处理 → 组应答帧
    fn respond(&mut self) {
        let n = self.pending.len();
        if n < 4 {
            return;
        }
        let frame = std::mem::take(&mut self.pending);
        let addr = frame[0];
        let crc = u16::from_le_bytes([frame[n - 2], frame[n - 1]]);
        if crc != crc16(&frame[..n - 2]) {
            return; // 坏帧丢弃
        }
        if addr != self.unit {
            return; // 非本站：不应答
        }
        if matches!(self.fault, FakeFault::Silent) {
            return; // 超时故障：不应答
        }
        let pdu_req = &frame[1..n - 2];
        let resp_pdu = match self.fault {
            FakeFault::Exception(code) => vec![pdu_req[0] | 0x80, code],
            _ => {
                let mut g = self.slave.lock().unwrap();
                pdu::process_request(pdu_req, &mut g).expect("应答 PDU")
            }
        };
        let mut out = Vec::with_capacity(3 + resp_pdu.len());
        out.push(addr);
        out.extend_from_slice(&resp_pdu);
        let mut c = crc16(&out);
        if matches!(self.fault, FakeFault::CorruptCrc) {
            c ^= 1; // 脏帧：翻转一位
        }
        out.extend_from_slice(&c.to_le_bytes());
        self.to_master.extend(out);
    }
}

/// 从机设备：holding 0 = 123.5(f32) / input 0 = 456.25(f32) / coil 0 = 1 / coil 1 = 0
fn slave_device() -> Device {
    Device {
        id: 1,
        name: "从机".into(),
        slave_id: 7,
        enabled: true,
        fault: FaultConfig::default(),
        poll_period_ms: 1000,
        points: vec![
            Point {
                id: 11,
                name: "设定".into(),
                area: Area::HoldingRegister,
                address: 0,
                data_type: DataType::F32,
                source: ValueSource::Static { value: 123.5 },
                note: None,
            },
            Point {
                id: 12,
                name: "遥测".into(),
                area: Area::InputRegister,
                address: 0,
                data_type: DataType::F32,
                source: ValueSource::Static { value: 456.25 },
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
    }
}

/// 主机采集设备（点表与从机一致，地址 7）
fn master_device() -> Device {
    Device {
        id: 2,
        name: "采集".into(),
        slave_id: 7,
        enabled: true,
        fault: FaultConfig::default(),
        poll_period_ms: 100,
        points: vec![
            Point {
                id: 21,
                name: "设定".into(),
                area: Area::HoldingRegister,
                address: 0,
                data_type: DataType::F32,
                source: ValueSource::Static { value: 0.0 },
                note: None,
            },
            Point {
                id: 22,
                name: "遥测".into(),
                area: Area::InputRegister,
                address: 0,
                data_type: DataType::F32,
                source: ValueSource::Static { value: 0.0 },
                note: None,
            },
            Point {
                id: 23,
                name: "遥信".into(),
                area: Area::Coil,
                address: 0,
                data_type: DataType::U16,
                source: ValueSource::Static { value: 0.0 },
                note: None,
            },
        ],
    }
}

fn value_of(vals: &[(u64, f64)], id: u64) -> f64 {
    vals.iter().find(|(i, _)| *i == id).map(|(_, v)| *v).unwrap_or(f64::NAN)
}

async fn wait_until<F: Fn() -> bool>(ms: u64, f: F) -> bool {
    for _ in 0..(ms / 20) {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

#[tokio::test(flavor = "multi_thread")]
async fn rtu_master_polls_writes_and_rejects_readonly() {
    let slave_rt = Arc::new(Mutex::new(DeviceRuntime::new(&slave_device())));
    let master_rt = Arc::new(Mutex::new(DeviceRuntime::new(&master_device())));

    let manager = RuntimeManager::new();
    let ctx = Arc::new(ConnCtx {
        conn_id: 9,
        devices: vec![master_rt.clone()],
        clock: SimClock::new(),
        manager: manager.clone(),
    });

    let (tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let io = FakeRtuIo {
        to_master: VecDeque::new(),
        pending: Vec::new(),
        slave: slave_rt.clone(),
        unit: 7,
        fault: FakeFault::None,
    };

    let task = tokio::spawn(serve_rtu_with(ctx, Box::new(io), 300, cancel.clone(), rx));

    // ---- 1. 轮询采集：三区值导入主机运行时 ----
    let ok = wait_until(5000, || {
        let g = master_rt.lock().unwrap();
        let vals = g.snapshot_values();
        (value_of(&vals, 21) - 123.5).abs() < 0.01
            && (value_of(&vals, 22) - 456.25).abs() < 0.01
            && value_of(&vals, 23) == 1.0
    })
    .await;
    assert!(ok, "RTU 主机未采集到值: {:?}", master_rt.lock().unwrap().snapshot_values());

    // ---- 2. 写线圈（FC05）：遥信 1 → 0 ----
    let (r_tx, r_rx) = tokio::sync::oneshot::channel();
    tx.send(MasterMsg::Write { device: master_rt.clone(), point_id: 23, value: 0.0, reply: r_tx })
        .await
        .unwrap();
    r_rx.await.unwrap().unwrap();
    let g = slave_rt.lock().unwrap();
    let vals = g.snapshot_values();
    drop(g);
    assert!(value_of(&vals, 13) == 0.0, "从机线圈未翻转: {vals:?}");

    // ---- 3. 写保持寄存器（FC06/FC10）：设定 → 999.0 ----
    let (r_tx, r_rx) = tokio::sync::oneshot::channel();
    tx.send(MasterMsg::Write { device: master_rt.clone(), point_id: 21, value: 999.0, reply: r_tx })
        .await
        .unwrap();
    r_rx.await.unwrap().unwrap();
    let g = slave_rt.lock().unwrap();
    let vals = g.snapshot_values();
    drop(g);
    assert!((value_of(&vals, 11) - 999.0).abs() < 0.01, "从机寄存器未收到写入: {vals:?}");

    // ---- 4. 写输入区：协议只读，应被拒绝 ----
    let (r_tx, r_rx) = tokio::sync::oneshot::channel();
    tx.send(MasterMsg::Write { device: master_rt.clone(), point_id: 22, value: 1.0, reply: r_tx })
        .await
        .unwrap();
    let err = r_rx.await.unwrap().unwrap_err();
    assert!(err.contains("只读"), "错误应说明只读: {err}");

    // ---- 5. 测试帧：FC03 读 holding 0..2 → 999.0 的字 ----
    let (r_tx, r_rx) = tokio::sync::oneshot::channel();
    tx.send(MasterMsg::TestFrame { unit: 7, pdu: vec![0x03, 0x00, 0x00, 0x00, 0x02], reply: r_tx })
        .await
        .unwrap();
    let resp = r_rx.await.unwrap().unwrap();
    let words = netlink_core::runtime::encode_words(999.0, DataType::F32);
    let mut expect = vec![0x03, 0x04];
    for w in &words {
        expect.extend_from_slice(&w.to_be_bytes());
    }
    assert_eq!(resp, expect, "RTU 测试帧应答不符");

    // ---- 收尾 ----
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
    assert!(!AtomicBool::new(false).load(Ordering::Relaxed)); // 占位避免未用导入
}

/// 从机超时故障（不应答）：主机统计超时计数 + last_error + 监视器「(超时无应答)」事件
#[tokio::test(flavor = "multi_thread")]
async fn rtu_master_timeout_marks_stat_and_monitor() {
    let master_rt = Arc::new(Mutex::new(DeviceRuntime::new(&master_device())));

    let manager = RuntimeManager::new();
    let mut frame_rx = manager.subscribe_frames();
    let ctx = Arc::new(ConnCtx {
        conn_id: 9,
        devices: vec![master_rt.clone()],
        clock: SimClock::new(),
        manager: manager.clone(),
    });

    let (_tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let io = FakeRtuIo {
        to_master: VecDeque::new(),
        pending: Vec::new(),
        slave: Arc::new(Mutex::new(DeviceRuntime::new(&slave_device()))),
        unit: 7,
        fault: FakeFault::Silent,
    };
    let task = tokio::spawn(serve_rtu_with(ctx, Box::new(io), 80, cancel.clone(), rx));

    let ok = wait_until(5000, || master_rt.lock().unwrap().timeout_count > 0).await;
    assert!(
        ok,
        "超时未计数: timeout_count={}",
        master_rt.lock().unwrap().timeout_count
    );
    assert_eq!(master_rt.lock().unwrap().last_error.as_deref(), Some("超时无应答"));

    let mut saw_timeout_note = false;
    while let Ok(ev) = frame_rx.try_recv() {
        if ev.note.as_deref().is_some_and(|n| n.contains("超时无应答")) {
            saw_timeout_note = true;
        }
    }
    assert!(saw_timeout_note, "监视器应出现「(超时无应答)」标注事件");

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
}

/// 从机异常码应答：主机失败计数 + last_error 带异常码
#[tokio::test(flavor = "multi_thread")]
async fn rtu_master_exception_marks_last_error() {
    let master_rt = Arc::new(Mutex::new(DeviceRuntime::new(&master_device())));

    let manager = RuntimeManager::new();
    let ctx = Arc::new(ConnCtx {
        conn_id: 9,
        devices: vec![master_rt.clone()],
        clock: SimClock::new(),
        manager: manager.clone(),
    });

    let (_tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let io = FakeRtuIo {
        to_master: VecDeque::new(),
        pending: Vec::new(),
        slave: Arc::new(Mutex::new(DeviceRuntime::new(&slave_device()))),
        unit: 7,
        fault: FakeFault::Exception(0x04),
    };
    let task = tokio::spawn(serve_rtu_with(ctx, Box::new(io), 300, cancel.clone(), rx));

    let ok = wait_until(5000, || master_rt.lock().unwrap().fail_count > 0).await;
    assert!(
        ok,
        "异常应答未计入失败: fail_count={}",
        master_rt.lock().unwrap().fail_count
    );
    assert_eq!(master_rt.lock().unwrap().last_error.as_deref(), Some("异常应答 code=0x04"));

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
}

/// 从机脏帧（坏 CRC）：主机失败计数 + last_error（回归：此前 RTU 路径 CRC 错误不计统计）
#[tokio::test(flavor = "multi_thread")]
async fn rtu_master_corrupt_crc_marks_last_error() {
    let master_rt = Arc::new(Mutex::new(DeviceRuntime::new(&master_device())));

    let manager = RuntimeManager::new();
    let ctx = Arc::new(ConnCtx {
        conn_id: 9,
        devices: vec![master_rt.clone()],
        clock: SimClock::new(),
        manager: manager.clone(),
    });

    let (_tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let io = FakeRtuIo {
        to_master: VecDeque::new(),
        pending: Vec::new(),
        slave: Arc::new(Mutex::new(DeviceRuntime::new(&slave_device()))),
        unit: 7,
        fault: FakeFault::CorruptCrc,
    };
    let task = tokio::spawn(serve_rtu_with(ctx, Box::new(io), 300, cancel.clone(), rx));

    let ok = wait_until(5000, || master_rt.lock().unwrap().fail_count > 0).await;
    assert!(
        ok,
        "CRC 错误未计入失败: fail_count={}",
        master_rt.lock().unwrap().fail_count
    );
    assert_eq!(master_rt.lock().unwrap().last_error.as_deref(), Some("CRC 校验失败"));

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
}

/// serial_master 传输的序列化往返（工程文件兼容性）
#[test]
fn serial_master_transport_serde_roundtrip() {
    let mut p = ProjectSpec::demo();
    p.connections[2].transport = Transport::SerialMaster {
        serial_port: "COM5".into(),
        baud: 19200,
        parity: netlink_core::model::Parity::Even,
        stop_bits: 2,
        timeout_ms: 500,
    };
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains("\"serial_master\""));
    let back: ProjectSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(back.connections[2].transport, p.connections[2].transport);

    // 老工程（无 serial_master）仍可解析
    let legacy = json.replace("\"kind\":\"serial_master\"", "\"kind\":\"serial\"");
    let legacy: ProjectSpec = serde_json::from_str(&legacy)
        .expect("旧版 serial 工程应可解析（serial_master 字段被忽略）");
    assert!(matches!(legacy.connections[2].transport, Transport::Serial { .. }));
}

/// Connection 构造辅助（避免未用告警：rtu 主机连接形态）
#[allow(dead_code)]
fn _unused_conn() -> Connection {
    Connection {
        id: 1,
        name: "rtu-master".into(),
        transport: Transport::SerialMaster {
            serial_port: "COM3".into(),
            baud: 9600,
            parity: netlink_core::model::Parity::None,
            stop_bits: 1,
            timeout_ms: 1000,
        },
        devices: vec![],
    }
}
