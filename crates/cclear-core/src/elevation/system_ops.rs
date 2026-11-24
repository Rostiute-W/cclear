use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exec::{
    command,
    file_cleaner::{Cleaner, Filters},
};
use crate::guard::{self, GuardError, OpenIntent};
use crate::model::KnownRoot;
use crate::rules::{RootMap, forbidden::same_or_under};

use super::{OpOutcome, SystemOp};

pub fn file_targets(
    op: SystemOp,
    roots: &RootMap,
    temp_age: u32,
) -> Option<(Vec<PathBuf>, Filters)> {
    use KnownRoot::{ProgramData, SystemDrive, Windows};
    use SystemOp::*;
    let (root, targets, age, extensions): (_, &[&str], _, &[&str]) = match op {
        WindowsTemp => (Windows, &["Temp"], temp_age.max(24), &[]),
        WindowsLogs => (Windows, &["Logs"], 168, &["log", "etl", "cab"]),
        SoftwareDistribution => (Windows, &[r"SoftwareDistribution\Download"], 24, &[]),
        WerSystem => (
            ProgramData,
            &[
                r"Microsoft\Windows\WER\ReportArchive",
                r"Microsoft\Windows\WER\ReportQueue",
            ],
            24,
            &[],
        ),
        WindowsMinidump => (Windows, &["Minidump", "MEMORY.DMP"], 24, &["dmp", "mdmp"]),
        DefenderHistory => (
            ProgramData,
            &[r"Microsoft\Windows Defender\Scans\History\Service"],
            168,
            &[],
        ),
        SysReset => (
            SystemDrive,
            &[r"$SysReset\Logs", r"$SysReset\Scratch"],
            168,
            &[],
        ),
        WindowsBt => (
            SystemDrive,
            &[
                r"$WINDOWS.~BT\Sources\Panther",
                r"$WINDOWS.~BT\Sources\Rollback",
                r"$WINDOWS.~BT\Sources\SafeOS",
            ],
            168,
            &[],
        ),
        WindowsOld => (
            SystemDrive,
            &[
                r"Windows.old\Windows\Temp",
                r"Windows.old\Windows\Logs",
                r"Windows.old\Windows\SoftwareDistribution\Download",
                r"Windows.old\Windows\Minidump",
                r"Windows.old\Windows\MEMORY.DMP",
            ],
            168,
            &[],
        ),
        NvidiaDownloader => {
            let drive = roots.get(SystemDrive)?;
            let data = roots.get(ProgramData)?;
            return Some((
                vec![
                    drive.join("NVIDIA"),
                    drive.join("AMD"),
                    data.join(r"NVIDIA Corporation\Downloader"),
                    data.join(r"NVIDIA Corporation\NVIDIA App\UpdateFramework\ota-artifacts"),
                ],
                Filters {
                    min_age_hours: 168,
                    ..Filters::default()
                },
            ));
        }
        DeliveryOptimization => (
            Windows,
            &[
                r"ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache",
            ],
            24,
            &[],
        ),
        _ => return None,
    };
    let base = roots.get(root)?;
    let paths = targets.iter().map(|relative| base.join(relative)).collect();
    Some((
        paths,
        Filters {
            min_age_hours: age,
            extensions: extensions
                .iter()
                .map(|extension| (*extension).into())
                .collect(),
            ..Filters::default()
        },
    ))
}

pub fn validate_reboot_path(
    target: &Path,
    roots: &RootMap,
    cleaner: &Cleaner<'_>,
    temp_age: u32,
) -> Result<PathBuf, GuardError> {
    let target = guard::validate_external_path(target.to_str().ok_or(GuardError::InvalidName)?)?;
    if cleaner.whitelisted(&target) {
        return Err(GuardError::Forbidden(target));
    }
    let identity = guard::path::inspect_path(&target, cleaner.forbidden)?;
    if identity.is_dir {
        return Err(GuardError::InvalidName);
    }
    let leaf = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(GuardError::InvalidName)?;
    let eligible = [
        SystemOp::WindowsTemp,
        SystemOp::WindowsLogs,
        SystemOp::SoftwareDistribution,
        SystemOp::WerSystem,
        SystemOp::WindowsMinidump,
        SystemOp::DeliveryOptimization,
    ]
    .iter()
    .any(|op| {
        file_targets(*op, roots, temp_age).is_some_and(|(anchors, filters)| {
            anchors.iter().any(|anchor| same_or_under(&target, anchor))
                && filters.matches_file(leaf, identity.mtime)
        })
    });
    if !eligible {
        return Err(GuardError::Forbidden(target));
    }
    Ok(target)
}

fn schedule_locked(
    paths: &[PathBuf],
    roots: &RootMap,
    cleaner: &Cleaner<'_>,
    temp_age: u32,
) -> u64 {
    let mut scheduled = HashSet::new();
    for candidate in paths {
        if cleaner.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let Ok(target) = validate_reboot_path(candidate, roots, cleaner, temp_age) else {
            continue;
        };
        let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
            continue;
        };
        let Ok(anchor) = guard::open_anchor(parent, cleaner.forbidden) else {
            continue;
        };
        if !matches!(
            guard::open_rel(
                &anchor,
                &[name.to_os_string()],
                OpenIntent::Delete,
                cleaner.forbidden
            ),
            Err(GuardError::Locked)
        ) {
            continue;
        }
        if !scheduled.contains(&target)
            && guard::reboot::schedule_delete_on_reboot(&target, cleaner.forbidden).is_ok()
        {
            scheduled.insert(target);
        }
    }
    scheduled.len() as u64
}

pub(super) fn execute(
    op: SystemOp,
    roots: &RootMap,
    cleaner: &Cleaner<'_>,
    temp_age: u32,
    reboot: bool,
) -> OpOutcome {
    if !crate::win::is_elevated() {
        return OpOutcome::new(op, false, "操作需要管理员权限");
    }
    if let Some((mut paths, filters)) = file_targets(op, roots, temp_age) {
        paths.retain(|path| {
            !matches!(
                guard::path::inspect_path(path, cleaner.forbidden),
                Err(GuardError::Gone)
            )
        });
        if paths.is_empty() {
            return OpOutcome::new(op, true, "未发现符合条件的系统文件");
        }
        let mut files = cleaner.clean(&paths, &filters);
        let reboot_scheduled = if reboot {
            schedule_locked(&files.locked, roots, cleaner, temp_age)
        } else {
            0
        };
        files.locked.clear();
        let detail = files
            .detail
            .clone()
            .unwrap_or_else(|| "系统文件检查完成".into());
        let success = files.detail.is_none() || files.deleted_files > 0 || reboot_scheduled > 0;
        return OpOutcome {
            op,
            success,
            detail,
            files,
            reboot_scheduled,
        };
    }
    let Some(windows) = roots.get(KnownRoot::Windows) else {
        return OpOutcome::new(op, false, "无法读取 Windows 目录");
    };
    let system32 = windows.join("System32");
    let tool;
    let drive;
    let args: &[&str] = match op {
        SystemOp::DismAnalyze => {
            tool = system32.join("Dism.exe");
            &[
                "/English",
                "/Online",
                "/Cleanup-Image",
                "/AnalyzeComponentStore",
            ]
        }
        SystemOp::DismComponentCleanup => {
            tool = system32.join("Dism.exe");
            &[
                "/English",
                "/Online",
                "/Cleanup-Image",
                "/StartComponentCleanup",
            ]
        }
        SystemOp::HibernateOff | SystemOp::HibernateReduced | SystemOp::HibernateOn => {
            tool = system32.join("powercfg.exe");
            if op != SystemOp::HibernateOff {
                let outcome = run_system_command(op, &tool, &["/hibernate", "on"]);
                if !outcome.success {
                    return outcome;
                }
            }
            if op == SystemOp::HibernateReduced {
                let outcome = run_system_command(op, &tool, &["/hibernate", "/size", "0"]);
                if !outcome.success {
                    return outcome;
                }
            }
            match op {
                SystemOp::HibernateOff => &["/hibernate", "off"],
                SystemOp::HibernateReduced => &["/hibernate", "/type", "reduced"],
                _ => &["/hibernate", "/type", "full"],
            }
        }
        SystemOp::VssCleanOldest => {
            tool = system32.join("vssadmin.exe");
            let Some(root) = roots.get(KnownRoot::SystemDrive).and_then(Path::to_str) else {
                return OpOutcome::new(op, false, "无法判断系统盘");
            };
            drive = format!("/for={}", root.trim_end_matches('\\'));
            &["delete", "shadows", &drive, "/oldest", "/quiet"]
        }
        SystemOp::WslCompact => {
            return crate::orchestrator::deep_slim::compact(roots, &system32, cleaner.whitelist);
        }
        _ => return OpOutcome::new(op, false, "系统操作不适用于当前批次"),
    };
    run_system_command(op, &tool, args)
}

fn run_system_command(op: SystemOp, tool: &Path, args: &[&str]) -> OpOutcome {
    let output = command::run_clean(tool, args, Duration::from_secs(3600));
    let success = output.exit_code == Some(0) && !output.timed_out;
    let detail = if output.timed_out {
        "等待系统命令超时，它可能仍在运行".into()
    } else {
        let text = format!("{}\n{}", output.stdout_tail, output.stderr_tail);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            if success {
                "系统操作已完成".into()
            } else {
                format!("系统命令执行失败（退出码 {:?}）", output.exit_code)
            }
        } else {
            trimmed.to_owned()
        }
    };
    OpOutcome::new(op, success, detail)
}
