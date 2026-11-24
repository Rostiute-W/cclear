use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::exec::file_cleaner::{Cleaner, FileSelection, Filters};
use crate::guard::{self, Forbidden};
use crate::model::{CleanItem, CleanSource, DeleteMode, KnownRoot};
use crate::rules::RootMap;
use crate::rules::schema::{PathSpec, RuleDef, Target, relative_parts};
use crate::win::{char_equals_ignore_case, equals_ignore_case};

#[derive(Clone)]
pub struct Candidate {
    pub item: CleanItem,
    pub target: Target,
    pub filters: Filters,
    pub processes: Vec<String>,
}

pub fn evaluate(
    rule: &RuleDef,
    id: String,
    roots: &RootMap,
    cleaner: &Cleaner<'_>,
    builtin: bool,
    temp_age: u32,
) -> Option<Candidate> {
    if !rule.detect_paths.is_empty()
        && rule
            .detect_paths
            .iter()
            .all(|spec| expand_spec(spec, roots, cleaner.forbidden).is_empty())
    {
        return None;
    }
    let mut filters = Filters {
        min_age_hours: rule.min_age_hours,
        exclude_newest_per_dir: rule.exclude_newest_per_dir,
        exclude_patterns: rule.exclude_patterns.clone(),
        extensions: rule.extensions.clone(),
    };
    if rule.id == "user-temp" {
        filters.min_age_hours = temp_age.max(24);
    }
    let (source, anchors, size) = match &rule.target {
        Target::Command { command } => {
            let paths = super::commands::cache_paths(*command, roots, cleaner).ok()?;
            let measured = cleaner.measure(&paths, &filters);
            (CleanSource::Command, paths, Some(measured))
        }
        Target::RecycleBin => {
            let (count, bytes) = crate::special::recycle_bin::query_c().ok()?;
            (
                CleanSource::RecycleBin,
                Vec::new(),
                Some(FileSelection {
                    bytes: bytes.max(0) as u64,
                    files: count.max(0) as u64,
                    samples: Vec::new(),
                }),
            )
        }
        Target::SystemOp { op } => {
            let (paths, system_filters) =
                crate::elevation::system_ops::file_targets(*op, roots, temp_age)?;
            if paths.iter().all(|path| {
                matches!(
                    guard::path::inspect_path(path, cleaner.forbidden),
                    Err(guard::GuardError::Gone)
                )
            }) {
                return None;
            }
            filters = system_filters;
            let measure = cleaner.measure(&paths, &filters);
            let accessible = paths
                .iter()
                .all(|path| guard::path::inspect_path(path, cleaner.forbidden).is_ok());
            (
                CleanSource::SystemOp,
                paths,
                if measure.files > 0 || accessible {
                    Some(measure)
                } else {
                    None
                },
            )
        }
        target => {
            let mut paths: Vec<PathBuf> = match target {
                Target::Paths { paths } => paths
                    .iter()
                    .flat_map(|spec| expand_spec(spec, roots, cleaner.forbidden))
                    .collect(),
                Target::ChromiumProfiles { base, rels } => {
                    profile_paths(base, rels, roots, cleaner.forbidden, true)
                }
                Target::FirefoxProfiles { base, rels } => {
                    profile_paths(base, rels, roots, cleaner.forbidden, false)
                }
                _ => unreachable!(),
            };
            paths.sort_unstable();
            paths.dedup();
            paths.retain(|path| !cleaner.whitelisted(path));
            if rule.delete_mode == DeleteMode::RecycleBin {
                if filters.min_age_hours != 0
                    || filters.exclude_newest_per_dir != 0
                    || !filters.exclude_patterns.is_empty()
                    || !filters.extensions.is_empty()
                {
                    return None;
                }
                paths.retain(|path| {
                    !cleaner
                        .whitelist
                        .iter()
                        .any(|excluded| super::forbidden::same_or_under(excluded, path))
                        && guard::path::protected_tree(path, cleaner.forbidden).is_ok()
                });
            }
            let measured = cleaner.measure(&paths, &filters);
            let source = if builtin {
                CleanSource::BuiltinRule
            } else {
                CleanSource::ExternalRule
            };
            (source, paths, Some(measured))
        }
    };
    if size.as_ref().is_some_and(|measure| measure.files == 0) {
        return None;
    }
    let item = CleanItem {
        id,
        name: rule.name.clone(),
        description: rule.description.clone(),
        category: rule.category,
        source,
        size_bytes: size.as_ref().map(|measure| measure.bytes),
        file_count: size.as_ref().map_or(0, |measure| measure.files),
        default_selected: builtin && rule.default_selected,
        requires_admin: rule.admin,
        delete_mode: rule.delete_mode,
        warning: rule.warning.clone(),
        running_processes: Vec::new(),
        sample_paths: size.map_or_else(Vec::new, |measure| measure.samples),
        anchors,
        last_modified_at: None,
    };
    Some(Candidate {
        item,
        target: rule.target.clone(),
        filters,
        processes: rule.processes.clone(),
    })
}

pub(crate) fn expand_spec(spec: &PathSpec, roots: &RootMap, forbidden: &Forbidden) -> Vec<PathBuf> {
    let Some(root) = roots.get(spec.root) else {
        return Vec::new();
    };
    expand(root, &spec.rels, forbidden)
}

fn expand(root: &Path, patterns: &[String], forbidden: &Forbidden) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for pattern in patterns {
        let Some(parts) = relative_parts(pattern) else {
            continue;
        };
        let mut paths = vec![root.to_path_buf()];
        for part in parts {
            let mut next = Vec::new();
            for base in paths {
                if part.contains(['*', '?']) {
                    let Ok(anchor) = guard::open_anchor(&base, forbidden) else {
                        continue;
                    };
                    let Ok(children) = guard::list_dir(&anchor, &[], forbidden) else {
                        continue;
                    };
                    for child in children {
                        if !child.is_reparse() && glob_matches(part, &child.name.to_string_lossy())
                        {
                            next.push(base.join(child.name));
                        }
                    }
                } else {
                    next.push(base.join(part));
                }
            }
            paths = next;
        }
        result.extend(
            paths
                .into_iter()
                .filter(|path| guard::path::inspect_path(path, forbidden).is_ok()),
        );
    }
    result
}

fn profile_paths(
    base: &PathSpec,
    rels: &[String],
    roots: &RootMap,
    forbidden: &Forbidden,
    chromium: bool,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for root in expand_spec(base, roots, forbidden) {
        let mut profiles = HashSet::new();
        if chromium {
            if let Ok(state) = guard::path::read_file(&root, "Local State", forbidden) {
                let json: Result<serde_json::Value, _> =
                    serde_json::from_reader(state.take(4 * 1024 * 1024));
                if let Ok(state) = json
                    && let Some(cache) = state
                        .pointer("/profile/info_cache")
                        .and_then(serde_json::Value::as_object)
                {
                    for name in cache.keys() {
                        if relative_parts(name).is_some() && !name.contains(['\\', '/', '*', '?']) {
                            profiles.insert(root.join(name));
                        }
                    }
                }
            }
            profiles.extend(expand(
                &root,
                &["Default".into(), "Profile *".into()],
                forbidden,
            ));
            profiles.insert(root);
        } else {
            profiles.extend(expand(&root, &["*".into()], forbidden));
        }
        for profile in profiles {
            if guard::open_anchor(&profile, forbidden).is_ok() {
                paths.extend(expand(&profile, rels, forbidden));
            }
        }
    }
    paths
}

pub fn glob_matches(pattern: &str, name: &str) -> bool {
    if !pattern.contains(['*', '?']) {
        return equals_ignore_case(pattern, name);
    }
    let pattern: Vec<_> = pattern.chars().collect();
    let name: Vec<_> = name.chars().collect();
    let (mut pattern_index, mut name_index) = (0, 0);
    let mut wildcard = None;
    while name_index < name.len() {
        match pattern.get(pattern_index) {
            Some('*') => {
                wildcard = Some((pattern_index + 1, name_index));
                pattern_index += 1;
            }
            Some(symbol)
                if *symbol == '?' || char_equals_ignore_case(*symbol, name[name_index]) =>
            {
                pattern_index += 1;
                name_index += 1;
            }
            _ => {
                let Some((resume, previous)) = wildcard else {
                    return false;
                };
                pattern_index = resume;
                name_index = previous + 1;
                wildcard = Some((resume, name_index));
            }
        }
    }
    pattern[pattern_index..].iter().all(|symbol| *symbol == '*')
}

pub(crate) fn ordinary_children(root: &Path, forbidden: &Forbidden) -> Vec<PathBuf> {
    expand(root, &["*".into()], forbidden)
        .into_iter()
        .filter(|path| guard::open_anchor(path, forbidden).is_ok())
        .collect()
}

pub(crate) fn detect_paths(root: &Path, rels: &[&str], forbidden: &Forbidden) -> Vec<PathBuf> {
    expand(
        root,
        &rels
            .iter()
            .map(|relative| (*relative).to_owned())
            .collect::<Vec<_>>(),
        forbidden,
    )
}

pub(crate) const APP_ROOTS: [KnownRoot; 2] = [KnownRoot::LocalAppData, KnownRoot::RoamingAppData];
