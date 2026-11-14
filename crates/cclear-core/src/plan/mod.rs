//! 冻结清单：预览时定下条目，执行时只按这份清单动手。

use std::collections::HashSet;
use std::ffi::OsString;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{CoreError, Result};
use crate::guard::{self, DirHandle, Forbidden};
use crate::model::{FileIdentity, Recovery, SizeInfo};
use crate::rules::catalog::{Rule, SelectKind, Selector, Target};
use crate::rules::forbidden as rules_forbidden;
use crate::rules::resolve::{
    RootMap, age_hours, descend_into, is_excluded_child_dir, keep_anchor_file, resolve_anchors,
    selector_of, system_time_ticks,
};
use crate::scan::{FileCount, classify_file};
use crate::win::dirinfo::EntryInfo;
use crate::win::process::ProcInfo;

/// 枚举时遇到 reparse 条目统一用这个原因，界面和日志都按它显示。
const LINK_SKIPPED: &str = "链接，已跳过";

/// 清单里的一个条目。`rel` 相对所属锚点，执行时按它逐级相对打开，不用字符串路径删除。
pub struct FrozenItem {
    pub item_id: u32,
    pub rule_id: &'static str,
    pub anchor_index: u16,
    pub rel: Vec<OsString>,
    pub is_dir: bool,
    pub identity: FileIdentity,
    pub size: u64,
    pub allocated: u64,
    pub mtime: u64,
}

/// 一条规则在清单里的部分。`items` 是 `FrozenPlan::items` 中的下标范围。
pub struct PlanRule {
    pub rule_id: &'static str,
    pub recovery: Recovery,
    pub anchors: Vec<PathBuf>,
    pub items: Range<u32>,
    pub total: SizeInfo,
    pub skipped_at_build: Vec<(PathBuf, String)>,
}

/// 一次预览定下的完整清单。
pub struct FrozenPlan {
    pub plan_id: Uuid,
    pub created_at: OffsetDateTime,
    pub rules: Vec<PlanRule>,
    pub items: Vec<FrozenItem>,
}

/// 按规则生成冻结清单。锚点打不开、条目读不到都只记录跳过，不中断其他规则。
pub fn build_plan(
    rules: &[&Rule],
    roots: &RootMap,
    forbidden: &Forbidden,
    procs: &[ProcInfo],
    now: SystemTime,
) -> Result<FrozenPlan> {
    let now_ticks = system_time_ticks(now);
    let mut items: Vec<FrozenItem> = Vec::new();
    let mut plan_rules = Vec::with_capacity(rules.len());

    for rule in rules {
        if !plannable(rule) {
            return Err(CoreError::RuleNotPlannable(rule.id));
        }

        let first_item = items.len() as u32;
        let anchors = resolve_anchors(rule, roots);
        let selector = selector_of(rule);
        let mut skipped_at_build: Vec<(PathBuf, String)> = Vec::new();
        let mut total = SizeInfo::default();
        // 硬链接的多个名字只进清单一次，和 resolve 的去重口径一致。
        let mut seen: HashSet<FileIdentity> = HashSet::new();

        for (index, anchor_path) in anchors.iter().enumerate() {
            let anchor = match guard::open_anchor(anchor_path, forbidden) {
                Ok(anchor) => anchor,
                Err(error) => {
                    skipped_at_build.push((anchor_path.clone(), error.to_string()));
                    continue;
                }
            };

            let mut walker = Walker {
                anchor: &anchor,
                anchor_path,
                // 锚点数量远小于 u16 上限，真实规则最多几十个。
                anchor_index: index as u16,
                volume_serial: anchor.identity().volume_serial,
                selector,
                forbidden,
                procs,
                now_ticks,
                seen: &mut seen,
                items: &mut items,
                skipped: &mut skipped_at_build,
                total: &mut total,
                rule_id: rule.id,
            };

            let mut rel = Vec::new();
            walker.walk(&mut rel, None);
        }

        plan_rules.push(PlanRule {
            rule_id: rule.id,
            recovery: rule.recovery,
            anchors,
            items: first_item..items.len() as u32,
            total,
            skipped_at_build,
        });
    }

    Ok(FrozenPlan {
        plan_id: Uuid::new_v4(),
        created_at: local_now(),
        rules: plan_rules,
        items,
    })
}

/// 只有文件类规则能生成清单：命令、特殊和系统操作各有自己的任务。
///
/// 隔离类规则也走冻结清单，只是执行时交给隔离引擎，而不是逐项删除。
fn plannable(rule: &Rule) -> bool {
    let recovery_ok = matches!(
        rule.recovery,
        Recovery::Regenerable | Recovery::IrreversibleFile | Recovery::Quarantine
    );
    let target_ok = matches!(
        rule.target,
        Target::Paths { .. } | Target::ChromiumProfiles { .. }
    );

    recovery_ok && target_ok
}

/// 当前本地时间。多线程进程里取本地时区可能失败，失败就退回 UTC，只影响时间显示。
fn local_now() -> OffsetDateTime {
    OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc())
}

/// 一个锚点内部的遍历状态。
struct Walker<'a> {
    anchor: &'a DirHandle,
    anchor_path: &'a Path,
    anchor_index: u16,
    volume_serial: u64,
    selector: Selector,
    forbidden: &'a Forbidden,
    procs: &'a [ProcInfo],
    now_ticks: u64,
    seen: &'a mut HashSet<FileIdentity>,
    items: &'a mut Vec<FrozenItem>,
    skipped: &'a mut Vec<(PathBuf, String)>,
    total: &'a mut SizeInfo,
    rule_id: &'static str,
}

/// 当前目录里被选中的候选文件。
struct ScannedFile {
    entry: EntryInfo,
    identity: FileIdentity,
    count: FileCount,
}

impl Walker<'_> {
    /// 递归遍历 `rel` 指向的目录。`own` 是父目录枚举时看到的自己，锚点自身为 None。
    fn walk(&mut self, rel: &mut Vec<OsString>, own: Option<&EntryInfo>) {
        let dir_path = join_all(self.anchor_path, rel);

        // 读不到的目录整棵跳过：看不到内容就不能进清单，但必须留下原因，
        // 否则清单会比评估统计少却看不出少了什么。
        let entries = match guard::list_dir(self.anchor, rel, self.forbidden) {
            Ok(entries) => entries,
            Err(error) => {
                self.skipped
                    .push((dir_path, format!("无法读取目录，已跳过：{error}")));
                return;
            }
        };

        let at_anchor = own.is_none();
        let mut files: Vec<ScannedFile> = Vec::new();
        let mut subdirs: Vec<EntryInfo> = Vec::new();

        for entry in entries {
            let path = dir_path.join(&entry.name);

            if entry.is_dir {
                // junction 和符号链接一旦跟进，清单就会落到锚点之外的卷上。
                if entry.is_reparse() {
                    self.skipped.push((path, LINK_SKIPPED.to_string()));
                    continue;
                }

                let name = entry.name.to_string_lossy();
                if rules_forbidden::is_forbidden_name(&name) {
                    self.skipped
                        .push((path, "组件属于禁区，已跳过".to_string()));
                    continue;
                }
                // 只有锚点这一层才用选择器筛子目录，和 resolve::collect_dirs 一致。
                if at_anchor && is_excluded_child_dir(&self.selector, &name) {
                    continue;
                }
                if at_anchor && !descend_into(&self.selector, &name) {
                    continue;
                }
                subdirs.push(entry);
                continue;
            }

            if at_anchor && !keep_anchor_file(&self.selector, &entry.name.to_string_lossy()) {
                continue;
            }

            // 和扫描器口径一致：符号链接这类 reparse 文件不算进任何统计。
            let count = classify_file(&entry);
            if count == FileCount::Skip {
                self.skipped.push((path, LINK_SKIPPED.to_string()));
                continue;
            }

            let identity = FileIdentity {
                volume_serial: self.volume_serial,
                file_id: entry.file_id,
            };
            // 硬链接的另一个名字：先见到的那个名字进清单，只进一次。
            if !self.seen.insert(identity) {
                continue;
            }
            files.push(ScannedFile {
                entry,
                identity,
                count,
            });
        }

        // 年龄和“每目录保留最新”要按整个目录判断，所以先收齐再筛。
        let keep = allowed_in_dir(&files, &self.selector, self.now_ticks);
        for (file, keep) in files.iter().zip(keep) {
            if !keep {
                continue;
            }

            let path = dir_path.join(&file.entry.name);
            if self.is_running_process(&path) {
                self.skipped.push((path, "正在运行的程序文件".to_string()));
                continue;
            }

            let mut item_rel = rel.clone();
            item_rel.push(file.entry.name.clone());
            self.items.push(FrozenItem {
                item_id: self.items.len() as u32,
                rule_id: self.rule_id,
                anchor_index: self.anchor_index,
                rel: item_rel,
                is_dir: false,
                identity: file.identity,
                size: file.entry.logical,
                allocated: file.entry.allocated,
                mtime: file.entry.last_write_time,
            });

            // 云占位符本地没有数据，逻辑大小不计入，和统计口径一致。
            if file.count != FileCount::AllocatedOnly {
                self.total.logical += file.entry.logical;
            }
            self.total.allocated += file.entry.allocated;
            self.total.files += 1;
        }

        for dir in &subdirs {
            rel.push(dir.name.clone());
            self.walk(rel, Some(dir));
            rel.pop();
        }

        // 目录按后序进清单：子项先处理完，父目录才不会因为还有计划内条目而留下。
        // TopFiles 只收锚点直属的文件，一个目录都不进清单。
        if let Some(own) = own
            && matches!(
                self.selector.kind,
                SelectKind::AllContents | SelectKind::ChildDirs(_)
            )
        {
            self.items.push(FrozenItem {
                item_id: self.items.len() as u32,
                rule_id: self.rule_id,
                anchor_index: self.anchor_index,
                rel: rel.clone(),
                is_dir: true,
                identity: FileIdentity {
                    volume_serial: self.volume_serial,
                    file_id: own.file_id,
                },
                size: own.logical,
                allocated: own.allocated,
                mtime: own.last_write_time,
            });
            self.total.dirs += 1;
        }
    }

    /// 文件的完整路径是否就是某个正在运行的进程映像。
    fn is_running_process(&self, path: &Path) -> bool {
        let text = path.to_string_lossy();

        self.procs.iter().any(|proc| {
            proc.image_path.as_ref().is_some_and(|image| {
                crate::win::equals_ignore_case(&image.to_string_lossy(), &text)
            })
        })
    }
}

/// 拼出锚点下的完整路径，只用于记录和展示，不参与文件操作。
fn join_all(base: &Path, rel: &[OsString]) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in rel {
        path.push(part);
    }
    path
}

/// 这个目录里哪些文件该进清单。口径与 `resolve::allowed_ids` 相同。
fn allowed_in_dir(files: &[ScannedFile], selector: &Selector, now_ticks: u64) -> Vec<bool> {
    let mut keep: Vec<bool> = files
        .iter()
        .map(|file| match selector.min_age_hours {
            Some(hours) => {
                age_hours(
                    file.entry.creation_time,
                    file.entry.last_write_time,
                    now_ticks,
                ) >= u64::from(hours)
            }
            None => true,
        })
        .collect();

    if selector.exclude_newest_per_dir {
        let newest = files.iter().map(|file| file.entry.last_write_time).max();
        if let Some(newest) = newest {
            // 每个目录里最新的那个文件始终保留，时间相同的也一起保留。
            for (file, keep) in files.iter().zip(keep.iter_mut()) {
                if file.entry.last_write_time == newest {
                    *keep = false;
                }
            }
        }
    }

    keep
}
