use std::path::{Path, PathBuf};

use crate::model::KnownRoot;
use crate::rules::RootMap;
use crate::win::equals_ignore_case;

pub const PROTECTED_NAMES: &[&str] = &[
    "System32",
    "SysWOW64",
    "WinSxS",
    "DriverStore",
    "WindowsApps",
    "System Volume Information",
    "$Recycle.Bin",
    "$MFT",
    "$LogFile",
    "$Boot",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
    "bootmgr",
    "bootstat.dat",
    "NTUSER.DAT",
    "UsrClass.dat",
    "Cookies",
    "Cookies-journal",
    "Login Data",
    "Login Data-journal",
    "Login Data For Account",
    "Bookmarks",
    "Bookmarks.bak",
    "History",
    "History-journal",
    "Web Data",
    "Local Storage",
    "Session Storage",
    "IndexedDB",
    "Partitions",
    "Service Worker",
    "cookies.sqlite",
    "places.sqlite",
    "logins.json",
    "key4.db",
    "LocalState",
    "RoamingState",
    "db_storage",
    "msg",
    "nt_db",
    "auth",
    "security",
    "Protect",
    "Credentials",
    "Crypto",
    "Vault",
    "SystemCertificates",
    ".ssh",
    ".gnupg",
    "WPS Cloud Files",
];

pub fn forbidden_paths(roots: &RootMap) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for root in [
        KnownRoot::ProgramFiles,
        KnownRoot::ProgramFilesX86,
        KnownRoot::OneDrive,
        KnownRoot::Documents,
        KnownRoot::Downloads,
    ] {
        paths.extend(roots.get(root).map(Path::to_path_buf));
    }
    for (root, relative_paths) in [
        (
            KnownRoot::Windows,
            &[
                "Installer",
                "servicing",
                "System",
                "SystemApps",
                "Boot",
                "Fonts",
                "explorer.exe",
                "regedit.exe",
            ][..],
        ),
        (
            KnownRoot::LocalAppData,
            &["CClear", "com.zhuanz.cclear", r"Kingsoft\office6\backup"],
        ),
        (KnownRoot::RoamingAppData, &[r"Kingsoft\office6\backup"]),
        (
            KnownRoot::Profile,
            &["OneDrive", "Desktop", "Pictures", "Music", "Videos"],
        ),
        (
            KnownRoot::SystemDrive,
            &[
                "Program Files",
                "Program Files (x86)",
                "Recovery",
                "Boot",
                "EFI",
            ],
        ),
    ] {
        if let Some(base) = roots.get(root) {
            paths.extend(relative_paths.iter().map(|relative| base.join(relative)));
        }
    }
    paths
}

pub fn same_or_under(path: &Path, base: &Path) -> bool {
    let mut path_parts = path.components();
    base.components().all(|base_part| {
        path_parts.next().is_some_and(|path_part| {
            let left = path_part.as_os_str().to_string_lossy();
            let right = base_part.as_os_str().to_string_lossy();
            equals_ignore_case(
                left.trim_start_matches(r"\\?\"),
                right.trim_start_matches(r"\\?\"),
            )
        })
    })
}
