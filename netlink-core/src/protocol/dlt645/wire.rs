use crate::model::{Dlt645Format, Dlt645Point};

const MAX_DATA: usize = 200;

pub fn address_bytes(address: &str) -> Result<[u8; 6], String> {
    if address.len() != 12
        || !address.bytes().all(|b| b.is_ascii_digit())
        || address == "999999999999"
    {
        return Err("表地址须为 12 位十进制数字，不能使用广播地址".into());
    }
    let mut bytes = [0; 6];
    for (i, pair) in address.as_bytes().rchunks_exact(2).enumerate() {
        bytes[i] = ((pair[0] - b'0') << 4) | (pair[1] - b'0');
    }
    Ok(bytes)
}

pub fn address_text(address: [u8; 6]) -> String {
    address.iter().rev().map(|b| format!("{b:02X}")).collect()
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().copied().fold(0, u8::wrapping_add)
}

#[derive(Clone, Copy)]
pub enum Request {
    Address,
    Data { address: [u8; 6], di: u32 },
}

impl Request {
    pub fn bytes(self) -> Vec<u8> {
        let (address, control, data) = match self {
            Self::Address => ([0xaa; 6], 0x13, vec![]),
            Self::Data { address, di } => (address, 0x11, di.to_le_bytes().to_vec()),
        };
        let mut bytes = vec![0xfe; 4];
        bytes.push(0x68);
        bytes.extend(address);
        bytes.extend([0x68, control, data.len() as u8]);
        bytes.extend(data.iter().map(|b| b.wrapping_add(0x33)));
        bytes.extend([checksum(&bytes[4..]), 0x16]);
        bytes
    }

    pub fn description(self) -> String {
        match self {
            Self::Address => "读取通信地址".into(),
            Self::Data { address, di } => {
                format!("表 {} · 读取 DI {di:08X}", address_text(address))
            }
        }
    }

    pub fn address_matches(self, received: [u8; 6]) -> bool {
        match self {
            Self::Address => true,
            Self::Data { address, .. } => received == address,
        }
    }

    pub fn response(self, frame: &Frame) -> Option<Result<(), String>> {
        if !self.address_matches(frame.address) {
            return None;
        }
        match self {
            Self::Address if frame.control == 0x93 => {
                let address = address_text(frame.address);
                Some(
                    if address_bytes(&address).is_ok() && frame.data == frame.address {
                        Ok(())
                    } else {
                        Err("读址应答地址无效或与帧头不一致".into())
                    },
                )
            }
            Self::Data { .. } if frame.control == 0xd1 => Some(Err(match frame.data.as_slice() {
                [error] if error & 0x02 != 0 => format!("不支持：无此数据（异常字 {error:02X}）"),
                [error] => format!("电表异常应答（异常字 {error:02X}）"),
                _ => "异常应答长度无效".into(),
            })),
            Self::Data { di, .. } if matches!(frame.control, 0x91 | 0xb1) => {
                if frame.data.len() < 4 {
                    return Some(Err("读数据应答缺少 DI".into()));
                }
                if frame.data[..4] != di.to_le_bytes() {
                    return None;
                }
                Some(if frame.control == 0xb1 {
                    Err("应答包含后续帧，当前点位仅支持单帧数值".into())
                } else {
                    Ok(())
                })
            }
            _ => None,
        }
    }
}

pub struct Frame {
    pub bytes: Vec<u8>,
    pub address: [u8; 6],
    pub control: u8,
    pub data: Vec<u8>,
    pub valid: bool,
}

impl Frame {
    pub fn take(buffer: &mut Vec<u8>) -> Option<Self> {
        loop {
            let Some(start) = buffer.iter().position(|b| *b == 0x68) else {
                // 保留最多四个前导字节供报文监视。
                let keep = buffer
                    .iter()
                    .rev()
                    .take(4)
                    .take_while(|b| **b == 0xfe)
                    .count();
                buffer.drain(..buffer.len() - keep);
                return None;
            };
            let preamble = buffer[..start]
                .iter()
                .rev()
                .take(4)
                .take_while(|b| **b == 0xfe)
                .count();
            buffer.drain(..start - preamble);
            if buffer.len() < preamble + 10 {
                return None;
            }
            let len = buffer[preamble + 9] as usize;
            if buffer[preamble + 7] != 0x68 || len > MAX_DATA {
                buffer.drain(..=preamble);
                continue;
            }
            let end = preamble + 12 + len;
            let bytes = buffer.get(..end)?;
            let address = bytes[preamble + 1..preamble + 7].try_into().unwrap();
            let control = bytes[preamble + 8];
            let data = bytes[preamble + 10..end - 2]
                .iter()
                .map(|b| b.wrapping_sub(0x33))
                .collect();
            let valid =
                bytes[end - 1] == 0x16 && checksum(&bytes[preamble..end - 2]) == bytes[end - 2];
            let frame = Self {
                bytes: bytes.to_vec(),
                address,
                control,
                data,
                valid,
            };
            buffer.drain(
                ..if valid || bytes[end - 1] == 0x16 {
                    end
                } else {
                    preamble + 1
                },
            );
            return Some(frame);
        }
    }
}

pub fn decode_value(point: &Dlt645Point, data: &[u8]) -> Result<(f64, f64), String> {
    if data.len() != point.bytes as usize {
        return Err(format!(
            "数据长度为 {} 字节，点位配置为 {} 字节",
            data.len(),
            point.bytes
        ));
    }
    let number = match point.format {
        Dlt645Format::Bcd | Dlt645Format::SignedBcd => {
            let signed = matches!(point.format, Dlt645Format::SignedBcd);
            let negative = signed && data[data.len() - 1] & 0x80 != 0;
            let mut number = 0_i64;
            for (index, byte) in data.iter().rev().copied().enumerate() {
                let digit_pair = if signed && index == 0 {
                    byte & 0x7f
                } else {
                    byte
                };
                let (hi, lo) = (digit_pair >> 4, digit_pair & 0xf);
                if hi > 9 || lo > 9 {
                    return Err("应答包含无效 BCD 数字".into());
                }
                number = number * 100 + i64::from(hi) * 10 + i64::from(lo);
            }
            if negative {
                -number
            } else {
                number
            }
        }
        Dlt645Format::Unsigned | Dlt645Format::Signed => {
            let negative =
                matches!(point.format, Dlt645Format::Signed) && data[data.len() - 1] & 0x80 != 0;
            let mut bytes = [if negative { 0xff } else { 0 }; 8];
            bytes[..data.len()].copy_from_slice(data);
            i64::from_le_bytes(bytes)
        }
    };
    let raw = number as f64 / 10_f64.powi(i32::from(point.decimals));
    let value = raw * point.scale;
    if !value.is_finite() {
        return Err("读数乘倍率后超出数值范围".into());
    }
    Ok((raw, value))
}
