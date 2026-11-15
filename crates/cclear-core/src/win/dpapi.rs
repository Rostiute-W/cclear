//! DPAPI 加解密：API Key 只以密文形式落盘。
//!
//! 用当前用户范围、不弹 UI 的方式加解密：换用户或换机器都解不开，
//! 也不会有任何提示框打断后台操作。

use std::io;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

/// 用当前用户的 DPAPI 加密。
pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();

    // SAFETY: 输入 blob 指向本次调用的切片；描述文字、可选熵和提示结构都传空，
    // 输出 blob 由系统分配，读完立即释放。
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(io::Error::other)?;

    // SAFETY: 调用成功，output 是尚未释放的系统缓冲区。
    Ok(unsafe { take_blob(output) })
}

/// 用当前用户的 DPAPI 解密。换用户或数据被改过时会失败。
pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();

    // SAFETY: 输入 blob 指向本次调用的切片；描述文字、可选熵和提示结构都传空，
    // 输出 blob 由系统分配，读完立即释放。
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(io::Error::other)?;

    // SAFETY: 调用成功，output 是尚未释放的系统缓冲区。
    Ok(unsafe { take_blob(output) })
}

/// 复制出 DPAPI 分配的缓冲区，然后释放它。
///
/// # Safety
/// `blob` 必须是 `CryptProtectData` / `CryptUnprotectData` 返回且尚未释放的输出。
unsafe fn take_blob(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if blob.pbData.is_null() || blob.cbData == 0 {
        return Vec::new();
    }

    // SAFETY: 缓冲区由系统分配，长度由同一个结构体的 cbData 给出。
    let data = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();

    // SAFETY: 这块内存是系统分配的，只释放一次；上面已经复制过内容，释放后不再引用。
    unsafe {
        let _ = LocalFree(Some(HLOCAL(blob.pbData as *mut core::ffi::c_void)));
    }

    data
}
