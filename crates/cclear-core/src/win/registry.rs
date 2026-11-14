//! 注册表读写：环境状态卡要读休眠开关、页面文件配置和 WSL 发行版；
//! 工具缓存迁移要读写用户环境变量。
//!
//! 读不到、类型不符或权限不足都返回 None，由界面显示“未知”。这里不细分失败原因：
//! 环境状态卡只做展示，缺一项不影响其他项。写入只有迁移用到，失败必须如实返回。

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;

use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, LPARAM, WPARAM,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE,
    REG_DWORD, REG_EXPAND_SZ, REG_MULTI_SZ, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegDeleteValueW,
    RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use windows::core::{PCWSTR, PWSTR};

/// 用户环境变量所在的子键（相对 `HKEY_CURRENT_USER`）。
const USER_ENVIRONMENT_KEY: &str = "Environment";

/// 广播环境变量变化时的等待上限。有窗口卡住时不能拖住整个迁移。
const BROADCAST_TIMEOUT_MS: u32 = 5000;

/// 注册表根键。只开放这两个真实用到的根，避免调用方拿到任意句柄。
#[derive(Clone, Copy)]
pub enum RegistryRoot {
    LocalMachine,
    CurrentUser,
}

impl RegistryRoot {
    fn raw(self) -> HKEY {
        match self {
            RegistryRoot::LocalMachine => HKEY_LOCAL_MACHINE,
            RegistryRoot::CurrentUser => HKEY_CURRENT_USER,
        }
    }
}

/// 子键名的缓冲区长度。注册表键名最长 255 个字符。
const KEY_NAME_CHARS: usize = 256;

/// 读 32 位整数（`REG_DWORD`）。
pub fn read_dword(root: RegistryRoot, subkey: &str, value: &str) -> Option<u32> {
    let (kind, data) = read_value(root, subkey, value)?;
    if kind != REG_DWORD || data.len() < 4 {
        return None;
    }

    Some(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

/// 读单条字符串（`REG_SZ`）。空字符串当作没有配置。
pub fn read_string(root: RegistryRoot, subkey: &str, value: &str) -> Option<String> {
    let (kind, data) = read_value(root, subkey, value)?;
    if kind != REG_SZ {
        return None;
    }

    let text = decode_string(&data);
    if text.is_empty() { None } else { Some(text) }
}

/// 读多行字符串（`REG_MULTI_SZ`）。空项被丢弃。
pub fn read_multi_string(root: RegistryRoot, subkey: &str, value: &str) -> Option<Vec<String>> {
    let (kind, data) = read_value(root, subkey, value)?;
    if kind != REG_MULTI_SZ {
        return None;
    }

    Some(decode_multi_string(&data))
}

/// 列出子键名。一个子键都没有时返回空列表，读失败返回 None。
pub fn subkeys(root: RegistryRoot, subkey: &str) -> Option<Vec<String>> {
    let key = open_key(root, subkey)?;
    let mut names = Vec::new();
    let mut buffer = [0u16; KEY_NAME_CHARS];

    for index in 0.. {
        let mut length = buffer.len() as u32;
        // SAFETY: 缓冲区长度按元素个数传入，系统最多写入这么多宽字符；其余输出参数都不需要。
        let listed = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(PWSTR(buffer.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };

        if listed == ERROR_NO_MORE_ITEMS {
            break;
        }
        if listed != ERROR_SUCCESS {
            // SAFETY: 句柄由 open_key 打开，成功路径和失败路径都要关闭。
            unsafe {
                let _ = RegCloseKey(key);
            }
            return None;
        }

        names.push(
            OsString::from_wide(&buffer[..length as usize])
                .to_string_lossy()
                .into_owned(),
        );
    }

    // SAFETY: 同上，句柄在枚举结束后关闭。
    unsafe {
        let _ = RegCloseKey(key);
    }
    Some(names)
}

/// 读用户环境变量（`HKEY_CURRENT_USER\Environment`）。
///
/// `REG_SZ` 和 `REG_EXPAND_SZ` 都接受：迁移要如实记下旧值，类型不同不代表没有配置。
/// 返回值原样给出，不展开其中的 `%VAR%`。
pub fn read_env_string(name: &str) -> Option<String> {
    let (kind, data) = read_value(RegistryRoot::CurrentUser, USER_ENVIRONMENT_KEY, name)?;
    if kind != REG_SZ && kind != REG_EXPAND_SZ {
        return None;
    }

    let text = decode_string(&data);
    if text.is_empty() { None } else { Some(text) }
}

/// 写用户环境变量（`REG_SZ`）。
///
/// 只写 `HKEY_CURRENT_USER`，不碰系统级环境变量。写完要调用
/// [`broadcast_environment_change`]，否则只有本进程能看到新值。
pub fn write_env_string(name: &str, value: &str) -> io::Result<()> {
    let key = open_key_with(
        RegistryRoot::CurrentUser,
        USER_ENVIRONMENT_KEY,
        KEY_SET_VALUE | KEY_QUERY_VALUE,
    )
    .ok_or_else(|| io::Error::other("无法打开用户环境变量所在的注册表键"))?;

    let name = encode(name);
    // 字符串值要把结尾的 NUL 一起写进去，否则读回来时会被当成没有结尾。
    let mut data: Vec<u8> = Vec::new();
    for unit in value.encode_utf16() {
        data.extend_from_slice(&unit.to_le_bytes());
    }
    data.extend_from_slice(&0u16.to_le_bytes());

    // SAFETY: 值名和值数据都以 NUL 结尾且在调用期间存活；句柄由 open_key_with 打开，
    // 成功和失败路径都关闭。
    let written =
        unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), Some(0), REG_SZ, Some(&data)) };

    // SAFETY: 同上，句柄用完关闭。
    unsafe {
        let _ = RegCloseKey(key);
    }

    written
        .ok()
        .map_err(|error| io::Error::other(format!("写入注册表失败：{error}")))
}

/// 删除用户环境变量。值本来就不存在时不算失败。
pub fn delete_env_string(name: &str) -> io::Result<()> {
    let key = open_key_with(
        RegistryRoot::CurrentUser,
        USER_ENVIRONMENT_KEY,
        KEY_SET_VALUE | KEY_QUERY_VALUE,
    )
    .ok_or_else(|| io::Error::other("无法打开用户环境变量所在的注册表键"))?;

    let name = encode(name);
    // SAFETY: 值名以 NUL 结尾且在调用期间存活；句柄由 open_key_with 打开，用完关闭。
    let deleted = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };

    // SAFETY: 同上，句柄用完关闭。
    unsafe {
        let _ = RegCloseKey(key);
    }

    if deleted == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }

    deleted
        .ok()
        .map_err(|error| io::Error::other(format!("删除注册表值失败：{error}")))
}

/// 广播 `WM_SETTINGCHANGE("Environment")`，让已经运行的资源管理器等程序重新读环境变量。
///
/// 已经在运行的程序不会因为广播就换掉自己的环境，所以界面还要提示重启 IDE 和终端。
/// 广播失败只影响提示效果，由调用方决定是否可以忽略。
pub fn broadcast_environment_change() -> io::Result<()> {
    let text: Vec<u16> = "Environment\0".encode_utf16().collect();
    let mut result = 0usize;

    // SAFETY: 广播消息不带额外数据结构；lparam 指向调用期间存活的以 NUL 结尾的字符串。
    let sent = unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(text.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            BROADCAST_TIMEOUT_MS,
            Some(&mut result),
        )
    };

    if sent.0 == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

/// 读一个值的数据和类型。值不存在时返回 None。
fn read_value(root: RegistryRoot, subkey: &str, value: &str) -> Option<(REG_VALUE_TYPE, Vec<u8>)> {
    let key = open_key(root, subkey)?;
    let name = encode(value);
    let mut kind = REG_VALUE_TYPE::default();

    // 先查数据长度，再按长度分配缓冲区，避免读到一半被截断。
    let mut size = 0u32;
    // SAFETY: 只查询长度，不传数据缓冲区；值名以 NUL 结尾且在调用期间存活。
    let queried = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    if queried != ERROR_SUCCESS {
        // SAFETY: 句柄由 open_key 打开，这里用完就关。
        unsafe {
            let _ = RegCloseKey(key);
        }
        return None;
    }

    let mut data = vec![0u8; size as usize];
    let mut read = size;
    let queried = if data.is_empty() {
        ERROR_SUCCESS
    } else {
        // SAFETY: 缓冲区长度就是上面查到的字节数，系统最多写入这么多。
        unsafe {
            RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut read),
            )
        }
    };

    // SAFETY: 同上，句柄用完关闭。
    unsafe {
        let _ = RegCloseKey(key);
    }
    if queried != ERROR_SUCCESS && queried != ERROR_MORE_DATA {
        return None;
    }

    data.truncate(read as usize);
    Some((kind, data))
}

/// 打开子键。读权限就够，环境状态卡不做任何写入。
fn open_key(root: RegistryRoot, subkey: &str) -> Option<HKEY> {
    open_key_with(root, subkey, KEY_READ)
}

/// 按指定权限打开子键。开不出来时返回 None，由调用方决定怎么报告。
fn open_key_with(
    root: RegistryRoot,
    subkey: &str,
    access: windows::Win32::System::Registry::REG_SAM_FLAGS,
) -> Option<HKEY> {
    let name = encode(subkey);
    let mut key = HKEY(std::ptr::null_mut());

    // SAFETY: 子键名以 NUL 结尾且在调用期间存活；句柄写入 key，由调用方负责关闭。
    let opened =
        unsafe { RegOpenKeyExW(root.raw(), PCWSTR(name.as_ptr()), None, access, &mut key) };
    if opened != ERROR_SUCCESS {
        return None;
    }

    Some(key)
}

/// 转成以 NUL 结尾的宽字符串。
fn encode(text: &str) -> Vec<u16> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    wide
}

/// 注册表数据是 UTF-16 小端，遇到第一个 NUL 就是字符串结尾。
fn decode_string(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let length = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());

    OsString::from_wide(&units[..length])
        .to_string_lossy()
        .into_owned()
}

/// `REG_MULTI_SZ` 是若干条以 NUL 分隔的字符串，末尾再补一个 NUL。
fn decode_multi_string(data: &[u8]) -> Vec<String> {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();

    let mut items = Vec::new();
    let mut start = 0;
    for (index, unit) in units.iter().enumerate() {
        if *unit != 0 {
            continue;
        }
        if index > start {
            items.push(
                OsString::from_wide(&units[start..index])
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        start = index + 1;
    }

    items
}
