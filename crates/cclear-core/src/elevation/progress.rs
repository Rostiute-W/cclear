//! helper 的进度文件：每行一条 JSON，主程序每秒读一次显示。
//!
//! 进度只用于展示。写不进去时不影响操作本身，界面最多看不到百分比，
//! 所以这里的写入错误一律不向上报告。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 进度文件里的一行。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpProgress {
    /// 操作的字符串形式，例如 `dism-component-cleanup`。
    pub op: String,
    /// 0–100 的百分比。命令还没给出数字时为 None。
    pub percent: Option<f32>,
    /// 中文说明，界面直接显示。
    pub detail: String,
}

impl OpProgress {
    /// 解析进度文件的一行。读到半行或坏行时返回 None。
    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line).ok()
    }
}

/// 追加写进度文件。同一个百分比只写一次，免得界面每秒收到重复的行。
pub struct ProgressWriter {
    path: Option<PathBuf>,
    op: String,
    last_percent: Option<f32>,
}

impl ProgressWriter {
    /// 没有 `--progress` 参数时得到一个什么都不写的写入器。
    pub fn new(path: Option<&Path>, op: &str) -> Self {
        Self {
            path: path.map(Path::to_path_buf),
            op: op.to_string(),
            last_percent: None,
        }
    }

    /// 记一条进度。百分比和上一条相同时跳过。
    pub fn note(&mut self, percent: Option<f32>, detail: String) {
        if percent.is_some() && percent == self.last_percent {
            return;
        }
        self.last_percent = percent;

        let Some(path) = &self.path else {
            return;
        };
        let Ok(line) = serde_json::to_string(&OpProgress {
            op: self.op.clone(),
            percent,
            detail,
        }) else {
            return;
        };

        // 每行单独打开再追加：行很短，写入是原子的，读的一方不会看到半行。
        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
            return;
        };
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}
