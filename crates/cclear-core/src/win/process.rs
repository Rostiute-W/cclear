//! 运行中进程枚举：规则的资格条件大多要求相关应用已经退出。

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

/// 进程映像路径的缓冲区长度。系统路径不会超过这个长度，超出的只保留进程名。
const IMAGE_PATH_CHARS: usize = 1024;

/// 一个正在运行的进程。
#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    /// 取不到时为 None：受保护进程和已退出进程都会查不到映像路径。
    pub image_path: Option<PathBuf>,
}

/// 枚举当前所有进程。拿不到快照时返回空列表，调用方按“不完整”处理。
pub fn running_processes() -> Vec<ProcInfo> {
    let mut processes = Vec::new();

    // SAFETY: 快照句柄由本次调用创建，在函数返回前关闭；枚举结构体按系统要求填好 dwSize。
    unsafe {
        let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(handle) => handle,
            Err(_) => return processes,
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name = process_name(&entry);
                if !name.is_empty() {
                    let pid = entry.th32ProcessID;
                    processes.push(ProcInfo {
                        pid,
                        name,
                        image_path: image_path(pid),
                    });
                }

                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
    }

    processes
}

/// `szExeFile` 是定长数组，遇到第一个 NUL 就是名字结尾。
fn process_name(entry: &PROCESSENTRY32W) -> String {
    let length = entry
        .szExeFile
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(entry.szExeFile.len());

    OsString::from_wide(&entry.szExeFile[..length])
        .to_string_lossy()
        .into_owned()
}

/// 查询进程映像的完整路径。受保护进程会因为打不开而返回 None。
fn image_path(pid: u32) -> Option<PathBuf> {
    // SAFETY: 进程句柄在查询后立即关闭；缓冲区指针与长度一致，成功时系统回写实际长度。
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buffer = [0u16; IMAGE_PATH_CHARS];
        let mut length = buffer.len() as u32;
        let queried = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
        .is_ok();

        let _ = CloseHandle(process);
        if !queried {
            return None;
        }

        let path = OsString::from_wide(&buffer[..length as usize]);
        Some(PathBuf::from(path))
    }
}
