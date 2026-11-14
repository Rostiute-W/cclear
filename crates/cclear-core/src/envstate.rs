//! 环境状态卡：休眠开关、页面文件位置、WSL 发行版、Known Folder 重定向和 OneDrive。
//!
//! 全部只读。读不到的项留空（None），界面显示“未知”，不因为一项失败影响其他项。

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::win::registry::{self, RegistryRoot};
use crate::win::{KnownFolder, equals_ignore_case, known_folder};

/// 休眠开关所在的注册表位置。
const POWER_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Power";
/// 页面文件配置所在的注册表位置。
const MEMORY_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management";
/// 当前用户的 WSL 发行版注册项。
const LXSS_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

/// 环境状态卡的内容。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvState {
    /// 休眠是否开启。注册表里没有这个值时为 None。
    pub hibernate_enabled: Option<bool>,
    /// 页面文件位置。None 表示读不到配置。
    pub pagefiles: Option<Vec<Location>>,
    /// WSL 发行版。None 表示读不到注册表。
    pub wsl_distros: Option<Vec<Location>>,
    /// 五个用户目录的位置，顺序固定。
    pub known_folders: Vec<Location>,
    /// OneDrive 位置。没有配置时是 None。
    pub onedrive: Option<Location>,
}

/// 一个位置项：显示名、路径，以及它是否在系统盘（也就是 C 盘）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// 显示名。页面文件和 OneDrive 这类靠分组标题说明的项为空。
    pub name: Option<String>,
    /// 路径。读不到时为 None。
    pub path: Option<String>,
    /// true 表示在 C 盘，false 表示已在其他盘，None 表示判断不了。
    pub on_system_drive: Option<bool>,
}

/// 读取环境状态卡的全部内容。
pub fn env_state() -> EnvState {
    let system_drive = system_drive();

    EnvState {
        hibernate_enabled: registry::read_dword(
            RegistryRoot::LocalMachine,
            POWER_KEY,
            "HibernateEnabled",
        )
        .map(|value| value != 0),
        pagefiles: registry::read_multi_string(
            RegistryRoot::LocalMachine,
            MEMORY_KEY,
            "PagingFiles",
        )
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| pagefile_location(entry, system_drive.as_deref()))
                .collect()
        }),
        wsl_distros: wsl_distros(system_drive.as_deref()),
        known_folders: known_folder_locations(system_drive.as_deref()),
        onedrive: onedrive_location(system_drive.as_deref()),
    }
}

/// 系统盘取 Windows 目录所在的卷根，Windows 装在别的盘时判断也正确。
fn system_drive() -> Option<PathBuf> {
    let windows = known_folder(KnownFolder::Windows).ok()?;
    windows.parent().map(Path::to_path_buf)
}

/// `PagingFiles` 的每一项形如 `G:\pagefile.sys 0 0`，只取路径部分。
fn pagefile_location(entry: &str, system_drive: Option<&Path>) -> Option<Location> {
    let path = entry.split_whitespace().next()?;

    Some(location(None, Path::new(path), system_drive))
}

/// 按名称和路径组装一个位置项。
fn location(name: Option<&str>, path: &Path, system_drive: Option<&Path>) -> Location {
    Location {
        name: name.map(str::to_string),
        path: Some(path.to_string_lossy().into_owned()),
        on_system_drive: on_system_drive(path, system_drive),
    }
}

/// 路径是否在系统盘。路径没有盘符（例如 `?:\pagefile.sys`）时判断不了。
fn on_system_drive(path: &Path, system_drive: Option<&Path>) -> Option<bool> {
    let root = volume_root(path)?;
    let system_root = volume_root(system_drive?)?;

    Some(equals_ignore_case(&root, &system_root))
}

/// 取路径的卷根（形如 `C:`）。不是盘符路径时返回 None。
fn volume_root(path: &Path) -> Option<String> {
    match path.components().next()? {
        Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().into_owned()),
        _ => None,
    }
}

/// 五个用户目录的位置。单个目录解析失败时该项路径为空，其余照常显示。
fn known_folder_locations(system_drive: Option<&Path>) -> Vec<Location> {
    const FOLDERS: &[(&str, KnownFolder)] = &[
        ("桌面", KnownFolder::Desktop),
        ("文档", KnownFolder::Documents),
        ("下载", KnownFolder::Downloads),
        ("图片", KnownFolder::Pictures),
        ("视频", KnownFolder::Videos),
    ];

    FOLDERS
        .iter()
        .map(|(name, folder)| match known_folder(*folder) {
            Ok(path) => location(Some(*name), &path, system_drive),
            Err(_) => Location {
                name: Some(name.to_string()),
                path: None,
                on_system_drive: None,
            },
        })
        .collect()
}

/// 当前用户注册的 WSL 发行版。读不到注册表时返回 None。
fn wsl_distros(system_drive: Option<&Path>) -> Option<Vec<Location>> {
    let subkeys = registry::subkeys(RegistryRoot::CurrentUser, LXSS_KEY)?;
    let mut distros = Vec::new();

    for subkey in subkeys {
        let key = format!("{LXSS_KEY}\\{subkey}");
        // 发行版名读不到时用注册项 id 顶替，至少还能看到它的位置。
        let name = registry::read_string(RegistryRoot::CurrentUser, &key, "DistributionName")
            .unwrap_or(subkey);

        let distro = match registry::read_string(RegistryRoot::CurrentUser, &key, "BasePath") {
            Some(base) => location(Some(&name), Path::new(&base), system_drive),
            None => Location {
                name: Some(name),
                path: None,
                on_system_drive: None,
            },
        };
        distros.push(distro);
    }

    Some(distros)
}

/// OneDrive 位置。没装 OneDrive 时系统仍可能给出路径，目录不存在就当作没配置。
fn onedrive_location(system_drive: Option<&Path>) -> Option<Location> {
    let path = known_folder(KnownFolder::SkyDrive).ok()?;
    if !path.is_dir() {
        return None;
    }

    Some(location(None, &path, system_drive))
}
