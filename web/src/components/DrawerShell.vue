<script setup lang="ts">
defineProps<{
  title: string
  width?: number
}>()
const emit = defineEmits<{ close: [] }>()
</script>

<template>
  <Teleport to="body">
    <div class="drawer-mask" @click.self="emit('close')">
      <div class="drawer" :style="{ width: (width ?? 380) + 'px' }">
        <div class="drawer-head">
          <span>{{ title }}</span>
          <a class="close" @click="emit('close')">✕</a>
        </div>
        <div class="drawer-body">
          <slot />
        </div>
        <div class="drawer-foot">
          <slot name="footer" />
        </div>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.drawer-mask {
  position: fixed;
  inset: 0;
  background: rgba(15, 30, 36, 0.35);
  display: flex;
  justify-content: flex-end;
  z-index: 100;
}

.drawer {
  background: var(--panel);
  border-left: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  box-shadow: -8px 0 24px rgba(0, 0, 0, 0.12);
}

.drawer-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 16px;
  font-weight: 700;
  border-bottom: 1px solid var(--border);
}

.close {
  cursor: pointer;
  color: var(--text-2);
  user-select: none;
}

.close:hover {
  color: var(--err);
}

.drawer-body {
  flex: 1;
  overflow: auto;
  padding: 14px 16px;
}

.drawer-foot {
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
  padding: 12px 16px;
  border-top: 1px solid var(--border);
}
</style>

<style>
/* 表单行样式（各编辑器共用） */
.f-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 10px;
}

.f-lab {
  width: 72px;
  flex: none;
  color: var(--text-2);
}

.f-lab2 {
  width: 44px;
}

.f-row input,
.f-row select {
  flex: 1;
  min-width: 0;
}

.f-group {
  margin: 4px 0 12px;
  padding: 10px;
  background: #f4f8fa;
  border-radius: 6px;
}

.f-hint {
  font-size: 11px;
  color: var(--warn);
  margin-right: auto;
}
</style>
