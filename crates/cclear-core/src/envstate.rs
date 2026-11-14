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
    /// `LocalAppData\wsl\*` 下不属于任何已注册发行版的 vhdx。只展示，不做任何处理。
    pub unregistered_vhdx: Option<Vec<Location>>,
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
    /// 文件大小。只有按文件展示的项（未关联虚拟磁盘）才有。
    pub size_bytes: Option<u64>,
}

/// 读取环境状态卡的全部内容。
pub fn env_state() -> EnvState {
    let system_drive = system_drive();
    let wsl = wsl_entries();

    EnvState {
        hibernate_enabled: registry::read_dword(
            RegistryRoot::LocalMachine,
            POWER_KEY,
            "HibernateEnabled",
        )
        .map(|value| value != 0),
        pagefiles: pagefiles(system_drive.as_deref()),
        wsl_distros: wsl
            .as_ref()
            .map(|entries| wsl_locations(entries, system_drive.as_deref())),
        unregistered_vhdx: unregistered_vhdx(wsl.as_ref(), system_drive.as_deref()),
        known_folders: known_folder_locations(system_drive.as_deref()),
        onedrive: onedrive_location(system_drive.as_deref()),
    }
}

/// 系统盘取 Windows 目录所在的卷根，Windows 装在别的盘时判断也正确。
fn system_drive() -> Option<PathBuf> {
    let windows = known_folder(KnownFolder::Windows).ok()?;
    windows.parent().map(Path::to_path_buf)
}

/// 页面文件位置。
///
/// 系统自动管理时 `PagingFiles` 里只有 `?:` 这种占位，实际位置要从同键的 `ExistingPageFiles` 读。
fn pagefiles(system_drive: Option<&Path>) -> Option<Vec<Location>> {
    let entries =
        registry::read_multi_string(RegistryRoot::LocalMachine, MEMORY_KEY, "PagingFiles")?;

    let system_managed = entries.iter().any(|entry| entry.trim().starts_with("?:"));
    let paths = if system_managed {
        registry::read_multi_string(RegistryRoot::LocalMachine, MEMORY_KEY, "ExistingPageFiles")?
    } else {
        entries
    };

    Some(
        paths
            .iter()
            .filter_map(|entry| pagefile_location(entry, system_drive))
            .collect(),
    )
}

/// `PagingFiles` 的每一项形如 `G:\pagefile.sys 0 0`，只取路径部分。
fn pagefile_location(entry: &str, system_drive: Option<&Path>) -> Option<Location> {
    let path = entry.split_whitespace().next()?;

    Some(location(None, Path::new(path), system_drive, None))
}

/// 按名称和路径组装一个位置项。
fn location(
    name: Option<&str>,
    path: &Path,
    system_drive: Option<&Path>,
    size_bytes: Option<u64>,
) -> Location {
    Location {
        name: name.map(str::to_string),
        path: Some(display_path(path)),
        on_system_drive: on_system_drive(path, system_drive),
        size_bytes,
    }
}

/// 路径是否在系统盘。`X:\` 和 `\\?\X:\` 两种写法都按盘符比较。
fn on_system_drive(path: &Path, system_drive: Option<&Path>) -> Option<bool> {
    let root = volume_root(path)?;
    let system_root = volume_root(system_drive?)?;

    Some(equals_ignore_case(&root, &system_root))
}

/// 取路径的卷根（形如 `C:`）。不是盘符路径时返回 None。
fn volume_root(path: &Path) -> Option<String> {
    let normalized = normalize(path);

    match Path::new(&normalized).components().next()? {
        Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().into_owned()),
        _ => None,
    }
}

/// 去掉 `\\?\`、`\??\` 这类等价前缀后的路径文本，用于展示。
fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();

    text.strip_prefix(r"\\?\")
        .or_else(|| text.strip_prefix(r"\??\"))
        .unwrap_or(&text)
        .to_string()
}

/// 用于比较的规范化路径：去掉等价前缀和末尾分隔符。
fn normalize(path: &Path) -> String {
    display_path(path).trim_end_matches(['\\', '/']).to_string()
}

/// 两个路径是否指同一个目录（规范化后不区分大小写比较）。
fn same_path(left: &Path, right: &Path) -> bool {
    equals_ignore_case(&normalize(left), &normalize(right))
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
            Ok(path) => location(Some(*name), &path, system_drive, None),
            Err(_) => Location {
                name: Some(name.to_string()),
                path: None,
                on_system_drive: None,
                size_bytes: None,
            },
        })
        .collect()
}

/// 当前用户注册的 WSL 发行版：显示名和 BasePath。读不到注册表时返回 None。
fn wsl_entries() -> Option<Vec<(String, Option<String>)>> {
    let subkeys = registry::subkeys(RegistryRoot::CurrentUser, LXSS_KEY)?;
    let mut entries = Vec::new();

    for subkey in subkeys {
        let key = format!("{LXSS_KEY}\\{subkey}");
        // 发行版名读不到时用注册项 id 顶替，至少还能看到它的位置。
        let name = registry::read_string(RegistryRoot::CurrentUser, &key, "DistributionName")
            .unwrap_or(subkey);
        let base = registry::read_string(RegistryRoot::CurrentUser, &key, "BasePath");
        entries.push((name, base));
    }

    Some(entries)
}

/// 把 WSL 注册项转成位置项。
fn wsl_locations(
    entries: &[(String, Option<String>)],
    system_drive: Option<&Path>,
) -> Vec<Location> {
    entries
        .iter()
        .map(|(name, base)| match base {
            Some(path) => location(Some(name), Path::new(path), system_drive, None),
            None => Location {
                name: Some(name.clone()),
                path: None,
                on_system_drive: None,
                size_bytes: None,
            },
        })
        .collect()
}

/// `LocalAppData\wsl\*` 下不属于任何已注册发行版的 vhdx。
///
/// 已注册发行版的 vhdx 由 WSL 自己管理，列进来只会误导。
fn unregistered_vhdx(
    wsl: Option<&Vec<(String, Option<String>)>>,
    system_drive: Option<&Path>,
) -> Option<Vec<Location>> {
    let base = known_folder(KnownFolder::LocalAppData).ok()?.join("wsl");
    let registered: Vec<PathBuf> = wsl
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(_, path)| path.as_deref().map(PathBuf::from))
                .collect()
        })
        .unwrap_or_default();

    let mut disks = Vec::new();
    let Ok(dirs) = std::fs::read_dir(&base) else {
        // 没有 wsl 目录就当没有未关联的虚拟磁盘，不显示成“未知”。
        return Some(disks);
    };

    for dir in dirs.flatten() {
        if !dir.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }

        let dir_path = dir.path();
        if registered.iter().any(|base| same_path(&dir_path, base)) {
            continue;
        }

        let Ok(files) = std::fs::read_dir(&dir_path) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if !is_vhdx(&path) {
                continue;
            }
            let size = file.metadata().ok().map(|metadata| metadata.len());
            disks.push(location(None, &path, system_drive, size));
        }
    }

    disks.sort_by(|left, right| left.path.cmp(&right.path));
    Some(disks)
}

/// 扩展名是否为 vhdx（不区分大小写）。
fn is_vhdx(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| equals_ignore_case(&extension.to_string_lossy(), "vhdx"))
}

/// OneDrive 位置。没装 OneDrive 时系统仍可能给出路径，目录不存在就当作没配置。
fn onedrive_location(system_drive: Option<&Path>) -> Option<Location> {
    let path = known_folder(KnownFolder::SkyDrive).ok()?;
    if !path.is_dir() {
        return None;
    }

    Some(location(None, &path, system_drive, None))
}
