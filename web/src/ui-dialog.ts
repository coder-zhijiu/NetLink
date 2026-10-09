import { api } from './api'

/** 跨环境确认框。
 *  Tauri 用 dialog 插件（IPC 异步，不阻塞 WebView）；浏览器用原生 confirm。
 *  注意：WebView 里直接 window.confirm 会阻塞整个页面 JS（Tauri 官方不建议），
 *  这曾导致「点了按钮整个界面冻结」的问题，一律走这里。
 */
export async function uiConfirm(message: string, title = 'NetLink'): Promise<boolean> {
  if (api.kind === 'tauri') {
    const { confirm } = await import('@tauri-apps/plugin-dialog')
    return await confirm(message, { title })
  }
  return window.confirm(message)
}

/** 跨环境提示框（同 uiConfirm 的理由，alert 也走 dialog 插件） */
export async function uiAlert(message: string, title = 'NetLink'): Promise<void> {
  if (api.kind === 'tauri') {
    const { message: showMessage } = await import('@tauri-apps/plugin-dialog')
    await showMessage(message, { title })
  } else {
    window.alert(message)
  }
}
