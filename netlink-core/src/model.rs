//! 领域模型：工程 / 连接 / 设备 / 点位。
//!
//! 序列化格式与前端 `web/src/types.ts` 严格同构（snake_case + 内部标签 `kind`），
//! 工程文件即本模型的 JSON 序列化。

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),
}

// ---------- 区域 ----------

/// Modbus 四区
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    Coil,
    DiscreteInput,
    HoldingRegister,
    InputRegister,
}

impl Area {
    /// 从机视角该区域是否可被主机写入
    pub fn writable(self) -> bool {
        matches!(self, Area::Coil | Area::HoldingRegister)
    }

    /// 是否为位区域（线圈/离散输入）
    pub fn is_bit(self) -> bool {
        matches!(self, Area::Coil | Area::DiscreteInput)
    }
}

// ---------- 数据类型 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    U16,
    I16,
    U32,
    I32,
    F32,
    U64,
    I64,
    F64,
}

impl DataType {
    /// 占用的 16 位寄存器个数（位区域不使用本值，见 Point::words）
    pub fn words(self) -> usize {
        match self {
            DataType::U16 | DataType::I16 => 1,
            DataType::U32 | DataType::I32 | DataType::F32 => 2,
            DataType::U64 | DataType::I64 | DataType::F64 => 4,
        }
    }
}

// ---------- 数据源 ----------

// 各波形参数的缺省值（容错：旧工程文件缺字段时不至于反序列化失败）
fn d_period() -> f64 {
    10.0
}
fn d_amplitude() -> f64 {
    1.0
}
fn d_step() -> f64 {
    1.0
}
fn d_min() -> f64 {
    0.0
}
fn d_max() -> f64 {
    100.0
}

/// 点位数据来源（内部标签 `kind`，与前端 ValueSource 同构）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValueSource {
    Static {
        value: f64,
    },
    Sine {
        #[serde(default = "d_period")]
        period_s: f64,
        #[serde(default = "d_amplitude")]
        amplitude: f64,
        #[serde(default)]
        offset: f64,
    },
    Ramp {
        #[serde(default = "d_step")]
        step: f64,
        #[serde(default = "d_min")]
        min: f64,
        #[serde(default = "d_max")]
        max: f64,
    },
    RandomWalk {
        #[serde(default = "d_step")]
        step: f64,
        #[serde(default = "d_min")]
        min: f64,
        #[serde(default = "d_max")]
        max: f64,
    },
    Swing {
        #[serde(default = "d_min")]
        min: f64,
        #[serde(default = "d_max")]
        max: f64,
        #[serde(default = "d_period")]
        period_s: f64,
    },
}

// ---------- 点位 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point {
    pub id: u64,
    pub name: String,
    pub area: Area,
    pub address: u16,
    pub data_type: DataType,
    pub source: ValueSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Point {
    /// 占用槽位个数：位区域 1 位；寄存器区域按数据类型占 N 个字
    pub fn slots(&self) -> usize {
        if self.area.is_bit() {
            1
        } else {
            self.data_type.words()
        }
    }
}

// ---------- 故障注入 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    Normal,
    Timeout,
    Delay,
    Exception,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaultConfig {
    pub kind: FaultKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exception_code: Option<u8>,
}

impl Default for FaultConfig {
    fn default() -> Self {
        Self {
            kind: FaultKind::Normal,
            delay_ms: None,
            exception_code: None,
        }
    }
}

// ---------- 设备 ----------

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: u64,
    pub name: String,
    pub slave_id: u8,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub fault: FaultConfig,
    /// 主机连接下的轮询周期（毫秒）；从机连接忽略此字段
    #[serde(default = "default_poll_period")]
    pub poll_period_ms: u32,
    pub points: Vec<Point>,
}

// ---------- 传输 / 连接 / 工程 ----------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parity {
    #[default]
    None,
    Even,
    Odd,
}

fn default_listen_addr() -> String {
    "0.0.0.0".to_string()
}
fn default_baud() -> u32 {
    9600
}
fn default_stop_bits() -> u8 {
    1
}
fn default_timeout_ms() -> u32 {
    1000
}
fn default_poll_period() -> u32 {
    1000
}

/// 一条总线的传输方式（内部标签 `kind`，与前端 Transport 同构）。
/// TcpServer/Serial 为从机（监听/串口应答）；TcpClient/SerialMaster 为主机（主动连远端/串口轮询）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transport {
    TcpServer {
        #[serde(default = "default_listen_addr")]
        listen_addr: String,
        port: u16,
    },
    Serial {
        serial_port: String,
        #[serde(default = "default_baud")]
        baud: u32,
        #[serde(default)]
        parity: Parity,
        #[serde(default = "default_stop_bits")]
        stop_bits: u8,
    },
    TcpClient {
        host: String,
        port: u16,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u32,
    },
    /// RTU 主机：独占串口按点表轮询远端从机
    SerialMaster {
        serial_port: String,
        #[serde(default = "default_baud")]
        baud: u32,
        #[serde(default)]
        parity: Parity,
        #[serde(default = "default_stop_bits")]
        stop_bits: u8,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub id: u64,
    pub name: String,
    pub transport: Transport,
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSpec {
    pub schema_version: u32,
    pub name: String,
    pub connections: Vec<Connection>,
}

impl ProjectSpec {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path)?;
        let spec: ProjectSpec = serde_json::from_str(&text)?;
        Ok(spec)
    }

    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// 与前端 mock 演示工程一致的默认工程
    pub fn demo() -> Self {
        let mut id = 100u64;

        fn pt(
            counter: &mut u64,
            name: &str,
            area: Area,
            address: u16,
            data_type: DataType,
            source: ValueSource,
        ) -> Point {
            *counter += 1;
            Point {
                id: *counter,
                name: name.to_string(),
                area,
                address,
                data_type,
                source,
                note: None,
            }
        }

        let ai = |counter: &mut u64, name: &str, addr: u16, source: ValueSource| {
            pt(counter, name, Area::InputRegister, addr, DataType::F32, source)
        };

        let dev1 = Device {
            id: 101,
            name: "1号测控装置".to_string(),
            slave_id: 1,
            enabled: true,
            fault: FaultConfig::default(),
            poll_period_ms: 1000,
            points: vec![
                ai(&mut id, "A相电压", 0, ValueSource::Sine { period_s: 60.0, amplitude: 5.0, offset: 220.0 }),
                ai(&mut id, "B相电压", 2, ValueSource::Sine { period_s: 60.0, amplitude: 5.0, offset: 220.0 }),
                ai(&mut id, "C相电压", 4, ValueSource::Sine { period_s: 60.0, amplitude: 5.0, offset: 220.0 }),
                ai(&mut id, "A相电流", 6, ValueSource::Sine { period_s: 20.0, amplitude: 30.0, offset: 55.0 }),
                ai(&mut id, "B相电流", 8, ValueSource::Sine { period_s: 20.0, amplitude: 30.0, offset: 52.0 }),
                ai(&mut id, "C相电流", 10, ValueSource::Sine { period_s: 20.0, amplitude: 30.0, offset: 57.0 }),
                ai(&mut id, "有功功率", 12, ValueSource::RandomWalk { step: 2.0, min: 0.0, max: 400.0 }),
                ai(&mut id, "无功功率", 14, ValueSource::RandomWalk { step: 1.0, min: 0.0, max: 150.0 }),
                ai(&mut id, "频率", 16, ValueSource::Sine { period_s: 30.0, amplitude: 0.2, offset: 50.0 }),
                ai(&mut id, "功率因数", 18, ValueSource::Swing { min: 0.85, max: 0.99, period_s: 45.0 }),
                ai(&mut id, "正向有功电能", 20, ValueSource::Ramp { step: 0.05, min: 0.0, max: 999999.0 }),
                ai(&mut id, "有功遥测", 24, ValueSource::Sine { period_s: 15.0, amplitude: 80.0, offset: 200.0 }),
                pt(&mut id, "遥信合位", Area::Coil, 0, DataType::U16, ValueSource::Static { value: 1.0 }),
                pt(&mut id, "遥信分位", Area::Coil, 1, DataType::U16, ValueSource::Static { value: 0.0 }),
                pt(&mut id, "远方/就地", Area::DiscreteInput, 0, DataType::U16, ValueSource::Static { value: 1.0 }),
                ai(&mut id, "温度", 30, ValueSource::Sine { period_s: 120.0, amplitude: 8.0, offset: 36.0 }),
                ai(&mut id, "湿度", 32, ValueSource::RandomWalk { step: 1.0, min: 30.0, max: 80.0 }),
                pt(&mut id, "有功设定", Area::HoldingRegister, 0, DataType::F32, ValueSource::Static { value: 250.0 }),
                pt(&mut id, "无功设定", Area::HoldingRegister, 2, DataType::F32, ValueSource::Static { value: 30.0 }),
            ],
        };

        let dev2 = Device {
            id: 102,
            name: "2号测控装置".to_string(),
            slave_id: 2,
            enabled: true,
            fault: FaultConfig {
                kind: FaultKind::Delay,
                delay_ms: Some(200),
                exception_code: None,
            },
            poll_period_ms: 1000,
            points: vec![
                ai(&mut id, "A相电压", 0, ValueSource::Sine { period_s: 50.0, amplitude: 3.0, offset: 221.0 }),
                ai(&mut id, "A相电流", 2, ValueSource::Sine { period_s: 25.0, amplitude: 20.0, offset: 40.0 }),
                ai(&mut id, "总有功", 4, ValueSource::RandomWalk { step: 3.0, min: 10.0, max: 200.0 }),
                ai(&mut id, "正向有功电能", 6, ValueSource::Ramp { step: 0.03, min: 0.0, max: 999999.0 }),
                pt(&mut id, "遥信合位", Area::Coil, 0, DataType::U16, ValueSource::Static { value: 1.0 }),
            ],
        };

        let dev3 = Device {
            id: 201,
            name: "1号电表".to_string(),
            slave_id: 1,
            enabled: false,
            fault: FaultConfig::default(),
            poll_period_ms: 1000,
            points: vec![
                ai(&mut id, "电压", 0, ValueSource::Sine { period_s: 40.0, amplitude: 4.0, offset: 220.0 }),
                ai(&mut id, "电流", 2, ValueSource::Sine { period_s: 18.0, amplitude: 10.0, offset: 15.0 }),
                ai(&mut id, "功率", 4, ValueSource::RandomWalk { step: 0.5, min: 1.0, max: 30.0 }),
                ai(&mut id, "电能量", 6, ValueSource::Ramp { step: 0.01, min: 0.0, max: 999999.0 }),
            ],
        };

        Self {
            schema_version: SCHEMA_VERSION,
            name: "演示工程 · 一号站".to_string(),
            connections: vec![
                Connection {
                    id: 1,
                    name: "TCP 主网 (502)".to_string(),
                    transport: Transport::TcpServer {
                        listen_addr: default_listen_addr(),
                        port: 502,
                    },
                    devices: vec![dev1, dev2],
                },
                Connection {
                    id: 2,
                    name: "485 总线 (COM3)".to_string(),
                    transport: Transport::Serial {
                        serial_port: "COM3".to_string(),
                        baud: 9600,
                        parity: Parity::None,
                        stop_bits: 1,
                    },
                    devices: vec![dev3],
                },
                // 主机连接：连本工具自己模拟的从机（127.0.0.1:502），
                // 演示「同一工程一边模拟从机、一边当主站打它」的自测玩法
                Connection {
                    id: 3,
                    name: "主机采集 → 本机从机".to_string(),
                    transport: Transport::TcpClient {
                        host: "127.0.0.1".to_string(),
                        port: 502,
                        timeout_ms: default_timeout_ms(),
                    },
                    devices: vec![Device {
                        id: 301,
                        name: "1号测控装置(采集)".to_string(),
                        slave_id: 1,
                        enabled: true,
                        fault: FaultConfig::default(),
                        poll_period_ms: 1000,
                        points: vec![
                            pt(&mut id, "A相电压", Area::InputRegister, 0, DataType::F32, ValueSource::Static { value: 0.0 }),
                            pt(&mut id, "有功遥测", Area::InputRegister, 24, DataType::F32, ValueSource::Static { value: 0.0 }),
                            pt(&mut id, "遥信合位", Area::Coil, 0, DataType::U16, ValueSource::Static { value: 0.0 }),
                            pt(&mut id, "有功设定", Area::HoldingRegister, 0, DataType::F32, ValueSource::Static { value: 0.0 }),
                        ],
                    }],
                },
            ],
        }
    }
}
