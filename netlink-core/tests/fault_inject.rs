//! M2 故障注入增强测试（RTU 侧）：
//! 直接驱动纯函数 `handle_rtu_frame`（IO 由 run_serial 承担），覆盖
//! 脏帧（应答 CRC 故意破坏）与断连故障在串口上的兜底（无连接概念 → 不应答）。

use std::sync::{Arc, Mutex};

use netlink_core::model::{Area, DataType, Device, FaultConfig, FaultKind, Point, ValueSource};
use netlink_core::protocol::modbus::rtu::{crc16, handle_rtu_frame};
use netlink_core::protocol::modbus::ConnCtx;
use netlink_core::runtime::{DeviceRuntime, RuntimeManager, SimClock};

/// 从机设备：holding 0 = 250.0(f32)
fn device_with(fault: FaultConfig) -> Device {
    Device {
        id: 1,
        name: "从机".into(),
        slave_id: 7,
        enabled: true,
        fault,
        poll_period_ms: 1000,
        points: vec![Point {
            id: 11,
            name: "设定".into(),
            area: Area::HoldingRegister,
            address: 0,
            data_type: DataType::F32,
            source: ValueSource::Static { value: 250.0 },
            note: None,
        }],
    }
}

fn rtu_ctx(dev: Device) -> Arc<ConnCtx> {
    Arc::new(ConnCtx {
        conn_id: 1,
        devices: vec![Arc::new(Mutex::new(DeviceRuntime::new(&dev)))],
        clock: SimClock::new(),
        manager: RuntimeManager::new(),
    })
}

/// 组一条 CRC 正确的 RTU 请求帧 `[addr][pdu][crc16 le]`
fn request_frame(addr: u8, pdu: &[u8]) -> Vec<u8> {
    let mut f = vec![addr];
    f.extend_from_slice(pdu);
    let c = crc16(&f).to_le_bytes();
    f.extend_from_slice(&c);
    f
}

/// FC03 读 holding 0..2 的正常应答 PDU：250.0(f32) → 0x437A0000
const EXPECT_PDU: &[u8] = &[0x03, 0x04, 0x43, 0x7A, 0x00, 0x00];

/// 脏帧故障：应答内容正常，但 CRC 必校验失败（恰翻转一位）
#[test]
fn corrupt_fault_breaks_response_crc() {
    let ctx = rtu_ctx(device_with(FaultConfig {
        kind: FaultKind::Corrupt,
        delay_ms: None,
        exception_code: None,
    }));
    let resp = handle_rtu_frame(&ctx, &request_frame(7, &[0x03, 0x00, 0x00, 0x00, 0x02]))
        .expect("脏帧故障仍应产生应答帧");

    let n = resp.len();
    // PDU 部分与正常应答完全一致（破坏的只是 CRC）
    assert_eq!(&resp[1..n - 2], EXPECT_PDU);
    let crc = u16::from_le_bytes([resp[n - 2], resp[n - 1]]);
    let good = crc16(&resp[..n - 2]);
    assert_ne!(crc, good, "脏帧应答的 CRC 不应校验通过");
    assert_eq!(crc ^ good, 1, "破坏方式应为翻转一位（确定性）");
}

/// 对照组：正常设备的应答 CRC 校验通过（保证脏帧路径没有污染正常路径）
#[test]
fn normal_fault_keeps_response_crc_valid() {
    let ctx = rtu_ctx(device_with(FaultConfig::default()));
    let resp = handle_rtu_frame(&ctx, &request_frame(7, &[0x03, 0x00, 0x00, 0x00, 0x02]))
        .expect("正常设备应应答");
    let n = resp.len();
    assert_eq!(&resp[1..n - 2], EXPECT_PDU);
    let crc = u16::from_le_bytes([resp[n - 2], resp[n - 1]]);
    assert_eq!(crc, crc16(&resp[..n - 2]));
}

/// 断连故障在 RTU 上兜底：串口无连接概念，按不应答处理
#[test]
fn disconnect_fault_on_rtu_is_silence() {
    let ctx = rtu_ctx(device_with(FaultConfig {
        kind: FaultKind::Disconnect,
        delay_ms: None,
        exception_code: None,
    }));
    let resp = handle_rtu_frame(&ctx, &request_frame(7, &[0x03, 0x00, 0x00, 0x00, 0x02]));
    assert!(resp.is_none(), "断连故障在串口上应兜底为不应答");
}
