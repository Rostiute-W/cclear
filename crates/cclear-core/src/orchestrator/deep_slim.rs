use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::elevation::{self, ElevationError, OpOutcome, SystemOp};
use crate::exec::command;
use crate::guard::{self, Forbidden};
use crate::model::KnownRoot;
use crate::rules::{RootMap, forbidden::same_or_under};
use crate::win::{
    self,
    registry::{self, RegistryRoot},
    volume,
};

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeepSlimAction {
    HibernateOff,
    HibernateReduced,
    HibernateOn,
    DismAnalyze,
    DismCleanup,
    VssCleanOldest,
    WslCompact,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WinSxSState {
    reclaimable_text: Option<String>,
    recommended: Option<bool>,
    detail: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HibernateState {
    mode: &'static str,
    file_bytes: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PagefileEntry {
    path: PathBuf,
    size_bytes: Option<u64>,
    on_system_drive: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WslVhdxInfo {
    name: String,
    path: PathBuf,
    size_bytes: u64,
    on_system_drive: bool,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct VssState {
    used_bytes: Option<u64>,
    shadow_count: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSlimState {
    hibernate: HibernateState,
    pagefiles: Vec<PagefileEntry>,
    wsl_vhdx: Vec<WslVhdxInfo>,
    vss: VssState,
    winsxs: WinSxSState,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSlimProgress {
    action: DeepSlimAction,
    percent: Option<u8>,
    detail: String,
    done: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSlimResult {
    action: DeepSlimAction,
    status: &'static str,
    detail: String,
    free_before: u64,
    free_after: u64,
    updated_state: DeepSlimState,
}

pub fn status(roots: &RootMap, winsxs: &WinSxSState) -> DeepSlimState {
    let system = roots.get(KnownRoot::SystemDrive);
    let on_system_drive = |target: &Path| system.is_some_and(|root| same_or_under(target, root));
    let power = r"SYSTEM\CurrentControlSet\Control\Power";
    let machine = RegistryRoot::LocalMachine;
    let file_bytes = system.and_then(|root| file_size(&root.join("hiberfil.sys")));
    let mode = match (
        registry::read_dword(machine, power, "HibernateEnabled"),
        registry::read_dword(machine, power, "HiberFileType"),
    ) {
        (Some(0), _) => "off",
        (_, Some(1)) => "reduced",
        (_, Some(2)) => "full",
        (Some(1), _) if file_bytes.is_some() => "full",
        _ => "unknown",
    };
    let memory = r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management";
    let pagefiles = registry::read_multi_string(machine, memory, "PagingFiles")
        .unwrap_or_default()
        .iter()
        .filter_map(|value| {
            let text = value.trim().strip_prefix(r"\??\").unwrap_or(value.trim());
            let (filename, _) = text
                .split_once(".sys")
                .or_else(|| text.split_once(".SYS"))?;
            if filename.trim() == "?:" {
                return None;
            }
            let path = guard::validate_external_path(&format!("{filename}.sys")).ok()?;
            Some(PagefileEntry {
                size_bytes: file_size(&path),
                on_system_drive: on_system_drive(&path),
                path,
            })
        })
        .collect();
    let wsl_vhdx = wsl_paths(roots)
        .into_iter()
        .filter_map(|(name, path)| {
            let size_bytes = file_size(&path)?;
            Some(WslVhdxInfo {
                name,
                size_bytes,
                on_system_drive: on_system_drive(&path),
                path,
            })
        })
        .collect();
    DeepSlimState {
        hibernate: HibernateState { mode, file_bytes },
        pagefiles,
        wsl_vhdx,
        vss: vss_state(roots),
        winsxs: winsxs.clone(),
    }
}

pub fn execute(
    roots: &RootMap,
    action: DeepSlimAction,
    winsxs: &mut WinSxSState,
    progress: impl Fn(DeepSlimProgress),
) -> DeepSlimResult {
    let op = match action {
        DeepSlimAction::HibernateOff => SystemOp::HibernateOff,
        DeepSlimAction::HibernateReduced => SystemOp::HibernateReduced,
        DeepSlimAction::HibernateOn => SystemOp::HibernateOn,
        DeepSlimAction::DismAnalyze => SystemOp::DismAnalyze,
        DeepSlimAction::DismCleanup => SystemOp::DismComponentCleanup,
        DeepSlimAction::VssCleanOldest => SystemOp::VssCleanOldest,
        DeepSlimAction::WslCompact => SystemOp::WslCompact,
    };
    let free = || {
        roots
            .get(KnownRoot::SystemDrive)
            .map_or(0, volume::free_space)
    };
    let free_before = free();
    progress(DeepSlimProgress {
        action,
        percent: None,
        detail: "等待管理员授权并执行系统操作".into(),
        done: false,
    });
    let (result_status, detail) =
        match elevation::run_elevated(roots, &[op], elevation::RunOptions::default()) {
            Ok(result) => match result.outcomes.into_iter().find(|outcome| outcome.op == op) {
                Some(outcome) if outcome.success => {
                    if op == SystemOp::DismAnalyze {
                        *winsxs = parse_winsxs(&outcome.detail);
                    } else if op == SystemOp::DismComponentCleanup {
                        *winsxs = WinSxSState::default();
                    }
                    ("done", outcome.detail)
                }
                Some(outcome) => ("failed", outcome.detail),
                None => (
                    "failed",
                    result
                        .error
                        .unwrap_or_else(|| "管理员进程没有返回此操作结果".into()),
                ),
            },
            Err(ElevationError::Cancelled) => ("skipped", "已取消管理员授权".into()),
            Err(error) => ("failed", error.to_string()),
        };
    let free_after = free();
    let updated_state = status(roots, winsxs);
    progress(DeepSlimProgress {
        action,
        percent: (result_status == "done").then_some(100),
        detail: detail.clone(),
        done: true,
    });
    DeepSlimResult {
        action,
        status: result_status,
        detail,
        free_before,
        free_after,
        updated_state,
    }
}

fn file_size(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(win::to_verbatim(path)).ok()?;
    (metadata.is_file() && !metadata.is_symlink()).then_some(metadata.len())
}

fn vss_state(roots: &RootMap) -> VssState {
    let Some(windows) = roots.get(KnownRoot::Windows) else {
        return VssState::default();
    };
    let Some(system) = roots.get(KnownRoot::SystemDrive).and_then(Path::to_str) else {
        return VssState::default();
    };
    let Ok(system) = guard::validate_external_path(system) else {
        return VssState::default();
    };
    if system.as_os_str().len() != 3 {
        return VssState::default();
    }
    let drive = system.to_string_lossy();
    let tool = windows.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let script = format!(
        r#"$ErrorActionPreference='Stop'; $v=Get-CimInstance Win32_Volume -Filter ("DriveLetter='{}'"); if ($v) {{ $s=@(Get-CimInstance Win32_ShadowStorage | Where-Object {{ $_.Volume -like ('*'+($v.DeviceID.Split('{{','}}')[1])+'*') }}); $c=@(Get-CimInstance Win32_ShadowCopy | Where-Object {{ $_.VolumeName -eq $v.DeviceID }}); @{{usedBytes=[long](($s | Measure-Object UsedSpace -Sum).Sum);shadowCount=$c.Count}} | ConvertTo-Json -Compress }}"#,
        &drive[..2]
    );
    let output = command::run_clean(
        &tool,
        &["-NoProfile", "-NonInteractive", "-Command", &script],
        Duration::from_secs(8),
    );
    if output.exit_code == Some(0) {
        serde_json::from_str(output.stdout_tail.trim()).unwrap_or_default()
    } else {
        VssState::default()
    }
}

fn parse_winsxs(detail: &str) -> WinSxSState {
    let mut state = WinSxSState {
        detail: Some(detail.into()),
        ..WinSxSState::default()
    };
    for line in detail.lines() {
        let Some((label, value)) = line.split_once([':', '：']) else {
            continue;
        };
        let value = value.trim();
        if label.contains("Backups and Disabled Features") || label.contains("备份和禁用") {
            state.reclaimable_text = Some(value.into());
        }
        if label.contains("Cleanup Recommended") || label.contains("建议清理") {
            state.recommended = match value {
                "Yes" | "是" => Some(true),
                "No" | "否" => Some(false),
                _ => None,
            };
        }
    }
    state
}

fn wsl_paths(roots: &RootMap) -> Vec<(String, PathBuf)> {
    let Some(local) = roots.get(KnownRoot::LocalAppData) else {
        return Vec::new();
    };
    let Some(profile) = roots.get(KnownRoot::Profile) else {
        return Vec::new();
    };
    let lxss = r"Software\Microsoft\Windows\CurrentVersion\Lxss";
    let user = RegistryRoot::CurrentUser;
    let mut candidates = Vec::new();
    for subkey in registry::subkeys(user, lxss).unwrap_or_default() {
        let key = format!(r"{lxss}\{subkey}");
        let Some(base) = registry::read_string(user, &key, "BasePath") else {
            continue;
        };
        let Ok(base) = guard::validate_external_path(base.trim_start_matches(r"\\?\")) else {
            continue;
        };
        if !same_or_under(&base, profile) || same_or_under(profile, &base) {
            continue;
        }
        let name =
            registry::read_string(user, &key, "DistributionName").unwrap_or_else(|| "WSL".into());
        candidates.push((name, base.join("ext4.vhdx")));
    }
    for relative in [
        r"Docker\wsl\data\ext4.vhdx",
        r"Docker\wsl\disk\docker_data.vhdx",
        r"Docker\wsl\distro\ext4.vhdx",
    ] {
        candidates.push(("Docker Desktop".into(), local.join(relative)));
    }
    let mut forbidden = Forbidden::from_rules(roots);
    forbidden
        .names
        .retain(|name| !name.eq_ignore_ascii_case("LocalState"));
    candidates.sort_by(|left, right| left.1.cmp(&right.1));
    candidates.dedup_by(|left, right| left.1 == right.1);
    candidates.retain(|(_, target)| {
        guard::path::inspect_path(target, &forbidden)
            .is_ok_and(|entry| !entry.is_dir && entry.attributes & 1 == 0)
    });
    candidates
}

pub(crate) fn compact(roots: &RootMap, system32: &Path, whitelist: &[PathBuf]) -> OpOutcome {
    let op = SystemOp::WslCompact;
    let targets = wsl_paths(roots);
    let targets: Vec<_> = targets
        .into_iter()
        .filter(|(_, target)| {
            !whitelist
                .iter()
                .any(|excluded| same_or_under(target, excluded))
        })
        .collect();
    if targets.is_empty() {
        return OpOutcome::new(op, false, "没有找到可压缩的 WSL 或 Docker 虚拟磁盘");
    }
    let shutdown = command::run_clean(
        &system32.join("wsl.exe"),
        &["--shutdown"],
        Duration::from_secs(45),
    );
    if shutdown.exit_code != Some(0) || shutdown.timed_out {
        return OpOutcome::new(op, false, "WSL 未正常停止，未执行压缩");
    }
    let mut details = Vec::new();
    let mut success = true;
    let mut forbidden = Forbidden::from_rules(roots);
    forbidden
        .names
        .retain(|name| !name.eq_ignore_ascii_case("LocalState"));
    for (_, target) in targets {
        match guard::path::with_stable_file(&target, &forbidden, || win::compact_vhdx(&target)) {
            Ok(Ok(())) => details.push(format!("已压缩 {}", target.display())),
            Ok(Err(error)) => {
                success = false;
                details.push(format!("{} 压缩失败：{error}", target.display()));
            }
            Err(error) => {
                success = false;
                details.push(format!("{} 未通过路径检查：{error}", target.display()));
            }
        }
    }
    OpOutcome::new(op, success, details.join("\n"))
}
