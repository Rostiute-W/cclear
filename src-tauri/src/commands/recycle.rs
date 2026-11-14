//! 回收站命令：清空 C 盘回收站。
//!
//! 前端只能传上次查询到的条目数和大小，路径固定由核心库给出，不接受任何自由路径。

use cclear_core::journal::{FileJournal, JournalSink, Record, unix_now};
use cclear_core::special::recycle_bin::{empty_c, query_c};
use serde::Serialize;

/// C 盘回收站当前的内容。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecycleBinDto {
    pub items: i64,
    pub bytes: i64,
}

#[tauri::command]
pub async fn recycle_bin_query() -> Result<RecycleBinDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (items, bytes) = query_c()?;
        Ok(RecycleBinDto { items, bytes })
    })
    .await
    .map_err(|error| format!("读取回收站失败：{error}"))?
}

/// 清空 C 盘回收站。内容与用户确认时不一致就不动，让他重新确认。
#[tauri::command]
pub async fn recycle_bin_empty(
    expected_items: i64,
    expected_bytes: i64,
) -> Result<RecycleBinDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (items, bytes) = query_c()?;
        if items != expected_items || bytes != expected_bytes {
            return Err("回收站内容已变化，请重新确认".to_string());
        }

        empty_c()?;

        // 已经清空了，日志写不进去也必须如实报告，不能让这次操作没有记录。
        let mut journal = FileJournal::new()
            .map_err(|error| format!("回收站已清空，但操作日志写入失败：{error}"))?;
        let record = Record::SpecialAction {
            at: unix_now(),
            action: "emptyRecycleBin".to_string(),
            detail: format!("清空 C 盘回收站：{items} 项，{bytes} 字节"),
        };
        journal
            .append(&record)
            .map_err(|error| format!("回收站已清空，但操作日志写入失败：{error}"))?;

        let (items, bytes) = query_c()?;
        Ok(RecycleBinDto { items, bytes })
    })
    .await
    .map_err(|error| format!("清空回收站失败：{error}"))?
}
