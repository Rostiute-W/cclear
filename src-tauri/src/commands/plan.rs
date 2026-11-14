//! 冻结清单命令：生成清单、列出条目、执行、取消与历史。
//!
//! 执行类命令只接受计划 ID 和规则 ID，条目路径一律从计划里取，前端拿不到自由路径。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use cclear_core::exec::executor::{ExecProgress, ExecReport, execute};
use cclear_core::guard::Forbidden;
use cclear_core::journal::{FileJournal, read_detail, read_summaries};
use cclear_core::model::Recovery;
use cclear_core::plan::{FrozenItem, FrozenPlan, PlanRule, build_plan};
use cclear_core::rules::catalog::find;
use cclear_core::rules::resolve::{RootMap, evaluate};
use cclear_core::win::process::running_processes;
use serde::Serialize;
use tauri::State;
use tauri::ipc::Channel;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRuleDto {
    pub rule_id: String,
    pub recovery: Recovery,
    pub anchors: Vec<String>,
    pub items: u64,
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
    pub dirs: u64,
    /// 构建清单时被跳过的条目数（链接、禁区、正在运行的程序文件等）。
    pub skipped_at_build: u32,
    /// 被跳过条目的明细，让用户能展开看原因。
    pub skipped_details: Vec<PlanSkippedDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanSkippedDto {
    pub path: String,
    pub reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanSummaryDto {
    pub plan_id: String,
    /// Unix 秒，界面按本地时区格式化。
    pub created_at: i64,
    pub rules: Vec<PlanRuleDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItemDto {
    pub item_id: u32,
    pub rule_id: String,
    pub path: String,
    pub is_dir: bool,
    pub allocated: u64,
    /// Unix 秒，0 表示时间不可用。
    pub last_write: i64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecProgressDto {
    pub rule_id: String,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub freed_allocated: u64,
    pub skipped: u64,
    pub done: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecRuleDto {
    pub rule_id: String,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub freed_allocated: u64,
    pub skipped: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecSkipDto {
    pub rule_id: String,
    pub path: String,
    pub reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecReportDto {
    pub rules: Vec<ExecRuleDto>,
    pub skipped: Vec<ExecSkipDto>,
    pub skipped_total: u64,
    pub free_before: u64,
    pub free_after: u64,
    pub cancelled: bool,
    pub aborted: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanHistoryEntryDto {
    pub plan_id: String,
    /// Unix 秒，0 表示日志里没有开始记录。
    pub started_at: i64,
    /// 这次执行涉及的规则 ID，名称由界面查规则目录。
    pub rules: Vec<String>,
    pub free_before: u64,
    pub free_after: u64,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub freed_allocated: u64,
    pub skipped: u64,
    pub finished: bool,
    /// 用户取消了这次执行。
    pub cancelled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanHistoryDto {
    pub plans: Vec<PlanHistoryEntryDto>,
    /// 解析失败的日志行数。大于 0 说明历史不完整。
    pub skipped_lines: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDetailDto {
    pub plan_id: String,
    pub started_at: i64,
    pub free_before: u64,
    pub free_after: u64,
    pub finished: bool,
    /// 用户取消了这次执行。
    pub cancelled: bool,
    pub rules: Vec<PlanDetailRuleDto>,
    /// 跳过明细，复用执行报告里的跳过结构。
    pub skipped: Vec<ExecSkipDto>,
    pub skipped_total: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDetailRuleDto {
    pub rule_id: String,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub freed_allocated: u64,
    pub skipped: u64,
}

#[tauri::command]
pub async fn plan_build(
    rule_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<PlanSummaryDto, String> {
    let plan = tauri::async_runtime::spawn_blocking(move || build(&rule_ids))
        .await
        .map_err(|error| format!("生成清理清单失败：{error}"))??;

    let summary = summary_dto(&plan);
    state.store_plan(plan);

    Ok(summary)
}

#[tauri::command]
pub fn plan_items(
    plan_id: String,
    rule_id: String,
    offset: u32,
    limit: u32,
    state: State<'_, AppState>,
) -> Result<Vec<PlanItemDto>, String> {
    let plan = stored_plan(&plan_id, &state)?;
    let rule = plan
        .rules
        .iter()
        .find(|rule| rule.rule_id == rule_id)
        .ok_or_else(|| "这份清理计划里没有这条规则".to_string())?;

    let items = &plan.items[rule.items.start as usize..rule.items.end as usize];
    let start = (offset as usize).min(items.len());
    let end = (start + limit as usize).min(items.len());

    Ok(items[start..end]
        .iter()
        .map(|item| item_dto(rule, item))
        .collect())
}

#[tauri::command]
pub async fn plan_execute(
    plan_id: String,
    rule_ids: Vec<String>,
    on_progress: Channel<ExecProgressDto>,
    state: State<'_, AppState>,
) -> Result<ExecReportDto, String> {
    let plan = stored_plan(&plan_id, &state)?;
    // 命令执行、清空回收站和清单执行共用这把锁，同一时间只允许一次清理类操作。
    let exec = state.start_exec()?;
    let cancel = exec.cancel();

    let result = tauri::async_runtime::spawn_blocking(move || {
        run_execution(plan, &rule_ids, on_progress, cancel)
    })
    .await;

    match result {
        Ok(report) => Ok(report_dto(report)),
        Err(error) => Err(format!("清理失败：{error}")),
    }
}

#[tauri::command]
pub fn plan_cancel(state: State<'_, AppState>) {
    state.cancel_exec();
}

#[tauri::command]
pub async fn history_list(days: u32) -> Result<PlanHistoryDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (plans, skipped_lines) = read_summaries(days.max(1) as usize)
            .map_err(|error| format!("读取操作日志失败：{error}"))?;

        Ok(PlanHistoryDto {
            plans: plans.into_iter().map(history_dto).collect(),
            skipped_lines,
        })
    })
    .await
    .map_err(|error| format!("读取操作日志失败：{error}"))?
}

/// 某一次执行的历史详情。只接受计划 ID，路径全部来自日志。
#[tauri::command]
pub async fn history_detail(plan_id: String, days: u32) -> Result<PlanDetailDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let id = Uuid::parse_str(&plan_id).map_err(|_| "这条历史记录不存在".to_string())?;

        read_detail(days.max(1) as usize, id)
            .map_err(|error| format!("读取操作日志失败：{error}"))?
            .map(detail_dto)
            .ok_or_else(|| "这条历史记录已经找不到了，可能日志已被清理".to_string())
    })
    .await
    .map_err(|error| format!("读取操作日志失败：{error}"))?
}

/// 生成冻结清单。只有当前确实可执行的规则才能进来。
fn build(rule_ids: &[String]) -> Result<FrozenPlan, String> {
    if rule_ids.is_empty() {
        return Err("没有选择任何清理项".to_string());
    }

    let roots = RootMap::system().map_err(|error| error.to_string())?;
    let procs = running_processes();
    let now = SystemTime::now();

    let mut rules = Vec::with_capacity(rule_ids.len());
    for id in rule_ids {
        let rule = find(id).ok_or_else(|| format!("没有这条清理规则：{id}"))?;
        // 界面上的可执行状态是评估时算的，这里按当前状态再确认一次。
        if !evaluate(rule, &roots, &procs, now).executable {
            return Err(format!(
                "「{}」现在没有执行资格，请重新评估后再试",
                rule.name
            ));
        }
        rules.push(rule);
    }

    let forbidden = Forbidden::from_rules(&roots);
    build_plan(&rules, &roots, &forbidden, &procs, now).map_err(|error| error.to_string())
}

/// 按 ID 取已存下的计划。前端只能拿到计划 ID，拿不到清单内容。
fn stored_plan(plan_id: &str, state: &AppState) -> Result<Arc<FrozenPlan>, String> {
    let id = Uuid::parse_str(plan_id).map_err(|_| "清理计划不存在，请重新生成".to_string())?;

    state
        .plan(id)
        .ok_or_else(|| "清理计划不存在或已被新的预览替换，请重新生成".to_string())
}

/// 真正执行的地方。日志和禁区集合都在这里按当前状态重建。
fn run_execution(
    plan: Arc<FrozenPlan>,
    rule_ids: &[String],
    channel: Channel<ExecProgressDto>,
    cancel: Arc<AtomicBool>,
) -> ExecReport {
    let mut journal = match FileJournal::new() {
        Ok(journal) => journal,
        Err(error) => return failed_report(format!("日志无法写入：{error}")),
    };

    let forbidden = match RootMap::system() {
        Ok(roots) => Forbidden::from_rules(&roots),
        Err(error) => return failed_report(error.to_string()),
    };

    let selected: Vec<&str> = rule_ids.iter().map(String::as_str).collect();

    execute(
        plan.as_ref(),
        &selected,
        &forbidden,
        &|| running_processes(),
        &mut journal,
        cancel.as_ref(),
        &|progress| {
            let _ = channel.send(progress_dto(progress));
        },
    )
}

/// 执行还没开始就失败时的报告。
fn failed_report(message: String) -> ExecReport {
    ExecReport {
        rules: Vec::new(),
        skipped: Vec::new(),
        skipped_total: 0,
        free_before: 0,
        free_after: 0,
        cancelled: false,
        aborted: Some(message),
    }
}

fn summary_dto(plan: &FrozenPlan) -> PlanSummaryDto {
    PlanSummaryDto {
        plan_id: plan.plan_id.to_string(),
        created_at: plan.created_at.unix_timestamp(),
        rules: plan
            .rules
            .iter()
            .map(|rule| PlanRuleDto {
                rule_id: rule.rule_id.to_string(),
                recovery: rule.recovery,
                anchors: rule
                    .anchors
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                items: (rule.items.end - rule.items.start) as u64,
                logical: rule.total.logical,
                allocated: rule.total.allocated,
                files: rule.total.files,
                dirs: rule.total.dirs,
                skipped_at_build: rule.skipped_at_build.len() as u32,
                skipped_details: rule
                    .skipped_at_build
                    .iter()
                    .map(|(path, reason)| PlanSkippedDto {
                        path: path.to_string_lossy().into_owned(),
                        reason: reason.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn item_dto(rule: &PlanRule, item: &FrozenItem) -> PlanItemDto {
    PlanItemDto {
        item_id: item.item_id,
        rule_id: rule.rule_id.to_string(),
        path: item_path(rule, item),
        is_dir: item.is_dir,
        allocated: item.allocated,
        last_write: filetime_to_unix(item.mtime),
    }
}

/// 条目的完整路径，只用于展示。
fn item_path(rule: &PlanRule, item: &FrozenItem) -> String {
    let anchor = item.anchor_index as usize;
    let mut path = match rule.anchors.get(anchor) {
        Some(anchor) => anchor.clone(),
        None => return String::new(),
    };
    for part in &item.rel {
        path.push(part);
    }

    path.to_string_lossy().into_owned()
}

fn progress_dto(progress: ExecProgress) -> ExecProgressDto {
    ExecProgressDto {
        rule_id: progress.rule_id,
        deleted_files: progress.deleted_files,
        deleted_dirs: progress.deleted_dirs,
        freed_allocated: progress.freed_allocated,
        skipped: progress.skipped,
        done: progress.done,
    }
}

fn report_dto(report: ExecReport) -> ExecReportDto {
    ExecReportDto {
        rules: report
            .rules
            .into_iter()
            .map(|rule| ExecRuleDto {
                rule_id: rule.rule_id.to_string(),
                deleted_files: rule.deleted_files,
                deleted_dirs: rule.deleted_dirs,
                freed_allocated: rule.freed_allocated,
                skipped: rule.skipped,
            })
            .collect(),
        skipped: report
            .skipped
            .into_iter()
            .map(|item| ExecSkipDto {
                rule_id: item.rule_id.to_string(),
                path: item.path,
                reason: item.reason,
            })
            .collect(),
        skipped_total: report.skipped_total,
        free_before: report.free_before,
        free_after: report.free_after,
        cancelled: report.cancelled,
        aborted: report.aborted,
    }
}

fn history_dto(summary: cclear_core::journal::PlanSummary) -> PlanHistoryEntryDto {
    PlanHistoryEntryDto {
        plan_id: summary.plan_id.to_string(),
        started_at: summary.started_at.unix_timestamp(),
        rules: summary.rules,
        free_before: summary.free_before,
        free_after: summary.free_after,
        deleted_files: summary.deleted_files,
        deleted_dirs: summary.deleted_dirs,
        freed_allocated: summary.freed_allocated,
        skipped: summary.skipped,
        finished: summary.finished,
        cancelled: summary.cancelled,
    }
}

fn detail_dto(detail: cclear_core::journal::PlanDetail) -> PlanDetailDto {
    PlanDetailDto {
        plan_id: detail.plan_id.to_string(),
        started_at: detail.started_at.unix_timestamp(),
        free_before: detail.free_before,
        free_after: detail.free_after,
        finished: detail.finished,
        cancelled: detail.cancelled,
        rules: detail
            .rules
            .into_iter()
            .map(|rule| PlanDetailRuleDto {
                rule_id: rule.rule_id,
                deleted_files: rule.deleted_files,
                deleted_dirs: rule.deleted_dirs,
                freed_allocated: rule.freed_allocated,
                skipped: rule.skipped,
            })
            .collect(),
        skipped: detail
            .skipped
            .into_iter()
            .map(|item| ExecSkipDto {
                rule_id: item.rule_id,
                path: item.path,
                reason: item.reason,
            })
            .collect(),
        skipped_total: detail.skipped_total,
    }
}

/// FILETIME（1601 年起的 100 纳秒）换算成 Unix 秒，界面统一按本地时区格式化。
fn filetime_to_unix(filetime: u64) -> i64 {
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const SECONDS_TO_1601: u64 = 11_644_473_600;

    let seconds = filetime / TICKS_PER_SECOND;
    seconds.saturating_sub(SECONDS_TO_1601) as i64
}
