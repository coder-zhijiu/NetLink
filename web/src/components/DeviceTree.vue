<script setup lang="ts">
import { computed, reactive, ref } from 'vue'
import { api } from '../api'
import {
  store,
  addConnection,
  patchConnection,
  removeConnection,
  addDevice,
  patchDevice,
  removeDevice,
  nextConnId,
  nextDevId,
  nextPointId,
  connRunning,
  deviceRunning,
  restartConnIfRunning,
  runningDevicesOfConn,
} from '../store'
import type { Connection, Device, Transport, TransportKind, FaultKind, Parity, TestConnResult } from '../types'
import { FAULT_LABELS, TRANSPORT_LABELS, isMasterTransport } from '../types'
import { uiAlert, uiConfirm } from '../ui-dialog'
import DrawerShell from './DrawerShell.vue'
import { openTestPanel } from '../store'

const project = computed(() => store.project)

const allDevCount = computed(() => (store.project?.connections ?? []).reduce((n, c) => n + c.devices.length, 0))

function isMasterConn(conn: Connection): boolean {
  return isMasterTransport(conn.transport.kind)
}

function transportSummary(t: Transport): string {
  if (t.kind === 'tcp_server') return `TCP :${t.port ?? 502}`
  if (t.kind === 'tcp_client') return `→ ${t.host ?? '?'}:${t.port ?? 502}`
  return `${t.serial_port ?? 'COM?'} @${t.baud ?? 9600}`
}

/** 连接行悬浮提示：名称 + 传输类型 + 端点（取代行内徽章，避免挤压行） */
function connTooltip(conn: Connection): string {
  const lines = [conn.name, `${TRANSPORT_LABELS[conn.transport.kind]} · ${transportSummary(conn.transport)}`]
  if (isMasterConn(conn)) lines.push('主机连接：主动连接/轮询远端从机')
  return lines.join('\n')
}

/** 设备行悬浮提示：名称 + 从机地址 + 点数（ID 徽章移入提示） */
function devTooltip(dev: Device): string {
  const lines = [`${dev.name}（从机地址 ID ${dev.slave_id}）`, `${dev.points.length} 个点位`]
  if (dev.fault.kind !== 'normal') lines.push(`故障注入：${FAULT_LABELS[dev.fault.kind]}`)
  return lines.join('\n')
}

/** 主机设备采集健康：true = 最近 3 个周期内有成功 */
function devHealthy(dev: Device): boolean | null {
  if (store.selectedDeviceId == null) return null
  const conn = (store.project?.connections ?? []).find((c) => c.devices.some((d) => d.id === dev.id))
  if (!conn || !isMasterConn(conn)) return null
  const st = store.stats[dev.id]
  if (!st || st.last_ok_ms === 0) return false
  return Date.now() - st.last_ok_ms < (dev.poll_period_ms ?? 1000) * 3 + 1500
}

/** 设备状态点：连接未运行=灰；连接运行但设备未在线=琥珀；在线=绿（主机采集不健康=红） */
function devDotClass(dev: Device): Record<string, boolean> {
  const conn = connOfDevice(dev.id)
  if (!conn || !connRunning(conn.id)) return {}
  if (!deviceRunning(dev.id)) return { warn: true }
  const healthy = devHealthy(dev)
  if (healthy === false) return { bad: true }
  return { on: true }
}

function selectDevice(id: number) {
  store.selectedDeviceId = id
  api.selectDevice(id)
}

/** 找设备所属连接 */
function connOfDevice(devId: number): Connection | undefined {
  return (store.project?.connections ?? []).find((c) => c.devices.some((d) => d.id === devId))
}

// ---------- 单连接 / 单设备启停 ----------

/** 防连点（启停是异步命令） */
const switching = ref(false)

async function toggleConn(conn: Connection) {
  if (!store.project || switching.value) return
  switching.value = true
  try {
    if (connRunning(conn.id)) {
      await api.stopConn(conn.id)
    } else {
      await api.startConn(store.project, conn.id)
    }
  } catch (e) {
    await uiAlert(`连接「${conn.name}」启动失败：\n${e instanceof Error ? e.message : String(e)}`)
  } finally {
    switching.value = false
  }
}

async function toggleDev(dev: Device) {
  const conn = connOfDevice(dev.id)
  if (!conn || !store.project || switching.value) return
  switching.value = true
  try {
    if (deviceRunning(dev.id)) {
      // 在线停用该设备（连接保持运行，其他设备不受影响）
      await api.setDeviceEnabled(dev.id, false)
    } else if (connRunning(conn.id)) {
      // 连接已运行：在线启用该设备
      await api.setDeviceEnabled(dev.id, true)
    } else {
      // 连接未启动：启动连接但仅带起这一台设备（其余设备之后可单独 ▶）
      await api.startConn(store.project, conn.id, [dev.id])
    }
  } catch (e) {
    await uiAlert(`设备「${dev.name}」操作失败：\n${e instanceof Error ? e.message : String(e)}`)
  } finally {
    switching.value = false
  }
}

// ---------- 右键菜单 ----------
interface MenuItem {
  label: string
  action: () => void
  danger?: boolean
}

const menu = reactive({
  visible: false,
  x: 0,
  y: 0,
  title: '',
  items: [] as MenuItem[],
})

function openMenu(e: MouseEvent, title: string, items: MenuItem[]) {
  e.preventDefault()
  e.stopPropagation()
  const n = items.length
  menu.x = Math.min(e.clientX, window.innerWidth - 180)
  menu.y = Math.min(e.clientY, window.innerHeight - n * 32 - 60)
  menu.title = title
  menu.items = items
  menu.visible = true
}

function closeMenu() {
  menu.visible = false
}

function runMenu(item: MenuItem) {
  closeMenu()
  item.action()
}

function projMenu(e: MouseEvent) {
  openMenu(e, store.project?.name ?? '工程', [
    { label: '＋ 新建连接', action: () => openConnEditor(null) },
  ])
}

function connMenu(e: MouseEvent, conn: Connection) {
  openMenu(e, conn.name, [
    { label: '＋ 新建设备', action: () => openDevEditor(conn.id, null) },
    { label: connRunning(conn.id) ? '■ 停止连接' : '▶ 启动连接', action: () => toggleConn(conn) },
    { label: '编辑连接', action: () => openConnEditor(conn) },
    { label: '删除连接', action: () => delConn(conn), danger: true },
  ])
}

function devMenu(e: MouseEvent, connId: number, dev: Device) {
  const conn = store.project?.connections.find((c) => c.id === connId)
  const items: MenuItem[] = [
    { label: '编辑设备', action: () => openDevEditor(connId, dev) },
    { label: deviceRunning(dev.id) ? '■ 停止设备' : '▶ 启动设备', action: () => toggleDev(dev) },
    { label: '复制设备', action: () => duplicateDevice(connId, dev) },
    { label: '删除设备', action: () => delDev(dev), danger: true },
  ]
  if (conn && isMasterConn(conn)) {
    items.splice(2, 0, { label: '发送测试帧', action: () => openTestPanel(connId, dev.slave_id) })
  }
  openMenu(e, `${dev.name} (ID ${dev.slave_id})`, items)
}

/** 复制设备：同参数新设备，从机地址自动取本连接最大值 +1；连接运行中则重启使其生效 */
async function duplicateDevice(connId: number, dev: Device) {
  const conn = store.project?.connections.find((c) => c.id === connId)
  if (!conn) return
  const maxId = conn.devices.reduce((m, d) => Math.max(m, d.slave_id), 0)
  const copy: Device = {
    id: nextDevId(),
    name: `${dev.name} 副本`,
    slave_id: Math.min(247, maxId + 1),
    enabled: dev.enabled,
    fault: JSON.parse(JSON.stringify(dev.fault)),
    points: dev.points.map((p) => ({ ...JSON.parse(JSON.stringify(p)), id: nextPointId() })),
  }
  addDevice(connId, copy)
  selectDevice(copy.id)
  // 重启保留当前在线设备；副本按其配置的「启用」决定是否一并在线
  if (connRunning(connId)) {
    const keep = runningDevicesOfConn(connId)
    if (copy.enabled) keep.push(copy.id)
    await restartConnIfRunning(connId, keep)
  }
}

// ---------- 连接编辑 ----------
interface ConnEditState {
  id: number | null
  name: string
  kind: TransportKind
  listenAddr: string
  port: number
  host: string
  timeoutMs: number
  serialPort: string
  baud: number
  parity: Parity
  stopBits: number
}

const connEdit = reactive<ConnEditState>({
  id: null,
  name: '新连接',
  kind: 'tcp_server',
  listenAddr: '0.0.0.0',
  port: 502,
  host: '127.0.0.1',
  timeoutMs: 1000,
  serialPort: 'COM3',
  baud: 9600,
  parity: 'none',
  stopBits: 1,
})

const connEditorOpen = ref(false)
const serialPorts = ref<string[]>([])

/** 连接编辑器检测结果（「测试连接 / 检查端口 / 检查串口」） */
const connTest = reactive<{ busy: boolean; result: TestConnResult | null }>({
  busy: false,
  result: null,
})

/** 检测按钮语义随传输方式变化：TCP 主机=测试连接；从机=检查端口；串口=检查串口占用 */
const connTestLabel = computed(() => {
  if (connEdit.kind === 'tcp_client') return '测试连接'
  if (connEdit.kind === 'tcp_server') return '检查端口'
  return '检查串口'
})

async function testConnEditor() {
  connTest.busy = true
  connTest.result = null
  try {
    connTest.result = await api.testConnection(applyTransport())
  } catch (e) {
    connTest.result = { ok: false, message: e instanceof Error ? e.message : String(e) }
  } finally {
    connTest.busy = false
  }
}

function openConnEditor(c: Connection | null) {
  if (c) {
    connEdit.id = c.id
    connEdit.name = c.name
    connEdit.kind = c.transport.kind
    if (c.transport.kind === 'tcp_server') {
      connEdit.listenAddr = c.transport.listen_addr ?? '0.0.0.0'
      connEdit.port = c.transport.port ?? 502
    } else if (c.transport.kind === 'tcp_client') {
      connEdit.host = c.transport.host ?? '127.0.0.1'
      connEdit.port = c.transport.port ?? 502
      connEdit.timeoutMs = c.transport.timeout_ms ?? 1000
    } else {
      // serial / serial_master 共用串口字段；serial_master 另有超时
      connEdit.serialPort = c.transport.serial_port ?? 'COM3'
      connEdit.baud = c.transport.baud ?? 9600
      connEdit.parity = c.transport.parity ?? 'none'
      connEdit.stopBits = c.transport.stop_bits ?? 1
      if (c.transport.kind === 'serial_master') {
        connEdit.timeoutMs = c.transport.timeout_ms ?? 1000
      }
    }
  } else {
    connEdit.id = null
    connEdit.name = `新连接 ${(store.project?.connections.length ?? 0) + 1}`
    connEdit.kind = 'tcp_server'
    connEdit.listenAddr = '0.0.0.0'
    connEdit.port = 5020
    connEdit.host = '127.0.0.1'
    connEdit.timeoutMs = 1000
    connEdit.serialPort = 'COM3'
    connEdit.baud = 9600
    connEdit.parity = 'none'
    connEdit.stopBits = 1
  }
  connTest.result = null
  connEditorOpen.value = true
  void refreshSerialPorts()
}

async function refreshSerialPorts() {
  try {
    serialPorts.value = await api.listSerialPorts()
  } catch {
    serialPorts.value = []
  }
}

function applyTransport(): Transport {
  if (connEdit.kind === 'tcp_server') {
    return {
      kind: 'tcp_server',
      listen_addr: connEdit.listenAddr.trim() || '0.0.0.0',
      port: Math.max(1, Math.min(65535, Math.round(connEdit.port) || 502)),
    }
  }
  if (connEdit.kind === 'tcp_client') {
    return {
      kind: 'tcp_client',
      host: connEdit.host.trim() || '127.0.0.1',
      port: Math.max(1, Math.min(65535, Math.round(connEdit.port) || 502)),
      timeout_ms: Math.max(50, Math.round(connEdit.timeoutMs) || 1000),
    }
  }
  if (connEdit.kind === 'serial_master') {
    return {
      kind: 'serial_master',
      serial_port: connEdit.serialPort.trim() || 'COM3',
      baud: connEdit.baud,
      parity: connEdit.parity,
      stop_bits: connEdit.stopBits,
      timeout_ms: Math.max(50, Math.round(connEdit.timeoutMs) || 1000),
    }
  }
  return {
    kind: 'serial',
    serial_port: connEdit.serialPort.trim() || 'COM3',
    baud: connEdit.baud,
    parity: connEdit.parity,
    stop_bits: connEdit.stopBits,
  }
}

async function saveConnEditor() {
  if (!store.project) return
  const base = { name: connEdit.name.trim() || '未命名连接', transport: applyTransport() }
  const editId = connEdit.id
  if (editId != null) {
    patchConnection(editId, base)
  } else {
    addConnection({ id: nextConnId(), devices: [], ...base })
  }
  connEditorOpen.value = false
  // 运行中的连接改了传输参数：重启该连接让新配置生效（新建的连接不自动启动）
  if (editId != null) await restartConnIfRunning(editId)
}

async function delConn(conn: Connection) {
  const n = conn.devices.length
  const msg = n > 0 ? `删除连接「${conn.name}」及其 ${n} 台设备？` : `删除连接「${conn.name}」？`
  if (!(await uiConfirm(msg, '删除连接'))) return
  // 先停后删，避免删除后端口/串口仍被旧实例占用
  if (connRunning(conn.id)) {
    await api.stopConn(conn.id)
    store.runningConns = store.runningConns.filter((id) => id !== conn.id)
  }
  removeConnection(conn.id)
}

// ---------- 设备编辑 ----------
interface DevEditState {
  connId: number | null
  id: number | null
  name: string
  slaveId: number
  enabled: boolean
  faultKind: FaultKind
  delayMs: number
  exceptionCode: number
  pollPeriodMs: number
}

const devEdit = reactive<DevEditState>({
  connId: null,
  id: null,
  name: '',
  slaveId: 1,
  enabled: true,
  faultKind: 'normal',
  delayMs: 500,
  exceptionCode: 1,
  pollPeriodMs: 1000,
})

const devEditorOpen = ref(false)

/** 当前编辑的设备是否属于主机连接 */
const devEditIsMaster = computed(() => {
  const conn = store.project?.connections.find((c) => c.id === devEdit.connId)
  return conn != null && isMasterTransport(conn.transport.kind)
})

function openDevEditor(connId: number, d: Device | null) {
  if (d) {
    devEdit.connId = connId
    devEdit.id = d.id
    devEdit.name = d.name
    devEdit.slaveId = d.slave_id
    devEdit.enabled = d.enabled
    devEdit.faultKind = d.fault.kind
    devEdit.delayMs = d.fault.delay_ms ?? 500
    devEdit.exceptionCode = d.fault.exception_code ?? 1
    devEdit.pollPeriodMs = d.poll_period_ms ?? 1000
  } else {
    devEdit.connId = connId
    devEdit.id = null
    devEdit.name = '新装置'
    const conn = store.project?.connections.find((c) => c.id === connId)
    const maxId = conn?.devices.reduce((m, d) => Math.max(m, d.slave_id), 0) ?? 0
    devEdit.slaveId = Math.min(247, maxId + 1)
    devEdit.enabled = true
    devEdit.faultKind = 'normal'
    devEdit.delayMs = 500
    devEdit.exceptionCode = 1
    devEdit.pollPeriodMs = 1000
  }
  devEditorOpen.value = true
}

async function saveDevEditor() {
  const connId = devEdit.connId
  if (connId == null) return
  const fault = {
    kind: devEdit.faultKind,
    ...(devEdit.faultKind === 'delay' ? { delay_ms: Math.max(0, Math.round(devEdit.delayMs)) } : {}),
    ...(devEdit.faultKind === 'exception' ? { exception_code: Math.max(1, Math.min(255, Math.round(devEdit.exceptionCode))) } : {}),
  }
  const base = {
    name: devEdit.name.trim() || '未命名设备',
    slave_id: Math.max(1, Math.min(247, Math.round(devEdit.slaveId) || 1)),
    enabled: devEdit.enabled,
    fault,
    poll_period_ms: Math.max(50, Math.round(devEdit.pollPeriodMs) || 1000),
  }
  let savedId: number
  if (devEdit.id != null) {
    savedId = devEdit.id
    patchDevice(devEdit.id, base)
  } else {
    const dev: Device = { id: nextDevId(), points: [], ...base }
    savedId = dev.id
    addDevice(connId, dev)
    selectDevice(dev.id)
  }
  devEditorOpen.value = false
  // 从机地址/故障/周期等改动是运行时快照，重启所属连接使其生效；
  // 重启保留当前在线设备，保存的这台按其「启用」勾选决定是否在线
  if (connRunning(connId)) {
    const keep = runningDevicesOfConn(connId)
    const i = keep.indexOf(savedId)
    if (devEdit.enabled && i < 0) keep.push(savedId)
    if (!devEdit.enabled && i >= 0) keep.splice(i, 1)
    await restartConnIfRunning(connId, keep)
  }
}

async function delDev(dev: Device) {
  if (!(await uiConfirm(`删除设备「${dev.name}」（含 ${dev.points.length} 个点位）？`, '删除设备'))) return
  const conn = connOfDevice(dev.id)
  removeDevice(dev.id)
  if (conn) await restartConnIfRunning(conn.id)
}

const faultOptions = (Object.keys(FAULT_LABELS) as FaultKind[]).map((k) => ({ label: FAULT_LABELS[k], value: k }))
const parityOptions: Array<{ label: string; value: Parity }> = [
  { label: '无校验', value: 'none' },
  { label: '偶校验', value: 'even' },
  { label: '奇校验', value: 'odd' },
]
const baudOptions = [1200, 2400, 4800, 9600, 19200, 38400, 115200]
</script>

<template>
  <div class="panel">
    <div class="panel-head">
      <span class="panel-title">设备树</span>
      <span class="chip">{{ project?.connections.length ?? 0 }} 连接</span>
      <div class="spacer" />
      <button class="primary" :disabled="!project" @click="openConnEditor(null)">＋ 连接</button>
    </div>

    <div class="panel-body tree" @contextmenu="projMenu">
      <template v-if="project">
        <div class="node proj-node" @contextmenu="projMenu($event)">
          <span class="icon">📁</span>
          <span class="label">{{ project.name }}</span>
          <span class="chip">{{ allDevCount }} 设备</span>
        </div>

        <div v-for="conn in project.connections" :key="conn.id" class="group">
          <div class="node conn-node" :title="connTooltip(conn)" @contextmenu="connMenu($event, conn)">
            <span class="icon">{{ isMasterConn(conn) ? '🔌' : '🔀' }}</span>
            <span class="label">{{ conn.name }}</span>
            <button
              class="node-btn"
              :class="{ on: connRunning(conn.id) }"
              :disabled="switching"
              :title="connRunning(conn.id) ? '停止该连接（释放端口/串口）' : '启动该连接的模拟'"
              @click.stop="toggleConn(conn)"
            >
              {{ connRunning(conn.id) ? '■' : '▶' }}
            </button>
          </div>

          <div
            v-for="dev in conn.devices"
            :key="dev.id"
            class="node dev-node"
            :class="{ selected: dev.id === store.selectedDeviceId }"
            :title="devTooltip(dev)"
            @click="selectDevice(dev.id)"
            @contextmenu="devMenu($event, conn.id, dev)"
          >
            <span class="dot" :class="devDotClass(dev)" />
            <span class="label">{{ dev.name }}</span>
            <span v-if="dev.fault.kind !== 'normal' && !isMasterConn(conn)" class="chip fault" :title="FAULT_LABELS[dev.fault.kind]">故障</span>
            <span class="count">{{ dev.points.length }} 点</span>
            <button
              class="node-btn"
              :class="{ on: deviceRunning(dev.id) }"
              :disabled="switching"
              :title="deviceRunning(dev.id) ? '停用该设备（连接保持运行）' : '启动该设备（连接未运行时一并启动，且只带起这一台）'"
              @click.stop="toggleDev(dev)"
            >
              {{ deviceRunning(dev.id) ? '■' : '▶' }}
            </button>
          </div>

          <div v-if="conn.devices.length === 0" class="dev-empty" @contextmenu="connMenu($event, conn)">
            <span>无设备（右键新建）</span>
          </div>
        </div>

        <div v-if="project.connections.length === 0" class="empty">
          <span>暂无连接（右键新建）</span>
        </div>

        <div class="tree-hint">💡 点击 ▶/■ 单独启停连接或设备；右键节点有更多操作</div>
      </template>
      <div v-else class="empty">加载中…</div>
    </div>

    <!-- 右键菜单 -->
    <Teleport to="body">
      <div v-if="menu.visible" class="menu-mask" @click="closeMenu" @contextmenu.prevent="closeMenu" />
      <div v-if="menu.visible" class="ctx-menu" :style="{ left: menu.x + 'px', top: menu.y + 'px' }">
        <div class="ctx-title">{{ menu.title }}</div>
        <div
          v-for="(item, i) in menu.items"
          :key="i"
          class="ctx-item"
          :class="{ danger: item.danger }"
          @click="runMenu(item)"
        >
          {{ item.label }}
        </div>
      </div>
    </Teleport>

    <!-- 连接编辑抽屉 -->
    <DrawerShell v-if="connEditorOpen" :title="connEdit.id != null ? '编辑连接' : '新建连接'" @close="connEditorOpen = false">
      <label class="f-row">
        <span class="f-lab">名称</span>
        <input v-model="connEdit.name" placeholder="连接名称" />
      </label>
      <label class="f-row">
        <span class="f-lab">传输方式</span>
        <select v-model="connEdit.kind">
          <option value="tcp_server">{{ TRANSPORT_LABELS['tcp_server'] }}</option>
          <option value="tcp_client">{{ TRANSPORT_LABELS['tcp_client'] }}</option>
          <option value="serial">{{ TRANSPORT_LABELS['serial'] }}</option>
          <option value="serial_master">{{ TRANSPORT_LABELS['serial_master'] }}</option>
        </select>
      </label>

      <div v-if="connEdit.kind === 'tcp_server'" class="f-group">
        <label class="f-row">
          <span class="f-lab">监听地址</span>
          <input v-model="connEdit.listenAddr" placeholder="0.0.0.0" class="mono" />
        </label>
        <div class="f-row">
          <span class="f-lab">端口</span>
          <input v-model.number="connEdit.port" type="number" min="1" max="65535" class="mono" />
          <button class="mini" title="常用 Modbus TCP 端口" @click="connEdit.port = 502">502</button>
        </div>
        <p class="tip">模拟从机将在该端口监听，上位机（主站）连接此地址轮询。保存前可点「检查端口」确认未被占用。</p>
      </div>

      <div v-else-if="connEdit.kind === 'tcp_client'" class="f-group">
        <div class="f-row">
          <span class="f-lab">远端地址</span>
          <input v-model="connEdit.host" placeholder="127.0.0.1" class="mono" />
          <span class="f-lab f-lab2">端口</span>
          <input v-model.number="connEdit.port" type="number" min="1" max="65535" class="mono" style="flex: 0.6" />
        </div>
        <div class="f-row">
          <span class="f-lab">超时(ms)</span>
          <input v-model.number="connEdit.timeoutMs" type="number" min="50" step="50" class="mono" />
          <span class="tip-inline">请求超时+重试2次；断线自动重连</span>
        </div>
        <p class="tip">主机连接：主动连接远端从机并按设备点表轮询采集。保存前可「测试连接」验证远端可达。</p>
      </div>

      <div v-else class="f-group">
        <div class="f-row">
          <span class="f-lab">串口</span>
          <input v-model="connEdit.serialPort" list="serial-ports" placeholder="COM3" class="mono" />
          <button class="mini" title="重新枚举本机串口" @click="refreshSerialPorts">枚举</button>
        </div>
        <datalist id="serial-ports">
          <option v-for="p in serialPorts" :key="p" :value="p" />
        </datalist>
        <div class="f-row">
          <span class="f-lab">波特率</span>
          <select v-model.number="connEdit.baud">
            <option v-for="b in baudOptions" :key="b" :value="b">{{ b }}</option>
          </select>
          <span class="f-lab f-lab2">校验</span>
          <select v-model="connEdit.parity">
            <option v-for="o in parityOptions" :key="o.value" :value="o.value">{{ o.label }}</option>
          </select>
        </div>
        <div class="f-row">
          <span class="f-lab">停止位</span>
          <select v-model.number="connEdit.stopBits">
            <option :value="1">1</option>
            <option :value="2">2</option>
          </select>
        </div>
        <div v-if="connEdit.kind === 'serial_master'" class="f-row">
          <span class="f-lab">超时(ms)</span>
          <input v-model.number="connEdit.timeoutMs" type="number" min="50" step="50" class="mono" />
          <span class="tip-inline">请求超时+重试2次</span>
        </div>
        <p v-if="connEdit.kind === 'serial_master'" class="tip">主机连接：独占该串口，按设备点表轮询远端 RTU 从机（地址+CRC16 帧）。保存前可「检查串口」确认未被占用。</p>
        <p v-else class="tip">本机无 485 口时，可装 com0com 虚拟出串口对联调。</p>
      </div>

      <p v-if="connTest.result" class="test-result" :class="{ ok: connTest.result.ok, bad: !connTest.result.ok }">
        {{ connTest.result.ok ? '✓' : '✗' }} {{ connTest.result.message }}
        <span v-if="connTest.result.ok && connTest.result.latency_ms != null" class="mono">（{{ connTest.result.latency_ms }}ms）</span>
      </p>

      <template #footer>
        <span v-if="connEdit.id != null && connRunning(connEdit.id)" class="f-hint">该连接运行中，保存后自动重启生效</span>
        <button @click="connEditorOpen = false">取消</button>
        <button :disabled="connTest.busy" @click="testConnEditor">
          {{ connTest.busy ? '检测中…' : connTestLabel }}
        </button>
        <button class="primary" @click="saveConnEditor">保存</button>
      </template>
    </DrawerShell>

    <!-- 设备编辑抽屉 -->
    <DrawerShell v-if="devEditorOpen" :title="devEdit.id != null ? '编辑设备' : '新建设备'" @close="devEditorOpen = false">
      <label class="f-row">
        <span class="f-lab">名称</span>
        <input v-model="devEdit.name" placeholder="设备名称" />
      </label>
      <div class="f-row">
        <span class="f-lab">从机地址</span>
        <input v-model.number="devEdit.slaveId" type="number" min="1" max="247" class="mono" />
        <span class="f-lab f-lab2" title="勾选=连接整体启动时默认参与模拟；运行期间以设备行的 ▶/■ 为准">启用</span>
        <input v-model="devEdit.enabled" type="checkbox" class="chk" />
      </div>

      <div v-if="devEditIsMaster" class="f-group">
        <div class="f-row">
          <span class="f-lab">轮询周期</span>
          <input v-model.number="devEdit.pollPeriodMs" type="number" min="50" step="50" class="mono" />
          <span class="tip-inline">ms；引擎自动合并连续地址段</span>
        </div>
        <p class="tip">采集健康（最近成功/超时/失败）显示在树与点表头部。</p>
      </div>

      <div v-else class="f-group">
        <label class="f-row">
          <span class="f-lab">故障注入</span>
          <select v-model="devEdit.faultKind">
            <option v-for="o in faultOptions" :key="o.value" :value="o.value">{{ o.label }}</option>
          </select>
        </label>
        <div v-if="devEdit.faultKind === 'delay'" class="f-row">
          <span class="f-lab">延迟(ms)</span>
          <input v-model.number="devEdit.delayMs" type="number" min="0" step="50" class="mono" />
        </div>
        <div v-else-if="devEdit.faultKind === 'exception'" class="f-row">
          <span class="f-lab">异常码</span>
          <input v-model.number="devEdit.exceptionCode" type="number" min="1" max="255" class="mono" />
          <span class="tip-inline">01 非法功能 / 02 非法地址 / 03 非法值 / 04 从站故障</span>
        </div>
        <p v-else class="tip">用于测上位机对坏设备的容错：超时不应答、延迟应答或固定异常码。</p>
      </div>

      <template #footer>
        <span v-if="devEdit.connId != null && connRunning(devEdit.connId)" class="f-hint">该连接运行中，保存后自动重启生效</span>
        <button @click="devEditorOpen = false">取消</button>
        <button class="primary" @click="saveDevEditor">保存</button>
      </template>
    </DrawerShell>
  </div>
</template>

<style scoped>
.spacer {
  flex: 1;
}

.tree {
  padding: 4px 0 4px;
}

.node {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 5px 10px;
  min-height: 30px;
  cursor: default;
  user-select: none;
}

.node .label {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.proj-node .label,
.conn-node .label {
  font-weight: 600;
}

.dev-node {
  cursor: pointer;
  padding-left: 26px;
  border-left: 2px solid transparent;
}

.dev-node:hover {
  background: #edf4f6;
}

.dev-node.selected {
  background: #e0eff4;
  border-left-color: var(--primary);
}

.dev-node.selected .label {
  color: var(--primary-dark);
  font-weight: 600;
}

.count {
  margin-left: auto;
  font-size: 11px;
  color: var(--text-2);
  flex: none;
}

/* 行内徽章不收缩不换行（空间不足时由名称列省略号兜底） */
.node .chip {
  flex: none;
  white-space: nowrap;
}

.fault {
  color: var(--warn);
  border-color: var(--warn);
  flex: none;
}

.dot.bad {
  background: var(--err);
}

.dev-empty,
.empty {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 10px;
  color: var(--text-2);
  font-size: 12px;
}

.dev-empty {
  padding-left: 30px;
  justify-content: flex-start;
}

.proj-node .chip {
  margin-left: auto;
  flex: none;
}

.tree-hint {
  margin-top: 8px;
  padding: 6px 10px;
  font-size: 11px;
  color: var(--text-2);
  border-top: 1px dashed var(--border);
}

.tip {
  margin: 2px 0 0;
  font-size: 11px;
  color: var(--text-2);
  line-height: 1.5;
}

.tip-inline {
  font-size: 11px;
  color: var(--text-2);
  white-space: nowrap;
}

/* 节点行内启停按钮（▶ / ■） */
.node-btn {
  flex: none;
  width: 22px;
  height: 22px;
  padding: 0;
  font-size: 10px;
  line-height: 1;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--text-2);
  background: #fff;
  cursor: pointer;
}

.node-btn:hover {
  color: var(--primary);
  border-color: var(--primary);
}

.node-btn.on {
  background: var(--err);
  border-color: var(--err);
  color: #fff;
}

.node-btn.on:hover {
  background: #b91c1c;
  border-color: #b91c1c;
  color: #fff;
}

.conn-node .node-btn {
  margin-left: auto;
}

/* 连接编辑器检测结果 */
.test-result {
  margin: 6px 0 0;
  padding: 8px 10px;
  font-size: 12px;
  line-height: 1.5;
  border-radius: 6px;
  border: 1px solid var(--border);
  background: #f4f8fa;
  word-break: break-all;
}

.test-result.ok {
  color: var(--ok);
  border-color: #bbe7c9;
  background: #f0fbf3;
}

.test-result.bad {
  color: var(--err);
  border-color: #f5c6c6;
  background: #fdf1f1;
}

.chk {
  flex: none;
}

button.mini {
  padding: 3px 8px;
  font-size: 11px;
  flex: none;
}

/* 右键菜单 */
.menu-mask {
  position: fixed;
  inset: 0;
  z-index: 190;
}

.ctx-menu {
  position: fixed;
  z-index: 200;
  min-width: 150px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 6px;
  box-shadow: 0 6px 24px rgba(0, 0, 0, 0.18);
  padding: 4px 0;
  user-select: none;
}

.ctx-title {
  padding: 4px 12px 6px;
  font-size: 11px;
  color: var(--text-2);
  border-bottom: 1px solid var(--border);
  margin-bottom: 4px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 240px;
}

.ctx-item {
  padding: 6px 12px;
  font-size: 12px;
  cursor: pointer;
  white-space: nowrap;
}

.ctx-item:hover {
  background: #e0eff4;
  color: var(--primary-dark);
}

.ctx-item.danger {
  color: var(--err);
}

.ctx-item.danger:hover {
  background: #fdeaea;
}
</style>
