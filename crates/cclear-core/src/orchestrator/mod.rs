pub mod deep_slim;
pub mod space;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::elevation::{self, ElevationError};
use crate::exec::{
    command,
    file_cleaner::{Cleaner, FileReport},
};
use crate::guard::{Forbidden, GuardError, recycle};
use crate::journal;
use crate::model::{CleanCategory, CleanItem, CleanSource, DeleteMode, KnownRoot};
use crate::rules::{
    RootMap, commands,
    engine::{self, Candidate},
    forbidden::same_or_under,
    heuristics,
    schema::{self, Target},
};
use crate::settings;
use crate::special::recycle_bin;
use crate::win::{
    equals_ignore_case,
    process::{self, ProcInfo},
    registry, volume,
};

use deep_slim::{DeepSlimState, WinSxSState};

const CATEGORIES: [(CleanCategory, &str, &str); 5] = [
    (CleanCategory::System, "系统垃圾", "系统与用户临时文件"),
    (CleanCategory::Browser, "浏览器缓存", "网页和图形缓存"),
    (CleanCategory::App, "应用缓存", "常用应用可重新生成的数据"),
    (CleanCategory::Dev, "开发工具缓存", "包管理和构建缓存"),
    (
        CleanCategory::Leftover,
        "疑似卸载残留",
        "确认后可移入回收站",
    ),
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryGroup {
    category: CleanCategory,
    label: &'static str,
    description: &'static str,
    total_bytes: u64,
    item_count: usize,
    items: Vec<CleanItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JunkScanProgress {
    phase: &'static str,
    current_item: String,
    found_bytes: u64,
    found_items: usize,
    done: bool,
    cancelled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JunkScanResult {
    pub scan_id: String,
    elapsed_ms: u64,
    total_bytes: u64,
    default_selected_bytes: u64,
    categories: Vec<CategoryGroup>,
    deep_slim: DeepSlimState,
    pub cancelled: bool,
}

pub struct JunkScan {
    pub id: String,
    candidates: Vec<Candidate>,
}

pub fn junk_scan(
    roots: &RootMap,
    winsxs: &WinSxSState,
    cancel: &AtomicBool,
    progress: impl Fn(JunkScanProgress),
) -> Result<(JunkScanResult, JunkScan), String> {
    let started = Instant::now();
    let preferences = settings::load_preferences(roots)?;
    let packs = settings::external_packs(roots)?;
    let mut forbidden = Forbidden::from_rules(roots);
    forbidden.protect_paths(&preferences.whitelist_paths);
    let cleaner = Cleaner {
        forbidden: &forbidden,
        whitelist: &preferences.whitelist_paths,
        cancel,
    };
    let processes = process::running_processes();
    let mut candidates = Vec::new();
    let mut found_bytes = 0u64;
    for (pack, builtin) in
        std::iter::once((schema::builtin(), true)).chain(packs.iter().map(|pack| (pack, false)))
    {
        for rule in &pack.rules {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            progress(JunkScanProgress {
                phase: "rules",
                current_item: rule.name.clone(),
                found_bytes,
                found_items: candidates.len(),
                done: false,
                cancelled: false,
            });
            let id = format!("{}:{}", pack.id, rule.id);
            if let Some(mut candidate) = engine::evaluate(
                rule,
                id,
                roots,
                &cleaner,
                builtin,
                preferences.min_temp_age_hours,
            ) {
                candidate.item.running_processes = matching_names(&candidate, &processes);
                found_bytes =
                    found_bytes.saturating_add(candidate.item.size_bytes.unwrap_or_default());
                candidates.push(candidate);
            }
        }
    }
    if !cancel.load(Ordering::Relaxed) {
        progress(JunkScanProgress {
            phase: "heuristics",
            current_item: "识别应用缓存和卸载残留".into(),
            found_bytes,
            found_items: candidates.len(),
            done: false,
            cancelled: false,
        });
        let covered: Vec<_> = candidates
            .iter()
            .flat_map(|candidate| candidate.item.anchors.iter().cloned())
            .collect();
        let mut detected = heuristics::electron::detect(roots, &cleaner, &covered);
        detected.extend(heuristics::uwp::detect(roots, &cleaner));
        if !cancel.load(Ordering::Relaxed) {
            let installed = registry::installed_software();
            if !installed.is_empty() && !processes.is_empty() {
                detected.extend(heuristics::orphans::detect(
                    roots,
                    &cleaner,
                    &installed,
                    &processes,
                    &process::loaded_module_paths(&processes),
                    journal::unix_now(),
                ));
            }
        }
        for mut candidate in detected {
            candidate.item.running_processes = matching_names(&candidate, &processes);
            found_bytes = found_bytes.saturating_add(candidate.item.size_bytes.unwrap_or_default());
            candidates.push(candidate);
        }
    }
    let categories = CATEGORIES
        .into_iter()
        .map(|(category, label, description)| {
            let items: Vec<_> = candidates
                .iter()
                .filter(|candidate| candidate.item.category == category)
                .map(|candidate| candidate.item.clone())
                .collect();
            let total_bytes = items.iter().fold(0u64, |sum, item| {
                sum.saturating_add(item.size_bytes.unwrap_or(0))
            });
            CategoryGroup {
                category,
                label,
                description,
                total_bytes,
                item_count: items.len(),
                items,
            }
        })
        .collect();
    let default_selected_bytes = candidates
        .iter()
        .filter(|candidate| candidate.item.default_selected)
        .fold(0u64, |sum, candidate| {
            sum.saturating_add(candidate.item.size_bytes.unwrap_or(0))
        });
    let id = Uuid::new_v4().to_string();
    let deep_slim = deep_slim::status(roots, winsxs);
    let cancelled = cancel.load(Ordering::Relaxed);
    progress(JunkScanProgress {
        phase: "done",
        current_item: String::new(),
        found_bytes,
        found_items: candidates.len(),
        done: true,
        cancelled,
    });
    let result = JunkScanResult {
        scan_id: id.clone(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        total_bytes: found_bytes,
        default_selected_bytes,
        categories,
        deep_slim,
        cancelled,
    };
    Ok((result, JunkScan { id, candidates }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningProcessConflict {
    app_name: String,
    pub process_names: Vec<String>,
    pids: Vec<u32>,
    affected_item_ids: Vec<String>,
    affected_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessCloseResult {
    closed: Vec<String>,
    still_running: Vec<String>,
}

impl JunkScan {
    fn select(&self, scan_id: &str, ids: &[String]) -> Result<Vec<&Candidate>, String> {
        if scan_id != self.id {
            return Err("扫描已过期，请重新扫描".into());
        }
        let ids: HashSet<_> = ids.iter().map(String::as_str).collect();
        let candidates: Vec<_> = self
            .candidates
            .iter()
            .filter(|candidate| ids.contains(candidate.item.id.as_str()))
            .collect();
        if candidates.len() != ids.len() {
            return Err("部分清理项目不属于当前扫描".into());
        }
        Ok(candidates)
    }

    pub fn check_processes(
        &self,
        scan_id: &str,
        ids: &[String],
    ) -> Result<Vec<RunningProcessConflict>, String> {
        let candidates = self.select(scan_id, ids)?;
        let processes = process::running_processes();
        let mut conflicts = HashMap::new();
        for candidate in candidates {
            for running in processes
                .iter()
                .filter(|running| matches_process(candidate, running))
            {
                let row = conflicts
                    .entry(running.name.to_ascii_lowercase())
                    .or_insert_with(|| RunningProcessConflict {
                        app_name: running.name.clone(),
                        process_names: vec![running.name.clone()],
                        pids: Vec::new(),
                        affected_item_ids: Vec::new(),
                        affected_bytes: 0,
                    });
                if !row.pids.contains(&running.pid) {
                    row.pids.push(running.pid);
                }
                if !row.affected_item_ids.contains(&candidate.item.id) {
                    row.affected_item_ids.push(candidate.item.id.clone());
                    row.affected_bytes = row
                        .affected_bytes
                        .saturating_add(candidate.item.size_bytes.unwrap_or(0));
                }
            }
        }
        let mut result: Vec<_> = conflicts.into_values().collect();
        result.sort_by(|left, right| left.app_name.cmp(&right.app_name));
        Ok(result)
    }

    pub fn open_location(
        &self,
        scan_id: &str,
        id: &str,
        index: Option<usize>,
    ) -> Result<(), String> {
        let candidate = self.select(scan_id, &[id.into()])?[0];
        let path = match index {
            Some(index) => candidate.item.sample_paths.get(index),
            None => candidate.item.anchors.first(),
        }
        .ok_or("没有可打开的路径")?;
        command::open_in_explorer(path).map_err(|error| format!("无法打开资源管理器：{error}"))
    }
}

pub fn close_processes(
    allowed: &[String],
    requested: &[String],
) -> Result<ProcessCloseResult, String> {
    let mut names = Vec::new();
    for name in requested {
        if process::is_protected(name)
            || !allowed
                .iter()
                .any(|allowed| equals_ignore_case(name, allowed))
        {
            return Err("只能关闭当前扫描确认的冲突应用".into());
        }
        if !names
            .iter()
            .any(|known: &String| equals_ignore_case(name, known))
        {
            names.push(name.clone());
        }
    }
    let (closed, still_running) = process::close_processes_gracefully(&names, 4000);
    Ok(ProcessCloseResult {
        closed,
        still_running,
    })
}

fn matching_names(candidate: &Candidate, processes: &[ProcInfo]) -> Vec<String> {
    let mut names: Vec<_> = processes
        .iter()
        .filter(|process| matches_process(candidate, process))
        .map(|process| process.name.clone())
        .collect();
    names.sort_unstable();
    names.dedup_by(|left, right| equals_ignore_case(left, right));
    names
}

fn matches_process(candidate: &Candidate, running: &ProcInfo) -> bool {
    running.image_path.is_some()
        && !process::is_protected(&running.name)
        && (candidate
            .processes
            .iter()
            .any(|name| equals_ignore_case(name, &running.name))
            || running.image_path.as_ref().is_some_and(|image| {
                candidate
                    .item
                    .anchors
                    .iter()
                    .any(|path| same_or_under(image, path))
            }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanExecuteRequest {
    pub scan_id: String,
    pub item_ids: Vec<String>,
    pub schedule_reboot_delete: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanProgress {
    current_item_id: String,
    current_item_name: String,
    completed_items: usize,
    total_items: usize,
    freed_bytes: u64,
    skipped_files: u64,
    stage: &'static str,
    done: bool,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemStatus {
    Done,
    Partial,
    Skipped,
    Failed,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanItemReport {
    pub item_id: String,
    pub name: String,
    pub category: CleanCategory,
    pub status: ItemStatus,
    pub freed_bytes: u64,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub recycled_items: u64,
    pub skipped_files: u64,
    pub reboot_scheduled: u64,
    pub detail: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanReport {
    pub run_id: String,
    pub started_at: i64,
    pub elapsed_ms: u64,
    pub freed_bytes: u64,
    pub free_before: u64,
    pub free_after: u64,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub recycled_items: u64,
    pub skipped_files: u64,
    pub reboot_scheduled: u64,
    pub items: Vec<CleanItemReport>,
    pub cancelled: bool,
    pub error: Option<String>,
}

pub fn clean_execute(
    roots: &RootMap,
    scan: &JunkScan,
    request: CleanExecuteRequest,
    cancel: &AtomicBool,
    progress: impl Fn(CleanProgress),
) -> Result<CleanReport, String> {
    let started = Instant::now();
    let mut selected = scan.select(&request.scan_id, &request.item_ids)?;
    if selected.is_empty() {
        return Err("请至少选择一个清理项目".into());
    }
    let preferences = settings::load_preferences(roots)?;
    let mut forbidden = Forbidden::from_rules(roots);
    forbidden.protect_paths(&preferences.whitelist_paths);
    let cleaner = Cleaner {
        forbidden: &forbidden,
        whitelist: &preferences.whitelist_paths,
        cancel,
    };
    let free = || {
        roots
            .get(KnownRoot::SystemDrive)
            .map_or(0, volume::free_space)
    };
    let mut report = CleanReport {
        run_id: Uuid::new_v4().to_string(),
        started_at: journal::unix_now(),
        elapsed_ms: 0,
        freed_bytes: 0,
        free_before: free(),
        free_after: 0,
        deleted_files: 0,
        deleted_dirs: 0,
        recycled_items: 0,
        skipped_files: 0,
        reboot_scheduled: 0,
        items: Vec::new(),
        cancelled: false,
        error: None,
    };
    let total = selected.len();
    selected.sort_by_key(|candidate| match candidate.target {
        Target::SystemOp { .. } => 4,
        Target::Command { .. } => 3,
        Target::RecycleBin => 1,
        _ if candidate.item.delete_mode == DeleteMode::RecycleBin => 2,
        _ => 0,
    });
    let elevated: Vec<_> = selected
        .iter()
        .filter_map(|candidate| match candidate.target {
            Target::SystemOp { op } => Some(op),
            _ => None,
        })
        .collect();
    let mut elevation_result = None;
    for candidate in selected {
        let mut item = CleanItemReport {
            item_id: candidate.item.id.clone(),
            name: candidate.item.name.clone(),
            category: candidate.item.category,
            status: ItemStatus::Done,
            freed_bytes: 0,
            deleted_files: 0,
            deleted_dirs: 0,
            recycled_items: 0,
            skipped_files: 0,
            reboot_scheduled: 0,
            detail: String::new(),
        };
        let stage = match candidate.target {
            Target::SystemOp { .. } => "elevated",
            Target::Command { .. } => "command",
            Target::RecycleBin => "recycle",
            _ => "user",
        };
        if cancel.load(Ordering::Relaxed) && !(stage == "elevated" && elevation_result.is_some()) {
            item.status = ItemStatus::Skipped;
            item.detail = "清理已取消".into();
            report.items.push(item);
            continue;
        }
        emit(&report, Some(candidate), total, stage, &progress);
        match candidate.target {
            Target::SystemOp { op } => {
                let result = elevation_result.get_or_insert_with(|| {
                    elevation::run_elevated(
                        roots,
                        &elevated,
                        elevation::RunOptions {
                            temp_age: preferences.min_temp_age_hours,
                            reboot_delete: request.schedule_reboot_delete,
                            cancel: Some(cancel),
                            ..elevation::RunOptions::default()
                        },
                    )
                });
                match result {
                    Ok(result) => {
                        if let Some(outcome) =
                            result.outcomes.iter().find(|outcome| outcome.op == op)
                        {
                            apply_files(&mut item, &outcome.files);
                            item.reboot_scheduled = outcome.reboot_scheduled;
                            item.detail = outcome.detail.clone();
                            if !outcome.success {
                                item.status = ItemStatus::Failed;
                            }
                        } else if cancel.load(Ordering::Relaxed) {
                            item.status = ItemStatus::Skipped;
                            item.detail = "清理已取消".into();
                        } else {
                            fail(
                                &mut item,
                                result
                                    .error
                                    .clone()
                                    .unwrap_or_else(|| "管理员进程未返回结果".into()),
                            );
                        }
                    }
                    Err(ElevationError::Cancelled) => {
                        item.status = ItemStatus::Skipped;
                        item.detail = "已取消管理员授权".into();
                    }
                    Err(error) => fail(&mut item, error.to_string()),
                }
            }
            Target::Command { command: id } => {
                let before = cleaner
                    .measure(&candidate.item.anchors, &candidate.filters)
                    .bytes;
                match commands::execute(id, roots, &cleaner, &candidate.item.anchors) {
                    Ok((paths, outcomes)) => {
                        if let Some(error) = outcomes
                            .iter()
                            .find(|outcome| outcome.exit_code != Some(0) || outcome.timed_out)
                        {
                            fail(
                                &mut item,
                                format!("清理命令未确认成功。{}", error.stderr_tail),
                            );
                        } else {
                            item.detail = "清理命令已完成".into();
                        }
                        if paths == candidate.item.anchors {
                            item.freed_bytes = before
                                .saturating_sub(cleaner.measure(&paths, &candidate.filters).bytes);
                        }
                    }
                    Err(error) => fail(&mut item, error),
                }
            }
            Target::RecycleBin => match recycle_bin::query_c() {
                Err(error) => fail(&mut item, error),
                Ok((count, bytes)) => match recycle_bin::empty_c() {
                    Ok(()) => {
                        item.deleted_files = count.max(0) as u64;
                        item.freed_bytes = bytes.max(0) as u64;
                        item.detail = "C 盘回收站已清空".into();
                    }
                    Err(error) => fail(&mut item, error),
                },
            },
            _ if candidate.item.delete_mode == DeleteMode::RecycleBin => {
                let orphan = candidate.item.source == CleanSource::HeuristicOrphan;
                let still_orphan = if orphan {
                    let installed = registry::installed_software();
                    let processes = process::running_processes();
                    !installed.is_empty()
                        && !processes.is_empty()
                        && heuristics::orphans::detect(
                            roots,
                            &cleaner,
                            &installed,
                            &processes,
                            &process::loaded_module_paths(&processes),
                            journal::unix_now(),
                        )
                        .iter()
                        .any(|candidate| candidate.item.id == item.item_id)
                } else {
                    true
                };
                let mut cannot_recycle = false;
                for target in &candidate.item.anchors {
                    if !still_orphan
                        || cleaner.whitelist.iter().any(|excluded| {
                            same_or_under(excluded, target) || same_or_under(target, excluded)
                        })
                    {
                        item.skipped_files += 1;
                        item.detail = "目标状态已变化或受到白名单保护".into();
                        continue;
                    }
                    match recycle::send_to_recycle_bin(target, &forbidden) {
                        Ok(()) => item.recycled_items += 1,
                        Err(error) => {
                            item.skipped_files += 1;
                            cannot_recycle |= matches!(error, GuardError::CannotRecycle);
                            item.detail = error.to_string();
                        }
                    }
                }
                if item.skipped_files > 0 {
                    item.status = if item.recycled_items > 0 {
                        ItemStatus::Partial
                    } else {
                        ItemStatus::Skipped
                    };
                } else {
                    item.detail = "已移入 Windows 回收站".into();
                }
                if cannot_recycle {
                    item.detail = GuardError::CannotRecycle.to_string();
                }
            }
            _ => {
                let mut filters = candidate.filters.clone();
                if candidate.item.id == "builtin:user-temp" {
                    filters.min_age_hours =
                        filters.min_age_hours.max(preferences.min_temp_age_hours);
                }
                apply_files(&mut item, &cleaner.clean(&candidate.item.anchors, &filters));
            }
        }
        report.freed_bytes = report.freed_bytes.saturating_add(item.freed_bytes);
        report.deleted_files += item.deleted_files;
        report.deleted_dirs += item.deleted_dirs;
        report.recycled_items += item.recycled_items;
        report.skipped_files += item.skipped_files;
        report.reboot_scheduled += item.reboot_scheduled;
        report.items.push(item);
        emit(&report, Some(candidate), total, stage, &progress);
    }
    report.free_after = free();
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    report.cancelled = cancel.load(Ordering::Relaxed);
    if let Err(error) = journal::append(roots, &report) {
        report.error = Some(format!("清理历史写入失败：{error}"));
    }
    emit(&report, None, total, "done", &progress);
    Ok(report)
}

fn apply_files(item: &mut CleanItemReport, files: &FileReport) {
    item.freed_bytes = files.freed_bytes;
    item.deleted_files = files.deleted_files;
    item.deleted_dirs = files.deleted_dirs;
    item.skipped_files = files.skipped_files;
    item.detail = files
        .detail
        .clone()
        .unwrap_or_else(|| "文件清理已完成".into());
    if files.detail.is_some() || files.skipped_files > 0 {
        item.status = if files.deleted_files > 0 || files.deleted_dirs > 0 {
            ItemStatus::Partial
        } else {
            ItemStatus::Skipped
        };
    }
}

fn fail(item: &mut CleanItemReport, detail: String) {
    item.status = ItemStatus::Failed;
    item.detail = detail;
}

fn emit(
    report: &CleanReport,
    candidate: Option<&Candidate>,
    total: usize,
    stage: &'static str,
    progress: &impl Fn(CleanProgress),
) {
    progress(CleanProgress {
        current_item_id: candidate.map_or_else(String::new, |item| item.item.id.clone()),
        current_item_name: candidate.map_or_else(String::new, |item| item.item.name.clone()),
        completed_items: report.items.len(),
        total_items: total,
        freed_bytes: report.freed_bytes,
        skipped_files: report.skipped_files,
        stage,
        done: candidate.is_none(),
    });
}
