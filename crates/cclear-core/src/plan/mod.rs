//! 冻结清单：预览时定下条目，执行时只按这份清单动手。

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{CoreError, Result};
use crate::guard::{self, DirHandle, Forbidden, OpenIntent};
use crate::model::{FileIdentity, Recovery, SizeInfo};
use crate::rules::adapters::{self, SystemVersionProbe};
use crate::rules::catalog::{AdapterId, Rule, SelectKind, Selector, Target};
use crate::rules::forbidden as rules_forbidden;
use crate::rules::resolve::{
    CandidateStatus, RootMap, age_hours, descend_into, is_excluded_child_dir, keep_anchor_file,
    resolve_anchors, selector_of, system_time_ticks,
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
    /// 执行前必须确认已经退出的进程。迁移这类临时规则不在规则目录里，
    /// 进程名只能随清单一起带过来。
    pub processes: Vec<&'static str>,
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
    // 进程映像的文件身份只在这里解析一次，逐条规则、逐个文件都不用再打开映像。
    let images = RunningImages::new(procs);

    for rule in rules {
        if !plannable(rule) {
            return Err(CoreError::RuleNotPlannable(rule.id));
        }

        let first_item = items.len() as u32;
        let mut skipped_at_build: Vec<(PathBuf, String)> = Vec::new();
        let mut total = SizeInfo::default();
        // 硬链接的多个名字只进清单一次，和 resolve 的去重口径一致。
        let mut seen: HashSet<FileIdentity> = HashSet::new();

        let anchors = {
            let mut state = PlanBuildState {
                items: &mut items,
                skipped: &mut skipped_at_build,
                total: &mut total,
                seen: &mut seen,
                images: &images,
            };

            match &rule.target {
                // 适配器规则的候选由适配器按证据给出，不走路径模板。
                Target::Adapter(id) => {
                    expand_adapter(rule, *id, roots, forbidden, procs, now, &mut state)
                }
                _ => {
                    let anchors = resolve_anchors(rule, roots);
                    walk_anchors(&anchors, rule, forbidden, now_ticks, &mut state);
                    anchors
                }
            }
        };

        plan_rules.push(PlanRule {
            rule_id: rule.id,
            recovery: rule.recovery,
            anchors,
            items: first_item..items.len() as u32,
            total,
            skipped_at_build,
            processes: rule.processes.to_vec(),
        });
    }

    Ok(FrozenPlan {
        plan_id: Uuid::new_v4(),
        created_at: local_now(),
        rules: plan_rules,
        items,
    })
}

/// 生成清单时的筛选条件。普通规则直接用规则的 `Selector`；
/// 需要按扩展名筛选或只收文件的操作（helper 的系统维护）用它描述。
pub struct PlanOptions {
    pub selector: Selector,
    /// 只收这些扩展名的文件，比较时不区分大小写。空表示不限扩展名。
    pub extensions: &'static [&'static str],
    /// 子目录是否也进清单。只清文件的操作填 false，免得顺带删掉别的空目录。
    pub keep_dirs: bool,
}

/// 全部内容：递归、不限年龄、目录也进清单。
const ALL_CONTENTS: PlanOptions = PlanOptions {
    selector: Selector {
        kind: SelectKind::AllContents,
        min_age_hours: None,
        exclude_newest_per_dir: false,
        exclude_child_dirs: &[],
    },
    extensions: &[],
    keep_dirs: true,
};

/// 按给定的锚点生成冻结清单。
///
/// 迁移这类锚点来自运行时查询（当前生效的缓存目录）的流程用它：没有规则模板，
/// 但和普通规则共用同一套枚举、禁区检查和条目记录，执行时同样只认这份清单。
/// 锚点自身不进清单，只收它下面的内容。
pub fn build_plan_at(
    rule_id: &'static str,
    recovery: Recovery,
    anchors: &[PathBuf],
    forbidden: &Forbidden,
    procs: &[ProcInfo],
    now: SystemTime,
) -> FrozenPlan {
    build_plan_selected(
        rule_id,
        recovery,
        anchors,
        ALL_CONTENTS,
        forbidden,
        procs,
        now,
    )
}

/// 同 [`build_plan_at`]，但可以用更窄的筛选条件。
///
/// 需要管理员权限的系统维护操作（Windows\Temp、Windows\Logs）用它：只清指定年龄、
/// 指定扩展名的文件，清单之外的一个都不碰。
pub fn build_plan_selected(
    rule_id: &'static str,
    recovery: Recovery,
    anchors: &[PathBuf],
    options: PlanOptions,
    forbidden: &Forbidden,
    procs: &[ProcInfo],
    now: SystemTime,
) -> FrozenPlan {
    let now_ticks = system_time_ticks(now);
    let images = RunningImages::new(procs);

    let mut items: Vec<FrozenItem> = Vec::new();
    let mut skipped: Vec<(PathBuf, String)> = Vec::new();
    let mut total = SizeInfo::default();
    let mut seen: HashSet<FileIdentity> = HashSet::new();

    for (index, anchor_path) in anchors.iter().enumerate() {
        let anchor = match guard::open_anchor(anchor_path, forbidden) {
            Ok(anchor) => anchor,
            Err(error) => {
                skipped.push((anchor_path.clone(), error.to_string()));
                continue;
            }
        };

        let mut walker = Walker {
            anchor: &anchor,
            anchor_path,
            // 锚点数量远小于 u16 上限。
            anchor_index: index as u16,
            volume_serial: anchor.identity().volume_serial,
            selector: options.selector,
            extensions: options.extensions,
            keep_dirs: options.keep_dirs,
            forbidden,
            images: &images,
            now_ticks,
            seen: &mut seen,
            items: &mut items,
            skipped: &mut skipped,
            total: &mut total,
            rule_id,
        };

        let mut rel = Vec::new();
        walker.walk(&mut rel, None);
    }

    let count = items.len() as u32;

    FrozenPlan {
        plan_id: Uuid::new_v4(),
        created_at: local_now(),
        rules: vec![PlanRule {
            rule_id,
            recovery,
            anchors: anchors.to_vec(),
            items: 0..count,
            total,
            skipped_at_build: skipped,
            // 调用方可以填上自己的进程名（迁移清理清单就这么做）。
            processes: Vec::new(),
        }],
        items,
    }
}

/// 生成一条规则的条目时共用的可变状态。
struct PlanBuildState<'a> {
    items: &'a mut Vec<FrozenItem>,
    skipped: &'a mut Vec<(PathBuf, String)>,
    total: &'a mut SizeInfo,
    seen: &'a mut HashSet<FileIdentity>,
    /// 正在运行的进程映像，整次清单生成只解析一次。
    images: &'a RunningImages,
}

/// 按路径模板的锚点逐棵遍历，把条目收进清单。
fn walk_anchors(
    anchors: &[PathBuf],
    rule: &Rule,
    forbidden: &Forbidden,
    now_ticks: u64,
    state: &mut PlanBuildState,
) {
    let PlanBuildState {
        items,
        skipped,
        total,
        seen,
        images,
    } = state;
    let images = *images;
    let selector = selector_of(rule);

    for (index, anchor_path) in anchors.iter().enumerate() {
        let anchor = match guard::open_anchor(anchor_path, forbidden) {
            Ok(anchor) => anchor,
            Err(error) => {
                skipped.push((anchor_path.clone(), error.to_string()));
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
            extensions: &[],
            keep_dirs: true,
            forbidden,
            images,
            now_ticks,
            seen,
            items,
            skipped,
            total,
            rule_id: rule.id,
        };

        let mut rel = Vec::new();
        walker.walk(&mut rel, None);
    }
}

/// 适配器规则：把适配器给出的条目展开成冻结清单。
///
/// 目录展开成整棵子树。隔离引擎只复制文件，所以子树里的目录不进清单，只收文件；
/// 文件都隔离之后留下的空目录不在这里处理，也不退回去直接删除。
fn expand_adapter(
    rule: &Rule,
    id: AdapterId,
    roots: &RootMap,
    forbidden: &Forbidden,
    procs: &[ProcInfo],
    now: SystemTime,
    state: &mut PlanBuildState,
) -> Vec<PathBuf> {
    let PlanBuildState {
        items,
        skipped,
        total,
        seen,
        images,
    } = state;
    let images = *images;
    let now_ticks = system_time_ticks(now);

    // 预览和执行之间状态可能已经变了，这时一个条目都不生成。
    let result = adapters::evaluate_adapter(id, roots, procs, &SystemVersionProbe, now);
    if result.status != CandidateStatus::Ready {
        skipped.push((
            PathBuf::new(),
            format!(
                "证据不足，没有生成条目：{}",
                result.status.detail().unwrap_or_default()
            ),
        ));
        return Vec::new();
    }

    let mut anchors: Vec<PathBuf> = Vec::new();

    for item in &result.items {
        let anchor_index = match anchors
            .iter()
            .position(|known| same_path(known, &item.anchor))
        {
            Some(index) => index,
            None => {
                anchors.push(item.anchor.clone());
                anchors.len() - 1
            }
        };

        let anchor = match guard::open_anchor(&item.anchor, forbidden) {
            Ok(anchor) => anchor,
            Err(error) => {
                skipped.push((item.anchor.clone(), error.to_string()));
                continue;
            }
        };

        if item.is_dir {
            let mut walker = Walker {
                anchor: &anchor,
                anchor_path: &item.anchor,
                anchor_index: anchor_index as u16,
                volume_serial: anchor.identity().volume_serial,
                selector: selector_of(rule),
                extensions: &[],
                keep_dirs: false,
                forbidden,
                images,
                now_ticks,
                seen,
                items,
                skipped,
                total,
                rule_id: rule.id,
            };

            let mut rel = item.rel.clone();
            walker.walk(&mut rel, None);
            continue;
        }

        // 单个文件：按条目打开一次，确认它确实是普通文件再进清单。
        let entry = match guard::open_rel(&anchor, &item.rel, OpenIntent::Inspect, forbidden) {
            Ok(entry) => entry,
            Err(error) => {
                skipped.push((join_all(&item.anchor, &item.rel), error.to_string()));
                continue;
            }
        };
        if entry.is_dir {
            skipped.push((
                join_all(&item.anchor, &item.rel),
                "条目实际是目录，已跳过".to_string(),
            ));
            continue;
        }

        // 正在运行的程序文件不能动，这一条和目录分支的口径一致。
        let path = join_all(&item.anchor, &item.rel);
        if images.contains(entry.identity, &path) {
            skipped.push((path, "正在运行的程序文件".to_string()));
            continue;
        }

        // 同一文件的多个硬链接名字都要留在清单里：只隔离其中一个名字释放不了空间。
        // 大小只计一次，和扫描器的去重口径一致。
        let counted_before = !seen.insert(entry.identity);

        items.push(FrozenItem {
            item_id: items.len() as u32,
            rule_id: rule.id,
            anchor_index: anchor_index as u16,
            rel: item.rel.clone(),
            is_dir: false,
            identity: entry.identity,
            size: entry.size,
            allocated: entry.allocated,
            mtime: entry.mtime,
        });

        if !counted_before {
            total.logical += entry.size;
            total.allocated += entry.allocated;
        }
        total.files += 1;
    }

    anchors
}

/// 两个路径是否相同。Windows 路径不区分大小写。
fn same_path(left: &Path, right: &Path) -> bool {
    crate::win::equals_ignore_case(&left.to_string_lossy(), &right.to_string_lossy())
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
        Target::Paths { .. } | Target::ChromiumProfiles { .. } | Target::Adapter(_)
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
    /// 只收这些扩展名的文件。空表示不限。
    extensions: &'a [&'a str],
    /// 子目录是否也进清单。适配器展开的子树只收文件，目录交给用户自己处理。
    keep_dirs: bool,
    forbidden: &'a Forbidden,
    images: &'a RunningImages,
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

            // 扩展名不在范围内只是不选，不是跳过原因，所以不记录。
            if !extension_allowed(&entry.name, self.extensions) {
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
            if self.is_running_process(file.identity, &path) {
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
        if self.keep_dirs
            && let Some(own) = own
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
    fn is_running_process(&self, identity: FileIdentity, path: &Path) -> bool {
        self.images.contains(identity, path)
    }
}

/// 正在运行的程序映像，按文件身份判断。
///
/// 同一个文件可以有多个硬链接名字（本机就有一对 `xxx.exe` / `xxx.exe.old-*`），
/// 只比路径会漏掉其中一个名字，所以能打开映像文件时按“卷序列号 + 文件 ID”比较。
struct RunningImages {
    /// 能打开映像文件时记下的身份。
    identities: HashSet<FileIdentity>,
    /// 打不开的映像（受保护进程等）只能退回按路径比较。
    paths: Vec<PathBuf>,
}

impl RunningImages {
    fn new(procs: &[ProcInfo]) -> Self {
        let mut identities = HashSet::new();
        let mut paths = Vec::new();

        for proc in procs {
            let Some(image) = proc.image_path.as_ref() else {
                continue;
            };

            match file_identity(image) {
                Some(identity) => {
                    identities.insert(identity);
                }
                None => paths.push(image.clone()),
            }
        }

        Self { identities, paths }
    }

    /// `identity` 是 `path` 的文件身份。硬链接的另一个名字也能认出来。
    fn contains(&self, identity: FileIdentity, path: &Path) -> bool {
        if self.identities.contains(&identity) {
            return true;
        }

        let text = path.to_string_lossy();
        self.paths
            .iter()
            .any(|image| crate::win::equals_ignore_case(&image.to_string_lossy(), &text))
    }
}

/// 取一个路径的文件身份，跟随链接。打不开时返回 None。
fn file_identity(path: &Path) -> Option<FileIdentity> {
    let handle = crate::win::dirinfo::open_following_links(path).ok()?;

    crate::guard::path::identity_of(&handle).ok()
}

/// 拼出锚点下的完整路径，只用于记录和展示，不参与文件操作。
fn join_all(base: &Path, rel: &[OsString]) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in rel {
        path.push(part);
    }
    path
}

/// 文件名是否落在允许的扩展名集合里。空集合表示不限扩展名。
fn extension_allowed(name: &OsStr, extensions: &[&str]) -> bool {
    if extensions.is_empty() {
        return true;
    }

    let Some(extension) = Path::new(name).extension().and_then(OsStr::to_str) else {
        return false;
    };

    extensions
        .iter()
        .any(|allowed| crate::win::equals_ignore_case(extension, allowed))
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
