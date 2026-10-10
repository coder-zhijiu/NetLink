//! Modbus 主从自闭环集成测试：
//! 用本工具自己的从机引擎当被测设备，主机引擎轮询采集它。
//! 覆盖：采集值正确、写点下发、测试帧、断线重连恢复。

use std::time::Duration;

use netlink_core::model::{
    Area, Connection, DataType, Device, FaultConfig, FaultKind, Point, ProjectSpec, Transport, ValueSource,
};
use netlink_core::runtime::{encode_words, ConnStateKind, RuntimeManager};

const PORT: u16 = 15040;
const PORT2: u16 = 15041;

fn point(id: u64, name: &str, area: Area, addr: u16, dt: DataType) -> Point {
    Point {
        id,
        name: name.into(),
        area,
        address: addr,
        data_type: dt,
        source: ValueSource::Static { value: 0.0 },
        note: None,
    }
}

/// 被采集的从机（端口 PORT）：holding 0 f32=123.5 / input 0 f32=456.25 / coil 0 = 1
fn slave_project(port: u16) -> ProjectSpec {
    ProjectSpec {
        schema_version: 2,
        name: "slave".into(),
        connections: vec![Connection {
            id: 4,
            name: "被采从机".into(),
            transport: Transport::TcpServer {
                listen_addr: "127.0.0.1".into(),
                port,
            },
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
                    Point {
                        id: 14,
                        name: "遥信 DI".into(),
                        area: Area::DiscreteInput,
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

/// 主机（连 PORT）：轮询 200ms，点表与从机一致
fn master_project(port: u16) -> ProjectSpec {
    ProjectSpec {
        schema_version: 2,
        name: "master".into(),
        connections: vec![Connection {
            id: 5,
            name: "采集主机".into(),
            transport: Transport::TcpClient {
                host: "127.0.0.1".into(),
                port,
                timeout_ms: 400,
            },
            can_nodes: vec![],
            devices: vec![Device {
                id: 902,
                name: "采集装置".into(),
                slave_id: 1,
                enabled: true,
                fault: FaultConfig::default(),
                poll_period_ms: 200,
                points: vec![
                    point(21, "设定值", Area::HoldingRegister, 0, DataType::F32),
                    point(22, "遥测", Area::InputRegister, 0, DataType::F32),
                    point(23, "遥信", Area::Coil, 0, DataType::U16),
                    point(24, "遥信 DI", Area::DiscreteInput, 0, DataType::U16),
                ],
            }],
        }],
    }
}

fn value_of(vals: &[(u64, f64)], point_id: u64) -> f64 {
    vals.iter().find(|(id, _)| *id == point_id).map(|(_, v)| *v).unwrap_or(f64::NAN)
}

/// 轮询等待直到条件满足或超时
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
async fn master_polls_writes_testframe_and_reconnects() {
    let slave = RuntimeManager::new();
    slave.start(&slave_project(PORT)).await.unwrap();

    let master = RuntimeManager::new();
    master.start(&master_project(PORT)).await.unwrap();

    // ---- 1. 采集值正确（主机连从机可能经历首轮失败重试，等待足够时间） ----
    let ok = wait_until(8000, || {
        let vals = master.snapshot_values(902);
        (value_of(&vals, 21) - 123.5).abs() < 0.01
            && (value_of(&vals, 22) - 456.25).abs() < 0.01
            && value_of(&vals, 23) == 1.0
    })
    .await;
    assert!(ok, "主机未采集到从机值: {:?}", master.snapshot_values(902));

    // 采集值必须稳定：连续 5 次采样不允许抖回 0（回归用例：仿真 tick 不得清零主机采集值）
    for i in 0..5 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let vals = master.snapshot_values(902);
        assert!(
            (value_of(&vals, 21) - 123.5).abs() < 0.01 && (value_of(&vals, 22) - 456.25).abs() < 0.01,
            "第 {} 次采样抖动: {vals:?}",
            i + 1
        );
    }

    let stats = master.snapshot_stats();
    let s = stats.iter().find(|(id, ..)| *id == 902).expect("stats 缺 902");
    assert!(s.1 > 0, "last_ok_ms 应已更新");
    assert_eq!(s.4, None, "正常采集不应有失败原因");

    // 链路状态：已连接
    let ok = wait_until(3000, || {
        master
            .snapshot_conn_states()
            .iter()
            .any(|(id, k, _)| *id == 5 && *k == ConnStateKind::Connected)
    })
    .await;
    assert!(ok, "主机连接应处于已连接状态: {:?}", master.snapshot_conn_states());

    // ---- 2. 写点：主机写 999.0 → 从机侧生效 → 主机下轮采到 ----
    master.write_point(902, 21, 999.0).await.unwrap();
    let slave_vals = slave.snapshot_values(901);
    assert!((value_of(&slave_vals, 11) - 999.0).abs() < 0.01, "从机未收到写入: {slave_vals:?}");

    // 输入区（输入寄存器/离散输入）协议只读：写应被拒绝并给出明确原因
    let err = master.write_point(902, 22, 1.0).await.unwrap_err();
    assert!(err.contains("只读"), "错误信息应说明只读: {err}");
    let err = master.write_point(902, 24, 1.0).await.unwrap_err();
    assert!(err.contains("只读"), "错误信息应说明只读: {err}");

    let ok = wait_until(3000, || {
        (value_of(&master.snapshot_values(902), 21) - 999.0).abs() < 0.01
    })
    .await;
    assert!(ok, "主机未采回新值 999");

    // ---- 3. 测试帧：FC03 读 holding 0..2 应答 = 999.0 的字 ----
    let resp = master.send_test_frame(5, 1, vec![0x03, 0x00, 0x00, 0x00, 0x02]).await.unwrap();
    let words = encode_words(999.0, DataType::F32);
    let mut expect = vec![0x03, 0x04];
    for w in &words {
        expect.extend_from_slice(&w.to_be_bytes());
    }
    assert_eq!(resp, expect, "测试帧应答不符");

    // ---- 4. 断线重连：停从机（连接被重置→主机走重连路径）→ 从机回来 → 采集恢复 ----
    slave.stop_all();
    // 断开后应推送「重连中」链路状态（含失败原因细节）
    let reconnecting = wait_until(3000, || {
        master
            .snapshot_conn_states()
            .iter()
            .any(|(id, k, _)| *id == 5 && *k == ConnStateKind::Reconnecting)
    })
    .await;
    assert!(reconnecting, "断开后应推送重连中状态: {:?}", master.snapshot_conn_states());
    tokio::time::sleep(Duration::from_millis(1500)).await;

    slave.start(&slave_project(PORT)).await.unwrap();
    let ok = wait_until(15000, || {
        let vals = master.snapshot_values(902);
        // 重启后的从机恢复初始值 123.5（或写入残留 999 的任一有效采集）
        (value_of(&vals, 21) - 123.5).abs() < 0.01
            || (value_of(&vals, 21) - 999.0).abs() < 0.01
            || (value_of(&vals, 22) - 456.25).abs() < 0.01
    })
    .await;
    assert!(ok, "断线重连后未恢复采集: {:?}", master.snapshot_values(902));

    master.stop_all();
    slave.stop_all();
}

/// 从机超时故障（TCP）：主机超时计数 + last_error + 监视器「(超时无应答)」事件
#[tokio::test(flavor = "multi_thread")]
async fn master_timeout_fault_marks_stat_and_monitor() {
    let slave = RuntimeManager::new();
    let mut sp = slave_project(PORT2);
    sp.connections[0].devices[0].fault = FaultConfig {
        kind: FaultKind::Timeout,
        delay_ms: None,
        exception_code: None,
    };
    slave.start(&sp).await.unwrap();

    let master = RuntimeManager::new();
    let mut frame_rx = master.subscribe_frames();
    master.start(&master_project(PORT2)).await.unwrap();

    let ok = wait_until(10000, || {
        master
            .snapshot_stats()
            .iter()
            .any(|(id, _, to, ..)| *id == 902 && *to > 0)
    })
    .await;
    assert!(ok, "主机超时未计数: {:?}", master.snapshot_stats());
    let st = master
        .snapshot_stats()
        .into_iter()
        .find(|(id, ..)| *id == 902)
        .unwrap();
    assert_eq!(st.4.as_deref(), Some("超时无应答"));

    let mut saw_timeout_note = false;
    while let Ok(ev) = frame_rx.try_recv() {
        if ev.note.as_deref().is_some_and(|n| n.contains("超时无应答")) {
            saw_timeout_note = true;
        }
    }
    assert!(saw_timeout_note, "监视器应出现「(超时无应答)」标注事件");

    master.stop_all();
    slave.stop_all();
}
