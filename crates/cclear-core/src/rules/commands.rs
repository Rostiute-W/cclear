use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::exec::{
    command::{self, CmdOutcome},
    file_cleaner::Cleaner,
};
use crate::guard::{path, validate_external_path};
use crate::model::KnownRoot;
use crate::rules::{RootMap, forbidden::same_or_under};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandId {
    Npm,
    Pnpm,
    Pip,
    UvPrune,
    UvClean,
    Conda,
    NugetHttpTemp,
    NugetGlobalPackages,
}

type Steps = &'static [&'static [&'static str]];

fn run_offline(tool: &Path, args: &[&str], timeout: Duration) -> CmdOutcome {
    let variables = [
        ("NPM_CONFIG_OFFLINE", "true".into()),
        ("NPM_CONFIG_UPDATE_NOTIFIER", "false".into()),
        ("COREPACK_ENABLE_NETWORK", "0".into()),
        ("PIP_DISABLE_PIP_VERSION_CHECK", "1".into()),
        ("UV_OFFLINE", "1".into()),
        ("CONDA_OFFLINE", "true".into()),
        ("DOTNET_CLI_TELEMETRY_OPTOUT", "1".into()),
    ];
    command::run_with_env(tool, args, timeout, &variables)
}

impl CommandId {
    fn spec(self) -> (&'static str, Steps, Steps) {
        match self {
            Self::Npm => (
                "npm",
                &[&["config", "get", "cache"]],
                &[&["cache", "clean", "--force"]],
            ),
            Self::Pnpm => ("pnpm", &[&["store", "path"]], &[&["store", "prune"]]),
            Self::Pip => ("pip", &[&["cache", "dir"]], &[&["cache", "purge"]]),
            Self::UvPrune => ("uv", &[&["cache", "dir"]], &[&["cache", "prune"]]),
            Self::UvClean => ("uv", &[&["cache", "dir"]], &[&["cache", "clean"]]),
            Self::Conda => (
                "conda",
                &[&["info", "--json"]],
                &[&[
                    "clean",
                    "--tarballs",
                    "--index-cache",
                    "--logfiles",
                    "--yes",
                ]],
            ),
            Self::NugetHttpTemp => (
                "dotnet",
                &[
                    &["nuget", "locals", "http-cache", "--list"],
                    &["nuget", "locals", "temp", "--list"],
                ],
                &[
                    &["nuget", "locals", "http-cache", "--clear"],
                    &["nuget", "locals", "temp", "--clear"],
                ],
            ),
            Self::NugetGlobalPackages => (
                "dotnet",
                &[&["nuget", "locals", "global-packages", "--list"]],
                &[&["nuget", "locals", "global-packages", "--clear"]],
            ),
        }
    }
}

pub fn cache_paths(
    id: CommandId,
    roots: &RootMap,
    cleaner: &Cleaner<'_>,
) -> Result<Vec<PathBuf>, String> {
    let (name, _, _) = id.spec();
    let tool = command::resolve_tool(name).ok_or_else(|| format!("找不到 {name}"))?;
    query(id, &tool, roots, cleaner)
}

pub fn execute(
    id: CommandId,
    roots: &RootMap,
    cleaner: &Cleaner<'_>,
    expected: &[PathBuf],
) -> Result<(Vec<PathBuf>, Vec<CmdOutcome>), String> {
    let (name, _, steps) = id.spec();
    let tool = command::resolve_tool(name).ok_or_else(|| format!("找不到 {name}"))?;
    let paths = query(id, &tool, roots, cleaner)?;
    if paths != expected {
        return Err("开发工具的缓存位置已改变，请重新扫描".into());
    }
    let mut outcomes = Vec::new();
    for step in steps {
        let outcome = run_offline(&tool, step, Duration::from_secs(600));
        let success = outcome.exit_code == Some(0) && !outcome.timed_out;
        outcomes.push(outcome);
        if !success {
            break;
        }
    }
    Ok((paths, outcomes))
}

fn query(
    id: CommandId,
    tool: &Path,
    roots: &RootMap,
    cleaner: &Cleaner<'_>,
) -> Result<Vec<PathBuf>, String> {
    if crate::win::is_elevated() {
        return Err("开发工具缓存命令需要以普通用户权限运行".into());
    }
    let (name, steps, _) = id.spec();
    let system = roots.get(KnownRoot::SystemDrive).ok_or("无法判断系统盘")?;
    let profile = roots.get(KnownRoot::Profile).ok_or("无法判断用户目录")?;
    let mut paths = Vec::new();
    for step in steps {
        let output = run_offline(tool, step, Duration::from_secs(20));
        if output.exit_code != Some(0) || output.timed_out {
            return Err(format!("查询 {name} 缓存失败"));
        }
        let reported: Vec<String> = if matches!(id, CommandId::Conda) {
            let value: serde_json::Value =
                serde_json::from_str(&output.stdout_tail).map_err(|_| "Conda 缓存清单无效")?;
            value["pkgs_dirs"]
                .as_array()
                .ok_or("缺少 Conda 缓存路径")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(ToOwned::to_owned)
                        .ok_or("Conda 缓存路径无效")
                })
                .collect::<Result<_, _>>()?
        } else {
            output
                .stdout_tail
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    let text = line.split_once(": ").map_or(line, |(_, suffix)| suffix);
                    (text.as_bytes().get(1) == Some(&b':')).then(|| text.to_owned())
                })
                .collect()
        };
        if reported.len() != 1 && !matches!(id, CommandId::Conda) || reported.is_empty() {
            return Err(format!("无法确定 {name} 的缓存路径"));
        }
        for text in reported {
            let target = validate_external_path(&text).map_err(|error| error.to_string())?;
            if !same_or_under(&target, system)
                || !same_or_under(&target, profile)
                || same_or_under(profile, &target)
                || !target.file_name().is_some_and(|leaf| {
                    let leaf = leaf.to_string_lossy().to_ascii_lowercase();
                    leaf.contains("cache")
                        || leaf.contains("store")
                        || ["pip", "uv", "pkgs", "packages", "nugetscratch"]
                            .contains(&leaf.as_str())
                })
                || cleaner.whitelist.iter().any(|excluded| {
                    same_or_under(excluded, &target) || same_or_under(&target, excluded)
                })
            {
                return Err(format!("缓存路径不在允许范围内：{}", target.display()));
            }
            match path::protected_tree(&target, cleaner.forbidden) {
                Ok(()) => paths.push(target),
                Err(crate::guard::GuardError::Gone) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    paths.sort_unstable();
    paths.dedup();
    if paths.is_empty() {
        return Err("缓存目录不存在".into());
    }
    Ok(paths)
}
