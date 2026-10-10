//! Modbus TCP 从机服务器（MBAP 帧）。

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::{find_device_id, resolve, ConnCtx, Reply};
use crate::protocol::{hex_str, FrameEvent};

/// MBAP 头长度：事务号(2) + 协议号(2) + 长度(2) + 单元号(1)
const MBAP_LEN: usize = 7;

/// 监听已就绪的 TCP 从机服务器（MBAP 帧）。
/// listener 由调用方（RuntimeManager::start_connection）绑定，绑定失败在启动阶段即报错。
pub async fn serve(ctx: Arc<ConnCtx>, listener: TcpListener, cancel: CancellationToken) {
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "?".to_string());

    loop {
        let (socket, _peer) = tokio::select! {
            _ = cancel.cancelled() => break,
            r = listener.accept() => match r {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[netlink] TCP {addr} accept 失败: {e}");
                    continue;
                }
            },
        };
        let ctx2 = ctx.clone();
        let cancel2 = cancel.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_socket(socket, ctx2, cancel2).await {
                eprintln!("[netlink] 连接处理异常退出: {e}");
            }
        });
    }
}

async fn handle_socket(
    mut socket: tokio::net::TcpStream,
    ctx: Arc<ConnCtx>,
    cancel: CancellationToken,
) -> std::io::Result<()> {
    loop {
        // ---- 读 MBAP 头 ----
        let mut header = [0u8; MBAP_LEN];
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            r = socket.read_exact(&mut header) => { r?; }
        }
        let txn = u16::from_be_bytes([header[0], header[1]]);
        let proto = u16::from_be_bytes([header[2], header[3]]);
        let len = u16::from_be_bytes([header[4], header[5]]) as usize;
        let unit = header[6];
        if proto != 0 || len < 2 || len > 260 {
            // 不符合 Modbus TCP 的字节流，直接断开
            return Ok(());
        }

        // ---- 读 PDU ----
        let mut pdu = vec![0u8; len - 1];
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            r = socket.read_exact(&mut pdu) => { r?; }
        }

        // 请求帧（记录完整 MBAP，与前端 mock 展示一致）
        let device_id = find_device_id(&ctx, unit);
        let mut frame = Vec::with_capacity(MBAP_LEN + pdu.len());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(&pdu);
        ctx.manager.emit_frame(FrameEvent::new(
            ctx.conn_id,
            device_id,
            "in",
            &frame,
            pdu_note(&pdu),
        ));

        // ---- 处理与应答 ----
        if let Some(reply) = resolve(&ctx, unit, &pdu) {
            let resp_pdu = match reply {
                Reply::Drop => {
                    ctx.manager.emit_frame(FrameEvent::new(
                        ctx.conn_id,
                        device_id,
                        "out",
                        &[],
                        Some("(超时不响应)".to_string()),
                    ));
                    continue;
                }
                Reply::Delay { ms, pdu } => {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    pdu
                }
                Reply::Now(pdu) => pdu,
                // 脏帧故障兜底：Modbus TCP 无校验和，按正常应答（UI 已按传输方式过滤）
                Reply::Corrupt(pdu) => pdu,
                // 断连故障：不应答，直接关闭该条连接（监听保持，主站可重连）
                Reply::Disconnect => {
                    ctx.manager.emit_frame(FrameEvent::new(
                        ctx.conn_id,
                        device_id,
                        "out",
                        &[],
                        Some("(故障注入: 断开连接)".to_string()),
                    ));
                    return Ok(());
                }
            };

            let mut out = Vec::with_capacity(MBAP_LEN + resp_pdu.len());
            out.extend_from_slice(&txn.to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes());
            out.extend_from_slice(&(resp_pdu.len() as u16 + 1).to_be_bytes());
            out.push(unit);
            out.extend_from_slice(&resp_pdu);

            ctx.manager.emit_frame(FrameEvent::new(
                ctx.conn_id,
                device_id,
                "out",
                &out,
                pdu_note_resp(&resp_pdu),
            ));
            socket.write_all(&out).await?;
        }
    }
}

/// 帧摘要（报文监视器 note 列）——请求方向
pub fn pdu_note(pdu: &[u8]) -> Option<String> {
    let fc = *pdu.first()?;
    let name = fc_name(fc)?;
    if pdu.len() >= 5 {
        let start = ((pdu[1] as u16) << 8) | pdu[2] as u16;
        let qty = ((pdu[3] as u16) << 8) | pdu[4] as u16;
        if matches!(fc, 0x05 | 0x06) {
            return Some(format!("{name} addr={start} val={qty}"));
        }
        return Some(format!("{name} addr={start} qty={qty}"));
    }
    Some(name.to_string())
}

/// 帧摘要——应答方向（数据区不是 addr/qty 布局，避免误读）
pub fn pdu_note_resp(pdu: &[u8]) -> Option<String> {
    let fc = *pdu.first()?;
    if fc & 0x80 != 0 && pdu.len() >= 2 {
        return Some(format!("异常应答 code=0x{:02X}", pdu[1]));
    }
    let name = fc_name(fc)?;
    if pdu.len() >= 2 {
        return Some(format!("{name} 应答 {}字节", pdu[1]));
    }
    Some(format!("{name} 应答"))
}

fn fc_name(fc: u8) -> Option<&'static str> {
    Some(match fc {
        0x01 => "FC01 读线圈",
        0x02 => "FC02 读离散输入",
        0x03 => "FC03 读保持寄存器",
        0x04 => "FC04 读输入寄存器",
        0x05 => "FC05 写单线圈",
        0x06 => "FC06 写单寄存器",
        0x0F => "FC0F 写多线圈",
        0x10 => "FC10 写多寄存器",
        _ => return None,
    })
}

/// 完整帧的十六进制（导出/日志用）
#[allow(dead_code)]
pub fn full_hex(bytes: &[u8]) -> String {
    hex_str(bytes)
}
