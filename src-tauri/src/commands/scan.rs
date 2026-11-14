//! 扫描与空间分析命令。
//!
//! 只有 `scan_start` 接受路径且必须经过 `validate_external_path`，
//! 其余命令只接受节点 ID 或文件序号，路径一律从最近一次扫描结果里取。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cclear_core::exec::command::open_in_explorer;
use cclear_core::guard::validate_external_path;
use cclear_core::scan::{DirNode, ScanOptions, ScanProgress, ScanResult, scan};
use cclear_core::win::volume::list_fixed_volumes;
use cclear_core::win::{KnownFolder, known_folder};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{Manager, State};

use crate::state::AppState;

/// 进度推送间隔。扫 C 盘要几分钟，200 ms 既看得出变化又不会把 IPC 刷爆。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// 扫描过程中推给界面的进度。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgressDto {
    pub dirs: u64,
    pub files: u64,
    pub allocated: u64,
    pub elapsed_ms: u64,
    /// 扫描已结束，界面据此从进度切换到摘要。
    pub done: bool,
    pub cancelled: bool,
    /// 扫描无法开始或中途失败时的中文原因，成功时为 None。
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeDto {
    pub root: String,
    pub label: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub is_system: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRootDto {
    pub label: String,
    pub path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummaryDto {
    pub root: String,
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
    pub dirs: u64,
    pub denied_dirs: u64,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirNodeDto {
    pub id: u32,
    pub name: String,
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
    pub denied: bool,
    pub reparse_skipped: u32,
    pub has_children: bool,
    pub percent_of_parent: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntryDto {
    pub index: u32,
    pub path: String,
    pub logical: u64,
    pub allocated: u64,
    /// Unix 秒，界面自己按本地时区格式化；0 表示时间不可用。
    pub last_write: i64,
}

#[tauri::command]
pub fn volume_info() -> Vec<VolumeDto> {
    list_fixed_volumes()
        .into_iter()
        .map(|volume| VolumeDto {
            root: volume.root.to_string_lossy().into_owned(),
            label: volume.label,
            fs: volume.fs,
            total: volume.total,
            free: volume.free,
            is_system: volume.is_system,
        })
        .collect()
}

#[tauri::command]
pub fn default_scan_roots() -> Vec<ScanRootDto> {
    let mut roots = vec![ScanRootDto {
        label: "C 盘".to_string(),
        path: r"C:\".to_string(),
    }];

    // 用户目录可能被重定向到别的盘，所以单独给一项，而不是假定它在 C 盘。
    if let Ok(profile) = known_folder(KnownFolder::Profile) {
        roots.push(ScanRootDto {
            label: "用户目录".to_string(),
            path: profile.to_string_lossy().into_owned(),
        });
    }

    roots
}

#[tauri::command]
pub fn scan_start(
    root: String,
    on_progress: Channel<ScanProgressDto>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let path = validate_external_path(&root).map_err(|error| error.to_string())?;
    let (scan_id, cancel) = state.start_scan()?;

    std::thread::spawn(move || run_scan(path, on_progress, app, cancel));

    Ok(scan_id)
}

#[tauri::command]
pub fn scan_cancel(state: State<'_, AppState>) {
    state.cancel_scan();
}

#[tauri::command]
pub fn scan_summary(state: State<'_, AppState>) -> Option<ScanSummaryDto> {
    let result = state.result()?;
    let (allocated, logical, files, dirs) = root_totals(&result);

    Some(ScanSummaryDto {
        root: state.scan_root(),
        logical,
        allocated,
        files,
        dirs,
        denied_dirs: result.denied_dirs,
        elapsed_ms: result.elapsed_ms,
        cancelled: result.cancelled,
    })
}

/// 传 None 返回根节点，传节点 ID 返回它的子目录，都按占盘大小降序。
#[tauri::command]
pub fn scan_children(
    node_id: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Vec<DirNodeDto>, String> {
    let result = state.result().ok_or("还没有扫描结果，请先到总览页扫描")?;

    let nodes: Vec<&DirNode> = match node_id {
        None => {
            let mut roots: Vec<&DirNode> = result
                .roots
                .iter()
                .map(|id| &result.nodes[*id as usize])
                .collect();
            roots.sort_by_key(|node| std::cmp::Reverse(node.size.allocated));
            roots
        }
        Some(id) => {
            if result.nodes.get(id as usize).is_none() {
                return Err("目录不存在".to_string());
            }
            result.children_by_size(id)
        }
    };

    Ok(nodes
        .into_iter()
        .map(|node| dir_node_dto(&result, node))
        .collect())
}

#[tauri::command]
pub fn scan_top_files(limit: u32, state: State<'_, AppState>) -> Result<Vec<FileEntryDto>, String> {
    let result = state.result().ok_or("还没有扫描结果，请先到总览页扫描")?;
    let take = (limit as usize).min(result.top_files.len());

    Ok(result
        .top_files
        .iter()
        .take(take)
        .enumerate()
        .map(|(index, file)| FileEntryDto {
            index: index as u32,
            path: file.path.to_string_lossy().into_owned(),
            logical: file.logical,
            allocated: file.allocated,
            last_write: filetime_to_unix(file.last_write_time),
        })
        .collect())
}

#[tauri::command]
pub fn open_node_in_explorer(node_id: u32, state: State<'_, AppState>) -> Result<(), String> {
    let result = state.result().ok_or("还没有扫描结果，请先到总览页扫描")?;
    if result.nodes.get(node_id as usize).is_none() {
        return Err("目录不存在".to_string());
    }

    open_in_explorer(&result.path_of(node_id))
        .map_err(|error| format!("打开资源管理器失败：{error}"))
}

#[tauri::command]
pub fn open_top_file_in_explorer(index: u32, state: State<'_, AppState>) -> Result<(), String> {
    let result = state.result().ok_or("还没有扫描结果，请先到总览页扫描")?;
    let file = result
        .top_files
        .get(index as usize)
        .ok_or("文件不存在".to_string())?;

    open_in_explorer(&file.path).map_err(|error| format!("打开资源管理器失败：{error}"))
}

/// 在后台线程里扫描，并按固定间隔把进度推给界面。
fn run_scan(
    root: PathBuf,
    channel: Channel<ScanProgressDto>,
    app: tauri::AppHandle,
    cancel: Arc<AtomicBool>,
) {
    let progress = Arc::new(ScanProgress::default());
    let finished = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    // 根路径要随结果一起记下来：扫描在根目录就被取消时结果里没有根节点。
    let root_text = root.to_string_lossy().into_owned();

    let worker = {
        let progress = progress.clone();
        let finished = finished.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let result = scan(&[root], &ScanOptions::default(), &progress, &cancel);
            finished.store(true, Ordering::SeqCst);
            result
        })
    };

    while !finished.load(Ordering::SeqCst) {
        std::thread::sleep(PROGRESS_INTERVAL);
        if finished.load(Ordering::SeqCst) {
            break;
        }

        let _ = channel.send(ScanProgressDto {
            dirs: progress.dirs.load(Ordering::Relaxed),
            files: progress.files.load(Ordering::Relaxed),
            allocated: progress.allocated.load(Ordering::Relaxed),
            elapsed_ms: started.elapsed().as_millis() as u64,
            done: false,
            cancelled: false,
            error: None,
        });
    }

    // 扫描线程结束后状态和进度都要收尾，界面靠 done 这一帧切换成摘要。
    let state = app.state::<AppState>();
    match worker.join() {
        Ok(Ok(result)) => {
            let (allocated, _, files, dirs) = root_totals(&result);
            let finished_progress = ScanProgressDto {
                dirs,
                files,
                allocated,
                elapsed_ms: result.elapsed_ms,
                done: true,
                cancelled: result.cancelled,
                error: None,
            };
            state.finish_scan(result, root_text);
            let _ = channel.send(finished_progress);
        }
        Ok(Err(error)) => {
            state.abort_scan();
            let _ = channel.send(failed_progress(format!("扫描失败：{error}")));
        }
        Err(_) => {
            state.abort_scan();
            let _ = channel.send(failed_progress("扫描线程异常结束".to_string()));
        }
    }
}

fn failed_progress(message: String) -> ScanProgressDto {
    ScanProgressDto {
        dirs: 0,
        files: 0,
        allocated: 0,
        elapsed_ms: 0,
        done: true,
        cancelled: false,
        error: Some(message),
    }
}

/// 根节点的统计已经覆盖整棵子树，多个根就相加。
fn root_totals(result: &ScanResult) -> (u64, u64, u64, u64) {
    let mut allocated = 0;
    let mut logical = 0;
    let mut files = 0;
    let mut dirs = 0;

    for id in &result.roots {
        let node = &result.nodes[*id as usize];
        allocated += node.size.allocated;
        logical += node.size.logical;
        files += node.size.files;
        dirs += node.size.dirs;
    }

    (allocated, logical, files, dirs)
}

fn dir_node_dto(result: &ScanResult, node: &DirNode) -> DirNodeDto {
    // 根节点没有父目录，比例条按满格显示。
    let parent_allocated = match node.parent {
        Some(parent) => result.nodes[parent as usize].size.allocated,
        None => node.size.allocated,
    };
    let percent_of_parent = if parent_allocated == 0 {
        0.0
    } else {
        node.size.allocated as f64 / parent_allocated as f64
    };

    DirNodeDto {
        id: node.id,
        name: node.name.clone(),
        logical: node.size.logical,
        allocated: node.size.allocated,
        files: node.size.files,
        denied: node.denied,
        reparse_skipped: node.reparse_skipped,
        has_children: !node.children.is_empty(),
        percent_of_parent,
    }
}

/// FILETIME（1601 年起的 100 纳秒）换算成 Unix 秒，界面统一按本地时区格式化。
fn filetime_to_unix(filetime: u64) -> i64 {
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const SECONDS_TO_1601: u64 = 11_644_473_600;

    let seconds = filetime / TICKS_PER_SECOND;
    seconds.saturating_sub(SECONDS_TO_1601) as i64
}
