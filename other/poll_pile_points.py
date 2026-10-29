#!/usr/bin/env python3
"""COM7 充电桩（双枪）：把 gen_pile_points.py 写入工程的点位轮询一遍，
按厂家源表解码后输出 Markdown 采集报告。

- 串口：COM7 9600 8N1；A 枪=从机 3、B 枪=从机 4（可命令行覆盖）
- FC04 读输入寄存器 4001~4029（0x0FA1~0x0FBD），一次 29 字
- 4024-4025 与 4027-4028 两个电量点均按厂家源表协议以标准大端 U32 解码
  （用户决策「按协议走」）；实测 4025 为有效低字 180/97，4028 为恒定脏值
  20225/34048——表底值协议读数会到百万 kWh 级，原始值见报告（2026-10-16）
- 工程值 = 原始值 × scale + offset（与 NetLink 工程点位一致）
- CRC 与 NetLink/mbtest.py 一致（0xA001，低字节在前）

用法：python other/poll_pile_points.py [COM口] [从机A] [从机B]
      默认 COM7 3 4
输出：other/COM7充电桩采集报告_YYYY-MM-DD.md
"""
import sys
import time
import datetime
from pathlib import Path

import serial

PORT = sys.argv[1] if len(sys.argv) > 1 else 'COM7'
UNIT_A = int(sys.argv[2]) if len(sys.argv) > 2 else 3
UNIT_B = int(sys.argv[3]) if len(sys.argv) > 3 else 4
BAUD = 9600
START, QTY = 4001, 29      # 0x0FA1 起 29 个寄存器，覆盖全部读点
INTERVAL = 0.35            # 查询间隔
TIMEOUT_MS = 2000          # 首字节等待上限；实测桩完整帧 <500ms 返回
TRIES = 3
OUT = Path(__file__).parent / f'COM7充电桩采集报告_{datetime.date.today():%Y-%m-%d}.md'

# ---------------------------------------------------------------- 点表 ----
# (地址, 名称, scale, offset, 单位, 枚举/备注)  —— 与 gen_pile_points.py 一致
POINTS = [
    (4001, '充电接口标识', 1, 0, '', {0: '未定义', 1: '枪A', 2: '枪B'}),
    (4002, '充电输出电压', 0.1, 0, 'V', ''),
    (4003, '充电输出电流', 0.1, -400, 'A', ''),
    (4004, 'SOC', 1, 0, '%', ''),
    (4005, '电池组最低温度', 1, -50, '℃', ''),
    (4006, '电池组最高温度', 1, -50, '℃', ''),
    (4007, '单体电池最高电压', 0.01, 0, 'V', ''),
    (4008, '单体电池最低电压', 0.01, 0, 'V', ''),
    (4009, '充电机环境温度', 1, -50, '℃', '源表标暂未使用，实测有数据'),
    (4010, '充电导引电压', 0.01, 0, 'V', '源表标暂未使用'),
    (4011, 'BMS需求电压', 0.1, 0, 'V', ''),
    (4012, 'BMS需求电流', 0.1, -400, 'A', ''),
    (4013, '充电模式', 1, 0, '', {1: '恒压充电', 2: '恒流充电'}),
    (4014, 'BMS充电电压测量值', 0.1, 0, 'V', ''),
    (4015, 'BMS充电电流测量值', 0.1, -400, 'A', ''),
    (4016, '估算剩余充电时间', 1, 0, 'min', ''),
    (4017, '充电接口温度探头1(DC+)', 1, -50, '℃', ''),
    (4018, '充电接口温度探头2(DC-)', 1, -50, '℃', ''),
    (4019, '充电接口温度探头3(DC+)', 1, -50, '℃', '源表标暂未使用（充电接口2）'),
    (4020, '充电接口温度探头4(DC-)', 1, -50, '℃', '源表标暂未使用（充电接口2）'),
    (4021, '产品型号', 1, 0, '', ''),
    (4022, '充电状态', 1, 0, '', {1: '待机', 2: '工作', 3: '自检', 4: '故障'}),
    (4023, '充电功率', 0.1, 0, 'kW', ''),
    (4024, '当前充电电量', 0.01, 0, 'kWh', 'U32占4024-4025标准大端，实测高字恒0、低字180/97=1.8/0.97kWh'),
    (4026, '双枪充状态', 1, 0, '', {0: '单枪充', 1: '双枪充'}),
    (4027, '充电电量表底值', 0.01, 0, 'kWh', '按协议走：U32占4027-4028大端解码。实测4028脏值20225/34048，协议读数为百万kWh级；4027单字71.8/61.46kWh才合理'),
    (4029, '枪状态', 1, 0, '', {0: '未插枪', 1: '插枪'}),
]

# 按 U32（高字在前）解码的地址；其余按 u16 单字。4024/4027 均按厂家源表协议配置
U32_ADDRS = {4024, 4027}


# ---------------------------------------------------------------- Modbus ----
def crc16(data: bytes) -> int:
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc


def hx(data: bytes) -> str:
    return ' '.join(f'{b:02X}' for b in data)


def build_read(unit: int) -> bytes:
    body = bytes([unit, 0x04, (START >> 8) & 0xFF, START & 0xFF, (QTY >> 8) & 0xFF, QTY & 0xFF])
    c = crc16(body)
    return body + bytes([c & 0xFF, c >> 8])


def transact(ser: serial.Serial, frame: bytes):
    """发一帧并收应答。返回 (应答, 首字节延迟ms, 总耗时ms)。
    先 read(1) 等首字节（可测出设备真实响应延迟），再按 150ms 帧内间隙收完。"""
    ser.reset_input_buffer()
    t0 = time.perf_counter()
    ser.write(frame)
    ser.flush()
    b = ser.read(1)
    first_ms = (time.perf_counter() - t0) * 1000
    buf = bytearray(b)
    if b:
        ser.timeout = 0.15
        while True:
            chunk = ser.read(256)
            if not chunk:
                break
            buf += chunk
        ser.timeout = TIMEOUT_MS / 1000.0
    return bytes(buf), first_ms, (time.perf_counter() - t0) * 1000


def poll_gun(ser: serial.Serial, unit: int):
    """轮询一把枪：返回 (regs 或 None, 首字节延迟ms, 帧hex记录)"""
    frames = []
    first_ms = None
    for attempt in range(1, TRIES + 1):
        frame = build_read(unit)
        resp, fms, tms = transact(ser, frame)
        first_ms = fms
        frames.append(f'TX: {hx(frame)}\nRX({len(resp)}B, 首字节{fms:.0f}ms): {hx(resp)}' if resp
                      else f'TX: {hx(frame)}\nRX: 无应答')
        ok = (len(resp) >= 6 and resp[1] == 0x04
              and crc16(resp[:-2]) == (resp[-1] << 8 | resp[-2])
              and len(resp) == 5 + QTY * 2)
        if ok:
            regs = [(resp[3 + i * 2] << 8) | resp[4 + i * 2] for i in range(QTY)]
            return regs, fms, frames
        time.sleep(INTERVAL)
    return None, first_ms, frames


# ---------------------------------------------------------------- 解码 ----
def raw_of(regs, addr: int) -> int:
    """按点类型取原始值：U32 地址高字在前拼两字，其余 u16 单字"""
    i = addr - START
    if addr in U32_ADDRS:
        return (regs[i] << 16) | regs[i + 1]
    return regs[i]


def main():
    ts = datetime.datetime.now()
    L = [f'# COM7 充电桩（双枪）点位采集报告 {ts:%Y-%m-%d %H:%M}', '']
    L.append(f'- 串口：{PORT} {BAUD} 8N1，超时 {TIMEOUT_MS}ms，重试 {TRIES} 次')
    L.append(f'- 功能码：FC04 输入寄存器，0x{START:04X}（{START}）起 {QTY} 字')
    L.append(f'- A 枪=从机 {UNIT_A}，B 枪=从机 {UNIT_B}')
    L.append('- 工程值 = 原始值 × scale + offset；4024 电量按 U32 大端解码、4027 表底按 u16 单字（见文末说明）')
    L.append('')

    ser = serial.Serial(PORT, BAUD, bytesize=8, parity='N', stopbits=1,
                        timeout=TIMEOUT_MS / 1000.0, write_timeout=1.0)

    data = {}
    for unit, label in [(UNIT_A, 'A枪'), (UNIT_B, 'B枪')]:
        regs, fms, frames = poll_gun(ser, unit)
        data[label] = regs
        status = '✓ 正常应答' if regs else '✗ 无有效应答'
        L.append(f'## {label}（从机 {unit}）')
        L.append('')
        L.append(f'**链路：{status}**（首字节延迟 {fms:.0f}ms）' if fms is not None
                 else f'**链路：{status}**')
        L.append('')
        L.append('```')
        L.extend(frames)
        L.append('```')
        L.append('')
        if unit == UNIT_A:
            time.sleep(INTERVAL)

    # 两枪对照表
    L.append('## 点位数据对照')
    L.append('')
    L.append('| 地址(hex) | 名称 | A枪原始 | A枪工程值 | B枪原始 | B枪工程值 | 备注 |')
    L.append('|---|---|---|---|---|---|---|')
    for addr, name, scale, offset, unit_s, enum in POINTS:
        i = addr - START
        cells = []
        for label in ['A枪', 'B枪']:
            regs = data[label]
            if regs is None:
                cells += ['—', '—']
                continue
            raw = raw_of(regs, addr)
            eng = raw * scale + offset
            if isinstance(enum, dict):
                note = enum.get(raw, '?')
                cells += [str(raw), f'{eng:g}（{note}）']
            else:
                cells += [str(raw), f'{eng:g} {unit_s}'.strip()]
        L.append(f'| {addr}(0x{addr:04X}) | {name} | {cells[0]} | {cells[1]} | {cells[2]} | {cells[3]} | {enum if isinstance(enum, str) else ""} |')
    L.append('')

    # 摘要
    def g(label, addr, field='eng'):
        regs = data[label]
        if regs is None:
            return None
        for a, name, scale, offset, unit_s, enum in POINTS:
            if a == addr:
                raw = raw_of(regs, addr)
                return raw * scale + offset if field == 'eng' else raw
        return None

    both = all(r is not None for r in data.values())
    L.append('## 摘要与解读')
    L.append('')
    if both:
        L.append(f"- 桩整体状态：A 枪「{g('A枪',4022):.0f}」/ B 枪「{g('B枪',4022):.0f}」"
                 f"（1=待机 2=工作 3=自检 4=故障），两枪均未插枪" if g('A枪', 4029) == 0 and g('B枪', 4029) == 0 else
                 f"- 桩整体状态：A 枪「{g('A枪',4022):.0f}」/ B 枪「{g('B枪',4022):.0f}」")
        L.append(f"- 充电输出：A 枪 {g('A枪',4002):g} V / {g('A枪',4003):g} A；"
                 f"B 枪 {g('B枪',4002):g} V / {g('B枪',4003):g} A")
        L.append(f"- 接口温度：A 枪 {g('A枪',4017):g}/{g('A枪',4018):g} ℃，B 枪 {g('B枪',4017):g}/{g('B枪',4018):g} ℃")
        L.append(f"- 电量：A 枪表底 {g('A枪',4027):g} kWh、当前 {g('A枪',4024):g} kWh；"
                 f"B 枪表底 {g('B枪',4027):g} kWh、当前 {g('B枪',4024):g} kWh")
        L.append(f"- 产品型号：A {g('A枪',4021,'raw'):g} / B {g('B枪',4021,'raw'):g}")
        L.append('')
        L.append('未插枪时 SOC、BMS 需求/测量、单体电压、充电模式、剩余时间、功率等点原始值为 0，属正常空数据；'
                 '4012/4015 经 −400 偏移后显示 −400A 是「无数据」的正常表现，不是故障。')
        L.append('')
        L.append('电量点说明（2026-10-16 实测，按用户决策均按厂家源表协议配置 U32 大端）：'
                 '「当前充电电量」4024-4025 协议解码与实测一致（高字恒 0，低字 180/97）；'
                 '「充电电量表底值」4027-4028 协议解码读数为百万 kWh 级（4028 实测恒为脏值 '
                 'A 枪 20225/B 枪 34048），而 4027 单字 71.8/61.46 kWh 才是合理量级——'
                 '设备固件与源表不符，如需读合理值可临时按 u16@4027 解读。'
                 '桩完整响应帧实测 <500ms 返回，工程超时 2000ms 留足余量。')
    else:
        L.append('存在无应答的枪，检查供电/接线/从机地址后重跑。')
    L.append('')

    OUT.write_text('\n'.join(L), encoding='utf-8')
    print(f'报告已写入: {OUT}')
    for label in ['A枪', 'B枪']:
        regs = data[label]
        print(f'{label}: {"✓" if regs else "✗"}', end='  ')
    print()


if __name__ == '__main__':
    main()
