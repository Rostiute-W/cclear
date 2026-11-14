//! helper 的命令行参数：只认内置操作 ID 和三个固定格式的路径。
//!
//! 参数里没有任何用户自由路径：系统目录由 helper 自己用 Known Folder 解析，
//! 这里只校验调用方给的结果文件、进度文件和隔离根目录。

use std::path::PathBuf;

use uuid::Uuid;

use super::ElevationError;

/// 结果文件的固定目录前缀（相对用户名之后）。
const ELEVATED_DIR: &str = r"AppData\Local\CClear\elevated\";

/// 隔离根目录的名字，和 `quarantine::QUARANTINE_DIR_NAME` 保持一致。
const QUARANTINE_DIR: &str = "CClearQuarantine";

/// 需要管理员权限才能执行的内置操作。
///
/// 字符串形式是 kebab-case；批次类操作带上 UUID，写成 `restore-batch:<uuid>` 和 `purge-batch:<uuid>`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemOp {
    LeAppStoreCicada,
    NvidiaOta,
    WindowsTemp,
    WindowsLogs,
    SysReset,
    DismAnalyze,
    DismComponentCleanup,
    DeliveryOptimization,
    RestoreBatch(Uuid),
    PurgeBatch(Uuid),
}

impl SystemOp {
    /// 把命令行里的一项解析成内置操作。未知操作和格式不对的批次 ID 都算参数不合法。
    pub fn parse(text: &str) -> Result<Self, ElevationError> {
        if let Some(id) = text.strip_prefix("restore-batch:") {
            return Ok(Self::RestoreBatch(parse_uuid(id, text)?));
        }
        if let Some(id) = text.strip_prefix("purge-batch:") {
            return Ok(Self::PurgeBatch(parse_uuid(id, text)?));
        }

        match text {
            "leappstore-cicada" => Ok(Self::LeAppStoreCicada),
            "nvidia-ota" => Ok(Self::NvidiaOta),
            "windows-temp" => Ok(Self::WindowsTemp),
            "windows-logs" => Ok(Self::WindowsLogs),
            "sys-reset" => Ok(Self::SysReset),
            "dism-analyze" => Ok(Self::DismAnalyze),
            "dism-component-cleanup" => Ok(Self::DismComponentCleanup),
            "delivery-optimization" => Ok(Self::DeliveryOptimization),
            _ => Err(ElevationError::InvalidArgs(format!(
                "不认识的操作「{text}」"
            ))),
        }
    }

    /// 命令行里写的形式。批次类的操作带 UUID。
    pub fn token(self) -> String {
        match self {
            Self::LeAppStoreCicada => "leappstore-cicada".to_string(),
            Self::NvidiaOta => "nvidia-ota".to_string(),
            Self::WindowsTemp => "windows-temp".to_string(),
            Self::WindowsLogs => "windows-logs".to_string(),
            Self::SysReset => "sys-reset".to_string(),
            Self::DismAnalyze => "dism-analyze".to_string(),
            Self::DismComponentCleanup => "dism-component-cleanup".to_string(),
            Self::DeliveryOptimization => "delivery-optimization".to_string(),
            Self::RestoreBatch(id) => format!("restore-batch:{id}"),
            Self::PurgeBatch(id) => format!("purge-batch:{id}"),
        }
    }

    /// 执行这个操作是否需要隔离根目录。
    pub fn needs_quarantine_root(self) -> bool {
        matches!(
            self,
            Self::LeAppStoreCicada | Self::NvidiaOta | Self::RestoreBatch(_) | Self::PurgeBatch(_)
        )
    }
}

impl std::fmt::Display for SystemOp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.token())
    }
}

/// 解析后的 helper 命令行。
#[derive(Clone, Debug)]
pub struct HelperArgs {
    pub ops: Vec<SystemOp>,
    /// helper 把执行结果写到这里，主程序在这个进程结束后读它。
    pub result: PathBuf,
    /// 长耗时操作的进度文件。M5a 的两个操作很快，暂时不写进度。
    pub progress: Option<PathBuf>,
    /// 隔离根目录。隔离和批次类操作必须给出。
    pub quarantine_root: Option<PathBuf>,
}

/// 解析 helper 的命令行参数。
///
/// 命令行格式：`cclear.exe --elevated --ops <op1,op2> --result <path>
/// [--progress <path>] [--quarantine-root <X:\CClearQuarantine>]`。
/// 这里只校验格式；隔离根目录所在卷是不是固定 NTFS 非系统卷，在 helper 里执行前再查一次。
pub fn parse_helper_args(args: &[String]) -> Result<HelperArgs, ElevationError> {
    let mut elevated = false;
    let mut ops: Option<Vec<SystemOp>> = None;
    let mut result: Option<PathBuf> = None;
    let mut progress: Option<PathBuf> = None;
    let mut quarantine_root: Option<PathBuf> = None;

    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--elevated" => {
                if elevated {
                    return Err(ElevationError::InvalidArgs(
                        "参数 --elevated 重复".to_string(),
                    ));
                }
                elevated = true;
                index += 1;
            }
            "--ops" | "--result" | "--progress" | "--quarantine-root" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(ElevationError::InvalidArgs(format!("{flag} 缺少取值")));
                };

                match flag {
                    "--ops" => {
                        if ops.is_some() {
                            return Err(ElevationError::InvalidArgs("参数 --ops 重复".to_string()));
                        }
                        ops = Some(parse_ops(value)?);
                    }
                    "--result" => {
                        if result.is_some() {
                            return Err(ElevationError::InvalidArgs(
                                "参数 --result 重复".to_string(),
                            ));
                        }
                        if !matches_elevated_path(value, "json") {
                            return Err(ElevationError::InvalidArgs(format!(
                                "结果文件路径不合法：{value}"
                            )));
                        }
                        result = Some(PathBuf::from(value));
                    }
                    "--progress" => {
                        if progress.is_some() {
                            return Err(ElevationError::InvalidArgs(
                                "参数 --progress 重复".to_string(),
                            ));
                        }
                        if !matches_elevated_path(value, "progress") {
                            return Err(ElevationError::InvalidArgs(format!(
                                "进度文件路径不合法：{value}"
                            )));
                        }
                        progress = Some(PathBuf::from(value));
                    }
                    _ => {
                        if quarantine_root.is_some() {
                            return Err(ElevationError::InvalidArgs(
                                "参数 --quarantine-root 重复".to_string(),
                            ));
                        }
                        if !matches_quarantine_root(value) {
                            return Err(ElevationError::InvalidArgs(format!(
                                "隔离根目录不合法：{value}"
                            )));
                        }
                        quarantine_root = Some(PathBuf::from(value));
                    }
                }

                index += 2;
            }
            other => {
                return Err(ElevationError::InvalidArgs(format!(
                    "不认识的参数「{other}」"
                )));
            }
        }
    }

    if !elevated {
        return Err(ElevationError::InvalidArgs(
            "缺少参数 --elevated".to_string(),
        ));
    }

    let Some(ops) = ops else {
        return Err(ElevationError::InvalidArgs("缺少参数 --ops".to_string()));
    };
    let Some(result) = result else {
        return Err(ElevationError::InvalidArgs("缺少参数 --result".to_string()));
    };

    // 隔离和批次操作要用隔离根；给了它却格式不对时上面已经拒绝。
    if quarantine_root.is_none() && ops.iter().any(|op| op.needs_quarantine_root()) {
        return Err(ElevationError::InvalidArgs(
            "缺少参数 --quarantine-root".to_string(),
        ));
    }

    Ok(HelperArgs {
        ops,
        result,
        progress,
        quarantine_root,
    })
}

/// 逗号分隔的操作列表。空项、未知操作和重复操作都拒绝。
fn parse_ops(text: &str) -> Result<Vec<SystemOp>, ElevationError> {
    if text.is_empty() {
        return Err(ElevationError::InvalidArgs("参数 --ops 为空".to_string()));
    }

    let mut ops: Vec<SystemOp> = Vec::new();
    for token in text.split(',') {
        if token.is_empty() {
            return Err(ElevationError::InvalidArgs(
                "参数 --ops 里有空项".to_string(),
            ));
        }

        let op = SystemOp::parse(token)?;
        if ops.contains(&op) {
            return Err(ElevationError::InvalidArgs(format!("操作「{token}」重复")));
        }
        ops.push(op);
    }

    Ok(ops)
}

/// 批次类操作里的 UUID。
fn parse_uuid(text: &str, token: &str) -> Result<Uuid, ElevationError> {
    Uuid::parse_str(text)
        .map_err(|_| ElevationError::InvalidArgs(format!("操作「{token}」里的批次 ID 不合法")))
}

/// 校验结果文件和进度文件的路径：
/// `<盘符>:\Users\<用户名>\AppData\Local\CClear\elevated\<uuid>.<json|progress>`。
fn matches_elevated_path(text: &str, extension: &str) -> bool {
    let bytes = text.as_bytes();

    // 盘符 + `:\`
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return false;
    }
    let Some(rest) = text.get(3..) else {
        return false;
    };

    let Some(rest) = strip_prefix_ignore_ascii_case(rest, "Users\\") else {
        return false;
    };

    // 用户名必须是单个普通组件：`.`、`..`、带冒号或 `/` 的名字都会越出固定目录，
    // Windows 同样把 `/` 当路径分隔符，`a/..` 会穿越用户名段。
    let Some((user, rest)) = rest.split_once('\\') else {
        return false;
    };
    if user.is_empty() || user == "." || user == ".." || user.contains([':', '/']) {
        return false;
    }

    let Some(rest) = strip_prefix_ignore_ascii_case(rest, ELEVATED_DIR) else {
        return false;
    };

    let Some((stem, tail)) = rest.rsplit_once('.') else {
        return false;
    };
    if !tail.eq_ignore_ascii_case(extension) {
        return false;
    }

    is_uuid_lower_hex(stem)
}

/// 36 个字符的 UUID 写法：只有小写十六进制和连字符。
fn is_uuid_lower_hex(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return false;
    }

    bytes.iter().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
        }
    })
}

/// 隔离根目录必须正好是 `<大写盘符>:\CClearQuarantine`。
fn matches_quarantine_root(text: &str) -> bool {
    let bytes = text.as_bytes();

    bytes.len() == 19
        && bytes[0].is_ascii_uppercase()
        && bytes[1] == b':'
        && bytes[2] == b'\\'
        && &bytes[3..] == QUARANTINE_DIR.as_bytes()
}

/// 不区分大小写地去掉一个 ASCII 前缀。ASCII 大小写转换不改变字节长度。
fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }

    text.get(prefix.len()..)
}
