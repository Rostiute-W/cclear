//! 工具缓存目录迁移命令：列表、检查、开始迁移、生成旧目录清理清单、撤销。
//!
//! 前端只传工具名、模式名和目标卷根。缓存在哪、配置怎么写、用什么命令验证，
//! 全部由核心库按工具表决定，前端拿不到可执行的命令行。

use cclear_core::journal::FileJournal;
use cclear_core::migrate::tools::{self, ToolId};
use cclear_core::migrate::{self, EntryState, Migration, Mode, ToolState};
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

#[tauri::command]
pub async fn migrations_list() -> Result<MigrationsDto, String> {
    tauri::async_runtime::spawn_blocking(|| -> Result<MigrationsDto, String> {
        let tools = migrate::list().map_err(|error| error.to_string())?;

        Ok(MigrationsDto {
            tools: tools.iter().map(tool_dto).collect(),
            volumes: migrate::candidate_volumes()
                .into_iter()
                .map(volume_dto)
                .collect(),
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
