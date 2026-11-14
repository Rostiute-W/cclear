//! 禁区路径。
//!
//! 禁区只禁止执行和迁移，分析时仍然可以显示。§4 里的“云占位文件”“运行中进程加载的模块文件”
//! 和“独立应用安装根”不是固定路径，由执行阶段的路径护栏负责判断，这里只覆盖按路径就能确定的项。

use std::path::{Path, PathBuf};

use crate::model::KnownRoot;
use crate::rules::resolve::RootMap;
use crate::win::{KnownFolder, equals_ignore_case, known_folder};

/// 路径组件命中即视为禁区。任意位置的同名目录都算，不只是路径末尾。
pub fn forbidden_names() -> &'static [&'static str] {
    &[
        "db_storage",
        "msg",
        "nt_db",
        "auth",
        "security",
        "IndexedDB",
        "Local Storage",
        "WPS Cloud Files",
    ]
}

/// 单个组件名是否属于禁区。枚举目录时用它把整棵禁区子目录跳过去。
pub fn is_forbidden_name(name: &str) -> bool {
    forbidden_names()
        .iter()
        .any(|forbidden| equals_ignore_case(name, forbidden))
}

/// 整棵子树都禁止的已知根目录。
///
/// Program Files、Program Files (x86) 和 OneDrive 不在 `KnownRoot` 中，由 `is_forbidden` 直接解析。
pub fn forbidden_roots() -> Vec<(KnownRoot, &'static str)> {
    vec![(KnownRoot::Windows, "Windows 目录整体禁止")]
}

/// 路径中任意位置出现这段连续组件，整棵子树都禁止。
///
/// 不能只比末尾：`office6\backup` 下面还有多层子目录，只比末尾会漏判。
const FORBIDDEN_SEGMENTS: &[(&[&str], &str)] = &[(&["Kingsoft", "office6", "backup"], "WPS 备份")];

/// 执行阶段的路径护栏要按文件身份拦住的禁区目录。
///
/// 只按路径字符串比较会被 8.3 短名和大小写差异绕过，护栏需要先打开这些目录取身份。
pub fn forbidden_dirs(roots: &RootMap) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    for (root, _) in forbidden_roots() {
        if let Some(base) = roots.get(root) {
            dirs.push(base.to_path_buf());
        }
    }

    for folder in [
        KnownFolder::ProgramFiles,
        KnownFolder::ProgramFilesX86,
        KnownFolder::SkyDrive,
    ] {
        if let Ok(base) = known_folder(folder) {
            dirs.push(base);
        }
    }

    if let Some(base) = roots.get(KnownRoot::LocalAppData) {
        dirs.push(base.join("Packages"));
        dirs.push(base.join("CClear"));
        // Tauri/WebView2 按 tauri.conf.json 里的 identifier 建数据目录。
        dirs.push(base.join("com.zhuanz.cclear"));
    }

    // 连续组件型禁区可能挂在多个根下，能定位到的都列出来。
    for (segments, _) in FORBIDDEN_SEGMENTS {
        for root in [
            KnownRoot::RoamingAppData,
            KnownRoot::LocalAppData,
            KnownRoot::ProgramData,
        ] {
            let Some(base) = roots.get(root) else {
                continue;
            };

            let mut path = base.to_path_buf();
            for segment in *segments {
                path.push(segment);
            }
            dirs.push(path);
        }
    }

    dirs
}

/// 判断路径是否落在禁区，命中时返回中文原因。
pub fn is_forbidden(path: &Path, roots: &RootMap) -> Option<String> {
    // 组件名禁区优先：这类目录名本身就代表聊天记录或凭据，出现在任何位置都不能碰。
    for component in path_components(path) {
        if is_forbidden_name(&component) {
            return Some(format!("路径中的「{component}」属于禁区"));
        }
    }

    for (root, reason) in forbidden_roots() {
        if let Some(base) = roots.get(root)
            && starts_with(path, base)
        {
            return Some(reason.to_string());
        }
    }

    for folder in [KnownFolder::ProgramFiles, KnownFolder::ProgramFilesX86] {
        if let Ok(base) = known_folder(folder)
            && starts_with(path, &base)
        {
            return Some("Program Files 目录整体禁止".to_string());
        }
    }

    // OneDrive 根目录：解析失败时忽略这一项，不影响其他判断。
    if let Ok(base) = known_folder(KnownFolder::SkyDrive)
        && starts_with(path, &base)
    {
        return Some("OneDrive 根目录禁止".to_string());
    }

    if let Some(base) = roots.get(KnownRoot::LocalAppData) {
        if starts_with(path, &base.join("Packages")) {
            return Some("应用包目录 Local\\Packages 禁止".to_string());
        }
        if starts_with(path, &base.join("CClear")) {
            return Some("CClear 自身数据禁止".to_string());
        }
        // Tauri/WebView2 按 tauri.conf.json 里的 identifier 建数据目录，同样不能碰。
        if starts_with(path, &base.join("com.zhuanz.cclear")) {
            return Some("CClear 自身数据禁止".to_string());
        }
    }

    for (segments, reason) in FORBIDDEN_SEGMENTS {
        if contains_components(path, segments) {
            return Some(reason.to_string());
        }
    }

    None
}

fn path_components(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// 路径是否在 base 之下（含 base 自身），逐组件不区分大小写比较。
fn starts_with(path: &Path, base: &Path) -> bool {
    let path = path_components(path);
    let base = path_components(base);

    path.len() >= base.len()
        && base
            .iter()
            .zip(path.iter())
            .all(|(left, right)| equals_ignore_case(left, right))
}

/// 路径中是否有一处连续的组件与 segments 完全一致。
fn contains_components(path: &Path, segments: &[&str]) -> bool {
    let path = path_components(path);

    path.windows(segments.len()).any(|window| {
        segments
            .iter()
            .zip(window.iter())
            .all(|(left, right)| equals_ignore_case(left, right))
    })
}
