#!/usr/bin/env python3
"""COM7 锚浪 Solis 逆变器 · 寄存器空间只读扫描（不写任何寄存器）。

目的：除了协议 Ver3.7 文档定义的点位，还有哪些地址可读、有值。
方法：
  1) FC04/FC03 按 125 寄存器块粗扫 [0, 42100)，得到 OK/异常地图
  2) 与可读块相邻的异常块逐寄存器细扫（qty=1），精确定位边界与跨块小岛
  3) 35000（已知特殊地址）附近固定窗口逐点细扫
  4) 全 16 位地址空间 500 步长探针（岛检测），命中即 ±25 细扫
  5) FC02 离散量探测（静默则判定未实现，跳过）
  6) 与文档 5.1/5.2 地址集比对，输出「协议外有值」清单到 md
用法：python other/scan_solis_space.py [COM口]   （默认 COM7）
"""
import sys
import time
import datetime
from pathlib import Path

import serial
from serial.tools import list_ports

PORT = sys.argv[1] if len(sys.argv) > 1 else 'COM7'
UNIT, BAUD = 1, 9600
INTERVAL = 0.32          # >300ms 协议要求
TIMEOUT_MS = 800
SWEEP_END = 42100
OUT = Path(__file__).parent / f'COM7协议外寄存器扫描_{datetime.date.today():%Y-%m-%d}.md'

# ---- 协议文档定义的地址（按功能码区分），(起, 止, 名称) ----
SPEC4 = [
    (2999, 2999, '产品型号'), (3000, 3000, 'DSP软件版本'), (3001, 3001, '液晶软件版本'),
    (3003, 3003, '直流输入类型'), (3004, 3005, '有功功率'), (3006, 3007, '总直流输入功率'),
    (3008, 3009, '总发电量'), (3010, 3011, '当月发电量'), (3012, 3013, '上月发电量'),
    (3014, 3014, '当日发电量'), (3015, 3015, '昨日发电量'), (3020, 3020, '液晶软件版本2'),
    (3021, 3021, '直流电压1'), (3022, 3022, '直流电流1'), (3023, 3023, '直流电压2'),
    (3024, 3024, '直流电流2'), (3025, 3025, '直流电压3'), (3026, 3026, '直流电流3'),
    (3027, 3027, '直流电压4'), (3028, 3028, '直流电流4'), (3029, 3029, '警告信息故障数据'),
    (3033, 3033, 'AB线电压'), (3034, 3034, 'BC线电压'), (3035, 3035, 'CA线电压'),
    (3036, 3036, 'A相电流'), (3037, 3037, 'B相电流'), (3038, 3038, 'C相电流'),
    (3040, 3040, '标准工作模式'), (3041, 3041, '模块温度'), (3042, 3042, '电网频率'),
    (3043, 3043, '当前状态'), (3044, 3045, '限有功功率'), (3046, 3047, '限无功功率'),
    (3049, 3049, '有功功率百分比设定'), (3051, 3051, 'PF设定值'), (3052, 3052, '无功百分比设定'),
    (3053, 3053, '国家标准号'), (3055, 3056, '无功功率值'), (3057, 3058, '视在功率值'),
    (3059, 3059, '实时功率因数'), (3060, 3063, '序列号SN'), (3071, 3071, '工作状态位'),
    (3072, 3077, '系统时间'), (3089, 3089, '有功启停开关'), (3090, 3090, '无功启停开关'),
    (3094, 3094, '降额/限额状态'), (3095, 3102, '故障字01-08'), (3103, 3103, '警示信息3_DSP'),
    (3104, 3104, '警示信息HMI'), (3109, 3109, '本机标定额定功率'), (3288, 3297, 'PV21-30电流'),
    (3298, 3298, '总PV电压'), (3299, 3299, '总PV电流'), (3300, 3319, 'PV1-20电流'),
    (3336, 3337, 'PV31-32电流'), (3471, 3473, '故障字10-12'), (3499, 3513, 'MPPT1-15电压'),
    (3529, 3545, 'MPPT1-16电流'), (35000, 35000, '逆变器类型定义'),
]
SPEC3 = [
    (2999, 3004, '实时时钟'), (3006, 3006, '开关机'), (3050, 3050, '无功功率百分比设定'),
    (3051, 3051, '有功功率百分比设定'), (3053, 3053, '功率因数设定'), (3068, 3068, '掉电保存功能'),
    (3069, 3069, '有功启停开关(设置)'), (3070, 3070, '无功启停开关(设置)'), (3080, 3080, '有功功率调节值1'),
    (3082, 3082, '无功功率调节值1'), (3222, 3223, '有功功率调节值2'), (3224, 3225, '无功功率调节值2'),
]


def build_spec(spec):
    addrs, names = set(), {}
    for lo, hi, name in spec:
        for a in range(lo, hi + 1):
            addrs.add(a)
            names[a] = name if lo == hi else f'{name}({a})'
    return addrs, names


DOC4, NAMES4 = build_spec(SPEC4)
DOC3, NAMES3 = build_spec(SPEC3)


def crc16(data: bytes) -> int:
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc


def hx(data: bytes) -> str:
    return ' '.join(f'{b:02X}' for b in data)


def build_frame(fc: int, addr: int, qty: int) -> bytes:
    body = bytes([UNIT, fc, (addr >> 8) & 0xFF, addr & 0xFF, (qty >> 8) & 0xFF, qty & 0xFF])
    c = crc16(body)
    return body + bytes([c & 0xFF, c >> 8])


def transact(ser, frame):
    ser.reset_input_buffer()
    ser.reset_output_buffer()
    ser.write(frame)
    ser.flush()
    first = ser.read(1)
    buf = bytearray(first)
    if first:
        ser.timeout = 0.03
        while True:
            chunk = ser.read(256)
            if not chunk:
                break
            buf += chunk
        ser.timeout = TIMEOUT_MS / 1000.0
    return bytes(buf)


FRAME_COUNT = 0
T0 = time.time()


def read_regs(ser, fc, addr, qty):
    """读寄存器：返回 (值list 或 None, 错误描述)。超时/CRC 重试 1 次。"""
    global FRAME_COUNT
    exp_len = qty * 2 if fc in (3, 4) else (qty + 7) // 8
    last_err = ''
    for _ in range(2):
        FRAME_COUNT += 1
        resp = transact(ser, build_frame(fc, addr, qty))
        time.sleep(INTERVAL)
        if not resp:
            last_err = '无应答'
        elif len(resp) >= 4 and int.from_bytes(resp[-2:], 'little') != crc16(resp[:-2]):
            last_err = 'CRC错'
            continue
        elif resp[1] & 0x80:
            last_err = f'异常{resp[2]:02X}'
            return None, last_err          # 异常是确定性应答，不重试
        elif resp[1] == fc and resp[2] == exp_len:
            if fc in (3, 4):
                return [int.from_bytes(resp[3 + 2 * j:5 + 2 * j], 'big') for j in range(qty)], None
            return list(resp[3:3 + exp_len]), None           # fc2: 字节
        else:
            last_err = f'格式异常:{hx(resp[:8])}'
    return None, last_err


def intervals_of(keys):
    out = []
    for a in sorted(keys):
        if out and a == out[-1][1] + 1:
            out[-1][1] = a
        else:
            out.append([a, a])
    return out


def main():
    try:
        ser = serial.Serial(port=PORT, baudrate=BAUD, bytesize=8, parity=serial.PARITY_NONE,
                            stopbits=1, timeout=TIMEOUT_MS / 1000.0, write_timeout=1.0)
    except serial.SerialException as e:
        print(f'✗ 打开 {PORT} 失败：{e}')
        for p in list_ports.comports():
            print(f'    {p.device}  {p.description}')
        sys.exit(1)

    print(f'串口 {PORT}：{BAUD} 8N1 从机 {UNIT}，只读扫描（绝不写）')

    # ---- 块大小协商 ----
    STEP = None
    for q in (125, 100, 64, 32):
        regs, err = read_regs(ser, 4, 2999, q)
        if regs is not None:
            STEP = q
            break
    if STEP is None:
        print(f'✗ 连 2999 起始区都读不了（{err}）——设备状态与上次联调时不同，退出')
        sys.exit(1)
    print(f'块大小协商：{STEP} 寄存器/帧')

    data = {4: {}, 3: {}}        # addr -> u16 值
    blocks = {4: {}, 3: {}}      # 块起 -> (regs|None, err)
    exc_stat = {4: {}, 3: {}}    # 异常码 -> 出现块数

    # ---- 1) 粗扫 ----
    for fc in (4, 3):
        n = (SWEEP_END + STEP - 1) // STEP
        print(f'-- FC0{fc} 粗扫 [0, {SWEEP_END})：{n} 块')
        for i, start in enumerate(range(0, SWEEP_END, STEP)):
            regs, err = read_regs(ser, fc, start, STEP)
            blocks[fc][start] = (regs, err)
            if regs is not None:
                for j, v in enumerate(regs):
                    data[fc][start + j] = v
            else:
                exc_stat[fc][err] = exc_stat[fc].get(err, 0) + 1
            if (i + 1) % 60 == 0:
                print(f'   {i + 1}/{n} 块，可读 {len(data[fc])} 寄存器，{time.time() - T0:.0f}s')

    # ---- 2) 边界块逐点细扫 ----
    for fc in (4, 3):
        ok_starts = {s for s, (r, e) in blocks[fc].items() if r is not None}
        adj = set()
        for s in ok_starts:
            for nb in (s - STEP, s + STEP):
                if nb in blocks[fc] and blocks[fc][nb][0] is None:
                    adj.add(nb)
        for s in sorted(adj):
            print(f'-- FC0{fc} 细扫边界块 [{s}, {s + STEP})')
            for a in range(s, s + STEP):
                if a in data[fc]:
                    continue
                regs, err = read_regs(ser, fc, a, 1)
                if regs is not None:
                    data[fc][a] = regs[0]

    # ---- 3) 35000 固定窗口细扫（小岛防漏） ----
    print('-- FC04 细扫 35000 邻域 [34975, 35026)')
    for a in range(34975, 35026):
        if a in data[4]:
            continue
        regs, err = read_regs(ser, 4, a, 1)
        if regs is not None:
            data[4][a] = regs[0]

    # ---- 4) 500 步长全空间岛探针 ----
    for fc in (4, 3):
        print(f'-- FC0{fc} 岛探针（步长 500）')
        for a in range(0, 65536, 500):
            if a in data[fc]:
                continue
            regs, err = read_regs(ser, fc, a, 1)
            if regs is not None:
                data[fc][a] = regs[0]
                for b in range(max(0, a - 25), min(65536, a + 26)):
                    if b not in data[fc]:
                        r2, _ = read_regs(ser, fc, b, 1)
                        if r2 is not None:
                            data[fc][b] = r2[0]

    # ---- 5) FC02 离散量 ----
    fc2_note = ''
    probe, err = read_regs(ser, 2, 0, 16)
    if probe is None and err == '无应答':
        fc2_note = '静默无应答 → 固件未实现 FC02（与此前结论一致）'
    else:
        fc2_hits = {}
        for start in range(0, 65536, 2000):
            regs, e = read_regs(ser, 2, start, 2000)
            if regs is not None:
                for j, byte in enumerate(regs):
                    if byte:
                        for bit in range(8):
                            if byte & (1 << bit):
                                fc2_hits[start + j * 8 + bit] = 1
        fc2_note = (f'有应答（{len(fc2_hits)} 个离散量位为 1）' if fc2_hits
                    else '有应答，全空间 65536 位均为 0/可读但无置位')
        data[2] = fc2_hits
    ser.close()
    print(f'扫描完成：共发 {FRAME_COUNT} 帧，耗时 {time.time() - T0:.0f}s')

    # ---- 6) 分类与报告 ----
    def nearest_doc(fc, a):
        doc, names = (DOC4, NAMES4) if fc == 4 else (DOC3, NAMES3)
        cand = [(abs(d - a), d) for d in doc if abs(d - a) <= 10]
        if not cand:
            return ''
        _, d = min(cand)
        return f'邻近 {d} {names[d]}'

    findings = []           # (fc, addr, value)
    zero_undoc = {4: 0, 3: 0}
    for fc in (4, 3):
        doc = DOC4 if fc == 4 else DOC3
        for a, v in sorted(data[fc].items()):
            if a in doc:
                continue
            if v != 0:
                findings.append((fc, a, v))
            else:
                zero_undoc[fc] += 1

    L = []
    L.append('# COM7 锦浪 Solis 逆变器 · 协议外寄存器空间扫描报告')
    L.append('')
    L.append(f'- **扫描时间**：{datetime.datetime.now():%Y-%m-%d %H:%M:%S}（耗时 {time.time() - T0:.0f}s，共 {FRAME_COUNT} 帧，全程只读）')
    L.append(f'- **链路**：{PORT}，Modbus RTU，从机 {UNIT}，{BAUD} 8N1，查询间隔 {INTERVAL*1000:.0f}ms')
    L.append(f'- **方法**：FC04/FC03 以 {STEP} 寄存器/帧粗扫 [0, {SWEEP_END})；可读区相邻异常块逐点细扫；'
             f'35000 邻域细扫；全 16 位空间 500 步长岛探针（命中 ±25 细扫）；FC02 离散量探测')
    L.append('')
    for fc in (4, 3):
        doc = DOC4 if fc == 4 else DOC3
        L.append(f'## {"FC04 输入寄存器（4xxx 区）" if fc == 4 else "FC03 保持寄存器（设置区）"}可读区地图')
        L.append('')
        ivs = intervals_of(data[fc].keys())
        L.append(f'共 {len(data[fc])} 个可读地址，分 {len(ivs)} 段：')
        L.append('')
        for lo, hi in ivs:
            n_doc = sum(1 for a in range(lo, hi + 1) if a in doc)
            L.append(f'- **[{lo}, {hi}]**（{hi - lo + 1} 个地址，其中协议定义 {n_doc} 个）'
                     f' —— hex 0x{lo:X}~0x{hi:X}')
        L.append('')
        if exc_stat[fc]:
            L.append(f'区外行为：{ "；".join(f"{k}×{v}块" for k, v in sorted(exc_stat[fc].items())) }')
            L.append('')
    L.append('## FC02 离散量输入')
    L.append('')
    L.append(f'- {fc2_note}')
    L.append('')
    L.append('## 协议未定义但「有值」的寄存器（本次扫描的重点产出）')
    L.append('')
    if findings:
        L.append('| 功能码 | 地址(十进制) | 地址(hex) | 原始值 | 邻近协议点 |')
        L.append('| --- | ---: | --- | --- | --- |')
        for fc, a, v in findings:
            L.append(f'| FC0{fc} | {a} | 0x{a:X} | 0x{v:04X}（{v}） | {nearest_doc(fc, a)} |')
    else:
        L.append('（无——可读的协议外地址全部恒为 0）')
    L.append('')
    L.append('## 可读但恒 0 的协议外地址')
    L.append('')
    L.append(f'- FC04：{zero_undoc[4]} 个（多为预留/未实现，协议 3.4 说明区内未实现地址回 0 不抛异常）')
    L.append(f'- FC03：{zero_undoc[3]} 个')
    L.append('')
    L.append('## 结论与局限')
    L.append('')
    L.append('- 扫描为 125 块粗扫 + 边界/特殊窗口逐点细扫 + 500 步长探针：'
             '远离边界、宽度小于探针步长的孤立可读地址理论上仍可能漏检（如 35000 这类特殊点已在邻域细扫覆盖）。')
    L.append('- 「有值」仅代表当前时刻非 0，含义需结合固件/锦浪后续协议文档确认，本报告不做语义猜测。')
    L.append('')
    OUT.write_text('\n'.join(L), encoding='utf-8')
    print(f'报告已写入：{OUT}')

    print('\n===== 扫描结论速览 =====')
    for fc in (4, 3):
        ivs = intervals_of(data[fc].keys())
        print(f'FC0{fc}: {len(data[fc])} 可读地址 / {len(ivs)} 段: ' + '; '.join(f'[{lo}~{hi}]' for lo, hi in ivs))
    print(f'FC02: {fc2_note}')
    if findings:
        print(f'协议外有值寄存器 {len(findings)} 个：')
        for fc, a, v in findings:
            print(f'  FC0{fc} {a} (0x{a:X}) = 0x{v:04X} ({v})  {nearest_doc(fc, a)}')
    else:
        print('协议外无任何非 0 寄存器')


if __name__ == '__main__':
    main()
