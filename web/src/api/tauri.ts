import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open as openFileDialog, save as saveFileDialog } from '@tauri-apps/plugin-dialog'
import type { NetLinkApi, RuntimePatch } from './contract'
import type { ProjectHandle, ProjectMeta, ProjectSpec, PointRow, SheetParseResult, TestConnResult } from '../types'

/** Tauri 模式：所有操作走 netlink-app 的 commands，运行时状态经 netlink://update 事件推送 */
export function createTauriApi(): NetLinkApi {
  return {
    kind: 'tauri',
    async loadProject(path) {
      return await invoke<ProjectHandle>('load_project', { path: path ?? null })
    },
    async saveProject(path, project) {
      return await invoke<ProjectHandle>('save_project', { path, project })
    },
    async listProjects() {
      return await invoke<ProjectMeta[]>('list_projects')
    },
    async newProject(name) {
      return await invoke<ProjectHandle>('new_project', { name })
    },
    async openProject(file_name) {
      // Tauri v2 参数按 camelCase 反序列化（Rust 端 snake_case）
      return await invoke<ProjectHandle>('open_project', { fileName: file_name })
    },
    async openProjectFile(): Promise<ProjectHandle | null> {
      // 系统文件对话框选一个工程 JSON（取消返回 null）
      const path = await openFileDialog({
        title: '打开工程',
        filters: [{ name: 'NetLink 工程文件', extensions: ['json'] }],
        multiple: false,
        directory: false,
      })
      if (!path) return null
      // load_project 显式路径分支：读文件 + 记为当前工程（保存写回该文件）
      return await invoke<ProjectHandle>('load_project', { path })
    },
    async start(project) {
      await invoke('start_runtime', { project })
    },
    async stop() {
      await invoke('stop_runtime')
    },
    async startConn(project, connId, enabledDevices) {
      await invoke('start_conn', { project, connId, enabledDevices: enabledDevices ?? null })
    },
    async stopConn(connId) {
      await invoke('stop_conn', { connId })
    },
    async setDeviceEnabled(deviceId, enabled) {
      await invoke('set_device_enabled', { deviceId, enabled })
    },
    async testConnection(transport) {
      return await invoke<TestConnResult>('test_connection', { transport })
    },
    async setPaused(paused) {
      await invoke('set_sim_paused', { paused })
    },
    selectDevice(deviceId) {
      void invoke('select_device', { deviceId }).catch(() => {})
    },
    async listSerialPorts() {
      return await invoke<string[]>('list_serial_ports')
    },
    async writePoint(deviceId, pointId, value) {
      await invoke('write_point', { deviceId, pointId, value })
    },
    async sendTestFrame(connId, unit, hex) {
      return await invoke<string>('send_test_frame', { connId, unit, hex })
    },
    async sendCanFrame(connId, frame) {
      // 帧结构字段与 Rust 端 CanFrame（snake_case）对齐，参数名走 camelCase
      await invoke('send_can_frame', { connId, frame })
    },
    async readPointSheet() {
      const path = await openFileDialog({
        title: '导入点表',
        filters: [
          { name: 'NetLink 点表（xlsx / csv）', extensions: ['xlsx', 'csv'] },
        ],
        multiple: false,
        directory: false,
      })
      if (!path) return null
      return await invoke<SheetParseResult>('read_point_sheet', { path })
    },
    async writePointSheet(rows: PointRow[], defaultName: string) {
      const path = await saveFileDialog({
        title: '导出点表',
        defaultPath: defaultName,
        filters: [
          { name: 'Excel 工作簿', extensions: ['xlsx'] },
          { name: 'CSV（UTF-8，Excel 可直接打开）', extensions: ['csv'] },
        ],
      })
      if (!path) return null
      await invoke('write_point_sheet', { path, rows })
      return path
    },
    onUpdate(cb) {
      const p = listen<RuntimePatch>('netlink://update', (e) => cb(e.payload))
      return () => {
        void p.then((unlisten) => unlisten())
      }
    },
  }
}
