//! 点位工程量换算（系数/偏移量）测试：
//! 单元（换算函数 + 从机编码 + 主机导入）与主从端到端闭环（采集显示工程值、
//! 主机写工程值 → 寄存器存原始值 → 回读一致）。

use std::time::Duration;

use netlink_core::model::{
    Area, Connection, DataType, Device, FaultConfig, Point, ProjectSpec, Transport, ValueSource,
};
use netlink_core::runtime::{eng_to_raw, raw_to_eng, DeviceRuntime, RuntimeManager};

const PORT: u16 = 15060;

fn point(id: u64, name: &str, area: Area, addr: u16, dt: DataType, scale: f64, offset: f64, value: f64) -> Point {
    Point {
        id,
        name: name.into(),
        area,
        address: addr,
        data_type: dt,
        scale,
        offset,
        poll: true,
        writable: None,
        source: ValueSource::Static { value },
        note: None,
    }
}

fn device(id: u64, slave_id: u8, points: Vec<Point>) -> Device {
    Device {
        id,
        name: "装置".into(),
        slave_id,
        enabled: true,
        fault: FaultConfig::default(),
        poll_period_ms: 200,
        points,
    }
}

fn project(conn_id: u64, transport: Transport, devices: Vec<Device>) -> ProjectSpec {
    ProjectSpec {
        schema_version: 3,
        name: "scale".into(),
        connections: vec![Connection {
            id: conn_id,
            name: "conn".into(),
            transport,
            can_nodes: vec![],
            devices,
        }],
    }
}

fn value_of(vals: &[(u64, f64)], point_id: u64) -> f64 {
    vals.iter().find(|(id, _)| *id == point_id).map(|(_, v)| *v).unwrap_or(f64::NAN)
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

// ---------- 单元：换算函数 ----------

#[test]
fn transform_functions_roundtrip_and_guards() {
    // 正逆互逆
    let raw = 2200.0;
    let eng = raw_to_eng(raw, Area::InputRegister, 0.1, 0.0);
    assert!((eng - 220.0).abs() < 1e-9);
    assert!((eng_to_raw(eng, Area::InputRegister, 0.1, 0.0) - raw).abs() < 1e-6);
    // 偏移量参与
    assert!((raw_to_eng(100.0, Area::HoldingRegister, 0.5, -40.0) - 10.0).abs() < 1e-9);
    assert!((eng_to_raw(10.0, Area::HoldingRegister, 0.5, -40.0) - 100.0).abs() < 1e-9);
    // 恒等参数原样返回（位区域同）
    assert_eq!(raw_to_eng(123.0, Area::InputRegister, 1.0, 0.0), 123.0);
    assert_eq!(eng_to_raw(123.0, Area::InputRegister, 1.0, 0.0), 123.0);
    // 位区域不换算
    assert_eq!(raw_to_eng(1.0, Area::Coil, 0.1, 5.0), 1.0);
    assert_eq!(eng_to_raw(1.0, Area::DiscreteInput, 0.1, 5.0), 1.0);
    // 系数 0 防御兜底（按 1 处理，不除零）
    assert_eq!(eng_to_raw(220.0, Area::InputRegister, 0.0, 0.0), 220.0);
    assert_eq!(raw_to_eng(220.0, Area::InputRegister, 0.0, 0.0), 220.0);
}

// ---------- 单元：从机编码 / 主机导入 ----------

#[test]
fn slave_encode_scales_register_and_raws() {
    let dev = device(1, 1, vec![
        point(11, "电压×0.1", Area::HoldingRegister, 0, DataType::U16, 0.1, 0.0, 220.0),
        point(12, "温度", Area::InputRegister, 1, DataType::U16, 0.01, -40.0, 25.5),
        point(13, "无换算", Area::HoldingRegister, 2, DataType::U16, 1.0, 0.0, 7.0),
        point(14, "遥信", Area::Coil, 0, DataType::U16, 0.1, 0.0, 1.0),
    ]);
    let rt = DeviceRuntime::new(&dev);
    let blocks = rt.snapshot_blocks();
    // 220 ×0.1 → 寄存器 2200；(25.5+40) ÷0.01 → 6550（钳位范围内）
    assert_eq!(blocks.holding[0], 2200);
    assert_eq!(blocks.input[1], 6550);
    assert_eq!(blocks.holding[2], 7);
    // 位区域不换算
    assert!(blocks.coil[0]);
    // raws = 编码后解回值（含取整），工程值不动
    let raws = rt.snapshot_raws();
    assert_eq!(value_of(&raws, 11), 2200.0);
    assert_eq!(value_of(&raws, 12), 6550.0);
    let vals = rt.snapshot_values();
    assert!((value_of(&vals, 11) - 220.0).abs() < 1e-9);
}

#[test]
fn master_import_converts_to_engineering_value() {
    let dev = device(1, 1, vec![
        point(21, "电压", Area::HoldingRegister, 0, DataType::U16, 0.1, 0.0, 0.0),
        point(22, "温度", Area::InputRegister, 1, DataType::U16, 0.01, -40.0, 0.0),
    ]);
    let mut rt = DeviceRuntime::new(&dev);
    rt.import_registers(Area::HoldingRegister, 0, &[2200]);
    rt.import_registers(Area::InputRegister, 1, &[6550]);
    let vals = rt.snapshot_values();
    assert!((value_of(&vals, 21) - 220.0).abs() < 1e-9);
    assert!((value_of(&vals, 22) - 25.5).abs() < 1e-9);
    // raws 保持寄存器原文
    let raws = rt.snapshot_raws();
    assert_eq!(value_of(&raws, 21), 2200.0);
    assert_eq!(value_of(&raws, 22), 6550.0);
}

// ---------- 端到端：主从带系数闭环 ----------

/// 从机：holding 0 U16 scale 0.1 静态 220（寄存器存 2200）
fn slave_project(port: u16) -> ProjectSpec {
    project(
        4,
        Transport::TcpServer { listen_addr: "127.0.0.1".into(), port },
        vec![device(901, 1, vec![point(11, "电网电压", Area::HoldingRegister, 0, DataType::U16, 0.1, 0.0, 220.0)])],
    )
}

/// 主机：同地址同系数（现场典型：SCADA 点表与设备说明书同倍率）
fn master_project(port: u16) -> ProjectSpec {
    project(
        5,
        Transport::TcpClient { host: "127.0.0.1".into(), port, timeout_ms: 400 },
        vec![device(902, 1, vec![point(21, "电网电压", Area::HoldingRegister, 0, DataType::U16, 0.1, 0.0, 0.0)])],
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn master_slave_scale_closed_loop() {
    let slave = RuntimeManager::new();
    slave.start(&slave_project(PORT)).await.unwrap();
    let master = RuntimeManager::new();
    master.start(&master_project(PORT)).await.unwrap();

    // 1. 采集：主机显示工程值 220（寄存器里是 2200）
    let ok = wait_until(8000, || {
        (value_of(&master.snapshot_values(902), 21) - 220.0).abs() < 0.01
    })
    .await;
    assert!(ok, "主机未采集到换算后的工程值: {:?}", master.snapshot_values(902));
    let raws = master.snapshot_raws(902);
    assert!((value_of(&raws, 21) - 2200.0).abs() < 0.5, "raws 应为寄存器原文: {raws:?}");

    // 2. 主机写工程值 330 → 逆变换下发 3300 → 从机寄存器与静态工程值同步
    master.write_point(902, 21, 330.0).await.unwrap();
    let slave_vals = slave.snapshot_values(901);
    assert!(
        (value_of(&slave_vals, 11) - 330.0).abs() < 0.01,
        "从机静态工程值未更新: {slave_vals:?}"
    );
    let ok = wait_until(3000, || {
        (value_of(&master.snapshot_values(902), 21) - 330.0).abs() < 0.01
    })
    .await;
    assert!(ok, "主机未采回新工程值 330");

    // 3. 测试帧直读寄存器：FC03 holding 0 → 3300（0x0CE4），证明线上是原始值
    let resp = master.send_test_frame(5, 1, vec![0x03, 0x00, 0x00, 0x00, 0x01]).await.unwrap();
    assert_eq!(resp, vec![0x03, 0x02, 0x0C, 0xE4], "寄存器应存原始值 3300");

    master.stop_all();
    slave.stop_all();
}
