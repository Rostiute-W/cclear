//! 注册表读取：环境状态卡要读休眠开关、页面文件配置和 WSL 发行版。
//!
//! 读不到、类型不符或权限不足都返回 None，由界面显示“未知”。这里不细分失败原因：
//! 环境状态卡只做展示，缺一项不影响其他项。

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_MULTI_SZ, REG_SZ,
    REG_VALUE_TYPE, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW,
};
use windows::core::{PCWSTR, PWSTR};

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
    let name = encode(subkey);
    let mut key = HKEY(std::ptr::null_mut());

    // SAFETY: 子键名以 NUL 结尾且在调用期间存活；句柄写入 key，由调用方负责关闭。
    let opened =
        unsafe { RegOpenKeyExW(root.raw(), PCWSTR(name.as_ptr()), None, KEY_READ, &mut key) };
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
