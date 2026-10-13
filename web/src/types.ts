// 与 netlink-core 的 model.rs 保持同构的领域类型（snake_case 字段名直接对齐 Rust serde）

export type Area = 'coil' | 'discrete_input' | 'holding_register' | 'input_register'
export type DataType = 'u16' | 'i16' | 'u32' | 'i32' | 'f32' | 'u64' | 'i64' | 'f64'
export type SourceKind = 'static' | 'sine' | 'ramp' | 'random_walk' | 'swing'
export type FaultKind = 'normal' | 'timeout' | 'delay' | 'exception' | 'corrupt' | 'disconnect'
export type TransportKind = 'tcp_server' | 'serial' | 'tcp_client' | 'serial_master' | 'usb_can' | 'can_net'
export type Parity = 'none' | 'even' | 'odd'
/** USB-CAN 直采后端（v1 已实现 slcan；zlgcan/candle 待硬件到货） */
export type CanBackend = 'slcan' | 'zlgcan' | 'candle'

/** 主机类传输（主动发起轮询/连接的角色） */
export function isMasterTransport(kind: TransportKind): boolean {
  return kind === 'tcp_client' || kind === 'serial_master'
}

/** CAN 类传输（直采直控 / 转以太网） */
export function isCanTransport(kind: TransportKind): boolean {
  return kind === 'usb_can' || kind === 'can_net'
}

/** 已实现可启动的 CAN 后端（连接编辑器下拉用） */
export const CAN_BACKEND_LABELS: Record<CanBackend, string> = {
  slcan: 'slcan（串口 ASCII，CANable 等）',
  zlgcan: 'zlgcan（USBCAN/CANalyst-II，待硬件 M2）',
  candle: 'candle（WinUSB，CANable 原生，待硬件）',
}

/** 常用 CAN 波特率档位（slcan S 档：S4~S8） */
export const CAN_BITRATES = [125_000, 250_000, 500_000, 800_000, 1_000_000]

export interface ValueSource {
  kind: SourceKind
  /** static: 静态值 */
  value?: number
  /** sine / swing: 周期（秒） */
  period_s?: number
  /** sine: 幅值 */
  amplitude?: number
  /** sine: 偏移 */
  offset?: number
  /** ramp / random_walk: 步长 */
  step?: number
  /** ramp / random_walk / swing: 下限 */
  min?: number
  /** ramp / random_walk / swing: 上限 */
  max?: number
}

export interface Point {
  id: number
  name: string
  area: Area
  address: number
  data_type: DataType
  source: ValueSource
  note?: string
}

export interface FaultConfig {
  kind: FaultKind
  delay_ms?: number
  exception_code?: number
}

export interface Device {
  id: number
  name: string
  slave_id: number
  enabled: boolean
  fault: FaultConfig
  /** 主机连接下的轮询周期（毫秒） */
  poll_period_ms?: number
  points: Point[]
}

/** CAN 周期报文定义（CAN 连接下的「点位」层） */
export interface CanPeriodicFrame {
  id: number
  /** 0x000~0x7FF 标准 / 0x00000000~0x1FFFFFFF 扩展 */
  frame_id: number
  extended: boolean
  /** 远程帧（数据域为空） */
  remote: boolean
  /** ≤8 字节；dlc = data.length */
  data: number[]
  /** 发送周期（毫秒）；0 = 不自动发（仅保存定义，手动发送面板可载入） */
  period_ms: number
  enabled: boolean
  note?: string
}

/** CAN 节点（CAN 连接下的「设备」层：一组周期发送报文） */
export interface CanNode {
  id: number
  name: string
  enabled: boolean
  frames: CanPeriodicFrame[]
}

export interface Transport {
  kind: TransportKind
  /** tcp_server */
  listen_addr?: string
  port?: number
  /** serial */
  serial_port?: string
  baud?: number
  parity?: Parity
  stop_bits?: number
  /** tcp_client */
  host?: string
  timeout_ms?: number
  /** usb_can */
  backend?: CanBackend | string
  device_index?: number
  channel?: number
  bitrate?: number
  /** usb_can + slcan：串口名与串口侧波特率 */
  serial_baud?: number
  /** can_net */
}

export interface Connection {
  id: number
  name: string
  transport: Transport
  devices: Device[]
  /** CAN 连接下的节点（Modbus 连接恒为空数组） */
  can_nodes?: CanNode[]
}

export interface ProjectSpec {
  schema_version: number
  name: string
  connections: Connection[]
}

/** 工程目录里的一个工程（切换列表用） */
export interface ProjectMeta {
  /** 工程目录内的文件名（切换工程用它） */
  file_name: string
  /** 工程名（工程文件 name 字段；解析失败退化为文件名主干） */
  name: string
  /** 最后修改时间（Unix 毫秒） */
  modified_at: number
}

/** 加载/新建/保存工程返回的句柄：工程数据 + 落盘位置 */
export interface ProjectHandle {
  /** 工程文件绝对路径；null = 尚未落盘的演示工程 */
  path: string | null
  spec: ProjectSpec
}

/** 点表文件里的一行点位（Point 减 id；导入时由前端分配 id） */
export interface PointRow {
  name: string
  area: Area
  address: number
  data_type: DataType
  source: ValueSource
  note?: string
}

/** 点表文件的解析错误（行号 1 起始，含表头） */
export interface SheetRowError {
  line: number
  reason: string
}

/** 点表文件解析结果：成功行 + 坏行报告（坏行不中断整批） */
export interface SheetParseResult {
  rows: PointRow[]
  errors: SheetRowError[]
}

export interface FrameRecord {
  id: number
  /** epoch 毫秒 */
  ts: number
  conn_id: number
  device_id?: number
  dir: 'in' | 'out'
  hex: string
  note?: string
  /** 点位扫描事务产生的帧（监视器默认隐藏，可开） */
  scan?: boolean
}

// ---------- Modbus 点位扫描（与 Rust 端 scan.rs 同构） ----------

/** 扫描参数（下发到主机连接） */
export interface ScanParams {
  unit: number
  areas: Area[]
  /** 起始地址（含） */
  start_addr: number
  /** 结束地址（含） */
  end_addr: number
}

/** 扫描发现的可读区间 */
export interface ScanRange {
  area: Area
  start: number
  /** 槽位数量（字区 = 寄存器个数；位区 = 位个数） */
  qty: number
  /** 每槽位原始值（寄存器原值；位区 0/1） */
  values: number[]
}

/** 不可读的一块（存疑 / 拒读） */
export interface ScanBlock {
  area: Area
  start: number
  qty: number
  reason: string
}

/** 扫描结果 */
export interface ScanResult {
  ranges: ScanRange[]
  /** 超时未应答但设备存活的块 */
  suspicious: ScanBlock[]
  /** 从机明确拒绝的块 */
  rejected: ScanBlock[]
  /** 中止原因（取消 / 链路故障）；正常完成为 null */
  aborted: string | null
}

/** 扫描进度（运行时逐连接推送） */
export interface ScanProgress {
  conn_id: number
  unit: number
  area: Area | null
  cur_addr: number
  processed: number
  total: number
  ranges: number
  suspicious: number
  done: boolean
  aborted: string | null
}

/** 主机侧采集健康统计 */
export interface DevStat {
  last_ok_ms: number
  timeout_count: number
  fail_count: number
  /** 最近一次采集失败原因（成功采集后清空；仅主机侧设备维护） */
  last_error?: string | null
}

/** 主机连接链路状态 */
export interface ConnState {
  kind: 'connecting' | 'connected' | 'reconnecting'
  /** 断开原因等细节（悬浮提示展示） */
  detail?: string | null
}

/** CAN 接收聚合行（ID 聚合视图，运行时状态不进工程文件） */
export interface CanRxStat {
  frame_id: number
  extended: boolean
  count: number
  /** 平均到达间隔（毫秒）；不足两个间隔时为 0 */
  avg_period_ms: number
  /** 最近一次数据域（大写空格 hex） */
  last_data: string
  /** 最近一次到达（epoch 毫秒） */
  last_ts: number
}

/** CAN 连接观测快照（聚合行 + 帧率/计数） */
export interface CanAgg {
  rows: CanRxStat[]
  tx_total: number
  rx_total: number
  err_total: number
  /** 发送帧率（帧/秒，滑动窗口） */
  tx_rate: number
  /** 接收帧率（帧/秒，滑动窗口） */
  rx_rate: number
}

/** 连接编辑器「测试连接 / 检查端口 / 检查串口」结果 */
export interface TestConnResult {
  ok: boolean
  message: string
  latency_ms?: number | null
}

export const TRANSPORT_LABELS: Record<TransportKind, string> = {
  tcp_server: 'Modbus TCP 从机（网口监听）',
  serial: 'Modbus RTU 从机（串口 485）',
  tcp_client: 'Modbus TCP 主机（连接远端）',
  serial_master: 'Modbus RTU 主机（串口 485 轮询）',
  usb_can: 'CAN 直采直控（USB-CAN）',
  can_net: 'CAN 转以太网（ZLG CANET）',
}

export const AREA_LABELS: Record<Area, string> = {
  coil: '线圈',
  discrete_input: '离散输入',
  holding_register: '保持寄存器',
  input_register: '输入寄存器',
}

/** Modicon 风格各区起始地址（0 基协议地址 + 基址 = 全局地址） */
export const AREA_BASE: Record<Area, number> = {
  coil: 1,
  discrete_input: 10001,
  input_register: 30001,
  holding_register: 40001,
}

/** 点位全局起始地址（5 位显示，如 30025 / 40001 / 00001） */
export function areaGlobalAddr(area: Area, address: number): string {
  return String(AREA_BASE[area] + address).padStart(5, '0')
}

export const SOURCE_LABELS: Record<SourceKind, string> = {
  static: '静态',
  sine: '正弦',
  ramp: '斜坡',
  random_walk: '随机游走',
  swing: '区间摆动',
}

export const DATA_TYPE_LABELS: Record<DataType, string> = {
  u16: 'u16',
  i16: 'i16',
  u32: 'u32',
  i32: 'i32',
  f32: 'f32',
  u64: 'u64',
  i64: 'i64',
  f64: 'f64',
}

export const FAULT_LABELS: Record<FaultKind, string> = {
  normal: '正常',
  timeout: '超时不响应',
  delay: '延迟响应',
  exception: '异常码应答',
  corrupt: '脏帧 (CRC 错误)',
  disconnect: '断连',
}

/** 脏帧（CRC 破坏）仅对串口传输有意义；断连仅对 TCP 有意义（按设备所属连接过滤） */
export function isFaultKindAllowedForTransport(kind: FaultKind, transport: TransportKind): boolean {
  if (kind === 'corrupt') return transport === 'serial'
  if (kind === 'disconnect') return transport === 'tcp_server'
  return true
}

/** 按 data_type 格式化显示值 */
export function formatValue(v: number | undefined, dt: DataType): string {
  if (v === undefined || Number.isNaN(v)) return '—'
  switch (dt) {
    case 'u16':
    case 'i16':
    case 'u32':
    case 'i32':
    case 'u64':
    case 'i64':
      return String(Math.round(v))
    case 'f32':
      return v.toFixed(2)
    case 'f64':
      return v.toFixed(3)
  }
}

/** 数据源参数的可读摘要（表格里用） */
export function sourceSummary(s: ValueSource): string {
  switch (s.kind) {
    case 'static':
      return `值=${s.value ?? 0}`
    case 'sine':
      return `周期=${s.period_s ?? 10}s 幅值=${s.amplitude ?? 1} 偏移=${s.offset ?? 0}`
    case 'ramp':
      return `步长=${s.step ?? 1} [${s.min ?? 0}, ${s.max ?? 100}]`
    case 'random_walk':
      return `步长±${s.step ?? 1} [${s.min ?? 0}, ${s.max ?? 100}]`
    case 'swing':
      return `[${s.min ?? 0}, ${s.max ?? 100}] 周期=${s.period_s ?? 20}s`
  }
}

// ---------- CAN 帧工具（与 Rust 端 can::CanFrame 行为同构，mock 用同一份） ----------

/** CAN 帧（发送面板 / 手动发送命令） */
export interface CanFrame {
  frame_id: number
  extended: boolean
  remote: boolean
  data: number[]
}

/** 帧十六进制字符串 → 字节数组（容错空格/冒号/大小写；长度奇数或空返回 null） */
export function parseHexBytes(s: string): number[] | null {
  const cleaned = s.replace(/[^0-9a-fA-F]/g, '')
  if (cleaned.length === 0 || cleaned.length % 2 !== 0) return null
  const out: number[] = []
  for (let i = 0; i < cleaned.length; i += 2) out.push(parseInt(cleaned.slice(i, i + 2), 16))
  return out
}

/** 帧监视器 note：ID 按域宽补零 + DLC（+ 扩展/远程标注），与 Rust 端 CanFrame::note 一致 */
export function canFrameNote(f: CanFrame): string {
  const hex = (f.extended ? 8 : 3) as number
  let s = `ID=0x${f.frame_id.toString(16).toUpperCase().padStart(hex, '0')} DLC=${f.data.length}`
  if (f.remote) s += ' 远程帧'
  return s
}

/** 帧定义合法性（与 Rust 端 CanPeriodicFrame::validate 一致）；返回 null = 合法 */
export function validateCanFrameDef(frame_id: number, extended: boolean, remote: boolean, data: number[]): string | null {
  const max = extended ? 0x1fffffff : 0x7ff
  if (frame_id > max) return `帧 ID 0x${frame_id.toString(16).toUpperCase()} 超出${extended ? '扩展' : '标准'}域宽（最大 0x${max.toString(16).toUpperCase()}）`
  if (data.length > 8) return `数据域 ${data.length} 字节超长（CAN 经典帧最多 8）`
  if (remote && data.length > 0) return '远程帧不带数据域'
  return null
}
