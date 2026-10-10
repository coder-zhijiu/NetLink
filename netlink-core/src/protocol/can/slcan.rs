//! slcan 后端：串口 ASCII 协议（LAWICEL CANUSB 命令集；CANable/candleLight、
//! c.light 等兼容固件可直接用）。**复用现有 serialport 依赖，不碰 FFI/驱动**。
//!
//! 帧封装（行式，`\r` 结束）：标准数据 `t<id3><dlc><data>` / 扩展数据 `T<id8><dlc><data>`
//! / 标准远程 `r<id3><dlc>` / 扩展远程 `R<id8><dlc>`；其余行（应答、状态、错误帧）忽略。
//! 上电序列：`C`（复位）→ `S<n>`（波特率档位）→ `O`（进入 CAN 模式）；停止时 Drop 发 `C`。
//! 编解码为纯函数（无 IO）便于单测，往返样例见本文件测试。

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use super::{CanBusStatus, CanFrame, CanIo};

/// CAN 波特率 → S 命令档位（LAWICEL 表：S0=10k…S8=1M）
pub fn bitrate_code(bitrate: u32) -> Result<u8, String> {
    Ok(match bitrate {
        10_000 => 0,
        20_000 => 1,
        50_000 => 2,
        100_000 => 3,
        125_000 => 4,
        250_000 => 5,
        500_000 => 6,
        800_000 => 7,
        1_000_000 => 8,
        _ => {
            return Err(format!(
                "slcan 不支持波特率 {bitrate}（可选 125k/250k/500k/800k/1M 等档位）"
            ))
        }
    })
}

/// CanFrame → slcan 行（不含行结束符）
pub fn encode_frame(f: &CanFrame) -> String {
    let id = if f.extended { format!("{:08X}", f.id) } else { format!("{:03X}", f.id) };
    let mut s = match (f.extended, f.remote) {
        (false, false) => format!("t{id}{:X}", f.data.len()),
        (true, false) => format!("T{id}{:X}", f.data.len()),
        // 远程帧：DLC 位按 0 发（模型里远程帧无数据域）
        (false, true) => format!("r{id}0"),
        (true, true) => format!("R{id}0"),
    };
    for b in &f.data {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// slcan 行 → CanFrame；非数据/远程帧行（ACK、状态、错误帧、畸形行）返回 None
pub fn parse_line(line: &str) -> Option<CanFrame> {
    let line = line.trim_matches(|c| c == '\r' || c == '\n').trim();
    let (extended, remote) = match line.as_bytes().first()? {
        b't' => (false, false),
        b'T' => (true, false),
        b'r' => (false, true),
        b'R' => (true, true),
        _ => return None,
    };
    let id_w = if extended { 8 } else { 3 };
    let body = &line[1..];
    if body.len() < id_w + 1 || !body[..id_w].chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let id = u32::from_str_radix(&body[..id_w], 16).ok()?;
    let max_id = if extended { 0x1FFF_FFFF } else { 0x7FF };
    if id > max_id {
        return None;
    }
    let dlc = u8::from_str_radix(&body[id_w..id_w + 1], 16).ok()?;
    if dlc > 8 {
        return None;
    }
    let rest = &body[id_w + 1..];
    if remote {
        // 远程帧不应带数据域（容错：多出的字符视为下一行粘连，整行丢弃）
        if !rest.is_empty() {
            return None;
        }
        return Some(CanFrame { id, extended, remote: true, data: Vec::new() });
    }
    if rest.len() != dlc as usize * 2 || !rest.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut data = Vec::with_capacity(dlc as usize);
    for k in 0..dlc as usize {
        data.push(u8::from_str_radix(&rest[k * 2..k * 2 + 2], 16).ok()?);
    }
    Some(CanFrame { id, extended, remote: false, data })
}

// ---------- 串口 IO 适配 ----------

/// slcan 串口 IO（行缓冲累积到 `\r`/`\n`；读超时由 recv 的 deadline 控制）
pub struct SlcanIo {
    port: Box<dyn serialport::SerialPort>,
    buf: Vec<u8>,
}

/// 打开串口并完成上电序列（C 复位 → S<n> 波特率 → O 进入 CAN 模式）
pub fn open(serial_port: &str, serial_baud: u32, bitrate: u32) -> Result<SlcanIo, String> {
    let code = bitrate_code(bitrate)?;
    let port = serialport::new(serial_port, serial_baud)
        .timeout(Duration::from_millis(10))
        .open()
        .map_err(|e| format!("打开串口 {serial_port} 失败: {e}"))?;
    let mut io = SlcanIo { port, buf: Vec::new() };
    // 容错原则：BEL（0x07）是明确拒绝；无应答按成功放行（部分固件对部分命令不回 ACK）
    io.cmd("C")?;
    io.cmd(&format!("S{code}"))?;
    io.cmd("O")?;
    Ok(io)
}

impl SlcanIo {
    /// 发一条命令并等 ACK（`\r` 成功 / 0x07 拒绝；超时按成功放行）
    fn cmd(&mut self, cmd: &str) -> Result<(), String> {
        let mut line = String::with_capacity(cmd.len() + 1);
        line.push_str(cmd);
        line.push('\r');
        self.port
            .write_all(line.as_bytes())
            .map_err(|e| format!("串口写入失败（{cmd}）: {e}"))?;
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut one = [0u8; 1];
        while Instant::now() < deadline {
            match self.port.read(&mut one) {
                Ok(0) => {}
                Ok(_) => {
                    if one[0] == 0x07 {
                        self.buf.clear(); // 丢弃残留，后续命令从干净状态开始
                        return Err(format!("适配器拒绝命令 {cmd}（0x07）"));
                    }
                    if one[0] == b'\r' {
                        return Ok(());
                    }
                    // 其他字节（如固件 banner）：忽略
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("串口读取失败（{cmd}）: {e}")),
            }
        }
        Ok(()) // 超时无应答：容错放行
    }

    /// 读一条完整行（含 `\r`/`\n`）；deadline 前无完整行返回 None
    fn read_line(&mut self, deadline: Instant) -> Result<Option<String>, String> {
        loop {
            // 缓冲里已有完整行：直接切出
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\r' || b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=pos).collect();
                return Ok(Some(String::from_utf8_lossy(&line[..pos]).into_owned()));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            let mut chunk = [0u8; 64];
            match self.port.read(&mut chunk) {
                Ok(0) => {}
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("串口读取失败: {e}")),
            }
        }
    }
}

impl CanIo for SlcanIo {
    fn send(&mut self, f: &CanFrame) -> Result<(), String> {
        let mut line = encode_frame(f);
        line.push('\r');
        self.port
            .write_all(line.as_bytes())
            .map_err(|e| format!("串口写入失败: {e}"))
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<CanFrame>, String> {
        let deadline = Instant::now() + timeout;
        // 先消化缓冲（上个 deadline 残留的半行也算数），再等新字节
        loop {
            match self.read_line(deadline)? {
                Some(line) => {
                    if let Some(f) = parse_line(&line) {
                        return Ok(Some(f));
                    }
                    // ACK / 状态行 / 畸形行：跳过继续等
                }
                None => return Ok(None),
            }
        }
    }

    fn status(&mut self) -> CanBusStatus {
        CanBusStatus::default()
    }
}

impl Drop for SlcanIo {
    fn drop(&mut self) {
        // 退出 CAN 模式（尽力而为：连接停止时复位适配器状态）
        let _ = self.port.write_all(b"C\r");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: u32, extended: bool, remote: bool, data: Vec<u8>) -> CanFrame {
        CanFrame { id, extended, remote, data }
    }

    /// 四种帧型的编解码往返
    #[test]
    fn codec_roundtrip() {
        let cases = vec![
            (frame(0x123, false, false, vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]), "t12381122334455667788"),
            (frame(0x201, false, false, vec![0x01, 0x02]), "t20120102"),
            (frame(0x000, false, false, vec![]), "t0000"),
            (frame(0x18FF0102, true, false, vec![0xAA, 0x55]), "T18FF01022AA55"),
            (frame(0x1FFFFFFF, true, false, vec![0xFF]), "T1FFFFFFF1FF"),
            (frame(0x305, false, true, vec![]), "r3050"),
            (frame(0x18FF0102, true, true, vec![]), "R18FF01020"),
        ];
        for (f, line) in cases {
            assert_eq!(encode_frame(&f), line, "encode {line}");
            assert_eq!(parse_line(line), Some(f.clone()), "parse {line}");
            // 行结束符容错
            assert_eq!(parse_line(&format!("{line}\r")), Some(f));
        }
    }

    /// 畸形行容错：不 panic、返回 None（半行/坏 hex/DLC 不符/ID 超域/粘连）
    #[test]
    fn codec_malformed() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("\r"), None);
        assert_eq!(parse_line("t12"), None); // 不满 id+dlc
        assert_eq!(parse_line("t123"), None); // 缺 DLC
        assert_eq!(parse_line("t12G81"), None); // 坏 hex
        assert_eq!(parse_line("t1233AABB"), None); // DLC 与数据长度不符（3 字节声明只给 2）
        assert_eq!(parse_line("t8120"), None); // 标准 ID 超 0x7FF
        assert_eq!(parse_line("T200000000"), None); // 扩展 ID 超 0x1FFFFFFF
        assert_eq!(parse_line("r1230FF"), None); // 远程帧带数据
        assert_eq!(parse_line("z12345"), None); // 非帧命令
        assert_eq!(parse_line("1234"), None); // 无命令字
    }

    /// 非帧行（ACK/固件 banner）不产生帧
    #[test]
    fn codec_ignores_non_frame_lines() {
        assert_eq!(parse_line("\r"), None);
        assert_eq!(parse_line("F0"), None); // 状态标志
        assert_eq!(parse_line("hello world\r"), None);
    }

    #[test]
    fn bitrate_codes() {
        assert_eq!(bitrate_code(125_000).unwrap(), 4);
        assert_eq!(bitrate_code(250_000).unwrap(), 5);
        assert_eq!(bitrate_code(500_000).unwrap(), 6);
        assert_eq!(bitrate_code(1_000_000).unwrap(), 8);
        assert!(bitrate_code(123_456).is_err());
    }
}
