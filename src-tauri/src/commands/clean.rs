//! 清理页命令：候选占用检测与打开位置。只读，不做任何写入。
//!
//! 前端只传规则 ID 和样例序号，路径一律由核心库按规则重新解析，避免前端把任意路径送进来。

use std::time::SystemTime;

use cclear_core::exec::command::open_in_explorer;
use cclear_core::rules::catalog::find;
use cclear_core::rules::resolve::{RootMap, check_locks, resolve_anchors, sample_paths};
use cclear_core::win::restart_manager::LockReport;
use serde::Serialize;

/// 占用检测结果。`complete = false` 时界面显示“检测不完整”。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockReportDto {
    pub checked: usize,
    pub total: usize,
    pub complete: bool,
    pub lockers: Vec<LockerDto>,
    pub error: Option<String>,
}

/// 一个占用文件的进程。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockerDto {
    pub pid: u32,
    pub app_name: String,
    pub service: Option<String>,
}

#[tauri::command]
pub async fn candidate_locks(rule_id: String) -> Result<LockReportDto, String> {
    // 占用检测要枚举候选文件再逐个注册，放到阻塞线程池里执行，避免占住异步运行时。
    tauri::async_runtime::spawn_blocking(move || {
        let rule = find(&rule_id).ok_or_else(|| "没有这条清理规则".to_string())?;
        let roots = RootMap::system().map_err(|error| error.to_string())?;

        Ok(lock_report_dto(check_locks(
            rule,
            &roots,
            SystemTime::now(),
        )))
    })
    .await
    .map_err(|error| format!("占用检测失败：{error}"))?
}

/// 打开候选的位置。给了 `sample_index` 就打开对应样例，否则打开第一个锚点目录。
#[tauri::command]
pub fn open_candidate_in_explorer(
    rule_id: String,
    sample_index: Option<u32>,
) -> Result<(), String> {
    let rule = find(&rule_id).ok_or_else(|| "没有这条清理规则".to_string())?;
    let roots = RootMap::system().map_err(|error| error.to_string())?;

    let path = match sample_index {
        // 样例序号对应“重新评估”时的同一套统计，这里按同样方式重算一次。
        Some(index) => sample_paths(rule, &roots, SystemTime::now())
            .into_iter()
            .nth(index as usize)
            .ok_or_else(|| "样例路径不存在，请重新评估后再试".to_string())?,
        None => resolve_anchors(rule, &roots)
            .into_iter()
            .next()
            .ok_or_else(|| "这个候选没有可打开的目录".to_string())?,
    };

    open_in_explorer(&path).map_err(|error| format!("打开资源管理器失败：{error}"))
}

fn lock_report_dto(report: LockReport) -> LockReportDto {
    LockReportDto {
        checked: report.checked,
        total: report.total,
        complete: report.complete,
        lockers: report
            .lockers
            .into_iter()
            .map(|locker| LockerDto {
                pid: locker.pid,
                app_name: locker.app_name,
                service: locker.service,
            })
            .collect(),
        error: report.error,
    }
}
