//! 禁区路径。
//!
//! 禁区只禁止执行和迁移，分析时仍然可以显示。§4 里的“云占位文件”“运行中进程加载的模块文件”
//! 和“独立应用安装根”不是固定路径，由执行阶段的路径护栏负责判断，这里只覆盖按路径就能确定的项。

use std::path::Path;

use crate::model::KnownRoot;
use crate::rules::resolve::RootMap;
use crate::win::{KnownFolder, equals_ignore_case, known_folder};

/// 路径组件命中即视为禁区。
pub fn forbidden_names() -> &'static [&'static str] {
    &[
        "db_storage",
        "msg",
        "nt_db",
        "IndexedDB",
        "Local Storage",
        "WPS Cloud Files",
    ]
}

/// 整棵子树都禁止的已知根目录。
///
/// Program Files、Program Files (x86) 和 OneDrive 不在 `KnownRoot` 中，由 `is_forbidden` 直接解析。
pub fn forbidden_roots() -> Vec<(KnownRoot, &'static str)> {
    vec![(KnownRoot::Windows, "Windows 目录整体禁止")]
}

/// 末尾组件必须完全匹配的具体禁止路径。
const FORBIDDEN_TAILS: &[(&[&str], &str)] = &[
    (&["QQ", "auth"], "QQ 登录凭据"),
    (&["QQ", "security"], "QQ 安全数据"),
    (&["Kingsoft", "office6", "backup"], "WPS 备份"),
];

/// 判断路径是否落在禁区，命中时返回中文原因。
pub fn is_forbidden(path: &Path, roots: &RootMap) -> Option<String> {
    // 组件名禁区优先：这类目录名本身就代表聊天记录或凭据，出现在任何位置都不能碰。
    for component in path_components(path) {
        for name in forbidden_names() {
            if equals_ignore_case(&component, name) {
                return Some(format!("路径中的「{name}」属于禁区"));
            }
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
    }

    for (tail, reason) in FORBIDDEN_TAILS {
        if ends_with(path, tail) {
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

/// 路径末尾的组件是否与 tail 完全一致。
fn ends_with(path: &Path, tail: &[&str]) -> bool {
    let path = path_components(path);
    if path.len() < tail.len() {
        return false;
    }

    let actual = &path[path.len() - tail.len()..];
    tail.iter()
        .zip(actual.iter())
        .all(|(left, right)| equals_ignore_case(left, right))
}
