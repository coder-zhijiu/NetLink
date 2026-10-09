//! Modbus PDU 处理（从机侧，FC 01/02/03/04/05/06/0F/10 + 异常应答）。
//! 输入输出均为纯 PDU（不含 MBAP 头 / RTU 地址与 CRC）。

use crate::model::Area;
use crate::runtime::DeviceRuntime;

pub const EX_ILLEGAL_FUNCTION: u8 = 0x01;
pub const EX_ILLEGAL_ADDRESS: u8 = 0x02;
pub const EX_ILLEGAL_VALUE: u8 = 0x03;

fn exception(fc: u8, code: u8) -> Vec<u8> {
    vec![fc | 0x80, code]
}

fn u16_be(b: &[u8]) -> Option<u16> {
    if b.len() < 2 {
        return None;
    }
    Some(((b[0] as u16) << 8) | b[1] as u16)
}

/// 处理一条请求 PDU，返回应答 PDU；None 表示协议层不应答。
pub fn process_request(pdu: &[u8], dev: &mut DeviceRuntime) -> Option<Vec<u8>> {
    let fc = *pdu.first()?;
    let body = &pdu[1..];
    let wrap_read = |fc: u8, r: Result<Vec<u8>, u8>| match r {
        Ok(bytes) => {
            let mut out = Vec::with_capacity(2 + bytes.len());
            out.push(fc);
            out.push(bytes.len() as u8);
            out.extend_from_slice(&bytes);
            out
        }
        Err(code) => exception(fc, code),
    };
    match fc {
        0x01 => Some(wrap_read(fc, read_bits(dev, Area::Coil, body))),
        0x02 => Some(wrap_read(fc, read_bits(dev, Area::DiscreteInput, body))),
        0x03 => Some(wrap_read(fc, read_regs(dev, Area::HoldingRegister, body))),
        0x04 => Some(wrap_read(fc, read_regs(dev, Area::InputRegister, body))),
        0x05 => write_single_coil(dev, body),
        0x06 => write_single_reg(dev, body),
        0x0F => write_multi_coils(dev, body),
        0x10 => write_multi_regs(dev, body),
        _ => Some(exception(fc, EX_ILLEGAL_FUNCTION)),
    }
}

/// 读位区域，返回打包后的数据字节；Err 为异常码
fn read_bits(dev: &DeviceRuntime, area: Area, body: &[u8]) -> Result<Vec<u8>, u8> {
    if body.len() != 4 {
        return Err(EX_ILLEGAL_VALUE);
    }
    let start = u16_be(body).unwrap_or(0);
    let qty = u16_be(&body[2..]).unwrap_or(0);
    if qty == 0 || qty > 2000 {
        return Err(EX_ILLEGAL_VALUE);
    }
    let blocks = dev.snapshot_blocks();
    let src = match area {
        Area::Coil => &blocks.coil,
        Area::DiscreteInput => &blocks.discrete,
        _ => return Err(EX_ILLEGAL_ADDRESS),
    };
    let start = start as usize;
    let qty = qty as usize;
    if start + qty > src.len() {
        return Err(EX_ILLEGAL_ADDRESS);
    }
    Ok(pack_bits(&src[start..start + qty]))
}

/// 读寄存器区域，返回大端数据字节；Err 为异常码
fn read_regs(dev: &DeviceRuntime, area: Area, body: &[u8]) -> Result<Vec<u8>, u8> {
    if body.len() != 4 {
        return Err(EX_ILLEGAL_VALUE);
    }
    let start = u16_be(body).unwrap_or(0);
    let qty = u16_be(&body[2..]).unwrap_or(0);
    if qty == 0 || qty > 125 {
        return Err(EX_ILLEGAL_VALUE);
    }
    let blocks = dev.snapshot_blocks();
    let src = match area {
        Area::HoldingRegister => &blocks.holding,
        Area::InputRegister => &blocks.input,
        _ => return Err(EX_ILLEGAL_ADDRESS),
    };
    let start = start as usize;
    let qty = qty as usize;
    if start + qty > src.len() {
        return Err(EX_ILLEGAL_ADDRESS);
    }
    let mut out = Vec::with_capacity(qty * 2);
    for w in &src[start..start + qty] {
        out.push((w >> 8) as u8);
        out.push((w & 0xFF) as u8);
    }
    Ok(out)
}

fn write_single_coil(dev: &mut DeviceRuntime, body: &[u8]) -> Option<Vec<u8>> {
    if body.len() != 4 {
        return Some(exception(0x05, EX_ILLEGAL_VALUE));
    }
    let addr = u16_be(body).unwrap_or(0);
    let val = u16_be(&body[2..]).unwrap_or(0);
    if val != 0x0000 && val != 0xFF00 {
        return Some(exception(0x05, EX_ILLEGAL_VALUE));
    }
    let blocks = dev.snapshot_blocks();
    if addr as usize >= blocks.coil.len() {
        return Some(exception(0x05, EX_ILLEGAL_ADDRESS));
    }
    let bit = val == 0xFF00;
    dev.write_coils(addr, &[bit]);
    // 标准行为：原样回显请求
    let mut r = vec![0x05];
    r.extend_from_slice(body);
    Some(r)
}

fn write_single_reg(dev: &mut DeviceRuntime, body: &[u8]) -> Option<Vec<u8>> {
    if body.len() != 4 {
        return Some(exception(0x06, EX_ILLEGAL_VALUE));
    }
    let addr = u16_be(body).unwrap_or(0);
    let val = u16_be(&body[2..]).unwrap_or(0);
    let blocks = dev.snapshot_blocks();
    if addr as usize >= blocks.holding.len() {
        return Some(exception(0x06, EX_ILLEGAL_ADDRESS));
    }
    dev.write_registers(addr, &[val]);
    let mut r = vec![0x06];
    r.extend_from_slice(body);
    Some(r)
}

fn write_multi_coils(dev: &mut DeviceRuntime, body: &[u8]) -> Option<Vec<u8>> {
    if body.len() < 5 {
        return Some(exception(0x0F, EX_ILLEGAL_VALUE));
    }
    let addr = u16_be(body).unwrap_or(0);
    let qty = u16_be(&body[2..]).unwrap_or(0);
    let byte_count = body[4] as usize;
    if qty == 0 || qty > 0x07B0 || byte_count != (qty as usize + 7) / 8 || body.len() != 5 + byte_count {
        return Some(exception(0x0F, EX_ILLEGAL_VALUE));
    }
    let blocks = dev.snapshot_blocks();
    let end = addr as usize + qty as usize;
    if end > blocks.coil.len() {
        return Some(exception(0x0F, EX_ILLEGAL_ADDRESS));
    }
    let mut bits = Vec::with_capacity(qty as usize);
    for i in 0..qty as usize {
        let byte = body[5 + i / 8];
        bits.push(byte & (1 << (i % 8)) != 0);
    }
    dev.write_coils(addr, &bits);
    let mut r = vec![0x0F];
    r.extend_from_slice(&body[0..4]);
    Some(r)
}

fn write_multi_regs(dev: &mut DeviceRuntime, body: &[u8]) -> Option<Vec<u8>> {
    if body.len() < 5 {
        return Some(exception(0x10, EX_ILLEGAL_VALUE));
    }
    let addr = u16_be(body).unwrap_or(0);
    let qty = u16_be(&body[2..]).unwrap_or(0);
    let byte_count = body[4] as usize;
    if qty == 0 || qty > 123 || byte_count != qty as usize * 2 || body.len() != 5 + byte_count {
        return Some(exception(0x10, EX_ILLEGAL_VALUE));
    }
    let blocks = dev.snapshot_blocks();
    let end = addr as usize + qty as usize;
    if end > blocks.holding.len() {
        return Some(exception(0x10, EX_ILLEGAL_ADDRESS));
    }
    let mut words = Vec::with_capacity(qty as usize);
    for i in 0..qty as usize {
        let hi = body[5 + i * 2] as u16;
        let lo = body[5 + i * 2 + 1] as u16;
        words.push((hi << 8) | lo);
    }
    dev.write_registers(addr, &words);
    let mut r = vec![0x10];
    r.extend_from_slice(&body[0..4]);
    Some(r)
}

/// 位序列打包为字节（MSB first，Modbus 规范）
pub fn pack_bits(bits: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; (bits.len() + 7) / 8];
    for (i, b) in bits.iter().enumerate() {
        if *b {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}
