//! 系统维护命令：清理页“系统维护（需要管理员）”分组。
//!
//! 前端只传规则 ID：内置操作、可执行文件路径和系统目录都由核心库与 helper 决定。
//! 选中的多项合成一次 helper 调用，整个过程中只会弹一次 UAC。
//! DISM 清理这类长耗时操作不提供取消，进度靠 helper 写的进度文件每秒读一次。

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cclear_core::elevation::{HelperResult, OpProgress, SystemOp, elevated_dir, run_elevated_with};
use cclear_core::rules::catalog::{Rule, Target, find};
use serde::Serialize;
use tauri::State;
use tauri::ipc::Channel;
use uuid::Uuid;

use crate::commands::elevated::{ElevatedResultDto, result_dto};
use crate::state::AppState;

/// 进度文件的轮询间隔。任务要求每秒读一次。
const PROGRESS_POLL: Duration = Duration::from_secs(1);

/// 一条进度。界面直接显示 `detail`，百分比用来画进度条。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceProgressDto {
    pub op: String,
    pub percent: Option<f32>,
    pub detail: String,
}

/// 只读分析组件存储。结果就是一条 op 结果，`detail` 里是三项结论。
#[tauri::command]
pub async fn maintenance_analyze(state: State<'_, AppState>) -> Result<ElevatedResultDto, String> {
    // 分析也是提权操作，和清理类操作共用同一把执行锁。
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        run_ops(&[SystemOp::DismAnalyze], None).map(result_dto)
    })
    .await
    .map_err(|error| format!("分析组件存储失败：{error}"))?
}

/// 以管理员权限执行选中的系统维护操作，一次只弹一次授权。
#[tauri::command]
pub async fn maintenance_execute(
    rule_ids: Vec<String>,
    on_progress: Channel<MaintenanceProgressDto>,
    state: State<'_, AppState>,
) -> Result<ElevatedResultDto, String> {
    let ops = system_ops(&rule_ids)?;
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let progress_file = progress_path()?;

        // helper 在另一个进程里跑，进度只能靠文件传回来，这里每秒读一次。
        let stop = Arc::new(AtomicBool::new(false));
        let poller = {
            let path = progress_file.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || poll_progress(&path, &stop, on_progress))
        };

        let result = run_ops(&ops, Some(&progress_file));

        stop.store(true, Ordering::SeqCst);
        let _ = poller.join();

        result.map(result_dto)
    })
    .await
    .map_err(|error| format!("系统维护失败：{error}"))?
}

/// 调一次 helper。`progress` 有值时 helper 会把长耗时操作的进度写进去。
fn run_ops(ops: &[SystemOp], progress: Option<&Path>) -> Result<HelperResult, String> {
    if ops.is_empty() {
        return Err("没有选择任何系统维护操作".to_string());
    }

    run_elevated_with(ops, None, progress).map_err(|error| error.to_string())
}

/// 规则 ID 到内置操作。没有内置操作的规则（例如系统 WER）在这里拒绝。
fn system_ops(rule_ids: &[String]) -> Result<Vec<SystemOp>, String> {
    if rule_ids.is_empty() {
        return Err("没有选择任何系统维护操作".to_string());
    }

    let mut ops = Vec::with_capacity(rule_ids.len());
    for id in rule_ids {
        let rule = find(id).ok_or_else(|| format!("没有这条清理规则：{id}"))?;
        let op =
            system_op(rule).ok_or_else(|| format!("「{}」还没有可执行的内置操作", rule.name))?;

        // 同一个操作执行两次没有意义，重复选择按参数错误处理。
        if ops.contains(&op) {
            return Err(format!("「{}」被重复选择", rule.name));
        }
        ops.push(op);
    }

    Ok(ops)
}

fn system_op(rule: &Rule) -> Option<SystemOp> {
    let Target::System(id) = &rule.target else {
        return None;
    };

    SystemOp::of_id(*id)
}

/// 本次执行的进度文件。文件名格式由 helper 的参数校验固定为 `<uuid>.progress`。
fn progress_path() -> Result<PathBuf, String> {
    let dir = elevated_dir().map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|error| format!("进度目录无法创建：{error}"))?;

    Ok(dir.join(format!("{}.progress", Uuid::new_v4())))
}

/// 每秒读一次进度文件，把新出现的行推给界面。主进程结束时由 `stop` 叫停。
fn poll_progress(path: &Path, stop: &AtomicBool, channel: Channel<MaintenanceProgressDto>) {
    // 已经读到的字节数。进度文件只追加，从上次读到的位置继续就行。
    let mut offset = 0u64;

    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(PROGRESS_POLL);

        let Ok(mut file) = File::open(path) else {
            continue;
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }

        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(read) => {
                    offset += read as u64;
                    // 半行或坏行直接跳过：下一轮会从完整的位置继续读。
                    if let Some(progress) = OpProgress::parse(&line) {
                        let _ = channel.send(MaintenanceProgressDto {
                            op: progress.op,
                            percent: progress.percent,
                            detail: progress.detail,
                        });
                    }
                }
                Err(_) => break,
            }
        }
    }
}
