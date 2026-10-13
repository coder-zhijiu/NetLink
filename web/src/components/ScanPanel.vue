<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { store, selectedDevice, selectedConnection, importScanPoints, type ScanImportRow } from '../store'
import { api } from '../api'
import { uiAlert, uiConfirm } from '../ui-dialog'
import { AREA_LABELS, areaGlobalAddr, type Area, type ScanResult } from '../types'
import DrawerShell from './DrawerShell.vue'

const emit = defineEmits<{ close: []; imported: [added: number, skipped: number] }>()

const device = selectedDevice
const conn = selectedConnection

// ---------- 配置区 ----------

const unit = ref(1)
const areas = reactive<Record<Area, boolean>>({
  coil: true,
  discrete_input: true,
  holding_register: true,
  input_register: true,
})
const startAddr = ref(0)
const endAddr = ref(65535)

/** 面板打开时按当前设备预填从机地址 */
watch(
  () => store.selectedDeviceId,
  () => {
    if (device.value) unit.value = device.value.slave_id
  },
  { immediate: true },
)

const areaKeys = Object.keys(AREA_LABELS) as Area[]

// ---------- 运行 / 结果 ----------

type Phase = 'config' | 'running' | 'result'
const phase = ref<Phase>('config')
const result = ref<ScanResult | null>(null)
const starting = ref(false)

const progress = computed(() => (conn.value ? store.scanProgress[conn.value.id] : undefined))
const progressPct = computed(() => {
  const p = progress.value
  if (!p || !p.total) return 0
  return Math.min(100, Math.round((p.processed / p.total) * 100))
})

async function startScan() {
  const c = conn.value
  if (!c) return
  if (!store.runningConns.includes(c.id)) {
    await uiAlert('该主机连接未启动——扫描走主机连接的事务通道，请先在设备树上启动该连接')
    return
  }
  const checked = areaKeys.filter((a) => areas[a])
  if (checked.length === 0) {
    await uiAlert('至少勾选一个扫描区域')
    return
  }
  const s = Math.max(0, Math.min(65535, Math.round(startAddr.value) || 0))
  const e = Math.max(0, Math.min(65535, Math.round(endAddr.value) || 0))
  if (e < s) {
    await uiAlert('结束地址不能小于起始地址')
    return
  }
  startAddr.value = s
  endAddr.value = e
  phase.value = 'running'
  starting.value = true
  try {
    const r = await api.scanPoints(c.id, { unit: unit.value, areas: checked, start_addr: s, end_addr: e })
    result.value = r
    phase.value = 'result'
    buildRows(r)
  } catch (err) {
    phase.value = 'config'
    await uiAlert(`扫描失败：\n${err instanceof Error ? err.message : String(err)}`)
  } finally {
    starting.value = false
  }
}

async function stopScan() {
  const c = conn.value
  if (!c) return
  try {
    await api.scanCancel(c.id)
  } catch {
    // 扫描刚好结束：忽略
  }
}

function backToConfig() {
  phase.value = 'config'
  result.value = null
}

// ---------- 结果预览 ----------

interface PreviewRow extends ScanImportRow {
  key: string
  /** 当前设备已存在同区同地址点位（导入时跳过，不可勾选） */
  existing: boolean
  /** 所属可读区间序号（视觉分组） */
  rangeIdx: number
}

const rows = ref<PreviewRow[]>([])
const checked = ref<Set<string>>(new Set())

/** 展开结果为逐槽位行；已存在地址标灰禁选，其余默认勾选 */
function buildRows(r: ScanResult) {
  const out: PreviewRow[] = []
  const occupied = new Set((device.value?.points ?? []).map((p) => `${p.area}:${p.address}`))
  r.ranges.forEach((rg, idx) => {
    for (let i = 0; i < rg.qty; i++) {
      const address = rg.start + i
      const key = `${rg.area}:${address}`
      const existing = occupied.has(key)
      out.push({
        key,
        area: rg.area,
        address,
        value: rg.values[i] ?? 0,
        existing,
        rangeIdx: idx,
      })
    }
  })
  rows.value = out
  checked.value = new Set(out.filter((x) => !x.existing).map((x) => x.key))
}

const totalSlots = computed(() => rows.value.length)
const selectedCount = computed(() => checked.value.size)
const existingCount = computed(() => rows.value.filter((x) => x.existing).length)

function toggleRow(row: PreviewRow, on: boolean) {
  const s = checked.value
  if (on) s.add(row.key)
  else s.delete(row.key)
  checked.value = new Set(s) // 触发响应式
}

function selectAll() {
  checked.value = new Set(rows.value.filter((x) => !x.existing).map((x) => x.key))
}

function selectNone() {
  checked.value = new Set()
}

function fmtValue(v: number): string {
  return `${v} (0x${v.toString(16).toUpperCase().padStart(4, '0')})`
}

// ---------- 导入 ----------

const importTarget = ref<'current' | 'new'>('current')
const newDeviceName = ref('')
const importing = ref(false)

async function doImport() {
  const c = conn.value
  if (!c || !result.value || importing.value) return
  const sel: ScanImportRow[] = rows.value.filter((x) => checked.value.has(x.key))
  if (sel.length === 0) {
    await uiAlert('没有勾选任何地址')
    return
  }
  if (sel.length > 2000) {
    const ok = await uiConfirm(
      `一次导入 ${sel.length} 个点位会产生大量 U16 点位；建议只勾选需要的区间（可稍后删除）。仍要全部导入？`,
      '导入扫描点位',
    )
    if (!ok) return
  }
  if (importTarget.value === 'new' && !device.value) {
    await uiAlert('请先在设备树选择一个设备（用于确定目标连接）')
    return
  }
  importing.value = true
  try {
    const outcome = await importScanPoints(
      c.id,
      importTarget.value === 'current' ? device.value!.id : null,
      unit.value,
      newDeviceName.value,
      sel,
    )
    emit('imported', outcome.added, outcome.skipped)
    emit('close')
  } catch (e) {
    await uiAlert(`导入失败：\n${e instanceof Error ? e.message : String(e)}`)
  } finally {
    importing.value = false
  }
}
</script>

<template>
  <DrawerShell :title="phase === 'config' ? '扫描点位' : phase === 'running' ? '扫描中…' : '扫描结果'" :width="560" @close="emit('close')">
    <!-- 配置 -->
    <template v-if="phase === 'config'">
      <div class="f-row">
        <span class="f-lab">从机地址</span>
        <input v-model.number="unit" type="number" min="0" max="255" class="mono" />
        <span v-if="conn && !store.runningConns.includes(conn.id)" class="f-hint" style="margin: 0">连接未启动，扫描将失败</span>
      </div>

      <div class="f-row">
        <span class="f-lab">区域</span>
        <label v-for="a in areaKeys" :key="a" class="area-chk">
          <input v-model="areas[a]" type="checkbox" />
          {{ AREA_LABELS[a] }}
        </label>
      </div>

      <div class="f-row">
        <span class="f-lab">地址范围</span>
        <input v-model.number="startAddr" type="number" min="0" max="65535" class="mono" />
        <span class="range-sep">~</span>
        <input v-model.number="endAddr" type="number" min="0" max="65535" class="mono" />
      </div>

      <p class="tip">
        暴力探测可读地址：块读 + 异常码 02 二分定界；超时块自动探活（死设备会快速中止并提示）。
        扫描期间该连接的轮询自动暂停。全扫 65535 对慢速设备（尤其 RTU）可能很慢，可先收窄范围。
      </p>
      <p class="tip">
        扫描只能发现「哪些地址可读、原始值是多少」；相邻字是独立 U16 还是 F32/U32 的低/高字需要对照说明书人工合并（结果表展示每个字的原始值）。
      </p>
    </template>

    <!-- 运行 -->
    <template v-else-if="phase === 'running'">
      <div class="scan-progress">
        <div class="bar"><div class="fill" :style="{ width: progressPct + '%' }" /></div>
        <div class="pct mono">{{ progressPct }}%</div>
      </div>
        <div class="scan-stat">
        <span>{{ progress ? AREA_LABELS[progress.area ?? 'holding_register'] : '…' }}</span>
        <span class="mono">addr {{ progress?.cur_addr ?? 0 }}</span>
        <span>可读区间 <b>{{ progress?.ranges ?? 0 }}</b></span>
        <span v-if="progress?.suspicious" class="warn">存疑 {{ progress.suspicious }}</span>
      </div>
      <p class="tip">扫描中，再次点击「停止」返回部分结果；报文监视器默认隐藏扫描帧（顶部开关可开）。</p>
    </template>

    <!-- 结果 -->
    <template v-else-if="result">
      <div class="res-summary">
        <span>可读区间 <b class="okc">{{ result.ranges.length }}</b> 个 · <b class="okc">{{ totalSlots }}</b> 槽位</span>
        <span v-if="existingCount" class="dim">已存在 {{ existingCount }}（跳过）</span>
        <span v-if="result.suspicious.length" class="warn">存疑块 {{ result.suspicious.length }}</span>
        <span v-if="result.rejected.length" class="warn">拒读 {{ result.rejected.length }}</span>
      </div>
      <div v-if="result.aborted" class="res-aborted" :title="result.aborted">⚠ {{ result.aborted }}</div>

      <div v-if="result.suspicious.length" class="blocks">
        <div class="blocks-title">存疑（超时未读出，可缩小范围重扫）：</div>
        <div v-for="(b, i) in result.suspicious" :key="'s' + i" class="block-line">
          {{ AREA_LABELS[b.area] }} {{ b.start }}~{{ b.start + b.qty - 1 }}（{{ b.qty }} 槽位）· {{ b.reason }}
        </div>
      </div>
      <div v-if="result.rejected.length" class="blocks">
        <div class="blocks-title">拒读：</div>
        <div v-for="(b, i) in result.rejected" :key="'r' + i" class="block-line">
          {{ AREA_LABELS[b.area] }} {{ b.start }}~{{ b.start + b.qty - 1 }}（{{ b.qty }} 槽位）· {{ b.reason }}
        </div>
      </div>

      <template v-if="totalSlots > 0">
        <div class="sel-bar">
          <button class="mini" @click="selectAll">全选</button>
          <button class="mini" @click="selectNone">清空</button>
          <span class="dim">已勾选 <b>{{ selectedCount }}</b> / {{ totalSlots }}</span>
        </div>

        <div class="table-wrap">
          <vxe-table
            :data="rows"
            height="auto"
            :row-config="{ keyField: 'key' }"
            :row-class-name="({ row }: { row: PreviewRow }) => (row.rangeIdx % 2 === 1 ? 'rg-alt' : '') + (row.existing ? ' rg-exist' : '')"
            border="inner"
            show-overflow="tooltip"
            :scroll-y="{ enabled: true, gt: 60 }"
          >
            <vxe-column width="40" align="center">
              <template #default="{ row }">
                <input
                  type="checkbox"
                  :checked="checked.has(row.key)"
                  :disabled="row.existing"
                  @change="toggleRow(row as PreviewRow, ($event.target as HTMLInputElement).checked)"
                />
              </template>
            </vxe-column>
            <vxe-column field="area" title="区域" width="86">
              <template #default="{ row }">{{ AREA_LABELS[row.area as Area] }}</template>
            </vxe-column>
            <vxe-column field="address" title="地址" width="120" align="left">
              <template #default="{ row }">
                <span class="mono">{{ row.address }}</span>
                <span class="mono dim">({{ areaGlobalAddr(row.area as Area, row.address) }})</span>
              </template>
            </vxe-column>
            <vxe-column field="value" title="原始值" min-width="130" align="left">
              <template #default="{ row }">
                <span class="mono">{{ fmtValue(row.value) }}</span>
              </template>
            </vxe-column>
            <vxe-column title="" width="72">
              <template #default="{ row }">
                <span v-if="row.existing" class="dim chip-exist">已存在</span>
              </template>
            </vxe-column>
          </vxe-table>
        </div>

        <div class="f-row import-target">
          <span class="f-lab">导入到</span>
          <label class="tgt"><input v-model="importTarget" type="radio" value="current" />当前设备（同地址跳过）</label>
          <label class="tgt"><input v-model="importTarget" type="radio" value="new" />新建设备</label>
        </div>
        <div v-if="importTarget === 'new'" class="f-row">
          <span class="f-lab">设备名</span>
          <input v-model="newDeviceName" :placeholder="`扫描发现 · 从机 ${unit}`" />
        </div>
        <p class="tip">每个可读字生成一个 u16 点位（名称 = Modicon 全局地址，原始值在备注里）；类型合并请导入后在点表里改。</p>
      </template>
      <div v-else class="empty-res">没有发现可读地址。存疑/拒读信息见上方；可尝试收窄范围或确认从机地址。</div>
    </template>

    <template #footer>
      <template v-if="phase === 'config'">
        <span v-if="conn && store.runningConns.includes(conn.id) && device" class="f-hint">目标：{{ conn.name }} · 从机 {{ unit }}</span>
        <button class="primary" :disabled="starting" @click="startScan">开始扫描</button>
        <button @click="emit('close')">取消</button>
      </template>
      <template v-else-if="phase === 'running'">
        <button class="danger" @click="stopScan">停止</button>
      </template>
      <template v-else>
        <span class="f-hint">已勾选 {{ selectedCount }} 个地址</span>
        <button @click="backToConfig">重新扫描</button>
        <button class="primary" :disabled="importing || selectedCount === 0" @click="doImport">
          导入 {{ selectedCount }} 个点位
        </button>
      </template>
    </template>
  </DrawerShell>
</template>

<style scoped>
.tip {
  margin: 2px 0 10px;
  font-size: 11px;
  color: var(--text-2);
  line-height: 1.6;
}

.area-chk {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-right: 12px;
  font-size: 12.5px;
  cursor: pointer;
}

.range-sep {
  color: var(--text-2);
}

.scan-progress {
  display: flex;
  align-items: center;
  gap: 10px;
  margin: 8px 0 10px;
}

.bar {
  flex: 1;
  height: 10px;
  background: #e8eef1;
  border-radius: 5px;
  overflow: hidden;
}

.bar .fill {
  height: 100%;
  background: var(--primary);
  transition: width 0.2s;
}

.pct {
  width: 44px;
  text-align: right;
  font-size: 12px;
  color: var(--text-2);
}

.scan-stat {
  display: flex;
  gap: 16px;
  font-size: 12.5px;
  color: var(--text-2);
  margin-bottom: 6px;
}

.scan-stat b {
  color: var(--ok);
}

.warn {
  color: var(--warn);
}

.res-summary {
  display: flex;
  gap: 14px;
  font-size: 12.5px;
  margin-bottom: 8px;
}

.res-summary .okc {
  color: var(--ok);
}

.dim {
  color: var(--text-2);
}

.res-aborted {
  background: #fdf6ec;
  border: 1px solid #f0dcb8;
  border-radius: 6px;
  padding: 6px 10px;
  font-size: 12px;
  color: var(--warn);
  margin-bottom: 10px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.blocks {
  margin-bottom: 10px;
  background: #faf9f5;
  border-radius: 6px;
  padding: 8px 10px;
}

.blocks-title {
  font-size: 11px;
  color: var(--text-2);
  margin-bottom: 4px;
}

.block-line {
  font-size: 11.5px;
  line-height: 1.7;
}

.sel-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
}

.mini {
  padding: 2px 10px;
  font-size: 11.5px;
}

.table-wrap {
  display: flex;
  height: 260px;
  margin-bottom: 10px;
}

.table-wrap :deep(.vxe-table) {
  flex: 1;
  min-width: 0;
}

/* 可读区间分组：区间序号奇偶行底色交替 */
:deep(.rg-alt) {
  background: #f7fafb;
}

/* 已存在地址：置灰 */
:deep(.rg-exist) td {
  color: var(--text-2);
}

.chip-exist {
  font-size: 11px;
  color: var(--text-2);
}

.import-target .tgt {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 12.5px;
  cursor: pointer;
  margin-right: 10px;
}

.empty-res {
  padding: 30px 8px;
  color: var(--text-2);
  text-align: center;
  line-height: 1.8;
}

.danger {
  color: #fff;
  background: var(--err);
  border-color: var(--err);
}
</style>
