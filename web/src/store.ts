import { computed, reactive } from 'vue'
import { api } from './api'
import { uiAlert, uiConfirm } from './ui-dialog'
import { isCanTransport, isMasterTransport } from './types'
import type { CanAgg, CanNode, CanPeriodicFrame, Connection, ConnState, DevStat, Device, FrameRecord, PointRow, ProjectHandle, ProjectMeta, ProjectSpec } from './types'
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
  store.project = h.spec
  store.currentPath = h.path
  savedSnapshot = JSON.stringify(h.spec)
  store.frames.length = 0
  store.values = {}
  store.stats = {}
  store.connStates = {}
  store.canRx = {}
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

export function removePoint(deviceId: number, pointId: number) {
  const dev = store.project?.connections.flatMap((c) => c.devices).find((d) => d.id === deviceId)
  if (!dev) return
  const idx = dev.points.findIndex((p) => p.id === pointId)
  if (idx >= 0) dev.points.splice(idx, 1)
}

let pointSeq = 100000
export function nextPointId() {
  return pointSeq++
}

let connSeq = 100000
export function nextConnId() {
  return connSeq++
}

let devSeq = 200000
export function nextDevId() {
  return devSeq++
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

let nodeSeq = 300000
export function nextNodeId() {
  return nodeSeq++
}

let frameSeq = 400000
export function nextFrameId() {
  return frameSeq++
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
