//! Modbus 实现：从机（TCP/RTU）+ 主机（TCP 轮询采集）。

pub mod master;
pub mod pdu;
pub mod rtu;
pub mod tcp;

use std::sync::{Arc, Mutex};

use crate::runtime::{DeviceRuntime, RuntimeManager, SimClock};

/// 一条连接的共享上下文（TCP/RTU 服务器与故障判定共用）
pub struct ConnCtx {
    pub conn_id: u64,
    pub devices: Vec<Arc<Mutex<DeviceRuntime>>>,
    pub clock: SimClock,
    pub manager: RuntimeManager,
}

/// 从机对一条请求的处置方式
#[derive(Debug)]
pub enum Reply {
    /// 不应答（故障注入：超时）
    Drop,
    /// 延迟 ms 后再发送 PDU（故障注入：延迟）
    Delay { ms: u64, pdu: Vec<u8> },
    /// 立即应答
    Now(Vec<u8>),
    /// 立即应答且内容正常，但传输层应破坏校验（故障注入：脏帧，CRC 仅存在于 RTU）
    Corrupt(Vec<u8>),
    /// 断开连接（故障注入：断连，仅 TCP 有连接概念）
    Disconnect,
}

/// 按 unit id 找设备 id（帧事件标注用）
pub fn find_device_id(ctx: &ConnCtx, unit: u8) -> Option<u64> {
    ctx.devices
        .iter()
        .find(|d| {
            let g = d.lock().unwrap();
            g.enabled && g.slave_id == unit
        })
        .map(|d| d.lock().unwrap().id)
}

/// 按 unit id 找设备并应用故障注入，产出应答 PDU。
/// None 表示该 unit 无应答内容（无设备时回 0x0B 网关异常）。
pub fn resolve(ctx: &ConnCtx, unit: u8, pdu: &[u8]) -> Option<Reply> {
    if pdu.is_empty() {
        return None;
    }
    let fc = pdu[0];

    let dev = ctx.devices.iter().find(|d| {
        let g = d.lock().unwrap();
        g.enabled && g.slave_id == unit
    });

    let dev = match dev {
        Some(d) => d,
        None => {
            // 未知从机地址：按网关惯例回 0x0B（Gateway Target Device Failed to Respond）
            return Some(Reply::Now(vec![fc | 0x80, 0x0B]));
        }
    };

    let fault = {
        let g = dev.lock().unwrap();
        (g.fault.kind, g.fault.delay_ms, g.fault.exception_code)
    };

    use crate::model::FaultKind;
    match fault.0 {
        FaultKind::Timeout => Some(Reply::Drop),
        FaultKind::Delay => {
            let ms = fault.1.unwrap_or(500);
            let mut g = dev.lock().unwrap();
            let resp = pdu::process_request(pdu, &mut g);
            resp.map(|pdu| Reply::Delay { ms, pdu })
        }
        FaultKind::Exception => {
            let code = fault.2.unwrap_or(0x01);
            Some(Reply::Now(vec![fc | 0x80, code]))
        }
        FaultKind::Corrupt => {
            let mut g = dev.lock().unwrap();
            pdu::process_request(pdu, &mut g).map(Reply::Corrupt)
        }
        FaultKind::Disconnect => Some(Reply::Disconnect),
        FaultKind::Normal => {
            let mut g = dev.lock().unwrap();
            pdu::process_request(pdu, &mut g).map(Reply::Now)
        }
    }
}
