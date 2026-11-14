//! 外部路径校验：分析类命令只接受盘符绝对路径，其余一律拒绝。

use std::path::PathBuf;

use thiserror::Error;

/// 目录名里没有意义、只会出现在注入尝试中的字符。
const FORBIDDEN_CHARS: [char; 6] = ['*', '?', '"', '<', '>', '|'];

/// 路径护栏的错误。
#[derive(Debug, Error)]
pub enum GuardError {
    #[error("路径不合法：{0}")]
    InvalidExternalPath(String),
}

/// 校验并规范化前端传来的分析输入路径。
///
/// 只接受 `X:\...` 形式；UNC、设备路径、备用数据流、相对路径和通配符全部拒绝。
/// 返回的路径去掉重复分隔符和末尾分隔符，分隔符统一为反斜杠。
pub fn validate_external_path(s: &str) -> Result<PathBuf, GuardError> {
    let bytes = s.as_bytes();

    // 盘符必须是 ASCII 字母，紧跟冒号和一个分隔符。`\\`、`\\.\`、GLOBALROOT 都不满足这一点。
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return Err(invalid("必须以盘符开头，例如 C:\\"));
    }
    if bytes[2] != b'\\' && bytes[2] != b'/' {
        return Err(invalid("盘符后紧跟的必须是分隔符"));
    }

    for (index, ch) in s.char_indices() {
        // 盘符自身的冒号是唯一合法的冒号，其余位置出现冒号就是备用数据流。
        if index >= 2 && ch == ':' {
            return Err(invalid("不能包含盘符以外的冒号"));
        }
        if ch.is_control() || FORBIDDEN_CHARS.contains(&ch) {
            return Err(invalid("不能包含通配符、引号、尖括号、竖线或控制字符"));
        }
    }

    let mut parts: Vec<&str> = Vec::new();
    for part in s[2..].split(['\\', '/']) {
        // 重复分隔符和末尾分隔符在这里被丢掉。
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." {
            return Err(invalid("不能包含 . 或 .. 组件"));
        }
        if part.ends_with(' ') || part.ends_with('.') {
            return Err(invalid("目录名不能以空格或点结尾"));
        }
        parts.push(part);
    }

    let mut normalized = String::with_capacity(s.len());
    normalized.push(bytes[0] as char);
    normalized.push_str(":\\");
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            normalized.push('\\');
        }
        normalized.push_str(part);
    }

    Ok(PathBuf::from(normalized))
}

fn invalid(reason: &str) -> GuardError {
    GuardError::InvalidExternalPath(reason.to_string())
}
