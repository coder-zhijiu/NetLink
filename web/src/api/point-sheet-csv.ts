import type { Area, DataType, PointRow, SheetParseResult, SourceKind, ValueSource } from '../types'
import { AREA_LABELS, SOURCE_LABELS } from '../types'

/**
 * 浏览器演示模式（mock）的点表 CSV 文本解析/生成。
 * 列格式与 netlink-core/src/sheet.rs 严格一致：
 * 名称,区域,地址,类型,数据源,静态值,周期(秒),幅值,偏移,步长,下限,上限,备注
 */

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

/** 解析 CSV 文本（表头自动跳过；坏行收集错误不中断） */
export function parseCsvText(text: string): SheetParseResult {
  const lines = text.replace(/^\uFEFF/, '').split(/\r?\n/).filter((l) => l.trim() !== '')
  const result: SheetParseResult = { rows: [], errors: [] }
  const first = lines[0]?.split(',')[0]?.trim() ?? ''
  const start = first === '名称' || first.toLowerCase() === 'name' ? 1 : 0
  for (let i = start; i < lines.length; i++) {
    const cells = splitCsvLine(lines[i])
    const line = i + 1
    const name = (cells[0] ?? '').trim()
    if (!name) {
      result.errors.push({ line, reason: '名称为空' })
      continue
    }
    const area = AREA_CN_TO_KEY[(cells[1] ?? '').trim()]
    if (!area) {
      result.errors.push({ line, reason: `区域「${cells[1] ?? ''}」无法识别（可用：${Object.values(AREA_LABELS).join('/')}）` })
      continue
    }
    const address = Number((cells[2] ?? '').trim())
    if (!Number.isInteger(address) || address < 0 || address > 65535) {
      result.errors.push({ line, reason: `地址「${cells[2] ?? ''}」不是 0~65535 的整数` })
      continue
    }
    const dataType = (cells[3] ?? '').trim().toLowerCase() as DataType
    if (!DATA_TYPES.includes(dataType)) {
      result.errors.push({ line, reason: `类型「${cells[3] ?? ''}」无法识别（可用：${DATA_TYPES.join('/')}）` })
      continue
    }
    const kind = SOURCE_CN_TO_KEY[(cells[4] ?? '').trim()]
    if (!kind) {
      result.errors.push({ line, reason: `数据源「${cells[4] ?? ''}」无法识别（可用：${Object.values(SOURCE_LABELS).join('/')}）` })
      continue
    }
    // 参数列：空单元格用默认值；坏数字记行错误
    const num = (idx: number, col: string, def: number): number => {
      const s = (cells[idx] ?? '').trim()
      if (s === '') return def
      const n = Number(s)
      if (Number.isNaN(n)) throw new Error(`${col}列「${s}」不是数字`)
      return n
    }
    let source: ValueSource
    try {
      source = { kind }
      if (kind === 'static') source.value = num(5, '静态值', 0)
      if (kind === 'sine' || kind === 'swing') source.period_s = num(6, '周期', 10)
      if (kind === 'sine') {
        source.amplitude = num(7, '幅值', 1)
        source.offset = num(8, '偏移', 0)
      }
      if (kind === 'ramp' || kind === 'random_walk') source.step = num(9, '步长', 1)
      if (kind === 'ramp' || kind === 'random_walk' || kind === 'swing') {
        source.min = num(10, '下限', 0)
        source.max = num(11, '上限', 100)
      }
    } catch (e) {
      result.errors.push({ line, reason: e instanceof Error ? e.message : String(e) })
      continue
    }
    result.rows.push({
      name,
      area,
      address,
      data_type: dataType,
      source,
      note: (cells[12] ?? '').trim() || undefined,
    })
  }
  return result
}

/** 生成 CSV 文本（无 BOM；mock 下载用，桌面版由 Rust 端写 BOM） */
export function writeCsvText(rows: PointRow[]): string {
  const header = '名称,区域,地址,类型,数据源,静态值,周期(秒),幅值,偏移,步长,下限,上限,备注'
  const lines = rows.map((r) =>
    [r.name, AREA_KEY_TO_CN[r.area], String(r.address), r.data_type, SOURCE_LABELS[r.source.kind], ...rowToParams(r.source), r.note ?? '']
      .map(escapeCsvField)
      .join(','),
  )
  return [header, ...lines].join('\r\n')
}
