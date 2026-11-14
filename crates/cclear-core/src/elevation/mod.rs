//! 提权 helper：同一个 exe 以管理员权限执行内置操作。
//!
//! 主程序不整体提权（manifest 保持 asInvoker）。需要管理员的清理和隔离批次操作，
//! 由这里用 `ShellExecuteExW` 以 runas 启动当前 exe 的 helper 模式：helper 只接受内置操作 ID，
//! 所有系统路径由它自己用 Known Folder 解析，命令行里没有用户自由路径。

mod args;
mod helper;
mod launch;

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::win::{KnownFolder, known_folder};

pub use args::{HelperArgs, SystemOp, parse_helper_args};
pub use helper::helper_main;
pub use launch::command_line;

/// 提权流程的错误。文案都是给用户看的中文。
#[derive(Debug, Error)]
pub enum ElevationError {
    #[error("helper 的命令行参数不合法：{0}")]
    InvalidArgs(String),

    /// 用户在 UAC 提示里点了取消。
    #[error("已取消授权")]
    Cancelled,

    #[error("无法启动提权 helper：{0}")]
    Launch(String),

    #[error("隔离根目录不可用：{0}")]
    QuarantineRoot(String),

    #[error("无法解析系统目录「{0}」")]
    KnownFolder(&'static str),

    #[error("提权 helper 的日志无法写入：{0}")]
    Journal(String),

    #[error("提权 helper 没有写出结果（退出码 {exit_code}）")]
    MissingResult { exit_code: u32 },

    #[error("提权 helper 的结果读不出来：{0}")]
    Result(String),

    #[error("{0}")]
    Core(#[from] crate::error::CoreError),

    #[error("文件操作失败：{0}")]
    Io(#[from] io::Error),
}

/// helper 返回给主程序的结果，写在 `--result` 指定的文件里。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperResult {
    /// 每个操作的执行结果，顺序与命令行里的 ops 一致。
    pub ops: Vec<OpOutcome>,
}

impl HelperResult {
    /// 取第一个操作的结果。单独执行一个操作时直接用它。
    pub fn first(&self) -> Option<&OpOutcome> {
        self.ops.first()
    }
}

/// 一个操作的执行结果。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpOutcome {
    /// 操作的字符串形式，例如 `leappstore-cicada` 或 `restore-batch:<uuid>`。
    pub op: String,
    /// 机器可读状态：`done` 表示完成，`skipped` 表示按规则跳过，`failed` 表示出错。
    pub status: String,
    /// 中文说明，界面直接显示。
    pub detail: String,
    /// 隔离批次的 ID。只有隔离和批次类操作有。
    #[serde(default)]
    pub batch_id: Option<String>,
    /// 本次处理的条目数（隔离或还原的文件数）。
    #[serde(default)]
    pub items: u64,
}

impl OpOutcome {
    pub fn done(op: String, detail: String) -> Self {
        Self::new(op, "done", detail)
    }

    pub fn skipped(op: String, detail: String) -> Self {
        Self::new(op, "skipped", detail)
    }

    pub fn failed(op: String, detail: String) -> Self {
        Self::new(op, "failed", detail)
    }

    fn new(op: String, status: &str, detail: String) -> Self {
        Self {
            op,
            status: status.to_string(),
            detail,
            batch_id: None,
            items: 0,
        }
    }

    /// 完成了吗。界面据此区分“做完了”和“跳过了”。
    pub fn is_done(&self) -> bool {
        self.status == "done"
    }
}

/// 以管理员权限执行一组内置操作。
///
/// UAC 被取消时返回 [`ElevationError::Cancelled`]（显示“已取消授权”），
/// 启动或读取结果失败时返回对应的错误，不会退回普通权限去执行。
pub fn run_elevated(
    ops: &[SystemOp],
    quarantine_root: &Path,
) -> Result<HelperResult, ElevationError> {
    if ops.is_empty() {
        return Err(ElevationError::InvalidArgs("没有要执行的操作".to_string()));
    }

    let program = std::env::current_exe()
        .map_err(|error| ElevationError::Launch(format!("无法定位当前程序：{error}")))?;

    // 结果文件放在固定目录下，helper 只接受这个格式的路径。
    let dir = elevated_dir()?;
    std::fs::create_dir_all(&dir)?;
    let result_file = dir.join(format!("{}.json", Uuid::new_v4()));

    let params = vec![
        "--elevated".to_string(),
        "--ops".to_string(),
        ops.iter()
            .map(|op| op.token())
            .collect::<Vec<String>>()
            .join(","),
        "--result".to_string(),
        result_file.to_string_lossy().into_owned(),
        "--quarantine-root".to_string(),
        quarantine_root.to_string_lossy().into_owned(),
    ];

    launch::runas_and_wait(&program, &params, &result_file)
}

/// 结果文件和进度文件的固定目录：`<LocalAppData>\CClear\elevated`。
pub fn elevated_dir() -> Result<PathBuf, ElevationError> {
    known_folder(KnownFolder::LocalAppData)
        .map(|path| path.join("CClear").join("elevated"))
        .map_err(|_| ElevationError::KnownFolder("本地应用数据目录"))
}

/// 当前程序是否以管理员权限运行。界面用它提示“需要重启”之类的话。
pub fn is_elevated() -> bool {
    crate::win::is_elevated()
}
