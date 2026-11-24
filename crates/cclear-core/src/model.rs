use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeInfo {
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
    pub dirs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIdentity {
    pub volume_serial: u64,
    pub file_id: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CleanCategory {
    System,
    Browser,
    App,
    Dev,
    Leftover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeleteMode {
    Direct,
    RecycleBin,
    Command,
    SystemOp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CleanSource {
    BuiltinRule,
    ExternalRule,
    HeuristicElectron,
    HeuristicUwp,
    HeuristicOrphan,
    Command,
    RecycleBin,
    SystemOp,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: CleanCategory,
    pub source: CleanSource,
    pub size_bytes: Option<u64>,
    pub file_count: u64,
    pub default_selected: bool,
    pub requires_admin: bool,
    pub delete_mode: DeleteMode,
    pub warning: Option<String>,
    pub running_processes: Vec<String>,
    pub sample_paths: Vec<PathBuf>,
    pub anchors: Vec<PathBuf>,
    pub last_modified_at: Option<i64>,
}

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
    ProgramFiles,
    ProgramFilesX86,
    OneDrive,
}
