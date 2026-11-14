//! 隔离根目录的自保护 DACL。
//!
//! 隔离区里放的是从用户目录移出来的真实文件，只有 CClear 自己能读写才安全：
//! 根目录首次创建时把 DACL 收紧到 SYSTEM、Administrators 和当前用户，并标记为
//! 受保护（不再继承父目录的授权）。

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SetFileSecurityW, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PCWSTR};

/// 给隔离根目录设置受保护 DACL。
///
/// 对应的 SDDL 是 `D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;<当前用户 SID>)`：
/// `P` 表示受保护（不继承），`OICI` 让子目录和文件继承同样的权限。
pub fn protect_quarantine_root(path: &Path) -> windows::core::Result<()> {
    let sddl = format!(
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{})",
        current_user_sid()?
    );

    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: SDDL 以 HSTRING 传参并在调用期间存活；成功返回的描述符由系统分配，下面用 LocalFree 释放。
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(sddl),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )?;
    }

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);

    // SAFETY: wide 以 NUL 结尾且在调用期间存活；descriptor 是上面成功返回的有效描述符。
    let applied = unsafe {
        SetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };

    // SAFETY: 描述符由 ConvertStringSecurityDescriptorToSecurityDescriptorW 分配，这里释放一次。
    unsafe {
        LocalFree(Some(HLOCAL(descriptor.0)));
    }

    applied.ok()
}

/// 当前进程用户的 SID 字符串，形如 `S-1-5-21-...`。
fn current_user_sid() -> windows::core::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: 只查询当前进程自身的令牌；成功返回的句柄在下面关闭一次。
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
    }

    // 先问需要多大缓冲区，再按长度分配。SID 就在这块缓冲区里，取出字符串之前不能释放。
    let mut needed = 0u32;
    // SAFETY: 只查长度，不传缓冲区；这里的失败就是 ERROR_INSUFFICIENT_BUFFER，属于正常流程。
    unsafe {
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
    }

    let mut buffer = vec![0u8; needed as usize];
    let queried = {
        // SAFETY: 缓冲区长度与传入长度一致，令牌句柄有效，输出长度写回 needed。
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
                needed,
                &mut needed,
            )
        }
    };

    // SAFETY: token 是本函数打开的令牌句柄，这里关闭一次。
    unsafe {
        let _ = CloseHandle(token);
    }
    queried?;

    // SAFETY: 缓冲区里是系统写入的 TOKEN_USER，长度由 needed 给出，SID 指向缓冲区内部。
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };

    let mut text = windows::core::PWSTR::null();
    // SAFETY: SID 指针指向上面仍然存活的缓冲区；输出字符串由系统分配，下面用 LocalFree 释放。
    unsafe {
        ConvertSidToStringSidW(user.User.Sid, &mut text)?;
    }

    let sid = wide_ptr_to_string(text.0);

    // SAFETY: 字符串由 ConvertSidToStringSidW 分配，这里释放一次。
    unsafe {
        LocalFree(Some(HLOCAL(text.0 as *mut core::ffi::c_void)));
    }

    Ok(sid)
}

/// 读出以 NUL 结尾的宽字符串。
fn wide_ptr_to_string(pointer: *const u16) -> String {
    let mut length = 0usize;

    // SAFETY: 指针由系统返回且以 NUL 结尾，逐个字符找到结尾为止。
    unsafe {
        while *pointer.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length))
    }
}
