//! Tauri 全局状态。

use netlink_core::runtime::RuntimeManager;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

pub struct AppState {
    /// 当前运行的引擎实例（启动时重建，停止后置空）
    pub manager: AsyncMutex<Option<RuntimeManager>>,
    /// 前端当前选中设备（值推送任务只发这台，Arc 共享给后台任务）
    pub selected_device: Arc<AtomicU64>,
    /// 值推送/帧转发后台任务
    pub push_tasks: AsyncMutex<Vec<tokio::task::JoinHandle<()>>>,
    /// 当前工程文件路径（保存空路径时写这里；None = 尚未落盘的演示工程）
    pub current_project: Mutex<Option<PathBuf>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            manager: AsyncMutex::new(None),
            selected_device: Arc::new(AtomicU64::new(0)),
            push_tasks: AsyncMutex::new(Vec::new()),
            current_project: Mutex::new(None),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
