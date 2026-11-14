//! 用 `ShellExecuteExW` 以 runas 启动当前 exe 的 helper 模式，并等它结束。

use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::thread;

use windows::Win32::Foundation::{ERROR_CANCELLED, HANDLE};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::PCWSTR;

use super::ElevationError;
use super::HelperResult;

/// `runas` 动词。UAC 取消时 `ShellExecuteExW` 返回 `ERROR_CANCELLED`。
const RUN_AS_VERB: &str = "runas";

/// 启动 helper 并等它结束，然后读结果文件。
///
/// 等待放在后台线程里：UAC 提示可能要等用户一段时间，不能占着调用方的线程发呆。
pub fn runas_and_wait(
    program: &Path,
    params: &[String],
    result_file: &Path,
) -> Result<HelperResult, ElevationError> {
    let program_wide = wide_path(program);
    let verb_wide = wide(RUN_AS_VERB);
    // 用户名可能带空格，--result 路径必须按 Windows 命令行规则加引号，否则会被拆成多个参数。
    let params_wide = wide(&command_line(params));

    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb_wide.as_ptr()),
        lpFile: PCWSTR(program_wide.as_ptr()),
        lpParameters: PCWSTR(params_wide.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    // SAFETY: 四个宽字符串都以 NUL 结尾且在调用期间存活；结构体按系统要求填好 cbSize，
    // 传入的是有效指针。成功后 hProcess 由本次调用返回，下面交给 OwnedHandle 关闭。
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
        // UAC 被取消时 HRESULT 里带的是 Win32 的 ERROR_CANCELLED，取低 16 位比较。
        let raw = error.code().0 as u32 & 0xFFFF;
        if raw == ERROR_CANCELLED.0 {
            return Err(ElevationError::Cancelled);
        }
        return Err(ElevationError::Launch(error.to_string()));
    }

    if info.hProcess.is_invalid() {
        return Err(ElevationError::Launch(
            "没有拿到 helper 的进程句柄".to_string(),
        ));
    }

    // SAFETY: 句柄由 ShellExecuteExW 成功返回，所有权转交给 OwnedHandle，由它负责关闭。
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) };

    let waiter = thread::spawn(move || {
        // SAFETY: 句柄由 OwnedHandle 持有，等待期间一直有效；只等待，不结束进程。
        unsafe {
            WaitForSingleObject(HANDLE(process.as_raw_handle()), INFINITE);
            let mut code = 0u32;
            let _ = GetExitCodeProcess(HANDLE(process.as_raw_handle()), &mut code);
            code
        }
    });

    let exit_code = waiter
        .join()
        .map_err(|_| ElevationError::Launch("等待 helper 结束的线程异常退出".to_string()))?;

    let text = std::fs::read_to_string(result_file)
        .map_err(|_| ElevationError::MissingResult { exit_code })?;

    serde_json::from_str(&text).map_err(|error| ElevationError::Result(error.to_string()))
}

/// 把 helper 参数拼成 `lpParameters` 字符串。
///
/// `<LocalAppData>` 位于 `C:\Users\<用户名>\...`，用户名带空格时路径也带空格，
/// 必须按 `CommandLineToArgvW` 的规则加引号和转义，否则 helper 会收到被拆开的参数。
pub fn command_line(params: &[String]) -> String {
    params
        .iter()
        .map(|param| quote_arg(param))
        .collect::<Vec<String>>()
        .join(" ")
}

/// 按 Windows 命令行规则给一个参数加引号。规则同 `CommandLineToArgvW`：
/// 需要时用双引号包住；引号前的连续反斜杠加倍并补一个转义反斜杠；
/// 结尾的连续反斜杠加倍，避免把收尾引号转义掉。
fn quote_arg(arg: &str) -> String {
    let needs_quote = arg.is_empty() || arg.contains([' ', '\t', '"']);
    if !needs_quote {
        return arg.to_string();
    }

    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');

    let mut backslashes = 0usize;
    for character in arg.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                // 反斜杠先翻倍，再补一个转义引号用的反斜杠。
                for _ in 0..backslashes * 2 + 1 {
                    quoted.push('\\');
                }
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    quoted.push('\\');
                }
                backslashes = 0;
                quoted.push(character);
            }
        }
    }

    // 收尾引号前的反斜杠要翻倍，否则引号会被当成被转义的字符。
    for _ in 0..backslashes * 2 {
        quoted.push('\\');
    }
    quoted.push('"');

    quoted
}

/// 加 NUL 结尾的宽字符串。
fn wide(text: &str) -> Vec<u16> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);

    wide
}

/// 路径转成加 NUL 结尾的宽字符串。
fn wide_path(path: &Path) -> Vec<u16> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);

    wide
}
