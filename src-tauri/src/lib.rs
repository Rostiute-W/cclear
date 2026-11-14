//! Tauri 薄 IPC 层：只做窗口、状态与命令的接线，不放业务逻辑。

mod commands;
mod state;

use cclear_core::win::{KnownFolder, known_folder};
use tauri::{WebviewUrl, WebviewWindowBuilder};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(state::AppState::default())
        .setup(|app| {
            // WebView2 的用户数据目录与系统盘默认位置分开，便于自我保护。
            let data_directory = known_folder(KnownFolder::LocalAppData)?
                .join("CClear")
                .join("webview");

            WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("CClear — C 盘清理")
                .inner_size(1280.0, 820.0)
                .min_inner_size(1000.0, 680.0)
                .data_directory(data_directory)
                .build()?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::app_info,
            commands::clean::candidate_locks,
            commands::clean::open_candidate_in_explorer,
            commands::devcache::command_execute,
            commands::env::env_state,
            commands::plan::plan_build,
            commands::plan::plan_items,
            commands::plan::plan_execute,
            commands::plan::plan_cancel,
            commands::plan::history_list,
            commands::plan::history_detail,
            commands::recycle::recycle_bin_query,
            commands::recycle::recycle_bin_empty,
            commands::rules::rules_catalog,
            commands::rules::rules_evaluate,
            commands::scan::volume_info,
            commands::scan::default_scan_roots,
            commands::scan::scan_start,
            commands::scan::scan_cancel,
            commands::scan::scan_summary,
            commands::scan::scan_children,
            commands::scan::scan_top_files,
            commands::scan::open_node_in_explorer,
            commands::scan::open_top_file_in_explorer,
        ])
        .run(tauri::generate_context!())
        .expect("CClear 启动失败");
}
