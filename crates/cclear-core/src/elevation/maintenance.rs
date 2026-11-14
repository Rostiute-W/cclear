//! helper 的长耗时系统维护操作：DISM 组件存储分析与清理、传递优化缓存。
//!
//! 外部进程一律经 [`crate::exec::command`] 启动，可执行文件全部用绝对路径。
//! DISM 不加 `/ResetBase`：加上之后就不能再卸载已安装的更新。超时只停止等待，不结束进程。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exec::command::{self, CmdOutcome};
use crate::model::KnownRoot;
use crate::rules::resolve::RootMap;
use crate::win::volume;

use super::args::SystemOp;
use super::progress::ProgressWriter;
use super::{OpOutcome, format_size, windows_dir};

/// 分析组件存储一般一两分钟，留足余量。
const ANALYZE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// 组件存储清理可能十几分钟以上；超过一小时后只停止等待，不结束进程。
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// 传递优化缓存的官方 cmdlet 一般很快，超过十分钟只报告。
const DELIVERY_OPTIMIZATION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// 执行一个长耗时系统维护操作。
pub(super) fn run(op: SystemOp, roots: &RootMap, progress: Option<&Path>) -> OpOutcome {
    let token = op.token();

    let windows = match windows_dir(roots) {
        Ok(windows) => windows,
        Err(detail) => return OpOutcome::failed(token, detail),
    };

    match op {
        SystemOp::DismAnalyze => analyze(token, &system32(&windows, "Dism.exe")),
        SystemOp::DismComponentCleanup => cleanup(token, &system32(&windows, "Dism.exe"), progress),
        SystemOp::DeliveryOptimization => {
            delivery_optimization(token, &powershell(&windows), roots)
        }
        _ => OpOutcome::failed(token, format!("{op} 不是长耗时系统维护操作")),
    }
}

fn system32(windows: &Path, name: &str) -> PathBuf {
    windows.join("System32").join(name)
}

/// 传递优化缓存的 cmdlet 在 Windows PowerShell 5.1 里，PowerShell 7 默认没有这个模块。
fn powershell(windows: &Path) -> PathBuf {
    windows
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

/// 分析组件存储，只读，不改动任何内容。
fn analyze(token: String, tool: &Path) -> OpOutcome {
    if !tool.is_file() {
        return OpOutcome::failed(token, format!("找不到 DISM：{}", tool.display()));
    }

    // 所有 DISM 调用都带 /English（全局选项写在最前），输出里的字段名才能按固定文字解析。
    let args = [
        "/English",
        "/Online",
        "/Cleanup-Image",
        "/AnalyzeComponentStore",
    ];
    let outcome = command::run(tool, &args, ANALYZE_TIMEOUT);

    if outcome.timed_out {
        return OpOutcome::failed(
            token,
            "DISM 分析超时，已停止等待；进程可能仍在运行".to_string(),
        );
    }
    if outcome.exit_code != Some(0) {
        return OpOutcome::failed(token, failed_detail("DISM 分析", &outcome));
    }

    let info = parse_component_store(&outcome.stdout_tail);
    if info.is_empty() {
        return OpOutcome::failed(
            token,
            format!(
                "没有从 DISM 输出里解析出组件存储信息：{}",
                tail_line(&outcome.stdout_tail)
            ),
        );
    }

    OpOutcome::done(token, info.describe())
}

/// 清理组件存储。不加 `/ResetBase`，已安装的更新仍然可以卸载。
fn cleanup(token: String, tool: &Path, progress: Option<&Path>) -> OpOutcome {
    if !tool.is_file() {
        return OpOutcome::failed(token, format!("找不到 DISM：{}", tool.display()));
    }

    let args = [
        "/English",
        "/Online",
        "/Cleanup-Image",
        "/StartComponentCleanup",
    ];

    let mut writer = ProgressWriter::new(progress, &token);
    writer.note(
        None,
        "开始清理组件存储，可能需要 10–30 分钟，请勿关机".to_string(),
    );

    // DISM 每秒刷新一行 `[==  12.3%  ==]`；进度行可能被拆到两次输出里，这里按整行拼。
    let mut pending = String::new();
    let outcome = command::run_streaming(tool, &args, CLEANUP_TIMEOUT, &mut |chunk| {
        pending.push_str(chunk);
        while let Some(end) = pending.find(['\n', '\r']) {
            let line: String = pending.drain(..=end).collect();
            if let Some(percent) = parse_percent(&line) {
                writer.note(Some(percent), format!("组件存储清理中：{percent:.1}%"));
            }
        }
    });

    if outcome.timed_out {
        writer.note(None, "清理超时，已停止等待；命令可能仍在运行".to_string());
        return OpOutcome::failed(
            token,
            "DISM 清理超过一小时仍未结束，已停止等待；请稍后在历史里确认结果".to_string(),
        );
    }
    if outcome.exit_code != Some(0) {
        let detail = failed_detail("DISM 清理", &outcome);
        writer.note(None, detail.clone());
        return OpOutcome::failed(token, detail);
    }

    writer.note(Some(100.0), "组件存储清理完成".to_string());
    OpOutcome::done(
        token,
        "组件存储清理完成（没有使用 ResetBase，已安装的更新仍可卸载）".to_string(),
    )
}

/// 清空传递优化缓存，并记录执行前后的系统盘剩余空间。
fn delivery_optimization(token: String, tool: &Path, roots: &RootMap) -> OpOutcome {
    if !tool.is_file() {
        return OpOutcome::failed(token, format!("找不到 PowerShell：{}", tool.display()));
    }

    let Some(system_drive) = roots.get(KnownRoot::SystemDrive) else {
        return OpOutcome::failed(token, "无法解析系统盘根目录".to_string());
    };

    let before = volume::free_space(system_drive);
    // 只调用官方 cmdlet，不碰它的私有目录，也不停任何服务。
    let outcome = command::run(
        tool,
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Delete-DeliveryOptimizationCache -Force",
        ],
        DELIVERY_OPTIMIZATION_TIMEOUT,
    );
    let after = volume::free_space(system_drive);

    let detail = format!(
        "系统盘剩余空间 {} → {}",
        format_size(before),
        format_size(after)
    );

    if outcome.timed_out {
        return OpOutcome::failed(
            token,
            format!("调用 Delete-DeliveryOptimizationCache 超时，已停止等待；{detail}"),
        );
    }
    if outcome.exit_code != Some(0) {
        return OpOutcome::failed(
            token,
            format!(
                "{}；{detail}",
                failed_detail("Delete-DeliveryOptimizationCache", &outcome)
            ),
        );
    }

    OpOutcome::done(
        token,
        format!("已调用 Delete-DeliveryOptimizationCache 清空缓存；{detail}"),
    )
}

/// 分析结果里要显示的三项结论。
#[derive(Default)]
struct ComponentStoreInfo {
    actual_size: Option<String>,
    reclaimable: Option<String>,
    recommended: Option<String>,
}

impl ComponentStoreInfo {
    fn is_empty(&self) -> bool {
        self.actual_size.is_none() && self.reclaimable.is_none() && self.recommended.is_none()
    }

    /// 界面直接显示的一句话。缺失的项写“未知”，不能装作拿到了。
    fn describe(&self) -> String {
        format!(
            "组件存储实际大小 {}；可回收程序包 {}；建议清理 {}",
            self.actual_size.as_deref().unwrap_or("未知"),
            self.reclaimable.as_deref().unwrap_or("未知"),
            self.recommended.as_deref().unwrap_or("未知")
        )
    }
}

/// 从 DISM 的 `/English` 输出里取三项结论。字段名是固定的英文，值原样保留。
fn parse_component_store(text: &str) -> ComponentStoreInfo {
    let mut info = ComponentStoreInfo::default();

    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().to_string();

        if key.eq_ignore_ascii_case("Actual Size of Component Store") {
            info.actual_size = Some(value);
        } else if key.eq_ignore_ascii_case("Number of Reclaimable Packages") {
            info.reclaimable = Some(value);
        } else if key.eq_ignore_ascii_case("Component Store Cleanup Recommended") {
            info.recommended = Some(value);
        }
    }

    info
}

/// 从 DISM 的进度行里取百分比，例如 `[=====  12.3%  =====]`。
///
/// 不是进度行、或者百分比落在 0–100 之外时返回 None。
fn parse_percent(line: &str) -> Option<f32> {
    let (_, rest) = line.split_once('[')?;
    let (head, _) = rest.split_once('%')?;

    // 百分比紧跟在最后一串数字后面，前面的 `=` 和空格要剥掉。
    let token = head.split_whitespace().last()?;
    let token = token.trim_start_matches(|character: char| !character.is_ascii_digit());
    let percent: f32 = token.trim_end_matches('.').parse().ok()?;

    (0.0..=100.0).contains(&percent).then_some(percent)
}

/// 命令失败时的中文说明：退出码加上输出的最后一行。
fn failed_detail(what: &str, outcome: &CmdOutcome) -> String {
    let code = match outcome.exit_code {
        Some(code) => code.to_string(),
        None => "未知".to_string(),
    };

    let output = tail_line(&outcome.stderr_tail);
    let output = if output.is_empty() {
        tail_line(&outcome.stdout_tail)
    } else {
        output
    };

    if output.is_empty() {
        format!("{what}失败（退出码 {code}）")
    } else {
        format!("{what}失败（退出码 {code}）：{output}")
    }
}

/// 输出的最后一行有内容的文字。
fn tail_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}
