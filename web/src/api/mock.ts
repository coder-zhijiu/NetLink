import type { NetLinkApi, RuntimePatch } from './contract'
import type { CanAgg, CanFrame, CanRxStat, FrameRecord, ProjectHandle, ProjectMeta, ProjectSpec, Point, PointRow, SheetParseResult, SourceKind, DevStat, Transport, TestConnResult } from '../types'
import { canFrameNote, isCanTransport, isMasterTransport } from '../types'
import { parseCsvText, writeCsvText } from './point-sheet-csv'

/**
 * Mock 引擎：纯前端仿真，行为对齐 netlink-core 的 sim/protocol 实现。
 * 用于无 Tauri 环境下的 UI 开发与演示。
 */

let seq = 1
const nextId = () => seq++

function makeDemoProject(): ProjectSpec {
  const aiPoints: Array<[string, number, SourceKind, Record<string, number>]> = [
    ['A相电压', 0, 'sine', { period_s: 60, amplitude: 5, offset: 220 }],
    ['B相电压', 2, 'sine', { period_s: 60, amplitude: 5, offset: 220 }],
    ['C相电压', 4, 'sine', { period_s: 60, amplitude: 5, offset: 220 }],
    ['A相电流', 6, 'sine', { period_s: 20, amplitude: 30, offset: 55 }],
    ['B相电流', 8, 'sine', { period_s: 20, amplitude: 30, offset: 52 }],
    ['C相电流', 10, 'sine', { period_s: 20, amplitude: 30, offset: 57 }],
    ['有功功率', 12, 'random_walk', { step: 2, min: 0, max: 400 }],
    ['无功功率', 14, 'random_walk', { step: 1, min: 0, max: 150 }],
    ['频率', 16, 'sine', { period_s: 30, amplitude: 0.2, offset: 50 }],
    ['功率因数', 18, 'swing', { min: 0.85, max: 0.99, period_s: 45 }],
    ['正向有功电能', 20, 'ramp', { step: 0.05, min: 0, max: 999999 }],
  ]

  const makePoint = (name: string, area: Point['area'], address: number, dataType: Point['data_type'], kind: SourceKind, params: Record<string, number>): Point => ({
    id: nextId(),
    name,
    area,
    address,
    data_type: dataType,
    source: { kind, ...params } as Point['source'],
  })

  return {
    schema_version: 3,
    name: '演示工程 · 一号站',
    connections: [
      {
        id: 1,
        name: 'TCP 主网 (502)',
        transport: { kind: 'tcp_server', listen_addr: '0.0.0.0', port: 502 },
        devices: [
          {
            id: 101,
            name: '1号测控装置',
            slave_id: 1,
            enabled: true,
            fault: { kind: 'normal' },
            points: [
              ...aiPoints.map(([name, addr, kind, p]) => makePoint(name, 'input_register', addr, 'f32', kind, p)),
              makePoint('有功遥测', 'input_register', 24, 'f32', 'sine', { period_s: 15, amplitude: 80, offset: 200 }),
              makePoint('遥信合位', 'coil', 0, 'u16', 'static', { value: 1 }),
              makePoint('遥信分位', 'coil', 1, 'u16', 'static', { value: 0 }),
              makePoint('远方/就地', 'discrete_input', 0, 'u16', 'static', { value: 1 }),
              makePoint('温度', 'input_register', 30, 'f32', 'sine', { period_s: 120, amplitude: 8, offset: 36 }),
              makePoint('湿度', 'input_register', 32, 'f32', 'random_walk', { step: 1, min: 30, max: 80 }),
              makePoint('有功设定', 'holding_register', 0, 'f32', 'static', { value: 250 }),
              makePoint('无功设定', 'holding_register', 2, 'f32', 'static', { value: 30 }),
            ],
          },
          {
            id: 102,
            name: '2号测控装置',
            slave_id: 2,
            enabled: true,
            fault: { kind: 'delay', delay_ms: 200 },
            points: [
              makePoint('A相电压', 'input_register', 0, 'f32', 'sine', { period_s: 50, amplitude: 3, offset: 221 }),
              makePoint('A相电流', 'input_register', 2, 'f32', 'sine', { period_s: 25, amplitude: 20, offset: 40 }),
              makePoint('总有功', 'input_register', 4, 'f32', 'random_walk', { step: 3, min: 10, max: 200 }),
              makePoint('正向有功电能', 'input_register', 6, 'f32', 'ramp', { step: 0.03, min: 0, max: 999999 }),
              makePoint('遥信合位', 'coil', 0, 'u16', 'static', { value: 1 }),
            ],
          },
        ],
      },
      {
        id: 2,
        name: '485 总线 (COM3)',
        transport: { kind: 'serial', serial_port: 'COM3', baud: 9600, parity: 'none', stop_bits: 1 },
        devices: [
          {
            id: 201,
            name: '1号电表',
            slave_id: 1,
            enabled: false,
            fault: { kind: 'normal' },
            points: [
              makePoint('电压', 'input_register', 0, 'f32', 'sine', { period_s: 40, amplitude: 4, offset: 220 }),
              makePoint('电流', 'input_register', 2, 'f32', 'sine', { period_s: 18, amplitude: 10, offset: 15 }),
              makePoint('功率', 'input_register', 4, 'f32', 'random_walk', { step: 0.5, min: 1, max: 30 }),
              makePoint('电能量', 'input_register', 6, 'f32', 'ramp', { step: 0.01, min: 0, max: 999999 }),
            ],
          },
        ],
      },
      {
        id: 3,
        name: '主机采集 → 本机从机',
        transport: { kind: 'tcp_client', host: '127.0.0.1', port: 502, timeout_ms: 1000 },
        devices: [
          {
            id: 301,
            name: '1号测控装置(采集)',
            slave_id: 1,
            enabled: true,
            fault: { kind: 'normal' },
            poll_period_ms: 1000,
            points: [
              makePoint('A相电压', 'input_register', 0, 'f32', 'static', { value: 0 }),
              makePoint('有功遥测', 'input_register', 24, 'f32', 'static', { value: 0 }),
              makePoint('遥信合位', 'coil', 0, 'u16', 'static', { value: 0 }),
              makePoint('有功设定', 'holding_register', 0, 'f32', 'static', { value: 0 }),
            ],
          },
        ],
        can_nodes: [],
      },
      {
        id: 4,
        name: 'CAN 总线 (slcan COM5)',
        transport: { kind: 'usb_can', backend: 'slcan', device_index: 0, channel: 0, bitrate: 500000, serial_port: 'COM5', serial_baud: 115200 },
        devices: [],
        can_nodes: [
          {
            id: 401,
            name: '1号CAN节点',
            enabled: true,
            frames: [
              { id: 4011, frame_id: 0x123, extended: false, remote: false, data: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88], period_ms: 1000, enabled: true, note: '状态周期帧' },
              { id: 4012, frame_id: 0x201, extended: false, remote: false, data: [0x01, 0x02], period_ms: 250, enabled: true, note: '快速心跳' },
            ],
          },
          {
            id: 402,
            name: '2号CAN节点',
            enabled: false,
            frames: [
              { id: 4021, frame_id: 0x18ff0102, extended: true, remote: false, data: [0xaa, 0x55], period_ms: 2000, enabled: true },
            ],
          },
        ],
      },
    ],
  }
}

/** 与 Rust sim.rs 的 evaluate 行为一致 */
function evalSource(kind: SourceKind, params: Record<string, number | undefined>, t: number, last: number): number {
  switch (kind) {
    case 'static':
      return params.value ?? 0
    case 'sine': {
      const period = Math.max(0.1, params.period_s ?? 10)
      return (params.offset ?? 0) + (params.amplitude ?? 1) * Math.sin((2 * Math.PI * t) / period)
    }
    case 'ramp': {
      const min = params.min ?? 0
      const max = params.max ?? 100
      const step = params.step ?? 1
      const span = Math.max(1e-9, max - min)
      let v = last + step
      if (v > max) v = min + ((v - min) % span)
      return v
    }
    case 'random_walk': {
      const min = params.min ?? 0
      const max = params.max ?? 100
      const step = Math.max(0, params.step ?? 1)
      const delta = (Math.random() * 2 - 1) * step
      return Math.min(max, Math.max(min, last + delta))
    }
    case 'swing': {
      const min = params.min ?? 0
      const max = params.max ?? 100
      const period = Math.max(0.1, params.period_s ?? 20)
      const mid = (min + max) / 2
      const amp = (max - min) / 2
      return mid + amp * Math.sin((2 * Math.PI * t) / period)
    }
  }
}

function hex(n: number, width = 2): string {
  return n.toString(16).toUpperCase().padStart(width, '0')
}

export function createMockApi(): NetLinkApi {
  const listeners = new Set<(patch: RuntimePatch) => void>()
  let project: ProjectSpec | null = null
  // 内存工程库（演示模式无文件系统）：file_name → 工程数据 + 各自保存时间
  const projects = new Map<string, { spec: ProjectSpec; savedAt: number }>()
  const memPath = (fileName: string) => `演示内存:/${fileName}`
  let currentFile = '演示工程.json'
  projects.set(currentFile, { spec: makeDemoProject(), savedAt: Date.now() })
  const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T
  const values = new Map<number, number>()
  /** 逐连接 / 逐设备运行状态（与 tauri 端 running_conns / running_devices 语义一致） */
  const runningConns = new Set<number>()
  const runningDevices = new Set<number>()
  const running = () => runningConns.size > 0
  let paused = false
  let simMs = 0
  let lastTick = performance.now()
  let frameSeq = 1
  const timers: ReturnType<typeof setInterval>[] = []

  const emit = (patch: RuntimePatch) => listeners.forEach((f) => f(patch))
  const emitRun = () =>
    emit({
      running: running(),
      running_conns: [...runningConns],
      running_devices: [...runningDevices],
    })

  /** 设置一条连接的在线对象集合（先清掉该连接旧状态；设备与 CAN 节点共用 id 空间） */
  const setConnDevices = (connId: number, ids: number[]) => {
    const conn = project?.connections.find((c) => c.id === connId)
    for (const d of conn?.devices ?? []) runningDevices.delete(d.id)
    for (const n of conn?.can_nodes ?? []) runningDevices.delete(n.id)
    for (const id of ids) runningDevices.add(id)
  }

  const masterStats = new Map<number, DevStat>()

  // ---------- mock CAN 引擎（对齐 Rust can 主循环行为） ----------

  /** 内部聚合状态（rows 按 `${frame_id}:${extended}` 键合并；samples 为帧率窗口） */
  interface MockCanAgg {
    rows: Map<string, CanRxStat>
    tx: number
    rx: number
    err: number
    samples: Array<[number, number, number]>
  }
  const newMockCanAgg = (): MockCanAgg => ({ rows: new Map(), tx: 0, rx: 0, err: 0, samples: [] })
  const canAgg = new Map<number, MockCanAgg>()
  /** 周期报文/假心跳的下次到期（performance.now 基准） */
  const frameDue = new Map<string, number>()

  const canFrameRec = (dir: 'in' | 'out', connId: number, deviceId: number | undefined, f: CanFrame): FrameRecord => ({
    id: frameSeq++,
    ts: Date.now(),
    conn_id: connId,
    device_id: deviceId,
    dir,
    hex: f.data.map((b) => hex(b)).join(' '),
    note: canFrameNote(f),
  })

  /** 收帧聚合（与 Rust CanAgg::on_rx 一致） */
  const canOnRx = (agg: MockCanAgg, f: CanFrame, ts: number) => {
    agg.rx += 1
    const key = `${f.frame_id}:${f.extended}`
    const row = agg.rows.get(key)
    if (row) {
      row.count += 1
      row.avg_period_ms = (row.avg_period_ms * (row.count - 1) + (ts - row.last_ts)) / row.count
      row.last_data = f.data.map((b) => hex(b)).join(' ')
      row.last_ts = ts
    } else {
      agg.rows.set(key, {
        frame_id: f.frame_id,
        extended: f.extended,
        count: 1,
        avg_period_ms: 0,
        last_data: f.data.map((b) => hex(b)).join(' '),
        last_ts: ts,
      })
    }
  }

  const canAggSnapshot = (a: MockCanAgg): CanAgg => {
    const rows = [...a.rows.values()].sort((x, y) => x.frame_id - y.frame_id || Number(x.extended) - Number(y.extended))
    const first = a.samples[0]
    const last = a.samples[a.samples.length - 1]
    let txRate = 0
    let rxRate = 0
    if (first && last && last[0] > first[0]) {
      const secs = (last[0] - first[0]) / 1000
      txRate = (last[1] - first[1]) / secs
      rxRate = (last[2] - first[2]) / secs
    }
    return { rows, tx_total: a.tx, rx_total: a.rx, err_total: a.err, tx_rate: txRate, rx_rate: rxRate }
  }

  /** 每 250ms 一拍：周期报文发送 + 假总线心跳收帧 + 聚合/帧率推送 */
  const tickCan = () => {
    if (paused || !running()) return
    const now = performance.now()
    const frames: FrameRecord[] = []
    const canPatch: Record<number, CanAgg> = {}
    const connStates: Record<number, { kind: 'connected'; detail: null }> = {}
    for (const conn of project?.connections ?? []) {
      if (!isCanTransport(conn.transport.kind) || !runningConns.has(conn.id)) continue
      const agg = canAgg.get(conn.id) ?? newMockCanAgg()

      // ① 节点周期报文（在线节点 + 启用帧 + 周期 > 0）
      for (const node of conn.can_nodes ?? []) {
        if (!runningDevices.has(node.id)) continue
        for (const f of node.frames) {
          if (!f.enabled || f.period_ms <= 0) continue
          const key = `${node.id}:${f.id}`
          const due = frameDue.get(key) ?? 0
          if (now >= due) {
            frameDue.set(key, now + f.period_ms)
            agg.tx += 1
            frames.push(canFrameRec('out', conn.id, node.id, { frame_id: f.frame_id, extended: f.extended, remote: f.remote, data: f.data }))
          }
        }
      }

      // ② 假总线流量：固定心跳帧（约 1s 一帧）演示接收聚合与 ID 聚合视图
      const hbKey = `hb:${conn.id}`
      if (now >= (frameDue.get(hbKey) ?? 0)) {
        frameDue.set(hbKey, now + 1000)
        const f = { frame_id: 0x1f0, extended: false, remote: false, data: [0x00, Math.floor(Math.random() * 0x100)] }
        canOnRx(agg, f, Date.now())
        frames.push(canFrameRec('in', conn.id, undefined, f))
      }

      // ③ 帧率窗口采样
      agg.samples.push([Date.now(), agg.tx, agg.rx])
      while (agg.samples.length > 2 && Date.now() - agg.samples[0][0] > 4000) agg.samples.shift()
      canAgg.set(conn.id, agg)
      canPatch[conn.id] = canAggSnapshot(agg)
      connStates[conn.id] = { kind: 'connected', detail: null }
    }
    if (frames.length > 0) emit({ frames })
    if (Object.keys(canPatch).length > 0) emit({ can_rx: canPatch, conn_states: connStates })
  }

  const tickValues = () => {
    const now = performance.now()
    if (!paused) simMs += now - lastTick
    lastTick = now
    if (paused || !running()) return
    const t = simMs / 1000
    const patch: Record<number, number> = {}
    for (const conn of project?.connections ?? []) {
      if (isMasterTransport(conn.transport.kind)) continue // 主机设备由采集逻辑驱动
      if (!runningConns.has(conn.id)) continue
      for (const dev of conn.devices) {
        if (!runningDevices.has(dev.id)) continue
        for (const p of dev.points) {
          const last = values.get(p.id) ?? (p.source.kind === 'ramp' ? (p.source.min ?? 0) : 0)
          const v = evalSource(p.source.kind, p.source as unknown as Record<string, number | undefined>, t, last)
          values.set(p.id, v)
          patch[p.id] = v
        }
      }
    }
    emit({ values: patch })
    tickMaster()
    tickCan()
  }

  /** mock 主机采集：从同工程运行中的从机设备镜像值 + 采集健康统计 */
  const tickMaster = () => {
    const conns = project?.connections ?? []
    const slaveDevs = conns
      .filter((c) => c.transport.kind === 'tcp_server' && runningConns.has(c.id))
      .flatMap((c) => c.devices)
      .filter((d) => runningDevices.has(d.id))
    const slaveRunning = slaveDevs.length > 0
    const patch: Record<number, number> = {}
    const statsPatch: Record<number, DevStat> = {}
    for (const conn of conns) {
      if (!isMasterTransport(conn.transport.kind) || !runningConns.has(conn.id)) continue
      for (const dev of conn.devices) {
        if (!runningDevices.has(dev.id)) continue
        const st = masterStats.get(dev.id) ?? { last_ok_ms: 0, timeout_count: 0, fail_count: 0 }
        const source = slaveDevs.find((s) => s.slave_id === dev.slave_id)
        if (source && slaveRunning) {
          for (const p of dev.points) {
            const src = source.points.find((sp) => sp.area === p.area && sp.address === p.address)
            if (src) {
              const v = values.get(src.id) ?? 0
              values.set(p.id, v)
              patch[p.id] = v
            }
          }
          st.last_ok_ms = Date.now()
        } else {
          st.timeout_count += 1
        }
        masterStats.set(dev.id, st)
        statsPatch[dev.id] = { ...st }
      }
    }
    if (Object.keys(statsPatch).length > 0) {
      emit({ values: patch, stats: statsPatch })
    }
  }

  const makeFrame = (dir: 'in' | 'out', connId: number, deviceId: number | undefined, bytes: number[], note: string): FrameRecord => ({
    id: frameSeq++,
    ts: Date.now(),
    conn_id: connId,
    device_id: deviceId,
    dir,
    hex: bytes.map((b) => hex(b)).join(' '),
    note,
  })

  const tickFrames = () => {
    if (!running() || paused) return
    const frames: FrameRecord[] = []
    const runningSlave = project?.connections.find((c) => c.transport.kind === 'tcp_server' && runningConns.has(c.id))
    const devices = runningSlave?.devices.filter((d) => runningDevices.has(d.id)) ?? []
    if (runningSlave && devices.length > 0) {
      const dev = devices[Math.floor(Math.random() * devices.length)]
      const txn = Math.floor(Math.random() * 0xffff)
      // FC03 读保持寄存器 0..9
      frames.push(
        makeFrame('in', runningSlave.id, dev.id, [hex2b(txn, 2), 0, 0, 0, 6, dev.slave_id, 3, 0, 0, 0, 10].flat(), `FC03 读保持寄存器 addr=0 qty=10`),
      )
      const regs: number[] = []
      for (let i = 0; i < 10; i++) regs.push(Math.floor(Math.random() * 0xffff))
      frames.push(
        makeFrame('out', runningSlave.id, dev.id, [hex2b(txn, 2), 0, 0, 0, 25, dev.slave_id, 3, 20, ...regs].flat(), `FC03 应答 20字节`),
      )
    }
    // 主机方向的轮询报文（out=请求 / in=应答）
    const mconn = project?.connections.find((c) => isMasterTransport(c.transport.kind) && runningConns.has(c.id))
    const mdev = mconn?.devices.find((d) => runningDevices.has(d.id))
    if (mconn && mdev) {
      const txn = Math.floor(Math.random() * 0xffff)
      const w0 = 0x4360 + Math.floor(Math.random() * 0x10)
      const w1 = 0x4265
      frames.push(
        makeFrame('out', mconn.id, mdev.id, [hex2b(txn, 2), 0, 0, 0, 6, mdev.slave_id, 4, 0, 0, 0, 4].flat(), `FC04 读输入寄存器 addr=0 qty=4`),
      )
      frames.push(
        makeFrame('in', mconn.id, mdev.id, [hex2b(txn, 2), 0, 0, 0, 9, mdev.slave_id, 4, 8, ...hex2b((w0 << 16) | w1, 4)].flat(), `FC04 应答 8字节`),
      )
    }
    if (frames.length > 0) emit({ frames })
  }

  const hex2b = (n: number, w: number): number[] => {
    const out: number[] = []
    for (let i = w - 1; i >= 0; i--) out.push((n >> (i * 8)) & 0xff)
    return out
  }

  return {
    kind: 'mock',
    async loadProject(): Promise<ProjectHandle> {
      project ??= clone(projects.get(currentFile)!.spec)
      return { path: memPath(currentFile), spec: clone(project) }
    },
    async saveProject(_path, projectSpec): Promise<ProjectHandle> {
      project = clone(projectSpec)
      projects.set(currentFile, { spec: project, savedAt: Date.now() })
      return { path: memPath(currentFile), spec: clone(project) }
    },
    async listProjects(): Promise<ProjectMeta[]> {
      return [...projects.entries()]
        .map(([file_name, { spec, savedAt }]) => ({ file_name, name: spec.name, modified_at: savedAt }))
        .sort((a, b) => b.modified_at - a.modified_at)
    },
    async newProject(name): Promise<ProjectHandle> {
      const stem = name.trim() || '未命名工程'
      let fileName = `${stem}.json`
      let n = 1
      while (projects.has(fileName)) {
        n += 1
        fileName = `${stem}-${n}.json`
      }
      const spec: ProjectSpec = { schema_version: 3, name: stem, connections: [] }
      projects.set(fileName, { spec: clone(spec), savedAt: Date.now() })
      currentFile = fileName
      project = clone(spec)
      return { path: memPath(fileName), spec: clone(spec) }
    },
    async openProject(file_name): Promise<ProjectHandle> {
      const entry = projects.get(file_name)
      if (!entry) throw new Error('工程不存在（演示模式）')
      currentFile = file_name
      project = clone(entry.spec)
      return { path: memPath(file_name), spec: clone(entry.spec) }
    },
    async openProjectFile(): Promise<ProjectHandle | null> {
      // 演示模式没有系统文件对话框：用原生 file input 选文件读内容
      return await new Promise((resolve, reject) => {
        const input = document.createElement('input')
        input.type = 'file'
        input.accept = '.json,application/json'
        input.style.display = 'none'
        const cleanup = () => input.remove()
        input.oncancel = () => {
          cleanup()
          resolve(null)
        }
        input.onchange = () => {
          const file = input.files?.[0]
          cleanup()
          if (!file) {
            resolve(null)
            return
          }
          const reader = new FileReader()
          reader.onerror = () => reject(new Error('文件读取失败'))
          reader.onload = () => {
            try {
              const spec = JSON.parse(String(reader.result)) as ProjectSpec
              if (!Array.isArray(spec?.connections)) throw new Error('不是 NetLink 工程文件（缺 connections 字段）')
              let stem = file.name.replace(/\.json$/i, '') || spec.name || '未命名工程'
              let fileName = `${stem}.json`
              let n = 1
              while (projects.has(fileName)) {
                n += 1
                fileName = `${stem}-${n}.json`
              }
              projects.set(fileName, { spec: clone(spec), savedAt: file.lastModified || Date.now() })
              currentFile = fileName
              project = clone(spec)
              resolve({ path: memPath(fileName), spec: clone(spec) })
            } catch (e) {
              reject(new Error(`工程文件解析失败：${e instanceof Error ? e.message : String(e)}`))
            }
          }
          reader.readAsText(file)
        }
        document.body.appendChild(input)
        input.click()
      })
    },
    async start(p: ProjectSpec) {
      runningConns.clear()
      runningDevices.clear()
      canAgg.clear()
      for (const c of p.connections) {
        runningConns.add(c.id)
        const ids = [
          ...c.devices.filter((d) => d.enabled).map((d) => d.id),
          ...(c.can_nodes ?? []).filter((n) => n.enabled).map((n) => n.id),
        ]
        setConnDevices(c.id, ids)
      }
      lastTick = performance.now()
      emitRun()
    },
    async startConn(p: ProjectSpec, connId: number, enabledDevices?: number[] | null) {
      const conn = p.connections.find((c) => c.id === connId)
      if (!conn) throw new Error('连接不存在')
      runningConns.add(connId)
      // 指定集合 = 单台对象首启；未指定按工程配置（设备与 CAN 节点同规则）
      const configured = [
        ...conn.devices.filter((d) => d.enabled).map((d) => d.id),
        ...(conn.can_nodes ?? []).filter((n) => n.enabled).map((n) => n.id),
      ]
      const ids = enabledDevices ?? configured
      setConnDevices(connId, ids)
      lastTick = performance.now()
      emitRun()
    },
    async stopConn(connId: number) {
      runningConns.delete(connId)
      setConnDevices(connId, [])
      canAgg.delete(connId)
      emitRun()
    },
    async stop() {
      runningConns.clear()
      runningDevices.clear()
      canAgg.clear()
      emitRun()
    },
    async setDeviceEnabled(deviceId: number, enabled: boolean) {
      const conn = (project?.connections ?? []).find(
        (c) => c.devices.some((d) => d.id === deviceId) || (c.can_nodes ?? []).some((n) => n.id === deviceId),
      )
      if (!conn) throw new Error('设备不在工程里')
      if (!runningConns.has(conn.id)) throw new Error('该设备所属连接未启动')
      if (enabled) runningDevices.add(deviceId)
      else runningDevices.delete(deviceId)
      emitRun()
    },
    async testConnection(transport: Transport) {
      // 浏览器演示模式无法真正探测，给出诚实提示
      if (transport.kind === 'tcp_client') {
        return { ok: true, message: `演示模式：未实际连接 ${transport.host ?? '?'}:${transport.port ?? 502}，假定远端可达`, latency_ms: 1 }
      }
      if (transport.kind === 'tcp_server') {
        return { ok: true, message: `演示模式：未实际检测端口 ${transport.listen_addr ?? '0.0.0.0'}:${transport.port ?? 502} 占用，假定可用` }
      }
      if (transport.kind === 'can_net') {
        return { ok: true, message: `演示模式：未实际连接 ${transport.host ?? '?'}:${transport.port ?? 502}（CANET 网关），假定可达`, latency_ms: 1 }
      }
      if (transport.kind === 'usb_can') {
        return { ok: true, message: `演示模式：未实际检测串口 ${transport.serial_port ?? 'COM?'} 占用，假定可用` }
      }
      return { ok: true, message: `演示模式：未实际检测串口 ${transport.serial_port ?? 'COM?'} 占用，假定可用` }
    },
    async setPaused(p) {
      paused = p
      lastTick = performance.now()
      emit({ paused: p })
    },
    selectDevice(_deviceId) {
      // mock 引擎全量计算，无需按设备切换
    },
    async listSerialPorts() {
      // 浏览器演示模式拿不到真实串口，给两个假口演示表单交互
      return ['COM3', 'COM4']
    },
    async writePoint(deviceId, pointId, value) {
      const conns = project?.connections ?? []
      const dev = conns.flatMap((c) => c.devices).find((d) => d.id === deviceId)
      const pt = dev?.points.find((p) => p.id === pointId)
      if (!dev || !pt) return
      // 与 Rust 端一致：输入区只读
      if (pt.area === 'discrete_input' || pt.area === 'input_register') {
        throw new Error(`${pt.area === 'discrete_input' ? '离散输入' : '输入寄存器'}为只读区，仅线圈/保持寄存器可写`)
      }
      // 线圈按 Modbus 语义归一化为 0/1（≥0.5 = 合）
      if (pt.area === 'coil') value = value >= 0.5 ? 1 : 0
      values.set(pointId, value)
      // 静态源：写入成为新的静态值（与 Rust 引擎语义一致，tick 不打回）
      if (pt.source.kind === 'static') pt.source.value = value
      const patch: Record<number, number> = { [pointId]: value }
      // mock 闭环：镜像更新对应从机点位，下一轮采集读到新值
      const isMasterDev = conns.some((c) => isMasterTransport(c.transport.kind) && c.devices.some((d) => d.id === deviceId))
      if (isMasterDev) {
        const slaveDevs = conns.filter((c) => c.transport.kind === 'tcp_server').flatMap((c) => c.devices)
        const src = slaveDevs.find((s) => s.slave_id === dev.slave_id)?.points.find((sp) => sp.area === pt.area && sp.address === pt.address)
        if (src) {
          values.set(src.id, value)
          if (src.source.kind === 'static') src.source.value = value
          patch[src.id] = value
        }
      }
      emit({ values: patch })
    },
    async sendTestFrame(connId, unit, hexStr) {
      const clean = hexStr.replace(/[^0-9a-fA-F]/g, '')
      const fc = parseInt(clean.slice(0, 2), 16) || 0
      let resp: string
      if (fc === 0x03 || fc === 0x04) {
        resp = `${hex(fc)} 04 43 7A 00 00`
      } else if (fc === 0x05 || fc === 0x06 || fc === 0x10) {
        resp = clean.match(/.{1,2}/g)?.map((b) => hex(parseInt(b, 16))).join(' ') ?? hexStr
      } else {
        resp = `${hex(fc | 0x80)} 01`
      }
      const hexToBytes = (s: string): number[] =>
        s.replace(/[^0-9a-fA-F]/g, '').match(/.{1,2}/g)?.map((b) => parseInt(b, 16)) ?? []
      emit({
        frames: [
          makeFrame('out', connId, undefined, hexToBytes(hexStr), `测试帧 unit=${unit}`),
          makeFrame('in', connId, undefined, hexToBytes(resp), '测试帧应答(演示)'),
        ],
      })
      return resp
    },
    async sendCanFrame(connId, frame) {
      const conn = (project?.connections ?? []).find((c) => c.id === connId)
      if (!conn || !isCanTransport(conn.transport.kind)) throw new Error('目标连接不是 CAN 连接')
      if (!runningConns.has(connId)) throw new Error('CAN 连接未启动，发送将失败')
      // 手动发送：出帧事件 + 计数（与 Rust 主循环的命令插队路径同构）
      const agg = canAgg.get(connId) ?? newMockCanAgg()
      agg.tx += 1
      agg.samples.push([Date.now(), agg.tx, agg.rx])
      canAgg.set(connId, agg)
      emit({ frames: [canFrameRec('out', connId, undefined, frame)] })
      emit({ can_rx: { [connId]: canAggSnapshot(agg) } })
    },
    onUpdate(cb) {
      listeners.add(cb)
      // 订阅时启动内部定时器（首个订阅者触发）
      if (timers.length === 0) {
        timers.push(setInterval(tickValues, 250))
        timers.push(setInterval(tickFrames, 1400))
      }
      return () => {
        listeners.delete(cb)
        if (listeners.size === 0) {
          timers.forEach(clearInterval)
          timers.length = 0
        }
      }
    },
    async readPointSheet(): Promise<SheetParseResult | null> {
      // 演示模式没有系统文件对话框：file input 选文件读文本（仅 CSV，xlsx 需桌面版）
      const file = await pickLocalFile('.csv')
      if (!file) return null
      if (!/\.csv$/i.test(file.name)) throw new Error('演示模式仅支持 CSV 点表（xlsx 请用桌面版）')
      return parseCsvText(await file.text())
    },
    async writePointSheet(rows: PointRow[], defaultName: string): Promise<string | null> {
      // 演示模式没有本地盘：触发浏览器下载 CSV
      const fileName = `${(defaultName || '点表').replace(/[\\/:*?"<>|]/g, '_')}.csv`
      const blob = new Blob(['\uFEFF' + writeCsvText(rows)], { type: 'text/csv;charset=utf-8' })
      const a = document.createElement('a')
      a.href = URL.createObjectURL(blob)
      a.download = fileName
      a.click()
      URL.revokeObjectURL(a.href)
      return fileName
    },
  }
}

/** 隐藏 file input 选一个文件；null = 用户取消 */
function pickLocalFile(accept: string): Promise<File | null> {
  return new Promise((resolve) => {
    const input = document.createElement('input')
    input.type = 'file'
    input.accept = accept
    input.style.display = 'none'
    input.oncancel = () => {
      input.remove()
      resolve(null)
    }
    input.onchange = () => {
      const file = input.files?.[0] ?? null
      input.remove()
      resolve(file)
    }
    document.body.appendChild(input)
    input.click()
  })
}
