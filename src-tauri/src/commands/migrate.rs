//! 工具缓存目录迁移命令：列表、检查、开始迁移、生成旧目录清理清单、撤销。
//!
//! 前端只传工具名、模式名和目标卷根。缓存在哪、配置怎么写、用什么命令验证，
//! 全部由核心库按工具表决定，前端拿不到可执行的命令行。
//!
//! 目录迁移（junction）同样只传扫描节点 ID：源路径从上次扫描结果里取，并经护栏校验。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use cclear_core::exec::executor;
use cclear_core::guard::{Forbidden, validate_external_path};
use cclear_core::journal::FileJournal;
use cclear_core::migrate::junction::{self, DirMigration, DirStage, JunctionMigration};
use cclear_core::migrate::tools::{self, ToolId};
use cclear_core::migrate::{self, EntryState, Migration, Mode, ToolState};
use cclear_core::rules::resolve::RootMap;
use cclear_core::win::process::running_processes;
use serde::Serialize;
use tauri::State;

use crate::commands::plan::{PlanSummaryDto, summary_dto};
use crate::state::AppState;

/// 一项配置的当前状态。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationEntryDto {
    /// 环境变量名，或者 `npm cache` 这样的配置项名。
    pub name: String,
    pub path: String,
    /// 配置来源：环境变量 / 当前进程环境变量 / npmrc / 默认值。
    pub source: String,
    pub exists: bool,
    pub size: u64,
    pub on_system_drive: bool,
}

/// 一个工具的状态。表格里的一行。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationToolDto {
    pub tool: String,
    pub label: String,
    pub entries: Vec<MigrationEntryDto>,
    pub total_size: u64,
    /// 还有配置指向系统盘。
    pub on_system_drive: bool,
    /// 阶段名，没迁移过时为 null。
    pub stage: Option<String>,
    pub stage_label: Option<String>,
    pub note: String,
    /// 新的缓存根目录。
    pub target: Option<String>,
    pub volume_root: Option<String>,
    pub mode: Option<String>,
    /// 迁移前用过的目录里现在还存在、可以清理的那些。
    pub old_dirs: Vec<String>,
    /// 相关进程里正在运行的那些，开始迁移前要提醒用户。
    pub processes: Vec<String>,
    /// 工具是否在 PATH 里。
    pub installed: bool,
    /// 改配置或验证是否需要工具本身。Gradle 不需要。
    pub needs_program: bool,
    /// 是否支持“允许重建”。Gradle 无法查询自己的缓存位置，只支持迁移。
    pub supports_rebuild: bool,
    /// 需要特别注意的提示，例如 Gradle 会整体搬走 `.gradle`。
    pub warning: Option<String>,
}

/// 可以作为目标卷的卷。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationVolumeDto {
    pub root: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
}

/// 迁移页的全部数据。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationsDto {
    pub tools: Vec<MigrationToolDto>,
    pub volumes: Vec<MigrationVolumeDto>,
    /// 迁移记录主文件不可用、退回备份时的提示。
    pub store_note: Option<String>,
    /// 记录是从备份读出来的，确认使用备份之前不许写入。
    pub store_needs_confirm: bool,
    /// 启动恢复的结果和错误。
    pub recovery_notes: Vec<String>,
}

/// 一次迁移或撤销之后的结果。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationResultDto {
    pub tool: String,
    pub label: String,
    pub stage: String,
    pub stage_label: String,
    pub note: String,
    pub target: String,
}

/// 清理旧目录的清单。`planId` 交给预览页，条目路径仍然只留在后端。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationCleanupDto {
    pub plan_id: String,
    pub summary: PlanSummaryDto,
}

/// 一条目录迁移记录。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirMigrationDto {
    pub id: String,
    pub source: String,
    pub target: String,
    pub backup: String,
    pub stage: String,
    pub stage_label: String,
    pub note: String,
    /// Unix 秒，界面按本地时区格式化。
    pub updated_at: i64,
    /// 目标目录还在。迁回之后它就是等着确认删除的旧副本。
    pub target_exists: bool,
    /// 源位置现在确实是一个 junction，才谈得上迁回。
    pub has_junction: bool,
    /// 源目录已经被改到备份名下，可以“继续”。
    pub resumable: bool,
    /// 可以确认删除备份：备份还在，而且链接已经建好（或已经迁回）。
    pub can_delete_backup: bool,
}

/// 目录迁移页的数据：记录和候选卷。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirMigrationsDto {
    pub records: Vec<DirMigrationDto>,
    pub volumes: Vec<MigrationVolumeDto>,
    /// 迁移记录主文件不可用、退回备份时的提示。
    pub store_note: Option<String>,
    /// 记录是从备份读出来的，确认使用备份之前不许写入。
    pub store_needs_confirm: bool,
    /// 启动恢复的结果和错误。
    pub recovery_notes: Vec<String>,
}

/// 一个目录的资格检查结果。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JunctionCheckDto {
    pub source_id: u32,
    pub path: String,
    pub allocated: u64,
    /// 拒绝原因的列表，空表示可以迁移。
    pub reasons: Vec<String>,
    pub eligible: bool,
}

#[tauri::command]
pub async fn migrations_list(state: State<'_, AppState>) -> Result<MigrationsDto, String> {
    // 恢复说明只是读一次内存里的列表，留在异步线程上没有阻塞风险。
    let recovery_notes = state.recovery_notes();

    tauri::async_runtime::spawn_blocking(move || -> Result<MigrationsDto, String> {
        let tools = migrate::list().map_err(|error| error.to_string())?;

        Ok(MigrationsDto {
            tools: tools.iter().map(tool_dto).collect(),
            volumes: migrate::candidate_volumes()
                .into_iter()
                .map(volume_dto)
                .collect(),
            // 读记录文件是同步 I/O，和 junction_state 一样放到阻塞线程里做。
            store_note: migrate::store_notice(),
            store_needs_confirm: migrate::store_needs_confirm(),
            recovery_notes,
        })
    })
    .await
    .map_err(|error| format!("读取迁移状态失败：{error}"))?
    .map_err(|error| format!("读取迁移状态失败：{error}"))
}

/// 单个工具的详情。开始迁移之前界面用它拿最新的进程警告。
#[tauri::command]
pub async fn migration_inspect(tool: String) -> Result<MigrationToolDto, String> {
    let id = parse_tool(&tool)?;

    tauri::async_runtime::spawn_blocking(move || migrate::inspect(id).map(|state| tool_dto(&state)))
        .await
        .map_err(|error| format!("检查缓存位置失败：{error}"))?
        .map_err(|error| format!("检查缓存位置失败：{error}"))
}

/// 开始迁移。配置写了但验证不通过时，核心库会自动恢复旧配置并报错。
#[tauri::command]
pub async fn migration_start(
    tool: String,
    volume_root: String,
    mode: String,
    state: State<'_, AppState>,
) -> Result<MigrationResultDto, String> {
    let id = parse_tool(&tool)?;
    let mode = Mode::parse(&mode).ok_or_else(|| "不支持的迁移方式".to_string())?;

    // 迁移会复制文件和改配置，和清理类操作共用同一把执行锁。
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        migrate::start(id, &volume_root, mode, &mut journal)
            .map(|migration| result_dto(&migration))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("迁移失败：{error}"))?
    .map_err(|error| format!("迁移失败：{error}"))
}

/// 生成清理旧缓存目录的清单。真正的删除由用户在预览页确认后走冻结清单执行。
#[tauri::command]
pub async fn migration_cleanup_old(
    tool: String,
    state: State<'_, AppState>,
) -> Result<MigrationCleanupDto, String> {
    let id = parse_tool(&tool)?;

    let plan = tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        migrate::cleanup_plan(id, &mut journal).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("生成清理清单失败：{error}"))?
    .map_err(|error| format!("生成清理清单失败：{error}"))?;

    let summary = summary_dto(&plan);
    let plan_id = plan.plan_id.to_string();
    state.store_plan(plan);

    Ok(MigrationCleanupDto { plan_id, summary })
}

/// 确认使用备份里的迁移记录。迁移记录主文件读不出来时界面会退回 `.bak`，
/// 在用户明确确认之前，后端拒绝把这份内容写回主文件。
#[tauri::command]
pub async fn migration_confirm_backup(state: State<'_, AppState>) -> Result<(), String> {
    // 确认会写迁移记录，和迁移操作共用同一把执行锁。
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        migrate::adopt_backup_store().map_err(|error| format!("确认使用备份失败：{error}"))
    })
    .await
    .map_err(|error| format!("确认使用备份失败：{error}"))?
}

/// 撤销迁移：恢复写配置前记下的旧值。
#[tauri::command]
pub async fn migration_revert(
    tool: String,
    state: State<'_, AppState>,
) -> Result<MigrationResultDto, String> {
    let id = parse_tool(&tool)?;
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        migrate::revert(id, &mut journal)
            .map(|migration| result_dto(&migration))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("撤销失败：{error}"))?
    .map_err(|error| format!("撤销失败：{error}"))
}

fn parse_tool(tool: &str) -> Result<ToolId, String> {
    ToolId::parse(tool).ok_or_else(|| "没有这个工具".to_string())
}

// ---- 目录迁移（junction） ----

/// 目录迁移的状态和候选卷。
#[tauri::command]
pub async fn junction_state(state: State<'_, AppState>) -> Result<DirMigrationsDto, String> {
    let recovery_notes = state.recovery_notes();

    tauri::async_runtime::spawn_blocking(move || -> Result<DirMigrationsDto, String> {
        let migration = open_junction()?;
        let records = migration
            .list()
            .map_err(|error| format!("读取目录迁移状态失败：{error}"))?;

        Ok(DirMigrationsDto {
            records: records
                .iter()
                .map(|record| dir_record_dto(&migration, record))
                .collect(),
            volumes: migrate::candidate_volumes()
                .into_iter()
                .map(volume_dto)
                .collect(),
            store_note: migration.store_notice(),
            store_needs_confirm: migrate::store_needs_confirm(),
            recovery_notes,
        })
    })
    .await
    .map_err(|error| format!("读取目录迁移状态失败：{error}"))?
}

/// 对一个扫描节点做资格检查。前端只传节点 ID，路径从上次扫描结果里取。
#[tauri::command]
pub async fn junction_check(
    source_id: u32,
    state: State<'_, AppState>,
) -> Result<JunctionCheckDto, String> {
    let (path, allocated) = scan_path(&state, source_id)?;

    tauri::async_runtime::spawn_blocking(move || -> Result<JunctionCheckDto, String> {
        let migration = open_junction()?;
        let reasons = migration.check_eligibility(&path);

        Ok(JunctionCheckDto {
            source_id,
            path: path.to_string_lossy().into_owned(),
            allocated,
            eligible: reasons.is_empty(),
            reasons,
        })
    })
    .await
    .map_err(|error| format!("资格检查失败：{error}"))?
}

/// 开始把一个目录迁移到其他盘。迁移会复制并改名，和清理类操作共用执行锁。
#[tauri::command]
pub async fn junction_start(
    source_id: u32,
    volume_root: String,
    state: State<'_, AppState>,
) -> Result<DirMigrationDto, String> {
    let (path, _) = scan_path(&state, source_id)?;
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let migration = open_junction()?;
        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        migration
            .start(&path, &volume_root, &mut journal)
            .map(|record| dir_record_dto(&migration, &record))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("迁移失败：{error}"))?
}

/// 继续一次被打断在“源已改名、链接未建”的迁移。
#[tauri::command]
pub async fn junction_continue(
    id: String,
    state: State<'_, AppState>,
) -> Result<DirMigrationDto, String> {
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let migration = open_junction()?;
        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        migration
            .resume(&id, &mut journal)
            .map(|record| dir_record_dto(&migration, &record))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("继续迁移失败：{error}"))?
}

/// 迁回：把目标里的最新数据搬回原来的位置。
#[tauri::command]
pub async fn junction_return(
    id: String,
    state: State<'_, AppState>,
) -> Result<DirMigrationDto, String> {
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let migration = open_junction()?;
        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        migration
            .return_home(&id, &mut journal)
            .map(|record| dir_record_dto(&migration, &record))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("迁回失败：{error}"))?
}

/// 确认删除备份：按冻结清单清理备份目录，再删掉留下的空目录。
#[tauri::command]
pub async fn junction_delete_backup(
    id: String,
    state: State<'_, AppState>,
) -> Result<DirMigrationDto, String> {
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || -> Result<DirMigrationDto, String> {
        let migration = open_junction()?;
        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        let plan = migration
            .backup_plan(&id)
            .map_err(|error| format!("生成备份清理清单失败：{error}"))?;

        // 用户已经按下“确认删除备份”，这里不再走预览，直接执行这份清单。
        // 禁区按真实规则解析，进程列表也在执行前再查一次：备份目录可能还被程序占着。
        let roots = RootMap::system().map_err(|error| format!("无法解析系统目录：{error}"))?;
        let forbidden = Forbidden::from_rules(&roots);
        let cancel = AtomicBool::new(false);
        let report = executor::execute(
            &plan,
            &[junction::BACKUP_RULE_ID],
            &forbidden,
            &running_processes,
            &mut journal,
            &cancel,
            &|_| {},
        );
        if let Some(reason) = report.aborted {
            return Err(format!("清理备份被中止：{reason}"));
        }

        let record = migration
            .finish_backup(&id, &mut journal)
            .map_err(|error| format!("清理备份失败：{error}"))?;

        Ok(dir_record_dto(&migration, &record))
    })
    .await
    .map_err(|error| format!("清理备份失败：{error}"))?
}

/// 迁回之后删除目标里的旧副本。必须由用户确认之后调用。
#[tauri::command]
pub async fn junction_discard_target(
    id: String,
    state: State<'_, AppState>,
) -> Result<DirMigrationDto, String> {
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let migration = open_junction()?;
        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        migration
            .discard_target(&id, &mut journal)
            .map(|record| dir_record_dto(&migration, &record))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("删除目标副本失败：{error}"))?
}

/// 启动时把上次没有做完的目录迁移和工具迁移收到确定的状态上。
///
/// 拿到执行锁才会动手：恢复要改名和删目录，不能和用户刚发起的操作抢同一个目录。
/// 恢复结果和错误都记到状态里，迁移页会显示，不会只留在后台线程里。
pub fn recover_on_startup(state: &AppState) {
    let Ok(_exec) = state.start_exec() else {
        state.push_recovery_note(
            "启动恢复没有执行：已经有一次清理在进行，请重新打开迁移页确认状态".to_string(),
        );
        return;
    };

    let Ok(mut journal) = FileJournal::new() else {
        state.push_recovery_note("启动恢复没有执行：操作日志无法写入".to_string());
        return;
    };

    recover_directories(state, &mut journal);
    recover_tools(state, &mut journal);
}

/// 目录迁移的启动恢复。
fn recover_directories(state: &AppState, journal: &mut FileJournal) {
    let migration = match JunctionMigration::open() {
        Ok(migration) => migration,
        Err(error) => {
            state.push_recovery_note(format!("目录迁移记录读不出来，启动恢复没有执行：{error}"));
            return;
        }
    };

    match migration.recover_pending(journal) {
        Ok(report) => {
            if !report.recovered.is_empty() {
                state.push_recovery_note(format!(
                    "已恢复 {} 条中断的目录迁移，请在目录迁移里确认每条记录的状态",
                    report.recovered.len()
                ));
            }
            // 连状态都写不进去的记录也要让人看见，不能悄悄跳过。
            for failure in report.failures {
                state.push_recovery_note(format!("目录迁移启动恢复失败：{failure}"));
            }
        }
        Err(error) => state.push_recovery_note(format!("目录迁移启动恢复失败：{error}")),
    }
}

/// 工具缓存迁移的启动恢复：验证之前被中断的记录恢复旧配置。
fn recover_tools(state: &AppState, journal: &mut FileJournal) {
    match migrate::recover_tools(journal) {
        Ok(records) => {
            for record in records {
                state.push_recovery_note(format!("{}：{}", record.tool, record.note));
            }
        }
        Err(error) => state.push_recovery_note(format!("工具缓存迁移启动恢复失败：{error}")),
    }
}

fn open_junction() -> Result<JunctionMigration, String> {
    JunctionMigration::open().map_err(|error| error.to_string())
}

/// 扫描节点对应的路径和占盘大小。只接受节点 ID，路径一律从最近一次扫描结果里取。
fn scan_path(state: &AppState, source_id: u32) -> Result<(PathBuf, u64), String> {
    let result = state
        .result()
        .ok_or_else(|| "还没有扫描结果，请先到总览页扫描".to_string())?;
    let Some(node) = result.nodes.get(source_id as usize) else {
        return Err("目录不存在".to_string());
    };

    let path = validate_external_path(&result.path_of(source_id).to_string_lossy())
        .map_err(|error| error.to_string())?;

    Ok((path, node.size.allocated))
}

fn dir_record_dto(migration: &JunctionMigration, record: &DirMigration) -> DirMigrationDto {
    let backup_exists = record.backup_path().exists();
    let target_exists = record.target_path().exists();
    let source_exists = record.source_path().exists();
    let temp_exists = record.return_temp_path().exists();
    // 链接是否存在要回到磁盘上确认：NeedsAttention 也可能停在建链接失败的那一刻。
    let has_junction = record.stage.has_junction() && migration.source_is_junction(record);
    // “继续”有两种现场：链接已经建好（重新校验即可），或者源已改名、链接还没建。
    // 迁回中途留下的临时目录还在时不能当成后一种：那是迁回失败后的现场，需要人工判断。
    let resumable = record.stage == DirStage::NeedsAttention
        && target_exists
        && (has_junction || (backup_exists && !source_exists && !temp_exists));

    DirMigrationDto {
        id: record.id.clone(),
        source: record.source.clone(),
        target: record.target.clone(),
        backup: record.backup.clone(),
        stage: record.stage.key().to_string(),
        stage_label: record.stage.label().to_string(),
        note: record.note.clone(),
        updated_at: record.updated_at,
        target_exists,
        has_junction,
        resumable,
        // 已经迁回时源位置是普通目录，但备份仍然可以删；
        // 其余情况必须有链接，而且这条记录要经过经链接校验，才允许删备份。
        can_delete_backup: backup_exists
            && (record.stage == DirStage::Returned || (has_junction && record.verified)),
    }
}

fn tool_dto(state: &ToolState) -> MigrationToolDto {
    MigrationToolDto {
        tool: state.spec.id.key().to_string(),
        label: state.spec.label.to_string(),
        entries: state.entries.iter().map(entry_dto).collect(),
        total_size: state.total_size,
        on_system_drive: state.on_system_drive,
        stage: state.stage().map(|stage| stage.key().to_string()),
        stage_label: state.stage().map(|stage| stage.label().to_string()),
        note: state
            .migration
            .as_ref()
            .map(|migration| migration.note.clone())
            .unwrap_or_default(),
        target: state
            .migration
            .as_ref()
            .map(|migration| migration.target.clone()),
        volume_root: state
            .migration
            .as_ref()
            .map(|migration| migration.volume_root.clone()),
        mode: state
            .migration
            .as_ref()
            .map(|migration| migration.mode.key().to_string()),
        old_dirs: state
            .old_dirs
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        processes: state.running.iter().map(|name| name.to_string()).collect(),
        installed: state.installed,
        needs_program: migrate::needs_program(state.spec),
        supports_rebuild: state.spec.supports_rebuild,
        warning: state.spec.warning.map(str::to_string),
    }
}

fn entry_dto(state: &EntryState) -> MigrationEntryDto {
    MigrationEntryDto {
        name: state.name.clone(),
        path: state.path.to_string_lossy().into_owned(),
        source: state.source.clone(),
        exists: state.exists,
        size: state.size,
        on_system_drive: state.on_system_drive,
    }
}

fn volume_dto(info: cclear_core::win::volume::VolumeInfo) -> MigrationVolumeDto {
    MigrationVolumeDto {
        root: info.root.to_string_lossy().into_owned(),
        label: info.label,
        total: info.total,
        free: info.free,
    }
}

fn result_dto(migration: &Migration) -> MigrationResultDto {
    let label = ToolId::parse(&migration.tool)
        .and_then(tools::spec)
        .map(|spec| spec.label.to_string())
        .unwrap_or_else(|| migration.tool.clone());

    MigrationResultDto {
        tool: migration.tool.clone(),
        label,
        stage: migration.stage.key().to_string(),
        stage_label: migration.stage.label().to_string(),
        note: migration.note.clone(),
        target: migration.target.clone(),
    }
}
