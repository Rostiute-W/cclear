//! 工具缓存目录的官方配置迁移。
//!
//! 顺序是固定的：先记下旧配置并落盘，再复制并写新配置，然后**另起一个进程**验证新位置，
//! 验证不通过就把旧配置恢复回去。旧缓存目录只有在用户单独确认之后才按冻结清单清理，
//! 绝不在这里直接删除。
//!
//! 状态机持久化在 `<LocalAppData>\CClear\migrations.json`：旧配置值必须能在进程被结束
//! 之后仍然读得到，否则就撤不回去了。

pub mod junction;
pub mod tools;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::exec::command::{resolve_tool, run, run_with_env};
use crate::guard::{self, DirHandle, Forbidden, GuardError};
use crate::journal::{JournalSink, Record, unix_now};
use crate::model::Recovery;
use crate::plan::{FrozenPlan, build_plan_at};
use crate::quarantine::copy_tree_verified;
use crate::rules::commands::{output_text, parse_paths};
use crate::rules::resolve::{RootMap, measure_paths};
use crate::win::dirinfo;
use crate::win::process::{ProcInfo, running_processes};
use crate::win::registry;
use crate::win::volume::{self, VolumeInfo};
use crate::win::{KnownFolder, equals_ignore_case, known_folder};
use tools::{CacheEntry, ToolId, ToolSpec};

/// 目标卷下存放各工具缓存的目录名。
pub const DATA_DIR_NAME: &str = "CClearData";

/// 清理旧缓存目录时用的临时规则 ID。它不在内置规则目录里，只出现在迁移的清理清单中。
pub const OLD_CACHE_RULE_ID: &str = "migration-old-cache";

/// 查询生效路径和验证命令的超时。npm 首次启动要读一堆配置，给足时间。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// 迁移记录的位置：`<LocalAppData>\CClear\migrations.json`。
const STORE_DIR: &str = "CClear";
const STORE_FILE: &str = "migrations.json";

/// 配置来源的说明文字，界面直接显示。
const SOURCE_ENV: &str = "环境变量";
const SOURCE_PROCESS_ENV: &str = "当前进程环境变量";
const SOURCE_NPMRC: &str = "npmrc";
const SOURCE_DEFAULT: &str = "默认值";

/// 迁移过程里的错误。每种失败都对应“停下并报告”，不会扩大改动范围。
#[derive(Debug, Error)]
pub enum MigrateError {
    #[error("没有这个工具：{0}")]
    UnknownTool(String),

    #[error("这个卷不能用来放缓存目录，请从候选卷里选：{0}")]
    BadVolume(String),

    #[error("迁移记录读不出来，请先手工检查：{0}")]
    Store(String),

    #[error("没有在 PATH 里找到 {0}，无法改配置或验证")]
    MissingTool(String),

    #[error("无法解析系统目录：{0}")]
    Roots(String),

    #[error("复制缓存失败：{0}")]
    Copy(String),

    #[error("新位置没有生效：{0}")]
    NotApplied(String),

    #[error("恢复旧配置失败：{0}")]
    RestoreFailed(String),

    #[error("这个工具还没有迁移成功的记录，不能做这一步")]
    NotMigrated,

    #[error("{0} 的缓存目录已经在这个位置，不需要再迁移")]
    AlreadyMigrated(String),

    #[error("这些配置里引用了环境变量，自动改写会改变它的展开方式，请手工迁移：{0}")]
    ExpandableConfig(String),

    #[error("旧缓存目录里没有可清理的内容：{0}")]
    NothingToClean(String),

    #[error("路径护栏拒绝了这个操作：{0}")]
    Guard(#[from] GuardError),

    #[error("文件操作失败：{0}")]
    Io(#[from] io::Error),

    #[error("日志写入失败：{0}")]
    Log(io::Error),
}

/// 迁移方式：先把旧缓存搬过去，或者干脆让工具自己重建。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// 复制旧缓存到新位置并逐个校验。
    Migrate,
    /// 不复制，工具在新位置重新下载。
    Rebuild,
}

impl Mode {
    /// 命令参数里用的名字。
    pub fn key(self) -> &'static str {
        match self {
            Mode::Migrate => "migrate",
            Mode::Rebuild => "rebuild",
        }
    }

    /// 按名字解析。前端只传这个名字。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "migrate" => Some(Mode::Migrate),
            "rebuild" => Some(Mode::Rebuild),
            _ => None,
        }
    }

    /// 界面上的叫法。
    pub fn label(self) -> &'static str {
        match self {
            Mode::Migrate => "迁移旧缓存",
            Mode::Rebuild => "允许重建",
        }
    }
}

/// 迁移阶段。状态机持久化在 migrations.json 里。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    /// 旧配置已经记下，还没改。
    Planned,
    /// 新配置已写入，等待验证。
    Configured,
    /// 新位置已经验证生效。
    Verified,
    /// 验证失败，配置已经恢复。
    Failed,
    /// 已经生成清理旧目录的清单。
    CleanupPlanned,
    /// 旧目录已经清理。
    Cleaned,
    /// 已经撤销，恢复旧配置。
    Reverted,
}

impl Stage {
    /// 机器可读的阶段名。
    pub fn key(self) -> &'static str {
        match self {
            Stage::Planned => "planned",
            Stage::Configured => "configured",
            Stage::Verified => "verified",
            Stage::Failed => "failed",
            Stage::CleanupPlanned => "cleanupPlanned",
            Stage::Cleaned => "cleaned",
            Stage::Reverted => "reverted",
        }
    }

    /// 界面上的叫法。
    pub fn label(self) -> &'static str {
        match self {
            Stage::Planned => "已记录旧配置",
            Stage::Configured => "已写入新配置，等待验证",
            Stage::Verified => "新位置已生效",
            Stage::Failed => "验证失败，已恢复旧配置",
            Stage::CleanupPlanned => "已生成旧目录清理清单",
            Stage::Cleaned => "旧目录已清理",
            Stage::Reverted => "已撤销，恢复旧配置",
        }
    }
}

/// 写配置前记下的旧值。`None` 表示这一项原来没有配置，撤销时要删掉。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OldValue {
    /// 环境变量名，或者 `npm cache` 这样的配置项名。
    pub name: String,
    pub value: Option<String>,
}

/// 一个工具的迁移记录。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Migration {
    pub tool: String,
    pub stage: Stage,
    pub mode: Mode,
    /// 用户选的目标卷根，例如 `D:\`。
    pub volume_root: String,
    /// 新的缓存根目录，也就是 `<卷>:\CClearData`。
    pub target: String,
    /// 迁移前的生效路径，清理旧目录以它为锚点。
    pub old_paths: Vec<String>,
    pub old_values: Vec<OldValue>,
    /// 这一步做了什么，或者为什么失败。
    pub note: String,
    /// Unix 秒。
    pub updated_at: i64,
}

/// 一项配置的当前状态。
pub struct EntryState {
    /// 环境变量名，或者 `npm cache` 这样的配置项名。
    pub name: String,
    /// 当前生效的路径。
    pub path: PathBuf,
    /// 配置来源：环境变量 / 当前进程环境变量 / npmrc / 默认值。
    pub source: String,
    pub exists: bool,
    /// 占盘大小。目录不存在时为 0。
    pub size: u64,
    pub on_system_drive: bool,
}

/// 一个工具的当前状态和迁移记录。
pub struct ToolState {
    pub spec: &'static ToolSpec,
    pub entries: Vec<EntryState>,
    pub total_size: u64,
    /// 任何一项还在系统盘，就算这个工具还没搬走。
    pub on_system_drive: bool,
    /// 工具是否在 PATH 里。没有它就没法验证新位置。
    pub installed: bool,
    /// 相关进程里正在运行的那些。
    pub running: Vec<&'static str>,
    pub migration: Option<Migration>,
    /// 记录里存在、现在还存在、可以去清理的旧目录。
    pub old_dirs: Vec<PathBuf>,
}

impl ToolState {
    /// 记录里的阶段，旧目录都已经不在了就按“已清理”显示。
    ///
    /// 清理旧目录由用户在执行页单独确认，后台拿不到那一步的结果，
    /// 所以这里按“目录还在不在”对账，而不是等一个永远不会来的回调。
    pub fn stage(&self) -> Option<Stage> {
        let migration = self.migration.as_ref()?;

        if migration.stage == Stage::CleanupPlanned && self.old_dirs.is_empty() {
            return Some(Stage::Cleaned);
        }

        Some(migration.stage)
    }
}

/// 全部工具的状态，界面表格用它。
pub fn list() -> Result<Vec<ToolState>, MigrateError> {
    let roots = root_map()?;
    let migrations = load_all()?;
    let procs = running_processes();

    Ok(tools::all()
        .iter()
        .map(|spec| tool_state(spec, &migrations, &procs, &roots))
        .collect())
}

/// 单个工具的详情。开始迁移之前界面用它拿最新的进程警告。
pub fn inspect(tool: ToolId) -> Result<ToolState, MigrateError> {
    let spec =
        tools::spec(tool).ok_or_else(|| MigrateError::UnknownTool(tool.key().to_string()))?;
    let roots = root_map()?;
    let migrations = load_all()?;
    let procs = running_processes();

    Ok(tool_state(spec, &migrations, &procs, &roots))
}

/// 可以作为缓存盘的目标卷：本地固定 NTFS 盘，排除系统盘。
///
/// 缓存要离开的正是系统盘，目标必须来自这份列表，前端不能自己拼卷根。
pub fn candidate_volumes() -> Vec<VolumeInfo> {
    let mut volumes: Vec<VolumeInfo> = volume::list_fixed_volumes()
        .into_iter()
        .filter(|info| !info.is_system && equals_ignore_case(&info.fs, "NTFS"))
        .collect();

    volumes.sort_by_key(|info| std::cmp::Reverse(info.free));
    volumes
}

/// 开始迁移：记录旧配置 → 复制（可选）→ 写新配置 → 新进程验证。
///
/// 验证不通过时自动恢复旧配置并返回错误，不会把工具留在一个“配置改了但没生效”的状态上。
pub fn start(
    tool: ToolId,
    volume_root: &str,
    mode: Mode,
    journal: &mut dyn JournalSink,
) -> Result<Migration, MigrateError> {
    let spec =
        tools::spec(tool).ok_or_else(|| MigrateError::UnknownTool(tool.key().to_string()))?;
    let roots = root_map()?;

    let volume = candidate_volumes()
        .into_iter()
        .find(|info| equals_ignore_case(&info.root.to_string_lossy(), volume_root))
        .ok_or_else(|| MigrateError::BadVolume(volume_root.to_string()))?;
    let base = volume.root.join(DATA_DIR_NAME);

    // 要改配置就得先找到工具：npm 用命令写 npmrc，其余工具用命令验证新位置。
    // 找不到工具就别动配置，否则改完没法验证，等于把用户丢在不确定的状态里。
    if needs_program(spec) && resolve_tool(spec.program).is_none() {
        return Err(MigrateError::MissingTool(spec.program.to_string()));
    }

    let current: Vec<(PathBuf, String)> = spec
        .entries
        .iter()
        .map(|entry| current_path(spec, entry, &roots))
        .collect();

    // 所有配置项都已经指向目标目录时没有任何可迁移的内容。
    // 这一条必须在写记录之前：否则会把正在使用的缓存目录记成“旧目录”，
    // 之后的清理清单就会直接指向当前生效的缓存。
    let already_there = spec
        .entries
        .iter()
        .enumerate()
        .all(|(index, entry)| same_dir(&current[index].0, &target_of(&base, entry)));
    if already_there {
        return Err(MigrateError::AlreadyMigrated(spec.label.to_string()));
    }

    // 旧值里带 %VAR% 的配置写回去会变成字面量（REG_SZ 不再展开），
    // 这种情况宁可不迁移，也不能留下一个撤不干净的配置。
    let old_values: Vec<OldValue> = spec
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| old_value(entry, &current[index]))
        .collect();
    let expandable = expanded_names(&old_values);
    if !expandable.is_empty() {
        return Err(MigrateError::ExpandableConfig(expandable.join("、")));
    }

    let mut migration = Migration {
        tool: tool.key().to_string(),
        stage: Stage::Planned,
        mode,
        volume_root: volume.root.to_string_lossy().into_owned(),
        target: base.to_string_lossy().into_owned(),
        old_paths: current
            .iter()
            .map(|(path, _)| path.to_string_lossy().into_owned())
            .collect(),
        old_values,
        note: "已记录旧配置".to_string(),
        updated_at: unix_now(),
    };

    // 复制阶段配置还没改：失败只影响这次迁移，现有配置和上一次的记录都不动。
    if mode == Mode::Migrate {
        for (index, entry) in spec.entries.iter().enumerate() {
            let target = target_of(&base, entry);
            let (old, _) = &current[index];
            copy_cache(old, &target)
                .map_err(|error| MigrateError::Copy(format!("{}：{error}", target.display())))?;
        }
    } else {
        for entry in spec.entries {
            std::fs::create_dir_all(target_of(&base, entry))?;
        }
    }

    // 复制完成之后、写配置之前才落盘旧值：复制失败时不能把上一次的迁移记录
    // 覆盖成“已记录旧配置”，否则用户就看不到之前那次的有效状态了。
    save(&migration)?;
    write_log(journal, &migration)?;

    // 写配置。写失败就把已经写进去的恢复掉，不留下半套配置。
    if let Err(error) = write_config(spec, &base) {
        let restored = restore(spec, &migration.old_values);
        migration.stage = Stage::Failed;
        migration.note = failure_note("写新配置失败", &error.to_string(), &restored);
        record_failure(&migration, journal);
        return Err(error);
    }

    migration.stage = Stage::Configured;
    migration.note = "已写入新配置，正在验证".to_string();
    save(&migration)?;
    write_log(journal, &migration)?;

    // 验证：另起一个进程，环境块是当前环境加新变量。
    match verify(spec, &base) {
        Ok(()) => {
            migration.stage = Stage::Verified;
            migration.note = format!(
                "新缓存位置已经生效：{}。请重启 IDE 和终端，旧目录可以在你确认后单独清理",
                base.display()
            );
            save(&migration)?;
            write_log(journal, &migration)?;
            Ok(migration)
        }
        Err(error) => {
            let restored = restore(spec, &migration.old_values);
            migration.stage = Stage::Failed;
            migration.note = failure_note("验证失败", &error.to_string(), &restored);
            record_failure(&migration, journal);
            Err(error)
        }
    }
}

/// 为清理旧缓存目录生成冻结清单。
///
/// 这里只生成清单：真正的删除由用户在预览页确认后交给执行器，按清单逐项复核。
/// 旧目录本身会留成一个空目录。
pub fn cleanup_plan(
    tool: ToolId,
    journal: &mut dyn JournalSink,
) -> Result<FrozenPlan, MigrateError> {
    let spec =
        tools::spec(tool).ok_or_else(|| MigrateError::UnknownTool(tool.key().to_string()))?;
    let mut migration = load_one(tool.key())?.ok_or(MigrateError::NotMigrated)?;

    // 只有验证成功过的工具才有“可以清理的旧目录”。
    if !matches!(
        migration.stage,
        Stage::Verified | Stage::CleanupPlanned | Stage::Cleaned
    ) {
        return Err(MigrateError::NotMigrated);
    }

    let roots = root_map()?;
    let forbidden = Forbidden::from_rules(&roots);
    let current = current_paths(spec, &roots);

    // 正在使用的目录（当前生效目录和目标目录）一律不算旧目录，否则清单会指向活的缓存。
    let anchors: Vec<PathBuf> = migration
        .old_paths
        .iter()
        .map(PathBuf::from)
        .filter(|path| dirinfo::is_plain_dir(path))
        .filter(|path| !in_use(path, spec, &migration, &current))
        .collect();

    let plan = build_plan_at(
        OLD_CACHE_RULE_ID,
        Recovery::IrreversibleFile,
        &anchors,
        &forbidden,
        &running_processes(),
        SystemTime::now(),
    );
    if plan.items.is_empty() {
        // 把清单构建时记下的原因带出去：读不到目录、路径里出现链接都会让清单为空。
        let reason = plan
            .rules
            .first()
            .and_then(|rule| rule.skipped_at_build.first())
            .map(|(path, reason)| format!("{}：{reason}", path.display()))
            .unwrap_or_else(|| "旧目录可能已经被清理过".to_string());

        return Err(MigrateError::NothingToClean(reason));
    }

    let count = plan.items.len();
    migration.stage = Stage::CleanupPlanned;
    migration.note = format!("已生成旧缓存清理清单，共 {count} 个条目，确认后才会删除");
    migration.updated_at = unix_now();
    save(&migration)?;
    write_log(journal, &migration)?;

    Ok(plan)
}

/// 撤销：恢复旧配置。旧目录已经清理掉的工具会自己重建缓存。
pub fn revert(tool: ToolId, journal: &mut dyn JournalSink) -> Result<Migration, MigrateError> {
    let spec =
        tools::spec(tool).ok_or_else(|| MigrateError::UnknownTool(tool.key().to_string()))?;
    let mut migration = load_one(tool.key())?.ok_or(MigrateError::NotMigrated)?;

    restore(spec, &migration.old_values)?;

    migration.stage = Stage::Reverted;
    migration.note = "已恢复旧配置；旧缓存目录已经清理的工具会自己重建缓存".to_string();
    migration.updated_at = unix_now();
    save(&migration)?;
    write_log(journal, &migration)?;

    Ok(migration)
}

/// 按工具汇总当前状态。
fn tool_state(
    spec: &'static ToolSpec,
    migrations: &[Migration],
    procs: &[ProcInfo],
    roots: &RootMap,
) -> ToolState {
    let entries: Vec<EntryState> = spec
        .entries
        .iter()
        .map(|entry| entry_state(spec, entry, roots))
        .collect();

    let migration = migrations
        .iter()
        .find(|migration| migration.tool == spec.id.key())
        .cloned();

    // 旧目录只在迁移成功过之后才谈得上清理。
    let cleanup_allowed = migration.as_ref().is_some_and(|migration| {
        matches!(
            migration.stage,
            Stage::Verified | Stage::CleanupPlanned | Stage::Cleaned
        )
    });
    let current: Vec<PathBuf> = entries.iter().map(|entry| entry.path.clone()).collect();
    let old_dirs = match (&migration, cleanup_allowed) {
        (Some(migration), true) => migration
            .old_paths
            .iter()
            .map(PathBuf::from)
            .filter(|path| dirinfo::is_plain_dir(path))
            // 空目录已经没有可清理的内容：清理计划会把旧目录留成空目录，
            // 只看“目录还在”会让界面一直停在“已生成清理清单”。
            .filter(|path| has_entries(path))
            // 正在使用的目录绝不能当成旧目录，否则清理就是在删活的缓存。
            .filter(|path| !in_use(path, spec, migration, &current))
            .collect(),
        _ => Vec::new(),
    };

    ToolState {
        spec,
        total_size: entries.iter().map(|entry| entry.size).sum(),
        on_system_drive: entries.iter().any(|entry| entry.on_system_drive),
        installed: resolve_tool(spec.program).is_some(),
        running: spec
            .processes
            .iter()
            .copied()
            .filter(|name| {
                procs
                    .iter()
                    .any(|proc| equals_ignore_case(&proc.name, name))
            })
            .collect(),
        entries,
        migration,
        old_dirs,
    }
}

/// 目录里是否还有条目。清理计划只删目录内的条目，旧目录会留成空目录，
/// 空目录就等于“已经清理过”，界面上不该再提供清理按钮。
fn has_entries(path: &Path) -> bool {
    let forbidden = Forbidden::empty();
    let Ok(anchor) = guard::open_anchor(path, &forbidden) else {
        // 打不开时不能声称“已经清理干净”：保留按钮，让真正生成清单时报出原因。
        return true;
    };

    match guard::list_dir(&anchor, &[], &forbidden) {
        Ok(entries) => !entries.is_empty(),
        // 同上：读不出来就往“还有内容”这一侧判，宁可让用户点一次并看到原因。
        Err(_) => true,
    }
}

/// 这个路径是不是正在使用的缓存目录：当前生效目录，或者记录里的目标目录。
fn in_use(path: &Path, spec: &ToolSpec, migration: &Migration, current: &[PathBuf]) -> bool {
    let target = PathBuf::from(&migration.target);

    current.iter().any(|known| same_dir(known, path))
        || spec
            .entries
            .iter()
            .any(|entry| same_dir(&target_of(&target, entry), path))
}

/// 各配置项当前生效的路径。
fn current_paths(spec: &ToolSpec, roots: &RootMap) -> Vec<PathBuf> {
    spec.entries
        .iter()
        .map(|entry| current_path(spec, entry, roots).0)
        .collect()
}

/// 一项配置的当前状态。
fn entry_state(spec: &ToolSpec, entry: &CacheEntry, roots: &RootMap) -> EntryState {
    let (path, source) = current_path(spec, entry, roots);
    let exists = dirinfo::is_plain_dir(&path);

    EntryState {
        name: config_name(entry).to_string(),
        // 目录不存在就不去枚举它：不存在的路径统计出来只会是 0，白跑一趟。
        size: if exists {
            measure_paths(std::slice::from_ref(&path)).allocated
        } else {
            0
        },
        on_system_drive: on_system_drive(&path),
        path,
        source,
        exists,
    }
}

/// 当前生效的路径和它的来源。
fn current_path(spec: &ToolSpec, entry: &CacheEntry, roots: &RootMap) -> (PathBuf, String) {
    let default = default_path(entry, roots);

    let Some(var) = entry.env_var else {
        return npm_cache_dir(spec, default);
    };

    if let Some(text) = registry::read_env_string(var) {
        return (PathBuf::from(text), SOURCE_ENV.to_string());
    }
    // 注册表里没有、本进程里有：多半是这次会话里临时设的，重启后就不生效了。
    if let Some(text) = std::env::var_os(var) {
        return (PathBuf::from(text), SOURCE_PROCESS_ENV.to_string());
    }

    (default, SOURCE_DEFAULT.to_string())
}

/// npm 的缓存位置在用户级 npmrc 里，只能问 npm 本身。
fn npm_cache_dir(spec: &ToolSpec, default: PathBuf) -> (PathBuf, String) {
    let Some(tool) = resolve_tool(spec.program) else {
        return (default, format!("{SOURCE_DEFAULT}（没有找到 npm）"));
    };

    let outcome = run(&tool, &["config", "get", "cache"], COMMAND_TIMEOUT);
    if outcome.timed_out || outcome.exit_code != Some(0) {
        return (
            default,
            format!(
                "{SOURCE_DEFAULT}（读取 npm 配置失败：{}）",
                output_text(&outcome)
            ),
        );
    }

    let Some(path) = parse_paths(&outcome.stdout_tail).into_iter().next() else {
        return (
            default,
            format!("{SOURCE_DEFAULT}（没有从 npm 读到缓存路径）"),
        );
    };

    if equals_ignore_case(&path.to_string_lossy(), &default.to_string_lossy()) {
        return (path, SOURCE_DEFAULT.to_string());
    }
    // npm 先看环境变量 npm_config_cache，再读 npmrc。
    let from_env = registry::read_env_string("npm_config_cache").is_some()
        || std::env::var_os("npm_config_cache").is_some();
    if from_env {
        return (path, SOURCE_ENV.to_string());
    }

    (path, SOURCE_NPMRC.to_string())
}

/// 没有配置时工具的默认位置。
fn default_path(entry: &CacheEntry, roots: &RootMap) -> PathBuf {
    match roots.get(entry.default_root) {
        Some(base) => base.join(entry.default_rel),
        None => PathBuf::from(entry.default_rel),
    }
}

/// 当前生效的目录已经不在系统盘上。
fn on_system_drive(path: &Path) -> bool {
    match (
        volume::system_volume_serial(),
        volume::path_volume_serial(path),
    ) {
        (Some(system), Some(serial)) => serial == system,
        _ => false,
    }
}

/// 配置项在界面和记录里的名字。
fn config_name(entry: &CacheEntry) -> &'static str {
    entry.env_var.unwrap_or("npm cache")
}

/// 写配置之前记下旧值。
fn old_value(entry: &CacheEntry, current: &(PathBuf, String)) -> OldValue {
    let value = match entry.env_var {
        Some(var) => registry::read_env_string(var),
        // npm 的旧配置在 npmrc 里：和默认值不同才算“原来有配置”，撤销时要写回去。
        None => match current.1.as_str() {
            SOURCE_NPMRC => Some(current.0.to_string_lossy().into_owned()),
            _ => None,
        },
    };

    OldValue {
        name: config_name(entry).to_string(),
        value,
    }
}

/// 旧值里带 `%VAR%` 的配置项名。这类值原本是 `REG_EXPAND_SZ`，写回去只会变成字面量。
fn expanded_names(old_values: &[OldValue]) -> Vec<String> {
    old_values
        .iter()
        .filter(|value| value.value.as_ref().is_some_and(|text| text.contains('%')))
        .map(|value| value.name.clone())
        .collect()
}

/// 新缓存目录的完整路径。
fn target_of(base: &Path, entry: &CacheEntry) -> PathBuf {
    base.join(entry.dir_name)
}

/// 改配置或验证是否需要工具本身。Gradle 只写环境变量、只检查目录，不需要。
pub fn needs_program(spec: &ToolSpec) -> bool {
    spec.validate_args.is_some() || spec.entries.iter().any(|entry| entry.env_var.is_none())
}

/// 把旧缓存复制到新位置，逐个文件校验。
///
/// 空目录不复制：工具自己会重建。链接也不复制：它指向的内容不在这个目录里。
fn copy_cache(old: &Path, target: &Path) -> Result<(), MigrateError> {
    if !dirinfo::is_plain_dir(old) {
        std::fs::create_dir_all(target)?;
        return Ok(());
    }

    let forbidden = Forbidden::empty();
    let anchor = guard::open_anchor(old, &forbidden)?;
    let mut rels: Vec<Vec<OsString>> = Vec::new();
    collect_files(&anchor, &mut Vec::new(), &mut rels, &forbidden)?;

    if rels.is_empty() {
        std::fs::create_dir_all(target)?;
        return Ok(());
    }

    copy_tree_verified(old, &rels, target)
        .map_err(|error| MigrateError::Copy(error.to_string()))?;

    Ok(())
}

/// 列出目录下的全部文件（相对锚点的组件）。
fn collect_files(
    anchor: &DirHandle,
    rel: &mut Vec<OsString>,
    out: &mut Vec<Vec<OsString>>,
    forbidden: &Forbidden,
) -> Result<(), MigrateError> {
    for entry in guard::list_dir(anchor, rel, forbidden)? {
        if entry.is_reparse() {
            continue;
        }

        rel.push(entry.name.clone());
        if entry.is_dir {
            collect_files(anchor, rel, out, forbidden)?;
        } else {
            out.push(rel.clone());
        }
        rel.pop();
    }

    Ok(())
}

/// 写入新配置。用户环境变量写完广播一次，让之后新启动的进程能读到。
fn write_config(spec: &ToolSpec, base: &Path) -> Result<(), MigrateError> {
    for entry in spec.entries {
        let target = target_of(base, entry);

        match entry.env_var {
            Some(var) => {
                registry::write_env_string(var, &target.to_string_lossy()).map_err(|error| {
                    MigrateError::Io(io::Error::other(format!("写环境变量 {var} 失败：{error}")))
                })?;
            }
            None => set_npm_cache(spec, &target)?,
        }
    }

    if spec.entries.iter().any(|entry| entry.env_var.is_some()) {
        // 广播只影响之后新启动的进程，失败也不影响配置本身，所以只记录不报错。
        let _ = registry::broadcast_environment_change();
    }

    Ok(())
}

/// 用 npm 自己的命令改用户级 npmrc。
fn set_npm_cache(spec: &ToolSpec, path: &Path) -> Result<(), MigrateError> {
    let tool = resolve_tool(spec.program)
        .ok_or_else(|| MigrateError::MissingTool(spec.program.to_string()))?;
    let text = path.to_string_lossy().into_owned();

    let outcome = run(
        &tool,
        &["config", "set", "cache", &text, "--location=user"],
        COMMAND_TIMEOUT,
    );
    if outcome.timed_out || outcome.exit_code != Some(0) {
        return Err(MigrateError::Io(io::Error::other(format!(
            "npm 配置写入失败（退出码 {:?}）：{}",
            outcome.exit_code,
            output_text(&outcome)
        ))));
    }

    Ok(())
}

/// 恢复旧配置。撤销和验证失败都走这里。
fn restore(spec: &ToolSpec, old_values: &[OldValue]) -> Result<(), MigrateError> {
    let mut failures: Vec<String> = Vec::new();

    for entry in spec.entries.iter().rev() {
        let name = config_name(entry);
        let Some(old) = old_values.iter().find(|value| value.name == name) else {
            continue;
        };

        let result = match entry.env_var {
            Some(var) => match &old.value {
                Some(text) => {
                    registry::write_env_string(var, text).map_err(|error| error.to_string())
                }
                None => registry::delete_env_string(var).map_err(|error| error.to_string()),
            },
            None => restore_npm_cache(spec, old.value.as_deref()),
        };

        if let Err(reason) = result {
            failures.push(format!("{name}：{reason}"));
        }
    }

    if spec.entries.iter().any(|entry| entry.env_var.is_some()) {
        let _ = registry::broadcast_environment_change();
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(MigrateError::RestoreFailed(failures.join("；")))
    }
}

/// 把 npm 的缓存配置写回旧值；原来没有配置就删掉这一项。
fn restore_npm_cache(spec: &ToolSpec, old: Option<&str>) -> Result<(), String> {
    let tool = resolve_tool(spec.program).ok_or_else(|| format!("没有找到 {}", spec.program))?;

    let outcome = match old {
        Some(text) => run(
            &tool,
            &["config", "set", "cache", text, "--location=user"],
            COMMAND_TIMEOUT,
        ),
        None => run(
            &tool,
            &["config", "delete", "cache", "--location=user"],
            COMMAND_TIMEOUT,
        ),
    };

    if outcome.timed_out || outcome.exit_code != Some(0) {
        return Err(output_text(&outcome));
    }

    Ok(())
}

/// 验证新位置已经生效：另起一个进程，环境块是当前环境加新变量。
///
/// 只看配置是否写对不够：工具自己的配置文件、环境变量优先级都可能盖掉它，
/// 所以必须以工具自己报告的生效路径为准。
fn verify(spec: &ToolSpec, base: &Path) -> Result<(), MigrateError> {
    let targets: Vec<PathBuf> = spec
        .entries
        .iter()
        .map(|entry| target_of(base, entry))
        .collect();

    let Some(args) = spec.validate_args else {
        // 没有可查询的命令：确认目标目录存在并且能打开就够了。
        for target in &targets {
            guard::open_anchor(target, &Forbidden::empty()).map_err(|error| {
                MigrateError::NotApplied(format!("{} 不可用：{error}", target.display()))
            })?;
        }
        return Ok(());
    };

    let tool = resolve_tool(spec.program)
        .ok_or_else(|| MigrateError::MissingTool(spec.program.to_string()))?;

    // 新环境块 = 当前环境 + 新变量。这里只覆盖需要的变量，PATH 之类的由子进程继承。
    let env: Vec<(&str, String)> = spec
        .entries
        .iter()
        .filter_map(|entry| {
            entry.env_var.map(|var| {
                let target = target_of(base, entry);
                (var, target.to_string_lossy().into_owned())
            })
        })
        .collect();

    let outcome = run_with_env(&tool, args, COMMAND_TIMEOUT, &env);

    if outcome.timed_out {
        return Err(MigrateError::NotApplied(format!(
            "验证命令超过 {} 秒没有返回",
            COMMAND_TIMEOUT.as_secs()
        )));
    }
    if outcome.exit_code != Some(0) {
        return Err(MigrateError::NotApplied(format!(
            "验证命令失败（退出码 {:?}）：{}",
            outcome.exit_code,
            output_text(&outcome)
        )));
    }

    let reported = parse_paths(&outcome.stdout_tail);
    if reported.is_empty() {
        return Err(MigrateError::NotApplied(
            "没有从验证命令的输出里读到路径".to_string(),
        ));
    }

    for target in &targets {
        if !reported.iter().any(|path| same_dir(path, target)) {
            let found: Vec<String> = reported
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            return Err(MigrateError::NotApplied(format!(
                "{} 没有生效，工具报告的路径是 {}",
                target.display(),
                found.join("、")
            )));
        }
    }

    Ok(())
}

/// 两个路径是否指向同一个目录。
///
/// 按目录身份比较而不是字符串：8.3 短名、大小写和链接都会让同一个目录有两个写法。
/// 有一边打不开时退回按路径比较，至少不会把明显相同的路径判成不同。
fn same_dir(left: &Path, right: &Path) -> bool {
    let forbidden = Forbidden::empty();

    match (
        guard::open_anchor(left, &forbidden),
        guard::open_anchor(right, &forbidden),
    ) {
        (Ok(left), Ok(right)) => left.identity() == right.identity(),
        _ => equals_ignore_case(&left.to_string_lossy(), &right.to_string_lossy()),
    }
}

/// 失败时的说明：恢复成功和恢复失败要分开讲，后者需要用户手工处理。
fn failure_note(action: &str, reason: &str, restored: &Result<(), MigrateError>) -> String {
    match restored {
        Ok(()) => format!("{action}，已恢复旧配置：{reason}"),
        Err(error) => format!("{action}，恢复旧配置也失败，请手工检查：{reason}；{error}"),
    }
}

/// 失败状态也要落盘和记日志，但保存失败不能再盖掉真正的错误。
fn record_failure(migration: &Migration, journal: &mut dyn JournalSink) {
    let _ = save(migration);
    let _ = write_log(journal, migration);
}

/// 把一次状态变化写进操作日志。
fn write_log(journal: &mut dyn JournalSink, migration: &Migration) -> Result<(), MigrateError> {
    journal
        .append(&Record::MigrationStage {
            at: unix_now(),
            tool: migration.tool.clone(),
            stage: migration.stage.label().to_string(),
            detail: migration.note.clone(),
        })
        .map_err(MigrateError::Log)
}

/// 迁移记录文件的全部内容。
///
/// 工具缓存迁移和目录迁移共用同一个文件：两者都是“先把状态记下来再动手”的迁移，
/// 分开存会让界面上的两个迁移页各自漏掉对方的进度。
#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Store {
    #[serde(default)]
    tools: Vec<Migration>,
    #[serde(default)]
    pub(crate) directories: Vec<junction::DirMigration>,
}

/// 迁移记录文件的位置。给了目录就用目录，否则用系统的 `%LOCALAPPDATA%\CClear`。
///
/// 测试只能操作临时目录，所以目录必须可注入。
pub(crate) fn store_file(dir: Option<&Path>) -> Result<PathBuf, MigrateError> {
    match dir {
        Some(dir) => Ok(dir.join(STORE_FILE)),
        None => known_folder(KnownFolder::LocalAppData)
            .map(|path| path.join(STORE_DIR).join(STORE_FILE))
            .map_err(|error| {
                MigrateError::Io(io::Error::other(format!(
                    "无法定位本地应用数据目录：{error}"
                )))
            }),
    }
}

/// 读迁移记录。还没有记录时返回空结构。
pub(crate) fn load_store(dir: Option<&Path>) -> Result<Store, MigrateError> {
    let path = store_file(dir)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Store::default()),
        Err(error) => return Err(MigrateError::Io(error)),
    };

    // 记录里存着旧配置值和未完成的迁移，坏了不能当成“没迁移过”：否则用户既撤不回去，
    // 也不知道发生了什么。
    serde_json::from_str(&text)
        .map_err(|error| MigrateError::Store(format!("{}：{error}", path.display())))
}

/// 写回全部迁移记录。
pub(crate) fn save_store(dir: Option<&Path>, store: &Store) -> Result<(), MigrateError> {
    let path = store_file(dir)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_vec_pretty(store)
        .map_err(|error| MigrateError::Io(io::Error::other(error)))?;
    std::fs::write(&path, json)?;

    Ok(())
}

/// 读全部工具的迁移记录。
fn load_all() -> Result<Vec<Migration>, MigrateError> {
    Ok(load_store(None)?.tools)
}

/// 读一个工具的迁移记录。
fn load_one(tool: &str) -> Result<Option<Migration>, MigrateError> {
    Ok(load_all()?.into_iter().find(|item| item.tool == tool))
}

/// 保存一个工具的迁移记录，其他工具和目录迁移的记录原样保留。
fn save(migration: &Migration) -> Result<(), MigrateError> {
    let mut store = load_store(None)?;
    store.tools.retain(|item| item.tool != migration.tool);
    store.tools.push(migration.clone());

    save_store(None, &store)
}

/// 系统目录映射。一次操作只解析一次，避免反复查 Known Folder。
fn root_map() -> Result<RootMap, MigrateError> {
    RootMap::system().map_err(|error| MigrateError::Roots(error.to_string()))
}
