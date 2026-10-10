//! CAN 转以太网后端：ZLG CANET-E(2E)/CANET-2E 网关（TCP 工作端口，透传模式）。
//!
//! 帧封装为**13 字节定长**（官方用户手册附录「工作端口数据转换格式」，
//! CANET-E+/2E+ ©2013 与 CANET-E(2E)-U V1.08 ©2019 两代一致）：
//!
//! ```text
//! [0]      帧信息：bit7 FF(1=扩展) | bit6 RTR(1=远程) | bit5:4 保留(0) | bit3:0 DLC
//! [1..5]   帧ID：4 字节大端；标准帧右对齐低 11 位（样例 0x3FF → 00 00 03 FF）
//! [5..13]  数据域：恒 8 字节，DLC 之后补 00（样例 6 字节数据 → … 66 00 00）
//! ```
//!
//! 手册样例帧（单测逐字节比对）：
//! - 扩展数据帧 ID=0x12345678 数据 8 字节：`88 12 34 56 78 11 22 33 44 55 66 77 88`
//! - 标准数据帧 ID=0x3FF 数据 6 字节：`06 00 00 03 FF 11 22 33 44 55 66 00 00`
//!
//! 工作端口与 CAN 通道一一对应（CAN0 默认 4001、CAN1 默认 4002，设备可改）；
//! TCP 流即 13 字节帧的连续序列（一个 TCP 段可含多帧）。状态通知端口
//! （6001/6002，总线错误告警）为独立端口，v1 不接。
//!
//! 连接循环平移 Modbus TCP 主机模式：连接失败/断开 → 退避 1s→5s→30s +
//! Reconnecting 徽章（带原因），成功 → 进入 [`super::can_loop`]（fatal_io：
//! IO 错误上抛触发重连）。

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{can_loop, CanBusStatus, CanCtx, CanFrame, CanIo, CanMsg};
use crate::runtime::ConnStateKind;

/// 一帧 CAN 在 TCP 流上的定长字节数
pub const CANET_FRAME_LEN: usize = 13;

// ---------- 编解码（纯函数） ----------

/// CanFrame → 13 字节（帧信息 + ID 大端 + 数据补零到 8）
pub fn encode_frame(f: &CanFrame) -> [u8; CANET_FRAME_LEN] {
    let mut out = [0u8; CANET_FRAME_LEN];
    let info = ((f.extended as u8) << 7) | ((f.remote as u8) << 6) | ((f.data.len() as u8) & 0x0F);
    out[0] = info;
    out[1..5].copy_from_slice(&f.id.to_be_bytes());
    let n = f.data.len().min(8);
    out[5..5 + n].copy_from_slice(&f.data[..n]);
    out
}

/// 13 字节 → CanFrame；帧信息非法（DLC>8 / 保留位非 0）或 ID 超域返回 None（按流同步丢弃）
pub fn decode_frame(b: &[u8; CANET_FRAME_LEN]) -> Option<CanFrame> {
    let info = b[0];
    let dlc = (info & 0x0F) as usize;
    if dlc > 8 {
        return None;
    }
    if info & 0x30 != 0 {
        return None; // 手册：保留值为 0，不可写入 1
    }
    let extended = info & 0x80 != 0;
    let remote = info & 0x40 != 0;
    let id = u32::from_be_bytes([b[1], b[2], b[3], b[4]]);
    let max = if extended { 0x1FFF_FFFF } else { 0x7FF };
    if id > max {
        return None;
    }
    // 远程帧数据域忽略（模型里远程帧无数据）；数据帧按 DLC 截取
    let data = if remote { Vec::new() } else { b[5..5 + dlc].to_vec() };
    Some(CanFrame { id, extended, remote, data })
}

/// 从流缓冲中拆出全部完整帧（剩余半帧留在 buf 里）
/// 返回 (帧列表, 丢弃的非法块数——仅用于观测，正常永不发生)
pub fn decode_stream(buf: &mut Vec<u8>) -> (Vec<CanFrame>, usize) {
    let mut out = Vec::new();
    let mut bad = 0;
    while buf.len() >= CANET_FRAME_LEN {
        let chunk: [u8; CANET_FRAME_LEN] = buf.drain(..CANET_FRAME_LEN).collect::<Vec<u8>>().try_into().unwrap();
        match decode_frame(&chunk) {
            Some(f) => out.push(f),
            None => bad += 1,
        }
    }
    (out, bad)
}

// ---------- TCP IO 适配 ----------

/// CANET 工作端口的 TCP 适配（行缓冲到 13 字节边界；多解出的帧排队）
pub struct CanetTcpIo {
    stream: TcpStream,
    buf: Vec<u8>,
    pending: VecDeque<CanFrame>,
}

impl CanetTcpIo {
    pub fn new(stream: TcpStream) -> Self {
        Self { stream, buf: Vec::new(), pending: VecDeque::new() }
    }
}

impl CanIo for CanetTcpIo {
    fn send(&mut self, f: &CanFrame) -> Result<(), String> {
        let frame = encode_frame(f);
        self.stream
            .write_all(&frame)
            .map_err(|e| format!("TCP 写入失败: {e}"))
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<CanFrame>, String> {
        // 先吐上次一个 TCP 段里多解出来的帧
        if let Some(f) = self.pending.pop_front() {
            return Ok(Some(f));
        }
        let deadline = Instant::now() + timeout.max(Duration::from_millis(1));
        loop {
            if let Some(f) = self.pending.pop_front() {
                return Ok(Some(f));
            }
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                return Ok(None);
            }
            self.stream
                .set_read_timeout(Some(remain.max(Duration::from_millis(1))))
                .map_err(|e| format!("设置读超时失败: {e}"))?;
            let mut chunk = [0u8; 512];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("网关断开连接".to_string()),
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    let (frames, bad) = decode_stream(&mut self.buf);
                    if bad > 0 {
                        eprintln!("[netlink] CANET 收到 {bad} 个非法帧块（按帧边界跳过）");
                    }
                    self.pending.extend(frames);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                }
                Err(e) => return Err(format!("TCP 读取失败: {e}")),
            }
        }
    }

    fn status(&mut self) -> CanBusStatus {
        CanBusStatus::default()
    }
}

// ---------- 连接循环 ----------

/// 解析地址并带超时连接（阻塞线程内执行；多地址逐个尝试）
fn tcp_connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("解析地址 {host}:{port} 失败: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("{host}:{port} 无可解析地址"));
    }
    let mut last = String::new();
    for a in addrs {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = format!("{a}: {e}"),
        }
    }
    Err(format!("连接 {host}:{port} 失败: {last}"))
}

/// CAN 转以太网服务：连接循环（退避重连 1s→5s→30s 封顶）+ 透传主循环
pub async fn serve_can_net(
    ctx: Arc<CanCtx>,
    host: String,
    port: u16,
    timeout_ms: u32,
    cancel: CancellationToken,
    mut rx: mpsc::Receiver<CanMsg>,
) {
    // 连接超时限幅（防呆，与 check::test_tcp_client 一致）
    let timeout = Duration::from_millis(timeout_ms.clamp(200, 5000) as u64);
    let stop_flag = Arc::new(AtomicBool::new(false));

    // 取消令牌 → 置停止标志（唤醒阻塞读/退避睡眠）
    let sf = stop_flag.clone();
    let watcher = tokio::spawn(async move {
        cancel.cancelled().await;
        sf.store(true, Ordering::Release);
    });

    let conn_id = ctx.conn_id;
    let handle = tokio::task::spawn_blocking(move || {
        let mut backoff = Duration::from_secs(1);
        loop {
            if stop_flag.load(Ordering::Acquire) {
                return;
            }
            ctx.manager.set_conn_state(conn_id, ConnStateKind::Connecting, None);
            match tcp_connect(&host, port, timeout) {
                Err(e) => {
                    let detail = format!("{e}，{}ms 后重试", backoff.as_millis());
                    ctx.manager.set_conn_state(conn_id, ConnStateKind::Reconnecting, Some(detail));
                    eprintln!("[netlink] CANET {host}:{port} 连接失败（{e}），{}ms 后重试", backoff.as_millis());
                    sleep_sliced(&stop_flag, backoff);
                    backoff = (backoff * 5).min(Duration::from_secs(30));
                }
                Ok(stream) => {
                    backoff = Duration::from_secs(1);
                    ctx.manager.set_conn_state(conn_id, ConnStateKind::Connected, None);
                    let mut io = CanetTcpIo::new(stream);
                    // IO 错误上抛（fatal）：断线交给本层重连（同 Modbus TCP 主机语义）
                    match can_loop(&ctx, &mut io, &stop_flag, &mut rx, true) {
                        Ok(()) => return, // 取消
                        Err(e) => {
                            ctx.manager.set_conn_state(
                                conn_id,
                                ConnStateKind::Reconnecting,
                                Some(format!("{e}，重连中")),
                            );
                            eprintln!("[netlink] CANET {host}:{port} 断开（{e}），重连…");
                            sleep_sliced(&stop_flag, Duration::from_millis(200));
                        }
                    }
                }
            }
        }
    });

    let _ = handle.await;
    watcher.abort();
}

/// 分片睡眠（可被停止标志打断；退避最长 30s，不能卡住连接停止）
fn sleep_sliced(stop_flag: &AtomicBool, total: Duration) {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if stop_flag.load(Ordering::Acquire) {
            return;
        }
        let remain = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remain.min(Duration::from_millis(100)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: u32, extended: bool, remote: bool, data: Vec<u8>) -> CanFrame {
        CanFrame { id, extended, remote, data }
    }

    /// 手册附录样例帧 1：扩展数据帧 ID=0x12345678、8 字节数据
    #[test]
    fn manual_sample_extended() {
        let f = frame(0x12345678, true, false, vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        let bytes = [0x88u8, 0x12, 0x34, 0x56, 0x78, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        assert_eq!(encode_frame(&f), bytes);
        assert_eq!(decode_frame(&bytes), Some(f));
    }

    /// 手册附录样例帧 2：标准数据帧 ID=0x3FF、6 字节数据（数据域补零到 8）
    #[test]
    fn manual_sample_standard_padded() {
        let f = frame(0x3FF, false, false, vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let bytes = [0x06u8, 0x00, 0x00, 0x03, 0xFF, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x00, 0x00];
        assert_eq!(encode_frame(&f), bytes);
        // 解码侧按 DLC 截回 6 字节（补零不还原）
        assert_eq!(decode_frame(&bytes), Some(f));
    }

    /// 往返：标准/扩展/远程帧；远程帧数据域忽略
    #[test]
    fn roundtrip_all_kinds() {
        let cases = vec![
            frame(0x123, false, false, vec![0xAA; 8]),
            frame(0x000, false, false, vec![]),
            frame(0x18FF0102, true, false, vec![0x01, 0x02]),
            frame(0x1FFFFFFF, true, false, vec![0xFF]),
            frame(0x305, false, true, vec![]),
            frame(0x18FF0102, true, true, vec![]),
        ];
        for f in cases {
            let id = f.id;
            assert_eq!(decode_frame(&encode_frame(&f)), Some(f), "id=0x{id:X}");
        }
    }

    /// 非法帧：DLC>8、保留位非 0、标准 ID 超域、扩展 ID 超域
    #[test]
    fn invalid_frames_rejected() {
        let mut b = [0u8; CANET_FRAME_LEN];
        b[0] = 0x89; // DLC=9
        assert_eq!(decode_frame(&b), None);
        b[0] = 0x21; // 保留位 bit5=1
        assert_eq!(decode_frame(&b), None);
        b[0] = 0x02; // 标准帧
        b[1..5].copy_from_slice(&0x800u32.to_be_bytes()); // ID > 0x7FF
        assert_eq!(decode_frame(&b), None);
        b[0] = 0x82; // 扩展帧
        b[1..5].copy_from_slice(&0x2000_0000u32.to_be_bytes()); // ID > 0x1FFFFFFF
        assert_eq!(decode_frame(&b), None);
    }

    /// 流式拆帧：TCP 段任意切割（半帧跨段、一帧多段、一段多帧）
    #[test]
    fn stream_decode_splits_anywhere() {
        let f1 = frame(0x123, false, false, vec![1, 2, 3]);
        let f2 = frame(0x18FF0102, true, false, vec![9, 8, 7, 6, 5]);
        let mut wire = Vec::new();
        wire.extend_from_slice(&encode_frame(&f1));
        wire.extend_from_slice(&encode_frame(&f2));

        // 逐 5 字节喂入（13+13=26 字节，切成 5/5/5/5/5/1）
        let mut buf = Vec::new();
        let mut got: Vec<CanFrame> = Vec::new();
        for chunk in wire.chunks(5) {
            buf.extend_from_slice(chunk);
            let (frames, bad) = decode_stream(&mut buf);
            assert_eq!(bad, 0);
            got.extend(frames);
        }
        assert_eq!(got, vec![f1, f2]);
        assert!(buf.is_empty());

        // 一次性喂入
        let mut buf2 = wire.clone();
        let (frames, bad) = decode_stream(&mut buf2);
        assert_eq!((bad, frames.len()), (0, 2));
    }

    /// 0/13/26 字节对齐切分也不丢帧
    #[test]
    fn stream_decode_aligned() {
        let f = frame(0x42, false, false, vec![0x00]);
        let wire = [encode_frame(&f), encode_frame(&f)].concat();
        for size in [13usize, 26] {
            let mut buf = Vec::new();
            let mut got = 0;
            for chunk in wire.chunks(size) {
                buf.extend_from_slice(chunk);
                let (frames, bad) = decode_stream(&mut buf);
                assert_eq!(bad, 0);
                got += frames.len();
            }
            assert_eq!(got, 2, "chunk size {size}");
        }
    }
}
