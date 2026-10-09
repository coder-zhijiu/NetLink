//! netlink-core：与 UI 无关的协议引擎核心库。
//!
//! 分层：
//! - [`model`]：工程/连接/设备/点位的领域模型与 JSON 持久化（与前端 types.ts 同构）
//! - [`sim`]：数据仿真（波形求值）
//! - [`runtime`]：运行时管理（仿真时钟、设备状态缓存、寄存器块编码）
//! - [`check`]：连接编辑器的连通性 / 端口占用检测
//! - [`sheet`]：点表 CSV/xlsx 导入导出（行列转换与校验）
//! - [`protocol`]：各协议实现（Modbus TCP/RTU 从机先行，后续 104、645）

pub mod check;
pub mod model;
pub mod protocol;
pub mod runtime;
pub mod sheet;
pub mod sim;
