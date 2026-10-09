<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { store, selectedDevice, selectedConnection, isSelectedMaster, addPoint, removePoint, patchPoint, nextPointId, restartConnIfRunning, importPoints, exportRowsOfDevice } from '../store'
import type { ImportMode, ImportOutcome } from '../store'
import { api } from '../api'
import { uiAlert } from '../ui-dialog'
import {
  AREA_LABELS,
  AREA_BASE,
  DATA_TYPE_LABELS,
  SOURCE_LABELS,
  areaGlobalAddr,
  formatValue,
  sourceSummary,
  type Area,
  type DataType,
  type Point,
  type PointRow,
  type SheetRowError,
  type SourceKind,
  type ValueSource,
} from '../types'
import DrawerShell from './DrawerShell.vue'

const device = selectedDevice
const isMaster = isSelectedMaster

/** 选中设备所属连接是否在运行（逐连接启停） */
const connRunning = computed(() => {
  const conn = selectedConnection.value
  return !!conn && store.runningConns.includes(conn.id)
})

const vFocus = {
  mounted: (el: HTMLInputElement) => el.focus(),
}

// 主机模式：采集健康徽标
const stat = computed(() => (device.value ? store.stats[device.value.id] : undefined))

function fmtLastOk(ms: number): string {
  if (!ms) return '从未'
  const d = new Date(ms)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

// 主机模式：在线值直写
const editingPointId = ref<number | null>(null)
const editingValue = ref(0)
const writeError = ref('')

/** Modbus 只允许写线圈与保持寄存器；输入区（离散输入/输入寄存器）协议只读 */
function pointWritable(p: Point): boolean {
  return p.area === 'coil' || p.area === 'holding_register'
}

function beginWrite(row: Point) {
  if (!device.value || !isMaster.value || !connRunning.value || !pointWritable(row)) return
  writeError.value = ''
  editingPointId.value = row.id
  const v = store.values[row.id] ?? 0
  // 线圈为 0/1 开关量，编辑器里直接给出归一化值
  editingValue.value = row.area === 'coil' ? (v >= 0.5 ? 1 : 0) : v
}

/** 当前值单元格悬浮提示：写入口 / 只读区 / 未运行分别说明 */
function valueCellTitle(p: Point): string {
  if (!isMaster.value) return '仿真实时值'
  if (!connRunning.value) return '连接未启动，无实时值'
  if (!pointWritable(p)) return '只读区：Modbus 输入区（FC02/04）不可写，仅线圈/保持寄存器可写'
  if (p.area === 'coil') return '点击写值下发（线圈为 0/1 开关量：0=分，≥0.5 一律按合=1，FC05）'
  return '点击写值下发（自动选 FC06/FC10）'
}

async function commitWrite(row: Point) {
  if (editingPointId.value !== row.id) return
  const dev = device.value
  editingPointId.value = null
  if (!dev) return
  // 线圈是开关量：输入值按 Modbus 语义归一化为 0/1 再下发（≥0.5 = 合）
  const value = row.area === 'coil' ? (editingValue.value >= 0.5 ? 1 : 0) : editingValue.value
  const old = store.values[row.id]
  if (old !== undefined && Math.abs(old - value) < 1e-9) return
  try {
    await api.writePoint(dev.id, row.id, value)
    store.values[row.id] = value
    writeError.value = ''
  } catch (e) {
    writeError.value = `写入失败：${String(e)}`
  }
}

function cancelWrite() {
  editingPointId.value = null
}

const areaStat = computed(() => {
  const stat: Record<string, number> = {}
  for (const p of device.value?.points ?? []) stat[p.area] = (stat[p.area] ?? 0) + 1
  return stat
})

// ---------- 点位编辑抽屉 ----------
interface EditState {
  pointId: number | null
  name: string
  area: Area
  address: number
  dataType: DataType
  sourceKind: SourceKind
  params: Required<Pick<ValueSource, 'value' | 'period_s' | 'amplitude' | 'offset' | 'step' | 'min' | 'max'>>
  note: string
}

const emptyParams = () => ({ value: 0, period_s: 10, amplitude: 1, offset: 0, step: 1, min: 0, max: 100 })

const edit = reactive<EditState>({
  pointId: null,
  name: '',
  area: 'input_register',
  address: 0,
  dataType: 'f32',
  sourceKind: 'sine',
  params: emptyParams(),
  note: '',
})

const editorOpen = ref(false)

function openEditor(p: Point | null) {
  if (p) {
    edit.pointId = p.id
    edit.name = p.name
    edit.area = p.area
    edit.address = p.address
    edit.dataType = p.data_type
    edit.sourceKind = p.source.kind
    edit.params = { ...emptyParams(), ...p.source } as EditState['params']
    edit.note = p.note ?? ''
  } else {
    edit.pointId = null
    edit.name = '新点位'
    edit.area = 'input_register'
    edit.address = (device.value?.points.length ?? 0)
    edit.dataType = 'f32'
    edit.sourceKind = 'sine'
    edit.params = emptyParams()
    edit.note = ''
  }
  editorOpen.value = true
}

function applySource(): ValueSource {
  const k = edit.sourceKind
  const p = edit.params
  const src: ValueSource = { kind: k }
  if (k === 'static') src.value = p.value
  if (k === 'sine' || k === 'swing') {
    src.period_s = p.period_s
    if (k === 'sine') {
      src.amplitude = p.amplitude
      src.offset = p.offset
    } else {
      src.min = p.min
      src.max = p.max
    }
  }
  if (k === 'ramp' || k === 'random_walk') {
    src.step = p.step
    src.min = p.min
    src.max = p.max
  }
  return src
}

async function saveEditor() {
  const dev = device.value
  if (!dev) return
  const base = {
    name: edit.name.trim() || '未命名',
    area: edit.area,
    address: Math.max(0, Math.min(65535, Math.round(edit.address))),
    data_type: edit.dataType,
    source: applySource(),
    note: edit.note.trim() || undefined,
  }
  if (edit.pointId != null) {
    patchPoint(dev.id, edit.pointId, base)
  } else {
    addPoint(dev.id, { id: nextPointId(), ...base })
  }
  editorOpen.value = false
  // 点位是运行时快照，重启所属连接让改动生效
  if (selectedConnection.value) await restartConnIfRunning(selectedConnection.value.id)
}

async function delPoint(p: Point) {
  const dev = device.value
  if (!dev) return
  removePoint(dev.id, p.id)
  if (selectedConnection.value) await restartConnIfRunning(selectedConnection.value.id)
}

// 切换设备时若编辑抽屉开着则关掉
watch(
  () => store.selectedDeviceId,
  () => {
    editorOpen.value = false
  },
)

const areaOptions = (Object.keys(AREA_LABELS) as Area[]).map((k) => ({
  label: `${AREA_LABELS[k]} (${String(AREA_BASE[k]).padStart(5, '0')})`,
  value: k,
}))
const dataTypeOptions = (Object.keys(DATA_TYPE_LABELS) as DataType[]).map((k) => ({ label: DATA_TYPE_LABELS[k], value: k }))
const sourceOptions = (Object.keys(SOURCE_LABELS) as SourceKind[]).map((k) => ({ label: SOURCE_LABELS[k], value: k }))

// ---------- 点表导入导出（xlsx / CSV） ----------

const exporting = ref(false)
const sheetTip = ref('')

/** 选文件并解析 → 预览抽屉（确认后才真正写入点表） */
async function onImport() {
  if (!device.value) return
  try {
    const sheet = await api.readPointSheet()
    if (!sheet) return // 用户取消
    if (sheet.rows.length === 0 && sheet.errors.length === 0) {
      await uiAlert('文件里没有点位数据（首行是表头？列格式见导出文件）')
      return
    }
    importing.rows = sheet.rows
    importing.errors = sheet.errors
    importing.mode = 'append'
    importing.busy = false
    importing.open = true
  } catch (e) {
    await uiAlert(`读取点表失败：\n${e instanceof Error ? e.message : String(e)}`)
  }
}

const importing = reactive({
  open: false,
  rows: [] as PointRow[],
  errors: [] as SheetRowError[],
  mode: 'append' as ImportMode,
  busy: false,
})

async function confirmImport() {
  const dev = device.value
  if (!dev || importing.busy || importing.rows.length === 0) return
  importing.busy = true
  try {
    const outcome: ImportOutcome = await importPoints(dev.id, importing.rows, importing.mode)
    importing.open = false
    sheetTip.value = `✓ 已导入 ${outcome.added} 个点位${outcome.skipped ? `（跳过 ${outcome.skipped} 个冲突）` : ''}`
    setTimeout(() => (sheetTip.value = ''), 4000)
  } catch (e) {
    await uiAlert(`导入失败：\n${e instanceof Error ? e.message : String(e)}`)
  } finally {
    importing.busy = false
  }
}

async function onExport() {
  const dev = device.value
  if (!dev || exporting.value) return
  if (dev.points.length === 0) {
    await uiAlert('该设备没有点位可导出')
    return
  }
  exporting.value = true
  try {
    const written = await api.writePointSheet(exportRowsOfDevice(dev.id), `${dev.name}-点表`)
    if (written) {
      sheetTip.value = `✓ 已导出（${written.split(/[\\/]/).pop()}）`
      setTimeout(() => (sheetTip.value = ''), 4000)
    }
  } catch (e) {
    await uiAlert(`导出失败：\n${e instanceof Error ? e.message : String(e)}`)
  } finally {
    exporting.value = false
  }
}
</script>

<template>
  <div class="panel">
    <div class="panel-head">
      <span class="panel-title">点表</span>
      <template v-if="device">
        <span class="chip mono">从机 {{ device.slave_id }}</span>
        <span class="dev-name">{{ device.name }}</span>
        <span v-for="(n, a) in areaStat" :key="a" class="chip">{{ AREA_LABELS[a as Area] }} × {{ n }}</span>
        <template v-if="isMaster">
          <span v-if="connRunning" class="chip" title="主机模式：当前值列带 ✎ 的点位（线圈/保持寄存器）可点击直接写值下发；输入区只读">✎ 值列可写</span>
          <span class="chip" :class="{ okc: stat && Date.now() - stat.last_ok_ms < (device.poll_period_ms ?? 1000) * 3 + 1500, bad: stat && Date.now() - stat.last_ok_ms >= (device.poll_period_ms ?? 1000) * 3 + 1500 }" title="采集健康">● 采集</span>
          <span class="chip mono" title="请求超时累计">超时 {{ stat?.timeout_count ?? 0 }}</span>
          <span class="chip mono" title="失败累计">失败 {{ stat?.fail_count ?? 0 }}</span>
          <span class="chip mono" :title="`轮询周期 ${device.poll_period_ms ?? 1000}ms`">{{ fmtLastOk(stat?.last_ok_ms ?? 0) }}</span>
        </template>
        <span v-else-if="device.fault.kind !== 'normal'" class="chip fault">故障：{{ device.fault.kind === 'delay' ? `延迟 ${device.fault.delay_ms}ms` : device.fault.kind === 'timeout' ? '超时' : '异常码' }}</span>
        <span v-if="writeError" class="chip bad" :title="writeError">⚠ 写入失败</span>
      </template>
      <div class="spacer" />
      <span v-if="sheetTip" class="chip okc" :title="sheetTip">{{ sheetTip }}</span>
      <button :disabled="!device" title="从 xlsx / CSV 文件导入点位（先预览再写入）" @click="onImport">导入</button>
      <button :disabled="!device || exporting" title="导出该设备点表为 xlsx / CSV" @click="onExport">导出</button>
      <button class="primary" :disabled="!device" @click="openEditor(null)">＋ 新建点位</button>
    </div>

    <div class="panel-body table-wrap">
      <vxe-table
        v-if="device"
        :data="device.points"
        height="auto"
        :row-config="{ keyField: 'id', isCurrent: true }"
        :column-config="{ resizable: true }"
        show-overflow="tooltip"
        border="inner"
        :scroll-y="{ enabled: true, gt: 50 }"
      >
        <vxe-column type="seq" title="#" width="46" />
        <vxe-column field="name" title="名称" min-width="150">
          <template #default="{ row }">
            <span class="pt-name">{{ row.name }}</span>
          </template>
        </vxe-column>
        <vxe-column field="area" title="区域" width="104">
          <template #default="{ row }">{{ AREA_LABELS[row.area as Area] }}</template>
        </vxe-column>
        <vxe-column field="address" title="地址" width="130" align="left">
          <template #default="{ row }">
            <span class="mono">{{ row.address }}</span>
            <span class="mono dim">({{ areaGlobalAddr(row.area as Area, row.address) }})</span>
          </template>
        </vxe-column>
        <vxe-column field="data_type" title="类型" width="64" align="center">
          <template #default="{ row }">
            <span class="mono">{{ row.data_type }}</span>
          </template>
        </vxe-column>
        <vxe-column width="124" align="right">
          <template #header>
            <span
              :title="isMaster
                ? '主机模式：带 ✎ 的值可点击直接写值下发（仅线圈/保持寄存器；输入区只读）'
                : '仿真源实时值'"
            >当前值{{ isMaster ? ' ✎' : '' }}</span>
          </template>
          <template #default="{ row }">
            <!-- 主机模式：可写点（线圈/保持寄存器）点击直接编辑下发 -->
            <input
              v-if="isMaster && editingPointId === row.id"
              v-focus
              v-model.number="editingValue"
              class="cell-input mono"
              type="number"
              @keyup.enter="commitWrite(row as Point)"
              @keyup.esc="cancelWrite"
              @blur="commitWrite(row as Point)"
            />
            <span
              v-else
              class="live mono"
              :class="{
                stale: !connRunning,
                editable: isMaster && connRunning && pointWritable(row as Point),
                ro: isMaster && connRunning && !pointWritable(row as Point),
              }"
              :title="valueCellTitle(row as Point)"
              @click="beginWrite(row as Point)"
            >{{ formatValue(store.values[row.id], row.data_type as DataType) }}<span
              v-if="isMaster && connRunning && pointWritable(row as Point)"
              class="wr"
            > ✎</span></span>
          </template>
        </vxe-column>
        <vxe-column title="数据源" width="92">
          <template #default="{ row }">{{ SOURCE_LABELS[row.source.kind as SourceKind] }}</template>
        </vxe-column>
        <vxe-column title="参数" min-width="220">
          <template #default="{ row }">
            <span class="mono dim">{{ sourceSummary(row.source as ValueSource) }}</span>
          </template>
        </vxe-column>
        <vxe-column field="note" title="备注" min-width="120">
          <template #default="{ row }">
            <span class="dim">{{ row.note || '' }}</span>
          </template>
        </vxe-column>
        <vxe-column title="操作" width="110" fixed="right">
          <template #default="{ row }">
            <a class="op" @click="openEditor(row as Point)">编辑</a>
            <a class="op danger" @click="delPoint(row as Point)">删除</a>
          </template>
        </vxe-column>
      </vxe-table>
      <div v-else class="empty">在左侧设备树选择一个设备</div>
    </div>

    <!-- 点位编辑抽屉 -->
    <DrawerShell v-if="editorOpen" :title="edit.pointId ? '编辑点位' : '新建点位'" @close="editorOpen = false">
      <label class="f-row">
        <span class="f-lab">名称</span>
        <input v-model="edit.name" placeholder="点位名称" />
      </label>
      <label class="f-row">
        <span class="f-lab">区域</span>
        <select v-model="edit.area">
          <option v-for="o in areaOptions" :key="o.value" :value="o.value">{{ o.label }}</option>
        </select>
      </label>
      <div class="f-row">
        <span class="f-lab">地址</span>
        <input v-model.number="edit.address" type="number" min="0" max="65535" />
        <span class="f-lab f-lab2">类型</span>
        <select v-model="edit.dataType">
          <option v-for="o in dataTypeOptions" :key="o.value" :value="o.value">{{ o.label }}</option>
        </select>
      </div>
      <label v-if="!isMaster" class="f-row">
        <span class="f-lab">数据源</span>
        <select v-model="edit.sourceKind">
          <option v-for="o in sourceOptions" :key="o.value" :value="o.value">{{ o.label }}</option>
        </select>
      </label>

      <div v-if="!isMaster" class="f-group">
        <template v-if="edit.sourceKind === 'static'">
          <label class="f-row"><span class="f-lab">静态值</span><input v-model.number="edit.params.value" type="number" step="0.1" /></label>
        </template>
        <template v-else-if="edit.sourceKind === 'sine'">
          <label class="f-row"><span class="f-lab">周期(s)</span><input v-model.number="edit.params.period_s" type="number" min="0.1" step="0.5" /></label>
          <label class="f-row"><span class="f-lab">幅值</span><input v-model.number="edit.params.amplitude" type="number" step="0.1" /></label>
          <label class="f-row"><span class="f-lab">偏移</span><input v-model.number="edit.params.offset" type="number" step="0.1" /></label>
        </template>
        <template v-else-if="edit.sourceKind === 'ramp' || edit.sourceKind === 'random_walk'">
          <label class="f-row"><span class="f-lab">步长</span><input v-model.number="edit.params.step" type="number" min="0" step="0.1" /></label>
          <label class="f-row"><span class="f-lab">下限</span><input v-model.number="edit.params.min" type="number" step="0.1" /></label>
          <label class="f-row"><span class="f-lab">上限</span><input v-model.number="edit.params.max" type="number" step="0.1" /></label>
        </template>
        <template v-else-if="edit.sourceKind === 'swing'">
          <label class="f-row"><span class="f-lab">下限</span><input v-model.number="edit.params.min" type="number" step="0.1" /></label>
          <label class="f-row"><span class="f-lab">上限</span><input v-model.number="edit.params.max" type="number" step="0.1" /></label>
          <label class="f-row"><span class="f-lab">周期(s)</span><input v-model.number="edit.params.period_s" type="number" min="0.1" step="0.5" /></label>
        </template>
      </div>

      <label class="f-row">
        <span class="f-lab">备注</span>
        <input v-model="edit.note" placeholder="可选" />
      </label>

      <template #footer>
        <span v-if="connRunning" class="f-hint">该连接运行中，保存后自动重启生效</span>
        <button @click="editorOpen = false">取消</button>
        <button class="primary" @click="saveEditor">保存</button>
      </template>
    </DrawerShell>

    <!-- 点表导入预览抽屉 -->
    <DrawerShell v-if="importing.open" title="导入点表" :width="460" @close="importing.open = false">
      <div class="imp-summary">
        <span>解析成功 <b class="okc">{{ importing.rows.length }}</b> 行</span>
        <span v-if="importing.errors.length" class="bad">错误 {{ importing.errors.length }} 行（导入时跳过）</span>
        <span v-if="importing.rows.length === 0" class="bad">没有可导入的行，请检查列格式（参照导出文件）</span>
      </div>

      <div v-if="importing.errors.length" class="imp-errors">
        <div v-for="e in importing.errors" :key="e.line" class="imp-err-line">第 {{ e.line }} 行：{{ e.reason }}</div>
      </div>

      <div class="f-row">
        <span class="f-lab">导入方式</span>
      </div>
      <label class="imp-mode">
        <input v-model="importing.mode" type="radio" value="append" />
        <span><b>追加</b>：保留现有点位，(区域, 地址) 重复的行自动跳过</span>
      </label>
      <label class="imp-mode">
        <input v-model="importing.mode" type="radio" value="replace" />
        <span><b>替换</b>：清空该设备现有点位后导入（文件内部重复行仍跳过）</span>
      </label>

      <template #footer>
        <span v-if="connRunning" class="f-hint">该连接运行中，导入后自动重启生效</span>
        <button @click="importing.open = false">取消</button>
        <button class="primary" :disabled="importing.rows.length === 0 || importing.busy" @click="confirmImport">
          导入 {{ importing.rows.length }} 个点位
        </button>
      </template>
    </DrawerShell>
  </div>
</template>

<style scoped>
.spacer {
  flex: 1;
}

.dev-name {
  font-weight: 600;
}

.table-wrap {
  display: flex;
}

/* flex 布局下 vxe-table 根元素会收缩到列宽之和，强制撑满容器 */
.table-wrap :deep(.vxe-table) {
  flex: 1;
  min-width: 0;
}

.empty {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--text-2);
}

.pt-name {
  font-weight: 500;
}

.live {
  color: var(--primary-dark);
  font-weight: 600;
}

.live.stale {
  color: var(--text-2);
  font-weight: 400;
}

.live.editable {
  cursor: pointer;
  border-bottom: 1px dashed var(--primary);
}

.live.editable:hover {
  background: #e0eff4;
}

/* 主机模式下输入区（只读）的实时值：正常显示但不可点击 */
.live.ro {
  color: var(--text-2);
  cursor: default;
}

/* 可写值旁的 ✎ 小标记 */
.wr {
  color: var(--primary);
  font-size: 10px;
}

.cell-input {
  width: 100%;
  padding: 1px 6px;
  font-size: 12px;
  text-align: right;
}

.chip.okc {
  color: var(--ok);
  border-color: var(--ok);
}

.chip.bad {
  color: var(--err);
  border-color: var(--err);
}

.dim {
  color: var(--text-2);
  font-size: 11.5px;
}

.op {
  color: var(--primary);
  cursor: pointer;
  margin-right: 10px;
  user-select: none;
}

.op.danger {
  color: var(--err);
}

.fault {
  color: var(--warn);
  border-color: var(--warn);
}

/* 点表导入预览 */
.imp-summary {
  display: flex;
  gap: 14px;
  margin-bottom: 10px;
  font-size: 12.5px;
}

.imp-summary .okc {
  color: var(--ok);
}

.imp-summary .bad {
  color: var(--err);
}

.imp-errors {
  max-height: 180px;
  overflow: auto;
  background: #fdf2f2;
  border: 1px solid #f5c2c2;
  border-radius: 6px;
  padding: 8px 10px;
  margin-bottom: 12px;
}

.imp-err-line {
  font-size: 11.5px;
  color: var(--err);
  line-height: 1.7;
}

.imp-mode {
  display: flex;
  align-items: baseline;
  gap: 6px;
  padding: 6px 4px;
  cursor: pointer;
  font-size: 12.5px;
}

.imp-mode input {
  margin: 0;
}
</style>
