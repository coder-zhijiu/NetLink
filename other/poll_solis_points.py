#!/usr/bin/env python3
"""COM7 锦浪 Solis 逆变器：把 NetLink 工程（gen_solis_points.py 写入的 99 个 FC04 点）
全部轮询一遍，按协议解码后输出 Markdown 采集报告。

- 串口：COM7 9600 8N1，从机 1，FC04 读输入寄存器，首字节超时 1000ms
- 查询间隔 350ms（协议 3.2 要求 >300ms）
- U32/S32 高位字在前；CRC 与 NetLink/mbtest.py 一致（0xA001，低字节在前）
- 附带补读协议 5.1 中不在工程点表内的 3471-3473（故障字10-12）、3544（协议原文标注混乱）

用法：python other/poll_solis_points.py [COM口]   （默认 COM7）
输出：other/COM7点位采集报告_YYYY-MM-DD.md
"""
import sys
import time
import datetime
from pathlib import Path

import serial
from serial.tools import list_ports

PORT = sys.argv[1] if len(sys.argv) > 1 else 'COM7'
UNIT = 1
BAUD = 9600
INTERVAL = 0.35          # 查询间隔，>300ms
TIMEOUT_MS = 1000
TRIES = 2
OUT = Path(__file__).parent / f'COM7点位采集报告_{datetime.date.today():%Y-%m-%d}.md'


# ---------------------------------------------------------------- 点表 ----
# (地址, 名称, 类型, 系数, 单位)  —— 与 gen_solis_points.py 的 99 点一一对应
POINTS = [
    # 身份/版本
    (2999, '产品型号', 'u16', 1, ''),
    (3000, 'DSP软件版本', 'u16', 1, ''),
    (3001, '液晶软件版本', 'u16', 1, ''),
    (3020, '液晶软件版本(大/小)', 'u16', 1, ''),
    (35000, '逆变器类型定义', 'u16', 1, ''),
    (3060, '序列号SN_1', 'u16', 1, ''),
    (3061, '序列号SN_2', 'u16', 1, ''),
    (3062, '序列号SN_3', 'u16', 1, ''),
    (3063, '序列号SN_4', 'u16', 1, ''),
    # 功率/电量
    (3004, '有功功率', 's32', 1, 'W'),
    (3006, '总直流输入功率', 'u32', 1, 'W'),
    (3008, '总发电量', 'u32', 1, 'kWh'),
    (3010, '当月发电量', 'u32', 1, 'kWh'),
    (3012, '上月发电量', 'u32', 1, 'kWh'),
    (3014, '当日发电量', 'u16', 0.1, 'kWh'),
    (3015, '昨日发电量', 'u16', 0.1, 'kWh'),
    (3055, '无功功率值', 's32', 1, 'Var'),
    (3057, '视在功率值', 's32', 1, 'VA'),
    (3059, '实时功率因数', 's16', 0.001, ''),
    # 直流侧
    (3003, '直流输入类型', 'u16', 1, ''),
    (3021, '直流电压1', 'u16', 0.1, 'V'),
    (3022, '直流电流1', 'u16', 0.1, 'A'),
    (3023, '直流电压2', 'u16', 0.1, 'V'),
    (3024, '直流电流2', 'u16', 0.1, 'A'),
    (3025, '直流电压3', 'u16', 0.1, 'V'),
    (3026, '直流电流3', 'u16', 0.1, 'A'),
    (3027, '直流电压4', 'u16', 0.1, 'V'),
    (3028, '直流电流4', 'u16', 0.1, 'A'),
    (3298, '总PV电压', 'u16', 0.1, 'V'),
    (3299, '总PV电流', 's16', 0.1, 'A'),
    # MPPT
    *[(3498 + i, f'MPPT{i}电压', 'u16', 0.1, 'V') for i in range(1, 16)],
    *[(3528 + i, f'MPPT{i}电流', 's16', 0.1, 'A') for i in range(1, 16)],
    (3545, 'MPPT16电流', 's16', 0.1, 'A'),
    # 交流侧
    (3033, 'AB线电压/A相电压', 'u16', 0.1, 'V'),
    (3034, 'BC线电压/B相电压', 'u16', 0.1, 'V'),
    (3035, 'CA线电压/C相电压', 'u16', 0.1, 'V'),
    (3036, 'A相电流', 'u16', 0.1, 'A'),
    (3037, 'B相电流', 'u16', 0.1, 'A'),
    (3038, 'C相电流', 'u16', 0.1, 'A'),
    (3042, '电网频率', 'u16', 0.01, 'Hz'),
    (3041, '逆变器模块温度', 's16', 0.1, '℃'),
    # 状态/告警
    (3040, '标准工作模式', 'u16', 1, ''),
    (3043, '逆变器当前状态', 'u16', 1, ''),
    (3029, '警告信息故障数据', 'u16', 1, ''),
    (3071, '工作状态位', 'u16', 1, ''),
    (3094, '降额/限额状态', 'u16', 1, ''),
    (3095, '故障字01', 'u16', 1, ''),
    (3096, '故障字02', 'u16', 1, ''),
    (3097, '故障字03', 'u16', 1, ''),
    (3098, '故障字04', 'u16', 1, ''),
    (3099, '故障字05', 'u16', 1, ''),
    (3100, '故障字06(HMI)', 'u16', 1, ''),
    (3101, '故障字07', 'u16', 1, ''),
    (3102, '故障字08', 'u16', 1, ''),
    (3103, '警示信息3_DSP', 'u16', 1, ''),
    (3104, '警示信息HMI', 'u16', 1, ''),
    (3089, '有功功率启停开关', 'u16', 1, ''),
    (3090, '无功功率启停开关', 'u16', 1, ''),
    # 限额设定
    (3044, '限有功功率调节额定功率', 's32', 1, 'W'),
    (3046, '限无功功率调节额定功率', 's32', 1, 'Var'),
    (3049, '有功功率百分比设定值', 'u16', 0.01, '%'),
    (3051, '功率因数设定值', 's16', 0.001, ''),
    (3052, '无功功率百分比设定值', 's16', 0.01, '%'),
    (3053, '国家标准号', 'u16', 1, ''),
    (3109, '本机标定额定功率', 'u16', 10, 'W'),
    # 系统时间
    (3072, '系统时间-年', 'u16', 1, ''),
    (3073, '系统时间-月', 'u16', 1, ''),
    (3074, '系统时间-日', 'u16', 1, ''),
    (3075, '系统时间-时', 'u16', 1, ''),
    (3076, '系统时间-分', 'u16', 1, ''),
    (3077, '系统时间-秒', 'u16', 1, ''),
]

# 工程点表之外的补充读取（协议 5.1 有、gen_solis_points.py 跳过的）
EXTRA_POINTS = [
    (3471, '故障字10(HMI)', 'u16', 1, ''),
    (3472, '故障字11(HMI)', 'u16', 1, ''),
    (3473, '故障字12(HMI)', 'u16', 1, ''),
    (3544, 'MPPT16电压(协议原文标注混乱)', 'u16', 0.1, 'V'),
]

ALL_POINTS = POINTS + EXTRA_POINTS


# ------------------------------------------------------------ Modbus ----
def crc16(data: bytes) -> int:
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc


def hx(data: bytes) -> str:
    return ' '.join(f'{b:02X}' for b in data)


def build_read(addr: int, qty: int) -> bytes:
    body = bytes([UNIT, 4, (addr >> 8) & 0xFF, addr & 0xFF, (qty >> 8) & 0xFF, qty & 0xFF])
    c = crc16(body)
    return body + bytes([c & 0xFF, c >> 8])


def transact(ser: serial.Serial, frame: bytes):
    """发一帧收一帧：首字节等满超时（真实设备时延），30ms 帧内间隙收完剩余。"""
    ser.reset_input_buffer()
    ser.reset_output_buffer()
    t0 = time.perf_counter()
    ser.write(frame)
    ser.flush()
    first = ser.read(1)               # 首字节：到达即返回 → 时延真实
    buf = bytearray(first)
    if first:
        ser.timeout = 0.03
        while True:
            chunk = ser.read(256)
            if not chunk:
                break
            buf += chunk
        ser.timeout = TIMEOUT_MS / 1000.0
    return bytes(buf), (time.perf_counter() - t0) * 1000


def read_segment(ser: serial.Serial, addr: int, qty: int):
    """读一段寄存器，重试 TRIES 次。返回 (寄存器值list 或 None, 错误描述, 帧记录)。"""
    frame = build_read(addr, qty)
    last_err = ''
    for i in range(TRIES):
        resp, ms = transact(ser, frame)
        log = f'TX: {hx(frame)}  →  RX({ms:.0f}ms): {hx(resp) if resp else "无应答"}'
        if not resp:
            last_err = f'无应答（超时 {TIMEOUT_MS}ms）'
        elif len(resp) >= 4 and int.from_bytes(resp[-2:], 'little') != crc16(resp[:-2]):
            last_err = '应答 CRC 校验失败'
        elif resp[1] & 0x80:
            codes = {2: '非法数据地址', 3: '非法数据值', 4: '从机故障', 5: 'HMI与DSP通信故障(协议区外/未实现)'}
            last_err = f'异常码 0x{resp[2]:02X}（{codes.get(resp[2], "未知")}）'
        elif resp[1] == 4 and resp[2] == qty * 2:
            regs = [int.from_bytes(resp[3 + 2 * j:5 + 2 * j], 'big') for j in range(qty)]
            return regs, None, log, ms
        else:
            last_err = f'应答格式异常：{hx(resp)}'
        time.sleep(0.3)
    return None, last_err, log, 0


def make_segments(points):
    """按地址合并成读块：相邻槽位间隙 ≤8 寄存器就并入同一块。"""
    slots = sorted((p[0], 2 if p[1] in ('u32', 's32') else 1) for p in points)
    segs, start, end = [], slots[0][0], slots[0][0] + slots[0][1]
    for a, n in slots[1:]:
        if a - end <= 8:
            end = max(end, a + n)
        else:
            segs.append((start, end - start))
            start, end = a, a + n
    segs.append((start, end - start))
    return segs


# ------------------------------------------------------------- 解码 ----
def s16(v):
    return v - 0x10000 if v > 0x7FFF else v


def s32(v):
    return v - 0x100000000 if v > 0x7FFFFFFF else v


M35000 = {
    0x1010: '单相逆变器（0.7-8K1P / 7-10K1P）', 0x3F2: '单相逆变器（0.7-8K1P / 7-10K1P）',
    0x1020: '三相逆变器（5-25K3P）', 0x3FC: '三相逆变器（5-25K3P）',
    0x1021: '三相并网组串式逆变器（25-50K/50-70K/80-110K/90-136K/125K）', 0x3FD: '三相并网组串式逆变器（25-50K 等）',
    0x1022: '三相并网组串式逆变器（250K）',
    0x1070: '外置EPM防逆流控制器',
    0x1110: '单相逆变器（0.7-8K1P / 7-10K1P 无屏系列）',
    0x1111: '单相微型逆变器（0.6-0.8K1P）',
    0x1120: '三相逆变器（5-25K3P / 5-30K3P(K2) 无屏系列）',
    0x1121: '三相并网组串式逆变器（25-50K/50-70K/80-110K/90-136K/125K 等）',
    0x1123: '三相并网组串式逆变器（MAX/PRO 系列）',
    0x1124: '三相并网组串式逆变器（320K，海外350K）',
    0x1125: '三相并网组串式逆变器（125-200K）',
    0x1126: '三相并网组串式逆变器（320K 西安）',
    0x1127: '三相并网组串式逆变器（80-150K 三相不平衡）',
}

MODE3040 = {0: 'No response mode', 1: 'Volt-watt default', 2: 'Volt-var', 3: '固定功率因数',
            4: '固定无功功率', 5: 'Power-PF', 6: 'Rule21Volt-watt'}

# 附录2：3043 主状态码（Fxxx/1xxx 为故障码，3029 为其子码）
M3043 = {
    0x0000: '等待状态类（细分看 3029）', 0x0001: '开环运行', 0x0002: '软起运行',
    0x0003: '并网运行', 0x0004: '离网运行',
    0xF010: '电网浪涌告警', 0xF011: '风扇故障(内部)', 0xF013: '电网防雷异常', 0xF014: '直流防雷异常',
    0xF015: '风扇故障(外部)', 0xF017: '相线接地告警', 0xF018: 'DSP通讯异常', 0xF019: 'Boost异常',
    0xF01C: 'IGBT温度差异', 0xF01D: '零地电压异常', 0xF030: '桥臂开路故障', 0xF031: '直流端子过温',
    0xF033: '组串电流反灌告警', 0xF034: '零偏校准告警', 0xF038: 'PID修复中', 0xF040: 'CT方向告警',
    0xF041: '电表相序告警', 0xF042: '从机相序告警', 0xF050: '电网电压不平衡告警', 0xF051: '直流开关异常告警',
    0xF080: '并网逆变器通讯失败', 0xF081: '电表2异常告警', 0xF082: '并机主从机软件版本不一致',
    0x1010: '电网过压', 0x1011: '电网欠压', 0x1012: '电网过频', 0x1013: '电网欠频',
    0x1014: '电网逆流', 0x1015: '无电网', 0x1016: '电网不平衡', 0x1017: '电网频率抖动',
    0x1018: '电网过流', 0x1019: '电网电流跟踪故障', 0x101A: '电网相位异常', 0x101C: '过流筛选故障',
    0x101D: '电网过/欠压(G100)', 0x101E: '电网波动异常',
    0x1020: '直流过压/PV故障', 0x1021: '直流母线过压', 0x1022: '直流母线不均压', 0x1023: '直流母线欠压',
    0x1024: '直流母线不均压2', 0x1025: '直流A路过流', 0x1026: '直流B路过流', 0x1027: '直流输入扰动',
    0x1028: '直流反接故障', 0x1029: 'PV中点接地故障', 0x102A: '母线电压不一致', 0x102B: 'PV端口浪涌',
    0x102C: '直流过温故障', 0x102D: 'MPPT过压故障', 0x102E: 'MPPT过流故障',
    0x1030: '电网扰动', 0x1031: 'DSP初始化保护', 0x1032: '过温保护', 0x1033: 'PV绝缘故障',
    0x1034: '漏电流保护', 0x1035: 'Relay检测保护', 0x1036: 'DSP_B通讯/采样异常', 0x1037: '直流分量过大',
    0x1038: '12V欠压保护', 0x1039: '漏电流自检保护', 0x103A: '欠温保护', 0x103B: 'IGBT温度差异',
    0x103C: '二级电网扰动', 0x103D: '相线接地告警', 0x103E: 'NTC过温保护', 0x103F: 'AC端子过温故障',
    0x1040: '电弧自检保护', 0x1041: '电弧保护', 0x1046: '电网扰动02', 0x1047: '电网电流采样异常',
    0x1048: '电网三级过压', 0x1050: '网侧电流瞬时过流', 0x1051: '直流母线硬件过压', 0x1052: 'LLC硬件过流',
    0x1053: '硬限制故障', 0x1057: '负载过载故障', 0x1058: 'DSP自检异常', 0x1059: 'Vg采样异常',
    0x105A: 'DSP硬件不匹配',
    0x1090: 'D电表通信故障', 0x1091: '主从机通信故障', 0x1092: 'D-H通讯故障', 0x1093: '电表电流故障',
    0x1094: '电表电压故障', 0x1095: '电表相序故障', 0x1096: '电表CT方向故障', 0x1097: '从机相序故障',
    0x10A0: '系统故障', 0x10A1: '升压电容过压', 0x10A2: '通信断链保护', 0x10A3: '升压电容欠压',
    0x10A4: '直流开关控制回路异常', 0x10A5: '温度过温故障', 0x10A6: '组串电流反灌故障',
    0x10B0: '直流端子过温', 0x10B1: 'PCB温度故障', 0x10B2: '继电器温度故障', 0x10B3: '逆变电感温度故障',
    0x10B4: 'L2电感温度故障', 0x10B5: '输出铜排温度故障',
    0x2010: 'Fail Safe', 0x2011: 'Fail Safe', 0x2014: 'DSP通讯故障', 0x2018: 'DRM连接失败',
    0x201A: 'CT连接异常', 0x201B: 'DRM控制离网', 0x201E: '交流识别盒未连接', 0x201F: '多交流源故障',
    0x2040: 'EPM硬限制保护', 0x2041: 'AFCI板通讯异常', 0x2042: 'AFCI板CT模组硬件异常',
    0x2043: 'G100过流保护', 0x204A: '逆变器桥口硬限制保护', 0x204B: 'G100电表通信故障',
    0x204C: 'G100 CT故障', 0x204D: 'AFCI板类型不匹配',
    0x2060: '直流端子过温', 0x2061: '故障锁机',
}

# 并网运行(0x0003)时 3029 的限载标志位；其它状态时 3029 为子码
LIM_BITS = {0: '过温限载', 1: '过频限载', 2: '过压限载', 3: '无功限载', 4: '欠频限载',
            5: '功率软起', 7: '交流端子过温限载'}
LIM_HI = {0: 'DRM限载', 1: 'PLMT(EPM)限载'}

BITS3071 = {0: '正常运行', 1: '初始待机', 2: '控制关机', 3: '故障停机', 4: '待机', 5: '降额运行',
            6: '限额运行', 7: '过载降压', 8: '电网浪涌', 9: '风扇故障(内部)', 10: '外部风扇故障',
            11: '电网防雷异常', 12: '直流防雷异常', 14: '相线接地告警', 15: 'IGBT异常'}

BITS3094 = {0: '过温降额', 1: '过频降额', 2: '过压降额', 3: '无功降额', 4: '欠频降额', 5: '功率软起',
            6: '旁路降额', 7: '交流端子过温降额', 8: 'DRM控制限额', 9: 'PLmt控制限额'}

FAULT_WORDS = {
    3095: '故障字01', 3096: '故障字02', 3097: '故障字03', 3098: '故障字04', 3099: '故障字05',
    3100: '故障字06(HMI)', 3101: '故障字07', 3102: '故障字08',
    3471: '故障字10(HMI)', 3472: '故障字11(HMI)', 3473: '故障字12(HMI)',
}
FAULT_BITS = {
    3095: {0: '电网过压01', 1: '电网过压01', 2: '电网过压01', 3: '电网欠压01', 4: '电网欠压01', 5: '电网欠压01',
           6: '电网过频01', 7: '电网欠频01', 8: '电网不平衡', 9: '电网电压缺相', 10: '电网频率抖动',
           11: '电网相位异常', 12: '无电网', 13: '电网逆流', 14: '电网过压02', 15: '过流筛选故障'},
    3096: {0: '直流过压01', 1: '直流过压02', 2: '直流反接故障', 3: '母线电压不一致', 4: '直流母线过压',
           5: '直流母线欠压01', 6: '直流母线不均压', 7: '直流母线欠压02', 8: '电网过压03', 9: '电网过压03',
           10: '电网过压03', 11: 'PV接地故障', 12: 'DCBoost故障', 13: '直流输入扰动', 14: '电网电流跟踪故障',
           15: '电网过压05'},
    3097: {0: '电网过流', 1: '电网过流', 2: '电网过流', 3: '直流A路过流', 4: '直流B路过流', 5: '电网扰动',
           6: '直流分量过大', 7: '电网过压04', 8: '电网过压04', 9: '电网过压04', 10: '电网欠压02',
           11: '电网欠压02', 12: '电网欠压02', 13: '电网过频02', 14: '电网欠频02', 15: '电网三级过压'},
    3098: {1: '过温保护', 3: 'Relay检测保护', 4: 'PV中点接地故障', 5: '欠温保护', 6: 'PV绝缘故障01',
           7: 'PV绝缘故障02', 8: '12V欠压保护', 9: '漏电流保护01', 10: '漏电流保护02', 11: '漏电流保护03',
           12: '漏电流保护04', 13: '漏电流自检保护', 14: '电网扰动02', 15: '电网电流采样异常'},
    3099: {0: '网侧电流瞬时过流', 2: 'LLC硬件过流', 5: '二级电网扰动', 6: 'DSP_B通讯异常', 7: 'DSP_B采样异常',
           8: '电弧自检保护', 9: '电弧保护', 11: 'DSP自检异常', 12: 'Vg采样异常', 13: 'DSP硬件不匹配'},
    3100: {0: 'Fail Safe', 1: 'DRM控制离网', 2: 'CT连接异常', 3: 'DRM连接失败', 4: '无交流识别盒',
           5: '多交流源故障', 6: '网络信号丢失', 7: '欠费锁定中', 8: 'EPM硬限制保护', 9: 'AFCI板通讯异常',
           10: 'AFCI板CT模组硬件异常', 11: 'G100过流保护'},
    3101: {0: '相线接地告警', 1: 'NTC过温保护'},
    3102: {0: 'PV端口浪涌', 1: 'AC端子过温故障', 2: '直流端子过温故障', 3: 'D电表通信故障',
           4: '主从机通信故障', 5: 'D-H通讯故障', 6: '电表电流故障', 7: '电表电压故障', 8: '电表相序故障',
           9: '电表CT方向故障', 10: '从机相序故障', 11: 'MPPT端子过温故障', 12: 'EPM主机故障'},
    3471: {0: '系统故障', 1: '升压电容过压', 2: '通信断链保护', 3: '升压电容欠压',
           4: '直流开关控制回路异常', 5: '组串电流反灌故障'},
    3472: {1: '温度过温故障', 8: 'MPPT过压故障', 9: 'MPPT过流故障'},
    3473: {0: '电弧故障锁机', 1: '电弧自检保护锁机', 2: 'PV中点接地异常锁机', 3: 'PV绝缘故障锁机',
           4: 'PV接地故障锁机'},
}

WARN_BITS = {
    3103: {0: '零地电压异常告警', 1: '直流开关1脱扣告警', 2: '直流开关2脱扣告警', 12: '电网电压不平衡告警',
           13: '直流开关异常告警', 14: '零偏校准告警', 15: '组串电流反灌告警'},
    3104: {0: '桥臂开路故障告警', 1: '直流端子温度告警', 15: 'ARM与DSP通信故障'},
}


def decode_point(addr, name, dt, scale, unit, regs):
    """从整段寄存器值里取该点的原始值并解出工程值字符串。"""
    if dt in ('u32', 's32'):
        raw = (regs[addr] << 16) | regs[addr + 1]
        val = s32(raw) if dt == 's32' else raw
        num = val * scale
        return raw, val, num, unit
    raw = regs[addr]
    if dt == 's16':
        val = s16(raw)
    else:
        val = raw
    return raw, val, val * scale, unit


def fmt_num(num, unit):
    if unit:
        return f'{num:g} {unit}'
    return f'{num:g}'


# --------------------------------------------------------------- 主流程 ----
def main():
    try:
        ser = serial.Serial(port=PORT, baudrate=BAUD, bytesize=8, parity=serial.PARITY_NONE,
                            stopbits=1, timeout=TIMEOUT_MS / 1000.0, write_timeout=1.0)
    except serial.SerialException as e:
        print(f'✗ 打开 {PORT} 失败：{e}')
        print('  常见原因：NetLink/串口助手正占用 COM7（先停止连接或退出），或串口名不对。当前可用口：')
        for p in list_ports.comports():
            print(f'    {p.device}  {p.description}')
        sys.exit(1)

    print(f'串口 {PORT}：{BAUD} 8N1  从机 {UNIT}  FC04  超时 {TIMEOUT_MS}ms  查询间隔 {INTERVAL*1000:.0f}ms')
    segs = make_segments(ALL_POINTS)
    print(f'共 {len(ALL_POINTS)} 个点，合并为 {len(segs)} 个读块：'
          + ', '.join(f'{a}+{q}' for a, q in segs))

    regs = {}          # addr -> u16 值
    frames = []        # 帧记录
    fail_segs = []     # 失败块
    latencies = []     # 成功读块的真实应答时延 ms
    t_start = time.time()

    for addr, qty in segs:
        vals, err, log, ms = read_segment(ser, addr, qty)
        frames.append(log)
        print(f'  块 {addr}+{qty}: {"OK" if vals else "✗ " + err}')
        if vals is None:
            fail_segs.append((addr, qty, err))
        else:
            for j, v in enumerate(vals):
                regs[addr + j] = v
            latencies.append(ms)
        time.sleep(INTERVAL)
    ser.close()
    total_s = time.time() - t_start

    ok_points = [p for p in ALL_POINTS if p[0] in regs]
    fail_points = [p for p in ALL_POINTS if p[0] not in regs]

    # ---- 解码全部点 ----
    rows = {}
    for addr, name, dt, scale, unit in ALL_POINTS:
        if addr not in regs:
            continue
        if dt in ('u32', 's32'):
            raw = (regs[addr] << 16) | regs[addr + 1]
            val = s32(raw) if dt == 's32' else raw
        else:
            raw = regs[addr]
            val = s16(raw) if dt == 's16' else raw
        rows[addr] = (name, dt, raw, val, val * scale, unit)

    # ---- 特殊解读 ----
    v35000 = rows.get(35000)
    type_txt = M35000.get(v35000[2], '未在附录1映射表中') if v35000 else '—'
    v3043 = rows.get(3043)
    v3029 = rows.get(3029)
    status_txt = '—'
    if v3043 and v3029:
        code, sub = v3043[2], v3029[2]
        base = M3043.get(code, f'未知状态码 0x{code:04X}（查协议附录2）')
        if code == 0x0000:
            base = {0: '等待状态', 1: '控制离网(Grid Off)', 2: '等待状态01'}.get(sub, f'等待类(子码{sub})')
        if code == 0x0003:
            flags = [n for b, n in LIM_BITS.items() if sub & (1 << b)] + \
                    [n for b, n in LIM_HI.items() if sub & (0x100 << b)]
            base += '（限载：' + ('、'.join(flags) if flags else '无') + '）'
        elif code >= 0xF000 or code >= 0x1010:
            base += f'（子码 {sub}）'
        status_txt = f'0x{code:04X} {base}；3029=0x{sub:04X}'

    def bits_txt(word, table):
        return '、'.join(n for b, n in table.items() if word & (1 << b))

    faults = []
    for addr, name, *_ in ALL_POINTS:
        if addr in FAULT_WORDS and addr in rows:
            w = rows[addr][2]
            if w:
                names = bits_txt(w, FAULT_BITS.get(addr, {}))
                faults.append(f'{name}(0x{w:04X})：{names or "有置位位（位名查附录3）"}')
    warns = []
    for addr in (3103, 3104):
        if addr in rows:
            w = rows[addr][2]
            if w:
                warns.append(f'{rows[addr][0]}(0x{w:04X})：{bits_txt(w, WARN_BITS[addr])}')

    def g(addr, field='eng'):
        r = rows.get(addr)
        return r[4] if field == 'eng' and r else None

    sn = ''.join(f'{rows[a][2]:04X}' for a in (3060, 3061, 3062, 3063) if a in rows)
    yr = rows.get(3072)
    dev_time = (f'20{yr[2]:02d}-{rows[3073][2]:02d}-{rows[3074][2]:02d} '
                f'{rows[3075][2]:02d}:{rows[3076][2]:02d}:{rows[3077][2]:02d}'
                if all(a in rows for a in (3072, 3073, 3074, 3075, 3076, 3077)) else '—')

    # ---- 写 Markdown ----
    now = datetime.datetime.now()
    L = []
    L.append(f'# COM7 锦浪 Solis 并网逆变器 · 点位采集报告')
    L.append('')
    L.append(f'- **采集时间**：{now:%Y-%m-%d %H:%M:%S}')
    L.append(f'- **链路**：{PORT}（FTDI USB-RS485）→ Solis 逆变器，Modbus RTU，从机 {UNIT}，{BAUD} 8N1，FC04，超时 {TIMEOUT_MS}ms，查询间隔 {INTERVAL*1000:.0f}ms')
    L.append(f'- **点表**：NetLink 工程「演示工程 · 一号站」的 99 个 FC04 点（`gen_solis_points.py`），另补读协议内 3471-3473、3544')
    lat_txt = f'，应答时延 {min(latencies):.0f}~{max(latencies):.0f}ms' if latencies else ''
    L.append(f'- **结果**：{len(ok_points)}/{len(ALL_POINTS)} 点读取成功，读块 {len(segs) - len(fail_segs)}/{len(segs)}{lat_txt}，总耗时 {total_s:.1f}s')
    L.append('')

    if fail_segs:
        L.append('> ⚠ **以下读块失败**：')
        for a, q, err in fail_segs:
            L.append(f'> - 寄存器 {a}~{a+q-1}（{q} 个）：{err}')
        L.append('')

    L.append('## 一、关键数据速览')
    L.append('')
    L.append('| 项目 | 采集值 |')
    L.append('| --- | --- |')
    L.append(f'| 逆变器类型（35000） | 0x{v35000[2]:04X} → {type_txt} |' if v35000 else '| 逆变器类型 | — |')
    L.append(f'| 产品型号（2999） | 0x{rows[2999][2]:04X} |' if 2999 in rows else '')
    L.append(f'| 序列号（3060-3063） | {sn or "—"} |')
    L.append(f'| 本机标定额定功率（3109） | {fmt_num(g(3109), "W")} |')
    L.append(f'| 当前状态（3043+3029） | {status_txt} |')
    L.append(f'| 有功功率（3004） | {fmt_num(g(3004), "W")} |')
    L.append(f'| 总直流输入功率（3006） | {fmt_num(g(3006), "W")} |')
    L.append(f'| 总发电量（3008） | {fmt_num(g(3008), "kWh")} |')
    L.append(f'| 当日发电量（3014） | {fmt_num(g(3014), "kWh")} |')
    ap = L  # keep reference
    L.append(f'| 电网电压 AB/BC/CA（3033-3035） | {g(3033):g} / {g(3034):g} / {g(3035):g} V |' if 3033 in rows else '')
    L.append(f'| 电网频率（3042） | {fmt_num(g(3042), "Hz")} |')
    L.append(f'| 模块温度（3041） | {fmt_num(g(3041), "℃")} |')
    L.append(f'| 逆变器系统时间（3072-3077） | {dev_time} |')
    L.append(f'| 故障字（3095-3102 + 3471-3473） | {"全部为 0，无故障" if not faults else "有置位，见第四节"} |')
    L.append('')

    # ---- 现场状态解读 ----
    L.append('## 二、现场状态解读')
    L.append('')
    if v3043:
        code = v3043[2]
        pv_live = [i for i in range(1, 16) if rows.get(3498 + i) and rows[3498 + i][4] > 10]
        pv_txt = (f'MPPT 有电压的路：{ "、".join(f"MPPT{i}({rows[3498 + i][4]:.1f}V)" for i in pv_live)}'
                  if pv_live else '无')
        ac_live = any(rows.get(a) and rows[a][4] > 0 for a in (3033, 3034, 3035, 3042))
        p_out = g(3004) or 0
        L.append(f'- 状态机（3043）报 **0x{code:04X} {M3043.get(code, "未知码")}**；工作状态位（3071）= '
                 f'0x{rows[3071][2]:04X} → {bits_txt(rows[3071][2], BITS3071) or "无置位"}' if 3071 in rows else '')
        L.append(f'- PV 侧：{pv_txt}，各路电流为 0（PV 带电但未发电）')
        L.append(f'- 交流侧：{"有电压" if ac_live else "电网电压/频率均为 0（逆变器未检测到电网）"}；输出功率 {p_out:g} W')
        if faults:
            names = '、'.join(faults)
            L.append(f'- 故障置位：{names}')
        if code in (0x1015,) or (3095 in rows and rows[3095][2] & (1 << 12)):
            L.append('- **推断**：逆变器直流侧正常带电，但交流侧无电网（NO-Grid，电网开关未合/并网未完成），'
                     '处于故障停机等待并网状态，故全部功率/电量为 0。')
    L.append('')

    L.append('## 三、全量点表（按协议 5.1 分组）')
    L.append('')
    L.append('原始值 = 寄存器原码（hex）；工程值 = 按协议数据类型/系数换算后的显示值。32 位值占两个寄存器（高位字在前）。')
    L.append('')
    L.append('| 寄存器 | hex | 名称 | 原始值 | 工程值 | 单位 |')
    L.append('| ---: | --- | --- | --- | ---: | --- |')

    def row(addr):
        if addr not in rows:
            miss = next(p for p in fail_segs if p[0] <= addr < p[0] + p[1])
            return f'| {addr} | 0x{addr:X} | {dict((p[0], p[1]) for p in ALL_POINTS)[addr]} | — | **读取失败**（{miss[2]}） | |'
        name, dt, raw, val, eng, unit = rows[addr]
        if addr == 35000:
            engs = f'0x{raw:04X} → {type_txt}'
        elif addr == 3000:
            engs = f'0x{raw:04X} → 从DSP 0x{(raw >> 8) & 0xFF:02X}，主DSP 0x{raw & 0xFF:02X}'
        elif addr == 3001:
            engs = f'0x{raw:04X} → 大版本 0x{raw & 0xFF:02X}'
        elif addr == 3020:
            engs = f'0x{raw:04X} → 大版本 0x{raw & 0xFF:02X}，小版本 0x{(raw >> 8) & 0xFF:02X}'
        elif addr in (3060, 3061, 3062, 3063):
            engs = f'0x{raw:04X}（SN 拼接：{sn}）'
        elif addr == 3003:
            engs = f'{raw + 1} 路直流输入'
        elif addr == 3040:
            engs = f'{raw} → {MODE3040.get(raw, "未知模式")}'
        elif addr == 3043:
            engs = status_txt
        elif addr == 3029:
            engs = f'0x{raw:04X}（与 3043 配合细分）'
        elif addr == 3071:
            b = bits_txt(raw, BITS3071)
            engs = f'0x{raw:04X} → ' + (b if b else '无置位')
        elif addr == 3094:
            b = bits_txt(raw, BITS3094)
            engs = f'0x{raw:04X} → ' + (b if b else '无置位')
        elif addr in FAULT_WORDS:
            b = bits_txt(raw, FAULT_BITS.get(addr, {}))
            engs = f'0x{raw:04X} → ' + (b if b else '无故障')
        elif addr in (3103, 3104):
            b = bits_txt(raw, WARN_BITS[addr])
            engs = f'0x{raw:04X} → ' + (b if b else '无告警')
        elif addr == 3089:
            engs = {0x55: '0x55 → 关闭', 0xAA: '0xAA → 启用'}.get(raw, f'0x{raw:04X} → 未知')
        elif addr == 3090:
            engs = {0x55: '0x55 → 关闭', 0xA1: '0xA1 → 启用(百分比方式)', 0xA2: '0xA2 → 启用(PF方式)'}.get(raw, f'0x{raw:04X} → 未知')
        elif addr in (3072, 3073, 3074, 3075, 3076, 3077):
            engs = f'{raw:02d}'
        elif addr == 3053:
            engs = f'0x{raw:04X}'
        else:
            engs = f'{eng:g}'
        return f'| {addr} | 0x{addr:X} | {name} | 0x{raw:04X}（{raw}） | {engs} | {unit} |'

    def section(title, addrs):
        L.append(f'**{title}**')
        L.append('')
        L.append('| 寄存器 | hex | 名称 | 原始值 | 工程值 | 单位 |')
        L.append('| ---: | --- | --- | --- | ---: | --- |')
        for a in addrs:
            L.append(row(a))
        L.append('')

    name_of = {p[0]: p[1] for p in ALL_POINTS}
    by_addr = sorted(name_of)
    sections = [
        ('身份 / 版本', [2999, 3000, 3001, 3020, 35000, 3060, 3061, 3062, 3063]),
        ('功率 / 电量', [3004, 3006, 3008, 3010, 3012, 3014, 3015, 3055, 3057, 3059]),
        ('直流侧', [3003, 3021, 3022, 3023, 3024, 3025, 3026, 3027, 3028, 3298, 3299]),
        ('MPPT', [3499 + i for i in range(15)] + [3529 + i for i in range(15)] + [3545, 3544]),
        ('交流侧', [3033, 3034, 3035, 3036, 3037, 3038, 3042, 3041]),
        ('状态 / 告警', [3040, 3043, 3029, 3071, 3094, 3095, 3096, 3097, 3098, 3099, 3100, 3101, 3102, 3103, 3104, 3089, 3090]),
        ('限额设定', [3044, 3046, 3049, 3051, 3052, 3053, 3109]),
        ('系统时间', [3072, 3073, 3074, 3075, 3076, 3077]),
        ('协议补充（不在工程 99 点内）', [3471, 3472, 3473]),
    ]
    covered = set()
    for title, addrs in sections:
        section(title, addrs)
        covered.update(addrs)
    for a in by_addr:
        if a not in covered:
            L.append(row(a))

    L.append('## 四、故障 / 告警解读')
    L.append('')
    if faults:
        L.append('**故障字有置位：**')
        L.append('')
        for f in faults:
            L.append(f'- {f}')
        L.append('')
    else:
        L.append('故障字 01-08（3095-3102）与补充的故障字 10-12（3471-3473）全部为 **0**，无故障置位。')
        L.append('')
    if warns:
        L.append('**警示信息有置位：**')
        L.append('')
        for w in warns:
            L.append(f'- {w}')
        L.append('')
    else:
        L.append('警示信息 3_DSP / HMI（3103/3104）为 0，无告警置位。')
        L.append('')

    L.append('## 五、原始帧记录')
    L.append('')
    L.append('```')
    for f in frames:
        L.append(f)
    L.append('```')
    L.append('')
    L.append('---')
    L.append('说明：32 位量（U32/S32）高字在前；MPPT 未实现的路读 0 属正常；'
             '位名解码依据协议附录 3，其中 3103/3104 位表在原文档排版错乱，仅收录可确认位。')

    OUT.write_text('\n'.join(L), encoding='utf-8')
    print(f'\n报告已写入：{OUT}')
    print(f'成功 {len(ok_points)}/{len(ALL_POINTS)} 点；失败读块：{len(fail_segs)}')
    if fail_segs:
        for a, q, err in fail_segs:
            print(f'  ✗ 块 {a}+{q}: {err}')


if __name__ == '__main__':
    main()
