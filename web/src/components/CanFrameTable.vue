<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import {
  store,
  selectedCanNode,
  selectedConnection,
  addFrame,
  patchFrame,
  removeFrame,
  nextFrameId,
  restartConnIfRunning,
  runningDevicesOfConn,
} from '../store'
import { parseHexBytes, validateCanFrameDef } from '../types'
import type { CanPeriodicFrame } from '../types'
import DrawerShell from './DrawerShell.vue'

const node = selectedCanNode
const conn = selectedConnection

const connRunning = computed(() => !!conn.value && store.runningConns.includes(conn.value.id))

/** 该连接的观测快照（帧率徽章） */
const agg = computed(() => (conn.value ? store.canRx[conn.value.id] : undefined))

const frameStat = computed(() => {
  const frames = node.value?.frames ?? []
  const periodic = frames.filter((f) => f.enabled && f.period_ms > 0)
  return { total: frames.length, periodic: periodic.length }
})

/** 周期列显示：毫秒数或「仅手动」（period_ms = 0） */
function periodCell(f: CanPeriodicFrame): string {
  return f.period_ms > 0 ? `${f.period_ms} ms` : '仅手动'
}

function fmtId(f: CanPeriodicFrame): string {
  const width = f.extended ? 8 : 3
  return `0x${f.frame_id.toString(16).toUpperCase().padStart(width, '0')}`
}

function dataCell(f: CanPeriodicFrame): string {
  return f.data.map((b) => b.toString(16).toUpperCase().padStart(2, '0')).join(' ')
}

// ---------- 报文编辑抽屉 ----------

interface FrameEditState {
  frameRowId: number | null
  idHex: string
  extended: boolean
  remote: boolean
  dataHex: string
  periodMs: number
  enabled: boolean
  note: string
}

const edit = reactive<FrameEditState>({
  frameRowId: null,
  idHex: '123',
  extended: false,
  remote: false,
  dataHex: '',
  periodMs: 1000,
  enabled: true,
  note: '',
})

const editorOpen = ref(false)
const editError = ref('')

function openEditor(f: CanPeriodicFrame | null) {
  editError.value = ''
  if (f) {
    edit.frameRowId = f.id
    edit.idHex = f.frame_id.toString(16).toUpperCase()
    edit.extended = f.extended
    edit.remote = f.remote
    edit.dataHex = dataCell(f)
    edit.periodMs = f.period_ms
    edit.enabled = f.enabled
    edit.note = f.note ?? ''
  } else {
    edit.frameRowId = null
    edit.idHex = '123'
    edit.extended = false
    edit.remote = false
    edit.dataHex = ''
    edit.periodMs = 1000
    edit.enabled = true
    edit.note = ''
  }
  editorOpen.value = true
}

/** ID hex 输入 → 数值；非法返回 null */
function parseIdHex(s: string): number | null {
  const cleaned = s.replace(/[^0-9a-fA-F]/g, '')
  if (!cleaned) return null
  const v = parseInt(cleaned, 16)
  return Number.isSafeInteger(v) ? v : null
}

async function saveEditor() {
  const n = node.value
  if (!n) return
  editError.value = ''
  const frameId = parseIdHex(edit.idHex)
  if (frameId == null) {
    editError.value = '帧 ID 需为十六进制（如 123 或 18FF0102）'
    return
  }
  // 远程帧数据域强制为空
  const data = edit.remote ? [] : (parseHexBytes(edit.dataHex) ?? [])
  if (!edit.remote && edit.dataHex.trim() && data == null) {
    editError.value = '数据域十六进制格式非法（字节数须为偶数位）'
    return
  }
  const vErr = validateCanFrameDef(frameId, edit.extended, edit.remote, data ?? [])
  if (vErr) {
    editError.value = vErr
    return
  }
  const base = {
    frame_id: frameId,
    extended: edit.extended,
    remote: edit.remote,
    data,
    period_ms: Math.max(0, Math.round(edit.periodMs) || 0),
    enabled: edit.enabled,
    note: edit.note.trim() || undefined,
  }
  if (edit.frameRowId != null) {
    patchFrame(n.id, edit.frameRowId, base)
  } else {
    addFrame(n.id, { id: nextFrameId(), ...base })
  }
  editorOpen.value = false
  // 帧定义是运行时快照：重启所属连接生效（保留当前在线对象）
  if (selectedConnection.value) await restartConnIfRunning(selectedConnection.value.id)
}

async function delFrame(f: CanPeriodicFrame) {
  const n = node.value
  if (!n) return
  removeFrame(n.id, f.id)
  if (selectedConnection.value) await restartConnIfRunning(selectedConnection.value.id)
}

// 切换对象时若编辑抽屉开着则关掉
watch(
  () => store.selectedDeviceId,
  () => {
    editorOpen.value = false
  },
)

/** 保存按钮悬浮：连接运行中会重启（帧定义是运行时快照） */
function saveTitle(): string {
  return connRunning.value ? '该连接运行中，保存后自动重启生效（保留在线节点）' : undefined as unknown as string
}
</script>

<template>
  <div class="panel-head">
    <span class="panel-title">发送报文</span>
    <template v-if="node">
      <span class="dev-name">{{ node.name }}</span>
      <span class="chip">{{ frameStat.total }} 帧</span>
      <span class="chip mono" :title="`周期发送 ${frameStat.periodic} 条（启用且周期 > 0）`">周期 {{ frameStat.periodic }}</span>
      <template v-if="connRunning && agg">
        <span class="chip mono" :title="`发送帧率（滑动窗口）\n累计发送 ${agg.tx_total} 帧`">↑ {{ agg.tx_rate.toFixed(1) }}/s</span>
        <span class="chip mono" :title="`接收帧率（滑动窗口）\n累计接收 ${agg.rx_total} 帧`">↓ {{ agg.rx_rate.toFixed(1) }}/s</span>
        <span v-if="agg.err_total" class="chip bad" :title="`IO 错误累计 ${agg.err_total}`">错误 {{ agg.err_total }}</span>
      </template>
    </template>
    <div class="spacer" />
    <button class="primary" :disabled="!node" @click="openEditor(null)">＋ 新建报文</button>
  </div>

  <div class="panel-body table-wrap">
    <vxe-table
      v-if="node"
      :data="node.frames"
      height="auto"
      :row-config="{ keyField: 'id', isCurrent: true }"
      :column-config="{ resizable: true }"
      show-overflow="tooltip"
      border="inner"
      :scroll-y="{ enabled: true, gt: 50 }"
    >
      <vxe-column type="seq" title="#" width="46" />
      <vxe-column title="ID" width="120">
        <template #default="{ row }">
          <span class="mono id-cell" :class="{ ext: row.extended }">{{ fmtId(row as CanPeriodicFrame) }}</span>
        </template>
      </vxe-column>
      <vxe-column title="扩展" width="56" align="center">
        <template #default="{ row }">
          <span class="mono">{{ (row as CanPeriodicFrame).extended ? '✓' : '—' }}</span>
        </template>
      </vxe-column>
      <vxe-column title="远程" width="56" align="center">
        <template #default="{ row }">
          <span class="mono">{{ (row as CanPeriodicFrame).remote ? '✓' : '—' }}</span>
        </template>
      </vxe-column>
      <vxe-column title="DLC" width="56" align="center">
        <template #default="{ row }">
          <span class="mono">{{ (row as CanPeriodicFrame).data.length }}</span>
        </template>
      </vxe-column>
      <vxe-column title="数据 (hex)" min-width="200">
        <template #default="{ row }">
          <span class="mono hex-cell">{{ dataCell(row as CanPeriodicFrame) || '—' }}</span>
        </template>
      </vxe-column>
      <vxe-column title="周期" width="90">
        <template #default="{ row }">
          <span class="mono dim">{{ periodCell(row as CanPeriodicFrame) }}</span>
        </template>
      </vxe-column>
      <vxe-column title="启用" width="56" align="center">
        <template #default="{ row }">
          <span class="mono">{{ (row as CanPeriodicFrame).enabled ? '✓' : '—' }}</span>
        </template>
      </vxe-column>
      <vxe-column field="note" title="备注" min-width="120">
        <template #default="{ row }">
          <span class="dim">{{ (row as CanPeriodicFrame).note || '' }}</span>
        </template>
      </vxe-column>
      <vxe-column title="操作" width="110" fixed="right">
        <template #default="{ row }">
          <a class="op" @click="openEditor(row as CanPeriodicFrame)">编辑</a>
          <a class="op danger" @click="delFrame(row as CanPeriodicFrame)">删除</a>
        </template>
      </vxe-column>
    </vxe-table>
    <div v-else class="empty">在左侧设备树选择一个 CAN 节点</div>
  </div>

  <!-- 报文编辑抽屉 -->
  <DrawerShell v-if="editorOpen" :title="edit.frameRowId != null ? '编辑报文' : '新建报文'" @close="editorOpen = false">
    <label class="f-row">
      <span class="f-lab">帧 ID (hex)</span>
      <input v-model="edit.idHex" class="mono" placeholder="123（标准）/ 18FF0102（扩展）" spellcheck="false" />
      <label class="ext-chk"><input v-model="edit.extended" type="checkbox" /> 扩展帧</label>
    </label>
    <label class="f-row">
      <span class="f-lab">数据 (hex)</span>
      <input
        v-model="edit.dataHex"
        class="mono"
        :disabled="edit.remote"
        :placeholder="edit.remote ? '远程帧无数据域' : '11 22 33 …（最多 8 字节）'"
        spellcheck="false"
      />
      <label class="ext-chk"><input v-model="edit.remote" type="checkbox" /> 远程帧</label>
    </label>
    <div class="f-row">
      <span class="f-lab">周期 (ms)</span>
      <input v-model.number="edit.periodMs" type="number" min="0" step="50" class="mono" />
      <span class="tip-inline">0 = 不自动发（仅手动发送面板可载入）</span>
    </div>
    <label class="f-row">
      <span class="f-lab" title="勾选=节点在线时按时发送；行内启停节点可整体暂停">启用</span>
      <input v-model="edit.enabled" type="checkbox" class="chk" />
    </label>
    <label class="f-row">
      <span class="f-lab">备注</span>
      <input v-model="edit.note" placeholder="可选（如 遥测周期帧）" />
    </label>
    <p v-if="editError" class="err-tip">{{ editError }}</p>
    <p class="tip">启用且周期 &gt; 0 的报文按周期发送（节点在线时）；「ID 聚合」视图展示总线上收到的全部报文统计。</p>

    <template #footer>
      <span v-if="connRunning" class="f-hint" :title="saveTitle()">该连接运行中，保存后自动重启生效</span>
      <button @click="editorOpen = false">取消</button>
      <button class="primary" @click="saveEditor">保存</button>
    </template>
  </DrawerShell>
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

.id-cell {
  font-weight: 600;
}

.id-cell.ext {
  color: var(--primary-dark);
}

.hex-cell {
  letter-spacing: 0.4px;
}

.dim {
  color: var(--text-2);
  font-size: 11.5px;
}

.chip.bad {
  color: var(--err);
  border-color: var(--err);
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

.ext-chk {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
  color: var(--text-2);
  white-space: nowrap;
  flex: none;
  cursor: pointer;
}

.chk {
  flex: none;
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

.err-tip {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--err);
}
</style>
