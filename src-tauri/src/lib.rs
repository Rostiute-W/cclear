mod commands;
mod state;

use cclear_core::win::{KnownFolder, known_folder};
use tauri::{WebviewUrl, WebviewWindowBuilder};

pub fn run() {
    tauri::Builder::default()
        .manage(state::AppState::default())
        .setup(|app| {
            let data_directory = known_folder(KnownFolder::LocalAppData)?
                .join("CClear")
                .join("webview");
            WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("CClear · C 盘清理")
                .inner_size(1240.0, 800.0)
                .min_inner_size(1040.0, 680.0)
                .data_directory(data_directory)
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::volume_info,
            commands::junk_scan_start,
            commands::junk_scan_cancel,
            commands::clean_check_processes,
            commands::clean_close_processes,
            commands::clean_execute,
            commands::clean_cancel,
            commands::open_clean_item_location,
            commands::deep_slim_status,
            commands::deep_slim_execute,
            commands::default_scan_roots,
            commands::space_scan_start,
            commands::space_scan_cancel,
            commands::space_scan_summary,
            commands::space_scan_children,
            commands::space_scan_top_files,
            commands::space_open_in_explorer,
            commands::space_recycle_item,
            commands::history_list,
            commands::history_detail,
            commands::settings_get,
            commands::settings_save,
            commands::rule_pack_import,
            commands::rule_pack_remove,
        ])
        .run(tauri::generate_context!())
        .expect("CClear 启动失败");
}
