#!/usr/bin/env python3
"""Modbus TCP 从机 ID 探测脚本（独立脚本，仅标准库；不依赖、不改动 NetLink）。

场景：设备是 Modbus TCP server（IP:502），但从站 ID 不确定。
逐个 unit id 发读请求，看哪些 ID 后面真的有从机在应答。

用法：
  python mbtcp_scan.py 192.168.0.80                    # 扫 unit 1..100（FC03 无果时换 FC04 再试）
  python mbtcp_scan.py 192.168.0.80 --lo 60 --hi 77    # 只扫给定范围
  python mbtcp_scan.py 192.168.0.80 --unit 65          # 细看某个从机：FC03/FC04 读 0..15 + 设备标识
  python mbtcp_scan.py 192.168.0.80 --unit 65 --fc 4 --addr 3000 --qty 20
  python mbtcp_scan.py 192.168.0.80 --unit 65 --ident  # 只读设备标识（FC43/14）
  python mbtcp_scan.py 192.168.0.80 --rtu              # 透传 DTU：发裸 RTU 帧（带 CRC）而非 MBAP

判读要点：
  - 正常应答 / 异常码 01~04 → 该 unit 后面有从机在线
  - 异常码 0x0B / 0x0A（网关专用）→ 网关后面没有这个 ID 的从机
  - 一律超时 → 该 ID 无应答（或服务器只认特定 ID）
  - 所有 ID 回一模一样的应答 → 该服务器忽略 unit id（单从机直连 TCP 常见）
  - MBAP 与 RTU 都静默但 TCP 连得上 → 透传 DTU 的串口侧无从机应答（查接线/供电/波特率）
"""
import argparse
import socket
import struct
import sys
import time

if hasattr(sys.stdout, 'reconfigure'):
    sys.stdout.reconfigure(encoding='utf-8', errors='replace')


def crc16(data: bytes) -> int:
    """与 mbtest.py / netlink-core rtu.rs 一致：多项式 0xA001、初值 0xFFFF。"""
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc

EXC = {
    0x01: '功能码不支持', 0x02: '地址不可读', 0x03: '数量超限', 0x04: '从机故障',
    0x05: '厂商自定义异常码', 0x06: '忙，稍后重试', 0x08: '校验/存储错',
    0x0A: '网关路径不可用(0x0A)', 0x0B: '网关后无此从机(0x0B)',
}


def hx(data: bytes) -> str:
    return ' '.join(f'{b:02X}' for b in data)


def read_pdu(fc: int, addr: int, qty: int) -> bytes:
    return bytes([fc, (addr >> 8) & 0xFF, addr & 0xFF, (qty >> 8) & 0xFF, qty & 0xFF])


def ident_pdu(read_code: int = 1) -> bytes:
    """FC43/14 读设备标识，read_code 1=basic(厂商标识) 2=regular 3=extended"""
    return bytes([0x2E, 0x0E, read_code])


class Client:
    def __init__(self, host: str, port: int, timeout_ms: int, rtu: bool = False):
        self.host, self.port, self.timeout = host, port, timeout_ms
        self.rtu = rtu                # True：透传 DTU，线上是裸 RTU 帧（[unit][PDU][CRC]）
        self.sock = None
        self.tid = 0

    def connect(self):
        self.sock = socket.create_connection((self.host, self.port), timeout=3.0)

    def close(self):
        if self.sock:
            try:
                self.sock.close()
            except OSError:
                pass
            self.sock = None

    def transact(self, unit: int, pdu: bytes, timeout_ms: int | None = None):
        """发一帧收一帧。返回 (resp_pdu, elapsed_ms, err)；err 非 None 表示无应答。"""
        t = (timeout_ms if timeout_ms is not None else self.timeout) / 1000.0
        if self.sock is None:
            self.connect()
        if self.rtu:
            body = bytes([unit]) + pdu
            c = crc16(body)
            frame = body + bytes([c & 0xFF, c >> 8])
        else:
            self.tid = (self.tid + 1) & 0xFFFF
            frame = struct.pack('>HHHB', self.tid, 0, len(pdu) + 1, unit) + pdu
        t0 = time.perf_counter()
        try:
            self.sock.settimeout(t)
            self.sock.sendall(frame)
        except OSError:
            # 有些服务器对不认识的 unit 会直接断连，重连一次再发
            self.close()
            try:
                self.connect()
                self.sock.settimeout(t)
                self.sock.sendall(frame)
            except OSError as e:
                return None, (time.perf_counter() - t0) * 1000, f'发送失败: {e}'
        if self.rtu:
            # RTU 无长度域：首字节等满超时，之后 50ms 无新字节视为帧结束
            try:
                buf = bytearray()
                while not buf:
                    chunk = self.sock.recv(512)
                    if not chunk:
                        return None, (time.perf_counter() - t0) * 1000, '连接被设备关闭'
                    buf += chunk
                self.sock.settimeout(0.05)
                while True:
                    try:
                        chunk = self.sock.recv(512)
                    except socket.timeout:
                        break
                    if not chunk:
                        break
                    buf += chunk
                elapsed = (time.perf_counter() - t0) * 1000
                if len(buf) >= 5 and int.from_bytes(buf[-2:], 'little') == crc16(buf[:-2]):
                    return bytes(buf[1:-2]), elapsed, None   # 去掉 unit 与 CRC，返回 PDU
                return None, elapsed, f'应答 CRC 不符: {hx(bytes(buf))}'
            except socket.timeout:
                return None, (time.perf_counter() - t0) * 1000, '超时无应答'
            except OSError as e:
                self.close()
                return None, (time.perf_counter() - t0) * 1000, f'接收错误: {e}'
        try:
            buf = b''
            while len(buf) < 7:
                chunk = self.sock.recv(7 - len(buf))
                if not chunk:
                    return None, (time.perf_counter() - t0) * 1000, '连接被设备关闭'
                buf += chunk
            tid, pid, length, unit_r = struct.unpack('>HHHB', buf)
            if pid != 0:
                return None, (time.perf_counter() - t0) * 1000, f'协议ID异常 {pid}'
            need = 6 + length
            while len(buf) < need:
                chunk = self.sock.recv(need - len(buf))
                if not chunk:
                    return None, (time.perf_counter() - t0) * 1000, '应答半途断开'
                buf += chunk
            elapsed = (time.perf_counter() - t0) * 1000
            if tid != self.tid:
                print(f'  ⚠ 应答事务号 {tid} != 请求 {self.tid}（服务器实现不规范，帧照常解）')
            if unit_r != unit:
                print(f'  ⚠ 应答 unit {unit_r} != 请求 {unit}')
            return buf[7:need], elapsed, None
        except socket.timeout:
            return None, (time.perf_counter() - t0) * 1000, '超时无应答'
        except OSError as e:
            self.close()   # 连接已坏，下次 transact 自动重连
            return None, (time.perf_counter() - t0) * 1000, f'接收错误: {e}'


def classify(payload: bytes):
    """把应答 PDU 归类：('data'|'exc01'|'exc_gw'|'exc'|'other', 附加信息)"""
    fc = payload[0]
    if fc & 0x80:
        code = payload[1] if len(payload) > 1 else -1
        if code in (0x0A, 0x0B):
            return ('exc_gw', code)
        if code == 0x01:
            return ('exc01', None)
        return ('exc', code)
    if fc in (1, 2, 3, 4, 0x2E):
        return ('data', None)
    return ('other', None)


def fmt_resp(payload: bytes) -> str:
    fc = payload[0]
    if fc & 0x80:
        code = payload[1] if len(payload) > 1 else -1
        return f'异常码 0x{code:02X}（{EXC.get(code, "厂商自定义")}）'
    if fc in (3, 4):
        n = payload[1] // 2
        regs = [int.from_bytes(payload[2 + 2 * i:4 + 2 * i], 'big') for i in range(n)]
        return f'{n} 个寄存器 → ' + ', '.join(f'{v}(0x{v:04X})' for v in regs)
    if fc in (1, 2):
        n = payload[1]
        bits = [(payload[2 + i // 8] >> (i % 8)) & 1 for i in range(n * 8)]
        return f'{len(bits)} 个位 → {bits[:32]}'
    if fc == 0x2E:
        return fmt_ident(payload)
    return '原始 ' + hx(payload)


def fmt_ident(payload: bytes) -> str:
    """解 FC43/14 设备标识对象。"""
    if len(payload) < 6:
        return '标识应答过短 ' + hx(payload)
    conformity, more, nxt, nobj = payload[2], payload[3], payload[4], payload[5]
    names = {0: '厂商', 1: '产品代码', 2: '版本', 3: '厂商URL', 4: '产品名',
             5: '型号名', 6: '固件', 7: '序列号(对象6)', 0x80: '自定义设备ID'}
    out = [f'设备标识（一致性等级 {conformity:02X}，{nobj} 个对象）:']
    i = 6
    while i + 2 <= len(payload) and len(out) < nobj + 2:
        oid, olen = payload[i], payload[i + 1]
        i += 2
        val = payload[i:i + olen]
        i += olen
        out.append(f'  {names.get(oid, f"对象0x{oid:02X}")}: {val.decode("ascii", errors="replace")!r}')
    return '\n'.join(out)


def do_scan(args, c: Client):
    units = list(range(args.lo, args.hi + 1))
    print(f'-- 扫从机 ID {args.lo}..{args.hi}（地址 {args.addr} 数量 {args.qty}，先 FC03、超时/不支持再 FC04）')
    results = {}
    for u in units:
        got = None            # (kind, fc, info, ms)
        tried = []
        for fc in (3, 4):
            pdu = read_pdu(fc, args.addr, args.qty)
            resp, ms, err = c.transact(u, pdu)
            tried.append(f'FC{fc:02X}{"超时" if err else f"{ms:.0f}ms"}')
            if err:
                continue
            kind, info = classify(resp)
            got = (kind, fc, info, resp, ms)
            if kind in ('data', 'exc', 'exc_gw'):
                break          # 有数据 / 明确异常码，不再换功能码
            # exc01（功能码不支持）→ 换 FC04 再试
        if got is None:
            print(f'unit {u:>3}: ✗ 静默无应答（{"，".join(tried)}）')
            results[u] = ('silent', None)
        else:
            kind, fc, info, resp, ms = got
            tag = {'data': '✅ 正常应答', 'exc': '✅ 在线（返回异常）', 'exc01': '✅ 在线（两功能码均不支持）',
                   'exc_gw': '✗ 网关后无此从机', 'other': '? 有应答'}[kind]
            print(f'unit {u:>3}: {tag} FC{fc:02X} {ms:.0f}ms → {fmt_resp(resp)}')
            results[u] = (kind, resp)
        time.sleep(0.05)

    # 汇总
    data_u = [u for u, (k, _) in results.items() if k == 'data']
    exc_u = [u for u, (k, _) in results.items() if k in ('exc', 'exc01')]
    gw_u = [u for u, (k, _) in results.items() if k == 'exc_gw']
    silent_u = [u for u, (k, _) in results.items() if k == 'silent']
    print('\n' + '=' * 60)
    print(f'扫描范围 unit {args.lo}..{args.hi}，共 {len(units)} 个')
    print(f'① 正常读出数据的 unit：{data_u if data_u else "无"}')
    print(f'② 在线但拒读/不支持的 unit：{exc_u if exc_u else "无"}')
    print(f'③ 网关报告无此从机(0x0B/0x0A)的 unit：{len(gw_u)} 个' + (f'：{gw_u}' if len(gw_u) <= 20 else ''))
    print(f'④ 完全静默的 unit：{len(silent_u)} 个' + (f'：{silent_u}' if len(silent_u) <= 20 else ''))

    # 多个 unit 应答字节完全一致 → 服务器忽略 unit id
    from collections import defaultdict
    groups = defaultdict(list)
    for u, (k, resp) in results.items():
        if k in ('data', 'exc', 'exc01') and resp:
            groups[bytes(resp)].append(u)
    for resp, us in groups.items():
        if len(us) >= 3:
            print(f'⚠ 注意：unit {us} 应答字节完全一致 → 该服务器很可能忽略 unit id（无法用 unit 区分从机）')
    if data_u:
        print(f'下一步：python {sys.argv[0]} {args.host} -p {args.port} --unit {data_u[0]}   # 细看该从机')


def do_detail(args, c: Client):
    u = args.unit
    if args.ident:
        resp, ms, err = c.transact(u, ident_pdu(1 if args.conformity < 0 else 1))
        if err:
            # basic 不行再试 regular
            resp, ms, err = c.transact(u, ident_pdu(2))
        if err:
            print(f'unit {u}: 设备标识读取失败（{err}）——多数设备不支持 FC43，不代表不在线')
        else:
            kind, _ = classify(resp)
            if kind == 'exc':
                print(f'unit {u}: 设备标识返回异常 → {fmt_resp(resp)}（不支持 FC43，不代表不在线）')
            else:
                print(f'unit {u}: {ms:.0f}ms')
                print(fmt_resp(resp))
        return

    if args.fc:
        for i in range(2):    # 两发两收，稳定为准
            resp, ms, err = c.transact(u, read_pdu(args.fc, args.addr, args.qty))
            if err:
                print(f'unit {u} FC{args.fc:02X} 读 {args.addr} 起 {args.qty} 个 → {err}')
                return
            print(f'unit {u} FC{args.fc:02X} 读 {args.addr} 起 {args.qty} 个（{ms:.0f}ms）→ {fmt_resp(resp)}')
        return

    # 自动细看：FC03 / FC04 各读 0..15，再读设备标识
    for fc in (3, 4):
        resp, ms, err = c.transact(u, read_pdu(fc, 0, 16))
        if err:
            print(f'unit {u} FC{fc:02X} 读 0..15 → {err}')
        else:
            print(f'unit {u} FC{fc:02X} 读 0..15（{ms:.0f}ms）→ {fmt_resp(resp)}')
    resp, ms, err = c.transact(u, ident_pdu(1))
    if err or (resp and resp[0] & 0x80):
        print(f'unit {u} 设备标识（FC43/14）：不支持或无应答（不影响在线判断）')
    else:
        print(f'unit {u} 设备标识（FC43/14，{ms:.0f}ms）:')
        print(fmt_resp(resp))


def main():
    ap = argparse.ArgumentParser(description='Modbus TCP 从机 ID 探测（独立脚本）', epilog=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('host', help='设备 IP，如 192.168.0.80')
    ap.add_argument('-p', '--port', type=int, default=502)
    ap.add_argument('-t', '--timeout', type=int, default=800, help='每帧超时毫秒（默认 800）')
    ap.add_argument('--lo', type=int, default=1, help='扫描起始 unit（默认 1）')
    ap.add_argument('--hi', type=int, default=100, help='扫描结束 unit（默认 100）')
    ap.add_argument('--unit', type=int, help='不扫，细看指定从机')
    ap.add_argument('--fc', type=int, choices=[1, 2, 3, 4], help='细看模式的功能码')
    ap.add_argument('--addr', type=int, default=0, help='起始地址（默认 0）')
    ap.add_argument('--qty', type=int, default=4, help='读数量（默认 4；细看默认 16）')
    ap.add_argument('--ident', action='store_true', help='细看模式只读设备标识')
    ap.add_argument('--rtu', action='store_true', help='透传 DTU：发裸 RTU 帧（带 CRC）而非 MBAP')
    ap.add_argument('--conformity', type=int, default=-1, help='标识一致性等级 1/2/3（默认自动）')
    args = ap.parse_args()

    if args.unit is None and args.qty == 4:
        pass  # 扫描模式默认 qty=4 合理
    if args.unit is not None and not args.fc and not args.ident and args.addr == 0 and args.qty == 4:
        args.qty = 16  # 细看模式默认读 16 个

    print(f'目标 {args.host}:{args.port}  每帧超时 {args.timeout}ms  帧格式：{"裸 RTU" if args.rtu else "MBAP(Modbus TCP)"}')
    c = Client(args.host, args.port, args.timeout, rtu=args.rtu)
    try:
        c.connect()
    except OSError as e:
        print(f'✗ TCP 连接失败：{e}')
        print('  排查：设备供电/网线、本机与设备是否同网段（ipconfig）、防火墙、502 端口是否开放')
        sys.exit(1)
    print('✓ TCP 连接已建立（Modbus TCP 服务在监听）\n')
    try:
        if args.unit is not None:
            do_detail(args, c)
        else:
            do_scan(args, c)
    except KeyboardInterrupt:
        print('\n已中断')
    finally:
        c.close()


if __name__ == '__main__':
    main()
