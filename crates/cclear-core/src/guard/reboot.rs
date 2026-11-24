use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, INHERIT_ONLY_ACE,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};
use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows::core::{PCWSTR, PWSTR};

use super::{Forbidden, GuardError, path};

pub fn schedule_delete_on_reboot(target: &Path, forbidden: &Forbidden) -> Result<(), GuardError> {
    if !crate::win::is_elevated() {
        return Err(GuardError::Denied);
    }
    let target = super::validate_external_path(target.to_str().ok_or(GuardError::InvalidName)?)?;
    path::with_stable_file(&target, forbidden, || {
        for (index, ancestor) in target
            .ancestors()
            .filter(|ancestor| ancestor.has_root())
            .enumerate()
        {
            if !trusted_permissions(ancestor, index == 0) {
                return Err(GuardError::Denied);
            }
        }
        let full = crate::win::to_verbatim(&target);
        let wide: Vec<_> = full.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: 路径通过了重解析点、文件身份及权限检查，宽字符串在调用期间有效。
        unsafe {
            MoveFileExW(
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                MOVEFILE_DELAY_UNTIL_REBOOT,
            )
        }
        .map_err(|error| std::io::Error::from(error).into())
    })?
}

fn trusted_permissions(target: &Path, is_file: bool) -> bool {
    let full = crate::win::to_verbatim(target);
    let wide: Vec<_> = full.as_os_str().encode_wide().chain([0]).collect();
    let mut owner = PSID::default();
    let mut acl: *mut ACL = std::ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: 输出指针在栈上，安全描述符及其内部指针由 Windows 分配，在检查完成后统一释放。
    unsafe {
        if GetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut acl),
            None,
            &mut descriptor,
        )
        .is_err()
        {
            return false;
        }
        let protected = (|| {
            if owner.0.is_null() || acl.is_null() || !trusted_sid(owner) {
                return false;
            }
            let mut dangerous = 0x500d_0150;
            if is_file {
                dangerous |= 0x0006;
            }
            for index in 0..u32::from((*acl).AceCount) {
                let mut raw = std::ptr::null_mut();
                if GetAce(acl, index, &mut raw).is_err() || raw.is_null() {
                    return false;
                }
                let header = &*raw.cast::<ACE_HEADER>();
                if u32::from(header.AceFlags) & INHERIT_ONLY_ACE.0 != 0 {
                    continue;
                }
                match header.AceType {
                    0 if usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>() => {
                        let allowed = &*raw.cast::<ACCESS_ALLOWED_ACE>();
                        let principal =
                            PSID(std::ptr::addr_of!(allowed.SidStart).cast_mut().cast());
                        if allowed.Mask & dangerous != 0 && !trusted_sid(principal) {
                            return false;
                        }
                    }
                    1 => {}
                    _ => return false,
                }
            }
            true
        })();
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        protected
    }
}

fn trusted_sid(principal: PSID) -> bool {
    let mut text = PWSTR::null();
    // SAFETY: SID 位于仍有效的安全描述符中，字符串由 Windows 分配并在转换后释放。
    unsafe {
        if ConvertSidToStringSidW(principal, &mut text).is_err() {
            return false;
        }
        let sid = text.to_string().unwrap_or_default();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        matches!(
            sid.as_str(),
            "S-1-5-18"
                | "S-1-5-32-544"
                | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
        )
    }
}
