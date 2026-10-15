import type { Area, DataType, PointRow, SheetParseResult, SourceKind, ValueSource } from '../types'
import { AREA_LABELS, SOURCE_LABELS } from '../types'

/**
 * 浏览器演示模式（mock）的点表 CSV 文本解析/生成。
 * 列格式与 netlink-core/src/sheet.rs 严格一致（15 列）：
 * 名称,区域,地址,类型,数据源,静态值,周期(秒),幅值,偏移,步长,下限,上限,系数,换算偏移,备注
 *
 * 解析按表头名映射列（表头齐备 5 个必需列时）：列序自由、旧 13 列文件缺换算列按默认；
 * 「偏移」= 正弦波形基线，「换算偏移」= 工程量换算偏移，同名不同义。
 */

/** 逻辑列 → 15 列布局里的位置（无表头/残缺表头文件按此解析） */
const COL_POSITION: Record<string, number> = {
  name: 0,
  area: 1,
  addr: 2,
  type: 3,
  source: 4,
  value: 5,
  period: 6,
  amplitude: 7,
  sine_offset: 8,
  step: 9,
  min: 10,
  max: 11,
  scale: 12,
  eng_offset: 13,
  note: 14,
}

/** 表头单元格文本 → 逻辑列（与 Rust 端 header_col 别名一致） */
const HEADER_TO_COL: Record<string, string> = {
  '名称': 'name',
  name: 'name',
  '点名称': 'name',
  '区域': 'area',
  '地址': 'addr',
  '类型': 'type',
  '数据源': 'source',
  '静态值': 'value',
  '周期(秒)': 'period',
  '周期（秒）': 'period',
  '周期': 'period',
  '幅值': 'amplitude',
  '偏移': 'sine_offset',
  '步长': 'step',
  '下限': 'min',
  '上限': 'max',
  '系数': 'scale',
  '换算偏移': 'eng_offset',
  '偏移量': 'eng_offset',
  '备注': 'note',
}

/** 表头名映射生效所需的必需逻辑列 */
const REQUIRED_COLS = ['name', 'area', 'addr', 'type', 'source']

const AREA_CN_TO_KEY: Record<string, Area> = {
  '线圈': 'coil',
  '离散输入': 'discrete_input',
  '保持寄存器': 'holding_register',
  '输入寄存器': 'input_register',
  // 兼容英文枚举名
  coil: 'coil',
  discrete_input: 'discrete_input',
  holding_register: 'holding_register',
  input_register: 'input_register',
}

const SOURCE_CN_TO_KEY: Record<string, SourceKind> = {
  '静态': 'static',
  '正弦': 'sine',
  '斜坡': 'ramp',
  '随机游走': 'random_walk',
  '区间摆动': 'swing',
  static: 'static',
  sine: 'sine',
  ramp: 'ramp',
  random_walk: 'random_walk',
  swing: 'swing',
}

const AREA_KEY_TO_CN: Record<Area, string> = {
  coil: '线圈',
  discrete_input: '离散输入',
  holding_register: '保持寄存器',
  input_register: '输入寄存器',
}

const DATA_TYPES: DataType[] = ['u16', 'i16', 'u32', 'i32', 'f32', 'u64', 'i64', 'f64']

/** CSV 单行拆分（处理双引号包裹与转义） */
function splitCsvLine(line: string): string[] {
  const out: string[] = []
  let cur = ''
  let inQuote = false
  for (let i = 0; i < line.length; i++) {
    const c = line[i]
    if (inQuote) {
      if (c === '"') {
        if (line[i + 1] === '"') {
          cur += '"'
          i++
        } else {
          inQuote = false
        }
      } else {
        cur += c
      }
    } else if (c === '"') {
      inQuote = true
    } else if (c === ',') {
      out.push(cur)
      cur = ''
    } else {
      cur += c
    }
  }
  out.push(cur)
  return out
}

/** CSV 字段转义（含逗号/引号/换行时加引号包裹） */
function escapeCsvField(s: string): string {
  if (/[",\r\n]/.test(s)) return `"${s.replace(/"/g, '""')}"`
  return s
}

const trimNum = (x: number): string => (Number.isInteger(x) ? String(x) : String(x))

function rowToParams(source: ValueSource): string[] {
  const v = ['', '', '', '', '', '', ''] // 静态值/周期/幅值/偏移/步长/下限/上限
  switch (source.kind) {
    case 'static':
      v[0] = trimNum(source.value ?? 0)
      break
    case 'sine':
      v[1] = trimNum(source.period_s ?? 10)
      v[2] = trimNum(source.amplitude ?? 1)
      v[3] = trimNum(source.offset ?? 0)
      break
    case 'ramp':
    case 'random_walk':
      v[4] = trimNum(source.step ?? 1)
      v[5] = trimNum(source.min ?? 0)
      v[6] = trimNum(source.max ?? 100)
      break
    case 'swing':
      v[1] = trimNum(source.period_s ?? 10)
      v[5] = trimNum(source.min ?? 0)
      v[6] = trimNum(source.max ?? 100)
      break
  }
  return v
}

/** CSV 首行是否为表头行 */
function isHeaderLine(cells: string[]): boolean {
  const h = (cells[0] ?? '').trim()
  return h === '名称' || h.toLowerCase() === 'name' || h === '点名称'
}

/** 解析 CSV 文本（表头齐备必需列时按表头名映射，否则 15 列位置式；坏行收集错误不中断） */
export function parseCsvText(text: string): SheetParseResult {
  const lines = text.replace(/^\uFEFF/, '').split(/\r?\n/).filter((l) => l.trim() !== '')
  const result: SheetParseResult = { rows: [], errors: [] }
  if (lines.length === 0) return result

  // 表头 → 逻辑列 → 列下标映射；残缺表头（缺必需列）退回位置式并跳过首行（兼容旧行为）
  let start = 0
  let map: Record<string, number> | null = null
  const first = splitCsvLine(lines[0])
  if (isHeaderLine(first)) {
    start = 1
    const m: Record<string, number> = {}
    first.forEach((h, i) => {
      const col = HEADER_TO_COL[h.trim()]
      if (col && !(col in m)) m[col] = i
    })
    if (REQUIRED_COLS.every((c) => c in m)) map = m
  }

  for (let i = start; i < lines.length; i++) {
    const cells = splitCsvLine(lines[i])
    const line = i + 1
    // 按逻辑列取单元格文本（映射式缺列给空串 → 默认值）
    const col = (key: string): string => {
      if (map) return cells[map[key]] ?? ''
      return cells[COL_POSITION[key]] ?? ''
    }
    const name = col('name').trim()
    if (!name) {
      result.errors.push({ line, reason: '名称为空' })
      continue
    }
    const area = AREA_CN_TO_KEY[col('area').trim()]
    if (!area) {
      result.errors.push({ line, reason: `区域「${col('area')}」无法识别（可用：${Object.values(AREA_LABELS).join('/')}）` })
      continue
    }
    const address = Number(col('addr').trim())
    if (!Number.isInteger(address) || address < 0 || address > 65535) {
      result.errors.push({ line, reason: `地址「${col('addr')}」不是 0~65535 的整数` })
      continue
    }
    const dataType = col('type').trim().toLowerCase() as DataType
    if (!DATA_TYPES.includes(dataType)) {
      result.errors.push({ line, reason: `类型「${col('type')}」无法识别（可用：${DATA_TYPES.join('/')}）` })
      continue
    }
    const kind = SOURCE_CN_TO_KEY[col('source').trim()]
    if (!kind) {
      result.errors.push({ line, reason: `数据源「${col('source')}」无法识别（可用：${Object.values(SOURCE_LABELS).join('/')}）` })
      continue
    }
    // 参数列：表头缺列或空单元格用默认值；坏数字记行错误
    const num = (key: string, label: string, def: number): number => {
      const s = col(key).trim()
      if (s === '') return def
      const n = Number(s)
      if (Number.isNaN(n)) throw new Error(`${label}列「${s}」不是数字`)
      return n
    }
    let source: ValueSource
    let scale: number | undefined
    let offset: number | undefined
    try {
      source = { kind }
      if (kind === 'static') source.value = num('value', '静态值', 0)
      if (kind === 'sine' || kind === 'swing') source.period_s = num('period', '周期', 10)
      if (kind === 'sine') {
        source.amplitude = num('amplitude', '幅值', 1)
        source.offset = num('sine_offset', '偏移', 0)
      }
      if (kind === 'ramp' || kind === 'random_walk') source.step = num('step', '步长', 1)
      if (kind === 'ramp' || kind === 'random_walk' || kind === 'swing') {
        source.min = num('min', '下限', 0)
        source.max = num('max', '上限', 100)
      }
      scale = num('scale', '系数', 1)
      offset = num('eng_offset', '换算偏移', 0)
    } catch (e) {
      result.errors.push({ line, reason: e instanceof Error ? e.message : String(e) })
      continue
    }
    // 位区域是开关量不换算；系数 0 会除零
    if (area === 'coil' || area === 'discrete_input') {
      scale = 1
      offset = 0
    } else if (scale === 0) {
      result.errors.push({ line, reason: '系数不能为 0（工程值换算会除零）' })
      continue
    }
    result.rows.push({
      name,
      area,
      address,
      data_type: dataType,
      scale,
      offset,
      source,
      note: col('note').trim() || undefined,
    })
  }
  return result
}

/** 生成 CSV 文本（无 BOM；mock 下载用，桌面版由 Rust 端写 BOM） */
export function writeCsvText(rows: PointRow[]): string {
  const header = '名称,区域,地址,类型,数据源,静态值,周期(秒),幅值,偏移,步长,下限,上限,系数,换算偏移,备注'
  const lines = rows.map((r) => {
    // 换算列：位区域或恒等参数留空（导入按默认 1/0，表格干净）
    const identity = (r.scale ?? 1) === 1 && (r.offset ?? 0) === 0
    const isBit = r.area === 'coil' || r.area === 'discrete_input'
    const scaleCell = isBit || identity ? '' : trimNum(r.scale ?? 1)
    const offsetCell = isBit || identity ? '' : trimNum(r.offset ?? 0)
    return [
      r.name,
      AREA_KEY_TO_CN[r.area],
      String(r.address),
      r.data_type,
      SOURCE_LABELS[r.source.kind],
      ...rowToParams(r.source),
      scaleCell,
      offsetCell,
      r.note ?? '',
    ]
      .map(escapeCsvField)
      .join(',')
  })
  return [header, ...lines].join('\r\n')
}
