use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::guard::{self, DirHandle, Expect, Forbidden, GuardError, OpenIntent};
use crate::model::FileIdentity;
use crate::rules::{engine::glob_matches, forbidden::same_or_under};

#[derive(Clone, Debug, Default)]
pub struct Filters {
    pub min_age_hours: u32,
    pub exclude_newest_per_dir: usize,
    pub exclude_patterns: Vec<String>,
    pub extensions: Vec<String>,
}

#[derive(Default)]
pub struct FileSelection {
    pub bytes: u64,
    pub files: u64,
    pub samples: Vec<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReport {
    pub freed_bytes: u64,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub skipped_files: u64,
    pub locked: Vec<PathBuf>,
    pub detail: Option<String>,
}

pub struct Cleaner<'a> {
    pub forbidden: &'a Forbidden,
    pub whitelist: &'a [PathBuf],
    pub cancel: &'a AtomicBool,
}

impl Cleaner<'_> {
    pub fn measure(&self, paths: &[PathBuf], filters: &Filters) -> FileSelection {
        let mut result = FileSelection::default();
        let mut seen = HashSet::new();
        for path in paths {
            let _ = self.walk(
                path,
                filters,
                false,
                |_, _, full_path, expected, allocated, is_dir| {
                    if !is_dir && seen.insert(expected.identity) {
                        result.bytes = result.bytes.saturating_add(allocated);
                        result.files += 1;
                        if result.samples.len() < 10 {
                            result.samples.push(full_path.to_path_buf());
                        }
                    }
                },
            );
        }
        result
    }

    pub fn clean(&self, paths: &[PathBuf], filters: &Filters) -> FileReport {
        let mut result = FileReport::default();
        for path in paths {
            let completed = self.walk(
                path,
                filters,
                true,
                |anchor, relative, full_path, expected, _, is_dir| {
                    let deleted =
                        guard::open_rel(anchor, relative, OpenIntent::Delete, self.forbidden)
                            .and_then(|entry| guard::path::delete_with_release(entry, &expected));
                    match deleted {
                        Ok(_) if is_dir => result.deleted_dirs += 1,
                        Ok(bytes) => {
                            result.deleted_files += 1;
                            result.freed_bytes = result.freed_bytes.saturating_add(bytes);
                        }
                        Err(GuardError::DirNotEmpty) if is_dir => {}
                        Err(error) => {
                            if matches!(error, GuardError::Locked)
                                && !is_dir
                                && result.locked.len() < 1000
                            {
                                result.locked.push(full_path.to_path_buf());
                            }
                            result.skipped_files += u64::from(!is_dir);
                            result.detail.get_or_insert_with(|| error.to_string());
                        }
                    }
                },
            );
            if let Err(error) = completed {
                result.skipped_files += 1;
                result.detail.get_or_insert_with(|| error.to_string());
            }
        }
        result
    }

    fn walk(
        &self,
        path: &Path,
        filters: &Filters,
        include_dirs: bool,
        mut visit: impl FnMut(&DirHandle, &[OsString], &Path, Expect, u64, bool),
    ) -> Result<(), GuardError> {
        if self.cancel.load(Ordering::Relaxed) || self.whitelisted(path) {
            return Ok(());
        }
        let parent = path.parent().ok_or(GuardError::InvalidName)?;
        let name = path.file_name().ok_or(GuardError::InvalidName)?;
        let parent_handle = guard::open_anchor(parent, self.forbidden)?;
        let relative = vec![name.to_os_string()];
        let target = guard::open_rel(
            &parent_handle,
            &relative,
            OpenIntent::Inspect,
            self.forbidden,
        )?;
        if !target.is_dir {
            if filters.matches_file(name.to_string_lossy().as_ref(), target.mtime) {
                visit(
                    &parent_handle,
                    &relative,
                    path,
                    target.expect(),
                    target.allocated,
                    false,
                );
            }
            return Ok(());
        }
        let anchor = guard::open_anchor(path, self.forbidden)?;
        if target.identity != anchor.identity() {
            return Err(GuardError::Changed);
        }
        let mut pending = vec![(Vec::new(), None)];
        let mut first_error = None;
        while let Some((relative, directory)) = pending.pop() {
            if self.cancel.load(Ordering::Relaxed) {
                break;
            }
            let full_path = relative
                .iter()
                .fold(path.to_path_buf(), |base, name| base.join(name));
            if let Some(expected) = directory {
                visit(&anchor, &relative, &full_path, expected, 0, true);
                continue;
            }
            let mut children = match guard::list_dir(&anchor, &relative, self.forbidden) {
                Ok(children) => children,
                Err(error) => {
                    first_error.get_or_insert(error);
                    continue;
                }
            };
            children.sort_unstable_by(|left, right| {
                right
                    .last_write_time
                    .cmp(&left.last_write_time)
                    .then_with(|| left.name.cmp(&right.name))
            });
            let mut file_index = 0;
            for child in children {
                if self.cancel.load(Ordering::Relaxed) {
                    break;
                }
                let child_path = full_path.join(&child.name);
                let name = child.name.to_string_lossy();
                let identity = FileIdentity {
                    volume_serial: anchor.identity().volume_serial,
                    file_id: child.file_id,
                };
                if child.is_reparse()
                    || self.forbidden.matches(&child.name, &identity)
                    || self.whitelisted(&child_path)
                    || filters
                        .exclude_patterns
                        .iter()
                        .any(|pattern| glob_matches(pattern, &name))
                {
                    continue;
                }
                let mut next = relative.clone();
                next.push(child.name.clone());
                let expected = Expect {
                    identity,
                    size: child.logical,
                    mtime: child.last_write_time,
                };
                if child.is_dir {
                    if include_dirs && filters.min_age_hours == 0 {
                        pending.push((next.clone(), Some(expected)));
                    }
                    pending.push((next, None));
                } else {
                    file_index += 1;
                    if file_index > filters.exclude_newest_per_dir
                        && filters.matches_file(&name, child.last_write_time)
                    {
                        visit(
                            &anchor,
                            &next,
                            &child_path,
                            expected,
                            child.allocated,
                            false,
                        );
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub fn whitelisted(&self, path: &Path) -> bool {
        self.whitelist
            .iter()
            .any(|protected| same_or_under(path, protected))
    }
}

impl Filters {
    pub(crate) fn matches_file(&self, name: &str, last_write: u64) -> bool {
        let age = u64::from(self.min_age_hours) * 3600;
        let effective_now = crate::journal::unix_now().max(0) as u64;
        if age > 0 && filetime_unix(last_write).saturating_add(age) > effective_now {
            return false;
        }
        if self
            .exclude_patterns
            .iter()
            .any(|pattern| glob_matches(pattern, name))
        {
            return false;
        }
        self.extensions.is_empty()
            || name.rsplit_once('.').is_some_and(|(_, extension)| {
                self.extensions
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(extension))
            })
    }
}

pub fn filetime_unix(ticks: u64) -> u64 {
    (ticks / 10_000_000).saturating_sub(11_644_473_600)
}
