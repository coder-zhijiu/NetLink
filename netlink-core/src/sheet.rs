//! 点表导入导出：CSV / xlsx ↔ 点位行。
//!
//! 列格式（13 列，中文表头，Excel 友好）：
//! 名称 | 区域 | 地址 | 类型 | 数据源 | 静态值 | 周期(秒) | 幅值 | 偏移 | 步长 | 下限 | 上限 | 备注
//!
//! - 区域 / 数据源接受中文或英文枚举名（导出写中文）；
//! - 地址为 0 基十进制（与点表界面一致）；
//! - 数据源参数按类型取用对应列，空单元格用模型默认值；
//! - 解析容错：坏行不中断整批，逐行收集错误报告。

use std::io::Cursor;

use calamine::{Data as Cell, Range, Reader, Xlsx};
use serde::Serialize;

use crate::model::{Area, DataType as ModelDataType, Point, ValueSource};

/// 一行点位（Point 减 id；导入时由前端分配 id 保持全工程唯一）
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct PointRow {
    pub name: String,
    pub area: Area,
    pub address: u16,
    pub data_type: ModelDataType,
    pub source: ValueSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl From<&Point> for PointRow {
    fn from(p: &Point) -> Self {
        Self {
            name: p.name.clone(),
            area: p.area,
            address: p.address,
            data_type: p.data_type,
            source: p.source.clone(),
            note: p.note.clone(),
        }
    }
}

/// 解析结果：成功行 + 错误行报告（行号 1 起始，含表头行计数）
#[derive(Debug, Default, Serialize)]
pub struct SheetParseResult {
    pub rows: Vec<PointRow>,
    pub errors: Vec<SheetRowError>,
}

#[derive(Debug, Serialize)]
pub struct SheetRowError {
    /// 1 起始的物理行号（含表头）
    pub line: u32,
    pub reason: String,
}

// ---------- 列定义 ----------

pub const HEADERS: [&str; 13] = [
    "名称", "区域", "地址", "类型", "数据源", "静态值", "周期(秒)", "幅值", "偏移", "步长", "下限", "上限", "备注",
];

const AREA_CN: [(Area, &str); 4] = [
    (Area::Coil, "线圈"),
    (Area::DiscreteInput, "离散输入"),
    (Area::HoldingRegister, "保持寄存器"),
    (Area::InputRegister, "输入寄存器"),
];

const SOURCE_CN: [(&str, &str); 5] = [
    ("static", "静态"),
    ("sine", "正弦"),
    ("ramp", "斜坡"),
    ("random_walk", "随机游走"),
    ("swing", "区间摆动"),
];

fn area_label(a: Area) -> &'static str {
    AREA_CN.iter().find(|(k, _)| *k == a).map(|(_, v)| *v).unwrap_or("线圈")
}

fn source_label(kind: &str) -> &'static str {
    SOURCE_CN.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v).unwrap_or("静态")
}

/// 区域：中文或英文枚举名 → Area
fn parse_area(s: &str) -> Result<Area, String> {
    let t = s.trim();
    for (a, cn) in AREA_CN {
        if t == cn {
            return Ok(a);
        }
    }
    let lower = t.to_ascii_lowercase();
    match lower.as_str() {
        "coil" => Ok(Area::Coil),
        "discrete_input" | "discreteinput" => Ok(Area::DiscreteInput),
        "holding_register" | "holdingregister" => Ok(Area::HoldingRegister),
        "input_register" | "inputregister" => Ok(Area::InputRegister),
        _ => Err(format!("区域「{s}」无法识别（可用：线圈/离散输入/保持寄存器/输入寄存器）")),
    }
}

/// 数据类型：u16..f64
fn parse_data_type(s: &str) -> Result<ModelDataType, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "u16" => Ok(ModelDataType::U16),
        "i16" => Ok(ModelDataType::I16),
        "u32" => Ok(ModelDataType::U32),
        "i32" => Ok(ModelDataType::I32),
        "f32" => Ok(ModelDataType::F32),
        "u64" => Ok(ModelDataType::U64),
        "i64" => Ok(ModelDataType::I64),
        "f64" => Ok(ModelDataType::F64),
        _ => Err(format!("类型「{s}」无法识别（可用：u16/i16/u32/i32/f32/u64/i64/f64）")),
    }
}

/// 数据源：中文或英文 → 内部 kind 标签
fn parse_source_kind(s: &str) -> Result<String, String> {
    let t = s.trim();
    for (en, cn) in SOURCE_CN {
        if t == cn {
            return Ok(en.to_string());
        }
    }
    match t.to_ascii_lowercase().as_str() {
        "static" | "sine" | "ramp" | "random_walk" | "swing" => Ok(t.to_ascii_lowercase()),
        _ => Err(format!("数据源「{s}」无法识别（可用：静态/正弦/斜坡/随机游走/区间摆动）")),
    }
}

/// 取单元格文本（缺失/越界返回空串）
fn cell(cells: &[String], idx: usize) -> &str {
    cells.get(idx).map(|s| s.as_str()).unwrap_or("")
}

fn parse_u16(s: &str) -> Result<u16, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("地址列为空".to_string());
    }
    t.parse::<u16>().map_err(|_| format!("地址「{s}」不是 0~65535 的整数"))
}

// ---------- 行 ↔ 单元格 ----------

/// 一行字符串单元格 → PointRow（失败给出人话原因）
pub fn row_to_point(cells: &[String]) -> Result<PointRow, String> {
    let name = cell(cells, 0).trim().to_string();
    if name.is_empty() {
        return Err("名称为空".to_string());
    }
    let area = parse_area(cell(cells, 1))?;
    let address = parse_u16(cell(cells, 2))?;
    let data_type = parse_data_type(cell(cells, 3))?;
    let kind = parse_source_kind(cell(cells, 4))?;

    // 参数列按数据源取用；空单元格用模型默认值，坏数字报行错误（不静默换默认）
    let get_f = |idx: usize, col: &str, default: f64| -> Result<f64, String> {
        let s = cell(cells, idx).trim();
        if s.is_empty() {
            return Ok(default);
        }
        s.parse::<f64>().map_err(|_| format!("{col}列「{s}」不是数字"))
    };
    let value = get_f(5, "静态值", 0.0)?;
    let period = get_f(6, "周期", 10.0)?;
    let amplitude = get_f(7, "幅值", 1.0)?;
    let offset = get_f(8, "偏移", 0.0)?;
    let step = get_f(9, "步长", 1.0)?;
    let min = get_f(10, "下限", 0.0)?;
    let max = get_f(11, "上限", 100.0)?;

    let source = match kind.as_str() {
        "static" => ValueSource::Static { value },
        "sine" => ValueSource::Sine { period_s: period, amplitude, offset },
        "ramp" => ValueSource::Ramp { step, min, max },
        "random_walk" => ValueSource::RandomWalk { step, min, max },
        "swing" => ValueSource::Swing { min, max, period_s: period },
        _ => unreachable!(),
    };

    let note = cell(cells, 12).trim();
    Ok(PointRow {
        name,
        area,
        address,
        data_type,
        source,
        note: if note.is_empty() { None } else { Some(note.to_string()) },
    })
}

/// PointRow → 一行单元格（含备注）
pub fn point_to_cells(r: &PointRow) -> Vec<String> {
    let mut cells = vec![
        r.name.clone(),
        area_label(r.area).to_string(),
        r.address.to_string(),
        format!("{:?}", r.data_type).to_lowercase(),
        source_label(source_kind(&r.source)).to_string(),
    ];
    let params = source_params(&r.source);
    cells.extend(params);
    cells.push(r.note.clone().unwrap_or_default());
    cells
}

fn source_kind(s: &ValueSource) -> &'static str {
    match s {
        ValueSource::Static { .. } => "static",
        ValueSource::Sine { .. } => "sine",
        ValueSource::Ramp { .. } => "ramp",
        ValueSource::RandomWalk { .. } => "random_walk",
        ValueSource::Swing { .. } => "swing",
    }
}

/// 数据源 → 参数列（静态值/周期/幅值/偏移/步长/下限/上限，空位补空串）
fn source_params(s: &ValueSource) -> Vec<String> {
    let mut v = vec![String::new(); 7]; // 5..11 共 7 个参数列
    let fmt = |x: f64| trim_num(x);
    match s {
        ValueSource::Static { value } => v[0] = fmt(*value),
        ValueSource::Sine { period_s, amplitude, offset } => {
            v[1] = fmt(*period_s);
            v[2] = fmt(*amplitude);
            v[3] = fmt(*offset);
        }
        ValueSource::Ramp { step, min, max } | ValueSource::RandomWalk { step, min, max } => {
            v[4] = fmt(*step);
            v[5] = fmt(*min);
            v[6] = fmt(*max);
        }
        ValueSource::Swing { min, max, period_s } => {
            v[1] = fmt(*period_s);
            v[5] = fmt(*min);
            v[6] = fmt(*max);
        }
    }
    v
}

/// 数值文本化：整数不带小数点（Excel 里更干净）
fn trim_num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

// ---------- 表级：字符串行集合 ↔ SheetParseResult ----------

/// 全部字符串行 → 解析结果。首行若是表头（第 1 列为「名称」或 name）则跳过。
pub fn parse_sheet(raw_rows: &[Vec<String>]) -> SheetParseResult {
    let mut result = SheetParseResult::default();
    let start = raw_rows
        .first()
        .map(|first| {
            let h = cell(first, 0).trim();
            h == "名称" || h.eq_ignore_ascii_case("name") || h == "点名称"
        })
        .unwrap_or(false) as usize;
    for (i, cells) in raw_rows.iter().enumerate().skip(start) {
        let line = (i + 1) as u32; // 物理行号（含表头，1 起始）
        if cells.iter().all(|c| c.trim().is_empty()) {
            continue; // 空行跳过
        }
        match row_to_point(cells) {
            Ok(row) => result.rows.push(row),
            Err(reason) => result.errors.push(SheetRowError { line, reason }),
        }
    }
    result
}

/// 点位行集合 → 全部单元格行（含表头）
pub fn rows_to_cells(rows: &[PointRow]) -> Vec<Vec<String>> {
    let mut out = vec![HEADERS.iter().map(|s| s.to_string()).collect()];
    out.extend(rows.iter().map(point_to_cells));
    out
}

// ---------- CSV ----------

/// 解析 CSV 字节（自动去 UTF-8 BOM；坏记录（引号未闭合等）记为错误行不中断）
pub fn parse_csv(bytes: &[u8]) -> SheetParseResult {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut raw_rows: Vec<Vec<String>> = Vec::new();
    let mut record_errors: Vec<SheetRowError> = Vec::new();
    for rec in reader.records() {
        match rec {
            Ok(r) => raw_rows.push(r.iter().map(|s| s.to_string()).collect()),
            Err(e) => {
                let line = (raw_rows.len() + 1) as u32;
                record_errors.push(SheetRowError { line, reason: format!("CSV 记录损坏: {e}") });
                raw_rows.push(Vec::new()); // 占位保持行号对齐
            }
        }
    }
    let mut result = parse_sheet(&raw_rows);
    result.errors.extend(record_errors);
    result
}

/// 写 CSV 字节（带 UTF-8 BOM，Excel 直接打开中文不乱码）
pub fn write_csv(rows: &[PointRow]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice("\u{feff}".as_bytes());
    {
        let mut writer = csv::WriterBuilder::new().from_writer(&mut out);
        for cells in rows_to_cells(rows) {
            // csv crate 处理转义；表头行是 &str，数据行是 String
            let _ = writer.write_record(cells);
        }
        let _ = writer.flush();
    }
    out
}

// ---------- xlsx ----------

/// 解析 xlsx 字节（取第一个工作表）
pub fn parse_xlsx(bytes: &[u8]) -> Result<SheetParseResult, String> {
    let mut book: Xlsx<_> = Xlsx::new(Cursor::new(bytes))
        .map_err(|e| format!("打开 xlsx 失败: {e}"))?;
    let range: Range<Cell> = book
        .worksheet_range(book.sheet_names().first().map(|s| s.as_str()).unwrap_or("Sheet1"))
        .map_err(|e| format!("读取工作表失败: {e}"))?;
    let raw_rows: Vec<Vec<String>> = range
        .rows()
        .map(|row| row.iter().map(cell_to_string).collect())
        .collect();
    Ok(parse_sheet(&raw_rows))
}

/// 单元格值 → 字本（数字整值不带小数点，空单元格给空串）
fn cell_to_string(c: &Cell) -> String {
    match c {
        Cell::Empty => String::new(),
        Cell::String(s) => s.clone(),
        Cell::Float(f) => trim_num(*f),
        Cell::Int(i) => i.to_string(),
        Cell::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// 写 xlsx 字节（内存缓冲，单工作表「点表」）
pub fn write_xlsx(rows: &[PointRow]) -> Result<Vec<u8>, String> {
    use rust_xlsxwriter::{Format, Workbook};
    let mut book = Workbook::new();
    let sheet = book.add_worksheet().set_name("点表").map_err(|e| e.to_string())?;
    // 列宽：名称/参数列适中
    let widths = [22.0, 12.0, 8.0, 8.0, 12.0, 10.0, 10.0, 8.0, 8.0, 8.0, 8.0, 10.0, 24.0];
    for (i, w) in widths.iter().enumerate() {
        let _ = sheet.set_column_width(i as u16, *w);
    }
    let plain = Format::new();
    for (ri, cells) in rows_to_cells(rows).iter().enumerate() {
        for (ci, v) in cells.iter().enumerate() {
            // 全部按文本写入：地址/参数保持显示原样，避免 Excel 数字精度问题
            sheet
                .write_string(ri as u32, ci as u16, v, &plain)
                .map_err(|e| format!("写 xlsx 失败: {e}"))?;
        }
    }
    book.save_to_buffer().map_err(|e| format!("生成 xlsx 失败: {e}"))
}

// ---------- 测试 ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<PointRow> {
        vec![
            PointRow {
                name: "A相电压".to_string(),
                area: Area::InputRegister,
                address: 0,
                data_type: ModelDataType::F32,
                source: ValueSource::Sine { period_s: 60.0, amplitude: 5.0, offset: 220.0 },
                note: Some("进线".to_string()),
            },
            PointRow {
                name: "遥信合位".to_string(),
                area: Area::Coil,
                address: 1,
                data_type: ModelDataType::U16,
                source: ValueSource::Static { value: 1.0 },
                note: None,
            },
            PointRow {
                name: "有功电能".to_string(),
                area: Area::HoldingRegister,
                address: 20,
                data_type: ModelDataType::F64,
                source: ValueSource::Ramp { step: 0.05, min: 0.0, max: 999999.0 },
                note: None,
            },
        ]
    }

    #[test]
    fn csv_roundtrip() {
        let bytes = write_csv(&sample());
        assert!(bytes.starts_with("\u{feff}".as_bytes()));
        let parsed = parse_csv(&bytes);
        assert!(parsed.errors.is_empty());
        assert_eq!(parsed.rows, sample());
    }

    #[test]
    fn xlsx_roundtrip() {
        let bytes = write_xlsx(&sample()).unwrap();
        let parsed = parse_xlsx(&bytes).unwrap();
        assert!(parsed.errors.is_empty());
        assert_eq!(parsed.rows.len(), 3);
        // 局部核对（Float↔String 途径的数值文本化已在 cell_to_string/trim_num 覆盖）
        assert_eq!(parsed.rows[0].name, "A相电压");
        assert_eq!(parsed.rows[0].area, Area::InputRegister);
        assert_eq!(parsed.rows[1].source, ValueSource::Static { value: 1.0 });
    }

    #[test]
    fn csv_write_then_manual_parse() {
        // 导出的 CSV 手动核对列内容（表头 + 数据行）
        let bytes = write_csv(&sample());
        let text = String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "名称,区域,地址,类型,数据源,静态值,周期(秒),幅值,偏移,步长,下限,上限,备注");
        assert_eq!(lines[1], "A相电压,输入寄存器,0,f32,正弦,,60,5,220,,,,进线");
        assert_eq!(lines[2], "遥信合位,线圈,1,u16,静态,1,,,,,,,");
    }

    #[test]
    fn bad_rows_reported_not_fatal() {
        let raw = vec![
            vec!["名称".to_string()], // 表头（只有第一列也对）
            vec!["好点位".to_string(), "线圈".to_string(), "5".to_string(), "u16".to_string(), "静态".to_string(), "1".to_string()],
            vec!["坏区域".to_string(), "不存在的区".to_string(), "1".to_string(), "u16".to_string(), "静态".to_string(), "1".to_string()],
            vec!["坏地址".to_string(), "线圈".to_string(), "七万".to_string(), "u16".to_string(), "静态".to_string(), "1".to_string()],
            vec![], // 空行
            vec!["英文区域".to_string(), "input_register".to_string(), "8".to_string(), "f32".to_string(), "random_walk".to_string(), "".to_string(), "".to_string(), "".to_string(), "".to_string(), "2".to_string(), "0".to_string(), "80".to_string()],
        ];
        let r = parse_sheet(&raw);
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.errors.len(), 2);
        assert_eq!(r.errors[0].line, 3);
        assert!(r.errors[0].reason.contains("不存在的区"));
        assert!(r.errors[1].reason.contains("七万"));
        assert_eq!(r.rows[1].area, Area::InputRegister);
        assert_eq!(r.rows[1].source, ValueSource::RandomWalk { step: 2.0, min: 0.0, max: 80.0 });
    }

    #[test]
    fn default_params_fill() {
        // 参数列空 → 模型默认（周期 10 / 幅值 1 / 偏移 0 / 步长 1 / 范围 0..100）
        let raw = vec![vec!["v".to_string(), "输入寄存器".to_string(), "0".to_string(), "f32".to_string(), "正弦".to_string()]];
        let r = parse_sheet(&raw);
        assert_eq!(r.rows[0].source, ValueSource::Sine { period_s: 10.0, amplitude: 1.0, offset: 0.0 });
    }
}
