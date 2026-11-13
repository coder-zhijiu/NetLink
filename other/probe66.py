#!/usr/bin/env python3
"""COM7 排查专用：全波特率 × 全从机地址扫描，找 66/67 或任何在线设备。
判定标准：应答 >= 2 字节才算有效（单字节 00 是方向切换毛刺，忽略）。
"""
import sys
import time

import serial


def crc16(data: bytes) -> int:
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc


def build_frame(unit: int, fc: int, addr: int, qty: int) -> bytes:
    body = bytes([unit, fc, (addr >> 8) & 0xFF, addr & 0xFF, (qty >> 8) & 0xFF, qty & 0xFF])
    c = crc16(body)
    return body + bytes([c & 0xFF, c >> 8])


def hx(data: bytes) -> str:
    return ' '.join(f'{b:02X}' for b in data)


def transact(ser: serial.Serial, frame: bytes, timeout_ms: int) -> tuple[bytes, float]:
    ser.reset_input_buffer()
    t0 = time.perf_counter()
    ser.write(frame)
    ser.flush()
    buf = bytearray(ser.read(256))
    if buf:
        ser.timeout = 0.05
        while True:
            chunk = ser.read(256)
            if not chunk:
                break
            buf += chunk
        ser.timeout = timeout_ms / 1000.0
    return bytes(buf), (time.perf_counter() - t0) * 1000


def valid(resp: bytes) -> bool:
    """有效 = 至少 2 字节；顺带把纯 00 单字节毛刺排掉。"""
    return len(resp) >= 2


def main():
    port = sys.argv[1] if len(sys.argv) > 1 else 'COM7'
    mode = sys.argv[2] if len(sys.argv) > 2 else 'baud'
    ser = serial.Serial(port=port, baudrate=9600, bytesize=8, parity='N', stopbits=1,
                        timeout=0.5, write_timeout=1.0)

    if mode == 'baud':
        # 阶段1：全波特率扫从机 66（FC03 与 FC04 各来一帧）
        for baud in [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200]:
            ser.baudrate = baud
            for fc in (3, 4):
                resp, ms = transact(ser, build_frame(66, fc, 0, 8), 400)
                tag = '有效' if valid(resp) else ('毛刺' if resp else '静默')
                print(f'baud {baud:>6} FC{fc:02d}: {tag:4} {ms:6.0f}ms RX({len(resp)}B): {hx(resp)}')
            time.sleep(0.2)
    elif mode == 'unit':
        # 阶段2：9600 下扫 1..247，双帧（FC03/FC04），找任何应答
        for u in range(1, 248):
            for fc in (3, 4):
                resp, ms = transact(ser, build_frame(u, fc, 0, 4), 150)
                if valid(resp):
                    print(f'从机 {u:>3} FC{fc:02d}: ✅ RX({len(resp)}B, {ms:.0f}ms): {hx(resp)}')
            if u % 40 == 0:
                print(f'  ...已扫到 {u}', flush=True)
    ser.close()


if __name__ == '__main__':
    main()
