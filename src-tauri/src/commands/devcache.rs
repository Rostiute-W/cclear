//! 开发缓存命令：调用工具自己的官方清理命令。
//!
//! 前端只传规则 ID 和是否已二次确认。命令、参数、缓存路径都由核心库决定，
//! 前端拿不到任何可执行的命令行。

use cclear_core::journal::FileJournal;
use cclear_core::rules::catalog::{Target, find};
use cclear_core::rules::commands::{self, CommandReport};
use serde::Serialize;
use tauri::State;
use tauri::ipc::Channel;

use crate::state::AppState;

/// 执行进度。命令没有分项进度，只有“已开始”和“已结束”。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandProgressDto {
    /// running 表示命令已经启动，done 表示已经结束。
    pub stage: &'static str,
}

/// 一次命令执行的结果。大小是命令前后的缓存占盘大小。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReportDto {
    pub rule_id: String,
    /// 退出码。命令没跑起来时为 null。
    pub exit_code: Option<i32>,
    /// 等待超时。超时后命令仍在运行，界面要如实提示。
    pub timed_out: bool,
    pub logical_before: u64,
    pub allocated_before: u64,
    pub logical_after: u64,
    pub allocated_after: u64,
    /// 命令输出末尾，失败时给用户看原因。
    pub output: String,
    /// 命令已执行但日志写入失败时的原因。
    pub log_error: Option<String>,
}

#[tauri::command]
pub async fn command_execute(
    rule_id: String,
    second_confirm: bool,
    on_progress: Channel<CommandProgressDto>,
    state: State<'_, AppState>,
) -> Result<CommandReportDto, String> {
    // 清单执行、清空回收站和命令执行共用一把锁，同一时间只允许一次清理类操作。
    let _exec = state.start_exec()?;

    // 命令要等工具跑完，放到阻塞线程池里执行，避免占住异步运行时。
    tauri::async_runtime::spawn_blocking(move || {
        let rule = find(&rule_id).ok_or_else(|| "没有这条清理规则".to_string())?;
        let Target::Command(id) = &rule.target else {
            return Err("这条规则不是命令类规则".to_string());
        };

        // 日志先打开：命令跑完才写日志的话，中途出错就什么都不剩。
        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        let _ = on_progress.send(CommandProgressDto { stage: "running" });
        let report = commands::execute(rule.id, *id, second_confirm, &mut journal)?;
        let _ = on_progress.send(CommandProgressDto { stage: "done" });

        Ok(report_dto(rule.id, report))
    })
    .await
    .map_err(|error| format!("执行命令失败：{error}"))?
}

fn report_dto(rule_id: &str, report: CommandReport) -> CommandReportDto {
    CommandReportDto {
        rule_id: rule_id.to_string(),
        exit_code: report.exit_code,
        timed_out: report.timed_out,
        logical_before: report.size_before.logical,
        allocated_before: report.size_before.allocated,
        logical_after: report.size_after.logical,
        allocated_after: report.size_after.allocated,
        output: report.output,
        log_error: report.log_error,
    }
}
