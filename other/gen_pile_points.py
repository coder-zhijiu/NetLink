#!/usr/bin/env python3
"""把《充电桩 Modbus 测点》（厂家单枪源表：读 4001~4029 / 写 6001）作为一台新设备
写进 NetLink 当前工程（演示工程 · 一号站）的 COM7 serial_master 连接（id 100000）。

工程量换算走 NetLink 点位的 scale/offset 字段：工程值 = 原始值 × scale + offset
（与源表「实际值 = 原始值 × 精度 + 偏移量」一致，偏移量为负即零点负偏移）。

用法：
  python gen_pile_points.py          # 从机地址默认 2（逆变器占 1；占位，可后改）
  python gen_pile_points.py 5        # 指定从机地址
改前自动备份（后缀非 .json 不进工程列表）。可重复运行：整体替换本设备点位。
"""
import json
import shutil
import sys
import time
from pathlib import Path

PROJECTS = Path(r'E:\workspace-h5\NetLink\target\debug\projects')
PROJECT = PROJECTS / '演示工程 · 一号站.json'
CONN_ID = 100000
DEVICE_ID = 200001
SLAVE = int(sys.argv[1]) if len(sys.argv) > 1 else 2

# (地址, 名称, 类型, scale, offset, 备注)  —— 地址为十进制 PDU 地址
POINTS = [
    (4001, '充电接口标识', 'u16', 1, 0, '枪A应=1，枪B应=2'),
    (4002, '充电输出电压', 'u16', 0.1, 0, 'V'),
    (4003, '充电输出电流', 'u16', 0.1, -400, 'A（零点负偏移-400）'),
    (4004, 'SOC', 'u16', 1, 0, '%'),
    (4005, '电池组最低温度', 'u16', 1, -50, '℃'),
    (4006, '电池组最高温度', 'u16', 1, -50, '℃'),
    (4007, '单体电池最高电压', 'u16', 0.01, 0, 'V'),
    (4008, '单体电池最低电压', 'u16', 0.01, 0, 'V'),
    (4009, '充电机环境温度', 'u16', 1, -50, '℃（源表标注暂未使用）'),
    (4010, '充电导引电压', 'u16', 0.01, 0, 'V（源表标注暂未使用）'),
    (4011, 'BMS需求电压', 'u16', 0.1, 0, 'V'),
    (4012, 'BMS需求电流', 'u16', 0.1, -400, 'A（零点负偏移-400）'),
    (4013, '充电模式', 'u16', 1, 0, '1=恒压充电；2=恒流充电'),
    (4014, 'BMS充电电压测量值', 'u16', 0.1, 0, 'V'),
    (4015, 'BMS充电电流测量值', 'u16', 0.1, -400, 'A（零点负偏移-400）'),
    (4016, '估算剩余充电时间', 'u16', 1, 0, 'min'),
    (4017, '接口温度探头1(DC+)', 'u16', 1, -50, '℃'),
    (4018, '接口温度探头2(DC-)', 'u16', 1, -50, '℃'),
    (4019, '接口温度探头3', 'u16', 1, -50, '℃（源表标注暂未使用）'),
    (4020, '接口温度探头4', 'u16', 1, -50, '℃（源表标注暂未使用）'),
    (4021, '产品型号', 'u16', 1, 0, ''),
    (4022, '充电状态', 'u16', 1, 0, '1=待机；2=工作；3=自检；4=故障'),
    (4023, '充电功率', 'u16', 0.1, 0, 'kW'),
    (4024, '当前充电电量', 'u32', 0.01, 0, 'kWh（占4024-4025两字，字序联调确认）'),
    (4026, '双枪充状态', 'u16', 1, 0, '0=单枪充；1=双枪充'),
    (4027, '充电电量表底值', 'u32', 0.01, 0, 'kWh（占4027-4028两字，字序联调确认）'),
    (4029, '枪状态', 'u16', 1, 0, '0=未插枪；1=插枪'),
]

# 写保持寄存器（FC06）
WRITE_POINTS = [
    (6001, '限制充电功率', 'u16', 0.1, 0, 'kW，写工程值自动按 ×10 下发'),
]


def main():
    spec = json.loads(PROJECT.read_text(encoding='utf-8'))
    conn = next(c for c in spec['connections'] if c['id'] == CONN_ID)

    backup = PROJECT.with_suffix('.json.bak-pile-' + time.strftime('%Y%m%d'))
    if PROJECT.exists():
        shutil.copy2(PROJECT, backup)

    next_id = 500000
    points = []
    for addr, name, dt, scale, offset, note in POINTS:
        points.append({
            'id': next_id,
            'name': name,
            'area': 'input_register',
            'address': addr,
            'data_type': dt,
            'scale': scale,
            'offset': offset,
            'source': {'kind': 'static', 'value': 0.0},
            'note': note or None,
        })
        next_id += 1
    for addr, name, dt, scale, offset, note in WRITE_POINTS:
        points.append({
            'id': next_id,
            'name': name,
            'area': 'holding_register',
            'address': addr,
            'data_type': dt,
            'scale': scale,
            'offset': offset,
            'source': {'kind': 'static', 'value': 0.0},
            'note': note or None,
        })
        next_id += 1

    # 同连接下已有设备：整体替换同 id 设备（重复运行幂等），没有则追加
    dev = next((d for d in conn['devices'] if d['id'] == DEVICE_ID), None)
    if dev is None:
        conn['devices'].append(dev := {})
    dev.update({
        'id': DEVICE_ID,
        'name': '充电桩（枪A）',
        'slave_id': SLAVE,
        'enabled': True,
        'fault': {'kind': 'normal'},
        'poll_period_ms': 1000,
        'points': points,
    })
    PROJECT.write_text(json.dumps(spec, ensure_ascii=False, indent=2), encoding='utf-8')

    scaled = sum(1 for p in points if p['scale'] != 1 or p['offset'] != 0)
    print(f'已写入设备「{dev["name"]}」从机 {SLAVE}：读点 {len(POINTS)} + 写点 {len(WRITE_POINTS)}')
    print(f'带换算（scale/offset ≠ 1/0）的点：{scaled} 个')
    print(f'备份: {backup.name}')
    if SLAVE == 2:
        print('⚠ 从机 2 为占位地址——桩接上后确认真实地址：')
        print('   python gen_pile_points.py <真实地址>   或在界面设备编辑器里改')


if __name__ == '__main__':
    main()
