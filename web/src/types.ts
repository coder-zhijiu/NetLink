// 与 netlink-core 的 model.rs 保持同构的领域类型（snake_case 字段名直接对齐 Rust serde）

export type Area = 'coil' | 'discrete_input' | 'holding_register' | 'input_register'
export type DataType = 'u16' | 'i16' | 'u32' | 'i32' | 'f32' | 'u64' | 'i64' | 'f64'
export type SourceKind = 'static' | 'sine' | 'ramp' | 'random_walk' | 'swing'
export type FaultKind = 'normal' | 'timeout' | 'delay' | 'exception'
export type TransportKind = 'tcp_server' | 'serial' | 'tcp_client' | 'serial_master'
export type Parity = 'none' | 'even' | 'odd'

/** 主机类传输（主动发起轮询/连接的角色） */
export function isMasterTransport(kind: TransportKind): boolean {
  return kind === 'tcp_client' || kind === 'serial_master'
}

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
}

export interface Connection {
  id: number
  name: string
  transport: Transport
  devices: Device[]
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
}

/** 主机侧采集健康统计 */
export interface DevStat {
  last_ok_ms: number
  timeout_count: number
  fail_count: number
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
