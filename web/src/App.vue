<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import ToolBar from './components/ToolBar.vue'
import DeviceTree from './components/DeviceTree.vue'
import PointTable from './components/PointTable.vue'
import FrameMonitor from './components/FrameMonitor.vue'
import TestFramePanel from './components/TestFramePanel.vue'
import CanSendPanel from './components/CanSendPanel.vue'
import { api } from './api'
import { store, applyUpdate, applyProjectHandle, refreshProjects } from './store'
import { uiAlert } from './ui-dialog'

let off: (() => void) | null = null
/** 启动加载失败/超时：给出可重试入口，而不是永远「加载中」 */
const loadFailed = ref(false)
const loading = ref(true)

async function loadInitialProject() {
  loading.value = true
  loadFailed.value = false
  try {
    // 15 秒兜底：invoke 挂起时不再无限等待（历史上出现过主线程被饿死的情况）
    const h = await Promise.race([
      api.loadProject(),
      new Promise<never>((_, reject) => setTimeout(() => reject(new Error('加载超时')), 15_000)),
    ])
    applyProjectHandle(h)
  } catch (e) {
    loadFailed.value = true
    // 工程文件损坏：提示后给空工程（不覆盖原文件；点「保存」会按工程名另存新文件）
    if (e instanceof Error && !e.message.includes('超时')) {
      await uiAlert(`工程文件加载失败：\n${e.message}\n\n已改用空工程；点「保存」将以工程名另存。`)
      applyProjectHandle({
        path: null,
        spec: { schema_version: 3, name: '未命名工程', connections: [] },
      })
      loadFailed.value = false
    }
    // 超时：保留「加载中」+ 重试入口，等用户点击重试
  } finally {
    loading.value = false
  }
  await refreshProjects()
}

onMounted(async () => {
  store.apiKind = api.kind
  off = api.onUpdate(applyUpdate)
  await loadInitialProject()
})

onUnmounted(() => off?.())
</script>

<template>
  <div class="app-root">
    <div v-if="loadFailed" class="load-failed-bar">
      工程加载超时（引擎无响应）
      <button @click="loadInitialProject">重试</button>
    </div>
    <ToolBar />
    <div class="app-main">
      <DeviceTree class="app-tree panel" />
      <PointTable class="app-points panel" />
    </div>
    <FrameMonitor class="app-monitor panel" />
    <TestFramePanel />
    <CanSendPanel />
  </div>
</template>

<style scoped>
.load-failed-bar {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  height: 30px;
  background: #fdf2f2;
  border-bottom: 1px solid #f5c2c2;
  color: var(--err);
  font-size: 12px;
  flex: none;
}

.load-failed-bar button {
  border-color: var(--err);
  color: var(--err);
  padding: 2px 12px;
}
</style>
