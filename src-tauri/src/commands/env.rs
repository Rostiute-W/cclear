//! 环境状态卡命令。全部只读，读不到的项由核心库标成未知。

use cclear_core::envstate::{EnvState, env_state as read_env_state};

#[tauri::command]
pub fn env_state() -> EnvState {
    read_env_state()
}
