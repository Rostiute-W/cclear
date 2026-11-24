use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};

use serde::Serialize;

use crate::guard::{self, Forbidden, GuardError};
use crate::orchestrator::{CleanItemReport, CleanReport};
use crate::rules::RootMap;
use crate::settings;

const FILENAME: &str = "history-v2.jsonl";

pub type CleanRun = CleanReport;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanHistoryEntry {
    run_id: String,
    started_at: i64,
    elapsed_ms: u64,
    item_names: Vec<String>,
    freed_bytes: u64,
    free_before: u64,
    free_after: u64,
    deleted_files: u64,
    recycled_items: u64,
    skipped_files: u64,
    reboot_scheduled: u64,
    cancelled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanHistoryList {
    pub runs: Vec<CleanHistoryEntry>,
    pub skipped_lines: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanHistoryDetail {
    pub run_id: String,
    started_at: i64,
    elapsed_ms: u64,
    freed_bytes: u64,
    free_before: u64,
    free_after: u64,
    cancelled: bool,
    pub items: Vec<CleanItemReport>,
}

pub fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

pub fn append(roots: &RootMap, run: &CleanRun) -> Result<(), String> {
    let directory = settings::data_dir(roots)?;
    let mut file = guard::path::append_file(&directory, OsStr::new(FILENAME), &Forbidden::empty())
        .map_err(|error| error.to_string())?;
    let mut json = serde_json::to_vec(run).map_err(|error| error.to_string())?;
    json.push(b'\n');
    file.write_all(&json)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())
}

fn read(roots: &RootMap, days: u32) -> Result<(Vec<CleanRun>, usize), String> {
    let directory = settings::data_dir(roots)?;
    let file = match guard::path::read_file(directory.path(), FILENAME, &Forbidden::empty()) {
        Ok(file) => file,
        Err(GuardError::Gone) => return Ok((Vec::new(), 0)),
        Err(error) => return Err(error.to_string()),
    };
    let cutoff = unix_now().saturating_sub(i64::from(days) * 86_400);
    let mut skipped = 0;
    let mut runs = Vec::new();
    for line in BufReader::new(file).split(b'\n') {
        let line = line.map_err(|error| format!("读取清理历史失败：{error}"))?;
        match serde_json::from_slice::<CleanRun>(&line) {
            Ok(run) if run.started_at >= cutoff => runs.push(run),
            Ok(_) => {}
            Err(_) => skipped += 1,
        }
    }
    runs.reverse();
    runs.sort_by_key(|run| std::cmp::Reverse(run.started_at));
    Ok((runs, skipped))
}

pub fn list(roots: &RootMap, days: u32) -> Result<CleanHistoryList, String> {
    let (runs, skipped_lines) = read(roots, days)?;
    let runs = runs
        .into_iter()
        .map(|run| CleanHistoryEntry {
            run_id: run.run_id,
            started_at: run.started_at,
            elapsed_ms: run.elapsed_ms,
            item_names: run.items.into_iter().map(|item| item.name).collect(),
            freed_bytes: run.freed_bytes,
            free_before: run.free_before,
            free_after: run.free_after,
            deleted_files: run.deleted_files,
            recycled_items: run.recycled_items,
            skipped_files: run.skipped_files,
            reboot_scheduled: run.reboot_scheduled,
            cancelled: run.cancelled,
        })
        .collect();
    Ok(CleanHistoryList {
        runs,
        skipped_lines,
    })
}

pub fn detail(roots: &RootMap, id: &str, days: u32) -> Result<CleanHistoryDetail, String> {
    let (runs, _) = read(roots, days)?;
    let run = runs
        .into_iter()
        .find(|run| run.run_id == id)
        .ok_or("未找到清理记录")?;
    Ok(CleanHistoryDetail {
        run_id: run.run_id,
        started_at: run.started_at,
        elapsed_ms: run.elapsed_ms,
        freed_bytes: run.freed_bytes,
        free_before: run.free_before,
        free_after: run.free_after,
        cancelled: run.cancelled,
        items: run.items,
    })
}
