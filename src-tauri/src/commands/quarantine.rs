//! 隔离区命令：选卷、批次列表、还原、彻底删除与异常处理。
//!
//! 执行类命令只接受批次 ID 和动作名，条目路径一律从批次清单里取，前端拿不到自由路径。

use std::path::PathBuf;

use cclear_core::elevation::{SystemOp, run_elevated};
use cclear_core::guard::Forbidden;
use cclear_core::journal::FileJournal;
use cclear_core::quarantine::{
    BatchSummary, OVERDUE_DAYS, QuarantineRoot, ResolveAction, candidate_volumes, default_root,
    save_quarantine_root,
};
use cclear_core::rules::catalog::find;
use cclear_core::rules::resolve::RootMap;
use cclear_core::win::volume;
use cclear_core::win::{KnownFolder, equals_ignore_case, known_folder};
use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::state::AppState;

/// 可以作为隔离区的卷。`free` 是卷的剩余空间，界面按它排序展示。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineVolumeDto {
    pub root: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
}

/// 隔离区页的设置卡：可选卷、当前选择，以及当前隔离根的剩余空间。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineVolumesDto {
    pub candidates: Vec<QuarantineVolumeDto>,
    /// 已经设置好的隔离根目录，null 表示还没选过。
    pub current_root: Option<String>,
    pub current_free: u64,
}

/// 批次列表里的一行。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineBatchDto {
    pub batch_id: String,
    /// Unix 秒，界面按本地时区格式化。
    pub created_at: i64,
    pub rule_id: String,
    pub rule_name: String,
    /// 机器可读的状态名，界面按它决定显示哪些按钮。
    pub stage: String,
    pub stage_label: String,
    pub note: String,
    pub items: u64,
    pub size: u64,
    /// 已经存放了多少天。
    pub age_days: u64,
    /// 批次来自提权 helper，还原和彻底删除要重新提权。
    pub requires_admin: bool,
}

/// 还原结果。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineRestoreDto {
    pub batch_id: String,
    pub stage: String,
    pub stage_label: String,
    pub restored: u64,
    /// 原位已有同名文件、因此没有还原的路径。
    pub conflicts: Vec<String>,
}

/// 异常处理结果。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineResolveDto {
    pub batch_id: String,
    pub stage: String,
    pub stage_label: String,
    /// 源已删除、只留在隔离区的条目数。
    pub moved: u64,
    /// 源发生变化、源和副本都保留的条目数。
    pub kept: u64,
}

#[tauri::command]
pub async fn quarantine_volumes() -> Result<QuarantineVolumesDto, String> {
    tauri::async_runtime::spawn_blocking(current_state)
        .await
        .map_err(|error| format!("读取隔离区设置失败：{error}"))?
}

/// 保存隔离区所在的卷。只接受 `quarantine_volumes` 返回的候选卷。
#[tauri::command]
pub async fn quarantine_set_volume(
    volume_root: String,
    state: State<'_, AppState>,
) -> Result<QuarantineVolumesDto, String> {
    // 共享执行锁在异步函数体里拿：守卫借用 state，不能跟着闭包走进工作线程。
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let source = system_drive()?;
        let candidates = candidate_volumes(&source);
        let Some(chosen) = candidates
            .iter()
            .find(|info| equals_ignore_case(&info.root.to_string_lossy(), &volume_root))
        else {
            return Err("这个卷不能作为隔离区，请重新选择".to_string());
        };

        let root = chosen
            .root
            .join(cclear_core::quarantine::QUARANTINE_DIR_NAME);

        // 目录现在就建起来：DACL 设置失败要立刻让用户看到，而不是等到第一次隔离。
        // 建成功之后才保存设置，否则用户会留下一个指向上次失败的隔离根。
        QuarantineRoot::open(root.clone())
            .ensure_root()
            .map_err(|error| format!("创建隔离区目录失败：{error}"))?;
        save_quarantine_root(&root).map_err(|error| format!("保存隔离区设置失败：{error}"))?;

        current_state()
    })
    .await
    .map_err(|error| format!("设置隔离区失败：{error}"))?
}

#[tauri::command]
pub async fn quarantine_batches() -> Result<Vec<QuarantineBatchDto>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let batches = all_batches()?;
        Ok(batches.into_iter().map(batch_dto).collect())
    })
    .await
    .map_err(|error| format!("读取隔离批次失败：{error}"))?
}

/// 存放超过 7 天的批次。界面启动时用它决定是否显示横幅提示。
#[tauri::command]
pub async fn quarantine_overdue() -> Result<Vec<QuarantineBatchDto>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let overdue = all_batches()?
            .into_iter()
            .filter(|batch| batch.age_days > OVERDUE_DAYS)
            .collect::<Vec<BatchSummary>>();

        Ok(overdue.into_iter().map(batch_dto).collect())
    })
    .await
    .map_err(|error| format!("读取隔离批次失败：{error}"))?
}

#[tauri::command]
pub async fn quarantine_restore(
    batch_id: String,
    state: State<'_, AppState>,
) -> Result<QuarantineRestoreDto, String> {
    let id = parse_batch_id(&batch_id)?;
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let root = root_for_batch(id)?;

        // 提权 helper 创建的批次，源在 ProgramData 下，还原也必须回到 helper 里执行。
        if batch_requires_admin(&root, id)? {
            return restore_with_helper(&root, id);
        }

        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        let outcome = root
            .restore(id, &mut journal)
            .map_err(|error| format!("还原失败：{error}"))?;

        Ok(QuarantineRestoreDto {
            batch_id: id.to_string(),
            stage: outcome.stage.key().to_string(),
            stage_label: outcome.stage.label().to_string(),
            restored: outcome.restored,
            conflicts: outcome
                .conflicts
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        })
    })
    .await
    .map_err(|error| format!("还原失败：{error}"))?
}

#[tauri::command]
pub async fn quarantine_purge(batch_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let id = parse_batch_id(&batch_id)?;
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let root = root_for_batch(id)?;

        // 提权批次连批次目录本身都可能有 ACL 限制，彻底删除同样交给 helper。
        if batch_requires_admin(&root, id)? {
            return purge_with_helper(&root, id);
        }

        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        root.purge(id, &mut journal)
            .map_err(|error| format!("彻底删除失败：{error}"))
    })
    .await
    .map_err(|error| format!("彻底删除失败：{error}"))?
}

/// 处理异常批次：`continue` 逐项复核后继续隔离，`abandon` 放弃副本、保留源。
#[tauri::command]
pub async fn quarantine_resolve(
    batch_id: String,
    action: String,
    state: State<'_, AppState>,
) -> Result<QuarantineResolveDto, String> {
    let id = parse_batch_id(&batch_id)?;
    let action = parse_action(&action)?;
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        let root = root_for_batch(id)?;
        let mut journal =
            FileJournal::new().map_err(|error| format!("操作日志无法写入：{error}"))?;

        let outcome = root
            .resolve(id, action, &forbidden()?, &mut journal)
            .map_err(|error| format!("处理异常批次失败：{error}"))?;

        Ok(QuarantineResolveDto {
            batch_id: id.to_string(),
            stage: outcome.stage.key().to_string(),
            stage_label: outcome.stage.label().to_string(),
            moved: outcome.moved,
            kept: outcome.kept,
        })
    })
    .await
    .map_err(|error| format!("处理异常批次失败：{error}"))?
}

/// 启动时把上次没有做完的隔离批次收到确定的状态上。
///
/// 换过卷之后旧隔离根上的批次也要恢复，所以扫设置里记录过的每一个根。
/// 拿到执行锁才会动手，避免和用户刚发起的操作抢同一批文件。
pub fn recover_on_startup(state: &AppState) {
    let Ok(_exec) = state.start_exec() else {
        return;
    };
    let Ok(mut journal) = FileJournal::new() else {
        return;
    };

    // 恢复失败只影响这一次启动的提示，不影响界面其他功能。
    for root in QuarantineRoot::roots_from_settings() {
        let _ = root.recover_pending(&mut journal);
    }
}

/// 设置里记录过的全部隔离根上的批次，按创建时间从新到旧。
fn all_batches() -> Result<Vec<BatchSummary>, String> {
    let mut batches = Vec::new();

    for root in QuarantineRoot::roots_from_settings() {
        let read = root
            .batches()
            .map_err(|error| format!("读取隔离批次失败：{error}"))?;
        batches.extend(read);
    }

    batches.sort_by_key(|batch| std::cmp::Reverse(batch.created_at));
    Ok(batches)
}

/// 当前系统盘根目录，隔离区的源卷就是它。
fn system_drive() -> Result<PathBuf, String> {
    let windows =
        known_folder(KnownFolder::Windows).map_err(|error| format!("无法定位系统目录：{error}"))?;

    windows
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "无法定位系统盘".to_string())
}

/// 候选卷和当前选择。
fn current_state() -> Result<QuarantineVolumesDto, String> {
    let source = system_drive()?;
    let candidates = candidate_volumes(&source);
    // 当前选择走和隔离同样的默认逻辑，界面第一次打开就能看到默认隔离区。
    let current_root = configured_root().map(|root| root.path().to_path_buf());

    let current_free = current_root
        .as_ref()
        .map(|path| volume::free_space(path))
        .unwrap_or(0);

    Ok(QuarantineVolumesDto {
        candidates: candidates
            .into_iter()
            .map(|info| QuarantineVolumeDto {
                root: info.root.to_string_lossy().into_owned(),
                label: info.label,
                total: info.total,
                free: info.free,
            })
            .collect(),
        current_root: current_root.map(|path| path.to_string_lossy().into_owned()),
        current_free,
    })
}

/// 已配置的隔离根。
///
/// 还没选过卷时按“剩余空间最大的候选卷”保存一次默认位置：用户第一次打开隔离区页
/// 就已经有可用的隔离区，而不是每类规则都因为没设置而被跳过。
fn configured_root() -> Option<QuarantineRoot> {
    if let Some(root) = QuarantineRoot::from_settings() {
        return Some(root);
    }

    let candidates = candidate_volumes(&system_drive().ok()?);
    let root = default_root(&candidates)?;
    save_quarantine_root(&root).ok()?;

    Some(QuarantineRoot::open(root))
}

/// 执行类命令必须有隔离根，没有就明确告诉用户去选卷。
///
/// 提权清理命令也复用它：第一次直接点管理员项时同样会建好默认隔离区。
pub(crate) fn require_root() -> Result<QuarantineRoot, String> {
    configured_root().ok_or_else(|| "还没有选择隔离区位置，请先在隔离区页选择保存卷".to_string())
}

/// 批次所在的隔离根。批次可能属于以前用过的卷，按设置里记录过的根逐个找。
fn root_for_batch(batch_id: Uuid) -> Result<QuarantineRoot, String> {
    QuarantineRoot::find_root_of(batch_id)
        .ok_or_else(|| "这个隔离批次不存在，请刷新列表".to_string())
}

/// 这个批次是不是提权 helper 创建的。列表里没有它时返回 false，
/// 后面的本地流程会给出“批次不存在”的准确错误。
fn batch_requires_admin(root: &QuarantineRoot, batch_id: Uuid) -> Result<bool, String> {
    let batches = root
        .batches()
        .map_err(|error| format!("读取隔离批次失败：{error}"))?;

    Ok(batches
        .into_iter()
        .find(|batch| batch.batch_id == batch_id)
        .is_some_and(|batch| batch.requires_admin))
}

/// 通过提权 helper 还原批次。
///
/// 用户取消 UAC 时 helper 返回“已取消授权”，这里原样传给界面。
fn restore_with_helper(
    root: &QuarantineRoot,
    batch_id: Uuid,
) -> Result<QuarantineRestoreDto, String> {
    let result = run_elevated(&[SystemOp::RestoreBatch(batch_id)], root.path())
        .map_err(|error| error.to_string())?;

    let Some(outcome) = result.first() else {
        return Err("提权 helper 没有返回结果".to_string());
    };
    // helper 自己失败（例如隔离根不可用）时不能当成“还原了 0 个文件”。
    if outcome.status == "failed" {
        return Err(outcome.detail.clone());
    }

    // helper 只回一个总体状态：全部还原成功才算已还原，其余按“需要处理”交给用户再看一次。
    let stage = if outcome.is_done() {
        ("restored", "已还原")
    } else {
        ("needsAttention", "需要处理")
    };

    Ok(QuarantineRestoreDto {
        batch_id: batch_id.to_string(),
        stage: stage.0.to_string(),
        stage_label: stage.1.to_string(),
        restored: outcome.items,
        conflicts: outcome.conflicts.clone(),
    })
}

/// 通过提权 helper 彻底删除批次。
fn purge_with_helper(root: &QuarantineRoot, batch_id: Uuid) -> Result<(), String> {
    let result = run_elevated(&[SystemOp::PurgeBatch(batch_id)], root.path())
        .map_err(|error| error.to_string())?;

    let Some(outcome) = result.first() else {
        return Err("提权 helper 没有返回结果".to_string());
    };
    if !outcome.is_done() {
        return Err(outcome.detail.clone());
    }

    Ok(())
}

/// 隔离区在用户目录里，禁区规则不该挡住还原和异常处理。
///
/// 但“继续”会回到源目录去删源，源目录在用户目录里，禁区规则必须照常生效，
/// 所以这里按规则里的禁区根目录建集合，而不是传空集合。
fn forbidden() -> Result<Forbidden, String> {
    let roots = RootMap::system().map_err(|error| error.to_string())?;
    Ok(Forbidden::from_rules(&roots))
}

fn parse_batch_id(batch_id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(batch_id).map_err(|_| "这个隔离批次不存在，请刷新列表".to_string())
}

fn parse_action(action: &str) -> Result<ResolveAction, String> {
    match action {
        "continue" => Ok(ResolveAction::Continue),
        "abandon" => Ok(ResolveAction::Abandon),
        _ => Err("不支持的异常处理方式".to_string()),
    }
}

fn batch_dto(batch: cclear_core::quarantine::BatchSummary) -> QuarantineBatchDto {
    // 规则名取自内置规则目录；规则被删掉时退回显示 ID。
    let rule_name = find(&batch.rule_id)
        .map(|rule| rule.name.to_string())
        .unwrap_or_else(|| batch.rule_id.clone());

    QuarantineBatchDto {
        batch_id: batch.batch_id.to_string(),
        created_at: batch.created_at,
        rule_id: batch.rule_id,
        rule_name,
        stage: batch.stage.key().to_string(),
        stage_label: batch.stage.label().to_string(),
        note: batch.note,
        items: batch.items,
        size: batch.size,
        age_days: batch.age_days,
        requires_admin: batch.requires_admin,
    }
}
