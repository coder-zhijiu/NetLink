//! 前端可调用命令。

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

use netlink_core::check::{test_transport, TestConnResult};
use netlink_core::model::{ProjectSpec, Transport, SCHEMA_VERSION};
use netlink_core::runtime::{RuntimeManager, TICK_INTERVAL};
use serde::Serialize;
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

const UPDATE_EVENT: &str = "netlink://update";

// ---------- 工程存储 ----------

/// 工程目录：exe 所在目录下的 projects/（工程随安装目录走，不进 AppData）。
/// NETLINK_PROJECTS_DIR 环境变量可覆盖（测试用）。
fn projects_dir() -> Result<PathBuf, String> {
    if let Ok(v) = std::env::var("NETLINK_PROJECTS_DIR") {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    let exe = std::env::current_exe().map_err(|e| format!("取 exe 路径失败: {e}"))?;
    exe.parent().map(|d| d.join("projects")).ok_or_else(|| "取 exe 目录失败".to_string())
}

/// 当前工程指针文件（工程目录内，内容 = 工程文件名），重启时恢复上次打开的工程
const CURRENT_FILE: &str = "current";

/// 工程名 → 安全文件名主干（Windows 非法字符/控制符换 _，去首尾点与空格）
fn sanitize_stem(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            let bad = matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
                || (c as u32) < 0x20;
            if bad { '_' } else { c }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == ' ').to_string();
    if trimmed.is_empty() {
        "未命名工程".to_string()
    } else {
        trimmed
    }
}

fn is_windows_reserved(stem: &str) -> bool {
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
            | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" | "COM6" | "COM7" | "COM8" | "COM9"
            | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    )
}

/// 在工程目录下分配不冲突的文件名：name.json、name-2.json、name-3.json…
fn allocate_file_name(dir: &Path, name: &str) -> String {
    let mut stem = sanitize_stem(name);
    if is_windows_reserved(&stem) {
        stem.push('_');
    }
    let mut file_name = format!("{stem}.json");
    let mut n = 1;
    while dir.join(&file_name).exists() {
        n += 1;
        file_name = format!("{stem}-{n}.json");
    }
    file_name
}

/// 工程目录里的一个工程（列表用）
#[derive(Serialize, Clone)]
pub struct ProjectMeta {
    /// 工程目录内的文件名（前端切换工程用它）
    pub file_name: String,
    /// 工程名（取自工程文件 name 字段；解析失败退化为文件名主干）
    pub name: String,
    /// 最后修改时间（Unix 毫秒，列表按此倒序）
    pub modified_at: u64,
}

/// 加载/新建/保存工程返回的句柄：工程数据 + 落盘位置
#[derive(Serialize, Clone)]
pub struct ProjectHandle {
    /// 工程文件绝对路径；None = 尚未落盘的演示工程
    pub path: Option<String>,
    pub spec: ProjectSpec,
}

/// 记录当前工程：后端状态（保存空路径用）+ 指针文件（重启恢复）
fn set_current(state: &AppState, path: &Path) -> Result<(), String> {
    let dir = projects_dir()?;
    *state.current_project.lock().expect("current_project 锁中毒") = Some(path.to_path_buf());
    // 只在工程位于工程目录内时更新指针（外部路径的工程不参与重启恢复）
    if path.parent() == Some(&dir) {
        if let Some(name) = path.file_name() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("创建工程目录失败: {e}"))?;
            std::fs::write(dir.join(CURRENT_FILE), name.to_string_lossy().as_bytes())
                .map_err(|e| format!("写入指针文件失败: {e}"))?;
        }
    }
    Ok(())
}

/// 一次性迁移旧版数据（%APPDATA%\NetLink\project.json → 工程目录），已有工程时跳过
fn migrate_legacy(dir: &Path) {
    let has_project = std::fs::read_dir(dir)
        .map(|entries| entries.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".json")))
        .unwrap_or(false);
    if has_project {
        return;
    }
    let legacy = std::env::var("APPDATA")
        .ok()
        .map(|v| PathBuf::from(v).join("NetLink").join("project.json"))
        .filter(|p| p.exists());
    let Some(legacy) = legacy else { return };
    if let Ok(spec) = ProjectSpec::load(&legacy) {
        if std::fs::create_dir_all(dir).is_ok() {
            let file_name = allocate_file_name(dir, &spec.name);
            if spec.save(&dir.join(&file_name)).is_ok() {
                let _ = std::fs::write(dir.join(CURRENT_FILE), &file_name);
            }
        }
    }
}

fn do_list() -> Result<Vec<ProjectMeta>, String> {
    let dir = projects_dir()?;
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Ok(out),
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.ends_with(".json") {
            continue;
        }
        let modified_at = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // 工程名解析失败（文件损坏）不隐藏该工程：退化为文件名主干，打开时给出具体错误
        let name = ProjectSpec::load(&entry.path())
            .map(|p| p.name)
            .unwrap_or_else(|_| file_name.trim_end_matches(".json").to_string());
        out.push(ProjectMeta { file_name, name, modified_at });
    }
    out.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    Ok(out)
}

/// 启动恢复：优先指针指向的工程
fn load_current(state: &AppState, dir: &Path) -> Result<Option<ProjectHandle>, String> {
    let Ok(pointer) = std::fs::read_to_string(dir.join(CURRENT_FILE)) else {
        return Ok(None);
    };
    let file_name = pointer.trim();
    if file_name.is_empty() {
        return Ok(None);
    }
    let path = dir.join(file_name);
    if !path.is_file() {
        return Ok(None);
    }
    let spec = ProjectSpec::load(&path).map_err(|e| format!("工程文件损坏（{}）: {e}", path.display()))?;
    *state.current_project.lock().expect("current_project 锁中毒") = Some(path.clone());
    Ok(Some(ProjectHandle { path: Some(path.to_string_lossy().to_string()), spec }))
}

fn do_load(state: &AppState, path: Option<&str>) -> Result<ProjectHandle, String> {
    match path.filter(|p| !p.is_empty()) {
        Some(p) => {
            let path = PathBuf::from(p);
            let spec = ProjectSpec::load(&path).map_err(|e| format!("工程文件读取失败（{p}）: {e}"))?;
            set_current(state, &path)?;
            Ok(ProjectHandle { path: Some(p.to_string()), spec })
        }
        None => {
            let dir = projects_dir()?;
            migrate_legacy(&dir);
            if let Some(h) = load_current(state, &dir)? {
                return Ok(h);
            }
            // 工程目录还没有任何工程：给演示工程（点「保存」时才落盘）
            Ok(ProjectHandle { path: None, spec: ProjectSpec::demo() })
        }
    }
}

fn do_new(state: &AppState, name: &str) -> Result<ProjectHandle, String> {
    let name = name.trim();
    let name = if name.is_empty() { "未命名工程" } else { name };
    let dir = projects_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建工程目录失败: {e}"))?;
    let path = dir.join(allocate_file_name(&dir, name));
    let spec = ProjectSpec {
        schema_version: SCHEMA_VERSION,
        name: name.to_string(),
        connections: Vec::new(),
    };
    spec.save(&path).map_err(|e| e.to_string())?;
    set_current(state, &path)?;
    Ok(ProjectHandle { path: Some(path.to_string_lossy().to_string()), spec })
}

fn do_open(state: &AppState, file_name: &str) -> Result<ProjectHandle, String> {
    // 只接受工程目录内的纯文件名（防路径穿越）
    if file_name.is_empty() || Path::new(file_name).components().count() != 1 {
        return Err("非法工程文件名".to_string());
    }
    let dir = projects_dir()?;
    let path = dir.join(file_name);
    let spec = ProjectSpec::load(&path).map_err(|e| format!("工程文件读取失败（{file_name}）: {e}"))?;
    set_current(state, &path)?;
    Ok(ProjectHandle { path: Some(path.to_string_lossy().to_string()), spec })
}

fn do_save(state: &AppState, path: &str, project: ProjectSpec) -> Result<ProjectHandle, String> {
    // 空路径 = 保存到当前工程文件；当前是未落盘的演示工程时按工程名新建文件
    let target = if !path.is_empty() {
        PathBuf::from(path)
    } else {
        match state.current_project.lock().expect("current_project 锁中毒").clone() {
            Some(p) => p,
            None => {
                let dir = projects_dir()?;
                std::fs::create_dir_all(&dir).map_err(|e| format!("创建工程目录失败: {e}"))?;
                dir.join(allocate_file_name(&dir, &project.name))
            }
        }
    };
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    project.save(&target).map_err(|e| e.to_string())?;
    set_current(state, &target)?;
    Ok(ProjectHandle { path: Some(target.to_string_lossy().to_string()), spec: project })
}

/// 启动恢复 / 显式路径加载（前端启动时无参调用）。
/// async：Tauri 的同步 command 在 WebView 主线程执行，一个慢命令会饿死全部
/// 后续 invoke（前端表现为「加载中」永远挂着）——文件类命令一律 async 化。
#[tauri::command]
pub async fn load_project(state: State<'_, AppState>, path: Option<String>) -> Result<ProjectHandle, String> {
    do_load(state.inner(), path.as_deref())
}

/// 列出工程目录里的全部工程（按修改时间倒序）
#[tauri::command]
pub async fn list_projects() -> Result<Vec<ProjectMeta>, String> {
    do_list()
}

/// 新建空工程并立即落盘（成为当前工程）
#[tauri::command]
pub async fn new_project(state: State<'_, AppState>, name: String) -> Result<ProjectHandle, String> {
    do_new(state.inner(), &name)
}

/// 切换到工程目录内的指定工程（file_name 来自 list_projects）
#[tauri::command]
pub async fn open_project(state: State<'_, AppState>, file_name: String) -> Result<ProjectHandle, String> {
    do_open(state.inner(), &file_name)
}

/// 保存工程；返回句柄（演示工程首次保存会生成新文件，前端要更新路径）
#[tauri::command]
pub async fn save_project(
    state: State<'_, AppState>,
    path: String,
    project: ProjectSpec,
) -> Result<ProjectHandle, String> {
    do_save(state.inner(), &path, project)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 多工程生命周期：空库启动→demo、新建→落盘、修改保存→写回原文件、
    /// 列表/切换、重启恢复（指针文件）、重名自动加序号。
    /// （单测试函数串行执行：NETLINK_PROJECTS_DIR 是进程级环境变量，并行会互相污染）
    #[test]
    fn project_lifecycle() {
        let tmp = std::env::temp_dir().join(format!("netlink-proj-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("NETLINK_PROJECTS_DIR", &tmp);
        // 隔离本机真实 APPDATA，避免旧版迁移逻辑把 %APPDATA%\NetLink\project.json 带进测试
        let empty_appdata = std::env::temp_dir().join(format!("netlink-proj-appdata-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty_appdata);
        std::fs::create_dir_all(&empty_appdata).unwrap();
        std::env::set_var("APPDATA", &empty_appdata);
        let state = AppState::new();

        // 空工程库启动：演示工程，未落盘
        let h = do_load(&state, None).unwrap();
        assert!(h.path.is_none());
        assert!(!h.spec.connections.is_empty());

        // 演示工程空路径保存：按工程名落盘并成为当前工程
        let demo_path = do_save(&state, "", h.spec).unwrap().path.unwrap();
        assert!(Path::new(&demo_path).exists());
        assert_eq!(Path::new(&demo_path).parent().unwrap(), tmp);

        // 文件名清洗：非法字符换 _、去首尾点空格、保留名加后缀
        assert_eq!(sanitize_stem("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize_stem("  ..名字.. "), "名字");
        assert_eq!(sanitize_stem("   "), "未命名工程");

        // 新建：空工程落盘，文件名取自工程名
        let h1 = do_new(&state, " 测试 工程 ").unwrap();
        assert_eq!(h1.spec.name, "测试 工程");
        assert!(h1.spec.connections.is_empty());
        let p1 = PathBuf::from(h1.path.clone().unwrap());
        assert!(p1.starts_with(&tmp));
        assert!(p1.ends_with("测试 工程.json"));

        // 修改后空路径保存：写回同一文件
        let mut spec = h1.spec.clone();
        spec.name = "改名工程".into();
        let h2 = do_save(&state, "", spec).unwrap();
        assert_eq!(h2.path, h1.path);
        assert_eq!(ProjectSpec::load(&p1).unwrap().name, "改名工程");

        // 列表（此时有 demo + 测试两个工程）与切换
        let list = do_list().unwrap();
        assert_eq!(list.len(), 2);
        let fname = list.iter().map(|m| m.file_name.clone()).find(|f| f == "测试 工程.json").unwrap();
        let h3 = do_open(&state, &fname).unwrap();
        assert_eq!(h3.spec.name, "改名工程");
        assert_eq!(h3.path, h1.path);

        // 「重启」：新 state，指针文件恢复最后打开的工程
        let state2 = AppState::new();
        let h4 = do_load(&state2, None).unwrap();
        assert_eq!(h4.path, h1.path);
        assert_eq!(h4.spec.name, "改名工程");

        // 重名新建自动加序号；Windows 保留名加 _ 后缀
        let h5 = do_new(&state, "测试 工程").unwrap();
        assert_ne!(h5.path, h1.path);
        assert!(PathBuf::from(h5.path.unwrap()).ends_with("测试 工程-2.json"));
        let h7 = do_new(&state, "CON").unwrap();
        assert!(PathBuf::from(h7.path.unwrap()).ends_with("CON_.json"));

        // 路径穿越拒绝
        assert!(do_open(&state, "..\\..\\secret.json").is_err());
        assert!(do_open(&state, "").is_err());

        // 显式路径加载
        let h6 = do_load(&state, Some(demo_path.as_str())).unwrap();
        assert_eq!(h6.path, Some(demo_path.clone()));
        assert!(!h6.spec.connections.is_empty());

        // 工程目录外的工程（「打开工程文件」）：加载后保存写回原文件，且不进工程目录列表
        let external = std::env::temp_dir().join(format!("netlink-ext-{}.json", std::process::id()));
        let ext_spec = ProjectSpec { schema_version: SCHEMA_VERSION, name: "外部工程".into(), connections: Vec::new() };
        ext_spec.save(&external).unwrap();
        let h_ext = do_load(&state, Some(external.to_string_lossy().to_string().as_str())).unwrap();
        assert_eq!(h_ext.path, Some(external.to_string_lossy().to_string()));
        let before = do_list().unwrap().len();
        let mut changed = h_ext.spec.clone();
        changed.name = "外部工程-改".into();
        let h_saved = do_save(&state, "", changed).unwrap();
        assert_eq!(h_saved.path, h_ext.path);
        assert_eq!(ProjectSpec::load(&external).unwrap().name, "外部工程-改");
        assert_eq!(do_list().unwrap().len(), before); // 外部工程不进工程目录列表
        let _ = std::fs::remove_file(&external);

        // 旧版数据迁移：%APPDATA%\NetLink\project.json → 工程目录，且只迁一次
        let legacy_dir = empty_appdata.join("NetLink");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        ProjectSpec::demo().save(&legacy_dir.join("project.json")).unwrap();
        let tmp2 = std::env::temp_dir().join(format!("netlink-proj-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp2);
        std::env::set_var("NETLINK_PROJECTS_DIR", &tmp2);
        let state3 = AppState::new();
        let h8 = do_load(&state3, None).unwrap();
        assert!(Path::new(&h8.path.unwrap()).starts_with(&tmp2));
        assert_eq!(do_list().unwrap().len(), 1);
        // 第二次启动不再迁移（工程目录已有工程）
        let _ = do_load(&AppState::new(), None).unwrap();
        assert_eq!(do_list().unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
        let _ = std::fs::remove_dir_all(&empty_appdata);
    }
}

/// 以当前工程启动全部连接（先停旧实例）
#[tauri::command]
pub async fn start_runtime(
    app: AppHandle,
    state: State<'_, AppState>,
    project: ProjectSpec,
) -> Result<(), String> {
    do_start_runtime(app, state.inner(), project).await
}

/// 启动逻辑实现（NETLINK_AUTOSTART 测试钩子复用）
pub async fn do_start_runtime(
    app: AppHandle,
    state: &AppState,
    project: ProjectSpec,
) -> Result<(), String> {
    stop_runtime_inner(&app, state).await;

    let manager = ensure_runtime(&app, state).await;
    manager.start(&project).await?;
    emit_run_state(&app, &manager);
    Ok(())
}

/// 启动单条连接（前端设备树的 ▶ 按钮）；已在运行则以传入的连接配置重启。
/// enabled_devices = Some(ids) 时启动后仅这些设备在线（单台设备首启不带起同连接其他设备）。
/// 端口被占用 / 串口打不开等启动错误原样返回给前端。
#[tauri::command]
pub async fn start_conn(
    app: AppHandle,
    state: State<'_, AppState>,
    project: ProjectSpec,
    conn_id: u64,
    enabled_devices: Option<Vec<u64>>,
) -> Result<(), String> {
    let conn = project
        .connections
        .iter()
        .find(|c| c.id == conn_id)
        .ok_or_else(|| format!("连接 {conn_id} 不在当前工程里"))?
        .clone();

    let manager = ensure_runtime(&app, state.inner()).await;
    if let Err(e) = manager.start_connection_with(&conn, enabled_devices).await {
        emit_run_state(&app, &manager);
        return Err(format!("连接「{}」启动失败: {}", conn.name, e));
    }
    emit_run_state(&app, &manager);
    Ok(())
}

/// 停止单条连接（前端设备树的 ■ 按钮）
#[tauri::command]
pub async fn stop_conn(
    app: AppHandle,
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<(), String> {
    let manager = state.manager.lock().await.clone();
    if let Some(m) = manager {
        m.stop_connection(conn_id);
        emit_run_state(&app, &m);
    }
    Ok(())
}

/// 在线启停一台设备（设备树的 ▶/■；不动连接，改运行态不改工程配置）
#[tauri::command]
pub async fn set_device_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: u64,
    enabled: bool,
) -> Result<(), String> {
    let manager = state.manager.lock().await.clone();
    let m = manager.ok_or_else(|| "该设备所属连接未启动".to_string())?;
    m.set_device_enabled(device_id, enabled)?;
    emit_run_state(&app, &m);
    Ok(())
}

/// 连接编辑器「测试连接 / 检查端口 / 检查串口」：
/// 主机连接真实连一次远端；从机连接只做端口占用检查（bind / 串口 open）
#[tauri::command]
pub async fn test_connection(transport: Transport) -> TestConnResult {
    test_transport(&transport).await
}

/// 确保运行时管理器与推送任务存在（单连接启动/全局启动共用），返回管理器克隆。
/// 管理器跨单连接启停存活，全部停止也不销毁，直到 stop_runtime。
async fn ensure_runtime(app: &AppHandle, state: &AppState) -> RuntimeManager {
    let mut guard = state.manager.lock().await;
    if let Some(m) = guard.clone() {
        return m;
    }
    let manager = RuntimeManager::new();

    // 帧转发：broadcast → 前端事件
    let mut frame_rx = manager.subscribe_frames();
    let app_frames = app.clone();
    let frame_task = tokio::spawn(async move {
        while let Ok(ev) = frame_rx.recv().await {
            let _ = app_frames.emit(UPDATE_EVENT, json!({ "frames": [ev] }));
        }
    });

    // 值推送：每 tick 快照选中设备（selected 为 Arc 共享，切换即时生效）
    let push_manager = manager.clone();
    let selected = state.selected_device.clone();
    let app_values = app.clone();
    let push_task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let dev = selected.load(Ordering::Acquire);
            if dev == 0 {
                continue;
            }
            let values = push_manager.snapshot_values(dev);
            if values.is_empty() {
                continue;
            }
            let mut map = Map::new();
            for (id, v) in values {
                map.insert(id.to_string(), Value::from(v));
            }
            let mut patch = json!({ "values": map, "running": push_manager.is_running() });
            // 采集健康统计（主机设备）
            let stats = push_manager.snapshot_stats();
            if !stats.is_empty() {
                let mut sm = Map::new();
                for (id, last_ok, timeouts, fails) in stats {
                    sm.insert(
                        id.to_string(),
                        json!({ "last_ok_ms": last_ok, "timeout_count": timeouts, "fail_count": fails }),
                    );
                }
                patch["stats"] = Value::Object(sm);
            }
            let _ = app_values.emit(UPDATE_EVENT, patch);
        }
    });

    {
        let mut tasks = state.push_tasks.lock().await;
        tasks.push(frame_task);
        tasks.push(push_task);
    }
    *guard = Some(manager.clone());
    manager
}

/// 向前端推送运行状态：全局 running + 各连接/设备运行与否
fn emit_run_state(app: &AppHandle, m: &RuntimeManager) {
    let ids = m.running_conn_ids();
    let devices = m.running_device_ids();
    let _ = app.emit(
        UPDATE_EVENT,
        json!({
            "running": !ids.is_empty(),
            "running_conns": ids,
            "running_devices": devices,
        }),
    );
}

#[tauri::command]
pub async fn stop_runtime(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    stop_runtime_inner(&app, state.inner()).await;
    Ok(())
}

async fn stop_runtime_inner(app: &AppHandle, state: &AppState) {
    {
        let mut guard = state.manager.lock().await;
        if let Some(m) = guard.take() {
            m.stop_all();
        }
    }
    {
        let mut tasks = state.push_tasks.lock().await;
        for t in tasks.drain(..) {
            t.abort();
        }
    }
    let _ = app.emit(
        UPDATE_EVENT,
        json!({ "running": false, "running_conns": [], "running_devices": [] }),
    );
}

#[tauri::command]
pub async fn set_sim_paused(
    app: AppHandle,
    state: State<'_, AppState>,
    paused: bool,
) -> Result<(), String> {
    {
        let guard = state.manager.lock().await;
        if let Some(m) = &*guard {
            m.clock().set_paused(paused);
        }
    }
    let _ = app.emit(UPDATE_EVENT, json!({ "paused": paused }));
    Ok(())
}

#[tauri::command]
pub fn select_device(state: State<'_, AppState>, device_id: u64) {
    state.selected_device.store(device_id, Ordering::Release);
}

/// 读取点表文件（.xlsx / .csv 按扩展名）→ 解析后的行 + 逐行错误报告（坏行不中断）
#[tauri::command]
pub async fn read_point_sheet(path: String) -> Result<netlink_core::sheet::SheetParseResult, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("读取文件失败（{path}）: {e}"))?;
    let lower = path.to_lowercase();
    if lower.ends_with(".xlsx") {
        netlink_core::sheet::parse_xlsx(&bytes)
    } else if lower.ends_with(".csv") {
        Ok(netlink_core::sheet::parse_csv(&bytes))
    } else {
        Err("仅支持 .xlsx / .csv 点表文件".to_string())
    }
}

/// 写点表文件（按扩展名选格式，xlsx 单表「点表」/ CSV 带 BOM；目录不存在则创建）
#[tauri::command]
pub async fn write_point_sheet(
    path: String,
    rows: Vec<netlink_core::sheet::PointRow>,
) -> Result<(), String> {
    let bytes = {
        let lower = path.to_lowercase();
        if lower.ends_with(".xlsx") {
            netlink_core::sheet::write_xlsx(&rows)?
        } else if lower.ends_with(".csv") {
            netlink_core::sheet::write_csv(&rows)
        } else {
            return Err("仅支持导出为 .xlsx / .csv".to_string());
        }
    };
    if let Some(dir) = Path::new(&path).parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::write(&path, bytes).map_err(|e| format!("写入文件失败（{path}）: {e}"))
}

/// 枚举本机串口（连接编辑用）。async：Windows SetupAPI 枚举在个别机器上很慢，
/// 同步跑会占住 WebView 主线程饿死其他命令
#[tauri::command]
pub async fn list_serial_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

/// 主机侧写点位（在线值直写）
#[tauri::command]
pub async fn write_point(
    state: State<'_, AppState>,
    device_id: u64,
    point_id: u64,
    value: f64,
) -> Result<(), String> {
    let guard = state.manager.lock().await;
    match &*guard {
        Some(m) => m.write_point(device_id, point_id, value).await,
        None => Err("模拟未启动".to_string()),
    }
}

/// 主机侧发送测试帧：hex 为 PDU（不含 MBAP），返回应答 PDU hex
#[tauri::command]
pub async fn send_test_frame(
    state: State<'_, AppState>,
    conn_id: u64,
    unit: u8,
    hex: String,
) -> Result<String, String> {
    let pdu = netlink_core::protocol::parse_hex(&hex).ok_or("十六进制格式非法")?;
    let guard = state.manager.lock().await;
    let resp = match &*guard {
        Some(m) => m.send_test_frame(conn_id, unit, pdu).await?,
        None => return Err("模拟未启动".to_string()),
    };
    Ok(netlink_core::protocol::hex_str(&resp))
}
