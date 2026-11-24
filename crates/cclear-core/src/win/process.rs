//! 运行中进程枚举：规则的资格条件大多要求相关应用已经退出。

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, PROCESSENTRY32W,
    Process32FirstW, Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};
use windows::core::{BOOL, PWSTR};

pub fn is_protected(name: &str) -> bool {
    [
        "explorer.exe",
        "csrss.exe",
        "lsass.exe",
        "smss.exe",
        "winlogon.exe",
        "services.exe",
        "svchost.exe",
        "dwm.exe",
        "System",
        "System Idle Process",
        "wininit.exe",
        "lsaiso.exe",
        "Registry",
        "fontdrvhost.exe",
        "sihost.exe",
        "taskhostw.exe",
        "ctfmon.exe",
        "conhost.exe",
        "ShellExperienceHost.exe",
        "StartMenuExperienceHost.exe",
        "SearchHost.exe",
        "TextInputHost.exe",
        "RuntimeBroker.exe",
        "ApplicationFrameHost.exe",
        "SecurityHealthSystray.exe",
        "WindowsTerminal.exe",
        "OpenConsole.exe",
        "cclear.exe",
    ]
    .iter()
    .any(|protected| super::equals_ignore_case(name, protected))
}

pub fn close_processes_gracefully(names: &[String], timeout_ms: u64) -> (Vec<String>, Vec<String>) {
    let targets: Vec<_> = running_processes()
        .into_iter()
        .filter(|process| {
            process.image_path.is_some()
                && !is_protected(&process.name)
                && process.pid != std::process::id()
                && names
                    .iter()
                    .any(|name| super::equals_ignore_case(name, &process.name))
        })
        .collect();
    // SAFETY: 回调同步执行，参数指向调用期间存活的目标进程列表。
    let _ = unsafe {
        EnumWindows(
            Some(close_window),
            LPARAM((&targets as *const Vec<ProcInfo>) as isize),
        )
    };

    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.min(4000));
    loop {
        let current = running_processes();
        let (still_running, closed): (Vec<_>, Vec<_>) = names.iter().cloned().partition(|name| {
            is_protected(name)
                || current
                    .iter()
                    .any(|process| super::equals_ignore_case(name, &process.name))
        });
        if still_running.is_empty() || std::time::Instant::now() >= deadline {
            return (closed, still_running);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

unsafe extern "system" fn close_window(window: HWND, context: LPARAM) -> BOOL {
    // SAFETY: 窗口句柄来自同步枚举，系统检查句柄后返回可见状态。
    if !unsafe { IsWindowVisible(window) }.as_bool() {
        return BOOL(1);
    }
    let mut pid = 0;
    // SAFETY: 上下文来自同步枚举调用，输出指针指向本地进程 ID。
    let targets = unsafe { &*(context.0 as *const Vec<ProcInfo>) };
    // SAFETY: 窗口来自枚举器，进程 ID 写入本地变量。
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    if let Some(target) = targets.iter().find(|process| process.pid == pid)
        && image_path(pid).as_ref() == target.image_path.as_ref()
    {
        // SAFETY: 消息参数没有指针，系统负责检查窗口句柄和消息权限。
        let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    BOOL(1)
}

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

pub fn loaded_module_paths(processes: &[ProcInfo]) -> Vec<PathBuf> {
    let mut paths = std::collections::HashSet::new();
    for process in processes
        .iter()
        .filter(|process| process.image_path.is_some())
    {
        // SAFETY: 快照只读取给定进程的模块列表，句柄在枚举后关闭。
        unsafe {
            let Ok(snapshot) =
                CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, process.pid)
            else {
                continue;
            };
            let mut entry = MODULEENTRY32W {
                dwSize: size_of::<MODULEENTRY32W>() as u32,
                ..Default::default()
            };
            if Module32FirstW(snapshot, &mut entry).is_ok() {
                loop {
                    let name = entry
                        .szExePath
                        .split(|unit| *unit == 0)
                        .next()
                        .unwrap_or_default();
                    if !name.is_empty() {
                        paths.insert(PathBuf::from(OsString::from_wide(name)));
                    }
                    if Module32NextW(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);
        }
    }
    paths.into_iter().collect()
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
