use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::elevation::{self, SystemOp};
use crate::exec::{command, file_cleaner::filetime_unix};
use crate::guard::{self, Forbidden};
use crate::model::{KnownRoot, SizeInfo};
use crate::rules::{RootMap, forbidden::same_or_under};
use crate::scan::{self, ScanOptions, ScanResult};
use crate::settings;
use crate::win::{self, volume};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRoot {
    label: String,
    path: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    root: PathBuf,
    label: String,
    fs: String,
    total: u64,
    free: u64,
    is_system: bool,
}

pub fn volumes() -> Vec<VolumeInfo> {
    volume::list_fixed_volumes()
        .into_iter()
        .map(|volume| VolumeInfo {
            root: volume.root,
            label: volume.label,
            fs: volume.fs,
            total: volume.total,
            free: volume.free,
            is_system: volume.is_system,
        })
        .collect()
}

pub fn default_roots(roots: &RootMap) -> Vec<ScanRoot> {
    [
        (KnownRoot::SystemDrive, "系统盘"),
        (KnownRoot::Profile, "用户目录"),
        (KnownRoot::Downloads, "下载目录"),
    ]
    .into_iter()
    .filter_map(|(root, label)| {
        roots.get(root).map(|path| ScanRoot {
            label: label.into(),
            path: path.to_path_buf(),
        })
    })
    .collect()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    root: PathBuf,
    mode: &'static str,
    logical: u64,
    allocated: u64,
    files: u64,
    dirs: u64,
    denied_dirs: u64,
    elapsed_ms: u64,
    pub cancelled: bool,
    fallback_note: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    dirs: u64,
    files: u64,
    allocated: u64,
    elapsed_ms: u64,
    mode: &'static str,
    done: bool,
    cancelled: bool,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirNode {
    id: u32,
    name: String,
    logical: u64,
    allocated: u64,
    files: u64,
    denied: bool,
    reparse_skipped: u32,
    has_children: bool,
    percent_of_parent: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    index: usize,
    path: PathBuf,
    logical: u64,
    allocated: u64,
    last_write: u64,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SpaceTargetKind {
    Node,
    TopFile,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecycleActionResult {
    path: PathBuf,
    freed_bytes: u64,
    detail: String,
}

pub struct SpaceScan {
    pub summary: ScanSummary,
    result: ScanResult,
}

pub fn scan(
    roots: &RootMap,
    text: &str,
    use_mft: bool,
    cancel: &AtomicBool,
    progress: impl Fn(ScanProgress),
) -> Result<SpaceScan, String> {
    let root = guard::validate_external_path(text).map_err(|error| error.to_string())?;
    guard::open_anchor(&root, &Forbidden::empty())
        .map_err(|error| format!("无法打开扫描目录：{error}"))?;
    let started = Instant::now();
    let stats = scan::ScanProgress::default();
    let mut mode = if use_mft { "mft" } else { "standard" };
    let stream = |stats: &scan::ScanProgress, mode: &'static str, done, error| {
        progress(ScanProgress {
            dirs: stats.dirs.load(Ordering::Relaxed),
            files: stats.files.load(Ordering::Relaxed),
            allocated: stats.allocated.load(Ordering::Relaxed),
            elapsed_ms: started.elapsed().as_millis() as u64,
            mode,
            done,
            cancelled: cancel.load(Ordering::Relaxed),
            error,
        });
    };
    let mut fallback_note = None;
    let mut mft_result = None;
    if use_mft {
        let attempt = std::thread::scope(|scope| {
            let worker = scope.spawn(|| scan_mft(roots, &root, &stats, cancel));
            while !worker.is_finished() {
                stream(&stats, "mft", false, None);
                std::thread::sleep(Duration::from_millis(150));
            }
            worker
                .join()
                .map_err(|_| "MFT 扫描线程异常退出".to_owned())?
        });
        match attempt {
            Ok(result) => mft_result = Some(result),
            Err(error) => {
                fallback_note = Some(format!("{error}，已切换到标准扫描"));
                mode = "standard";
            }
        }
    }
    let final_stats = scan::ScanProgress::default();
    let scan = match mft_result {
        Some(result) => Ok(result),
        None => std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                scan::scan(
                    std::slice::from_ref(&root),
                    &ScanOptions::default(),
                    &final_stats,
                    cancel,
                )
                .map_err(|error| error.to_string())
            });
            while !worker.is_finished() {
                stream(&final_stats, "standard", false, None);
                std::thread::sleep(Duration::from_millis(150));
            }
            worker
                .join()
                .map_err(|_| "标准扫描线程异常退出".to_owned())?
        }),
    };
    let result = match scan {
        Ok(result) => result,
        Err(error) => {
            stream(&final_stats, mode, true, Some(error.clone()));
            return Err(error);
        }
    };
    let size = result
        .roots
        .first()
        .map_or(SizeInfo::default(), |id| result.nodes[*id as usize].size);
    final_stats.dirs.store(size.dirs, Ordering::Relaxed);
    final_stats.files.store(size.files, Ordering::Relaxed);
    final_stats
        .allocated
        .store(size.allocated, Ordering::Relaxed);
    stream(&final_stats, mode, true, None);
    let summary = ScanSummary {
        root,
        mode,
        logical: size.logical,
        allocated: size.allocated,
        files: size.files,
        dirs: size.dirs,
        denied_dirs: result.denied_dirs,
        elapsed_ms: started.elapsed().as_millis() as u64,
        cancelled: result.cancelled || cancel.load(Ordering::Relaxed),
        fallback_note,
    };
    Ok(SpaceScan { summary, result })
}

fn scan_mft(
    roots: &RootMap,
    root: &Path,
    stats: &scan::ScanProgress,
    cancel: &AtomicBool,
) -> Result<ScanResult, String> {
    let name = root.to_string_lossy();
    if name.len() != 3
        || !volume::list_fixed_volumes().iter().any(|volume| {
            volume.fs.eq_ignore_ascii_case("NTFS")
                && win::equals_ignore_case(&volume.root.to_string_lossy(), &name)
        })
    {
        return Err("MFT 扫描需要本地固定 NTFS 卷根目录".into());
    }
    if win::is_elevated() {
        return win::mft::scan_volume(root, stats, cancel).map_err(|error| error.to_string());
    }
    let reply = elevation::run_elevated(
        roots,
        &[SystemOp::MftScan],
        elevation::RunOptions {
            mft: name.chars().next().map(|drive| (drive, stats)),
            cancel: Some(cancel),
            ..elevation::RunOptions::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let mut result = reply
        .scan
        .ok_or_else(|| reply.error.unwrap_or_else(|| "未收到 MFT 扫描结果".into()))?;
    validate_result(&result, root)?;
    result.cancelled |= cancel.load(Ordering::Relaxed);
    Ok(result)
}

fn validate_result(result: &ScanResult, root: &Path) -> Result<(), String> {
    let error = "管理员 MFT 结果结构无效";
    if result.roots != [0] || result.nodes.is_empty() || result.top_files.len() > 500 {
        return Err(error.into());
    }
    for (index, node) in result.nodes.iter().enumerate() {
        if node.id as usize != index
            || node.children.iter().any(|id| {
                result
                    .nodes
                    .get(*id as usize)
                    .is_none_or(|child| child.parent != Some(node.id))
            })
        {
            return Err(error.into());
        }
        if index == 0 {
            let path = Path::new(&node.name);
            if node.parent.is_some() || !same_or_under(path, root) || !same_or_under(root, path) {
                return Err(error.into());
            }
        } else if node.parent.is_none_or(|parent| parent as usize >= index)
            || guard::validate_component(node.name.as_ref()).is_err()
            || node.name.ends_with([' ', '.'])
        {
            return Err(error.into());
        }
    }
    if result.top_files.iter().any(|file| {
        !same_or_under(&file.path, root)
            || guard::validate_external_path(&file.path.to_string_lossy()).is_err()
    }) {
        return Err(error.into());
    }
    Ok(())
}

impl SpaceScan {
    pub fn children(&self, id: Option<u32>) -> Result<Vec<DirNode>, String> {
        let (children, total) = if let Some(id) = id {
            let parent = self.result.nodes.get(id as usize).ok_or("目录编号无效")?;
            (parent.children.as_slice(), parent.size.allocated)
        } else {
            (self.result.roots.as_slice(), self.summary.allocated)
        };
        let mut rows: Vec<_> = children
            .iter()
            .map(|id| {
                let node = &self.result.nodes[*id as usize];
                DirNode {
                    id: node.id,
                    name: node.name.clone(),
                    logical: node.size.logical,
                    allocated: node.size.allocated,
                    files: node.size.files,
                    denied: node.denied,
                    reparse_skipped: node.reparse_skipped,
                    has_children: !node.children.is_empty(),
                    percent_of_parent: if total == 0 {
                        0.0
                    } else {
                        node.size.allocated as f64 / total as f64
                    },
                }
            })
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.allocated));
        Ok(rows)
    }

    pub fn top_files(&self, limit: usize) -> Vec<FileEntry> {
        self.result
            .top_files
            .iter()
            .take(limit.min(500))
            .enumerate()
            .map(|(index, file)| FileEntry {
                index,
                path: file.path.clone(),
                logical: file.logical,
                allocated: file.allocated,
                last_write: filetime_unix(file.last_write_time),
            })
            .collect()
    }

    pub fn target(&self, kind: SpaceTargetKind, id: u32) -> Result<PathBuf, String> {
        match kind {
            SpaceTargetKind::Node => self
                .result
                .nodes
                .get(id as usize)
                .map(|_| self.result.path_of(id)),
            SpaceTargetKind::TopFile => self
                .result
                .top_files
                .get(id as usize)
                .map(|file| file.path.clone()),
        }
        .ok_or_else(|| "空间扫描项目编号无效".into())
    }

    pub fn open(&self, kind: SpaceTargetKind, id: u32) -> Result<(), String> {
        command::open_in_explorer(&self.target(kind, id)?)
            .map_err(|error| format!("无法打开资源管理器：{error}"))
    }

    pub fn recycle(
        &self,
        roots: &RootMap,
        kind: SpaceTargetKind,
        id: u32,
    ) -> Result<RecycleActionResult, String> {
        if matches!(kind, SpaceTargetKind::Node) && self.result.roots.contains(&id) {
            return Err("不能回收扫描起始目录".into());
        }
        let target = self.target(kind, id)?;
        let preferences = settings::load_preferences(roots)?;
        let mut forbidden = Forbidden::from_rules(roots);
        forbidden.protect_paths(&preferences.whitelist_paths);
        let protected_roots = [
            KnownRoot::Windows,
            KnownRoot::ProgramData,
            KnownRoot::Profile,
            KnownRoot::LocalAppData,
            KnownRoot::RoamingAppData,
        ];
        if protected_roots
            .into_iter()
            .filter_map(|root| roots.get(root))
            .any(|root| {
                same_or_under(root, &target)
                    || same_or_under(&target, root) && roots.get(KnownRoot::Windows) == Some(root)
            })
        {
            return Err("此目录受系统保护".into());
        }
        if preferences
            .whitelist_paths
            .iter()
            .any(|excluded| same_or_under(&target, excluded) || same_or_under(excluded, &target))
        {
            return Err("此项目受到白名单保护".into());
        }
        guard::recycle::send_to_recycle_bin(&target, &forbidden)
            .map_err(|error| error.to_string())?;
        Ok(RecycleActionResult {
            path: target,
            freed_bytes: 0,
            detail: "已移入 Windows 回收站。清空回收站后才会释放空间。".into(),
        })
    }
}
