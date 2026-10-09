<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { store } from '../store'
import { api } from '../api'
import { isMasterTransport } from '../types'
import DrawerShell from './DrawerShell.vue'

const masterConns = computed(() =>
  (store.project?.connections ?? []).filter((c) => isMasterTransport(c.transport.kind)),
)

const connId = ref<number | null>(null)
const unit = ref<number>(1)

watch(
  () => store.testPanel.open,
  (open) => {
    if (!open) return
    if (store.testPanel.connId != null) connId.value = store.testPanel.connId
    if (store.testPanel.unit != null) unit.value = store.testPanel.unit
    if (connId.value == null || !masterConns.value.some((c) => c.id === connId.value)) {
      connId.value = masterConns.value[0]?.id ?? null
    }
  },
  { immediate: true },
)

const unitOptions = computed(() => {
  const conn = masterConns.value.find((c) => c.id === connId.value)
  return conn?.devices.map((d) => ({ label: `${d.name} (ID ${d.slave_id})`, value: d.slave_id })) ?? []
})

const fcTemplates = [
  { label: 'FC01 读线圈', value: '01' },
  { label: 'FC02 读离散输入', value: '02' },
  { label: 'FC03 读保持寄存器', value: '03' },
  { label: 'FC04 读输入寄存器', value: '04' },
  { label: 'FC05 写单线圈', value: '05' },
  { label: 'FC06 写单寄存器', value: '06' },
  { label: 'FC10 写多寄存器', value: '10' },
  { label: '自定义（直接编 hex）', value: 'custom' },
]
const fc = ref('03')
const addr = ref(0)
const qty = ref(2)
const coilOn = ref(true)
const valueNum = ref(0)
const valuesCsv = ref('100, 200')

const hexInput = ref('03 00 00 00 02')

function buildHex() {
  if (fc.value === 'custom') return
  const a = Math.max(0, Math.min(65535, Math.round(addr.value) || 0))
  const f = parseInt(fc.value, 16)
  const bytes: number[] = [f, (a >> 8) & 0xff, a & 0xff]
  if (['01', '02', '03', '04'].includes(fc.value)) {
    const q = Math.max(1, Math.round(qty.value) || 1)
    bytes.push((q >> 8) & 0xff, q & 0xff)
  } else if (fc.value === '05') {
    bytes.push(...(coilOn.value ? [0xff, 0x00] : [0x00, 0x00]))
  } else if (fc.value === '06') {
    const v = Math.max(0, Math.min(65535, Math.round(valueNum.value) || 0))
    bytes.push((v >> 8) & 0xff, v & 0xff)
  } else if (fc.value === '10') {
    const words = valuesCsv.value
      .split(/[,，\s]+/)
      .filter(Boolean)
      .map((s) => Math.max(0, Math.min(65535, Math.round(Number(s) || 0))))
    bytes.push((words.length >> 8) & 0xff, words.length & 0xff, words.length * 2)
    for (const w of words) bytes.push((w >> 8) & 0xff, w & 0xff)
  }
  hexInput.value = bytes.map((b) => b.toString(16).padStart(2, '0').toUpperCase()).join(' ')
}

watch([fc, addr, qty, coilOn, valueNum, valuesCsv], buildHex, { immediate: true })

const sending = ref(false)
const lastResp = ref<{ hex: string; note: string } | null>(null)
const history = ref<{ ts: number; req: string; resp: string; ok: boolean }[]>([])

async function send() {
  if (connId.value == null) return
  const pdu = hexInput.value.trim()
  if (!pdu) return
  sending.value = true
  try {
    const resp = await api.sendTestFrame(connId.value, unit.value, pdu)
    lastResp.value = { hex: resp, note: parseResp(pdu, resp) }
    history.value.unshift({ ts: Date.now(), req: pdu, resp, ok: true })
  } catch (e) {
    lastResp.value = { hex: '', note: String(e) }
    history.value.unshift({ ts: Date.now(), req: pdu, resp: String(e), ok: false })
  } finally {
    sending.value = false
    if (history.value.length > 10) history.value.length = 10
  }
}

function parseResp(req: string, resp: string): string {
  const r = resp.replace(/\s/g, '')
  const q = req.replace(/\s/g, '')
  const fcReq = parseInt(q.slice(0, 2), 16) || 0
  const fc = parseInt(r.slice(0, 2), 16) || 0
  if (fc & 0x80) return `异常应答 code=0x${r.slice(2, 4) || '??'}`
  if (fcReq >= 1 && fcReq <= 4) {
    const n = parseInt(r.slice(2, 4), 16) || 0
    return `正常应答，${n} 字节数据`
  }
  return '正常应答'
}

function fmtTime(ts: number): string {
  const d = new Date(ts)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

function close() {
  store.testPanel.open = false
}
</script>

<template>
  <DrawerShell v-if="store.testPanel.open" title="发送测试帧" :width="500" @close="close">
    <div v-if="masterConns.length === 0" class="none-hint">工程里还没有主机连接——右键设备树新建一个「Modbus TCP 主机」连接后再来。</div>

    <template v-else>
      <div class="f-row">
        <span class="f-lab">目标连接</span>
        <select v-model.number="connId">
          <option v-for="c in masterConns" :key="c.id" :value="c.id">{{ c.name }}</option>
        </select>
      </div>
      <div class="f-row">
        <span class="f-lab">从机地址</span>
        <select v-model.number="unit">
          <option v-for="o in unitOptions" :key="o.value" :value="o.value">{{ o.label }}</option>
        </select>
        <span v-if="connId != null && !store.runningConns.includes(connId)" class="f-hint" style="margin: 0">该连接未启动，发送将失败</span>
      </div>

      <div class="f-group">
        <div class="f-row">
          <span class="f-lab">帧模板</span>
          <select v-model="fc">
            <option v-for="t in fcTemplates" :key="t.value" :value="t.value">{{ t.label }}</option>
          </select>
        </div>
        <template v-if="fc !== 'custom'">
          <div class="f-row">
            <span class="f-lab">地址</span>
            <input v-model.number="addr" type="number" min="0" max="65535" class="mono" />
            <template v-if="['01', '02', '03', '04'].includes(fc)">
              <span class="f-lab f-lab2">数量</span>
              <input v-model.number="qty" type="number" min="1" class="mono" />
            </template>
          </div>
          <div v-if="fc === '05'" class="f-row">
            <span class="f-lab">线圈值</span>
            <select v-model="coilOn">
              <option :value="true">ON (FF00)</option>
              <option :value="false">OFF (0000)</option>
            </select>
          </div>
          <div v-else-if="fc === '06'" class="f-row">
            <span class="f-lab">写入值</span>
            <input v-model.number="valueNum" type="number" min="0" max="65535" class="mono" />
          </div>
          <div v-else-if="fc === '10'" class="f-row">
            <span class="f-lab">写入值</span>
            <input v-model="valuesCsv" placeholder="100, 200（逗号分隔）" class="mono" />
          </div>
        </template>
      </div>

      <div class="f-row">
        <span class="f-lab">PDU hex</span>
        <input v-model="hexInput" class="mono hex-input" spellcheck="false" placeholder="01 03 00 00 00 02" />
        <button class="primary" :disabled="sending || !connId" @click="send">发送</button>
      </div>
      <p class="tip">hex 为 PDU（不含 MBAP 头），引擎自动补事务号与长度；报文面板同步显示双向帧。</p>

      <div v-if="lastResp" class="resp-box" :class="{ err: !lastResp.hex }">
        <div class="resp-title">应答</div>
        <div v-if="lastResp.hex" class="mono resp-hex">{{ lastResp.hex }}</div>
        <div class="resp-note">{{ lastResp.note }}</div>
      </div>

      <div v-if="history.length > 0" class="history">
        <div class="resp-title">历史（最近 {{ history.length }} 条）</div>
        <div v-for="(h, i) in history" :key="i" class="history-row" :class="{ bad: !h.ok }">
          <span class="mono">{{ fmtTime(h.ts) }}</span>
          <span class="mono hex">{{ h.req }}</span>
          <span class="mono arrow">→</span>
          <span class="mono hex">{{ h.resp }}</span>
        </div>
      </div>
    </template>

    <template #footer>
      <span class="f-hint">应答与请求逐字节对照显示</span>
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

.hex-input {
  flex: 1;
  letter-spacing: 0.5px;
}

.resp-box {
  margin-top: 6px;
  padding: 10px;
  background: #eff7f1;
  border: 1px solid #bfe3c8;
  border-radius: 6px;
}

.resp-box.err {
  background: #fdf0f0;
  border-color: #efc4c4;
}

.resp-title {
  font-size: 11px;
  color: var(--text-2);
  margin-bottom: 4px;
}

.resp-hex {
  font-size: 13px;
  letter-spacing: 0.5px;
  word-break: break-all;
}

.resp-note {
  margin-top: 4px;
  font-size: 12px;
  color: var(--primary-dark);
}

.history {
  margin-top: 12px;
}

.history-row {
  display: flex;
  gap: 6px;
  align-items: center;
  padding: 3px 0;
  font-size: 11px;
  border-bottom: 1px dashed var(--border);
  color: var(--text-2);
}

.history-row .hex {
  word-break: break-all;
  min-width: 0;
}

.history-row .arrow {
  flex: none;
}

.history-row.bad .hex:last-child {
  color: var(--err);
}
</style>
