#!/usr/bin/env python3
"""把《SST及充电桩通讯点表》写进 NetLink 当前工程（演示工程 · 一号站）的
COM7 serial_master 连接（连接 id 100000，设备 id 200000）。

改前自动备份为 .bak-<日期>（后缀非 .json，不会出现在工程列表里）。
可重复运行：每次都整体替换设备点位。
"""
import json
import shutil
import time
from pathlib import Path

PROJECTS = Path(r'E:\workspace-h5\NetLink\target\debug\projects')
PROJECT = PROJECTS / '演示工程 · 一号站.json'
CONN_ID = 100000
DEVICE_ID = 200000

# ---------- SST 遥测（输入寄存器 FC04；表格有符号 → i16） ----------
SST_R = [
    # (地址, 名称, 系数备注)
    (0x100, 'SST·AC10kV端口Uab电压', 'kV，显示=原始×0.001'),
    (0x101, 'SST·AC10kV端口Ubc电压', 'kV，显示=原始×0.001'),
    (0x102, 'SST·AC10kV端口Uca电压', 'kV，显示=原始×0.001'),
    (0x103, 'SST·AC10kV端口A相电流', 'A，显示=原始×0.01'),
    (0x104, 'SST·AC10kV端口B相电流', 'A，显示=原始×0.01'),
    (0x105, 'SST·AC10kV端口C相电流', 'A，显示=原始×0.01'),
    (0x106, 'SST·AC10kV端口视在功率', 'kVA，显示=原始×0.1'),
    (0x107, 'SST·AC10kV端口有功功率', 'kW，显示=原始×0.1'),
    (0x108, 'SST·AC10kV端口无功功率', 'kVar，显示=原始×0.1'),
    (0x109, 'SST·AC10kV端口功率因数', '%，显示=原始×0.01'),
    (0x10A, 'SST·AC10kV端口电压频率', 'Hz，显示=原始×0.01'),
    (0x10B, 'SST·DC750V端口电压', 'V，显示=原始×0.1'),
    (0x10C, 'SST·DC750V端口电流', 'A，显示=原始×0.1'),
    (0x10D, 'SST·DC750V端口功率', 'kW，显示=原始×0.1'),
    (0x10E, 'SST·低压接口柜内湿度', '%，显示=原始×0.1'),
    (0x10F, 'SST·低压接口柜内温度', '℃，显示=原始×0.1'),
    (0x110, 'SST·高压接口柜内湿度', '%，显示=原始×0.1'),
    (0x111, 'SST·高压接口柜内温度', '℃，显示=原始×0.1'),
    (0x113, 'SST·A相模块瞬时电压平均值', 'V，×1'),
    (0x114, 'SST·B相模块瞬时电压平均值', 'V，×1'),
    (0x115, 'SST·C相模块瞬时电压平均值', 'V，×1'),
    (0x116, 'SST·模块最大电压', 'V，显示=原始×0.1'),
    (0x117, 'SST·模块最小电压', 'V，显示=原始×0.1'),
    (0x118, 'SST·模块最大温度', '℃，显示=原始×0.1'),
    (0x11C, 'SST·功率柜1内湿度', '%，显示=原始×0.1'),
    (0x11D, 'SST·功率柜1内温度', '℃，显示=原始×0.1'),
    (0x11E, 'SST·功率柜2内湿度', '%，显示=原始×0.1'),
    (0x11F, 'SST·功率柜2内温度', '℃，显示=原始×0.1'),
    (0x121, 'SST·A相模块旁路状态', '1=有模块旁路，0=无'),
    (0x122, 'SST·B相模块旁路状态', '1=有模块旁路，0=无'),
    (0x123, 'SST·C相模块旁路状态', '1=有模块旁路，0=无'),
    (0x131, 'SST·系统状态字', 'bit0停机 bit1运行 bit2故障 bit3待机 bit4VDC/Q bit5P/Q bit6模块旁路'),
    (0x132, 'SST·系统故障字1', 'bit0网侧母线瞬时过流 … bit15模块总故障（见点表）'),
    (0x133, 'SST·系统故障字2', 'bit0 AD采样故障 bit2 EPO急停 …（见点表）'),
    (0x134, 'SST·系统故障字3', 'bit0 10kV断路器合闸故障 … bit15充电桩断路器分闸故障（见点表）'),
]

# ---------- SST 遥控命令（保持寄存器 FC06 写） ----------
SST_W = [
    (0x1002, 'SST·遥控-PET启动', '写 0xFF00=启动 / 0x0000=停止'),
    (0x1004, 'SST·遥控-PET停止', '写 0xFF00=启动 / 0x0000=停止'),
    (0x1005, 'SST·遥控-故障复位', '写 0xFF00=复位触发'),
]

# ---------- 充电桩：枪块基址（全站单一从机，块地址区分桩/枪） ----------
GUNS = [
    ('1#直挂A', 0x0FA1), ('1#直挂B', 0x10A1),
    ('2#直挂A', 0x11A1), ('2#直挂B', 0x12A1),
    ('3#直挂A', 0x13A1), ('3#直挂B', 0x14A1),
    ('万能充A', 0x15A1), ('万能充B', 0x16A1),
]

# 枪遥测模板（块内偏移，输入寄存器 FC04）
GUN_TEMPLATE = [
    (0x00, '充电接口标识', 'u16', '枪A块应=1，枪B块应=2'),
    (0x01, '充电输出电压', 'u16', 'V，显示=原始×0.1'),
    (0x02, '充电输出电流', 'u16', 'A，显示=(原始-400)×0.1'),
    (0x03, 'SOC', 'u16', '%'),
    (0x04, '电池组最低温度', 'u16', '℃，显示=原始-50'),
    (0x05, '电池组最高温度', 'u16', '℃，显示=原始-50'),
    (0x06, '单体电池最高电压', 'u16', 'V，显示=原始×0.01'),
    (0x07, '单体电池最低电压', 'u16', 'V，显示=原始×0.01'),
    (0x08, '充电机环境温度', 'u16', '℃，显示=原始-50（暂未使用）'),
    (0x09, '充电导引电压', 'u16', 'V，显示=原始×0.01（暂未使用）'),
    (0x0A, 'BMS需求电压', 'u16', 'V，显示=原始×0.1'),
    (0x0B, 'BMS需求电流', 'u16', 'A，显示=(原始-400)×0.1'),
    (0x0C, '充电模式', 'u16', '1=恒压 2=恒流'),
    (0x0D, 'BMS充电电压测量值', 'u16', 'V，显示=原始×0.1'),
    (0x0E, 'BMS充电电流测量值', 'u16', 'A，显示=(原始-400)×0.1'),
    (0x0F, '估算剩余充电时间', 'u16', 'min'),
    (0x10, '接口温度探头1(DC+)', 'u16', '℃，显示=原始-50'),
    (0x11, '接口温度探头2(DC-)', 'u16', '℃，显示=原始-50'),
    (0x12, '接口温度探头3', 'u16', '暂未使用'),
    (0x13, '接口温度探头4', 'u16', '暂未使用'),
    (0x14, '产品型号', 'u16', ''),
    (0x15, '充电状态', 'u16', '1=待机 2=工作 3=自检 4=故障'),
    (0x16, '充电功率', 'u16', 'kW，显示=原始×0.1'),
    (0x17, '当前充电电量', 'u32', 'kWh，显示=原始×0.01（占2寄存器，字序联调确认）'),
    (0x19, '双枪充状态', 'u16', '0=单枪充 1=双枪充'),
    (0x1A, '充电电量表底值', 'u32', 'kWh，显示=原始×0.01（占2寄存器，字序联调确认）'),
    (0x1C, '枪状态', 'u16', '0=未插枪 1=插枪'),
]

# 枪级/桩级功率限值（保持寄存器 FC06 写）
GUN_W = [
    (0x1771, '写·1#直挂A 限制充电功率', 'kW，写值=目标×10'),
    (0x1772, '写·1#直挂B 限制充电功率', 'kW，写值=目标×10'),
    (0x1773, '写·2#直挂A 限制充电功率', 'kW，写值=目标×10'),
    (0x1774, '写·2#直挂B 限制充电功率', 'kW，写值=目标×10'),
    (0x1775, '写·3#直挂A 限制充电功率', 'kW，写值=目标×10'),
    (0x1776, '写·3#直挂B 限制充电功率', 'kW，写值=目标×10'),
    (0x1777, '写·万能充A 限制充电功率', 'kW，写值=目标×10'),
    (0x1778, '写·万能充B 限制充电功率', 'kW，写值=目标×10'),
    (0x1781, '写·1#直挂 桩级限制功率', 'kW×0.1；我方新增约定点，源表无'),
    (0x1782, '写·2#直挂 桩级限制功率', 'kW×0.1；我方新增约定点，源表无'),
    (0x1783, '写·3#直挂 桩级限制功率', 'kW×0.1；我方新增约定点，源表无'),
    (0x1784, '写·万能充 桩级限制功率', 'kW×0.1；我方新增约定点，源表无'),
]


def main():
    spec = json.loads(PROJECT.read_text(encoding='utf-8'))
    conn = next(c for c in spec['connections'] if c['id'] == CONN_ID)
    dev = next(d for d in conn['devices'] if d['id'] == DEVICE_ID)

    backup = PROJECT.with_suffix('.json.bak-' + time.strftime('%Y%m%d'))
    shutil.copy2(PROJECT, backup)

    next_id = 300000
    points = []

    def add(name, area, addr, dtype, note):
        nonlocal next_id
        points.append({
            'id': next_id,
            'name': name,
            'area': area,
            'address': addr,
            'data_type': dtype,
            'source': {'kind': 'static', 'value': 0.0},
            'note': note or None,
        })
        next_id += 1

    for addr, name, note in SST_R:
        add(name, 'input_register', addr, 'i16', note)
    for addr, name, note in SST_W:
        add(name, 'holding_register', addr, 'u16', note)
    for gun, base in GUNS:
        for off, name, dt, note in GUN_TEMPLATE:
            add(f'{gun}·{name}', 'input_register', base + off, dt, note)
    for addr, name, note in GUN_W:
        add(name, 'holding_register', addr, 'u16', note)

    conn['name'] = 'SST联调·COM7主机轮询'
    dev['name'] = 'SST及充电桩（从机1）'
    dev['points'] = points

    PROJECT.write_text(json.dumps(spec, ensure_ascii=False, indent=2), encoding='utf-8')

    areas = {}
    for p in points:
        areas[p['area']] = areas.get(p['area'], 0) + 1
    print(f'已写入 {len(points)} 个点到「{conn["name"]}」/ 设备「{dev["name"]}」')
    print('分区统计:', areas)
    print(f'备份: {backup.name}')
    print('点位 id 范围: 300000 ~', next_id - 1)


if __name__ == '__main__':
    main()
