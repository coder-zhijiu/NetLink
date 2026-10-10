<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { store, clearFrames } from '../store'
import { api } from '../api'

const autoScroll = ref(true)
const filterDeviceId = ref<number | ''>('')
/** 面板视图：报文流（逐帧）/ ID 聚合（CAN 接收统计） */
const view = ref<'stream' | 'agg'>('stream')

const bodyRef = ref<HTMLElement | null>(null)

const deviceOptions = computed(() =>
  (store.project?.connections ?? []).flatMap((c) => [
    ...c.devices.map((d) => ({ id: d.id, name: `${c.name} / ${d.name}` })),
    ...(c.can_nodes ?? []).map((n) => ({ id: n.id, name: `${c.name} / ${n.name}` })),
  ]),
)

const deviceName = computed(() => {
  const map = new Map<number, string>()
  for (const c of store.project?.connections ?? []) {
    for (const d of c.devices) map.set(d.id, d.name)
    for (const n of c.can_nodes ?? []) map.set(n.id, n.name)
  }
  return (id?: number) => (id != null ? (map.get(id) ?? `#${id}`) : '—')
})

const visibleFrames = computed(() => {
  let list = store.frames
  if (filterDeviceId.value !== '') list = list.filter((f) => f.device_id === filterDeviceId.value)
  // 只渲染尾部窗口，配合环形缓冲控制 DOM 数量
  return list.slice(-400)
})

/** ID 聚合视图数据：全部 CAN 连接的接收统计（连接名标注来源） */
const canAggRows = computed(() => {
  const out: Array<{ conn: string; id: string; count: number; avg: string; data: string; ts: number; extended: boolean }> = []
  for (const c of store.project?.connections ?? []) {
    const agg = store.canRx[c.id]
    if (!agg) continue
    for (const r of agg.rows) {
      out.push({
        conn: c.name,
        id: `0x${r.frame_id.toString(16).toUpperCase().padStart(r.extended ? 8 : 3, '0')}`,
        count: r.count,
        avg: r.avg_period_ms > 0 ? `${Math.round(r.avg_period_ms)} ms` : '—',
        data: r.last_data || '—',
        ts: r.last_ts,
        extended: r.extended,
      })
    }
  }
  return out.sort((a, b) => b.ts - a.ts)
})

watch(
  () => store.frames.length,
  async () => {
    if (!autoScroll.value || view.value !== 'stream') return
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
      <div class="view-switch" role="group" title="报文流 = 逐帧记录；ID 聚合 = CAN 接收统计（按 ID 合并计数）">
        <button :class="{ on: view === 'stream' }" @click="view = 'stream'">报文流</button>
        <button :class="{ on: view === 'agg' }" @click="view = 'agg'">ID 聚合</button>
      </div>
      <template v-if="view === 'stream'">
        <span class="chip mono">{{ store.frames.length }}</span>
        <span class="dir-legend"><i class="in" />请求</span>
        <span class="dir-legend"><i class="out" />应答</span>
        <select v-model="filterDeviceId" class="filter" title="按设备/CAN 节点过滤">
          <option value="">全部设备</option>
          <option v-for="o in deviceOptions" :key="o.id" :value="o.id">{{ o.name }}</option>
        </select>
      </template>
      <template v-else>
        <span class="chip mono" :title="`CAN 连接接收聚合（运行时生成，自动刷新）`">{{ canAggRows.length }} ID</span>
      </template>
      <div class="spacer" />
      <label v-if="view === 'stream'" class="autos"><input v-model="autoScroll" type="checkbox" /> 自动滚动</label>
      <button v-if="view === 'stream'" @click="clearFrames">清空</button>
    </div>

    <!-- 报文流视图 -->
    <div v-if="view === 'stream'" ref="bodyRef" class="panel-body frames">
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

    <!-- ID 聚合视图（CAN 接收统计） -->
    <div v-else class="panel-body agg">
      <div class="agg-row agg-head">
        <span class="c-conn">连接</span>
        <span class="c-id">ID</span>
        <span class="c-count">计数</span>
        <span class="c-avg">平均周期</span>
        <span class="c-data">最近数据</span>
        <span class="c-ts">最后到达</span>
      </div>
      <div v-for="(r, i) in canAggRows" :key="i" class="agg-row" :title="r.extended ? '扩展帧' : '标准帧'">
        <span class="c-conn">{{ r.conn }}</span>
        <span class="c-id mono">{{ r.id }}{{ r.extended ? ' 扩' : '' }}</span>
        <span class="c-count mono">{{ r.count }}</span>
        <span class="c-avg mono">{{ r.avg }}</span>
        <span class="c-data mono">{{ r.data }}</span>
        <span class="c-ts mono">{{ r.ts ? fmtTime(r.ts) : '—' }}</span>
      </div>
      <div v-if="canAggRows.length === 0" class="hint">
        {{ store.running ? '暂无 CAN 接收聚合（启动 CAN 连接后监听总线报文）' : '模拟未启动（在设备树点击 ▶ 启动连接）' }}
      </div>
    </div>
  </div>
</template>

<style scoped>
.spacer {
  flex: 1;
}

.view-switch {
  display: inline-flex;
  border: 1px solid var(--border);
  border-radius: 6px;
  overflow: hidden;
  flex: none;
}

.view-switch button {
  border: none;
  border-radius: 0;
  padding: 2px 10px;
  font-size: 11px;
  color: var(--text-2);
  background: #fff;
  cursor: pointer;
}

.view-switch button.on {
  background: var(--primary);
  color: #fff;
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

/* ---------- ID 聚合视图 ---------- */
.agg {
  padding: 4px 0;
}

.agg-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 3px 12px;
  font-size: 12px;
  min-height: 26px;
}

.agg-head {
  color: var(--text-2);
  font-size: 11px;
  border-bottom: 1px solid var(--border);
  background: #f7fafb;
  position: sticky;
  top: 0;
}

.agg-row .c-conn {
  width: 150px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  color: var(--text-2);
  flex: none;
}

.agg-row .c-id {
  width: 110px;
  font-weight: 600;
  flex: none;
}

.agg-row .c-count {
  width: 70px;
  text-align: right;
  flex: none;
}

.agg-row .c-avg {
  width: 90px;
  text-align: right;
  flex: none;
}

.agg-row .c-data {
  min-width: 0;
  flex: 1;
  letter-spacing: 0.4px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.agg-row .c-ts {
  width: 100px;
  text-align: right;
  color: var(--text-2);
  font-size: 11px;
  flex: none;
}
</style>
