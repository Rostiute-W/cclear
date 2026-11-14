//! 读可执行文件里记录的固定版本号（`VS_FIXEDFILEINFO`）。

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows::core::PCWSTR;

/// `VS_FIXEDFILEINFO` 的签名，出现在其他数据前时说明取到的不是版本块。
const FIXED_FILE_INFO_SIGNATURE: u32 = 0xFEEF_04BD;

/// 版本资源的根块查询串：反斜杠加 NUL。
const ROOT_BLOCK: &[u16] = &[b'\\' as u16, 0];

/// 读 `major.minor.build` 形式的文件版本。
///
/// 没有版本资源、文件打不开、版本块不是固定信息时都返回 None，由调用方按“证据不足”处理。
pub fn file_version(path: &Path) -> Option<String> {
    let mut wide: Vec<u16> = crate::win::to_verbatim(path)
        .as_os_str()
        .encode_wide()
        .collect();
    wide.push(0);

    let mut handle = 0u32;
    // SAFETY: wide 以 NUL 结尾且在调用期间存活；handle 由系统回写，只是占位。
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), Some(&mut handle)) };
    if size == 0 {
        return None;
    }

    let mut buffer = vec![0u8; size as usize];
    // SAFETY: 缓冲区长度就是系统给出的 size；句柄参数传 None 表示不使用。
    unsafe {
        GetFileVersionInfoW(
            PCWSTR(wide.as_ptr()),
            None,
            size,
            buffer.as_mut_ptr() as *mut core::ffi::c_void,
        )
    }
    .ok()?;

    let mut block: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut length = 0u32;
    // SAFETY: buffer 按 size 分配，块指针和长度由系统回写。
    let found = unsafe {
        VerQueryValueW(
            buffer.as_ptr() as *const core::ffi::c_void,
            PCWSTR(ROOT_BLOCK.as_ptr()),
            &mut block,
            &mut length,
        )
    };
    // 长度不足一个 VS_FIXEDFILEINFO 时不能按结构体读。
    if !found.as_bool() || block.is_null() || length < size_of::<VS_FIXEDFILEINFO>() as u32 {
        return None;
    }

    // SAFETY: 上面的长度检查保证这块内存至少能放下一个 VS_FIXEDFILEINFO；
    // 版本块在缓冲区里的偏移不保证按结构体对齐，所以用 read_unaligned 按值读出。
    let info = unsafe { std::ptr::read_unaligned(block as *const VS_FIXEDFILEINFO) };
    if info.dwSignature != FIXED_FILE_INFO_SIGNATURE {
        return None;
    }

    // 版本号按 Win32 的约定拆成四段：MS 高 16 位是 major，LS 高 16 位是 build。
    let major = info.dwFileVersionMS >> 16;
    let minor = info.dwFileVersionMS & 0xFFFF;
    let build = info.dwFileVersionLS >> 16;

    Some(format!("{major}.{minor}.{build}"))
}
