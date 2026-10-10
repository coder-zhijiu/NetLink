//! 领域模型：工程 / 连接 / 设备 / 点位。
//!
//! 序列化格式与前端 `web/src/types.ts` 严格同构（snake_case + 内部标签 `kind`），
//! 工程文件即本模型的 JSON 序列化。

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 3;

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
    /// 脏帧：应答 CRC 故意破坏（仅 RTU 有意义；TCP 无校验和，按正常应答兜底）
    Corrupt,
    /// 断连：收到请求后关闭 TCP 连接（仅 TCP 有意义；串口无连接概念，按不应答兜底）
    Disconnect,
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

// ---------- CAN 节点与周期报文 ----------

fn d_can_period_ms() -> u32 {
    1000
}

/// CAN 周期报文定义（CAN 连接下的「点位」层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanPeriodicFrame {
    pub id: u64,
    /// 0x000~0x7FF 标准 / 0x00000000~0x1FFFFFFF 扩展
    pub frame_id: u32,
    pub extended: bool,
    /// 远程帧（数据域为空）
    pub remote: bool,
    /// ≤8 字节；dlc = data.len()
    #[serde(default)]
    pub data: Vec<u8>,
    /// 发送周期（毫秒）；0 = 不自动发（仅保存定义，手动发送面板可载入）
    #[serde(default = "d_can_period_ms")]
    pub period_ms: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CanPeriodicFrame {
    /// 帧定义合法性：ID 域宽 / 数据长度 / 远程帧数据域
    pub fn validate(&self) -> Result<(), String> {
        let max = if self.extended { 0x1FFF_FFFF } else { 0x7FF };
        if self.frame_id > max {
            return Err(format!("帧 ID 0x{:X} 超出{}域宽（最大 0x{:X}）", self.frame_id, if self.extended { "扩展" } else { "标准" }, max));
        }
        if self.data.len() > 8 {
            return Err(format!("数据域 {} 字节超长（CAN 经典帧最多 8）", self.data.len()));
        }
        if self.remote && !self.data.is_empty() {
            return Err("远程帧不带数据域".to_string());
        }
        Ok(())
    }
}

/// CAN 节点（CAN 连接下的「设备」层：一组周期发送报文；接收侧是运行时聚合不进工程）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanNode {
    pub id: u64,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub frames: Vec<CanPeriodicFrame>,
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
fn default_can_backend() -> String {
    "slcan".to_string()
}
fn default_device_index() -> u32 {
    0
}
fn default_can_channel() -> u8 {
    0
}
fn default_can_bitrate() -> u32 {
    500_000
}
fn default_serial_baud() -> u32 {
    115_200
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
    /// CAN 直采直控：USB-CAN 适配器直连总线（被动监听全量报文 + 手动/周期发送）
    UsbCan {
        /// "slcan"（v1 已实现）；"zlgcan" / "candle" 待硬件到货启用
        #[serde(default = "default_can_backend")]
        backend: String,
        /// zlgcan 设备号；candle 通道路径序号（slcan 忽略）
        #[serde(default = "default_device_index")]
        device_index: u32,
        /// CAN 通道号（双通道设备 0/1）
        #[serde(default = "default_can_channel")]
        channel: u8,
        /// 常用档位 125k/250k/500k/1M；zlgcan 内部换算 Timing0/1（M2）
        #[serde(default = "default_can_bitrate")]
        bitrate: u32,
        /// slcan 后端的串口名（其余后端忽略）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serial_port: Option<String>,
        /// slcan 后端的串口侧波特率（适配器固件常为 115200）
        #[serde(default = "default_serial_baud")]
        serial_baud: u32,
    },
    /// CAN 转以太网：TCP 连接 ZLG CANET 网关收发 CAN 帧（M3 实现；v3 schema 先占位）
    CanNet {
        host: String,
        port: u16,
        /// 映射 CANET 的哪路 CAN
        #[serde(default = "default_can_channel")]
        channel: u8,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub id: u64,
    pub name: String,
    pub transport: Transport,
    #[serde(default)]
    pub devices: Vec<Device>,
    /// CAN 连接下的节点（替代「设备」层；Modbus 连接恒为空）
    #[serde(default)]
    pub can_nodes: Vec<CanNode>,
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
                    can_nodes: vec![],
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
                    can_nodes: vec![],
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
                    can_nodes: vec![],
                },
                // CAN 直采直控示例：slcan 后端（CANable 等）；串口名按实际适配器修改后启动
                Connection {
                    id: 4,
                    name: "CAN 总线 (slcan COM5)".to_string(),
                    transport: Transport::UsbCan {
                        backend: default_can_backend(),
                        device_index: 0,
                        channel: 0,
                        bitrate: 500_000,
                        serial_port: Some("COM5".to_string()),
                        serial_baud: default_serial_baud(),
                    },
                    devices: vec![],
                    can_nodes: vec![
                        CanNode {
                            id: 401,
                            name: "1号CAN节点".to_string(),
                            enabled: true,
                            frames: vec![
                                CanPeriodicFrame {
                                    id: 4011,
                                    frame_id: 0x123,
                                    extended: false,
                                    remote: false,
                                    data: vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
                                    period_ms: 1000,
                                    enabled: true,
                                    note: Some("状态周期帧".to_string()),
                                },
                                CanPeriodicFrame {
                                    id: 4012,
                                    frame_id: 0x201,
                                    extended: false,
                                    remote: false,
                                    data: vec![0x01, 0x02],
                                    period_ms: 250,
                                    enabled: true,
                                    note: Some("快速心跳".to_string()),
                                },
                            ],
                        },
                        CanNode {
                            id: 402,
                            name: "2号CAN节点".to_string(),
                            enabled: false,
                            frames: vec![CanPeriodicFrame {
                                id: 4021,
                                frame_id: 0x18FF0102,
                                extended: true,
                                remote: false,
                                data: vec![0xAA, 0x55],
                                period_ms: 2000,
                                enabled: true,
                                note: None,
                            }],
                        },
                    ],
                },
            ],
        }
    }
}
