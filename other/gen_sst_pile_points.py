#!/usr/bin/env python3
"""把《SST网关采点配置》§四——SST 桩充电枪点位（v2 2026-10-27 三端点版）写进
NetLink 当前工程（演示工程 · 一号站），新增独立 TCP 主机连接：

  充电桩端点 192.168.2.188:502（单从机，从机 ID 待现场确认，默认 1）

- 新连接 id 100002（tcp_client，可重复运行整体替换，幂等）
- 四桩八枪 = 8 个设备（id 210001~210008，名称对齐文档 §二 device_key 表），
  同一从机 ID，靠枪遥测块基址区分：0x0FA1 + n×0x100（万能充 B 至 0x16A1）
- 每枪 22 个遥测点（FC04，块内偏移 0x01~0x1D，不采集 0x00/0x08~09/0x12~14 空洞）
  + 1 个枪级限值写点（FC06，0x1771+n，poll=false 不进轮询）
- 工程值 = 原始值 × scale + offset：电流三点（0x02/0x0B/0x0E）带 -400A 零点偏移
  （独立桩 4031~33 零偏移口径不适用 SST）；温度 -50；U32 双寄存器标准大端
  （NetLink 高字在前解码，字序联调确认）
- 回读对账：写 0x1771+n（raw = 值×10）后由枪块内 0x1D 回读点 1:1 校验，
  回读与写是两个点位，同设备下并排
- 轮询分帧：块内空洞把 0x01~0x1D 切成 3 段连续读（0x01~07/0x0A~11/0x15~1D），
  8 枪共 24 帧/轮；poll_period_ms=3000，timeout 2000ms

用法：
  python gen_sst_pile_points.py                      # 192.168.2.188:502 从机 1
  python gen_sst_pile_points.py 192.168.2.188 502 5  # 现场确认后指定从机 ID
改前自动备份（后缀非 .json 不进工程列表）。
"""
import json
import shutil
import sys
import time
from pathlib import Path

PROJECTS = Path(r'E:\workspace-h5\NetLink\target\debug\projects')
PROJECT = PROJECTS / '演示工程 · 一号站.json'
CONN_ID = 100002
CONN_NAME = 'SST充电桩四桩八枪·TCP采集'
HOST = sys.argv[1] if len(sys.argv) > 1 else '192.168.2.188'
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 502
UNIT = int(sys.argv[3]) if len(sys.argv) > 3 else 1   # 文档：单从机，ID 待现场确认

# (设备id, 名称[文档§二], device_key, 枪块基址, 限值写点)
GUNS = [
    (210001, 'SST充电机1枪A', 'SSTCharger1Plug1', 0x0FA1, 0x1771),
    (210002, 'SST充电机1枪B', 'SSTCharger1Plug2', 0x10A1, 0x1772),
    (210003, 'SST充电机2枪A', 'SSTCharger2Plug1', 0x11A1, 0x1773),
    (210004, 'SST充电机2枪B', 'SSTCharger2Plug2', 0x12A1, 0x1774),
    (210005, 'SST充电机3枪A', 'SSTCharger3Plug1', 0x13A1, 0x1775),
    (210006, 'SST充电机3枪B', 'SSTCharger3Plug2', 0x14A1, 0x1776),
    (210007, '万能充枪A', 'SSTCharger4Plug1', 0x15A1, 0x1777),
    (210008, '万能充枪B', 'SSTCharger4Plug2', 0x16A1, 0x1778),
]

# 枪遥测模板（块内偏移，FC04 输入寄存器）——依据 SST网关采点配置.md §四上行表
# (块内偏移, 名称, 类型, scale, offset, 备注)
GUN_TEMPLATE = [
    (0x01, '充电输出电压', 'u16', 0.1, 0, 'V'),
    (0x02, '充电输出电流', 'u16', 0.1, -400, 'A（SST块内-400A零点偏移；独立桩4031零偏移口径不适用SST）'),
    (0x03, 'SOC', 'u16', 1, 0, '%'),
    (0x04, '电池组最低温度', 'u16', 1, -50, '℃'),
    (0x05, '电池组最高温度', 'u16', 1, -50, '℃'),
    (0x06, '单体电池最高电压', 'u16', 0.01, 0, 'V'),
    (0x07, '单体电池最低电压', 'u16', 0.01, 0, 'V'),
    (0x0A, 'BMS需求电压', 'u16', 0.1, 0, 'V'),
    (0x0B, 'BMS需求电流', 'u16', 0.1, -400, 'A（-400A零点偏移）'),
    (0x0C, '充电模式', 'u16', 1, 0, '1=恒压充电；2=恒流充电'),
    (0x0D, 'BMS充电电压测量值', 'u16', 0.1, 0, 'V'),
    (0x0E, 'BMS充电电流测量值', 'u16', 0.1, -400, 'A（-400A零点偏移）'),
    (0x0F, '估算剩余充电时间', 'u16', 1, 0, 'min'),
    (0x10, '充电接口温度探头1温度', 'u16', 1, -50, '℃（充电接口1 DC+）'),
    (0x11, '充电接口温度探头2温度', 'u16', 1, -50, '℃（充电接口1 DC-）'),
    (0x15, '充电状态', 'u16', 1, 0, '1=待机；2=工作；3=自检；4=故障（纯遥测，厂家无启停写点）'),
    (0x16, '充电功率', 'u16', 0.1, 0, 'kW'),
    (0x17, '当前充电电量', 'u32', 0.01, 0, 'kWh（U32占2寄存器标准大端，字序联调确认）'),
    (0x19, '双枪充状态', 'u16', 1, 0, '0=单枪充；1=双枪充'),
    (0x1A, '充电电量表底值', 'u32', 0.01, 0, 'kWh（U32占2寄存器标准大端，单调表计按差值算电量；字序联调确认）'),
    (0x1C, '枪状态', 'u16', 1, 0, '0=未插枪；1=插枪'),
    (0x1D, '限制充电功率（回读）', 'u16', 0.1, 0, 'kW（写点0x1771+n的回读对账点，raw应与写值1:1）'),
]

# 每枪限值写点（FC06 保持寄存器）：raw = 工程值 × 10（0.1kW 精度）
WRITE_SCALE_NOTE = 'kW，写工程值自动×10下发（FC06）；回读对账走枪块内0x1D回读点'


def gun_points(dev_id, base, write_addr):
    """一把枪的 22 读点 + 1 写点；点 id 从 610000 起按设备序号铺开"""
    start = 610000 + (dev_id - 210001) * 100
    points = []
    for i, (off, name, dt, scale, offset, note) in enumerate(GUN_TEMPLATE):
        points.append({
            'id': start + i,
            'name': name,
            'area': 'input_register',
            'address': base + off,
            'data_type': dt,
            'scale': scale,
            'offset': offset,
            'source': {'kind': 'static', 'value': 0.0},
            'note': note or None,
        })
    points.append({
        'id': start + len(GUN_TEMPLATE),
        'name': '限制充电功率（写）',
        'area': 'holding_register',
        'address': write_addr,
        'data_type': 'u16',
        'scale': 0.1,
        'offset': 0,
        'poll': False,   # 只写寄存器不进轮询读段；回读对账走枪块内 0x1D
        'source': {'kind': 'static', 'value': 0.0},
        'note': WRITE_SCALE_NOTE,
    })
    return points


def main():
    spec = json.loads(PROJECT.read_text(encoding='utf-8'))

    backup = PROJECT.with_suffix('.json.bak-sstpile-' + time.strftime('%Y%m%d'))
    if PROJECT.exists():
        shutil.copy2(PROJECT, backup)

    devices = [
        {
            'id': dev_id,
            'name': name,
            'slave_id': UNIT,
            'enabled': True,
            'fault': {'kind': 'normal'},
            'poll_period_ms': 3000,
            'points': gun_points(dev_id, base, write_addr),
        }
        for dev_id, name, key, base, write_addr in GUNS
    ]
    conn = {
        'id': CONN_ID,
        'name': CONN_NAME,
        'transport': {
            'kind': 'tcp_client',
            'host': HOST,
            'port': PORT,
            'timeout_ms': 2000,
        },
        'can_nodes': [],
        'devices': devices,
    }

    old = next((c for c in spec['connections'] if c['id'] == CONN_ID), None)
    if old is None:
        spec['connections'].append(conn)
        print(f'新增连接 id={CONN_ID}「{CONN_NAME}」→ {HOST}:{PORT}')
    else:
        old.update(conn)
        print(f'整体替换连接 id={CONN_ID}「{CONN_NAME}」→ {HOST}:{PORT}')

    PROJECT.write_text(json.dumps(spec, ensure_ascii=False, indent=2), encoding='utf-8')

    print(f'从机 ID = {UNIT}（文档待现场确认；确认后重跑：python gen_sst_pile_points.py {HOST} {PORT} <ID>）')
    for dev_id, name, key, base, write_addr in GUNS:
        seg = [(base + 0x01, 7), (base + 0x0A, 8), (base + 0x15, 9)]  # 0x15~0x1D 含 2 个 U32
        seg_s = ' + '.join(f'0x{s:04X}×{n}' for s, n in seg)
        print(f'  {name}（{key}）：块 0x{base:04X}，读段 {seg_s}，写 0x{write_addr:04X}')
    n = len(GUNS) * (len(GUN_TEMPLATE) + 1)
    print(f'合计 {n} 点（22 读 + 1 写 / 枪 × 8 枪），轮询 3000ms，超时 2000ms')
    print(f'备份: {backup.name}')


if __name__ == '__main__':
    main()
