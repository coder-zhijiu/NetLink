import type { NetLinkApi } from './contract'

const hasTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

/** 浏览器开发模式走 mock 引擎；在 Tauri 窗口内自动切换到真实后端 */
export const api: NetLinkApi = hasTauri
  ? await import('./tauri').then((m) => m.createTauriApi())
  : await import('./mock').then((m) => m.createMockApi())
