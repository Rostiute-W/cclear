//! 隔离区卷的选择：排除系统盘、源盘和 OneDrive 所在盘。

use std::path::{Path, PathBuf};

use crate::win::volume::{self, VolumeInfo};
use crate::win::{KnownFolder, equals_ignore_case, known_folder};

/// 隔离根目录的固定名字，放在用户选的卷根下。
pub const QUARANTINE_DIR_NAME: &str = "CClearQuarantine";

/// 可以作为隔离区的卷：本地固定 NTFS 盘，排除系统盘、源盘和 OneDrive 所在盘。
///
/// 返回值按剩余空间从大到小排序，第一个就是默认选择。
pub fn candidate_volumes(source_volume: &Path) -> Vec<VolumeInfo> {
    let source_serial = volume::path_volume_serial(source_volume);
    // OneDrive 目录若被重定向到别的卷，那个卷上就有同步中的文件，不适合放隔离区。
    let onedrive_serial = known_folder(KnownFolder::SkyDrive)
        .ok()
        .and_then(|path| volume::path_volume_serial(&path));

    let mut candidates: Vec<VolumeInfo> = volume::list_fixed_volumes()
        .into_iter()
        .filter(|info| is_usable(info, source_serial, onedrive_serial))
        .collect();

    candidates.sort_by_key(|info| std::cmp::Reverse(info.free));
    candidates
}

/// 一个卷能不能放隔离区。
fn is_usable(info: &VolumeInfo, source_serial: Option<u64>, onedrive_serial: Option<u64>) -> bool {
    // 只有 NTFS 才有隔离区依赖的 DACL、ADS 和文件 ID 语义。
    if !equals_ignore_case(&info.fs, "NTFS") {
        return false;
    }
    if info.is_system {
        return false;
    }

    let serial = volume::path_volume_serial(&info.root);
    // 卷序列号读不到时无法确认不是源卷，按不可用处理。
    if serial.is_none() || serial == source_serial || serial == onedrive_serial {
        return false;
    }

    true
}

/// 默认隔离根目录：剩余空间最大的候选卷下的 `CClearQuarantine`。没有候选卷时返回 None。
pub fn default_root(candidates: &[VolumeInfo]) -> Option<PathBuf> {
    candidates
        .first()
        .map(|info| info.root.join(QUARANTINE_DIR_NAME))
}
