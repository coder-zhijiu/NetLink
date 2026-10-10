<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { store } from '../store'
import { api } from '../api'
import { canFrameNote, isCanTransport, parseHexBytes, validateCanFrameDef } from '../types'
import type { CanFrame, CanPeriodicFrame } from '../types'
import DrawerShell from './DrawerShell.vue'

/** 目标连接候选：工程里的 CAN 连接 */
const canConns = computed(() =>
  (store.project?.connections ?? []).filter((c) => isCanTransport(c.transport.kind)),
)

const connId = ref<number | null>(null)

watch(
  () => store.canPanel.open,
  (open) => {
    if (!open) return
    if (store.canPanel.connId != null) connId.value = store.canPanel.connId
    if (connId.value == null || !canConns.value.some((c) => c.id === connId.value)) {
      connId.value = canConns.value[0]?.id ?? null
    }
  },
  { immediate: true },
)

/** 所选连接下已定义的报文（一键载入用） */
const definedFrames = computed(() => {
  const conn = canConns.value.find((c) => c.id === connId.value)
  return (conn?.can_nodes ?? []).flatMap((n) => n.frames.map((f) => ({ node: n.name, frame: f })))
})

function frameSummary(f: Pick<CanPeriodicFrame, 'frame_id' | 'extended' | 'remote' | 'data'>): string {
  const width = f.extended ? 8 : 3
  const id = `0x${f.frame_id.toString(16).toUpperCase().padStart(width, '0')}`
  return `${id}${f.extended ? ' 扩展' : ''}${f.remote ? ' 远程' : ''} · ${f.data.length} 字节`
}

// ---------- 发送表单 ----------

const idHex = ref('123')
const extended = ref(false)
const remote = ref(false)
const dataHex = ref('')
const sending = ref(false)
const sendError = ref('')
const history = ref<{ ts: number; summary: string; note: string; ok: boolean }[]>([])

/** 表单即时校验（空串 = 合法） */
const frameError = computed(() => {
  const cleaned = idHex.value.replace(/[^0-9a-fA-F]/g, '')
  if (!cleaned) return '帧 ID 需为十六进制'
  const id = parseInt(cleaned, 16)
  const data = remote.value ? [] : (parseHexBytes(dataHex.value) ?? [])
  if (!remote.value && dataHex.value.trim() && parseHexBytes(dataHex.value) == null) {
    return '数据域十六进制格式非法（字节数须为偶数位）'
  }
  return validateCanFrameDef(id, extended.value, remote.value, data)
})

function buildFrame(): CanFrame | null {
  if (frameError.value) return null
  const id = parseInt(idHex.value.replace(/[^0-9a-fA-F]/g, ''), 16)
  return {
    frame_id: id,
    extended: extended.value,
    remote: remote.value,
    data: remote.value ? [] : (parseHexBytes(dataHex.value) ?? []),
  }
}

/** 一键载入已定义报文 */
function loadDefined(f: CanPeriodicFrame) {
  idHex.value = f.frame_id.toString(16).toUpperCase()
  extended.value = f.extended
  remote.value = f.remote
  dataHex.value = f.data.map((b) => b.toString(16).toUpperCase().padStart(2, '0')).join(' ')
  sendError.value = ''
}

/** 下选择一条已定义报文载入表单（用后复位下拉） */
function onLoadSelect(e: Event) {
  const el = e.target as HTMLSelectElement
  const d = definedFrames.value[Number(el.value)]
  if (d) loadDefined(d.frame)
  el.value = ''
}

async function send() {
  if (connId.value == null || sending.value) return
  const frame = buildFrame()
  if (!frame) {
    sendError.value = frameError.value || '帧定义非法'
    return
  }
  sending.value = true
  sendError.value = ''
  try {
    await api.sendCanFrame(connId.value, frame)
    history.value.unshift({ ts: Date.now(), summary: frameSummary(frame), note: canFrameNote(frame), ok: true })
  } catch (e) {
    sendError.value = e instanceof Error ? e.message : String(e)
    history.value.unshift({ ts: Date.now(), summary: frameSummary(frame), note: sendError.value, ok: false })
  } finally {
    sending.value = false
    if (history.value.length > 10) history.value.length = 10
  }
}

function fmtTime(ts: number): string {
  const d = new Date(ts)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

function close() {
  store.canPanel.open = false
}
</script>

<template>
  <DrawerShell v-if="store.canPanel.open" title="发送 CAN 帧" :width="520" @close="close">
    <div v-if="canConns.length === 0" class="none-hint">
      工程里还没有 CAN 连接——右键设备树新建一个「CAN 直采直控（USB-CAN）」连接后再来。
    </div>

    <template v-else>
      <div class="f-row">
        <span class="f-lab">目标连接</span>
        <select v-model.number="connId">
          <option v-for="c in canConns" :key="c.id" :value="c.id">{{ c.name }}</option>
        </select>
        <span v-if="connId != null && !store.runningConns.includes(connId)" class="f-hint" style="margin: 0">该连接未启动，发送将失败</span>
      </div>

      <div class="f-group">
        <div class="f-row">
          <span class="f-lab">帧 ID (hex)</span>
          <input v-model="idHex" class="mono" placeholder="123（标准）/ 18FF0102（扩展）" spellcheck="false" />
          <label class="chk-inline"><input v-model="extended" type="checkbox" /> 扩展帧</label>
        </div>
        <div class="f-row">
          <span class="f-lab">数据 (hex)</span>
          <input
            v-model="dataHex"
            class="mono"
            :disabled="remote"
            :placeholder="remote ? '远程帧无数据域' : '11 22 33 …（最多 8 字节）'"
            spellcheck="false"
          />
          <label class="chk-inline"><input v-model="remote" type="checkbox" /> 远程帧</label>
        </div>
        <p v-if="frameError" class="err-tip">{{ frameError }}</p>
        <p v-else class="tip">DLC = 数据字节数；报文面板同步显示双向帧（出帧 note 携带 ID/DLC）。</p>
      </div>

      <div class="f-row">
        <span class="f-lab">载入报文</span>
        <select v-if="definedFrames.length > 0" class="load-select" @change="onLoadSelect($event)">
          <option value="">从已定义报文载入…</option>
          <option v-for="(d, i) in definedFrames" :key="i" :value="i">
            {{ d.node }} · {{ frameSummary(d.frame) }}{{ d.frame.period_ms > 0 ? `（周期 ${d.frame.period_ms}ms）` : '' }}
          </option>
        </select>
        <span v-else class="f-hint" style="margin: 0">该连接下还没有已定义报文</span>
        <button class="primary" :disabled="sending || connId == null || !!frameError" @click="send">发送</button>
      </div>

      <p v-if="sendError" class="err-tip">发送失败：{{ sendError }}</p>

      <div v-if="history.length > 0" class="history">
        <div class="resp-title">历史（最近 {{ history.length }} 条）</div>
        <div v-for="(h, i) in history" :key="i" class="history-row" :class="{ bad: !h.ok }">
          <span class="mono">{{ fmtTime(h.ts) }}</span>
          <span class="mono summary">{{ h.summary }}</span>
          <span class="note">{{ h.note }}</span>
        </div>
      </div>
    </template>

    <template #footer>
      <span class="f-hint">手动单帧直控（插队立即发出，不经周期调度）</span>
      <button @click="close">关闭</button>
    </template>
  </DrawerShell>
</template>

<style scoped>
.none-hint {
  padding: 24px 8px;
  color: var(--text-2);
  line-height: 1.7;
}

.tip {
  margin: 2px 0 12px;
  font-size: 11px;
  color: var(--text-2);
  line-height: 1.5;
}

.err-tip {
  margin: 2px 0 8px;
  font-size: 12px;
  color: var(--err);
}

.chk-inline {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
  color: var(--text-2);
  white-space: nowrap;
  flex: none;
  cursor: pointer;
}

.load-select {
  flex: 1;
  min-width: 0;
}

.resp-title {
  font-size: 11px;
  color: var(--text-2);
  margin-bottom: 4px;
}

.history {
  margin-top: 12px;
}

.history-row {
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 3px 0;
  font-size: 11px;
  border-bottom: 1px dashed var(--border);
  color: var(--text-2);
}

.history-row .summary {
  flex: none;
}

.history-row .note {
  min-width: 0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.history-row.bad .note {
  color: var(--err);
}
</style>
