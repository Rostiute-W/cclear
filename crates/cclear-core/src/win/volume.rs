//! 卷信息：固定盘的容量、文件系统与系统盘判定。

use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use windows::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
};
use windows::core::PCWSTR;

use super::{KnownFolder, dirinfo, known_folder, to_verbatim};

/// `GetDriveTypeW` 对本地固定盘返回的值。windows crate 把它放在 WindowsProgramming 模块，
/// 为了一个常量引入整个 feature 不划算，这里直接照 Win32 头文件的值。
const DRIVE_FIXED: u32 = 3;

/// 一个固定卷。
#[derive(Clone, Debug)]
pub struct VolumeInfo {
    pub root: PathBuf,
    pub label: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub is_system: bool,
}

/// 列出所有固定盘（不含移动盘、光驱和网络盘）。
pub fn list_fixed_volumes() -> Vec<VolumeInfo> {
    let system_root = system_volume_root();

    // 缓冲区按 26 个盘符各 4 个宽字符估，取 512 已远超实际需要。
    let mut buffer = vec![0u16; 512];
    // SAFETY: 缓冲区长度与实际传入的切片一致，函数不会写入超过该长度。
    let length = unsafe { GetLogicalDriveStringsW(Some(&mut buffer)) } as usize;
    if length == 0 {
        return Vec::new();
    }

    let mut volumes = Vec::new();
    let mut start = 0usize;
    // 返回的是一串以 NUL 结尾的根路径，最后一个 NUL 之后结束。
    while start < length {
        let end = match buffer[start..length].iter().position(|unit| *unit == 0) {
            Some(offset) => start + offset,
            None => break,
        };
        if end == start {
            break;
        }

        // 缓冲区里的根路径本身以 NUL 结尾，可以直接当作 C 字符串传入。
        let root_wide = PCWSTR(buffer[start..].as_ptr());
        let root_text = String::from_utf16_lossy(&buffer[start..end]);
        start = end + 1;

        // SAFETY: 指针指向缓冲区中一个以 NUL 结尾的根路径字符串。
        if unsafe { GetDriveTypeW(root_wide) } != DRIVE_FIXED {
            continue;
        }

        let mut available = 0u64;
        let mut total = 0u64;
        let mut free = 0u64;
        // SAFETY: 三个输出指针都指向本函数栈上的 u64，生命周期覆盖调用过程。
        let sized = unsafe {
            GetDiskFreeSpaceExW(
                root_wide,
                Some(&mut available as *mut u64),
                Some(&mut total as *mut u64),
                Some(&mut free as *mut u64),
            )
        };
        if sized.is_err() {
            continue;
        }

        // 卷标可以留空，文件系统名一定存在；两个缓冲区都以 NUL 结尾。
        let mut label = [0u16; 256];
        let mut fs_name = [0u16; 64];
        // SAFETY: 两个缓冲区长度与传入长度一致，根路径指针同上。
        let named = unsafe {
            GetVolumeInformationW(
                root_wide,
                Some(&mut label),
                None,
                None,
                None,
                Some(&mut fs_name),
            )
        };

        let label = if named.is_ok() {
            wide_to_string(&label)
        } else {
            String::new()
        };
        let fs = if named.is_ok() {
            wide_to_string(&fs_name)
        } else {
            String::new()
        };

        volumes.push(VolumeInfo {
            is_system: match &system_root {
                Some(system) => same_path(system, &root_text),
                None => false,
            },
            root: PathBuf::from(&root_text),
            label,
            fs,
            total,
            free,
        });
    }

    volumes
}

/// 查询路径所在卷的可用空间；查询失败按 0 处理，界面显示为不可用。
pub fn free_space(path: &Path) -> u64 {
    let mut wide: Vec<u16> = to_verbatim(path).as_os_str().encode_wide().collect();
    wide.push(0);

    let mut available = 0u64;
    // SAFETY: wide 是以 NUL 结尾的宽字符串，在调用期间存活。
    let queried = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut available as *mut u64),
            None,
            None,
        )
    };

    if queried.is_err() { 0 } else { available }
}

/// 任意路径所在卷的序列号。只看盘符根目录，路径本身不存在也能判断。
pub fn path_volume_serial(path: &Path) -> Option<u64> {
    let root = volume_root(path)?;
    let handle = dirinfo::open_dir(&root).ok()?;

    dirinfo::volume_serial(&handle).ok()
}

/// 系统盘（Windows 所在卷）的序列号。判断缓存是否在系统盘时按它比较。
pub fn system_volume_serial() -> Option<u64> {
    let windows = known_folder(KnownFolder::Windows).ok()?;

    path_volume_serial(&windows)
}

/// 路径所在的卷根，例如 `C:\`。相对路径没有盘符，返回 None。
fn volume_root(path: &Path) -> Option<PathBuf> {
    let Component::Prefix(prefix) = path.components().next()? else {
        return None;
    };

    let mut root = PathBuf::from(prefix.as_os_str());
    root.push("\\");

    Some(root)
}

/// 系统盘所在的卷根，例如 `C:\`。
fn system_volume_root() -> Option<String> {
    let windows = known_folder(KnownFolder::Windows).ok()?;
    // SHGetKnownFolderPath 返回的是完整目录，只需要第一个前缀组件（盘符）。
    let prefix = windows.components().next()?;
    let mut root = prefix.as_os_str().to_string_lossy().into_owned();
    root.push('\\');
    Some(root)
}

/// 按 Windows 的序号比较规则做大小写不敏感比较。
fn same_path(left: &str, right: &str) -> bool {
    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    // SAFETY: 两段宽字符串都在调用期间存活，长度由切片自身给出。
    unsafe { CompareStringOrdinal(&left, &right, true) == CSTR_EQUAL }
}

/// 把以 NUL 结尾的宽字符串缓冲区转成 String。
fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}
