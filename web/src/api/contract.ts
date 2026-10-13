import type { ProjectSpec, ProjectMeta, ProjectHandle, PointRow, SheetParseResult, FrameRecord, ConnState, DevStat, Transport, TestConnResult, CanAgg, CanFrame, ScanParams, ScanProgress, ScanResult } from '../types'

/** 运行时状态推送（mock 与 tauri 两种实现共用） */
export interface RuntimePatch {
  frames?: FrameRecord[]
  values?: Record<number, number>
  /** 主机侧采集健康统计（device_id → stat） */
  stats?: Record<number, DevStat>
  running?: boolean
  /** 逐连接运行状态（连接 id 列表，空 = 全部停止） */
  running_conns?: number[]
  /** 在线设备/CAN 节点 id 列表（运行态，与工程配置的 enabled 解耦） */
  running_devices?: number[]
  /** 主机连接链路状态（连接 id → 状态，替换式：连接停止后从推送中消失） */
  conn_states?: Record<number, ConnState>
  /** CAN 连接观测（连接 id → 聚合/帧率，替换式） */
  can_rx?: Record<number, CanAgg>
  /** 点位扫描进度（连接 id → 进度，替换式：扫描结束后从推送中消失） */
  scan?: Record<number, ScanProgress>
  paused?: boolean
}

export interface NetLinkApi {
  kind: 'mock' | 'tauri'

  /** 启动恢复（无参）或按绝对路径加载；返回工程数据与落盘位置 */
  loadProject(path?: string): Promise<ProjectHandle>
  /** 保存工程；空路径 = 当前工程文件（未落盘的演示工程按工程名新建文件）。
   *  返回句柄：首次落盘会生成新路径，前端要更新 */
  saveProject(path: string, project: ProjectSpec): Promise<ProjectHandle>

  /** 工程目录（exe 旁 projects/）里的全部工程，按修改时间倒序 */
  listProjects(): Promise<ProjectMeta[]>
  /** 新建空工程并立即落盘（成为当前工程） */
  newProject(name: string): Promise<ProjectHandle>
  /** 切换到工程目录内的指定工程（file_name 来自 listProjects） */
  openProject(file_name: string): Promise<ProjectHandle>
  /** 打开任意位置的工程文件：Tauri=系统文件对话框 → 后端加载该路径；
   *  浏览器演示=file input 选文件解析。返回 null = 用户取消。
   *  外部工程保存时写回原文件 */
  openProjectFile(): Promise<ProjectHandle | null>

  /** 以当前工程启动全部连接（Tauri 模式把工程传给后端引擎） */
  start(project: ProjectSpec): Promise<void>
  stop(): Promise<void>
  setPaused(paused: boolean): Promise<void>

  /** 启动单条连接（端口占用等启动错误会抛出）。
   *  enabledDevices：传设备 id 数组时启动后仅这些设备在线（单台设备首启用）；不传按工程配置 */
  startConn(project: ProjectSpec, connId: number, enabledDevices?: number[] | null): Promise<void>
  /** 停止单条连接 */
  stopConn(connId: number): Promise<void>
  /** 在线启停一台设备（不改工程配置） */
  setDeviceEnabled(deviceId: number, enabled: boolean): Promise<void>

  /** 连接编辑器检测：主机=测试连接远端；从机=检查端口/串口占用 */
  testConnection(transport: Transport): Promise<TestConnResult>

  selectDevice(deviceId: number): void

  /** 枚举本机串口（连接编辑器用） */
  listSerialPorts(): Promise<string[]>

  /** 主机侧写点位（在线值直写） */
  writePoint(deviceId: number, pointId: number, value: number): Promise<void>

  /** 主机侧发送测试帧（PDU hex，不含 MBAP），返回应答 PDU hex */
  sendTestFrame(connId: number, unit: number, hex: string): Promise<string>

  /** 主机侧点位扫描（长任务）：进度经 onUpdate 的 scan 字段推送，结束返回完整结果。
   *  取消/中止也返回部分结果（aborted 字段标原因） */
  scanPoints(connId: number, params: ScanParams): Promise<ScanResult>
  /** 取消进行中的点位扫描 */
  scanCancel(connId: number): Promise<void>

  /** CAN 手动发送一帧（插队立即发出） */
  sendCanFrame(connId: number, frame: CanFrame): Promise<void>

  /** 选一个点表文件并解析（.xlsx/.csv）：Tauri=系统文件对话框→后端解析；
   *  浏览器演示=file input 读文本（仅 CSV）。返回 null = 用户取消 */
  readPointSheet(): Promise<SheetParseResult | null>
  /** 导出点表文件：Tauri=保存对话框（后缀决定 xlsx/csv）→后端写盘，返回写入路径；
   *  浏览器演示=触发 CSV 下载，返回文件名。返回 null = 用户取消 */
  writePointSheet(rows: PointRow[], defaultName: string): Promise<string | null>

  onUpdate(cb: (patch: RuntimePatch) => void): () => void
}
