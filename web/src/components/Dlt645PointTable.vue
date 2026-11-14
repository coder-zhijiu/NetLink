<script setup lang="ts">
import { computed, reactive, ref } from 'vue'
import { api } from '../api'
import {
  connRunning, deviceRunning, nextPointId, restartConnIfRunning,
  selectedConnection, selectedDlt645Meter, store,
} from '../store'
import type { Dlt645Format, Dlt645Point } from '../types'
import { uiConfirm } from '../ui-dialog'
import DrawerShell from './DrawerShell.vue'

const meter = selectedDlt645Meter
const conn = selectedConnection
const connected = computed(() => conn.value != null && connRunning(conn.value.id)
  && store.connStates[conn.value.id]?.kind === 'connected')
const polling = computed(() => meter.value != null && deviceRunning(meter.value.id))
const busy = ref(false)
const readError = ref('')
const pickerOpen = ref(false)
const picked = ref<string[]>([])
const editorOpen = ref(false)
const editorError = ref('')
const existing = computed(() => new Set(meter.value?.points.map((p) => p.di.toUpperCase())))

type Preset = Omit<Dlt645Point, 'id'>
function preset(di: string, name: string, unit: string, bytes: number, decimals: number, format: Dlt645Format = 'bcd'): Preset {
  return { di, name, unit, bytes, decimals, format, scale: 1, poll: true }
}

const catalog = [
  { group: '电量', points: [
    preset('00010000', '正向有功总电量', 'kWh', 4, 2),
    preset('00020000', '反向有功总电量', 'kWh', 4, 2),
  ] },
  { group: '电压', points: ['A', 'B', 'C'].map((phase, i) => preset(`02010${i + 1}00`, `${phase} 相电压`, 'V', 2, 1)) },
  { group: '电流', points: ['A', 'B', 'C'].map((phase, i) => preset(`02020${i + 1}00`, `${phase} 相电流`, 'A', 3, 3, 'signed_bcd')) },
  { group: '功率与频率', points: [
    preset('02030000', '总有功功率', 'kW', 3, 4, 'signed_bcd'),
    preset('02060000', '总功率因数', '', 2, 3, 'signed_bcd'),
    preset('02800002', '电网频率', 'Hz', 2, 2),
  ] },
]

const formats: Record<Dlt645Format, string> = {
  bcd: 'BCD',
  signed_bcd: 'BCD（最高位符号）',
  unsigned: '无符号整数',
  signed: '有符号整数（补码）',
}
const edit = reactive<Dlt645Point>({ id: 0, ...preset('', '', '', 2, 0) })

function openPicker() {
  picked.value = []
  pickerOpen.value = true
}

async function addPicked() {
  if (!meter.value || !conn.value) return
  for (const p of catalog.flatMap((group) => group.points)) {
    if (picked.value.includes(p.di) && !existing.value.has(p.di)) {
      meter.value.points.push({ ...p, id: nextPointId() })
    }
  }
  pickerOpen.value = false
  await restartConnIfRunning(conn.value.id)
}

function openEditor(point?: Dlt645Point) {
  Object.assign(edit, point ?? { id: 0, ...preset('', '', '', 2, 0) })
  editorError.value = ''
  editorOpen.value = true
}

async function savePoint() {
  if (!meter.value || !conn.value) return
  const di = edit.di.trim().toUpperCase()
  if (!/^[0-9A-F]{8}$/.test(di)) {
    editorError.value = 'DI 必须是 8 位十六进制字符。'
    return
  }
  if (meter.value.points.some((p) => p.id !== edit.id && p.di.toUpperCase() === di)) {
    editorError.value = '此电表已经配置该 DI。'
    return
  }
  const maxBytes = edit.format === 'bcd' || edit.format === 'signed_bcd' ? 6 : 4
  if (!edit.name.trim() || !Number.isInteger(edit.bytes) || edit.bytes < 1 || edit.bytes > maxBytes
    || !Number.isInteger(edit.decimals) || edit.decimals < 0 || edit.decimals > 12 || !Number.isFinite(edit.scale)) {
    editorError.value = '请检查名称、数据字节数、小数位和倍率。'
    return
  }
  const point = { ...edit, di, name: edit.name.trim(), unit: edit.unit.trim(), id: edit.id || nextPointId() }
  const previous = meter.value.points.find((p) => p.id === edit.id)
  if (previous) Object.assign(previous, point)
  else meter.value.points.push(point)
  editorOpen.value = false
  await restartConnIfRunning(conn.value.id)
}

async function removePoint(point: Dlt645Point) {
  if (!await uiConfirm(`删除指标「${point.name}」？`) || !meter.value || !conn.value) return
  meter.value.points = meter.value.points.filter((p) => p.id !== point.id)
  await restartConnIfRunning(conn.value.id)
}

async function readOnce(point: Dlt645Point) {
  if (!meter.value || !conn.value || busy.value) return
  busy.value = true
  readError.value = ''
  try {
    await api.readDlt645Point(conn.value.id, meter.value.id, point.id)
  } catch (e) {
    readError.value = String(e)
  } finally {
    busy.value = false
  }
}

function status(point: Dlt645Point) {
  if (!connected.value) return '离线'
  const sample = store.dlt645Samples[point.id]
  if (sample?.error) return sample.error
  if (!polling.value || point.poll === false) return sample?.last_ok_ms ? '已读 · 未轮询' : '未轮询'
  return sample?.last_ok_ms ? '正常' : '等待采集'
}

function lastSuccess(point: Dlt645Point) {
  const ts = store.dlt645Samples[point.id]?.last_ok_ms
  return ts ? new Date(ts).toLocaleTimeString() : '—'
}
</script>

<template>
  <div class="panel" v-if="meter">
    <div class="panel-head">
      <span class="panel-title">{{ meter.name }}</span>
      <span class="mono">{{ meter.address }}</span>
      <span class="chip">{{ polling ? '周期采集' : '轮询暂停' }}</span>
      <div class="spacer" />
      <button :disabled="busy" @click="openPicker">选择指标</button>
      <button class="primary" :disabled="busy" @click="openEditor()">＋ 自定义指标</button>
    </div>
    <div class="panel-body data">
      <p v-if="readError" class="error" role="alert">{{ readError }}</p>
      <table v-if="meter.points.length" class="meter-table">
        <thead>
          <tr><th>指标</th><th>DI</th><th>当前值</th><th>单位</th><th>最后成功</th><th>状态</th><th>操作</th></tr>
        </thead>
        <tbody>
          <tr v-for="point in meter.points" :key="point.id">
            <td>{{ point.name }}</td>
            <td class="mono">{{ point.di }}</td>
            <td class="mono" :title="`电表读数 ${store.dlt645Samples[point.id]?.raw ?? '—'} × ${point.scale ?? 1}`">
              {{ store.dlt645Samples[point.id]?.value ?? '—' }}
            </td>
            <td>{{ point.unit || '—' }}</td>
            <td class="mono">{{ lastSuccess(point) }}</td>
            <td :class="{ error: store.dlt645Samples[point.id]?.error }">{{ status(point) }}</td>
            <td class="actions">
              <button class="mini" :disabled="busy || !connected" @click="readOnce(point)">读取一次</button>
              <button class="mini" :disabled="busy" @click="openEditor(point)">编辑</button>
              <button class="mini" :disabled="busy" @click="removePoint(point)">删除</button>
            </td>
          </tr>
        </tbody>
      </table>
      <div v-else class="empty">从指标目录勾选点位，或按厂家手册添加自定义 DI。</div>
    </div>

    <DrawerShell v-if="pickerOpen" title="选择指标" @close="pickerOpen = false">
      <fieldset v-for="group in catalog" :key="group.group" class="catalog-group">
        <legend>{{ group.group }}</legend>
        <label v-for="point in group.points" :key="point.di" class="catalog-row">
          <input v-model="picked" type="checkbox" :value="point.di" :disabled="existing.has(point.di)" />
          {{ point.name }} <span class="mono">{{ point.di }}</span>
          <span v-if="existing.has(point.di)">已添加</span>
        </label>
      </fieldset>
      <template #footer>
        <button @click="pickerOpen = false">取消</button>
        <button class="primary" :disabled="picked.length === 0" @click="addPicked">添加 {{ picked.length }} 项</button>
      </template>
    </DrawerShell>

    <DrawerShell v-if="editorOpen" :title="edit.id ? '编辑指标' : '自定义指标'" @close="editorOpen = false">
      <label class="field">名称 <input v-model="edit.name" /></label>
      <label class="field">DI <input v-model="edit.di" class="mono" maxlength="8" placeholder="8 位十六进制" /></label>
      <label class="field">解析格式
        <select v-model="edit.format">
          <option v-for="(label, key) in formats" :key="key" :value="key">{{ label }}</option>
        </select>
      </label>
      <label class="field">数据字节数 <input v-model.number="edit.bytes" type="number" min="1" max="6" /></label>
      <label class="field">小数位 <input v-model.number="edit.decimals" type="number" min="0" max="12" /></label>
      <label class="field">单位 <input v-model="edit.unit" /></label>
      <label class="field">倍率 <input v-model.number="edit.scale" type="number" step="any" /></label>
      <label class="catalog-row"><input v-model="edit.poll" type="checkbox" /> 参与周期轮询</label>
      <p v-if="editorError" class="error" role="alert">{{ editorError }}</p>
      <template #footer>
        <button @click="editorOpen = false">取消</button>
        <button class="primary" @click="savePoint">保存</button>
      </template>
    </DrawerShell>
  </div>
</template>

<style scoped>
.data { overflow: auto; }
.meter-table { width: 100%; border-collapse: collapse; text-align: left; }
.meter-table th, .meter-table td { padding: 8px 10px; border-bottom: 1px solid var(--border); }
.meter-table th { position: sticky; top: 0; background: var(--bg-panel, var(--panel)); }
.actions { white-space: nowrap; }
.actions button + button { margin-left: 4px; }
.error { color: var(--err); }
.catalog-group { border: 1px solid var(--border); margin: 0 0 10px; }
.catalog-row { display: flex; gap: 8px; align-items: center; padding: 4px 0; }
.field { display: grid; grid-template-columns: 92px 1fr; align-items: center; margin-bottom: 10px; }
</style>
