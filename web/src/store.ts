import { computed, reactive } from 'vue'
import { api } from './api'
import { uiAlert, uiConfirm } from './ui-dialog'
import { areaGlobalAddr, isCanTransport, isMasterTransport } from './types'
import type { Area, CanAgg, CanNode, CanPeriodicFrame, Connection, ConnState, DevStat, Device, FrameRecord, PointRow, ProjectHandle, ProjectMeta, ProjectSpec, ScanProgress } from './types'
import type { RuntimePatch } from './api/contract'

const FRAME_LIMIT = 5000

export const store = reactive({
  project: null as ProjectSpec | null,
  /** 当前工程文件路径（null = 演示工程尚未落盘；保存时按工程名建文件） */
  currentPath: null as string | null,
  /** 工程目录（exe 旁 projects/）里的工程列表，切换用 */
  projects: [] as ProjectMeta[],
  selectedDeviceId: null as number | null,
  frames: [] as FrameRecord[],
  values: {} as Record<number, number>,
  /** 主机侧采集健康统计（device_id → stat） */
  stats: {} as Record<number, DevStat>,
  /** 主机连接链路状态（conn_id → 状态；替换式，连接停止后消失） */
  connStates: {} as Record<number, ConnState>,
  /** CAN 连接观测（conn_id → 聚合/帧率；替换式，连接停止后消失） */
  canRx: {} as Record<number, CanAgg>,
  /** 点位扫描进度（conn_id → 进度；替换式，扫描结束 = 空表） */
  scanProgress: {} as Record<number, ScanProgress>,
  paused: false,
  running: false,
  /** 逐连接运行状态（元素为运行中的连接 id） */
  runningConns: [] as number[],
  /** 在线设备/CAN 节点 id 列表（运行态，与工程配置的 enabled 解耦） */
  runningDevices: [] as number[],
  apiKind: 'mock' as 'mock' | 'tauri',
  /** 测试帧面板 */
  testPanel: {
    open: false,
    connId: null as number | null,
    unit: null as number | null,
  },
  /** CAN 发送面板（手动单帧直控） */
  canPanel: {
    open: false,
    connId: null as number | null,
  },
})

// ---------- 工程文件管理（切换 / 新建 / 保存） ----------

/** 上次保存（或加载）时的工程快照，用于未保存修改检测 */
let savedSnapshot = ''

/** 当前工程是否有未保存的修改 */
export function hasUnsavedChanges(): boolean {
  if (!store.project) return false
  return JSON.stringify(store.project) !== savedSnapshot
}

/** 当前工程在工程目录里的文件名（列表高亮用；非工程目录内的路径也取主干） */
export function currentFileName(): string | null {
  if (!store.currentPath) return null
  return store.currentPath.split(/[\\/]/).pop() ?? null
}

/** 应用一个工程句柄：替换当前工程，重置运行视图（旧连接/设备 id 已失效） */
export function applyProjectHandle(h: ProjectHandle) {
  sanitizeIds(h.spec)
  store.project = h.spec
  store.currentPath = h.path
  savedSnapshot = JSON.stringify(h.spec)
  store.frames.length = 0
  store.values = {}
  store.stats = {}
  store.connStates = {}
  store.canRx = {}
  store.scanProgress = {}
  store.running = false
  store.runningConns = []
  store.runningDevices = []
  store.paused = false
  store.selectedDeviceId = firstSelectableId(h.spec)
  if (store.selectedDeviceId != null) api.selectDevice(store.selectedDeviceId)
}

/** 工程里第一个可选中对象（设备或 CAN 节点；id 全局唯一） */
function firstSelectableId(p: ProjectSpec): number | null {
  for (const c of p.connections) {
    const dev = c.devices[0]?.id ?? c.can_nodes?.[0]?.id
    if (dev != null) return dev
  }
  return null
}

/** 加载时修复工程文件里的重复 id（历史版本发号器重启重置造成的撞号）：
 *  重复的连接 id 会让按 id 查找（addDevice/启停/后端 startConn）命中靠前的连接。
 *  运行态映射（选中/统计/帧）随后整体重置，改号无副作用；下次保存即落盘。 */
function sanitizeIds(spec: ProjectSpec): void {
  // 连接 id
  {
    const seen = new Set<number>()
    let next = Math.max(100000 - 1, ...spec.connections.map((c) => c.id)) + 1
    for (const c of spec.connections) {
      if (seen.has(c.id)) c.id = next++
      else seen.add(c.id)
    }
  }
  // 设备 / CAN 节点 id（共用空间）
  {
    const objects = spec.connections.flatMap((c) => [...c.devices, ...(c.can_nodes ?? [])])
    const seen = new Set<number>()
    let next = Math.max(200000 - 1, ...objects.map((o) => o.id)) + 1
    for (const o of objects) {
      if (seen.has(o.id)) o.id = next++
      else seen.add(o.id)
    }
  }
  // 点位 id（全工程唯一）
  {
    const points = spec.connections.flatMap((c) => c.devices.flatMap((d) => d.points))
    const seen = new Set<number>()
    let next = Math.max(100000 - 1, ...points.map((p) => p.id)) + 1
    for (const p of points) {
      if (seen.has(p.id)) p.id = next++
      else seen.add(p.id)
    }
  }
  // CAN 报文 id（全工程唯一）
  {
    const frames = spec.connections.flatMap((c) => (c.can_nodes ?? []).flatMap((n) => n.frames))
    const seen = new Set<number>()
    let next = Math.max(400000 - 1, ...frames.map((f) => f.id)) + 1
    for (const f of frames) {
      if (seen.has(f.id)) f.id = next++
      else seen.add(f.id)
    }
  }
}

/** 刷新工程目录列表（失败不阻塞：列表为空仍可编辑当前工程） */
export async function refreshProjects() {
  try {
    store.projects = await api.listProjects()
  } catch {
    // 演示/异常环境下列表不可用，忽略
  }
}

/** 切换/新建前的守卫：未保存修改确认 + 停止运行。返回 false = 用户放弃 */
async function confirmDiscard(): Promise<boolean> {
  const reasons: string[] = []
  if (hasUnsavedChanges()) reasons.push('当前工程有未保存的修改，切换后将丢失')
  if (store.running) reasons.push('运行中的模拟将全部停止')
  if (reasons.length > 0 && !(await uiConfirm(`${reasons.join('，')}。继续？`, '切换工程'))) return false
  if (store.running) await api.stop()
  return true
}

/** 切换到工程目录内的指定工程（file_name 来自 store.projects） */
export async function switchProject(fileName: string) {
  if (fileName === currentFileName() || !(await confirmDiscard())) return
  const h = await api.openProject(fileName)
  applyProjectHandle(h)
  await refreshProjects()
}

/** 新建空工程（未保存修改/运行守卫同切换） */
export async function createProject(name: string) {
  if (!(await confirmDiscard())) return
  const h = await api.newProject(name)
  applyProjectHandle(h)
  await refreshProjects()
}

/** 打开任意位置的工程文件（系统文件对话框选择；外部工程保存写回原文件）。
 *  返回 false = 用户取消或放弃 */
export async function openProjectFile(): Promise<boolean> {
  if (!(await confirmDiscard())) return false
  const h = await api.openProjectFile()
  if (!h) return false
  applyProjectHandle(h)
  await refreshProjects()
  return true
}

/** 保存当前工程到对应的项目文件；浏览器演示模式没有本地盘，额外下载一份 JSON */
export async function saveCurrentProject() {
  if (!store.project) return
  const h = await api.saveProject(store.currentPath ?? '', store.project)
  store.currentPath = h.path
  savedSnapshot = JSON.stringify(store.project)
  await refreshProjects()
  if (api.kind === 'mock') {
    const blob = new Blob([JSON.stringify(store.project, null, 2)], { type: 'application/json' })
    const a = document.createElement('a')
    a.href = URL.createObjectURL(blob)
    a.download = `${store.project.name || 'netlink'}.json`
    a.click()
    URL.revokeObjectURL(a.href)
  }
}

export function applyUpdate(patch: RuntimePatch) {
  if (patch.frames) {
    for (const f of patch.frames) store.frames.push(f)
    while (store.frames.length > FRAME_LIMIT) store.frames.shift()
  }
  if (patch.values) Object.assign(store.values, patch.values)
  if (patch.stats) Object.assign(store.stats, patch.stats)
  if (patch.running !== undefined) store.running = patch.running
  if (patch.running_conns) store.runningConns = patch.running_conns
  if (patch.running_devices) store.runningDevices = patch.running_devices
  if (patch.conn_states) store.connStates = patch.conn_states
  if (patch.can_rx) store.canRx = patch.can_rx
  if (patch.scan) store.scanProgress = patch.scan
  if (patch.paused !== undefined) store.paused = patch.paused
}

/** 连接是否在运行（逐连接启停） */
export function connRunning(connId: number): boolean {
  return store.runningConns.includes(connId)
}

/** 设备/CAN 节点是否在线运行（连接在运行且在 runningDevices 里；两者共用 id 空间） */
export function deviceRunning(deviceId: number): boolean {
  if (!store.runningDevices.includes(deviceId)) return false
  const conn = connOfId(deviceId)
  return !!conn && connRunning(conn.id)
}

/** 设备或 CAN 节点所属连接 */
export function connOfId(id: number): Connection | null {
  for (const conn of store.project?.connections ?? []) {
    if (conn.devices.some((d) => d.id === id) || (conn.can_nodes ?? []).some((n) => n.id === id)) return conn
  }
  return null
}

/** 某连接当前在线的对象 id（设备 + CAN 节点；编辑后重启连接时保留原运行面用） */
export function runningDevicesOfConn(connId: number): number[] {
  const conn = store.project?.connections.find((c) => c.id === connId)
  if (!conn) return []
  const ids = [...conn.devices.map((d) => d.id), ...(conn.can_nodes ?? []).map((n) => n.id)]
  return ids.filter((id) => store.runningDevices.includes(id))
}

/** 运行中的连接被结构性修改（编辑/增删设备点位）后：重启该连接让改动生效。
 *  keep：重启后要在线的设备集合，缺省保留当前在线设备 */
export async function restartConnIfRunning(connId: number, keep?: number[]) {
  if (!store.project || !connRunning(connId)) return
  const conn = store.project.connections.find((c) => c.id === connId)
  if (!conn) return
  const online = keep ?? runningDevicesOfConn(connId)
  await api.stopConn(connId)
  // 旧实例的端口/串口句柄释放是异步的，稍等再重启
  await new Promise((r) => setTimeout(r, 200))
  try {
    await api.startConn(store.project, connId, online)
  } catch (e) {
    await uiAlert(`连接「${conn.name}」重启失败（已停止）：\n${e instanceof Error ? e.message : String(e)}`)
  }
}

export function clearFrames() {
  store.frames.length = 0
}

/** 当前选中的设备（设备树点击切换） */
export const selectedDevice = computed<Device | null>(() => {
  const id = store.selectedDeviceId
  if (id == null) return null
  for (const conn of store.project?.connections ?? []) {
    const dev = conn.devices.find((d) => d.id === id)
    if (dev) return dev
  }
  return null
})

/** 当前选中的 CAN 节点（与设备共用 selectedDeviceId / id 空间） */
export const selectedCanNode = computed<CanNode | null>(() => {
  const id = store.selectedDeviceId
  if (id == null) return null
  for (const conn of store.project?.connections ?? []) {
    const node = (conn.can_nodes ?? []).find((n) => n.id === id)
    if (node) return node
  }
  return null
})

/** 选中对象（设备或节点）所属连接 */
export const selectedConnection = computed(() => {
  const id = store.selectedDeviceId
  if (id == null) return null
  return connOfId(id)
})

/** 选中设备是否属于主机连接（决定点表交互模式） */
export const isSelectedMaster = computed(() => {
  const kind = selectedConnection.value?.transport.kind
  return kind != null && isMasterTransport(kind)
})

/** 选中对象是否属于 CAN 连接（决定发送报文表/聚合视图形态） */
export const isSelectedCan = computed(() => {
  const kind = selectedConnection.value?.transport.kind
  return kind != null && isCanTransport(kind)
})

/** 打开测试帧面板（可预填目标连接与从机地址） */
export function openTestPanel(connId?: number, unit?: number) {
  if (connId != null) store.testPanel.connId = connId
  if (unit != null) store.testPanel.unit = unit
  store.testPanel.open = true
}

/** 打开 CAN 发送面板（可预填目标连接） */
export function openCanPanel(connId?: number) {
  if (connId != null) store.canPanel.connId = connId
  store.canPanel.open = true
}

/** 点位编辑后的写回（直接改 store 内对象，Vue 响应式生效） */
export function patchPoint(deviceId: number, pointId: number, changes: Partial<Device['points'][number]>) {
  const dev = store.project?.connections.flatMap((c) => c.devices).find((d) => d.id === deviceId)
  const pt = dev?.points.find((p) => p.id === pointId)
  if (pt) Object.assign(pt, changes)
}

export function addPoint(deviceId: number, point: Device['points'][number]) {
  const dev = store.project?.connections.flatMap((c) => c.devices).find((d) => d.id === deviceId)
  dev?.points.push(point)
}

// ---------- 点表导入 ----------

export type ImportMode = 'append' | 'replace'

export interface ImportOutcome {
  /** 实际加入的点位数 */
  added: number
  /** 因 (区域, 地址) 冲突被跳过的行数（含文件内部重复与既有点位重复） */
  skipped: number
}

/** 导入点表行到设备：分配全工程唯一 id；(区域, 地址) 冲突跳过。
 *  追加=与现有点位查重；替换=清空后导入（文件内部仍查重）。
 *  若设备所属连接在运行，重启该连接让改动生效 */
export async function importPoints(deviceId: number, rows: PointRow[], mode: ImportMode): Promise<ImportOutcome> {
  const dev = allDevices().find((d) => d.id === deviceId)
  if (!dev) throw new Error('设备不在当前工程里')
  const conn = (store.project?.connections ?? []).find((c) => c.devices.some((d) => d.id === deviceId))
  if (mode === 'replace') dev.points.length = 0
  const occupied = new Set(dev.points.map((p) => `${p.area}:${p.address}`))
  let added = 0
  let skipped = 0
  for (const r of rows) {
    const key = `${r.area}:${r.address}`
    if (occupied.has(key)) {
      skipped++
      continue
    }
    occupied.add(key)
    dev.points.push({ ...r, id: nextPointId() })
    added++
  }
  if (conn && connRunning(conn.id)) await restartConnIfRunning(conn.id)
  return { added, skipped }
}

/** 当前选中设备的点位 → 导出行（Point 减 id） */
export function exportRowsOfDevice(deviceId: number): PointRow[] {
  const dev = allDevices().find((d) => d.id === deviceId)
  return (dev?.points ?? []).map(({ id: _id, ...row }) => row)
}

// ---------- 扫描结果导入 ----------

/** 扫描预览表勾选的一个槽位 */
export interface ScanImportRow {
  area: Area
  address: number
  /** 扫描时读到的原始字值（入备注供对照说明书判断类型合并） */
  value: number
}

/** 扫描点位名：Modicon 风格全局地址（40011 / 30025 / 00013），行业习惯一眼识区 */
export function scanPointName(area: Area, address: number): string {
  return areaGlobalAddr(area, address)
}

/** 导入扫描勾选的槽位：每字一点 U16（位区按 0/1）；同区同地址已有点位跳过（复用 importPoints）。
 *  目标为 null 时先在该连接下新建设备 */
export async function importScanPoints(
  connId: number,
  targetDeviceId: number | null,
  unit: number,
  newDeviceName: string,
  rows: ScanImportRow[],
): Promise<ImportOutcome> {
  if (!store.project) throw new Error('没有打开的工程')
  const conn = store.project.connections.find((c) => c.id === connId)
  if (!conn) throw new Error('连接不在当前工程里')
  let deviceId = targetDeviceId
  if (deviceId == null) {
    const dev: Device = {
      id: nextDevId(),
      name: newDeviceName.trim() || `扫描发现 · 从机 ${unit}`,
      slave_id: unit,
      enabled: true,
      fault: { kind: 'normal' },
      poll_period_ms: 1000,
      points: [],
    }
    addDevice(conn.id, dev)
    deviceId = dev.id
  }
  const pointRows: PointRow[] = rows.map((r) => ({
    name: scanPointName(r.area, r.address),
    area: r.area,
    address: r.address,
    // 每字一点 U16：扫描原理上不知道相邻字是独立 U16 还是 F32/U32 的低/高字，
    // 类型留给用户对照说明书再改（原始值在备注里）
    data_type: 'u16',
    source: { kind: 'static', value: 0 },
    note: `扫描发现（原始值 ${r.value}）`,
  }))
  return importPoints(deviceId, pointRows, 'append')
}

export function removePoint(deviceId: number, pointId: number) {
  const dev = store.project?.connections.flatMap((c) => c.devices).find((d) => d.id === deviceId)
  if (!dev) return
  const idx = dev.points.findIndex((p) => p.id === pointId)
  if (idx >= 0) dev.points.splice(idx, 1)
}

// ---------- id 发号 ----------
// 发号取「当前工程同 id 空间的最大值 + 1」而非模块级计数器：计数器初值写死，
// 应用重启后重置，会与上个会话存盘的对象撞号（曾导致新建设备落到同 id 的旧连接下）。
// 各调用点都是「发号后立即入树」，下一次发号能看到刚分配的 id，不会连号。

/** 点位 id（全工程唯一） */
export function nextPointId(): number {
  const pts = (store.project?.connections ?? []).flatMap((c) => c.devices.flatMap((d) => d.points))
  return 1 + pts.reduce((m, p) => Math.max(m, p.id), 100000 - 1)
}

/** 连接 id */
export function nextConnId(): number {
  return 1 + (store.project?.connections ?? []).reduce((m, c) => Math.max(m, c.id), 100000 - 1)
}

/** 设备 id（设备与 CAN 节点共用选中/在线 id 空间，两边都参与取最大值） */
export function nextDevId(): number {
  return 1 + allSelectableObjects().reduce((m, o) => Math.max(m, o.id), 200000 - 1)
}

/** 全部连接的扁平设备列表 */
export function allDevices(): Device[] {
  return (store.project?.connections ?? []).flatMap((c) => c.devices)
}

export function addConnection(conn: Connection) {
  store.project?.connections.push(conn)
}

export function patchConnection(id: number, changes: Partial<Omit<Connection, 'id' | 'devices'>>) {
  const conn = store.project?.connections.find((c) => c.id === id)
  if (conn) Object.assign(conn, changes)
}

export function removeConnection(id: number) {
  const p = store.project
  if (!p) return
  // 删掉的连接里可能包含当前选中对象，选中回退到首个可用对象
  const removed = p.connections.find((c) => c.id === id)
  const selectedInRemoved =
    removed?.devices.some((d) => d.id === store.selectedDeviceId) ||
    (removed?.can_nodes ?? []).some((n) => n.id === store.selectedDeviceId) ||
    false
  const idx = p.connections.findIndex((c) => c.id === id)
  if (idx >= 0) p.connections.splice(idx, 1)
  if (selectedInRemoved) {
    store.selectedDeviceId = firstSelectableId(p)
  }
}

export function addDevice(connId: number, dev: Device) {
  store.project?.connections.find((c) => c.id === connId)?.devices.push(dev)
}

export function patchDevice(deviceId: number, changes: Partial<Omit<Device, 'id' | 'points'>>) {
  const dev = allDevices().find((d) => d.id === deviceId)
  if (dev) Object.assign(dev, changes)
}

export function removeDevice(deviceId: number) {
  for (const conn of store.project?.connections ?? []) {
    const idx = conn.devices.findIndex((d) => d.id === deviceId)
    if (idx >= 0) {
      conn.devices.splice(idx, 1)
      if (store.selectedDeviceId === deviceId) {
        store.selectedDeviceId = allDevices()[0]?.id ?? firstSelectableId(store.project!) ?? null
      }
      return
    }
  }
}

// ---------- CAN 节点 / 周期报文 CRUD ----------

/** CAN 节点 id（与设备共用 id 空间） */
export function nextNodeId(): number {
  return 1 + allSelectableObjects().reduce((m, o) => Math.max(m, o.id), 300000 - 1)
}

/** CAN 周期报文 id（全工程唯一） */
export function nextFrameId(): number {
  const frs = (store.project?.connections ?? []).flatMap((c) => (c.can_nodes ?? []).flatMap((n) => n.frames))
  return 1 + frs.reduce((m, f) => Math.max(f.id, m), 400000 - 1)
}

/** 设备 + CAN 节点（共用 id 空间的全部对象） */
function allSelectableObjects(): Array<{ id: number }> {
  return (store.project?.connections ?? []).flatMap((c) => [
    ...c.devices,
    ...(c.can_nodes ?? []),
  ])
}

export function addCanNode(connId: number, node: CanNode) {
  const conn = store.project?.connections.find((c) => c.id === connId)
  if (!conn) return
  conn.can_nodes ??= []
  conn.can_nodes.push(node)
}

export function patchCanNode(nodeId: number, changes: Partial<Omit<CanNode, 'id' | 'frames'>>) {
  const node = allCanNodes().find((n) => n.id === nodeId)
  if (node) Object.assign(node, changes)
}

export function removeCanNode(nodeId: number) {
  for (const conn of store.project?.connections ?? []) {
    const nodes = conn.can_nodes ?? []
    const idx = nodes.findIndex((n) => n.id === nodeId)
    if (idx >= 0) {
      nodes.splice(idx, 1)
      if (store.selectedDeviceId === nodeId) {
        store.selectedDeviceId = firstSelectableId(store.project!)
      }
      return
    }
  }
}

export function addFrame(nodeId: number, frame: CanPeriodicFrame) {
  allCanNodes().find((n) => n.id === nodeId)?.frames.push(frame)
}

export function patchFrame(nodeId: number, frameId: number, changes: Partial<Omit<CanPeriodicFrame, 'id'>>) {
  const node = allCanNodes().find((n) => n.id === nodeId)
  const fr = node?.frames.find((f) => f.id === frameId)
  if (fr) Object.assign(fr, changes)
}

export function removeFrame(nodeId: number, frameId: number) {
  const node = allCanNodes().find((n) => n.id === nodeId)
  if (!node) return
  const idx = node.frames.findIndex((f) => f.id === frameId)
  if (idx >= 0) node.frames.splice(idx, 1)
}

/** 全部连接的扁平 CAN 节点列表 */
export function allCanNodes(): CanNode[] {
  return (store.project?.connections ?? []).flatMap((c) => c.can_nodes ?? [])
}
