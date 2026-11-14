//! 公共类型：分级、恢复类型、分类、阶段、已知根目录与体积信息。

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

/// 规则分级。Safe/Caution 可一键或逐项清理，Leftover 默认隔离，Manual 只分析，Forbidden 不可选中。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tier {
    Safe,
    Caution,
    Leftover,
    Manual,
    Forbidden,
}

/// 恢复方式：决定清理后还能不能找回。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Recovery {
    /// 可再生：应用会自己重建。
    Regenerable,
    /// 不可再生文件的永久删除。
    IrreversibleFile,
    /// 移入隔离区，可还原。
    Quarantine,
    /// 官方命令，单独分组并标明不可撤销。
    IrreversibleCommand,
    /// 不提供清理，只做展示。
    None,
}

/// 规则分类，界面按它分组。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    System,
    Browser,
    Dev,
    App,
    Leftover,
    UserData,
    Program,
}

/// 阶段。只有列在 `ENABLED_PHASES` 里的阶段才会给出执行资格。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Analysis,
    M3a,
    M3b,
    M4,
    M5a,
    M5b,
}

/// 规则模板可以挂靠的已知根目录。用户目录一律由 `SHGetKnownFolderPath` 解析。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KnownRoot {
    LocalAppData,
    RoamingAppData,
    Profile,
    Documents,
    Downloads,
    ProgramData,
    Windows,
    SystemDrive,
}
