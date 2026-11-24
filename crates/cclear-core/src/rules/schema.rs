use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::elevation::SystemOp;
use crate::guard::{Forbidden, GuardError, path};
use crate::model::{CleanCategory, DeleteMode, KnownRoot};
use crate::rules::{RootMap, commands::CommandId};

pub const MAX_PACK_BYTES: usize = 2 * 1024 * 1024;
const BUILTIN: &str = include_str!("../../rules/builtin.json");
const SAFE_DIRECT_LEAVES: &[&str] = &[
    "Cache",
    "Cache_Data",
    "Code Cache",
    "GPUCache",
    "GrShaderCache",
    "ShaderCache",
    "DawnCache",
    "logs",
    "log",
    "Temp",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RulePack {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub rules: Vec<RuleDef>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleDef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: CleanCategory,
    #[serde(default)]
    pub default_selected: bool,
    #[serde(default)]
    pub admin: bool,
    pub delete_mode: DeleteMode,
    pub warning: Option<String>,
    #[serde(default)]
    pub processes: Vec<String>,
    #[serde(default)]
    pub detect_paths: Vec<PathSpec>,
    pub target: Target,
    #[serde(default)]
    pub min_age_hours: u32,
    #[serde(default)]
    pub exclude_newest_per_dir: usize,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PathSpec {
    pub root: KnownRoot,
    pub rels: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Target {
    Paths { paths: Vec<PathSpec> },
    ChromiumProfiles { base: PathSpec, rels: Vec<String> },
    FirefoxProfiles { base: PathSpec, rels: Vec<String> },
    Command { command: CommandId },
    RecycleBin,
    SystemOp { op: SystemOp },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RulePackInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub rule_count: usize,
    pub builtin: bool,
    pub rules_summary: Vec<RulePackRuleSummary>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RulePackRuleSummary {
    pub id: String,
    pub name: String,
    pub delete_mode: DeleteMode,
    pub targets: Vec<String>,
}

impl RulePack {
    pub fn info(&self, builtin: bool) -> RulePackInfo {
        RulePackInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            rule_count: self.rules.len(),
            builtin,
            rules_summary: self
                .rules
                .iter()
                .map(|rule| RulePackRuleSummary {
                    id: rule.id.clone(),
                    name: rule.name.clone(),
                    delete_mode: rule.delete_mode,
                    targets: rule.target.summary(),
                })
                .collect(),
        }
    }
}

impl PathSpec {
    fn summary_paths(&self) -> impl Iterator<Item = String> + '_ {
        let root = serde_json::to_string(&self.root).expect("目录锚点序列化失败");
        self.rels.iter().map(move |relative| {
            format!(
                "{}\\{}",
                root.trim_matches('"'),
                relative.replace('/', "\\")
            )
        })
    }
}

impl Target {
    fn summary(&self) -> Vec<String> {
        match self {
            Self::Paths { paths } => paths.iter().flat_map(PathSpec::summary_paths).collect(),
            Self::ChromiumProfiles { base, rels } | Self::FirefoxProfiles { base, rels } => {
                let profile = if matches!(self, Self::ChromiumProfiles { .. }) {
                    "<基目录或配置目录>"
                } else {
                    "<配置目录>"
                };
                let mut targets: Vec<_> = base
                    .summary_paths()
                    .map(|path| format!("基目录：{path}"))
                    .collect();
                for relative in rels {
                    targets.push(format!("{profile}\\{}", relative.replace('/', "\\")));
                }
                targets
            }
            Self::Command { command } => {
                let token = serde_json::to_string(command).expect("命令序列化失败");
                vec![format!("command: {}", token.trim_matches('"'))]
            }
            Self::SystemOp { op } => vec![format!("systemOp: {}", op.token())],
            Self::RecycleBin => vec!["回收站".into()],
        }
    }
}

pub fn builtin() -> &'static RulePack {
    static PACK: OnceLock<RulePack> = OnceLock::new();
    PACK.get_or_init(|| serde_json::from_str(BUILTIN).expect("内置规则 JSON 无效"))
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !["con", "prn", "aux", "nul"].contains(&id)
        && !(id.len() == 4
            && (id.starts_with("com") || id.starts_with("lpt"))
            && id.as_bytes()[3].is_ascii_digit())
}

pub fn parse_external(json: &str, roots: &RootMap) -> Result<RulePack, String> {
    if json.len() > MAX_PACK_BYTES {
        return Err("规则包超过 2 MiB".into());
    }
    let pack: RulePack =
        serde_json::from_str(json).map_err(|error| format!("规则包格式错误：{error}"))?;
    if !valid_id(&pack.id)
        || pack.id == builtin().id
        || pack.rules.is_empty()
        || pack.rules.len() > 500
    {
        return Err("规则包 ID 或条目数量无效".into());
    }
    if pack.name.chars().count() > 120
        || pack.version.chars().count() > 32
        || pack.description.chars().count() > 500
    {
        return Err("规则包名称、版本或描述过长".into());
    }
    let forbidden = Forbidden::from_rules(roots);
    let mut ids = HashSet::new();
    for rule in &pack.rules {
        if !valid_id(&rule.id)
            || !ids.insert(&rule.id)
            || rule.admin
            || rule.category == CleanCategory::Leftover
                && rule.delete_mode != DeleteMode::RecycleBin
            || !matches!(
                rule.delete_mode,
                DeleteMode::Direct | DeleteMode::RecycleBin
            )
        {
            return Err(format!("规则「{}」的 ID、权限或删除模式无效", rule.id));
        }
        if rule.name.chars().count() > 120
            || rule.description.chars().count() > 500
            || rule
                .warning
                .as_ref()
                .is_some_and(|warning| warning.chars().count() > 500)
        {
            return Err(format!("规则「{}」的名称、描述或警告过长", rule.id));
        }
        for process in &rule.processes {
            let is_exe = process.rsplit_once('.').is_some_and(|(stem, extension)| {
                !stem.is_empty() && extension.eq_ignore_ascii_case("exe")
            });
            if !is_exe
                || process.contains(['\\', '/', '*', '?'])
                || relative_parts(process).is_none()
                || crate::win::process::is_protected(process)
            {
                return Err(format!("规则「{}」包含无效或受保护的进程名", rule.id));
            }
        }
        let target_paths = match &rule.target {
            Target::Paths { paths } if !paths.is_empty() => paths.as_slice(),
            Target::ChromiumProfiles { base, rels } | Target::FirefoxProfiles { base, rels } => {
                validate_rels(rels, None, &forbidden)?;
                std::slice::from_ref(base)
            }
            _ => return Err(format!("规则「{}」必须使用文件路径目标", rule.id)),
        };
        for spec in target_paths.iter().chain(&rule.detect_paths) {
            if !matches!(
                spec.root,
                KnownRoot::LocalAppData | KnownRoot::RoamingAppData
            ) {
                return Err("外置规则仅允许本地和漫游应用数据目录锚点".into());
            }
            validate_rels(&spec.rels, roots.get(spec.root), &forbidden)?;
        }
        if rule.delete_mode == DeleteMode::Direct {
            let all_cache_targets = match &rule.target {
                Target::Paths { .. } => target_paths
                    .iter()
                    .flat_map(|spec| &spec.rels)
                    .all(|relative| has_safe_direct_leaf(relative)),
                Target::ChromiumProfiles { rels, .. } | Target::FirefoxProfiles { rels, .. } => {
                    rels.iter().all(|relative| has_safe_direct_leaf(relative))
                }
                _ => false,
            };
            if !all_cache_targets {
                return Err(format!("规则「{}」的目标必须使用回收站", rule.id));
            }
        }
        for pattern in rule.exclude_patterns.iter().chain(&rule.extensions) {
            if relative_parts(pattern).is_none() || pattern.contains(['\\', '/']) {
                return Err("排除模式或扩展名无效".into());
            }
        }
    }
    Ok(pack)
}

fn has_safe_direct_leaf(relative: &str) -> bool {
    let mut parts = relative.rsplit(['\\', '/']);
    let leaf = parts.next().unwrap_or_default();
    SAFE_DIRECT_LEAVES
        .iter()
        .any(|safe| leaf.eq_ignore_ascii_case(safe))
        || leaf.eq_ignore_ascii_case("reports")
            && parts
                .next()
                .is_some_and(|parent| parent.eq_ignore_ascii_case("Crashpad"))
}

pub(crate) fn relative_parts(value: &str) -> Option<impl Iterator<Item = &str>> {
    let parts = value.split(['\\', '/']);
    if parts.clone().any(|part| {
        part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with([' ', '.'])
            || part.contains([':', '<', '>', '"', '|'])
            || part.chars().any(char::is_control)
            || part.eq_ignore_ascii_case("GLOBALROOT")
    }) {
        return None;
    }
    Some(parts)
}

fn validate_rels(
    rels: &[String],
    base: Option<&Path>,
    forbidden: &Forbidden,
) -> Result<(), String> {
    if rels.is_empty() {
        return Err("规则必须指定至少一个非空相对路径".into());
    }
    for relative in rels {
        let mut parts = relative_parts(relative)
            .ok_or_else(|| format!("非法相对路径：{relative}"))?
            .peekable();
        let first = parts.next().unwrap_or_default();
        if first.contains(['*', '?']) || parts.peek().is_none() {
            return Err(format!(
                "规则路径至少需要两级，且首级不能使用通配符：{relative}"
            ));
        }
        let mut static_path = base.map(Path::to_path_buf);
        for part in std::iter::once(first).chain(parts) {
            if forbidden.protects_name(part) {
                return Err(format!("规则路径包含受保护名称：{part}"));
            }
            if part.contains(['*', '?']) {
                static_path = None;
            }
            if let Some(current) = &mut static_path {
                current.push(part);
                if let Err(GuardError::Forbidden(_) | GuardError::ReparseInPath(_)) =
                    path::inspect_path(current, forbidden)
                {
                    return Err(format!(
                        "规则路径受保护或包含重解析点：{}",
                        current.display()
                    ));
                }
            }
        }
    }
    Ok(())
}
