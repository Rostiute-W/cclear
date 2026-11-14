//! 用户设置：目前只有隔离区根目录。

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::win::{KnownFolder, known_folder};

const SETTINGS_DIR: &str = "CClear";
const SETTINGS_FILE: &str = "settings.json";

/// 设置文件的全部字段。后续任务可以继续加字段，缺字段按默认值读。
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quarantine_root: Option<String>,
}

/// 设置文件的位置：`<LocalAppData>\CClear\settings.json`。
pub fn settings_path() -> io::Result<PathBuf> {
    known_folder(KnownFolder::LocalAppData)
        .map(|path| path.join(SETTINGS_DIR).join(SETTINGS_FILE))
        .map_err(|error| io::Error::other(error.to_string()))
}

/// 读已经保存的隔离区根目录。还没设置过时返回 None。
pub fn load_quarantine_root() -> io::Result<Option<PathBuf>> {
    let path = settings_path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // 第一次运行还没有设置文件，不是错误。
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let settings: Settings = serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    Ok(settings.quarantine_root.map(PathBuf::from))
}

/// 保存隔离区根目录。
///
/// 设置只是个偏好，不参与恢复：文件坏了就直接覆盖写入，不必为了保住一行 JSON 拒绝工作。
pub fn save_quarantine_root(root: &Path) -> io::Result<()> {
    let path = settings_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let settings = Settings {
        quarantine_root: Some(root.to_string_lossy().into_owned()),
    };
    let json = serde_json::to_vec_pretty(&settings).map_err(io::Error::other)?;

    std::fs::write(&path, json)
}
