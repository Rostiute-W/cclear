//! 对 Win32 的薄封装。

use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{
    FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData, FOLDERID_Profile,
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, FOLDERID_RoamingAppData,
    FOLDERID_Windows, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath,
};
use windows::core::GUID;

/// 需要解析的已知文件夹。
#[derive(Clone, Copy, Debug)]
pub enum KnownFolder {
    LocalAppData,
    RoamingAppData,
    Profile,
    Documents,
    Downloads,
    ProgramData,
    Windows,
    ProgramFiles,
    ProgramFilesX86,
}

impl KnownFolder {
    fn folder_id(self) -> GUID {
        match self {
            KnownFolder::LocalAppData => FOLDERID_LocalAppData,
            KnownFolder::RoamingAppData => FOLDERID_RoamingAppData,
            KnownFolder::Profile => FOLDERID_Profile,
            KnownFolder::Documents => FOLDERID_Documents,
            KnownFolder::Downloads => FOLDERID_Downloads,
            KnownFolder::ProgramData => FOLDERID_ProgramData,
            KnownFolder::Windows => FOLDERID_Windows,
            KnownFolder::ProgramFiles => FOLDERID_ProgramFiles,
            KnownFolder::ProgramFilesX86 => FOLDERID_ProgramFilesX86,
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
