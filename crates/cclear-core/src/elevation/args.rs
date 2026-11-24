use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SystemOp {
    WindowsTemp,
    WindowsLogs,
    SoftwareDistribution,
    WerSystem,
    WindowsMinidump,
    DefenderHistory,
    SysReset,
    WindowsBt,
    WindowsOld,
    NvidiaDownloader,
    DeliveryOptimization,
    DismAnalyze,
    DismComponentCleanup,
    HibernateOff,
    HibernateReduced,
    HibernateOn,
    VssCleanOldest,
    WslCompact,
    MftScan,
}

impl SystemOp {
    pub fn token(self) -> String {
        let json = serde_json::to_string(&self).expect("系统操作序列化失败");
        json.trim_matches('"').to_owned()
    }
}

#[derive(Debug, Error)]
pub enum ElevationError {
    #[error("提权参数无效")]
    InvalidArguments,
    #[error("已取消管理员授权")]
    Cancelled,
    #[error("启动管理员操作失败：{0}")]
    Launch(String),
    #[error("管理员进程未返回结果（退出码 {exit_code}）")]
    MissingResult { exit_code: u32 },
    #[error("管理员结果无效：{0}")]
    Result(String),
}

#[derive(Debug)]
pub struct HelperArgs {
    pub ops: Vec<SystemOp>,
    pub result: Uuid,
    pub mft_drive: Option<char>,
    pub temp_age: u32,
    pub reboot_delete: bool,
}

pub fn parse_helper_args(args: &[String]) -> Result<HelperArgs, ElevationError> {
    use ElevationError::InvalidArguments;

    if args.first().map(String::as_str) != Some("--elevated") {
        return Err(InvalidArguments);
    }
    let mut ops = None;
    let mut result = None;
    let mut mft_drive = None;
    let mut temp_age = None;
    let mut reboot_delete = false;
    let mut tokens = args.iter().skip(1);
    while let Some(argument) = tokens.next() {
        if argument == "--reboot-delete" && !reboot_delete {
            reboot_delete = true;
            continue;
        }
        let value = tokens.next().ok_or(InvalidArguments)?;
        match argument.as_str() {
            "--ops" if ops.is_none() => {
                let mut parsed = Vec::new();
                for token in value.split(',') {
                    if !token
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
                    {
                        return Err(InvalidArguments);
                    }
                    let op = serde_json::from_str(&format!("\"{token}\""))
                        .map_err(|_| InvalidArguments)?;
                    if parsed.contains(&op) || parsed.len() >= 20 {
                        return Err(InvalidArguments);
                    }
                    parsed.push(op);
                }
                ops = Some(parsed);
            }
            "--result" if result.is_none() => result = Some(parse_uuid(value)?),
            "--mft-drive"
                if mft_drive.is_none()
                    && value.len() == 1
                    && value.as_bytes()[0].is_ascii_alphabetic() =>
            {
                mft_drive = value.chars().next().map(|drive| drive.to_ascii_uppercase());
            }
            "--temp-age" if temp_age.is_none() => {
                let hours = value.parse().map_err(|_| InvalidArguments)?;
                if !(24..=8760).contains(&hours) {
                    return Err(InvalidArguments);
                }
                temp_age = Some(hours);
            }
            _ => return Err(InvalidArguments),
        }
    }
    let ops = ops.ok_or(InvalidArguments)?;
    let result = result.ok_or(InvalidArguments)?;
    if ops.contains(&SystemOp::MftScan) != mft_drive.is_some()
        || mft_drive.is_some() && ops.len() != 1
        || reboot_delete && mft_drive.is_some()
    {
        return Err(InvalidArguments);
    }
    Ok(HelperArgs {
        ops,
        result,
        mft_drive,
        temp_age: temp_age.unwrap_or(24),
        reboot_delete,
    })
}

fn parse_uuid(value: &str) -> Result<Uuid, ElevationError> {
    let uuid = Uuid::parse_str(value).map_err(|_| ElevationError::InvalidArguments)?;
    if uuid.to_string() != value || uuid.is_nil() {
        return Err(ElevationError::InvalidArguments);
    }
    Ok(uuid)
}
