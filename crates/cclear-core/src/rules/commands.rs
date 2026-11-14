//! 开发工具的官方清理命令：命令表、评估与执行。
//!
//! 命令只在 PATH 里已经装好的工具上调用，参数全部是内置常量。
//! 失败、超时、路径不在 C 盘都只报告并跳过，绝不退回直接删除缓存目录。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exec::command::{CmdOutcome, resolve_tool, run};
use crate::journal::{JournalSink, Record, unix_now};
use crate::model::SizeInfo;
use crate::rules::catalog::CommandId;
use crate::rules::resolve::measure_paths;
use crate::win::volume;

/// 查询缓存路径的超时。查询只读工具配置，正常几百毫秒。
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// 一条官方清理命令。
///
/// 一条规则可能需要多组子命令按顺序执行。例如 `dotnet nuget locals` 不接受
/// `http-cache,temp` 这种逗号拼接的写法，只能依次清理 `http-cache` 和 `temp`。
pub struct CommandSpec {
    /// 工具名。按 PATH 查找，也用来在界面上说明调用的是哪个工具。
    pub tool: &'static str,
    /// 查询缓存路径的子命令，按顺序执行。
    pub query_steps: &'static [&'static [&'static str]],
    /// 清理用的子命令，按顺序执行，中途失败就停下。
    pub clear_steps: &'static [&'static [&'static str]],
    /// 每组子命令的超时。超时只报告，不结束进程。
    pub timeout: Duration,
    /// 是否需要在普通确认之后再确认一次。
    pub needs_second_confirm: bool,
}

/// 命令表。每一条都对应规则目录里的一个 `CommandId`。
///
/// NuGet 的 `http-cache` 和 `temp` 归同一条规则 `command-nuget-http-temp`，
/// 各自执行一次增删查询和清理；`global-packages` 单列并需要二次确认。
static COMMANDS: &[(CommandId, CommandSpec)] = &[
    (
        CommandId::NpmCacheClean,
        CommandSpec {
            tool: "npm",
            query_steps: &[&["config", "get", "cache"]],
            clear_steps: &[&["cache", "clean", "--force"]],
            timeout: Duration::from_secs(600),
            needs_second_confirm: false,
        },
    ),
    (
        CommandId::PnpmStorePrune,
        CommandSpec {
            tool: "pnpm",
            query_steps: &[&["store", "path"]],
            clear_steps: &[&["store", "prune"]],
            timeout: Duration::from_secs(600),
            needs_second_confirm: false,
        },
    ),
    (
        CommandId::PipCachePurge,
        CommandSpec {
            tool: "pip",
            query_steps: &[&["cache", "dir"]],
            clear_steps: &[&["cache", "purge"]],
            timeout: Duration::from_secs(600),
            needs_second_confirm: false,
        },
    ),
    (
        CommandId::UvCachePrune,
        CommandSpec {
            tool: "uv",
            query_steps: &[&["cache", "dir"]],
            clear_steps: &[&["cache", "prune"]],
            timeout: Duration::from_secs(900),
            needs_second_confirm: false,
        },
    ),
    (
        CommandId::UvCacheClean,
        CommandSpec {
            tool: "uv",
            query_steps: &[&["cache", "dir"]],
            clear_steps: &[&["cache", "clean"]],
            timeout: Duration::from_secs(900),
            needs_second_confirm: true,
        },
    ),
    (
        CommandId::NuGetHttpTemp,
        CommandSpec {
            tool: "dotnet",
            query_steps: &[
                &["nuget", "locals", "http-cache", "--list"],
                &["nuget", "locals", "temp", "--list"],
            ],
            clear_steps: &[
                &["nuget", "locals", "http-cache", "--clear"],
                &["nuget", "locals", "temp", "--clear"],
            ],
            timeout: Duration::from_secs(600),
            needs_second_confirm: false,
        },
    ),
    (
        CommandId::NuGetGlobalPackages,
        CommandSpec {
            tool: "dotnet",
            query_steps: &[&["nuget", "locals", "global-packages", "--list"]],
            clear_steps: &[&["nuget", "locals", "global-packages", "--clear"]],
            timeout: Duration::from_secs(900),
            needs_second_confirm: true,
        },
    ),
];

/// 命令的评估结果。
pub enum CommandCheck {
    /// PATH 里找不到工具，等于没装。
    NotPresent,
    /// 查不到缓存路径，或者缓存在 C 盘以外，这条规则不能执行。
    Unusable(String),
    /// 可以调用命令清理这些路径。大小由调用方按同一套并行枚举统计。
    Ready { paths: Vec<PathBuf> },
}

/// 一次命令执行的结果。
pub struct CommandReport {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub size_before: SizeInfo,
    pub size_after: SizeInfo,
    /// 命令输出末尾（stdout 在前、stderr 在后），失败时给用户看原因。
    pub output: String,
    /// 命令已经执行但日志写入失败时的原因。界面要如实提示这次操作没有记录。
    pub log_error: Option<String>,
}

/// 按 id 取命令表里的条目。表里没有这个 id 说明规则挂了命令却忘了填。
pub fn spec(id: CommandId) -> Option<&'static CommandSpec> {
    COMMANDS
        .iter()
        .find(|(command, _)| *command == id)
        .map(|(_, spec)| spec)
}

/// 评估一条命令规则：工具在不在、缓存在不在 C 盘。
pub fn check(id: CommandId) -> CommandCheck {
    let Some(spec) = spec(id) else {
        return CommandCheck::Unusable("没有为这条规则配置清理命令".to_string());
    };

    if resolve_tool(spec.tool).is_none() {
        return CommandCheck::NotPresent;
    }

    match cache_paths(spec) {
        Ok(paths) => CommandCheck::Ready { paths },
        Err(reason) => CommandCheck::Unusable(reason),
    }
}

/// 一次执行多组子命令的结果。任意一组超时或非零退出码就停下，状态不再被后面的步骤覆盖。
struct StepsOutcome {
    exit_code: Option<i32>,
    timed_out: bool,
    /// 各组输出的末尾，按执行顺序拼起来。
    output: String,
}

/// 按顺序执行多组子命令，遇到超时或非零退出码就停止。
///
/// 中途的失败必须保留：后面的命令本身可能什么都不做，继续跑会把失败状态覆盖成成功。
fn run_steps(tool: &Path, steps: &[&[&str]], timeout: Duration) -> StepsOutcome {
    let mut outputs: Vec<String> = Vec::new();
    let mut exit_code = Some(0);
    let mut timed_out = false;

    for step in steps {
        let outcome = run(tool, step, timeout);

        let text = output_text(&outcome);
        if !text.is_empty() {
            outputs.push(text);
        }

        exit_code = outcome.exit_code;
        if outcome.timed_out || outcome.exit_code != Some(0) {
            timed_out = outcome.timed_out;
            break;
        }
    }

    StepsOutcome {
        exit_code,
        timed_out,
        output: outputs.join("\n"),
    }
}

/// 执行清理命令。
///
/// 需要二次确认的规则在 `second_confirm = false` 时直接拒绝，不做任何事。
pub fn execute(
    rule_id: &'static str,
    id: CommandId,
    second_confirm: bool,
    journal: &mut dyn JournalSink,
) -> Result<CommandReport, String> {
    let spec = spec(id).ok_or_else(|| "没有为这条规则配置清理命令".to_string())?;

    if spec.needs_second_confirm && !second_confirm {
        return Err("这条命令会删除全部缓存，需要二次确认后才能执行".to_string());
    }

    let tool = resolve_tool(spec.tool)
        .ok_or_else(|| format!("没有在 PATH 里找到 {}，请先安装", spec.tool))?;

    // 清理前重新解析一次路径：评估到执行之间工具配置可能已经变了。
    let paths = cache_paths(spec)?;
    let size_before = measure_paths(&paths);

    let outcome = run_steps(&tool, spec.clear_steps, spec.timeout);
    let size_after = measure_paths(&paths);

    let record = Record::CommandRun {
        at: unix_now(),
        rule_id: rule_id.to_string(),
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        allocated_before: size_before.allocated,
        allocated_after: size_after.allocated,
    };
    // 命令已经跑过了，日志写不进去也要把结果交回去，只把问题一起报告。
    let log_error = journal
        .append(&record)
        .err()
        .map(|error| format!("命令已执行，但操作日志写入失败：{error}"));

    Ok(CommandReport {
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        size_before,
        size_after,
        output: outcome.output,
        log_error,
    })
}

/// 查询工具自己的缓存路径，并确认这些路径都在系统盘。
fn cache_paths(spec: &'static CommandSpec) -> Result<Vec<PathBuf>, String> {
    let tool = resolve_tool(spec.tool)
        .ok_or_else(|| format!("没有在 PATH 里找到 {}，请先安装", spec.tool))?;

    let mut paths: Vec<PathBuf> = Vec::new();

    for step in spec.query_steps {
        let outcome = run(&tool, step, QUERY_TIMEOUT);
        if outcome.timed_out {
            return Err(format!(
                "查询缓存路径超过 {} 秒没有返回",
                QUERY_TIMEOUT.as_secs()
            ));
        }
        if outcome.exit_code != Some(0) {
            return Err(format!(
                "查询缓存路径失败（退出码 {}）：{}",
                exit_code_text(outcome.exit_code),
                output_text(&outcome)
            ));
        }

        // 不同子命令可能报出同一个目录（例如 NuGet 的 http-cache 和 temp），只统计一次。
        for path in parse_paths(&outcome.stdout_tail) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }

    if paths.is_empty() {
        return Err("没有从命令输出里读到缓存路径".to_string());
    }

    for path in &paths {
        drive_check(path)?;
    }

    Ok(paths)
}

/// 缓存路径必须在 C 盘。按路径本身（跟随链接后）所在的卷比较，盘符变化或目录重定向到别的盘都能认出来。
fn drive_check(path: &Path) -> Result<(), String> {
    let Some(system) = volume::system_volume_serial() else {
        return Err("无法读取系统盘的卷序列号".to_string());
    };

    match volume::path_volume_serial(path) {
        Some(serial) if serial == system => Ok(()),
        Some(_) => Err(format!("缓存不在 C 盘：{}", path.display())),
        None => Err(format!("无法判断缓存所在的卷：{}", path.display())),
    }
}

/// 从查询命令的输出里取出缓存路径。
///
/// NuGet 每一项是 `名字: <路径>`，npm、pnpm、pip、uv 直接输出路径本身。
/// 迁移复用同一套解析，两个地方看到的路径口径才不会不一致。
pub(crate) fn parse_paths(output: &str) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // 名字里不会有路径分隔符，用它区分「名字: 路径」和盘符里的冒号。
        let text = match line.split_once(": ") {
            Some((name, rest)) if !name.contains(['\\', '/']) && looks_like_path(rest.trim()) => {
                rest.trim()
            }
            _ => line,
        };

        if !looks_like_path(text) {
            continue;
        }

        let path = PathBuf::from(text);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }

    paths
}

/// 只认盘符开头和 UNC 的写法，避免把命令输出的提示语当成目录。
fn looks_like_path(text: &str) -> bool {
    let head: Vec<char> = text.chars().take(3).collect();

    match head.as_slice() {
        // npm 有时输出带正斜杠的路径，两种分隔符都认。
        [letter, ':', separator] => {
            letter.is_ascii_alphabetic() && (*separator == '\\' || *separator == '/')
        }
        ['\\', '\\', _] => true,
        _ => false,
    }
}

/// 命令输出末尾，stdout 和 stderr 各取一段。
pub(crate) fn output_text(outcome: &CmdOutcome) -> String {
    let stdout = outcome.stdout_tail.trim();
    let stderr = outcome.stderr_tail.trim();

    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

/// 退出码的显示文本。命令没跑起来时是 None。
fn exit_code_text(exit_code: Option<i32>) -> String {
    match exit_code {
        Some(code) => code.to_string(),
        None => "未知".to_string(),
    }
}
