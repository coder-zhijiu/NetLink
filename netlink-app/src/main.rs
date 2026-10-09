// 防止 release 构建在 Windows 上多出一个控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;

use state::AppState;
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .setup(|app| {
            // 开发/测试钩子：NETLINK_AUTOSTART=1 时启动即运行演示工程（无头验证用）
            if std::env::var("NETLINK_AUTOSTART").ok().as_deref() == Some("1") {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let project = netlink_core::model::ProjectSpec::demo();
                    let state = handle.state::<AppState>();
                    if let Err(e) = commands::do_start_runtime(handle.clone(), state.inner(), project).await {
                        eprintln!("[netlink] AUTOSTART 失败: {e}");
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::load_project,
            commands::save_project,
            commands::list_projects,
            commands::new_project,
            commands::open_project,
            commands::start_runtime,
            commands::stop_runtime,
            commands::start_conn,
            commands::stop_conn,
            commands::set_device_enabled,
            commands::test_connection,
            commands::set_sim_paused,
            commands::select_device,
            commands::list_serial_ports,
            commands::read_point_sheet,
            commands::write_point_sheet,
            commands::write_point,
            commands::send_test_frame,
        ])
        .run(tauri::generate_context!())
        .expect("NetLink 启动失败");
}
