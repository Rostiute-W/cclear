//! 对 Win32 的薄封装。

pub mod acl;
pub mod dirinfo;
pub mod dpapi;
pub mod fileversion;
pub mod http;
pub mod process;
pub mod registry;
pub mod restart_manager;
pub mod volume;

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_WRITE_ATTRIBUTES, OPEN_EXISTING, SetFileTime,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData,
    FOLDERID_LocalAppDataLow, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile,
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, FOLDERID_RoamingAppData,
    FOLDERID_SavedGames, FOLDERID_SkyDrive, FOLDERID_Videos, FOLDERID_Windows, KNOWN_FOLDER_FLAG,
    SHGetKnownFolderPath,
};
use windows::core::{GUID, PCWSTR};

/// 需要解析的已知文件夹。
#[derive(Clone, Copy, Debug)]
pub enum KnownFolder {
    LocalAppData,
    LocalAppDataLow,
    RoamingAppData,
    Profile,
    Desktop,
    Documents,
    Downloads,
    Music,
    Pictures,
    SavedGames,
    Videos,
    ProgramData,
    Windows,
    ProgramFiles,
    ProgramFilesX86,
    SkyDrive,
}

impl KnownFolder {
    fn folder_id(self) -> GUID {
        match self {
            KnownFolder::LocalAppData => FOLDERID_LocalAppData,
            KnownFolder::LocalAppDataLow => FOLDERID_LocalAppDataLow,
            KnownFolder::RoamingAppData => FOLDERID_RoamingAppData,
            KnownFolder::Profile => FOLDERID_Profile,
            KnownFolder::Desktop => FOLDERID_Desktop,
            KnownFolder::Documents => FOLDERID_Documents,
            KnownFolder::Downloads => FOLDERID_Downloads,
            KnownFolder::Music => FOLDERID_Music,
            KnownFolder::Pictures => FOLDERID_Pictures,
            KnownFolder::SavedGames => FOLDERID_SavedGames,
            KnownFolder::Videos => FOLDERID_Videos,
            KnownFolder::ProgramData => FOLDERID_ProgramData,
            KnownFolder::Windows => FOLDERID_Windows,
            KnownFolder::ProgramFiles => FOLDERID_ProgramFiles,
            KnownFolder::ProgramFilesX86 => FOLDERID_ProgramFilesX86,
            KnownFolder::SkyDrive => FOLDERID_SkyDrive,
        }
    }
}

/// 通过 `SHGetKnownFolderPath` 解析用户目录，避免读取环境变量被伪造。
pub fn known_folder(id: KnownFolder) -> windows::core::Result<PathBuf> {
    // SAFETY: 传入的 GUID 是进程内常量，返回的宽字符串由系统分配，这里用 CoTaskMemFree 释放。
    unsafe {
        let raw = SHGetKnownFolderPath(&id.folder_id(), KNOWN_FOLDER_FLAG(0), None)?;
        let path = PathBuf::from(String::from_utf16_lossy(raw.as_wide()));
        CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
        Ok(path)
    }
}

/// 当前进程是否以管理员权限运行。
pub fn is_elevated() -> bool {
    // SAFETY: 只查询当前进程自身的令牌，句柄在返回前关闭，缓冲区大小与结构体一致。
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let queried = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .is_ok();

        let _ = CloseHandle(token);
        queried && elevation.TokenIsElevated != 0
    }
}

/// 转成 `\\?\` 长路径形式。Win32 的宽字符接口只有在去掉 `MAX_PATH` 限制后才能处理超长路径。
pub fn to_verbatim(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();

    // 已经是长路径形式时原样返回，避免叠加前缀。
    if text.starts_with(r"\\?\") {
        return path.to_path_buf();
    }

    // UNC 路径必须用 \\?\UNC\ 前缀，不能直接拼 \\?\。
    if let Some(rest) = text.strip_prefix(r"\\") {
        return PathBuf::from(format!(r"\\?\UNC\{rest}"));
    }

    PathBuf::from(format!(r"\\?\{text}"))
}

/// 按系统的有序比较规则（`CompareStringOrdinal`）判断两个字符串是否相同，不区分大小写。
///
/// 路径比较不能用 `to_lowercase`：部分语言的转换结果和系统比较规则不一致。
pub fn equals_ignore_case(left: &str, right: &str) -> bool {
    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();

    // SAFETY: 两个切片都以各自长度为准且在调用期间存活，第三个参数要求不区分大小写。
    unsafe { CompareStringOrdinal(&left, &right, true) == CSTR_EQUAL }
}

/// 同 `equals_ignore_case`，但逐字符比较，供通配符匹配使用。
pub fn char_equals_ignore_case(left: char, right: char) -> bool {
    let mut left_buffer = [0u16; 2];
    let mut right_buffer = [0u16; 2];
    let left_length = left.encode_utf16(&mut left_buffer).len();
    let right_length = right.encode_utf16(&mut right_buffer).len();

    // SAFETY: 缓冲区长度取自 encode_utf16 的返回值，调用期间局部数组一直存活。
    unsafe {
        CompareStringOrdinal(
            &left_buffer[..left_length],
            &right_buffer[..right_length],
            true,
        ) == CSTR_EQUAL
    }
}

/// 把文件的时间戳对齐到给定的 FILETIME（1601 年起的 100 纳秒）。
///
/// `std::fs::copy` 在 Windows 上不保证保留源文件的创建时间，隔离区要能原样还原，
/// 所以复制之后统一补设一次：创建时间和最后写入时间都按源文件写。
pub fn set_file_times(path: &Path, creation: u64, last_write: u64) -> io::Result<()> {
    let mut wide: Vec<u16> = to_verbatim(path).as_os_str().encode_wide().collect();
    wide.push(0);

    // SAFETY: wide 以 NUL 结尾且在调用期间存活；不传安全属性和模板句柄；
    // 共享模式放开读/写/删，避免别的程序正在用这个文件时打不开。
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_WRITE_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(io::Error::from)?;

    let creation_time = to_filetime(creation);
    let last_write_time = to_filetime(last_write);

    // SAFETY: 两个时间结构体都在调用期间存活；访问时间传 None 表示不改。
    let result = unsafe { SetFileTime(handle, Some(&creation_time), None, Some(&last_write_time)) };

    // SAFETY: handle 由上面的 CreateFileW 成功返回，这里关闭一次。
    unsafe {
        let _ = CloseHandle(handle);
    }

    result.map_err(io::Error::from)
}

/// 把 FILETIME 的 64 位整数拆成 Win32 结构体。
fn to_filetime(ticks: u64) -> FILETIME {
    FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    }
}
