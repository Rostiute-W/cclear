use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State, ipc::Channel};

use cclear_core::journal::{self, CleanHistoryDetail, CleanHistoryList};
use cclear_core::orchestrator::deep_slim::{
    self, DeepSlimAction, DeepSlimProgress, DeepSlimResult, DeepSlimState,
};
use cclear_core::orchestrator::space::{
    self, DirNode, FileEntry, RecycleActionResult, ScanProgress, ScanRoot, ScanSummary,
    SpaceTargetKind, VolumeInfo,
};
use cclear_core::orchestrator::{
    self, CleanExecuteRequest, CleanProgress, CleanReport, JunkScanProgress, JunkScanResult,
    ProcessCloseResult, RunningProcessConflict,
};
use cclear_core::rules::{
    RootMap,
    schema::{self, RulePackInfo},
};
use cclear_core::settings::{self, AppSettings, AppSettingsPatch};

use crate::state::{AppState, lock};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: &'static str,
    is_admin: bool,
    rule_pack_version: String,
    rule_count: usize,
}

async fn work<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| format!("后台操作失败：{error}"))?
}

fn roots() -> Result<RootMap, String> {
    RootMap::system().map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn app_info() -> Result<AppInfo, String> {
    work(|| {
        let pack = schema::builtin();
        Ok(AppInfo {
            version: env!("CARGO_PKG_VERSION"),
            is_admin: cclear_core::win::is_elevated(),
            rule_pack_version: pack.version.clone(),
            rule_count: pack.rules.len(),
        })
    })
    .await
}

#[tauri::command]
pub async fn volume_info() -> Result<Vec<VolumeInfo>, String> {
    work(|| Ok(space::volumes())).await
}

#[tauri::command]
pub async fn junk_scan_start(
    app: AppHandle,
    on_progress: Channel<JunkScanProgress>,
) -> Result<JunkScanResult, String> {
    work(move || {
        let state = app.state::<AppState>();
        let _operation = state.operation.try_lock().map_err(|_| "已有操作正在运行")?;
        let active = state.junk_cancel.start()?;
        *lock(&state.junk)? = None;
        lock(&state.conflicts)?.clear();
        let winsxs = lock(&state.winsxs)?.clone();
        let (result, scan) =
            orchestrator::junk_scan(&roots()?, &winsxs, &active.flag, |progress| {
                let _ = on_progress.send(progress);
            })?;
        if !result.cancelled {
            *lock(&state.junk)? = Some(scan);
        }
        Ok(result)
    })
    .await
}

#[tauri::command]
pub async fn junk_scan_cancel(state: State<'_, AppState>) -> Result<(), String> {
    state.junk_cancel.cancel()
}

#[tauri::command]
pub async fn clean_check_processes(
    app: AppHandle,
    scan_id: String,
    item_ids: Vec<String>,
) -> Result<Vec<RunningProcessConflict>, String> {
    work(move || {
        let state = app.state::<AppState>();
        let _operation = state.operation.try_lock().map_err(|_| "已有操作正在运行")?;
        lock(&state.conflicts)?.clear();
        let scan = lock(&state.junk)?;
        let result = scan
            .as_ref()
            .ok_or("请先扫描可清理项目")?
            .check_processes(&scan_id, &item_ids)?;
        *lock(&state.conflicts)? = result
            .iter()
            .flat_map(|conflict| conflict.process_names.clone())
            .collect();
        Ok(result)
    })
    .await
}

#[tauri::command]
pub async fn clean_close_processes(
    app: AppHandle,
    process_names: Vec<String>,
) -> Result<ProcessCloseResult, String> {
    work(move || {
        let state = app.state::<AppState>();
        let _operation = state.operation.try_lock().map_err(|_| "已有操作正在运行")?;
        let allowed = std::mem::take(&mut *lock(&state.conflicts)?);
        orchestrator::close_processes(&allowed, &process_names)
    })
    .await
}

#[tauri::command]
pub async fn clean_execute(
    app: AppHandle,
    request: CleanExecuteRequest,
    on_progress: Channel<CleanProgress>,
) -> Result<CleanReport, String> {
    work(move || {
        let state = app.state::<AppState>();
        let _operation = state.operation.try_lock().map_err(|_| "已有操作正在运行")?;
        let active = state.clean_cancel.start()?;
        lock(&state.conflicts)?.clear();
        let roots = roots()?;
        let scan = lock(&state.junk)?.take().ok_or("请先扫描可清理项目")?;
        orchestrator::clean_execute(&roots, &scan, request, &active.flag, |progress| {
            let _ = on_progress.send(progress);
        })
    })
    .await
}

#[tauri::command]
pub async fn clean_cancel(state: State<'_, AppState>) -> Result<(), String> {
    state.clean_cancel.cancel()
}

#[tauri::command]
pub async fn open_clean_item_location(
    app: AppHandle,
    scan_id: String,
    item_id: String,
    sample_index: Option<usize>,
) -> Result<(), String> {
    work(move || {
        let state = app.state::<AppState>();
        lock(&state.junk)?
            .as_ref()
            .ok_or("请先扫描可清理项目")?
            .open_location(&scan_id, &item_id, sample_index)
    })
    .await
}

#[tauri::command]
pub async fn deep_slim_status(app: AppHandle) -> Result<DeepSlimState, String> {
    work(move || {
        let state = app.state::<AppState>();
        let winsxs = lock(&state.winsxs)?.clone();
        Ok(deep_slim::status(&roots()?, &winsxs))
    })
    .await
}

#[tauri::command]
pub async fn deep_slim_execute(
    app: AppHandle,
    action: DeepSlimAction,
    on_progress: Channel<DeepSlimProgress>,
) -> Result<DeepSlimResult, String> {
    work(move || {
        let state = app.state::<AppState>();
        let _operation = state.operation.try_lock().map_err(|_| "已有操作正在运行")?;
        let mut winsxs = lock(&state.winsxs)?.clone();
        let result = deep_slim::execute(&roots()?, action, &mut winsxs, |progress| {
            let _ = on_progress.send(progress);
        });
        *lock(&state.winsxs)? = winsxs;
        Ok(result)
    })
    .await
}

#[tauri::command]
pub async fn default_scan_roots() -> Result<Vec<ScanRoot>, String> {
    work(|| Ok(space::default_roots(&roots()?))).await
}

#[tauri::command]
pub async fn space_scan_start(
    app: AppHandle,
    root_path: String,
    use_mft: bool,
    on_progress: Channel<ScanProgress>,
) -> Result<ScanSummary, String> {
    work(move || {
        let state = app.state::<AppState>();
        let _operation = state.operation.try_lock().map_err(|_| "已有操作正在运行")?;
        let active = state.space_cancel.start()?;
        *lock(&state.space)? = None;
        let scan = space::scan(&roots()?, &root_path, use_mft, &active.flag, |progress| {
            let _ = on_progress.send(progress);
        })?;
        let summary = scan.summary.clone();
        *lock(&state.space)? = Some(Arc::new(scan));
        Ok(summary)
    })
    .await
}

#[tauri::command]
pub async fn space_scan_cancel(state: State<'_, AppState>) -> Result<(), String> {
    state.space_cancel.cancel()
}

#[tauri::command]
pub async fn space_scan_summary(app: AppHandle) -> Result<Option<ScanSummary>, String> {
    work(move || {
        let state = app.state::<AppState>();
        let scan = lock(&state.space)?.clone();
        Ok(scan.map(|scan| scan.summary.clone()))
    })
    .await
}

#[tauri::command]
pub async fn space_scan_children(
    app: AppHandle,
    node_id: Option<u32>,
) -> Result<Vec<DirNode>, String> {
    work(move || {
        let state = app.state::<AppState>();
        let scan = lock(&state.space)?.clone();
        match scan {
            Some(scan) => scan.children(node_id),
            None => Ok(Vec::new()),
        }
    })
    .await
}

#[tauri::command]
pub async fn space_scan_top_files(app: AppHandle, limit: usize) -> Result<Vec<FileEntry>, String> {
    work(move || {
        let state = app.state::<AppState>();
        let scan = lock(&state.space)?.clone();
        Ok(scan.map_or_else(Vec::new, |scan| scan.top_files(limit)))
    })
    .await
}

#[tauri::command]
pub async fn space_open_in_explorer(
    app: AppHandle,
    target_kind: SpaceTargetKind,
    id: u32,
) -> Result<(), String> {
    work(move || {
        let state = app.state::<AppState>();
        let scan = lock(&state.space)?.clone().ok_or("请先完成空间扫描")?;
        scan.open(target_kind, id)
    })
    .await
}

#[tauri::command]
pub async fn space_recycle_item(
    app: AppHandle,
    target_kind: SpaceTargetKind,
    id: u32,
) -> Result<RecycleActionResult, String> {
    work(move || {
        let state = app.state::<AppState>();
        let _operation = state.operation.try_lock().map_err(|_| "已有操作正在运行")?;
        let scan = lock(&state.space)?.clone().ok_or("请先完成空间扫描")?;
        scan.recycle(&roots()?, target_kind, id)
    })
    .await
}

#[tauri::command]
pub async fn history_list(days: u32) -> Result<CleanHistoryList, String> {
    work(move || journal::list(&roots()?, days)).await
}

#[tauri::command]
pub async fn history_detail(run_id: String, days: u32) -> Result<CleanHistoryDetail, String> {
    work(move || journal::detail(&roots()?, &run_id, days)).await
}

#[tauri::command]
pub async fn settings_get() -> Result<AppSettings, String> {
    work(move || settings::get(&roots()?)).await
}

#[tauri::command]
pub async fn settings_save(app: AppHandle, patch: AppSettingsPatch) -> Result<AppSettings, String> {
    work(move || {
        let state = app.state::<AppState>();
        let _operation = state.operation.try_lock().map_err(|_| "已有操作正在运行")?;
        settings::save(&roots()?, patch)
    })
    .await
}

#[tauri::command]
pub async fn rule_pack_import(
    app: AppHandle,
    json_content: String,
) -> Result<RulePackInfo, String> {
    work(move || {
        let state = app.state::<AppState>();
        let _operation = state.operation.try_lock().map_err(|_| "已有操作正在运行")?;
        let pack = settings::import_pack(&roots()?, &json_content)?;
        *lock(&state.junk)? = None;
        lock(&state.conflicts)?.clear();
        Ok(pack)
    })
    .await
}

#[tauri::command]
pub async fn rule_pack_remove(app: AppHandle, pack_id: String) -> Result<(), String> {
    work(move || {
        let state = app.state::<AppState>();
        let _operation = state.operation.try_lock().map_err(|_| "已有操作正在运行")?;
        settings::remove_pack(&roots()?, &pack_id)?;
        *lock(&state.junk)? = None;
        lock(&state.conflicts)?.clear();
        Ok(())
    })
    .await
}
