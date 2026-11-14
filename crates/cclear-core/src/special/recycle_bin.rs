//! C 盘回收站：只清 C 盘，执行前重新查询并比对。

use windows::Win32::UI::Shell::{
    SHERB_NOCONFIRMATION, SHERB_NOSOUND, SHEmptyRecycleBinW, SHQUERYRBINFO, SHQueryRecycleBinW,
};
use windows::core::PCWSTR;

/// 只处理 C 盘回收站。其它卷回收站由用户自己在资源管理器里处理。
const C_ROOT: &str = "C:\\";

/// 查询 C 盘回收站的条目数和占用字节数。
pub fn query_c() -> Result<(i64, i64), String> {
    let root = wide(C_ROOT);
    let mut info = SHQUERYRBINFO {
        cbSize: size_of::<SHQUERYRBINFO>() as u32,
        ..SHQUERYRBINFO::default()
    };

    // SAFETY: 根路径是以 NUL 结尾的宽字符串且在调用期间存活；
    // 输出结构体的 cbSize 与类型大小一致，指针在调用期间有效。
    let result = unsafe { SHQueryRecycleBinW(PCWSTR(root.as_ptr()), &mut info) };
    result.map_err(|error| format!("无法读取 C 盘回收站信息：{error}"))?;

    Ok((info.i64NumItems, info.i64Size))
}

/// 清空 C 盘回收站。不弹系统确认框、不响提示音，确认由本应用负责。
pub fn empty_c() -> Result<(), String> {
    let root = wide(C_ROOT);
    let flags = SHERB_NOCONFIRMATION | SHERB_NOSOUND;

    // SAFETY: 根路径是以 NUL 结尾的宽字符串且在调用期间存活；没有窗口句柄，不传回调。
    let result = unsafe { SHEmptyRecycleBinW(None, PCWSTR(root.as_ptr()), flags) };
    result.map_err(|error| format!("清空 C 盘回收站失败：{error}"))
}

/// 转成 NUL 结尾的 UTF-16，供 Win32 接口使用。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
