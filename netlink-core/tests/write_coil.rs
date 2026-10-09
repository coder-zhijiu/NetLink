//! 复现用户报告：主机写「遥信合位」(线圈) 失败。
//! 用演示工程同构数据（从机含 coil 0/1，主机采集线圈 0），跑完整写路径。

use std::time::Duration;

use netlink_core::model::{ProjectSpec, Transport};
use netlink_core::runtime::RuntimeManager;

const PORT: u16 = 15990;

fn demo_on_free_port() -> ProjectSpec {
    let mut p = ProjectSpec::demo();
    // 只留 TCP 从机 + 主机两条连接，端口避开本机 502
    p.connections.truncate(3);
    p.connections[0].transport = Transport::TcpServer {
        listen_addr: "127.0.0.1".into(),
        port: PORT,
    };
    p.connections[2].transport = Transport::TcpClient {
        host: "127.0.0.1".into(),
        port: PORT,
        timeout_ms: 1000,
    };
    p
}

async fn wait_until<F: Fn() -> bool>(ms: u64, f: F) -> bool {
    for _ in 0..(ms / 100) {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    f()
}

#[tokio::test(flavor = "multi_thread")]
async fn master_write_coil_point_succeeds() {
    let p = demo_on_free_port();
    let mgr = RuntimeManager::new();

    mgr.start_connection(&p.connections[0]).await.unwrap();
    mgr.start_connection(&p.connections[2]).await.unwrap();

    // 等主机首轮采集：遥信合位（主机点位 id=131）应采到从机的 1
    let ok = wait_until(8000, || {
        mgr.snapshot_values(301).iter().any(|(id, v)| *id == 131 && *v == 1.0)
    })
    .await;
    assert!(ok, "主机未采到线圈初始值: {:?}", mgr.snapshot_values(301));

    // 主机写线圈 0 → 0（演示工程主机 遥信合位 = 点 id 131）
    let r = mgr.write_point(301, 131, 0.0).await;
    assert!(r.is_ok(), "写线圈失败: {:?}", r.err());

    // 从机侧（1号测控装置 遥信合位 = 点 id 113）应变为 0
    let ok = wait_until(3000, || {
        mgr.snapshot_values(101).iter().any(|(id, v)| *id == 113 && *v == 0.0)
    })
    .await;
    assert!(ok, "从机线圈未更新: {:?}", mgr.snapshot_values(101));

    // 写回 1 也应成功
    let r = mgr.write_point(301, 131, 1.0).await;
    assert!(r.is_ok(), "写线圈 1 失败: {:?}", r.err());

    mgr.stop_all();
}
