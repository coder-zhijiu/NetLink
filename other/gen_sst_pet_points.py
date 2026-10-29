#!/usr/bin/env python3
"""把《SST网关采点配置》§三——SST 本体点位（v2 2026-10-27 三端点版）写进
NetLink 当前工程（演示工程 · 一号站），新增两个独立 TCP 主机连接：

  SST 1# = 192.168.2.10:8002（transformerSST1 中压直挂变压器1#）
  SST 2# = 192.168.2.11:8002（transformerSST2 中压直挂变压器2#）

- 连接 id 100003 / 100004（tcp_client，可重复运行整体替换，幂等）
- 两台各 1 个设备（id 220001 / 220002），同一套单台模板，各采各端点分别上报
- 遥测 26 属性点（文档 §三上行表：0x100~0x111 / 0x11C~0x11F / 0x121~123 / 0x131）
  + 3 个故障字（0x132~0x134，平台侧走 fault 通道不建属性，此处联调观测原始位域）
  + 3 个遥控写点（0x1002/0x1004/0x1005，文档不接入下行，联调手动操作备用，
    poll=false 不进轮询）
- 工程值 = 原始值 × scale + offset；点表 16bit 有符号 → 模拟量 i16（无功反向等
  负值原样透传），位域（旁路/状态字/故障字）u16
- 遥测默认 FC04 输入寄存器；联调首轮读 0x100 无有效值（Uab 原始应 ≈10000）则
  加 --fc03 回退保持寄存器
- 轮询分帧：空洞（0x112、0x113~118 模块组、0x119~120、0x124~130、0x135~13E）
  把读点切成 4 段（0x100~111×18 / 0x11C~11F×4 / 0x121~123×3 / 0x131~134×4）
- 不采集：cumulativeInputEnergy（平台聚合）/ cumulativeOutputEnergy（无源空置）——
  两属性均无寄存器，不建点

用法：
  python gen_sst_pet_points.py                            # 默认 .10/.11:8002 从机各 1
  python gen_sst_pet_points.py 192.168.2.10 192.168.2.11  # 指定两台主机
  python gen_sst_pet_points.py <host1> <host2> <unit1> <unit2>
  python gen_sst_pet_points.py --fc03                     # 回退 FC03 保持寄存器
改前自动备份（后缀非 .json 不进工程列表）。
"""
import json
import shutil
import sys
import time
from pathlib import Path

PROJECTS = Path(r'E:\workspace-h5\NetLink\target\debug\projects')
PROJECT = PROJECTS / '演示工程 · 一号站.json'
FC03 = '--fc03' in sys.argv
args = [a for a in sys.argv[1:] if a != '--fc03']
HOST1 = args[0] if len(args) > 0 else '192.168.2.10'
HOST2 = args[1] if len(args) > 1 else '192.168.2.11'
UNIT1 = int(args[2]) if len(args) > 2 else 1   # 从站号/单元标识符按端点联调核对
UNIT2 = int(args[3]) if len(args) > 3 else 1
READ_AREA = 'holding_register' if FC03 else 'input_register'

# (连接id, 连接名, 设备id, 设备名, device_key, host, unit)
UNITS = [
    (100003, 'SST本体1#变压器·TCP采集', 220001, '中压直挂变压器1#', 'transformerSST1', HOST1, UNIT1),
    (100004, 'SST本体2#变压器·TCP采集', 220002, '中压直挂变压器2#', 'transformerSST2', HOST2, UNIT2),
]
PORT = 8002

# 单台模板——遥测（§三上行表，地址排序）；模拟量 i16 / 位域 u16
# (地址, 名称, 类型, scale, offset, 备注)
PET_POINTS = [
    (0x100, '高压侧Uab线电压', 'i16', 0.001, 0, 'kV（联校验：Uab 原始应 ≈10000 = 10kV×0.001）'),
    (0x101, '高压侧Ubc线电压', 'i16', 0.001, 0, 'kV'),
    (0x102, '高压侧Uca线电压', 'i16', 0.001, 0, 'kV'),
    (0x103, '高压侧A相电流', 'i16', 0.01, 0, 'A'),
    (0x104, '高压侧B相电流', 'i16', 0.01, 0, 'A'),
    (0x105, '高压侧C相电流', 'i16', 0.01, 0, 'A'),
    (0x106, '高压侧视在功率', 'i16', 0.1, 0, 'kVA'),
    (0x107, '高压侧有功功率', 'i16', 0.1, 0, 'kW'),
    (0x108, '高压侧无功功率', 'i16', 0.1, 0, 'kVar（有符号，无功反向为负原样透传）'),
    (0x109, '高压侧功率因数', 'i16', 0.01, 0, '%'),
    (0x10A, '高压侧频率', 'i16', 0.01, 0, 'Hz'),
    (0x10B, '电压（DC750V端口）', 'i16', 0.1, 0, 'V'),
    (0x10C, '电流（DC750V端口）', 'i16', 0.1, 0, 'A'),
    (0x10D, '实时功率（DC750V端口）', 'i16', 0.1, 0, 'kW（有符号，方向按符号联调核一次）'),
    (0x10E, '低压接口柜湿度', 'i16', 0.1, 0, '%'),
    (0x10F, '低压接口柜温度', 'i16', 0.1, 0, '℃'),
    (0x110, '高压接口柜湿度', 'i16', 0.1, 0, '%'),
    (0x111, '高压接口柜温度', 'i16', 0.1, 0, '℃'),
    (0x11C, '功率柜1湿度', 'i16', 0.1, 0, '%'),
    (0x11D, '功率柜1温度', 'i16', 0.1, 0, '℃'),
    (0x11E, '功率柜2湿度', 'i16', 0.1, 0, '%'),
    (0x11F, '功率柜2温度', 'i16', 0.1, 0, '℃'),
    (0x121, 'A相模块旁路状态', 'u16', 1, 0, '值义联调确认（位域）'),
    (0x122, 'B相模块旁路状态', 'u16', 1, 0, '值义联调确认（位域）'),
    (0x123, 'C相模块旁路状态', 'u16', 1, 0, '值义联调确认（位域）'),
    (0x131, '系统状态字', 'u16', 1, 0, 'bit0停机 bit1运行 bit2故障 bit3待机 bit4VDC/Q bit5P/Q bit6模块旁路'),
    (0x132, '系统故障字1', 'u16', 1, 0, '平台走fault通道（码=N×0x100+M），此处联调观测原始位域'),
    (0x133, '系统故障字2', 'u16', 1, 0, '同上，bit0 AD采样故障等（见SST通讯点表）'),
    (0x134, '系统故障字3', 'u16', 1, 0, '同上，bit0 10kV断路器合闸故障等（见SST通讯点表）'),
]

# 遥控写点（FC06 保持寄存器）——文档 §一：不接入下行，联调期手动操作备用
PET_WRITE = [
    (0x1002, '遥控-PET启动', '写 65280(0xFF00)=启动 / 0=停止'),
    (0x1004, '遥控-PET停止', '写 65280(0xFF00)=停止触发'),
    (0x1005, '遥控-故障复位', '写 65280(0xFF00)=复位触发'),
]


def pet_points(dev_id):
    """一台 SST 的 29 读点 + 3 写点；点 id 从 620000 起按台铺开"""
    start = 620000 + (dev_id - 220001) * 100
    points = []
    for i, (addr, name, dt, scale, offset, note) in enumerate(PET_POINTS):
        points.append({
            'id': start + i,
            'name': name,
            'area': READ_AREA,
            'address': addr,
            'data_type': dt,
            'scale': scale,
            'offset': offset,
            'writable': False,   # 纯采集遥测：FC03 回退后住在保持寄存器（区可写），显式只读防误写设备
            'source': {'kind': 'static', 'value': 0.0},
            'note': note or None,
        })
    for j, (addr, name, note) in enumerate(PET_WRITE):
        points.append({
            'id': start + len(PET_POINTS) + j,
            'name': name,
            'area': 'holding_register',
            'address': addr,
            'data_type': 'u16',
            'scale': 1,
            'offset': 0,
            'poll': False,   # 遥控区不进轮询读段；联调手动写不受影响
            'source': {'kind': 'static', 'value': 0.0},
            'note': note,
        })
    return points


def main():
    spec = json.loads(PROJECT.read_text(encoding='utf-8'))

    backup = PROJECT.with_suffix('.json.bak-sstpet-' + time.strftime('%Y%m%d'))
    if PROJECT.exists():
        shutil.copy2(PROJECT, backup)

    for conn_id, conn_name, dev_id, dev_name, key, host, unit in UNITS:
        conn = {
            'id': conn_id,
            'name': conn_name,
            'transport': {
                'kind': 'tcp_client',
                'host': host,
                'port': PORT,
                'timeout_ms': 2000,
            },
            'can_nodes': [],
            'devices': [{
                'id': dev_id,
                'name': dev_name,
                'slave_id': unit,
                'enabled': True,
                'fault': {'kind': 'normal'},
                'poll_period_ms': 1000,
                'points': pet_points(dev_id),
            }],
        }
        old = next((c for c in spec['connections'] if c['id'] == conn_id), None)
        if old is None:
            spec['connections'].append(conn)
            print(f'新增连接 id={conn_id}「{conn_name}」→ {host}:{PORT}（{key}，从机 {unit}）')
        else:
            old.update(conn)
            print(f'整体替换连接 id={conn_id}「{conn_name}」→ {host}:{PORT}（{key}，从机 {unit}）')

    PROJECT.write_text(json.dumps(spec, ensure_ascii=False, indent=2), encoding='utf-8')

    fc = 'FC03 保持寄存器（回退）' if FC03 else 'FC04 输入寄存器（默认）'
    print(f'遥测读区：{fc}；每台 {len(PET_POINTS)} 读点 + {len(PET_WRITE)} 写点，'
          f'轮询 1000ms，超时 2000ms')
    print('读段（两台同布局）：0x0100×18 + 0x011C×4 + 0x0121×3 + 0x0131×4 → 每台 4 帧/轮')
    print(f'备份: {backup.name}')


if __name__ == '__main__':
    main()
