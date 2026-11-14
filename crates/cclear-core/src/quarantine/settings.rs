//! 用户设置：目前只有隔离区根目录。

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::win::{KnownFolder, equals_ignore_case, known_folder};

const SETTINGS_DIR: &str = "CClear";
const SETTINGS_FILE: &str = "settings.json";

/// 设置文件的全部字段。后续任务可以继续加字段，缺字段按默认值读。
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quarantine_root: Option<String>,
    /// 曾经用过的全部隔离根。换过卷之后旧批次还在，必须能在列表里看到。
    /// 旧设置文件没有这个字段，按空列表读。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    quarantine_roots: Vec<String>,
}

/// 设置文件的位置：`<LocalAppData>\CClear\settings.json`。
pub fn settings_path() -> io::Result<PathBuf> {
    known_folder(KnownFolder::LocalAppData)
        .map(|path| path.join(SETTINGS_DIR).join(SETTINGS_FILE))
        .map_err(|error| io::Error::other(error.to_string()))
}

/// 读设置文件。还没设置过时返回默认值。
fn read_settings() -> io::Result<Settings> {
    let path = settings_path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // 第一次运行还没有设置文件，不是错误。
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(error) => return Err(error),
    };

    serde_json::from_str(&text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// 读已经保存的隔离区根目录。还没设置过时返回 None。
pub fn load_quarantine_root() -> io::Result<Option<PathBuf>> {
    Ok(read_settings()?.quarantine_root.map(PathBuf::from))
}

/// 读曾经用过的全部隔离根，当前选择排在最前。
///
/// 去重时不区分大小写：Windows 路径本来就大小写不敏感，同一个目录只扫一次。
pub fn load_quarantine_roots() -> io::Result<Vec<PathBuf>> {
    let settings = read_settings()?;
    let mut roots: Vec<PathBuf> = Vec::new();

    for text in settings
        .quarantine_root
        .iter()
        .chain(&settings.quarantine_roots)
    {
        let path = PathBuf::from(text);
        if roots
            .iter()
            .any(|known| equals_ignore_case(&known.to_string_lossy(), text))
        {
            continue;
        }
        roots.push(path);
    }

    Ok(roots)
}

/// 保存隔离区根目录，并记进“用过的根”列表。
///
/// 设置只是个偏好，不参与恢复：文件坏了就直接覆盖写入，不必为了保住一行 JSON 拒绝工作。
pub fn save_quarantine_root(root: &Path) -> io::Result<()> {
    let path = settings_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    // 读不出来就当成空设置：宁可丢掉旧的卷列表，也不能让用户选不了隔离区。
    let mut settings = read_settings().unwrap_or_default();
    let text = root.to_string_lossy().into_owned();

    if !settings
        .quarantine_roots
        .iter()
        .any(|known| equals_ignore_case(known, &text))
    {
        settings.quarantine_roots.push(text.clone());
    }
    settings.quarantine_root = Some(text);

    let json = serde_json::to_vec_pretty(&settings).map_err(io::Error::other)?;
    std::fs::write(&path, json)
}
