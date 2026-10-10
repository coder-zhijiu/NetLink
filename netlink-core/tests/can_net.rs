//! CAN 转以太网（ZLG CANET）集成测试：
//! 本地起假 CANET server（说同一 13 字节透传协议，见 zlg_canet.rs 手册摘录）
//! 驱动真实 serve_can_net，覆盖：周期帧到达网关、网关回帧进接收聚合、
//! 断线退避重连与链路状态推送、手动发送走重连后的连接、启动即成功
//! （连接不可达只转「重连中」徽章，不阻塞启动）。

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use netlink_core::model::{CanNode, CanPeriodicFrame, Connection, Transport};
use netlink_core::protocol::can::zlg_canet::{decode_frame, encode_frame, CANET_FRAME_LEN};
use netlink_core::protocol::can::zlg_canet::serve_can_net;
use netlink_core::protocol::can::{CanAgg, CanCtx, CanFrame, CanMsg, CanNodeRuntime};
use netlink_core::runtime::{ConnStateKind, RuntimeManager};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

async fn wait_until<F: Fn() -> bool>(ms: u64, f: F) -> bool {
    for _ in 0..(ms / 20) {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

fn node_of(id: u64, frames: Vec<CanPeriodicFrame>) -> CanNode {
    CanNode { id, name: format!("节点{id}"), enabled: true, frames }
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

/// 阻塞读一帧（13 字节定长）
fn read_frame(s: &mut TcpStream) -> CanFrame {
    let mut b = [0u8; CANET_FRAME_LEN];
    s.read_exact(&mut b).expect("读完整帧");
    decode_frame(&b).expect("合法帧")
}

/// 假网关线程：
/// 连接 1 —— 读 2 个周期帧、回发 1 帧（0x110，聚合数据源）、300ms 后主动断开；
/// 连接 2 —— 重连后持续读帧直到收到手动帧 0x777 或客户端断开。
fn fake_canet_server(listener: TcpListener, received: Arc<Mutex<Vec<CanFrame>>>) {
    use std::io::Write;

    // ---- 连接 1 ----
    let (mut s1, _) = listener.accept().expect("首个连接");
    received.lock().unwrap().push(read_frame(&mut s1));
    received.lock().unwrap().push(read_frame(&mut s1));
    let echo = CanFrame { id: 0x110, extended: false, remote: false, data: vec![0xAB] };
    s1.write_all(&encode_frame(&echo)).expect("回发帧");
    std::thread::sleep(Duration::from_millis(300));
    let _ = s1.shutdown(std::net::Shutdown::Both);

    // ---- 连接 2（重连后） ----
    let (mut s2, _) = listener.accept().expect("重连");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let mut b = [0u8; CANET_FRAME_LEN];
        match s2.read_exact(&mut b) {
            Ok(()) => {
                if let Some(f) = decode_frame(&b) {
                    let id = f.id;
                    received.lock().unwrap().push(f);
                    if id == 0x777 {
                        break; // 收到手动帧：任务完成
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break, // 客户端断开（测试结束）
        }
    }
}

/// 端到端：周期帧 → 网关；网关回帧 → 聚合；断线 → Reconnecting → 重连 → Connected；
/// 手动发送走重连后的连接
#[tokio::test(flavor = "multi_thread")]
async fn can_net_e2e_frames_reconnect_and_manual_send() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = Arc::new(Mutex::new(Vec::<CanFrame>::new()));
    let server = std::thread::spawn({
        let received = received.clone();
        move || fake_canet_server(listener, received)
    });

    // 引擎侧：与 can_master.rs 的 Harness 同构（直驱 serve_can_net）
    let manager = RuntimeManager::new();
    let node = node_of(1, vec![frame_def(11, 0x123, vec![0x42], 100)]);
    let node_rts: Vec<Arc<Mutex<CanNodeRuntime>>> = vec![Arc::new(Mutex::new(CanNodeRuntime::new(&node)))];
    let agg = Arc::new(Mutex::new(CanAgg::default()));
    let ctx = Arc::new(CanCtx {
        conn_id: 9,
        nodes: node_rts,
        manager: manager.clone(),
        agg: agg.clone(),
    });
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(serve_can_net(ctx, "127.0.0.1".into(), port, 1000, cancel.clone(), cmd_rx));

    // panic 兜底取消：blocking 线程不退出会卡死 runtime drop（M1 的教训）
    struct CancelGuard<'a>(&'a CancellationToken);
    impl Drop for CancelGuard<'_> {
        fn drop(&mut self) {
            self.0.cancel();
        }
    }
    let _guard = CancelGuard(&cancel);

    // ---- 1. 周期帧到达网关（逐字节内容） ----
    let ok = wait_until(8000, || received.lock().unwrap().iter().filter(|f| f.id == 0x123).count() >= 2).await;
    assert!(ok, "网关未收到周期帧: {:?}", received.lock().unwrap().len());
    let first = received.lock().unwrap().iter().find(|f| f.id == 0x123).cloned().unwrap();
    assert_eq!(first.data, vec![0x42]);

    // ---- 2. 网关回帧进接收聚合 ----
    let ok = wait_until(4000, || {
        agg.lock().unwrap().snapshot().rows.iter().any(|r| r.frame_id == 0x110 && r.count >= 1)
    })
    .await;
    assert!(ok, "网关回帧未进聚合: {:?}", agg.lock().unwrap().snapshot().rows);

    // ---- 3. 网关断开 → Reconnecting（带原因） ----
    let ok = wait_until(6000, || {
        manager.snapshot_conn_states().iter().any(|(_, k, _)| *k == ConnStateKind::Reconnecting)
    })
    .await;
    assert!(ok, "断线未触发重连徽章: {:?}", manager.snapshot_conn_states());

    // ---- 4. 退避后重连成功 → Connected，周期帧继续 ----
    let ok = wait_until(8000, || {
        manager.snapshot_conn_states().iter().any(|(_, k, _)| *k == ConnStateKind::Connected)
    })
    .await;
    assert!(ok, "未重连成功: {:?}", manager.snapshot_conn_states());
    let ok = wait_until(6000, || received.lock().unwrap().iter().filter(|f| f.id == 0x123).count() >= 4).await;
    assert!(ok, "重连后周期帧未续发: {:?}", received.lock().unwrap().len());

    // ---- 5. 手动发送（走重连后的连接，插队语义） ----
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(CanMsg::SendFrame {
            frame: CanFrame { id: 0x777, extended: false, remote: false, data: vec![0xDE, 0xAD] },
            reply: reply_tx,
        })
        .await
        .unwrap();
    reply_rx.await.unwrap().expect("手动发送应成功");
    let ok = wait_until(6000, || received.lock().unwrap().iter().any(|f| f.id == 0x777)).await;
    assert!(ok, "网关未收到手动帧: {:?}", received.lock().unwrap().len());

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
    // 网关线程自行退出（客户端断开后 read_exact 报错）
    let _ = server.join();
}

/// 连接不可达：启动仍成功（TCP 连接循环异步退避），徽章转「重连中」带原因
#[tokio::test(flavor = "multi_thread")]
async fn can_net_start_succeeds_when_unreachable() {
    // 拿一个空闲端口后立即释放（保证无监听）
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };

    let manager = RuntimeManager::new();
    let conn = Connection {
        id: 1,
        name: "不可达网关".into(),
        transport: Transport::CanNet { host: "127.0.0.1".into(), port, channel: 0, timeout_ms: 200 },
        devices: vec![],
        can_nodes: vec![],
    };
    manager.start_connection(&conn).await.expect("can_net 启动应成功（连接循环异步重连）");

    let ok = wait_until(3000, || {
        manager.snapshot_conn_states().iter().any(|(id, k, _)| *id == 1 && *k == ConnStateKind::Reconnecting)
    })
    .await;
    assert!(ok, "不可达应转重连徽章: {:?}", manager.snapshot_conn_states());

    manager.stop_connection(1);
}
