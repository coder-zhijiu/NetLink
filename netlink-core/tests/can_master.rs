//! CAN 直采直控集成测试：
//! 用内存假总线（实现 CanIo）驱动真实的主机主循环（serve_can_with），
//! 覆盖：周期报文按时发送与帧内容、报文事件（note/device_id 归属）、
//! 手动发送插队、节点启停（暂停周期报文）、接收聚合统计、
//! IO 错误的链路状态徽章、工程文件 v2→v3 兼容与未实现后端的启动拒绝。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netlink_core::model::{CanNode, CanPeriodicFrame, Connection, ProjectSpec, Transport};
use netlink_core::protocol::FrameEvent;
use netlink_core::protocol::can::{
    serve_can_with, CanAgg, CanAggSnapshot, CanBusStatus, CanCtx, CanFrame, CanIo, CanMsg,
    CanNodeRuntime,
};
use netlink_core::runtime::RuntimeManager;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 内存假总线：测试注入接收帧（inject 共享句柄随时可塞）；
/// 空闲时 recv 睡满超时（与真实串口行为一致，主循环节拍同构）
struct FakeCanIo {
    inject: Arc<Mutex<VecDeque<CanFrame>>>,
    sent: Arc<Mutex<Vec<CanFrame>>>,
    fail: Arc<AtomicBool>,
}

impl CanIo for FakeCanIo {
    fn send(&mut self, f: &CanFrame) -> Result<(), String> {
        if self.fail.load(Ordering::Acquire) {
            return Err("串口写入失败(模拟)".into());
        }
        self.sent.lock().unwrap().push(f.clone());
        Ok(())
    }
    fn recv(&mut self, timeout: Duration) -> Result<Option<CanFrame>, String> {
        if let Some(f) = self.inject.lock().unwrap().pop_front() {
            return Ok(Some(f));
        }
        if self.fail.load(Ordering::Acquire) {
            return Err("串口读取失败(模拟)".into());
        }
        std::thread::sleep(timeout);
        Ok(None)
    }
    fn status(&mut self) -> CanBusStatus {
        CanBusStatus::default()
    }
}

fn frame_def(id: u64, frame_id: u32, data: Vec<u8>, period_ms: u32) -> CanPeriodicFrame {
    CanPeriodicFrame {
        id,
        frame_id,
        extended: false,
        remote: false,
        data,
        period_ms,
        enabled: true,
        note: None,
    }
}

fn node_of(id: u64, enabled: bool, frames: Vec<CanPeriodicFrame>) -> CanNode {
    CanNode { id, name: format!("节点{id}"), enabled, frames }
}

/// 组装一条 CAN 连接上下文 + 假总线；返回可驱动句柄。
/// Drop 兜底取消：断言 panic 展开时也能停掉主循环线程，
/// 否则 blocking 任务不结束会卡住 tokio runtime 的关闭。
struct Harness {
    manager: RuntimeManager,
    node_rts: Vec<Arc<Mutex<CanNodeRuntime>>>,
    inject: Arc<Mutex<VecDeque<CanFrame>>>,
    sent: Arc<Mutex<Vec<CanFrame>>>,
    fail: Arc<AtomicBool>,
    agg: Arc<Mutex<CanAgg>>,
    frames_rx: tokio::sync::broadcast::Receiver<FrameEvent>,
    cmd_tx: mpsc::Sender<CanMsg>,
    cancel: CancellationToken,
    #[allow(dead_code)] // 持有句柄仅为保持任务存活；取消经 cancel 表达
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn start_harness(conn_id: u64, nodes: Vec<CanNode>) -> Harness {
    let manager = RuntimeManager::new();
    // 先订阅再启动（broadcast 无订阅者即丢），周期帧事件从第一拍就可观测
    let frames_rx = manager.subscribe_frames();
    let node_rts: Vec<Arc<Mutex<CanNodeRuntime>>> = nodes
        .iter()
        .map(|n| Arc::new(Mutex::new(CanNodeRuntime::new(n))))
        .collect();
    let inject = Arc::new(Mutex::new(VecDeque::new()));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let fail = Arc::new(AtomicBool::new(false));
    let agg = Arc::new(Mutex::new(CanAgg::default()));
    let ctx = Arc::new(CanCtx {
        conn_id,
        nodes: node_rts.clone(),
        manager: manager.clone(),
        agg: agg.clone(),
    });
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let io = FakeCanIo { inject: inject.clone(), sent: sent.clone(), fail: fail.clone() };
    let task = tokio::spawn(serve_can_with(ctx, Box::new(io), cancel.clone(), cmd_rx));
    Harness {
        manager,
        node_rts,
        inject,
        sent,
        fail,
        agg,
        frames_rx,
        cmd_tx,
        cancel,
        task,
    }
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

fn sent_count(sent: &Arc<Mutex<Vec<CanFrame>>>, frame_id: u32) -> usize {
    sent.lock().unwrap().iter().filter(|f| f.id == frame_id).count()
}

fn agg_snapshot(agg: &Arc<Mutex<CanAgg>>) -> CanAggSnapshot {
    agg.lock().unwrap().snapshot()
}

/// 周期报文：按时发送、帧内容逐字节正确、事件归属到节点（note 携带 ID/DLC）
#[tokio::test(flavor = "multi_thread")]
async fn periodic_frames_sent_on_schedule() {
    let mut h = start_harness(
        9,
        vec![
            node_of(1, true, vec![frame_def(11, 0x123, vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88], 100)]),
            node_of(2, true, vec![frame_def(12, 0x201, vec![0x01, 0x02], 250)]),
        ],
    );

    tokio::time::sleep(Duration::from_millis(1300)).await;

    let n_fast = sent_count(&h.sent, 0x123);
    let n_slow = sent_count(&h.sent, 0x201);
    assert!(n_fast >= 8, "100ms 周期帧发送过少: {n_fast}");
    assert!(n_slow >= 3, "250ms 周期帧发送过少: {n_slow}");

    // 帧内容逐字节正确（取第一帧比对即可，内容恒定）
    let first = h.sent.lock().unwrap().iter().find(|f| f.id == 0x123).cloned().unwrap();
    assert_eq!(first.data, vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    assert!(!first.extended && !first.remote);

    // 报文事件：出帧带节点归属与 note
    let mut saw_event = false;
    while let Ok(ev) = h.frames_rx.try_recv() {
        if ev.conn_id == 9 && ev.dir == "out" && ev.device_id == Some(1) {
            assert_eq!(ev.note.as_deref(), Some("ID=0x123 DLC=8"));
            assert_eq!(ev.hex, "11 22 33 44 55 66 77 88");
            saw_event = true;
        }
    }
    assert!(saw_event, "未捕获周期帧的出帧事件");

    h.cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), &mut h.task).await;
}

/// 手动发送插队 + 节点启停（停用即暂停周期报文，恢复即续发）+ 链路状态 Connected
#[tokio::test(flavor = "multi_thread")]
async fn manual_send_node_toggle_and_conn_state() {
    let mut h = start_harness(9, vec![node_of(1, true, vec![frame_def(11, 0x123, vec![0xAA], 100)])]);

    tokio::time::sleep(Duration::from_millis(350)).await;

    // ---- 1. 手动发送：命令插队，应答 Ok，帧到达总线 ----
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    h.cmd_tx
        .send(CanMsg::SendFrame {
            frame: CanFrame { id: 0x777, extended: false, remote: false, data: vec![0xDE, 0xAD] },
            reply: reply_tx,
        })
        .await
        .unwrap();
    reply_rx.await.unwrap().expect("手动发送应成功");
    assert!(h.sent.lock().unwrap().iter().any(|f| f.id == 0x777 && f.data == vec![0xDE, 0xAD]));

    // ---- 2. 停用节点：周期报文暂停 ----
    h.node_rts[0].lock().unwrap().enabled = false;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let frozen = sent_count(&h.sent, 0x123);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(sent_count(&h.sent, 0x123), frozen, "停用节点后周期帧仍在发送");

    // ---- 3. 重新启用：续发 ----
    h.node_rts[0].lock().unwrap().enabled = true;
    let ok = wait_until(1500, || sent_count(&h.sent, 0x123) > frozen).await;
    assert!(ok, "重新启用后周期帧未续发");

    // ---- 4. 链路状态：进入主循环即 Connected ----
    let states = h.manager.snapshot_conn_states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].0, 9);
    assert_eq!(states[0].1, netlink_core::runtime::ConnStateKind::Connected);

    h.cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), &mut h.task).await;
}

/// 接收聚合：同 ID 合并计数、平均周期与最近数据；不同 ID 各自成行（按 ID 排序）
#[tokio::test(flavor = "multi_thread")]
async fn rx_aggregation_counts_and_periods() {
    let mut h = start_harness(9, vec![node_of(1, true, vec![])]);

    // 批次 1：三帧 0x110（数据 0A）
    for _ in 0..3 {
        h.inject.lock().unwrap().push_back(CanFrame { id: 0x110, extended: false, remote: false, data: vec![0x0A] });
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    // 批次 2：两帧 0x110（数据 0B，验证 last_data）+ 一帧 0x220（扩展帧）
    for _ in 0..2 {
        h.inject.lock().unwrap().push_back(CanFrame { id: 0x110, extended: false, remote: false, data: vec![0x0B] });
    }
    h.inject.lock().unwrap().push_back(CanFrame { id: 0x220, extended: true, remote: false, data: vec![0x01, 0x02] });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let snap = agg_snapshot(&h.agg);
    assert_eq!(snap.rows.len(), 2);
    // 排序：0x110 在前，0x220 在后
    assert_eq!(snap.rows[0].frame_id, 0x110);
    assert_eq!(snap.rows[0].count, 5);
    assert!(snap.rows[0].avg_period_ms > 0.0 && snap.rows[0].avg_period_ms < 500.0, "平均周期异常: {}", snap.rows[0].avg_period_ms);
    assert_eq!(snap.rows[0].last_data, "0B");
    assert!(snap.rows[0].last_ts > 0);
    assert_eq!(snap.rows[1].frame_id, 0x220);
    assert_eq!(snap.rows[1].count, 1);
    assert!(snap.rows[1].extended);
    assert_eq!(snap.rows[1].last_data, "01 02");
    assert_eq!(snap.rx_total, 6);

    h.cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), &mut h.task).await;
}

/// IO 错误：链路徽章转 Reconnecting（带原因），恢复后回 Connected；错误计数累计
#[tokio::test(flavor = "multi_thread")]
async fn io_error_sets_reconnecting_badge() {
    let mut h = start_harness(9, vec![node_of(1, true, vec![frame_def(11, 0x123, vec![0x00], 100)])]);

    tokio::time::sleep(Duration::from_millis(200)).await;
    h.fail.store(true, Ordering::Release);
    let ok = wait_until(1500, || {
        h.manager.snapshot_conn_states().iter().any(|(_, k, _)| {
            *k == netlink_core::runtime::ConnStateKind::Reconnecting
        })
    })
    .await;
    assert!(ok, "IO 错误未触发重连徽章");

    h.fail.store(false, Ordering::Release);
    let ok = wait_until(1500, || {
        h.manager.snapshot_conn_states().iter().any(|(_, k, _)| {
            *k == netlink_core::runtime::ConnStateKind::Connected
        })
    })
    .await;
    assert!(ok, "IO 恢复后未回到已连接");
    let snap = agg_snapshot(&h.agg);
    assert!(snap.err_total > 0, "错误计数未累计");

    h.cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), &mut h.task).await;
}

/// 工程文件兼容：v2 旧工程（无 can_nodes）读入不丢数据；
/// UsbCan 全字段缺省（只给 kind）按默认值补齐；v3 完整往返
#[test]
fn project_serde_v2_compat_and_can_roundtrip() {
    // v2 旧工程：Connection 无 can_nodes 字段
    let legacy = r#"{
        "schema_version": 2,
        "name": "旧工程",
        "connections": [{
            "id": 1, "name": "485 总线",
            "transport": {"kind": "serial", "serial_port": "COM3", "baud": 9600},
            "devices": []
        }]
    }"#;
    let p: ProjectSpec = serde_json::from_str(legacy).expect("v2 工程应可读入");
    assert!(p.connections[0].can_nodes.is_empty());
    assert_eq!(p.schema_version, 2);

    // UsbCan 最小 JSON：backend/bitrate/serial_baud 等全部走默认
    let minimal = r#"{
        "schema_version": 3, "name": "m",
        "connections": [{
            "id": 1, "name": "CAN",
            "transport": {"kind": "usb_can", "serial_port": "COM9"},
            "devices": []
        }]
    }"#;
    let p: ProjectSpec = serde_json::from_str(minimal).expect("最小 usb_can 应可读入");
    match &p.connections[0].transport {
        Transport::UsbCan { backend, bitrate, serial_baud, channel, .. } => {
            assert_eq!(backend, "slcan");
            assert_eq!(*bitrate, 500_000);
            assert_eq!(*serial_baud, 115_200);
            assert_eq!(*channel, 0);
        }
        _ => panic!("传输方式应为 usb_can"),
    }

    // demo（含 slcan CAN 连接）完整往返
    let json = serde_json::to_string(&ProjectSpec::demo()).unwrap();
    assert!(json.contains("\"usb_can\""));
    let back: ProjectSpec = serde_json::from_str(&json).unwrap();
    let can = &back.connections[3];
    assert_eq!(can.can_nodes.len(), 2);
    assert_eq!(can.can_nodes[0].frames[0].frame_id, 0x123);
    assert_eq!(back.schema_version, 3);
}

/// 未实现后端在启动阶段即拒绝（明确报错而不是带病运行）
#[tokio::test]
async fn unsupported_can_backends_rejected_at_start() {
    let manager = RuntimeManager::new();

    // zlgcan：后端未实现
    let conn = Connection {
        id: 1,
        name: "zlgcan".into(),
        transport: Transport::UsbCan {
            backend: "zlgcan".into(),
            device_index: 0,
            channel: 0,
            bitrate: 500_000,
            serial_port: None,
            serial_baud: 115_200,
        },
        devices: vec![],
        can_nodes: vec![],
    };
    let e = manager.start_connection(&conn).await.unwrap_err();
    assert!(e.contains("尚未实现"), "zlgcan 应被拒绝: {e}");

    // slcan 未配串口
    let conn = Connection {
        id: 3,
        name: "无串口".into(),
        transport: Transport::UsbCan {
            backend: "slcan".into(),
            device_index: 0,
            channel: 0,
            bitrate: 500_000,
            serial_port: None,
            serial_baud: 115_200,
        },
        devices: vec![],
        can_nodes: vec![],
    };
    let e = manager.start_connection(&conn).await.unwrap_err();
    assert!(e.contains("串口"), "缺串口应被拒绝: {e}");

    // 帧定义非法（数据超 8 字节）：预校验先于串口打开报出
    let conn = Connection {
        id: 4,
        name: "坏帧".into(),
        transport: Transport::UsbCan {
            backend: "slcan".into(),
            device_index: 0,
            channel: 0,
            bitrate: 500_000,
            serial_port: Some("COM_NOT_EXIST".into()),
            serial_baud: 115_200,
        },
        devices: vec![],
        can_nodes: vec![node_of(1, true, vec![CanPeriodicFrame {
            id: 11,
            frame_id: 0x123,
            extended: false,
            remote: false,
            data: vec![0; 9],
            period_ms: 100,
            enabled: true,
            note: None,
        }])],
    };
    let e = manager.start_connection(&conn).await.unwrap_err();
    assert!(e.contains("超长"), "非法帧定义应被预校验拦截: {e}");
}
