//! Modbus TCP 从机端到端集成测试：
//! 用裸 TCP 客户端（MBAP 帧）直连引擎，覆盖正常读写、异常应答、故障注入三件套。
//!
//! 测试使用 127.0.0.1 上的独立端口（1502x 段），不依赖任何桌面 UI。

use std::time::Duration;

use netlink_core::model::{
    Area, Connection, DataType, Device, FaultConfig, FaultKind, Point, ProjectSpec, Transport,
    ValueSource,
};
use netlink_core::runtime::RuntimeManager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_project(port: u16, fault: FaultConfig) -> ProjectSpec {
    ProjectSpec {
        schema_version: 1,
        name: "e2e-test".into(),
        connections: vec![Connection {
            id: 1,
            name: "test-tcp".into(),
            transport: Transport::TcpServer {
                listen_addr: "127.0.0.1".into(),
                port,
            },
            devices: vec![Device {
                id: 1,
                name: "测试装置".into(),
                slave_id: 1,
                enabled: true,
                fault,
                poll_period_ms: 200,
                points: vec![
                    Point {
                        id: 1,
                        name: "有功设定".into(),
                        area: Area::HoldingRegister,
                        address: 0,
                        data_type: DataType::F32,
                        source: ValueSource::Static { value: 250.0 },
                        note: None,
                    },
                    Point {
                        id: 2,
                        name: "遥信合位".into(),
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

/// 带重试的连接（监听任务可能尚未完成 bind）
async fn connect_with_retry(port: u16) -> TcpStream {
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("连接 127.0.0.1:{port} 失败");
}

/// 发一条请求，读回应答 PDU
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
async fn normal_flow_read_write_and_exceptions() {
    let mgr = RuntimeManager::new();
    mgr.start(&test_project(15020, FaultConfig::default()))
        .await
        .unwrap();
    let mut s = connect_with_retry(15020).await;

    // FC03 读保持寄存器 addr=0 qty=2：有功设定 250.0(f32) → 0x437A0000 → 大端两字
    let resp = transact(&mut s, 1, 1, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x03, 0x04, 0x43, 0x7A, 0x00, 0x00]);

    // FC06 写 0x1234 到 addr=0：应答回显
    let resp = transact(&mut s, 2, 1, &[0x06, 0x00, 0x00, 0x12, 0x34]).await.unwrap();
    assert_eq!(resp, vec![0x06, 0x00, 0x00, 0x12, 0x34]);

    // FC03 读回 1 个字：写入生效
    let resp = transact(&mut s, 3, 1, &[0x03, 0x00, 0x00, 0x00, 0x01]).await.unwrap();
    assert_eq!(resp, vec![0x03, 0x02, 0x12, 0x34]);

    // FC01 读线圈 addr=0 qty=1：合位=1
    let resp = transact(&mut s, 4, 1, &[0x01, 0x00, 0x00, 0x00, 0x01]).await.unwrap();
    assert_eq!(resp, vec![0x01, 0x01, 0x01]);

    // 未知从机地址 → 网关异常 0x0B
    let resp = transact(&mut s, 5, 9, &[0x03, 0x00, 0x00, 0x00, 0x01]).await.unwrap();
    assert_eq!(resp, vec![0x83, 0x0B]);

    // 越界读 → 非法地址 0x02
    let resp = transact(&mut s, 6, 1, &[0x03, 0x00, 0x64, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x83, 0x02]);

    // 不支持的功能码 0x2B → 非法功能 0x01
    let resp = transact(&mut s, 7, 1, &[0x2B, 0x00, 0x00]).await.unwrap();
    assert_eq!(resp, vec![0xAB, 0x01]);

    mgr.stop_all();
}

#[tokio::test(flavor = "multi_thread")]
async fn fault_timeout_no_response() {
    let mgr = RuntimeManager::new();
    mgr.start(&test_project(
        15021,
        FaultConfig {
            kind: FaultKind::Timeout,
            delay_ms: None,
            exception_code: None,
        },
    ))
    .await
    .unwrap();
    let mut s = connect_with_retry(15021).await;

    // 发请求后 500ms 内不应有任何应答字节
    let mut frame = Vec::new();
    frame.extend_from_slice(&8u16.to_be_bytes());
    frame.extend_from_slice(&0u16.to_be_bytes());
    frame.extend_from_slice(&6u16.to_be_bytes());
    frame.push(1);
    frame.extend_from_slice(&[0x03, 0x00, 0x00, 0x00, 0x02]);
    s.write_all(&frame).await.unwrap();

    let mut buf = [0u8; 1];
    let r = tokio::time::timeout(Duration::from_millis(500), s.read(&mut buf)).await;
    assert!(r.is_err(), "超时故障不应有应答，却读到了字节");

    mgr.stop_all();
}

#[tokio::test(flavor = "multi_thread")]
async fn fault_delay_response_after_configured_ms() {
    let mgr = RuntimeManager::new();
    mgr.start(&test_project(
        15022,
        FaultConfig {
            kind: FaultKind::Delay,
            delay_ms: Some(200),
            exception_code: None,
        },
    ))
    .await
    .unwrap();
    let mut s = connect_with_retry(15022).await;

    let t0 = std::time::Instant::now();
    let resp = transact(&mut s, 1, 1, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    let elapsed = t0.elapsed();

    // 应答内容正常，但至少延迟了 200ms
    assert_eq!(resp, vec![0x03, 0x04, 0x43, 0x7A, 0x00, 0x00]);
    assert!(elapsed >= Duration::from_millis(200), "应答过早: {elapsed:?}");

    mgr.stop_all();
}

#[tokio::test(flavor = "multi_thread")]
async fn fault_exception_code_response() {
    let mgr = RuntimeManager::new();
    mgr.start(&test_project(
        15023,
        FaultConfig {
            kind: FaultKind::Exception,
            delay_ms: None,
            exception_code: Some(0x04),
        },
    ))
    .await
    .unwrap();
    let mut s = connect_with_retry(15023).await;

    // 任意请求 → 固定异常码 0x04（从站设备故障）
    let resp = transact(&mut s, 1, 1, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x83, 0x04]);

    mgr.stop_all();
}

/// 断连故障：收到请求后连接被立即关闭（读到 EOF），监听保持、可再次接入
#[tokio::test(flavor = "multi_thread")]
async fn fault_disconnect_closes_connection_and_reaccepts() {
    let mgr = RuntimeManager::new();
    mgr.start(&test_project(
        15024,
        FaultConfig {
            kind: FaultKind::Disconnect,
            delay_ms: None,
            exception_code: None,
        },
    ))
    .await
    .unwrap();

    // 两轮连接都应被断开（监听未随 socket 关闭而退出）
    for round in 0..2u16 {
        let mut s = connect_with_retry(15024).await;
        let mut frame = Vec::new();
        frame.extend_from_slice(&(20 + round).to_be_bytes());
        frame.extend_from_slice(&0u16.to_be_bytes());
        frame.extend_from_slice(&6u16.to_be_bytes());
        frame.push(1);
        frame.extend_from_slice(&[0x03, 0x00, 0x00, 0x00, 0x02]);
        s.write_all(&frame).await.unwrap();

        // 不应答、直接关闭：读到 EOF（0 字节）
        let mut buf = [0u8; 1];
        let n = tokio::time::timeout(Duration::from_millis(2000), s.read(&mut buf))
            .await
            .expect("关闭应立即发生")
            .unwrap();
        assert_eq!(n, 0, "断连故障应关闭连接（EOF）");
    }

    mgr.stop_all();
}

/// 脏帧故障在 TCP 上兜底：Modbus TCP 帧无校验和，按正常应答
#[tokio::test(flavor = "multi_thread")]
async fn fault_corrupt_on_tcp_falls_back_to_normal() {
    let mgr = RuntimeManager::new();
    mgr.start(&test_project(
        15025,
        FaultConfig {
            kind: FaultKind::Corrupt,
            delay_ms: None,
            exception_code: None,
        },
    ))
    .await
    .unwrap();
    let mut s = connect_with_retry(15025).await;

    let resp = transact(&mut s, 1, 1, &[0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    assert_eq!(resp, vec![0x03, 0x04, 0x43, 0x7A, 0x00, 0x00]);

    mgr.stop_all();
}
