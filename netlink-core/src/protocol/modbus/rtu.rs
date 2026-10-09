//! Modbus RTU 从机（RS-485/RS-232 串口）。
//!
//! 帧格式：`[地址(1)] [PDU(n)] [CRC16低(1)] [CRC16高(1)]`。
//! 串口 IO 为阻塞模式，跑在 spawn_blocking 线程；以读超时近似帧间隔（T3.5）。

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::tcp::{pdu_note, pdu_note_resp};
use super::{find_device_id, resolve, ConnCtx, Reply};
use crate::model::Parity;
use crate::protocol::FrameEvent;

/// RTU 单帧上限（256B）
const RTU_MAX_FRAME: usize = 256;

/// 打开串口（8 数据位 + 配置的校验/停止位，50ms 读超时近似帧间隔）。
/// 由 RuntimeManager::start_connection 在启动阶段调用，失败（被占用/不存在）即报错。
pub fn open_port(
    serial_port: &str,
    baud: u32,
    parity: Parity,
    stop_bits: u8,
) -> Result<Box<dyn serialport::SerialPort>, String> {
    use serialport::{DataBits, Parity as SerialParity, StopBits as SerialStopBits};

    let par = match parity {
        Parity::None => SerialParity::None,
        Parity::Even => SerialParity::Even,
        Parity::Odd => SerialParity::Odd,
    };
    let sb = match stop_bits {
        2 => SerialStopBits::Two,
        _ => SerialStopBits::One,
    };

    serialport::new(serial_port, baud)
        .data_bits(DataBits::Eight)
        .parity(par)
        .stop_bits(sb)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|e| format!("串口「{serial_port}」打开失败: {e}"))
}

/// 串口已打开的 RTU 从机服务器；串口句柄由调用方传入。
pub async fn serve(
    ctx: Arc<ConnCtx>,
    serial_port: String,
    port: Box<dyn serialport::SerialPort>,
    cancel: CancellationToken,
) {
    let stop_flag = Arc::new(AtomicBool::new(false));

    // 取消令牌 → 置停止标志（唤醒阻塞读）
    let sf = stop_flag.clone();
    let watcher = tokio::spawn(async move {
        cancel.cancelled().await;
        sf.store(true, Ordering::Release);
    });

    let handle = tokio::task::spawn_blocking(move || {
        if let Err(e) = run_serial(&ctx, port, &stop_flag) {
            eprintln!("[netlink] 串口「{serial_port}」退出: {e}");
        }
    });

    let _ = handle.await;
    watcher.abort();
}

fn run_serial(
    ctx: &Arc<ConnCtx>,
    mut port: Box<dyn serialport::SerialPort>,
    stop_flag: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf: Vec<u8> = Vec::with_capacity(RTU_MAX_FRAME);

    loop {
        if stop_flag.load(Ordering::Acquire) {
            return Ok(());
        }

        let mut b = [0u8; 1];
        match port.read(&mut b) {
            Ok(_) => {
                buf.push(b[0]);
                if buf.len() >= RTU_MAX_FRAME {
                    // 无意义长流，丢弃重来
                    buf.clear();
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                // 帧间隔：缓冲非空则处理一帧
                if !buf.is_empty() {
                    if let Some(resp) = handle_rtu_frame(ctx, &buf) {
                        let _ = port.write_all(&resp);
                        let _ = port.flush();
                    }
                    buf.clear();
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Box::new(e)),
        }
    }
}

/// 处理一帧完整 RTU 报文；返回需要写回串口的应答字节（None = 不应答）
fn handle_rtu_frame(ctx: &Arc<ConnCtx>, frame: &[u8]) -> Option<Vec<u8>> {
    // 至少：地址 + 功能码 + CRC(2)
    if frame.len() < 4 {
        return None;
    }
    let n = frame.len();
    let addr = frame[0];
    let pdu = &frame[1..n - 2];
    let crc = u16::from_le_bytes([frame[n - 2], frame[n - 1]]);
    if crc != crc16(&frame[..n - 2]) {
        ctx.manager.emit_frame(FrameEvent::new(
            ctx.conn_id,
            find_device_id(ctx, addr),
            "in",
            frame,
            Some("(CRC 校验失败，丢弃)".to_string()),
        ));
        return None;
    }

    ctx.manager.emit_frame(FrameEvent::new(
        ctx.conn_id,
        find_device_id(ctx, addr),
        "in",
        frame,
        pdu_note(pdu),
    ));

    let resp_pdu = match resolve(ctx, addr, pdu)? {
        Reply::Drop => {
            ctx.manager.emit_frame(FrameEvent::new(
                ctx.conn_id,
                find_device_id(ctx, addr),
                "out",
                &[],
                Some("(超时不响应)".to_string()),
            ));
            return None;
        }
        Reply::Delay { ms, pdu } => {
            std::thread::sleep(Duration::from_millis(ms));
            pdu
        }
        Reply::Now(pdu) => pdu,
    };

    let mut out = Vec::with_capacity(3 + resp_pdu.len());
    out.push(addr);
    out.extend_from_slice(&resp_pdu);
    let crc = crc16(&out).to_le_bytes();
    out.extend_from_slice(&crc);

    ctx.manager.emit_frame(FrameEvent::new(
        ctx.conn_id,
        find_device_id(ctx, addr),
        "out",
        &out,
        pdu_note_resp(&resp_pdu),
    ));

    Some(out)
}

/// Modbus CRC16（多项式 0xA001，初值 0xFFFF）；主机侧组帧/测试共用
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}
