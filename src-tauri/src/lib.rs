//! Tauri 薄 IPC 层：只做窗口、状态与命令的接线，不放业务逻辑。

mod commands;
mod state;

use cclear_core::win::{KnownFolder, known_folder};
use tauri::{WebviewUrl, WebviewWindowBuilder};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
        .invoke_handler(tauri::generate_handler![commands::app::app_info])
        .run(tauri::generate_context!())
        .expect("CClear 启动失败");
}
