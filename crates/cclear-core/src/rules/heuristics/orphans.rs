use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crate::exec::file_cleaner::{Cleaner, Filters, filetime_unix};
use crate::guard::path;
use crate::model::CleanSource;
use crate::rules::{
    RootMap,
    engine::{APP_ROOTS, Candidate, ordinary_children},
    forbidden::same_or_under,
};
use crate::win::{process::ProcInfo, registry::InstalledSoftware};

const PROTECTED: &str = "Microsoft|Windows|Packages|Programs|Temp|Comms|ConnectedDevicesPlatform|VirtualStore|CrashDumps|D3DSCache|Publishers|\
    Google|Mozilla|Apple|Adobe|Intel|AMD|NVIDIA|Qualcomm|Realtek|Broadcom|Synaptics|Dell|HP|Lenovo|ASUS|Acer|MSI|\
    Corsair|Logitech|Razer|SteelSeries|Samsung|Western Digital|Seagate|Brother|Canon|Epson|\
    Tencent|WeChat|Weixin|QQ|DingTalk|Alibaba|ByteDance|Feishu|Lark|Kingsoft|WPS|Baidu|Qihoo|360|NetEase|Huawei|Xiaomi|\
    Steam|Valve|Epic Games|Riot Games|Blizzard|Battle.net|Ubisoft|Electronic Arts|Rockstar Games|GOG|Roblox|\
    Docker|wsl|Lxss|Hyper-V|VMware|Oracle|VirtualBox|Python|pip|uv|Conda|Anaconda|Java|OpenJDK|JetBrains|\
    Rustup|Cargo|Go|npm|pnpm|Yarn|NuGet|dotnet|Gradle|GitHub|Git|\
    Discord|Slack|Spotify|Zoom|Teams|Notion|Obsidian|Code|Cursor|BraveSoftware|Opera Software|Vivaldi|\
    1Password|Bitwarden|KeePass|Dropbox|OneDrive|Box|iCloud|Syncthing|Signal|Telegram Desktop|\
    Autodesk|Unity|UnrealEngine|OBS Studio|VideoLAN|CClear|com.zhuanz.cclear";

pub fn detect(
    roots: &RootMap,
    cleaner: &Cleaner<'_>,
    installed: &[InstalledSoftware],
    processes: &[ProcInfo],
    modules: &[PathBuf],
    now: i64,
) -> Vec<Candidate> {
    let protected: Vec<_> = PROTECTED.split('|').map(normalize).collect();
    let mut evidence = Vec::new();
    for app in installed {
        for field in [
            &app.display_name,
            &app.publisher,
            &app.install_location,
            &app.uninstall_string,
        ] {
            let name = normalize(field);
            if !name.is_empty() {
                evidence.push(name);
            }
        }
    }
    for process in processes {
        evidence.push(normalize(process.name.trim_end_matches(".exe")));
    }
    let mut candidates = Vec::new();
    for root in APP_ROOTS.into_iter().filter_map(|root| roots.get(root)) {
        for directory in ordinary_children(root, cleaner.forbidden) {
            if cleaner.cancel.load(Ordering::Relaxed) {
                return candidates;
            }
            let display = directory.file_name().unwrap_or_default().to_string_lossy();
            let name = normalize(&display);
            if name.len() < 3
                || protected.iter().any(|vendor| name.contains(vendor))
                || evidence
                    .iter()
                    .any(|entry| entry.contains(&name) || entry.len() >= 4 && name.contains(entry))
                || processes
                    .iter()
                    .filter_map(|process| process.image_path.as_ref())
                    .any(|image| same_or_under(image, &directory))
                || modules.iter().any(|image| same_or_under(image, &directory))
                || cleaner.whitelisted(&directory)
                || cleaner
                    .whitelist
                    .iter()
                    .any(|excluded| same_or_under(excluded, &directory))
            {
                continue;
            }
            let Ok(entry) = path::inspect_path(&directory, cleaner.forbidden) else {
                continue;
            };
            let modified = filetime_unix(entry.mtime) as i64;
            if modified == 0 || now.saturating_sub(modified) <= 30 * 86_400 {
                continue;
            }
            let measure = cleaner.measure(std::slice::from_ref(&directory), &Filters::default());
            if measure.bytes < 10 * 1024 * 1024
                || path::protected_tree(&directory, cleaner.forbidden).is_err()
            {
                continue;
            }
            let name = format!("{display} 疑似卸载残留");
            let mut candidate = super::file_candidate(
                name,
                "长期未修改，未找到匹配的安装或运行记录。",
                CleanSource::HeuristicOrphan,
                vec![directory],
                measure,
            );
            candidate.item.last_modified_at = Some(modified);
            candidates.push(candidate);
        }
    }
    candidates
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
