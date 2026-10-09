//! 单连接启停 / 端口占用报错 / 在线启停设备的集成测试。
//! 对应前端设备树的 ▶/■ 逐连接、逐设备控制。

use std::time::Duration;

use netlink_core::model::{
    Area, Connection, DataType, Device, FaultConfig, Point, ProjectSpec, Transport, ValueSource,
};
use netlink_core::runtime::RuntimeManager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn one_conn_project(port: u16) -> ProjectSpec {
    ProjectSpec {
        schema_version: 1,
        name: "lifecycle-test".into(),
        connections: vec![Connection {
            id: 1,
            name: "test-tcp".into(),
            transport: Transport::TcpServer {
                listen_addr: "127.0.0.1".into(),
                port,
            },
            devices: vec![
                Device {
                    id: 1,
                    name: "测试装置A".into(),
                    slave_id: 1,
                    enabled: true,
                    fault: FaultConfig::default(),
                    poll_period_ms: 200,
                    points: vec![Point {
                        id: 1,
                        name: "有功设定".into(),
                        area: Area::HoldingRegister,
                        address: 0,
                        data_type: DataType::F32,
                        source: ValueSource::Static { value: 250.0 },
                        note: None,
                    }],
                },
                Device {
                    id: 2,
                    name: "测试装置B".into(),
                    slave_id: 2,
                    enabled: true,
                    fault: FaultConfig::default(),
                    poll_period_ms: 200,
                    points: vec![Point {
                        id: 2,
                        name: "电压".into(),
                        area: Area::HoldingRegister,
                        address: 0,
                        data_type: DataType::F32,
                        source: ValueSource::Static { value: 220.0 },
                        note: None,
                    }],
                },
            ],
        }],
    }
}

async fn connect_with_retry(port: u16) -> TcpStream {
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("连接 127.0.0.1:{port} 失败");
}

async fn transact(s: &mut TcpStream, txn: u16, unit: u8, pdu: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut frame = Vec::with_capacity(7 + pdu.len());
    frame.extend_from_slice(&txn.to_be_bytes());
    frame.extend_from_slice(&0u16.to_be_bytes());
    frame.extend_from_slice(&((pdu.len() as u16 + 1).to_be_bytes()));
    frame.push(unit);
    frame.extend_from_slice(pdu);
    s.write_all(&frame).await?;

    let mut header = [0u8; 7];
    s.read_exact(&mut header).await?;
    let len = u16::from_be_bytes([header[4], header[5]]) as usize;
    let mut resp = vec![0u8; len - 1];
    s.read_exact(&mut resp).await?;
    Ok(resp)
}

#[tokio::test(flavor = "multi_thread")]
async fn start_and_stop_single_connection() {
    let mgr = RuntimeManager::new();
    let project = one_conn_project(15030);

    // 未启动时设备在线启停应报错
    assert!(mgr.set_device_enabled(1, false).is_err());

    // 单独启动一条连接
    mgr.start_connection(&project.connections[0]).await.unwrap();
    assert_eq!(mgr.running_conn_ids(), vec![1]);
    assert!(mgr.is_running());

    let mut s = connect_with_retry(15030).await;
    let resp = transact(&mut s, 1, 1, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x03, 0x04, 0x43, 0x7A, 0x00, 0x00]);

    // 在线禁用设备：该从机地址按网关惯例回 0x0B
    mgr.set_device_enabled(1, false).unwrap();
    let resp = transact(&mut s, 2, 1, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x83, 0x0B]);

    // 重新启用恢复正常应答
    mgr.set_device_enabled(1, true).unwrap();
    let resp = transact(&mut s, 3, 1, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x03, 0x04, 0x43, 0x7A, 0x00, 0x00]);

    // 单独停止该连接：端口释放，运行状态清空
    assert!(mgr.stop_connection(1));
    assert!(mgr.running_conn_ids().is_empty());
    assert!(!mgr.is_running());
    assert!(!mgr.stop_connection(1), "重复停止应返回 false");

    // 停止后在线启停设备应报错
    assert!(mgr.set_device_enabled(1, true).is_err());

    // 端口已释放：可再次启动（重启语义）
    mgr.start_connection(&project.connections[0]).await.unwrap();
    let mut s2 = connect_with_retry(15030).await;
    let resp = transact(&mut s2, 4, 1, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x03, 0x04, 0x43, 0x7A, 0x00, 0x00]);
    mgr.stop_all();
}

#[tokio::test(flavor = "multi_thread")]
async fn start_single_device_does_not_start_siblings() {
    let mgr = RuntimeManager::new();
    let project = one_conn_project(15033);

    // 单台设备首启：连接带起但只有 1 号设备在线，2 号不参与
    mgr.start_connection_with(&project.connections[0], Some(vec![1]))
        .await
        .unwrap();
    assert_eq!(mgr.running_conn_ids(), vec![1]);
    assert_eq!(mgr.running_device_ids(), vec![1]);

    let mut s = connect_with_retry(15033).await;
    // 1 号（从机地址 1）正常应答 250.0(f32)
    let resp = transact(&mut s, 1, 1, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x03, 0x04, 0x43, 0x7A, 0x00, 0x00]);
    // 2 号（从机地址 2）未在线：按网关惯例回 0x0B
    let resp = transact(&mut s, 2, 2, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x83, 0x0B]);

    // 在线启用 2 号：应答变为该设备的 220.0(f32)
    mgr.set_device_enabled(2, true).unwrap();
    let mut devices = mgr.running_device_ids();
    devices.sort();
    assert_eq!(devices, vec![1, 2]);
    let resp = transact(&mut s, 3, 2, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x03, 0x04, 0x43, 0x5C, 0x00, 0x00]);

    // 重启为默认（按配置全员在线）
    mgr.start_connection(&project.connections[0]).await.unwrap();
    assert_eq!(mgr.running_device_ids(), vec![1, 2]);

    mgr.stop_all();
}

#[tokio::test(flavor = "multi_thread")]
async fn start_fails_when_port_occupied() {
    // 外部占用一个端口
    let holder = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = holder.local_addr().unwrap().port();

    let mgr = RuntimeManager::new();
    let project = one_conn_project(port);

    // 启动应显式报错（之前 bind 失败只打日志，启动却返回成功）
    let err = mgr.start_connection(&project.connections[0]).await.unwrap_err();
    assert!(err.contains("失败"), "错误信息应包含失败原因: {err}");

    // 失败后连接不应处于运行状态
    assert!(mgr.running_conn_ids().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_running_connection_reapplies_config() {
    let mgr = RuntimeManager::new();
    let project = one_conn_project(15031);
    mgr.start_connection(&project.connections[0]).await.unwrap();

    // 修改配置（换端口）后重启：新端口生效、旧端口释放
    let mut updated = project.connections[0].clone();
    updated.transport = Transport::TcpServer {
        listen_addr: "127.0.0.1".into(),
        port: 15032,
    };
    mgr.start_connection(&updated).await.unwrap();
    assert_eq!(mgr.running_conn_ids(), vec![1]);

    let mut s = connect_with_retry(15032).await;
    let resp = transact(&mut s, 1, 1, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x03, 0x04, 0x43, 0x7A, 0x00, 0x00]);

    // 旧端口不再监听
    assert!(
        TcpStream::connect(("127.0.0.1", 15031)).await.is_err(),
        "旧端口应已释放"
    );

    mgr.stop_all();
}
