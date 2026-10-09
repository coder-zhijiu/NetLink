<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { createProject, currentFileName, openProjectFile, switchProject } from '../store'
import { store } from '../store'
import type { ProjectMeta } from '../types'

const open = ref(false)
const creating = ref(false)
const newName = ref('')
const busy = ref(false)
const error = ref('')
const root = ref<HTMLElement | null>(null)

/** 列表时间显示（当年省年份） */
function fmtTime(ts: number): string {
  const d = new Date(ts)
  const now = new Date()
  const hm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
  const md = `${d.getMonth() + 1}-${d.getDate()}`
  return d.getFullYear() === now.getFullYear() ? `${md} ${hm}` : `${d.getFullYear()}-${md} ${hm}`
}

/** 点工程条目：切换（后端有未保存/运行守卫会先弹确认） */
async function onPick(m: ProjectMeta) {
  if (busy.value) return
  busy.value = true
  error.value = ''
  try {
    await switchProject(m.file_name)
    open.value = false
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    busy.value = false
  }
}

async function onCreate() {
  const name = newName.value.trim()
  if (!name || busy.value) return
  busy.value = true
  error.value = ''
  try {
    await createProject(name)
    creating.value = false
    newName.value = ''
    open.value = false
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    busy.value = false
  }
}

/** 打开工程文件：系统文件对话框选 JSON（浏览器演示=file input） */
async function onOpenFile() {
  if (busy.value) return
  busy.value = true
  error.value = ''
  try {
    const done = await openProjectFile()
    if (done) open.value = false
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    busy.value = false
  }
}

/** 点击组件外部关闭浮层 */
function onDocMouseDown(e: MouseEvent) {
  if (open.value && root.value && !root.value.contains(e.target as Node)) {
    open.value = false
    creating.value = false
    error.value = ''
  }
}

onMounted(() => document.addEventListener('mousedown', onDocMouseDown))
onUnmounted(() => document.removeEventListener('mousedown', onDocMouseDown))
</script>

<template>
  <div ref="root" class="proj-switch">
    <button
      class="proj-switch-btn"
      :title="store.currentPath ? `工程文件：${store.currentPath}` : '演示工程（尚未落盘，点「保存」存入工程目录）'"
      @click="open = !open"
    >
      <span class="proj-switch-name">{{ store.project?.name || '未命名工程' }}</span><span class="proj-caret">▾</span>
    </button>

    <div v-if="open" class="proj-pop">
      <template v-if="!creating">
        <div class="proj-pop-head">工程目录（安装目录 projects/）</div>
        <div class="proj-list">
          <button
            v-for="m in store.projects"
            :key="m.file_name"
            class="proj-item"
            :class="{ active: m.file_name === currentFileName() }"
            :disabled="busy"
            @click="onPick(m)"
          >
            <span class="proj-item-name">{{ m.name }}</span>
            <span class="proj-item-time">{{ fmtTime(m.modified_at) }}</span>
          </button>
          <div v-if="store.projects.length === 0" class="proj-empty">还没有工程，新建一个吧</div>
        </div>
        <div class="proj-actions">
          <button class="proj-new-btn" :disabled="busy" @click="creating = true">＋ 新建工程…</button>
          <button class="proj-open-btn" :disabled="busy" title="从任意位置选择工程文件（保存写回原文件）" @click="onOpenFile">📂 打开工程文件…</button>
        </div>
      </template>

      <template v-else>
        <div class="proj-pop-head">新建工程</div>
        <div class="proj-form">
          <input v-model="newName" placeholder="工程名称" autofocus @keyup.enter="onCreate" />
          <div class="proj-form-btns">
            <button :disabled="busy" @click="creating = false">取消</button>
            <button class="primary" :disabled="!newName.trim() || busy" @click="onCreate">创建</button>
          </div>
          <div class="proj-hint">创建后为空工程；点「保存」写入对应项目文件</div>
        </div>
      </template>

      <div v-if="error" class="proj-error">{{ error }}</div>
    </div>
  </div>
</template>

<style scoped>
.proj-switch {
  position: relative;
  flex: none;
}

.proj-switch-btn {
  display: flex;
  align-items: center;
  gap: 4px;
  max-width: 220px;
}

.proj-switch-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.proj-caret {
  font-size: 10px;
  opacity: 0.7;
  flex: none;
}

.proj-pop {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  width: 300px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 6px;
  box-shadow: 0 6px 24px rgba(0, 0, 0, 0.16);
  z-index: 200;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.proj-pop-head {
  padding: 8px 12px;
  font-size: 11px;
  color: var(--text-2);
  border-bottom: 1px solid var(--border);
  background: #f4f8fa;
  flex: none;
}

.proj-list {
  max-height: 280px;
  overflow: auto;
  padding: 4px;
}

.proj-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
  text-align: left;
  background: transparent;
  border-color: transparent;
  padding: 7px 10px;
}

.proj-item:hover {
  background: #eaf3f7;
}

.proj-item.active {
  background: #dceef6;
  border-color: #b9dcee;
  font-weight: 600;
}

.proj-item-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.proj-item-time {
  font-size: 11px;
  color: var(--text-2);
  flex: none;
}

.proj-empty {
  padding: 14px 10px;
  font-size: 12px;
  color: var(--text-2);
  text-align: center;
}

.proj-actions {
  display: flex;
  gap: 4px;
  padding: 4px;
  flex: none;
}

.proj-actions button {
  flex: 1;
}

.proj-new-btn,
.proj-open-btn {
  border-color: #9ecde0;
  color: #0e7490;
}

.proj-form {
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.proj-form-btns {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

.proj-hint {
  font-size: 11px;
  color: var(--text-2);
}

.proj-error {
  padding: 8px 12px;
  font-size: 12px;
  color: var(--err);
  border-top: 1px solid var(--border);
  background: #fdf2f2;
}
</style>
