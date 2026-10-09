<script setup lang="ts">
import { computed, ref } from 'vue'
import { api } from '../api'
import { store, openTestPanel, saveCurrentProject } from '../store'
import { uiAlert } from '../ui-dialog'
import { isMasterTransport } from '../types'
import ProjectPicker from './ProjectPicker.vue'

const saving = ref(false)
const savedTip = ref('')

const hasMasterConn = computed(() =>
  (store.project?.connections ?? []).some((c) => isMasterTransport(c.transport.kind)),
)

async function stopAll() {
  await api.stop()
}

async function togglePause() {
  await api.setPaused(!store.paused)
}

/** 保存工程 → 当前工程对应的项目文件（工程目录内） */
async function saveProject() {
  if (!store.project) return
  saving.value = true
  try {
    await saveCurrentProject()
    savedTip.value = '✓ 已保存'
    setTimeout(() => (savedTip.value = ''), 2000)
  } catch (e) {
    await uiAlert(`保存失败：\n${e instanceof Error ? e.message : String(e)}`)
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <div class="toolbar">
    <div class="brand">
      <span class="logo">⬡</span>
      <span class="name">NetLink</span>
      <span class="sub">多协议工业模拟器</span>
    </div>

    <div v-if="store.project" class="proj">
      <ProjectPicker />
      <input v-model="store.project.name" class="proj-name" title="工程名称（改名不影响文件名）" />
      <button :disabled="saving" @click="saveProject">保存</button>
      <span
        v-if="savedTip"
        class="saved-tip"
        :title="store.currentPath ? `已写入 ${store.currentPath}，重启自动恢复` : ''"
      >{{ savedTip }}</span>
    </div>

    <div class="spacer" />

    <div class="run-group">
      <span v-if="!store.running" class="run-hint">在设备树点击 ▶ 单独启动连接 / 设备</span>
      <template v-else>
        <button class="primary running" @click="stopAll">■ 全部停止</button>
        <button :class="{ paused: store.paused }" @click="togglePause">
          {{ store.paused ? '▶ 继续仿真' : '⏸ 暂停仿真' }}
        </button>
      </template>
      <button :disabled="!hasMasterConn" title="向主机连接发送任意 Modbus 帧" @click="openTestPanel()">🧪 测试帧</button>
    </div>

    <span class="mode chip" :title="api.kind === 'mock' ? '浏览器演示模式，未连接 Rust 引擎' : '已连接本地引擎'">
      {{ api.kind === 'mock' ? 'MOCK 演示' : '本地引擎' }}
    </span>
  </div>
</template>

<style scoped>
.toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  height: 44px;
  padding: 0 14px;
  background: var(--header-bg);
  color: var(--header-text);
  flex: none;
}

.brand {
  display: flex;
  align-items: baseline;
  gap: 6px;
}

.logo {
  color: #4cc9e8;
  font-size: 18px;
}

.name {
  font-weight: 700;
  font-size: 15px;
  letter-spacing: 0.5px;
}

.sub {
  font-size: 11px;
  opacity: 0.6;
}

.proj {
  display: flex;
  align-items: center;
  gap: 6px;
}

.saved-tip {
  font-size: 11px;
  color: #4ade80;
  flex: none;
}

.proj-name {
  width: 180px;
  background: rgba(255, 255, 255, 0.08);
  border-color: rgba(255, 255, 255, 0.15);
  color: var(--header-text);
}

.toolbar button {
  background: rgba(255, 255, 255, 0.08);
  border-color: rgba(255, 255, 255, 0.2);
  color: var(--header-text);
}

.toolbar button:hover {
  border-color: #4cc9e8;
  color: #4cc9e8;
}

.toolbar button.primary {
  background: #0e7490;
  border-color: #0e7490;
  color: #fff;
}

.toolbar button.primary.running {
  background: #7c2d12;
  border-color: #7c2d12;
}

.toolbar button.paused {
  background: #92400e;
  border-color: #92400e;
  color: #fff;
}

.spacer {
  flex: 1;
}

.run-hint {
  font-size: 11px;
  opacity: 0.6;
  color: #cbd5dc;
}

.mode {
  background: rgba(255, 255, 255, 0.1);
  border-color: rgba(255, 255, 255, 0.2);
  color: #cbd5dc;
}
</style>
