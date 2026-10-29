#!/usr/bin/env python3
"""把《充电桩 Modbus 测点》（厂家源表：读 4001~4029 / 写 6001）写成两台枪设备
写进 NetLink 当前工程（演示工程 · 一号站）的 COM7 serial_master 连接（id 100000）。

工程量换算走 NetLink 点位的 scale/offset 字段：工程值 = 原始值 × scale + offset
（与源表「实际值 = 原始值 × 精度 + 偏移量」一致，偏移量为负即零点负偏移）。

2026-10-16 联调实测（A枪=从机3 / B枪=从机4，9600 8N1，待机未插枪，三轮采集）：
  4003=4000 → 0A，证实 -400 偏移正确；4024 恒 0、4025 恒 180/97 →「当前充电电量」
  是标准大端 U32（NetLink 高字在前解码正确，高位恒 0 只因值 <65536）；
  4027 恒 7180/6146（表底 71.8/61.46kWh）而 4028 为脏值（A枪 20225/B枪 34048）
  ——「表底值」厂家未按 U32 实现，按 u16 读 4027 避开脏字。
  桩完整响应帧 <500ms 返回（首读窗口实测），timeout/poll 设 2000ms 留足余量。

用法：
  python gen_pile_points.py          # A枪=3 B枪=4
  python gen_pile_points.py 5 6      # 指定两枪从机地址
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
SLAVE_A = int(sys.argv[1]) if len(sys.argv) > 1 else 3
SLAVE_B = int(sys.argv[2]) if len(sys.argv) > 2 else 4
# (设备id, 设备名, 从机地址)
GUNS = [(200001, '充电桩（枪A）', SLAVE_A), (200002, '充电桩（枪B）', SLAVE_B)]

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
    (4024, '当前充电电量', 'u32', 0.01, 0, 'kWh（U32占4024-4025标准大端，实测高位恒0、低字=180/97即1.8/0.97kWh）'),
    (4026, '双枪充状态', 'u16', 1, 0, '0=单枪充；1=双枪充'),
    (4027, '充电电量表底值', 'u32', 0.01, 0, 'kWh（按协议走：U32占4027-4028标准大端。实测4028为脏值20225/34048且4027单字71.8/61.46kWh才合理——固件与源表不符，协议解码下读数会到百万kWh级）'),
    (4029, '枪状态', 'u16', 1, 0, '0=未插枪；1=插枪'),
    # ---- 2026-10-16 用户新增（源表追加段，桩可读区上界实测为 4035）----
    (4030, '限值充电功率', 'u16', 0.1, 0, 'kW'),
    (4031, '充电输出电流', 'u16', 0.1, 0, 'A（同名点4003带-400偏移，新点表若同规格需加offset=-400）'),
    (4032, 'BMS需求电流', 'u16', 0.1, 0, 'A（同名点4012带-400偏移，新点表若同规格需加offset=-400）'),
    (4033, 'BMS充电电流测量值', 'u16', 0.1, 0, 'A（同名点4015带-400偏移，新点表若同规格需加offset=-400）'),
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
    for dev_id, dev_name, slave in GUNS:
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
                'poll': False,   # 只写寄存器：实测 FC03 回读 100% 静默，不进轮询读段；手动写不受影响
                'source': {'kind': 'static', 'value': 0.0},
                'note': note or None,
            })
            next_id += 1

        # 同连接下已有设备：整体替换同 id 设备（重复运行幂等），没有则追加
        dev = next((d for d in conn['devices'] if d['id'] == dev_id), None)
        if dev is None:
            conn['devices'].append(dev := {})
        dev.update({
            'id': dev_id,
            'name': dev_name,
            'slave_id': slave,
            'enabled': True,
            'fault': {'kind': 'normal'},
            'poll_period_ms': 2000,   # 实测桩完整响应帧<500ms，留足余量
            'points': points,
        })
        print(f'已写入设备「{dev_name}」从机 {slave}：读点 {len(POINTS)} + 写点 {len(WRITE_POINTS)}')

    # 连接超时 2000ms（旧值 1000ms 偏紧）
    if conn['transport'].get('timeout_ms', 0) < 2000:
        conn['transport']['timeout_ms'] = 2000
        print('连接超时已放宽：timeout_ms=2000')

    PROJECT.write_text(json.dumps(spec, ensure_ascii=False, indent=2), encoding='utf-8')

    n = len(GUNS) * (len(POINTS) + len(WRITE_POINTS))
    print(f'合计 {n} 点（含换算 scale/offset）')
    print(f'备份: {backup.name}')


if __name__ == '__main__':
    main()
