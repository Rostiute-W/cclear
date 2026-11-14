//! helper 的文件类系统操作：`Windows\Temp`、`Windows\Logs`、`$SysReset`。
//!
//! 三者都按冻结清单删除：清单在这里按当前状态生成，删除只走路径护栏和执行器，
//! 清单之外的路径一个都不碰。被占用的文件由护栏报“被占用”，执行器跳过并记录。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use crate::exec::executor::{ExecReport, execute};
use crate::guard::Forbidden;
use crate::journal::JournalSink;
use crate::model::{KnownRoot, Recovery};
use crate::plan::{FrozenPlan, PlanOptions, build_plan_selected};
use crate::rules::catalog::{SelectKind, Selector};
use crate::rules::resolve::RootMap;
use crate::win::dirinfo;
use crate::win::process::running_processes;

use super::args::SystemOp;
use super::{OpOutcome, format_size, windows_dir};

/// `Windows\Temp` 只清 max(ctime, mtime) 超过 7 天的文件。
const TEMP_MIN_AGE_HOURS: u32 = 168;
/// `Windows\Logs` 只清超过 30 天的日志、跟踪记录和归档。
const LOGS_MIN_AGE_HOURS: u32 = 720;
/// `Windows\Logs` 下要清的扩展名。其他文件可能是应用自己的数据，不在范围内。
const LOG_EXTENSIONS: &[&str] = &["log", "etl", "cab"];

/// 执行完成后把清单统计翻译成界面能看的一句话。
pub(super) fn run(
    op: SystemOp,
    forbidden: &Forbidden,
    roots: &RootMap,
    journal: &mut dyn JournalSink,
) -> OpOutcome {
    let token = op.token();

    let (rule_id, anchor) = match target(op, roots) {
        Ok(target) => target,
        Err(detail) => return OpOutcome::failed(token, detail),
    };
    // 锚点不存在或本身是链接时直接跳过：链接会把删除带到别的位置。
    if !dirinfo::is_plain_dir(&anchor) {
        return OpOutcome::skipped(
            token,
            format!("目标目录不存在或不是普通目录：{}", anchor.display()),
        );
    }

    let procs = running_processes();
    let plan = build_plan_selected(
        rule_id,
        Recovery::IrreversibleFile,
        std::slice::from_ref(&anchor),
        options(op),
        forbidden,
        &procs,
        SystemTime::now(),
    );

    if plan.items.is_empty() {
        return OpOutcome::skipped(token, empty_reason(op, &plan));
    }

    // helper 没有取消按钮，取消开关恒为 false；进度只在结果里给出。
    let cancel = AtomicBool::new(false);
    let report = execute(
        &plan,
        &[rule_id],
        forbidden,
        &|| running_processes(),
        journal,
        &cancel,
        &|_| {},
    );

    outcome(op, &report)
}

/// 操作对应的规则 ID 和锚点目录。路径全部由 Known Folder 解析，命令行里没有。
fn target(op: SystemOp, roots: &RootMap) -> Result<(&'static str, PathBuf), String> {
    match op {
        SystemOp::WindowsTemp => {
            let windows = windows_dir(roots)?;
            Ok(("system-windows-temp", windows.join("Temp")))
        }
        SystemOp::WindowsLogs => {
            let windows = windows_dir(roots)?;
            Ok(("system-windows-logs", windows.join("Logs")))
        }
        SystemOp::SysReset => {
            let system_drive = roots
                .get(KnownRoot::SystemDrive)
                .ok_or_else(|| "无法解析系统盘根目录".to_string())?;
            Ok(("system-sysreset", system_drive.join("$SysReset")))
        }
        _ => Err(format!("{op} 不是文件类系统操作")),
    }
}

/// 每个操作的筛选条件。
fn options(op: SystemOp) -> PlanOptions {
    match op {
        // 只看 Temp 直属的文件：它的子目录里可能是正在使用的新内容。
        SystemOp::WindowsTemp => PlanOptions {
            selector: Selector {
                kind: SelectKind::TopFiles(&["*"]),
                min_age_hours: Some(TEMP_MIN_AGE_HOURS),
                exclude_newest_per_dir: false,
                exclude_child_dirs: &[],
            },
            extensions: &[],
            keep_dirs: false,
        },
        // 日志目录要递归：.etl 和 .cab 常放在子目录里。
        SystemOp::WindowsLogs => PlanOptions {
            selector: Selector {
                kind: SelectKind::AllContents,
                min_age_hours: Some(LOGS_MIN_AGE_HOURS),
                exclude_newest_per_dir: false,
                exclude_child_dirs: &[],
            },
            extensions: LOG_EXTENSIONS,
            keep_dirs: false,
        },
        // $SysReset 是重置功能的临时目录，内容全部可以清掉。
        _ => PlanOptions {
            selector: Selector {
                kind: SelectKind::AllContents,
                min_age_hours: None,
                exclude_newest_per_dir: false,
                exclude_child_dirs: &[],
            },
            extensions: &[],
            keep_dirs: true,
        },
    }
}

/// 清单为空时的说明：优先用生成清单时记下的跳过原因。
fn empty_reason(op: SystemOp, plan: &FrozenPlan) -> String {
    if let Some((path, reason)) = plan
        .rules
        .first()
        .and_then(|rule| rule.skipped_at_build.first())
    {
        return format!("没有可清理的条目：{}（{reason}）", path.display());
    }

    match op {
        SystemOp::WindowsTemp => format!("没有超过 {} 天的文件", TEMP_MIN_AGE_HOURS / 24),
        SystemOp::WindowsLogs => format!("没有超过 {} 天的日志文件", LOGS_MIN_AGE_HOURS / 24),
        _ => "没有可清理的条目".to_string(),
    }
}

/// 执行报告到操作结果。
fn outcome(op: SystemOp, report: &ExecReport) -> OpOutcome {
    let token = op.token();
    let files: u64 = report.rules.iter().map(|rule| rule.deleted_files).sum();
    let dirs: u64 = report.rules.iter().map(|rule| rule.deleted_dirs).sum();
    let freed: u64 = report.rules.iter().map(|rule| rule.freed_allocated).sum();

    let mut detail = format!("已删除 {files} 个文件，释放 {}", format_size(freed));
    if dirs > 0 {
        detail.push_str(&format!("；顺带删除 {dirs} 个已空的目录"));
    }
    if report.skipped_total > 0 {
        detail.push_str(&format!(
            "；{} 个条目被占用或已变化，已跳过",
            report.skipped_total
        ));
    }

    if let Some(aborted) = &report.aborted {
        return OpOutcome::failed(token, format!("{detail}；已中止：{aborted}"));
    }

    // 只删掉空目录也算做过事，不能报成“跳过”。
    let mut result = if files > 0 || dirs > 0 {
        OpOutcome::done(token, detail)
    } else {
        OpOutcome::skipped(token, detail)
    };
    result.items = files;
    result
}
