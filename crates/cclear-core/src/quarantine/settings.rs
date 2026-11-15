//! 用户设置：隔离区根目录与 AI 咨询服务。

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::win::dpapi;
use crate::win::{KnownFolder, equals_ignore_case, known_folder};

const SETTINGS_DIR: &str = "CClear";
const SETTINGS_FILE: &str = "settings.json";

/// 加密 Key 的前缀。只有带这个前缀的内容才当作 Key，手工写进去的明文一律忽略。
const KEY_PREFIX: &str = "dpapi:";

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
    /// AI 咨询的三项必须和隔离区放在同一份结构里：两边都是整份读出再整份写回，
    /// 分成两个结构会互相把对方的字段写丢。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ai_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ai_model: Option<String>,
    /// DPAPI 密文，格式是 `dpapi:` 加小写十六进制。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ai_api_key: Option<String>,
}

/// Key 的保存方式。
pub enum KeyUpdate {
    /// 保留原来的 Key。
    Keep,
    /// 清除已经保存的 Key。
    Clear,
    /// 用新的明文 Key 覆盖。
    Set(String),
}

/// AI 咨询的设置。Key 只以密文形式放在这里，明文只在发请求时短暂解开。
pub struct AiSettings {
    pub base_url: Option<String>,
    pub model: Option<String>,
    key: Option<String>,
}

impl AiSettings {
    /// 有没有可用的 Key。界面只显示这个开关，不显示 Key 本身。
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    /// 解开 Key。没有配置时返回空字符串：本机服务（例如 Ollama）不需要 Key。
    pub fn api_key(&self) -> io::Result<String> {
        let Some(sealed) = &self.key else {
            return Ok(String::new());
        };

        let hex = sealed
            .strip_prefix(KEY_PREFIX)
            .ok_or_else(|| invalid_data("API Key 的保存格式不对"))?;
        let sealed = hex_decode(hex).ok_or_else(|| invalid_data("API Key 的保存格式不对"))?;
        let plain = dpapi::unprotect(&sealed)?;

        String::from_utf8(plain).map_err(|_| invalid_data("API Key 解密后不是文本"))
    }
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
    let mut settings = writable_settings();
    let text = root.to_string_lossy().into_owned();

    if !settings
        .quarantine_roots
        .iter()
        .any(|known| equals_ignore_case(known, &text))
    {
        settings.quarantine_roots.push(text.clone());
    }
    settings.quarantine_root = Some(text);

    write_settings(&settings)
}

/// 读 AI 咨询设置。Key 保持密文形式，只有 `AiSettings::api_key` 才会解开。
pub fn load_ai_settings() -> io::Result<AiSettings> {
    let settings = read_settings()?;

    // 只接受 dpapi: 前缀：手工改成明文、或是别的工具写进去的内容一律当作没有 Key。
    let key = settings
        .ai_api_key
        .filter(|text| text.starts_with(KEY_PREFIX));

    Ok(AiSettings {
        base_url: settings.ai_base_url,
        model: settings.ai_model,
        key,
    })
}

/// 保存 AI 咨询设置。隔离区字段原样保留。
pub fn save_ai_settings(base_url: &str, model: &str, api_key: KeyUpdate) -> io::Result<()> {
    let mut settings = writable_settings();
    settings.ai_base_url = Some(base_url.to_string());
    settings.ai_model = Some(model.to_string());

    match api_key {
        KeyUpdate::Keep => {}
        KeyUpdate::Clear => settings.ai_api_key = None,
        KeyUpdate::Set(plain) => {
            let sealed = dpapi::protect(plain.as_bytes())?;
            settings.ai_api_key = Some(format!("{KEY_PREFIX}{}", hex_encode(&sealed)));
        }
    }

    write_settings(&settings)
}

/// 读出准备写回的设置。读不出来就当成空设置：宁可丢掉旧的卷列表，
/// 也不能因为一行 JSON 坏了让用户改不了任何设置。
fn writable_settings() -> Settings {
    read_settings().unwrap_or_default()
}

fn write_settings(settings: &Settings) -> io::Result<()> {
    let path = settings_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let json = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
    std::fs::write(&path, json)
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// 小写十六进制编码。
fn hex_encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// 小写十六进制解码。长度是奇数或含非十六进制字符时返回 None。
fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }

    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        decoded.push((high * 16 + low) as u8);
    }

    Some(decoded)
}
