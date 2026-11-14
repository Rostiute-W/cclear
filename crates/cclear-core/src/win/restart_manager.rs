//! Restart Manager 占用检测：按具体文件查出正在使用它的进程和服务。
//!
//! 出错的语义很关键：查询失败不等于没有占用，所以任何一步失败都会让 `complete` 变成 false，
//! 界面据此显示“检测不完整”，而不是“没有占用”。

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
use windows::Win32::System::RestartManager::{
    CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources,
    RmStartSession,
};
use windows::core::{PCWSTR, PWSTR};

use super::to_verbatim;

/// 每次注册的文件数。一次注册太多文件会让 Restart Manager 明显变慢，分批注册更稳。
const BATCH_SIZE: usize = 64;

/// 一个占用文件的进程，可能同时是某个服务。
#[derive(Clone, Debug)]
pub struct Locker {
    pub pid: u32,
    pub app_name: String,
    pub service: Option<String>,
}

/// 一次占用检测的结果。
#[derive(Clone, Debug)]
pub struct LockReport {
    /// 实际注册成功、参与本次查询的文件数。
    pub checked: usize,
    /// 需要检查的文件总数。`checked < total` 说明还有文件没查。
    pub total: usize,
    /// 是否完整检查了 `total` 个文件。
    pub complete: bool,
    pub lockers: Vec<Locker>,
    /// 检测不完整时的中文原因。
    pub error: Option<String>,
}

/// 查询这些文件的占用进程。
pub fn query_lockers(files: &[PathBuf]) -> LockReport {
    let mut report = LockReport {
        checked: 0,
        total: files.len(),
        complete: true,
        lockers: Vec::new(),
        error: None,
    };

    if files.is_empty() {
        return report;
    }

    // 宽字符串要在整个会话期间存活：系统只保存指针，不复制内容。
    let wide: Vec<Vec<u16>> = files.iter().map(|file| path_wide(file)).collect();
    let mut session = 0u32;
    let mut key = [0u16; (CCH_RM_SESSION_KEY + 1) as usize];

    // SAFETY: 会话句柄由 RmStartSession 写入，成功后在返回前用 RmEndSession 关闭；
    // 文件名的宽字符串切片在调用期间一直存活，指针不会被系统长期保存。
    unsafe {
        let started = RmStartSession(&mut session, None, PWSTR(key.as_mut_ptr()));
        if started != ERROR_SUCCESS {
            report.complete = false;
            report.error = Some(format!(
                "Restart Manager 无法启动会话（错误码 {}）",
                started.0
            ));
            return report;
        }

        for batch in wide.chunks(BATCH_SIZE) {
            let names: Vec<PCWSTR> = batch.iter().map(|text| PCWSTR(text.as_ptr())).collect();
            let registered = RmRegisterResources(session, Some(&names), None, None);
            if registered != ERROR_SUCCESS {
                report.complete = false;
                report.error = Some(format!(
                    "Restart Manager 注册文件失败（错误码 {}）",
                    registered.0
                ));
                // 注册失败的那一批文件没有被查过，后面的批次也不必再查：结果已经不完整。
                break;
            }
            report.checked += batch.len();
        }

        // 即使某一批注册失败也要读一次列表：已经注册成功的文件里查到的占用同样要报出来。
        match list_lockers(session) {
            Ok(lockers) => report.lockers = lockers,
            Err(reason) => {
                report.complete = false;
                // 注册失败是根本原因，优先保留它。
                if report.error.is_none() {
                    report.error = Some(reason);
                }
            }
        }

        let _ = RmEndSession(session);
    }

    report
}

/// 读取会话里的占用进程。缓冲区不够时按系统给出的数量扩容重试。
///
/// # Safety
/// `session` 必须是 `RmStartSession` 成功返回且尚未结束的会话句柄。
unsafe fn list_lockers(session: u32) -> Result<Vec<Locker>, String> {
    let mut needed = 0u32;
    let mut count = 0u32;
    let mut reboot_reasons = 0u32;

    // 第一次传空缓冲区，只为拿到实际需要的条目数。
    // SAFETY: 空缓冲区允许，系统只会写入需要的条目数。
    let listed = unsafe { RmGetList(session, &mut needed, &mut count, None, &mut reboot_reasons) };
    if listed == ERROR_SUCCESS && needed == 0 {
        return Ok(Vec::new());
    }
    if listed != ERROR_SUCCESS && listed != ERROR_MORE_DATA {
        return Err(format!(
            "Restart Manager 读取占用列表失败（错误码 {}）",
            listed.0
        ));
    }
    if needed == 0 {
        return Ok(Vec::new());
    }

    let mut entries = vec![RM_PROCESS_INFO::default(); needed as usize];
    let mut count = needed;
    // SAFETY: 缓冲区长度与传入的条目数一致，系统最多写入这么多条。
    let listed = unsafe {
        RmGetList(
            session,
            &mut needed,
            &mut count,
            Some(entries.as_mut_ptr()),
            &mut reboot_reasons,
        )
    };
    if listed != ERROR_SUCCESS {
        return Err(format!(
            "Restart Manager 读取占用列表失败（错误码 {}）",
            listed.0
        ));
    }

    let returned = count.min(entries.len() as u32) as usize;
    Ok(entries[..returned].iter().map(locker_of).collect())
}

/// 把系统结构体转成占用进程。服务名为空表示只是普通进程。
fn locker_of(entry: &RM_PROCESS_INFO) -> Locker {
    let service = fixed_string(&entry.strServiceShortName);

    Locker {
        pid: entry.Process.dwProcessId,
        app_name: fixed_string(&entry.strAppName),
        service: if service.is_empty() {
            None
        } else {
            Some(service)
        },
    }
}

/// 定长宽字符数组转字符串，遇到第一个 NUL 结束。
fn fixed_string(buffer: &[u16]) -> String {
    let length = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());

    OsString::from_wide(&buffer[..length])
        .to_string_lossy()
        .into_owned()
}

/// Restart Manager 要求长路径形式，否则超长路径会被截断。
fn path_wide(path: &Path) -> Vec<u16> {
    let mut wide: Vec<u16> = to_verbatim(path).as_os_str().encode_wide().collect();
    wide.push(0);
    wide
}
