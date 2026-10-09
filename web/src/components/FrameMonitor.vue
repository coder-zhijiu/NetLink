<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { store, clearFrames } from '../store'
import { api } from '../api'

const autoScroll = ref(true)
const filterDeviceId = ref<number | ''>('')

const bodyRef = ref<HTMLElement | null>(null)

const deviceOptions = computed(() =>
  (store.project?.connections ?? []).flatMap((c) => c.devices.map((d) => ({ id: d.id, name: `${c.name} / ${d.name}` }))),
)

const deviceName = computed(() => {
  const map = new Map<number, string>()
  for (const c of store.project?.connections ?? []) for (const d of c.devices) map.set(d.id, d.name)
  return (id?: number) => (id != null ? (map.get(id) ?? `#${id}`) : '—')
})

const visibleFrames = computed(() => {
  let list = store.frames
  if (filterDeviceId.value !== '') list = list.filter((f) => f.device_id === filterDeviceId.value)
  // 只渲染尾部窗口，配合环形缓冲控制 DOM 数量
  return list.slice(-400)
})

watch(
  () => store.frames.length,
  async () => {
    if (!autoScroll.value) return
    await nextTick()
    const el = bodyRef.value
    if (el) el.scrollTop = el.scrollHeight
  },
)

function fmtTime(ts: number): string {
  const d = new Date(ts)
  const p = (n: number, w = 2) => String(n).padStart(w, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`
}
</script>

<template>
  <div class="panel">
    <div class="panel-head">
      <span class="panel-title">报文监视</span>
      <span class="chip mono">{{ store.frames.length }}</span>
      <span class="dir-legend"><i class="in" />请求</span>
      <span class="dir-legend"><i class="out" />应答</span>
      <select v-model="filterDeviceId" class="filter" title="按设备过滤">
        <option value="">全部设备</option>
        <option v-for="o in deviceOptions" :key="o.id" :value="o.id">{{ o.name }}</option>
      </select>
      <div class="spacer" />
      <label class="autos"><input v-model="autoScroll" type="checkbox" /> 自动滚动</label>
      <button @click="clearFrames">清空</button>
    </div>

    <div ref="bodyRef" class="panel-body frames">
      <div v-for="f in visibleFrames" :key="f.id" class="frame" :class="f.dir">
        <span class="ts mono">{{ fmtTime(f.ts) }}</span>
        <span class="dir" :class="f.dir">{{ f.dir === 'in' ? '→' : '←' }}</span>
        <span class="dev">{{ deviceName(f.device_id) }}</span>
        <span class="hex mono">{{ f.hex }}</span>
        <span class="note">{{ f.note ?? '' }}</span>
      </div>
      <div v-if="visibleFrames.length === 0" class="hint">
        {{ store.running ? '等待报文…' : '模拟未启动（在设备树点击 ▶ 启动连接）' }}
      </div>
    </div>
  </div>
</template>

<style scoped>
.spacer {
  flex: 1;
}

.dir-legend {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 11px;
  color: var(--text-2);
}

.dir-legend i {
  width: 8px;
  height: 8px;
  border-radius: 2px;
  display: inline-block;
}

.dir-legend .in {
  background: #2563eb;
}

.dir-legend .out {
  background: #16a34a;
}

.filter {
  width: 220px;
}

.autos {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
  color: var(--text-2);
  cursor: pointer;
}

.frames {
  padding: 2px 0;
}

.frame {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 2px 10px;
  font-size: 12px;
  border-left: 3px solid transparent;
  min-height: 24px;
}

.frame.in {
  border-left-color: #2563eb;
}

.frame.out {
  border-left-color: #16a34a;
}

.frame .ts {
  color: var(--text-2);
  font-size: 11px;
  flex: none;
}

.frame .dir {
  flex: none;
  font-weight: 700;
}

.frame .dir.in {
  color: #2563eb;
}

.frame .dir.out {
  color: #16a34a;
}

.frame .dev {
  flex: none;
  width: 120px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  color: var(--text-2);
}

.frame .hex {
  flex: none;
  color: var(--text);
  letter-spacing: 0.4px;
}

.frame .note {
  color: var(--text-2);
  font-size: 11px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.hint {
  padding: 24px;
  text-align: center;
  color: var(--text-2);
}
</style>
