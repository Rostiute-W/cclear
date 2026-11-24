use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::guard::{self, DirHandle, Forbidden, GuardError, OpenIntent, path};
use crate::model::KnownRoot;
use crate::rules::{
    RootMap,
    schema::{self, RulePack, RulePackInfo},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    pub whitelist_paths: Vec<PathBuf>,
    pub schedule_reboot_delete_default: bool,
    pub min_temp_age_hours: u32,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            whitelist_paths: Vec::new(),
            schedule_reboot_delete_default: false,
            min_temp_age_hours: 24,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(flatten)]
    pub preferences: Preferences,
    pub rule_packs: Vec<RulePackInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppSettingsPatch {
    pub whitelist_paths: Option<Vec<String>>,
    pub schedule_reboot_delete_default: Option<bool>,
    pub min_temp_age_hours: Option<u32>,
}

pub fn data_dir(roots: &RootMap) -> Result<DirHandle, String> {
    let local = roots
        .get(KnownRoot::LocalAppData)
        .ok_or("无法解析本地应用数据目录")?;
    let parent = guard::open_anchor(local, &Forbidden::empty()).map_err(display)?;
    child_dir(&parent, "CClear")
}

pub fn child_dir(parent: &DirHandle, name: &str) -> Result<DirHandle, String> {
    let forbidden = Forbidden::empty();
    let target = parent.path().join(name);
    match guard::open_anchor(&target, &forbidden) {
        Ok(directory) => return Ok(directory),
        Err(GuardError::Gone) => {}
        Err(error) => return Err(display(error)),
    }
    if let Err(error) = guard::create_dir_in(parent, OsStr::new(name)) {
        return guard::open_anchor(&target, &forbidden).map_err(|_| display(error));
    }
    guard::open_anchor(&target, &forbidden).map_err(display)
}

pub fn load_preferences(roots: &RootMap) -> Result<Preferences, String> {
    let data = data_dir(roots)?;
    match read_limited(data.path(), "settings-v2.json", 128 * 1024) {
        Ok(text) => {
            let Ok(mut preferences) = serde_json::from_str::<Preferences>(&text) else {
                return Ok(Preferences::default());
            };
            preferences.whitelist_paths = preferences
                .whitelist_paths
                .iter()
                .map(|path| guard::validate_external_path(&path.to_string_lossy()).map_err(display))
                .collect::<Result<_, _>>()?;
            if !(24..=8760).contains(&preferences.min_temp_age_hours) {
                return Err("临时文件保留时长无效".into());
            }
            Ok(preferences)
        }
        Err(GuardError::Gone) => Ok(Preferences::default()),
        Err(GuardError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData => {
            Ok(Preferences::default())
        }
        Err(error) => Err(display(error)),
    }
}

pub fn get(roots: &RootMap) -> Result<AppSettings, String> {
    let preferences = load_preferences(roots)?;
    let mut rule_packs = vec![schema::builtin().info(true)];
    rule_packs.extend(external_packs(roots)?.iter().map(|pack| pack.info(false)));
    Ok(AppSettings {
        preferences,
        rule_packs,
    })
}

pub fn save(roots: &RootMap, patch: AppSettingsPatch) -> Result<AppSettings, String> {
    let mut preferences = load_preferences(roots)?;
    if let Some(paths) = patch.whitelist_paths {
        if paths.len() > 512 {
            return Err("白名单最多保存 512 个路径".into());
        }
        preferences.whitelist_paths.clear();
        for text in paths {
            let target = guard::validate_external_path(&text).map_err(display)?;
            let normalized = match std::fs::canonicalize(&target) {
                Ok(path) => guard::validate_external_path(
                    path.to_string_lossy().trim_start_matches(r"\\?\"),
                )
                .map_err(display)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => target,
                Err(error) => return Err(format!("无法检查白名单路径：{error}")),
            };
            if !preferences.whitelist_paths.iter().any(|path| {
                crate::rules::forbidden::same_or_under(&normalized, path)
                    && crate::rules::forbidden::same_or_under(path, &normalized)
            }) {
                preferences.whitelist_paths.push(normalized);
            }
        }
    }
    if let Some(default) = patch.schedule_reboot_delete_default {
        preferences.schedule_reboot_delete_default = default;
    }
    if let Some(hours) = patch.min_temp_age_hours {
        if !(24..=8760).contains(&hours) {
            return Err("临时文件保留时长必须在 24 至 8760 小时之间".into());
        }
        preferences.min_temp_age_hours = hours;
    }
    let data = data_dir(roots)?;
    let text = serde_json::to_vec_pretty(&preferences)
        .map_err(|error| format!("无法保存设置：{error}"))?;
    write_atomic(&data, "settings-v2.json", &text)?;
    get(roots)
}

pub fn external_packs(roots: &RootMap) -> Result<Vec<RulePack>, String> {
    let directory = child_dir(&data_dir(roots)?, "rules")?;
    let entries = guard::list_dir(&directory, &[], &Forbidden::empty()).map_err(display)?;
    let mut packs = Vec::new();
    for entry in entries {
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        if entry.is_dir || entry.is_reparse() || !schema::valid_id(id) {
            continue;
        }
        if let Ok(text) = read_limited(directory.path(), name, schema::MAX_PACK_BYTES)
            && let Ok(pack) = schema::parse_external(&text, roots)
            && pack.id == id
        {
            packs.push(pack);
        }
    }
    packs.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(packs)
}

pub fn import_pack(roots: &RootMap, text: &str) -> Result<RulePackInfo, String> {
    let pack = schema::parse_external(text, roots)?;
    let dir = child_dir(&data_dir(roots)?, "rules")?;
    write_atomic(&dir, &format!("{}.json", pack.id), text.as_bytes())?;
    Ok(pack.info(false))
}

pub fn remove_pack(roots: &RootMap, id: &str) -> Result<(), String> {
    if !schema::valid_id(id) || id == schema::builtin().id {
        return Err("规则包 ID 无效或属于内置规则".into());
    }
    let dir = child_dir(&data_dir(roots)?, "rules")?;
    let entry = guard::open_rel(
        &dir,
        &[format!("{id}.json").into()],
        OpenIntent::Delete,
        &Forbidden::empty(),
    )
    .map_err(display)?;
    if entry.is_dir {
        return Err("规则包文件无效".into());
    }
    let expected = entry.expect();
    guard::delete(entry, &expected).map_err(display)
}

pub fn read_limited(directory: &Path, name: &str, limit: usize) -> Result<String, GuardError> {
    let file = path::read_file(directory, name, &Forbidden::empty())?;
    let mut text = String::new();
    file.take(limit as u64 + 1).read_to_string(&mut text)?;
    if text.len() > limit {
        return Err(std::io::Error::other("文件超过大小限制").into());
    }
    Ok(text)
}

fn write_atomic(directory: &DirHandle, final_name: &str, bytes: &[u8]) -> Result<(), String> {
    let temp = format!("{}.tmp", uuid::Uuid::new_v4());
    let mut file = guard::create_file_in(directory, OsStr::new(&temp)).map_err(display)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(display)?;
    drop(file);
    guard::replace_in_dir(directory, OsStr::new(&temp), OsStr::new(final_name)).map_err(display)
}

fn display(error: impl std::fmt::Display) -> String {
    format!("CClear 数据目录操作失败：{error}")
}
