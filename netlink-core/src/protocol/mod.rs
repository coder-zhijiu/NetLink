//! 协议实现层。

pub mod modbus;

use serde::Serialize;

/// 报文监视事件：与前端 FrameRecord 同构
#[derive(Debug, Clone, Serialize)]
pub struct FrameEvent {
    pub id: u64,
    /// epoch 毫秒
    pub ts: u64,
    pub conn_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<u64>,
    /// "in" = 收到主机请求，"out" = 从机应答
    pub dir: String,
    /// 空格分隔的大写十六进制
    pub hex: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl FrameEvent {
    pub fn new(conn_id: u64, device_id: Option<u64>, dir: &'static str, bytes: &[u8], note: Option<String>) -> Self {
        Self {
            id: 0,
            ts: crate::runtime::now_ms(),
            conn_id,
            device_id,
            dir: dir.to_string(),
            hex: hex_str(bytes),
            note,
        }
    }
}

/// 字节序列 → "AA BB CC" 大写十六进制
pub fn hex_str(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{:02X}", b));
    }
    s
}

/// "AA BB CC" / "AABBCC" 十六进制字符串 → 字节（容错空格/冒号/大小写）
pub fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if cleaned.len() % 2 != 0 || cleaned.is_empty() {
        return None;
    }
    (0..cleaned.len() / 2)
        .map(|i| u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16).ok())
        .collect()
}
