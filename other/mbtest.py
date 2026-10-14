#!/usr/bin/env python3
"""Modbus RTU 串口独立探测脚本（不依赖 NetLink，仅 pyserial）。

用途：设备接在 COM 口上时，用最原始的方式发一帧读请求，看从机应不应答，
从而判断「通信链路是否正常」以及隔离问题（接线/参数/地址/设备本身）。

用法：
  python mbtest.py COM7                     # 9600 8N1，从机 1，读保持寄存器 0 起 8 个，试 3 次
  python mbtest.py COM7 -b 19200            # 指定波特率
  python mbtest.py COM7 -p E -s 2           # 偶校验、2 停止位
  python mbtest.py COM7 --unit 2            # 指定从机地址
  python mbtest.py COM7 --fc 4 --addr 100   # 功能码 1/2/3/4，起始地址
  python mbtest.py COM7 --scan-unit         # 扫从机地址 1..32（每个地址发 1 帧）
  python mbtest.py COM7 --scan-baud         # 用当前从机地址扫常见波特率
  python mbtest.py COM7 --at                # 探测塔石 401SE：+++ 进指令模式后 AT+CGMR 查版本
  python mbtest.py COM7 --hex "01 03 00 00 00 0a"   # 发任意 HEX（末尾 CRC 自动补齐）
  python mbtest.py COM7 --raw "2b 2b 2b"    # 发任意原始字节（不做任何加工）
  python mbtest.py COM7 --listen 10         # 纯监听 10 秒：看总线上有没有主站轮询流量
  python mbtest.py --list                   # 列出本机可用 COM 口
"""
import argparse
import sys
import time

import serial
from serial.tools import list_ports

# 与 NetLink netlink-core/src/protocol/modbus/rtu.rs 的 crc16 一致：
# 多项式 0xA001、初值 0xFFFF；线上帧尾为 CRC 低字节在前。
def crc16(data: bytes) -> int:
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ 0xA001
            else:
                crc >>= 1
    return crc


def build_frame(unit: int, fc: int, addr: int, qty: int) -> bytes:
    body = bytes([unit, fc, (addr >> 8) & 0xFF, addr & 0xFF, (qty >> 8) & 0xFF, qty & 0xFF])
    c = crc16(body)
    return body + bytes([c & 0xFF, c >> 8])


def hx(data: bytes) -> str:
    return ' '.join(f'{b:02X}' for b in data)


def open_port(args, baud: int) -> serial.Serial:
    parity = {'N': serial.PARITY_NONE, 'E': serial.PARITY_EVEN, 'O': serial.PARITY_ODD}[args.parity]
    return serial.Serial(
        port=args.port, baudrate=baud, bytesize=8, parity=parity,
        stopbits=args.stopbits, timeout=args.timeout / 1000.0, write_timeout=1.0,
    )


def transact(ser: serial.Serial, frame: bytes, timeout_ms: int):
    """发一帧并收应答：首字节等满超时，后续 30ms 无新字节视为帧结束。"""
    ser.reset_input_buffer()
    ser.reset_output_buffer()
    t0 = time.perf_counter()
    ser.write(frame)
    ser.flush()
    buf = bytearray(ser.read(256))          # 首读：最多等到 timeout
    if buf:
        ser.timeout = 0.03                  # 帧内间隙，收完一帧就走
        while True:
            chunk = ser.read(256)
            if not chunk:
                break
            buf += chunk
        ser.timeout = timeout_ms / 1000.0
    elapsed = (time.perf_counter() - t0) * 1000
    return bytes(buf), elapsed


def crc_ok(resp: bytes) -> bool:
    return len(resp) >= 4 and int.from_bytes(resp[-2:], 'little') == crc16(resp[:-2])


def parse_resp(req: bytes, resp: bytes) -> str:
    """把应答帧解成人话。能走到这里就说明链路是通的。"""
    unit, fc = resp[0], resp[1]
    if not crc_ok(resp):
        return f'⚠ 应答 CRC 校验失败: {hx(resp)}（可能是干扰或半帧）'
    if fc & 0x80:
        codes = {1: '功能码不支持', 2: '地址不可读', 3: '数量超限', 4: '从机故障', 6: '忙，稍后重试', 0x0A: '网关路径不通'}
        why = codes.get(resp[2], f'未知 0x{resp[2]:02X}')
        return f'✅ 设备在线！返回异常应答（链路正常，该地址不可读）：功能码 0x{fc:02X} 异常码 0x{resp[2]:02X}（{why}）'
    if fc in (1, 2):
        n = resp[2]
        bits = [(resp[3 + i // 8] >> (i % 8)) & 1 for i in range(n * 8)]
        return f'✅ 正常应答：{len(bits)} 个位 → {bits[:32]}'
    if fc in (3, 4):
        n = resp[2] // 2
        regs = [int.from_bytes(resp[3 + 2 * i:5 + 2 * i], 'big') for i in range(n)]
        return f'✅ 正常应答：{n} 个寄存器 → ' + ', '.join(f'{v} (0x{v:04X})' for v in regs)
    return f'✅ 有应答：{hx(resp)}'


def probe(ser: serial.Serial, frame: bytes, tries: int, timeout_ms: int, label: str = '') -> bytes | None:
    for i in range(1, tries + 1):
        resp, ms = transact(ser, frame, timeout_ms)
        head = f'[{label} 试 {i}/{tries}] TX({len(frame)}B): {hx(frame)}'
        if resp:
            print(f'{head}\n{" " * len(head)}RX({len(resp)}B, {ms:.0f}ms): {hx(resp)}')
            return resp
        print(f'{head} → {ms:.0f}ms 无应答')
        time.sleep(0.2)
    return None


def at_probe(ser: serial.Serial, timeout_ms: int, tries: int) -> bytes | None:
    """塔石 401SE 探测：静默 500ms → +++（不带回车）→ 800ms → AT+CGMR。"""
    for i in range(1, tries + 1):
        time.sleep(0.5)                      # 进指令模式前的总线静默要求
        ser.write(b'+++')
        ser.flush()
        time.sleep(0.8)
        ser.write(b'AT+CGMR\r\n')
        ser.flush()
        buf = bytearray(ser.read(256))
        if buf:
            ser.timeout = 0.03
            while True:
                chunk = ser.read(256)
                if not chunk:
                    break
                buf += chunk
            ser.timeout = timeout_ms / 1000.0
            return bytes(buf)
        print(f'[AT 试 {i}/{tries}] +++ 与 AT+CGMR 无应答')
    return None


def listen(ser: serial.Serial, seconds: int, timeout_ms: int) -> bytes:
    """纯监听：不发数据，收总线上已有流量（若 401SE 开着网关/轮询，应能看到它的轮询帧）。"""
    print(f'-- 监听总线 {seconds} 秒（不发任何数据）……')
    t0 = time.perf_counter()
    total = bytearray()
    ser.timeout = 0.1
    while time.perf_counter() - t0 < seconds:
        chunk = ser.read(512)
        if chunk:
            total += chunk
            stamp = time.perf_counter() - t0
            print(f'  [{stamp:6.1f}s] RX({len(chunk)}B): {hx(chunk)}')
    ser.timeout = timeout_ms / 1000.0
    if not total:
        print('  总线静默，未收到任何数据')
    return bytes(total)


def diagnose(had_any: bool, args):
    print('\n' + '=' * 56)
    if had_any:
        print('结论：链路是通的（详见上方解析）。')
        return
    print('结论：✗ 完全无应答。按概率排查：')
    print('  1) RS485 的 A/B 两根线接反（最常见，现象就是完全无应答）')
    print('  2) 波特率/校验/停止位与从机不符 → 试 --scan-baud 自动扫')
    print('  3) 从机地址不对 → 试 --scan-unit 扫地址 1..32')
    if args.at:
        print('  4) AT 也无应答：若总线上只有 Modbus 从机属正常；若挂的是塔石 401SE 本体，')
        print('     注意它开启 Modbus 网关/寄存器轮询后会丢弃一切非 Modbus 数据（说明书 6.3）')
    print('  5) 从机供电不足 / USB-RS485 转换器问题（可换口或换线交叉验证）')


def main():
    ap = argparse.ArgumentParser(description='Modbus RTU 串口独立探测', formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    ap.add_argument('port', nargs='?', help='串口名，如 COM7')
    ap.add_argument('-b', '--baud', type=int, default=9600, help='波特率（默认 9600，401SE 出厂值）')
    ap.add_argument('-p', '--parity', choices=['N', 'E', 'O'], default='N', help='校验（默认 N）')
    ap.add_argument('-s', '--stopbits', type=int, choices=[1, 2], default=1, help='停止位（默认 1）')
    ap.add_argument('-t', '--timeout', type=int, default=500, help='读超时毫秒（默认 500）')
    ap.add_argument('--unit', type=int, default=1, help='从机地址（默认 1）')
    ap.add_argument('--fc', type=int, choices=[1, 2, 3, 4], default=3, help='功能码（默认 3 保持寄存器）')
    ap.add_argument('--addr', type=int, default=0, help='起始地址（默认 0）')
    ap.add_argument('--qty', type=int, default=8, help='读数量（默认 8）')
    ap.add_argument('--tries', type=int, default=3, help='每帧重试次数（默认 3）')
    ap.add_argument('--scan-unit', type=int, nargs='?', const=32, metavar='结束地址', help='扫从机地址 1..N（默认 1..32）')
    ap.add_argument('--scan-baud', action='store_true', help='用当前从机地址扫常见波特率')
    ap.add_argument('--at', action='store_true', help='探测塔石 401SE：+++ 进指令模式后 AT+CGMR 查版本')
    ap.add_argument('--hex', help='发任意 HEX 帧（如 "01 03 00 00 00 0a"，CRC 自动补齐）')
    ap.add_argument('--raw', help='发任意原始字节（如 "2b 2b 2b"，不做任何加工）')
    ap.add_argument('--listen', type=int, nargs='?', const=5, metavar='秒', help='纯监听总线（默认 5 秒）')
    ap.add_argument('--list', action='store_true', help='列出可用 COM 口')
    args = ap.parse_args()

    if args.list:
        for p in list_ports.comports():
            print(f'{p.device}  {p.description}')
        return
    if not args.port:
        ap.error('缺少串口名，例如：python mbtest.py COM7（--list 可先列出口）')

    ser = None
    try:
        ser = open_port(args, args.baud)
    except serial.SerialException as e:
        print(f'✗ 打开 {args.port} 失败：{e}')
        print('  常见原因：端口被 NetLink/串口助手占用（先停止连接或退出程序），或串口名不对（--list 查看）')
        sys.exit(1)

    fc_names = {1: '读线圈', 2: '读离散量', 3: '读保持寄存器', 4: '读输入寄存器'}
    print(f'串口 {args.port}：{args.baud} 8{args.parity}{args.stopbits}  超时 {args.timeout}ms')
    had_any = False

    try:
        if args.listen is not None:
            data = listen(ser, args.listen, args.timeout)
            if data:
                had_any = True
                print(f'（共收到 {len(data)}B —— 总线上有主站在活动，可用 --hex 观察具体帧）')

        if args.at:
            print('-- AT 探测（+++ 进指令模式 → AT+CGMR 查版本）')
            resp = at_probe(ser, args.timeout, args.tries)
            if resp:
                had_any = True
                print(f'AT 应答（原文）：{resp!r}')
            if args.scan_unit or args.scan_baud or args.hex:
                print()

        if args.scan_baud:
            print('-- 扫波特率（固定从机地址、帧参数）')
            for b in [1200, 2400, 4800, 9600, 14400, 19200, 38400, 57600, 115200]:
                if b == args.baud and not args.at and not args.scan_unit:
                    continue
                ser.baudrate = b
                frame = build_frame(args.unit, args.fc, args.addr, args.qty)
                resp, ms = transact(ser, frame, args.timeout)
                mark = f'波特率 {b:>6}: '
                if resp:
                    had_any = True
                    print(f'{mark}✅ 有应答 {hx(resp)} → 换成 -b {b} 再跑即可稳定读取')
                    break
                print(f'{mark}无应答')
                time.sleep(0.2)
            ser.baudrate = args.baud
            print()

        if args.scan_unit:
            hi = args.scan_unit
            print(f'-- 扫从机地址 1..{hi}（{fc_names[args.fc]} 地址 {args.addr} 数量 {args.qty}）')
            for u in range(1, hi + 1):
                frame = build_frame(u, args.fc, args.addr, args.qty)
                resp, ms = transact(ser, frame, args.timeout)
                if resp:
                    had_any = True
                    print(f'从机 {u:>3}: ✅ 有应答 {hx(resp)}')
                else:
                    print(f'从机 {u:>3}: 无应答')
                time.sleep(0.1)
            print()

        if args.raw:
            frame = bytes.fromhex(''.join(args.raw.split()))
            print(f'-- 发原始字节 {len(frame)}B: {hx(frame)}')
            resp = probe(ser, frame, args.tries, args.timeout, label='RAW')
            if resp:
                had_any = True
                print(f'应答（原文）：{resp!r}')

        if args.hex:
            raw = bytes.fromhex(''.join(args.hex.split()))
            c = crc16(raw[:-2]) if len(raw) >= 4 else None
            if len(raw) >= 4 and raw[-2:] == bytes([c & 0xFF, c >> 8]):
                frame = raw                      # 自带正确 CRC，原样发
            else:
                frame = raw + bytes([crc16(raw) & 0xFF, crc16(raw) >> 8])
                print(f'（HEX 帧末尾 CRC 不完整/不对，已自动补齐）')
            resp = probe(ser, frame, args.tries, args.timeout, label='HEX')
            if resp:
                had_any = True
                print(parse_resp(frame, resp))
        elif not (args.at or args.scan_baud or args.scan_unit or args.listen is not None or args.raw):
            frame = build_frame(args.unit, args.fc, args.addr, args.qty)
            print(f'-- {fc_names[args.fc]}：从机 {args.unit}，地址 {args.addr}，数量 {args.qty}')
            resp = probe(ser, frame, args.tries, args.timeout)
            if resp:
                had_any = True
                if resp[0] != args.unit:
                    print(f'⚠ 应答从机地址 {resp[0]} 与请求 {args.unit} 不一致（总线噪声？）')
                print(parse_resp(frame, resp))

        diagnose(had_any, args)
    except KeyboardInterrupt:
        print('\n已中断')
    finally:
        ser.close()


if __name__ == '__main__':
    main()
