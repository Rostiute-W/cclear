//! 公共类型：分级、恢复类型、分类、体积信息。

use serde::{Deserialize, Serialize};

/// 体积统计。`dirs` 含子树中的全部目录（含自身）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeInfo {
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
    pub dirs: u64,
}

/// 文件身份：硬链接去重和冻结清单校验都按“卷序列号 + 文件 ID”判断同一个文件。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIdentity {
    pub volume_serial: u64,
    pub file_id: u128,
}
