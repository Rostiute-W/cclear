// 发行版不弹出额外的控制台窗口。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 带 --elevated 时以提权 helper 模式运行：不开窗口，执行完直接按结果退出。
    if std::env::args().skip(1).any(|arg| arg == "--elevated") {
        std::process::exit(cclear_core::elevation::helper_main());
    }

    cclear_app_lib::run()
}
