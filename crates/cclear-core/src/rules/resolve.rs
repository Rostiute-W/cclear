//! 路径模板的候选解析：解析锚点、统计大小、判断状态。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{CoreError, Result};
use crate::model::{FileIdentity, KnownRoot, Phase, SizeInfo, Tier};
use crate::rules::catalog::{ENABLED_PHASES, Rule, SelectKind, Selector, Target, catalog};
use crate::rules::forbidden;
use crate::scan::{FileCount, HARD_LINK_SHARDS, HardLinkSet, classify_file};
use crate::win::process::ProcInfo;
use crate::win::restart_manager::{LockReport, query_lockers};
use crate::win::{KnownFolder, dirinfo, known_folder};

/// 候选展示的样例路径条数上限。
const SAMPLE_LIMIT: usize = 20;

/// 每个候选最多检查占用的文件数。占用检查只为给出证据，不必查全部文件。
const LOCK_CHECK_LIMIT: usize = 256;

/// FILETIME 的计时单位是 100 纳秒。
const TICKS_PER_SECOND: u64 = 10_000_000;

/// 1601 年到 1970 年的秒数，换算成 FILETIME 计时单位。
const TICKS_TO_UNIX_EPOCH: u64 = 11_644_473_600 * TICKS_PER_SECOND;

/// 已知根目录到实际路径的映射。测试注入临时目录，线上走 Known Folder。
pub struct RootMap {
    entries: Vec<(KnownRoot, PathBuf)>,
}

impl RootMap {
    /// 按当前用户的 Known Folder 建映射。任何一个解析不出来都算失败：缺了根目录会让规则静默失效。
    pub fn system() -> Result<Self> {
        let local_app_data = known_folder(KnownFolder::LocalAppData)
            .map_err(|_| CoreError::KnownFolder("本地应用数据目录"))?;
        let roaming_app_data = known_folder(KnownFolder::RoamingAppData)
            .map_err(|_| CoreError::KnownFolder("漫游应用数据目录"))?;
        let profile =
            known_folder(KnownFolder::Profile).map_err(|_| CoreError::KnownFolder("用户目录"))?;
        let documents =
            known_folder(KnownFolder::Documents).map_err(|_| CoreError::KnownFolder("文档目录"))?;
        let downloads =
            known_folder(KnownFolder::Downloads).map_err(|_| CoreError::KnownFolder("下载目录"))?;
        let program_data = known_folder(KnownFolder::ProgramData)
            .map_err(|_| CoreError::KnownFolder("ProgramData"))?;
        let windows =
            known_folder(KnownFolder::Windows).map_err(|_| CoreError::KnownFolder("Windows"))?;

        // 系统盘取 Windows 目录所在的卷根，Windows 装在别的盘时也能对得上。
        let system_drive = windows
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| windows.clone());

        Ok(Self {
            entries: vec![
                (KnownRoot::LocalAppData, local_app_data),
                (KnownRoot::RoamingAppData, roaming_app_data),
                (KnownRoot::Profile, profile),
                (KnownRoot::Documents, documents),
                (KnownRoot::Downloads, downloads),
                (KnownRoot::ProgramData, program_data),
                (KnownRoot::Windows, windows),
                (KnownRoot::SystemDrive, system_drive),
            ],
        })
    }

    /// 测试注入用的映射。
    pub fn with(entries: Vec<(KnownRoot, PathBuf)>) -> Self {
        Self { entries }
    }

    /// 取某个根目录的实际路径。没有映射时返回 None。
    pub fn get(&self, root: KnownRoot) -> Option<&Path> {
        self.entries
            .iter()
            .find(|(known, _)| *known == root)
            .map(|(_, path)| path.as_path())
    }
}

/// 候选的判定状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateStatus {
    /// 可以清理。
    Ready,
    /// 目录不存在，或者还没装这个应用。
    NotPresent,
    /// 相关应用还在运行。
    ProcessRunning(Vec<String>),
    /// 目录布局和预期不符（布局标记缺失）。
    LayoutMismatch(String),
    /// 规则所属阶段还没开放。
    PhaseLocked(Phase),
    /// 锚点落在禁区。
    Forbidden(String),
    /// 证据不足，只做分析。
    Insufficient(String),
}

impl CandidateStatus {
    /// 机器可读的状态名，界面据此选徽标样式。
    pub fn kind(&self) -> &'static str {
        match self {
            CandidateStatus::Ready => "ready",
            CandidateStatus::NotPresent => "notPresent",
            CandidateStatus::ProcessRunning(_) => "processRunning",
            CandidateStatus::LayoutMismatch(_) => "layoutMismatch",
            CandidateStatus::PhaseLocked(_) => "phaseLocked",
            CandidateStatus::Forbidden(_) => "forbidden",
            CandidateStatus::Insufficient(_) => "insufficient",
        }
    }

    /// 状态补充说明：进程名、原因或阶段。没有补充信息时返回 None。
    pub fn detail(&self) -> Option<String> {
        match self {
            CandidateStatus::Ready | CandidateStatus::NotPresent => None,
            CandidateStatus::ProcessRunning(names) => Some(names.join("、")),
            CandidateStatus::LayoutMismatch(marker) => Some(format!("缺少布局标记：{marker}")),
            CandidateStatus::PhaseLocked(phase) => Some(format!("将在 {phase:?} 开放")),
            CandidateStatus::Forbidden(reason) | CandidateStatus::Insufficient(reason) => {
                Some(reason.clone())
            }
        }
    }
}

/// 一条规则的评估结果。
pub struct Candidate {
    pub rule_id: &'static str,
    pub status: CandidateStatus,
    pub anchors: Vec<PathBuf>,
    pub size: SizeInfo,
    /// 按大小降序，最多 `SAMPLE_LIMIT` 个。
    pub samples: Vec<PathBuf>,
    pub executable: bool,
}

/// 解析规则锚点：只返回实际存在的目录。适配器、命令、特殊和系统操作由后续任务实现，这里返回空。
pub fn resolve_anchors(rule: &Rule, roots: &RootMap) -> Vec<PathBuf> {
    match &rule.target {
        Target::Paths { root, rels, .. } => {
            let Some(base) = roots.get(*root) else {
                return Vec::new();
            };

            let mut anchors = Vec::new();
            for rel in *rels {
                expand_relative(base, rel, &mut anchors);
            }
            anchors
        }
        Target::ChromiumProfiles {
            root,
            user_data,
            leaves,
            root_leaves,
        } => {
            let Some(base) = roots.get(*root) else {
                return Vec::new();
            };

            // `user_data` 是多级路径，每一级都要校验：中间某一级是 junction 时，末级会被带到别处。
            if !plain_dirs_below(base, user_data) {
                return Vec::new();
            }
            let user_data = base.join(user_data);

            let mut anchors = Vec::new();

            for profile in chromium_profiles(&user_data) {
                let profile_dir = user_data.join(&profile);
                if !dirinfo::is_plain_dir(&profile_dir) {
                    continue;
                }
                for leaf in *leaves {
                    if plain_dirs_below(&profile_dir, leaf) {
                        anchors.push(profile_dir.join(leaf));
                    }
                }
            }

            for leaf in *root_leaves {
                if plain_dirs_below(&user_data, leaf) {
                    anchors.push(user_data.join(leaf));
                }
            }

            anchors
        }
        Target::Adapter(_) | Target::Command(_) | Target::Special(_) | Target::System(_) => {
            Vec::new()
        }
    }
}

/// 从 `start` 起逐级下探 `rel` 的每一级目录，全部必须是真实目录（存在且不是 reparse point）。
///
/// 只看末级不够：中间某一级是 junction 或符号链接时，末级会被带到别的卷甚至禁区。
fn plain_dirs_below(start: &Path, rel: &str) -> bool {
    let mut current = start.to_path_buf();

    for part in rel
        .split(['\\', '/'])
        .filter(|part| !part.is_empty() && *part != ".")
    {
        current.push(part);
        if !dirinfo::is_plain_dir(&current) {
            return false;
        }
    }

    true
}

/// 评估一条规则。
pub fn evaluate(rule: &Rule, roots: &RootMap, procs: &[ProcInfo], now: SystemTime) -> Candidate {
    let mut candidate = Candidate {
        rule_id: rule.id,
        status: CandidateStatus::NotPresent,
        anchors: Vec::new(),
        size: SizeInfo::default(),
        samples: Vec::new(),
        executable: false,
    };

    // 这些目标的具体逻辑属于后续任务，现在只能报“阶段未开放”。
    if matches!(
        rule.target,
        Target::Adapter(_) | Target::Command(_) | Target::Special(_) | Target::System(_)
    ) {
        candidate.status = CandidateStatus::PhaseLocked(rule.phase);
        return candidate;
    }

    if let Some(marker) = missing_layout_marker(rule, roots) {
        candidate.status = CandidateStatus::LayoutMismatch(marker);
        return candidate;
    }

    let anchors = resolve_anchors(rule, roots);
    if anchors.is_empty() {
        return candidate;
    }

    let selector = selector_of(rule);
    let forbidden_reason = anchors
        .iter()
        .find_map(|anchor| forbidden::is_forbidden(anchor, roots));
    let (size, samples) = measure(&anchors, &selector, now);
    candidate.anchors = anchors;
    candidate.size = size;
    candidate.samples = samples;

    // 禁区只禁止执行和迁移，分析时仍然可以显示体积和样例。
    if let Some(reason) = forbidden_reason {
        candidate.status = CandidateStatus::Forbidden(reason);
        return candidate;
    }

    let running = running_process_names(rule, procs);
    candidate.status = if !running.is_empty() {
        CandidateStatus::ProcessRunning(running)
    } else if rule.tier == Tier::Manual {
        CandidateStatus::Insufficient("语义未验证，只做分析".to_string())
    } else {
        CandidateStatus::Ready
    };

    candidate.executable = matches!(rule.tier, Tier::Safe | Tier::Caution | Tier::Leftover)
        && ENABLED_PHASES.contains(&rule.phase)
        && candidate.status == CandidateStatus::Ready;

    candidate
}

/// 评估全部内置规则。规则之间互不依赖，用 rayon 跨规则并行，输出顺序与 `catalog()` 相同。
pub fn evaluate_all(roots: &RootMap, procs: &[ProcInfo], now: SystemTime) -> Vec<Candidate> {
    use rayon::prelude::*;

    catalog()
        .par_iter()
        .map(|rule| evaluate(rule, roots, procs, now))
        .collect()
}

/// 候选的样例路径：占盘最大的前 `SAMPLE_LIMIT` 个文件。
///
/// 界面上的样例序号就是这里的下标，打开样例时按同一套统计重算才能对上。
pub fn sample_paths(rule: &Rule, roots: &RootMap, now: SystemTime) -> Vec<PathBuf> {
    let (_, samples) = measure(&resolve_anchors(rule, roots), &selector_of(rule), now);

    samples
}

/// 对候选做占用检查：查询占盘最大的前 `LOCK_CHECK_LIMIT` 个文件。
///
/// 检查的文件比候选的文件总数少时，结果同样不完整：没查过的文件不能当成没有占用。
pub fn check_locks(rule: &Rule, roots: &RootMap, now: SystemTime) -> LockReport {
    let files = lock_files(rule, roots, now);
    let mut report = query_lockers(&files.largest);

    report.total = files.total;
    if report.total == 0 {
        // 没有文件可查时 Restart Manager 查不出任何东西，不能因此报“没有占用”。
        report.complete = false;
        report.error = Some("没有可检查的文件".to_string());
        return report;
    }

    if report.checked < report.total {
        report.complete = false;
        if report.error.is_none() {
            report.error = Some(format!(
                "只检查了占盘最大的 {} 个文件（共 {} 个）",
                report.checked, report.total
            ));
        }
    }

    report
}

/// 要检查占用的文件，以及候选的文件总数。
struct LockFiles {
    largest: Vec<PathBuf>,
    total: usize,
}

/// 按选择器挑出候选的文件，再取占盘最大的若干个。
fn lock_files(rule: &Rule, roots: &RootMap, now: SystemTime) -> LockFiles {
    let selector = selector_of(rule);
    let dirs = collect_all(&resolve_anchors(rule, roots), &selector);
    let allowed = allowed_ids(&dirs, &selector, now);

    let mut files = selected_files(&dirs, &allowed);
    sort_by_allocated(&mut files);

    LockFiles {
        total: files.len(),
        largest: files
            .into_iter()
            .take(LOCK_CHECK_LIMIT)
            .map(|file| file.path.clone())
            .collect(),
    }
}

/// 统计和占用检查共用的文件取舍：只保留选择器和年龄条件允许的文件。
fn selected_files<'a>(
    dirs: &'a [DirFiles],
    allowed: &HashSet<FileIdentity>,
) -> Vec<&'a ScannedFile> {
    dirs.iter()
        .flat_map(|dir| dir.files.iter())
        .filter(|file| allowed.contains(&file.identity))
        .collect()
}

/// 占盘大的排前面，大小相同时按路径排，保证统计出的样例顺序稳定。
fn sort_by_allocated(files: &mut [&ScannedFile]) {
    files.sort_by(|left, right| {
        right
            .allocated
            .cmp(&left.allocated)
            .then_with(|| left.path.cmp(&right.path))
    });
}

/// 从 `Local State` 的 `profile.info_cache` 读出 Profile 目录名。读不到就当没有 Profile。
fn chromium_profiles(user_data: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(user_data.join("Local State")) else {
        return Vec::new();
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(cache) = state
        .get("profile")
        .and_then(|profile| profile.get("info_cache"))
        .and_then(|cache| cache.as_object())
    else {
        return Vec::new();
    };

    cache
        .keys()
        .filter(|name| is_valid_profile_name(name))
        .cloned()
        .collect()
}

/// Profile 名只能是单个普通路径组件，否则 `..\evil`、`a/b` 这类键会把锚点带出 User Data。
fn is_valid_profile_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['\\', '/', ':'])
}

/// 布局标记必须全部存在，否则不能确定目录就是目标应用的布局。
fn missing_layout_marker(rule: &Rule, roots: &RootMap) -> Option<String> {
    let root = match &rule.target {
        Target::Paths { root, .. } => *root,
        _ => return None,
    };
    let base = roots.get(root)?;

    rule.layout_markers
        .iter()
        .find(|marker| !plain_dirs_below(base, marker))
        .map(|marker| marker.to_string())
}

/// 路径类规则的选择器。其余目标没有选择器，按全部内容处理。
pub fn selector_of(rule: &Rule) -> Selector {
    match &rule.target {
        Target::Paths { selector, .. } => *selector,
        _ => Selector {
            kind: SelectKind::AllContents,
            min_age_hours: None,
            exclude_newest_per_dir: false,
            exclude_child_dirs: &[],
        },
    }
}

/// 统计锚点的体积，并取最大的若干文件作为样例。
fn measure(anchors: &[PathBuf], selector: &Selector, now: SystemTime) -> (SizeInfo, Vec<PathBuf>) {
    let dirs = collect_all(anchors, selector);
    let allowed = allowed_ids(&dirs, selector, now);

    let mut files = selected_files(&dirs, &allowed);
    let mut total = SizeInfo {
        dirs: dirs.len() as u64,
        ..SizeInfo::default()
    };
    for file in &files {
        // 云占位符本地没有数据，逻辑大小没有意义，只计入占盘大小。
        if file.counts_logical {
            total.logical += file.logical;
        }
        total.allocated += file.allocated;
        total.files += 1;
    }

    sort_by_allocated(&mut files);
    let samples = files
        .into_iter()
        .take(SAMPLE_LIMIT)
        .map(|file| file.path.clone())
        .collect();

    (total, samples)
}

/// 枚举锚点子树，保留每个目录里的文件列表。
///
/// 统计和占用检查都走这里，两者看到的文件集合才不会出现差异。
fn collect_all(anchors: &[PathBuf], selector: &Selector) -> Vec<DirFiles> {
    // 硬链接的多个名字只算一次，和扫描器的去重口径一致。
    let seen = HardLinkSet::new(HARD_LINK_SHARDS);
    let mut dirs = Vec::new();

    for anchor in anchors {
        dirs.extend(collect_tree(anchor, true, selector, &seen));
    }

    dirs
}

/// 计算本次允许统计的文件身份集合。
fn allowed_ids(dirs: &[DirFiles], selector: &Selector, now: SystemTime) -> HashSet<FileIdentity> {
    let now = system_time_ticks(now);
    let mut allowed = HashSet::new();
    for dir in dirs {
        for file in &dir.files {
            if let Some(hours) = selector.min_age_hours
                && age_hours(file.creation_time, file.last_write_time, now) < u64::from(hours)
            {
                continue;
            }
            allowed.insert(file.identity);
        }
    }

    if selector.exclude_newest_per_dir {
        for dir in dirs {
            let Some(newest) = dir.files.iter().map(|file| file.last_write_time).max() else {
                continue;
            };
            // 每个目录里最新的那个文件始终保留，时间相同的也一起保留。
            for file in &dir.files {
                if file.last_write_time == newest {
                    allowed.remove(&file.identity);
                }
            }
        }
    }

    allowed
}

/// 锚点下一层的子目录是否整棵纳入统计。
///
/// `TopFiles` 只匹配锚点直属的文件，所以一个子目录都不进；`ChildDirs` 只进名字匹配的那些。
pub(crate) fn descend_into(selector: &Selector, name: &str) -> bool {
    match selector.kind {
        SelectKind::AllContents => true,
        SelectKind::TopFiles(_) => false,
        SelectKind::ChildDirs(patterns) => patterns
            .iter()
            .any(|pattern| matches_pattern(pattern, name)),
    }
}

/// 锚点直属的文件是否纳入统计。`TopFiles` 只收名字匹配的，`ChildDirs` 一个都不收。
pub(crate) fn keep_anchor_file(selector: &Selector, name: &str) -> bool {
    match selector.kind {
        SelectKind::AllContents => true,
        SelectKind::ChildDirs(_) => false,
        SelectKind::TopFiles(patterns) => patterns
            .iter()
            .any(|pattern| matches_pattern(pattern, name)),
    }
}

/// 锚点下一层按名称排除的子目录。
pub(crate) fn is_excluded_child_dir(selector: &Selector, name: &str) -> bool {
    selector
        .exclude_child_dirs
        .iter()
        .any(|pattern| matches_pattern(pattern, name))
}

/// 枚举目录树时看到的文件。
struct ScannedFile {
    path: PathBuf,
    identity: FileIdentity,
    logical: u64,
    allocated: u64,
    /// 云占位符本地没有数据，逻辑大小不计入。
    counts_logical: bool,
    creation_time: u64,
    last_write_time: u64,
}

/// 一个目录里的文件列表。
struct DirFiles {
    files: Vec<ScannedFile>,
}

/// 递归枚举一棵子树，返回本目录和各级子目录的文件列表。
///
/// 子目录用 rayon 并行枚举：全部规则评估原来要 8 秒，单线程递归是主要开销。
/// 返回顺序仍是“本目录在前，子目录按枚举顺序在后”，和单线程遍历一致。
fn collect_tree(
    dir: &Path,
    is_anchor: bool,
    selector: &Selector,
    seen: &HardLinkSet,
) -> Vec<DirFiles> {
    let Ok(handle) = dirinfo::open_dir(dir) else {
        return Vec::new();
    };

    // 文件身份按“卷序列号 + 文件 ID”判断同一个文件，卷序列号取目录所在卷的。
    let volume_serial = dirinfo::volume_serial(&handle).unwrap_or(0);
    let mut current = DirFiles { files: Vec::new() };
    let mut subdirs = Vec::new();

    let _ = dirinfo::for_each_entry(&handle, |entry| {
        if entry.is_dir {
            // junction 和符号链接不进，否则锚点会跑到别的卷。
            if entry.is_reparse() {
                return;
            }
            let name = entry.name.to_string_lossy();
            // 禁区目录整棵不碰，统计和占用检查都不能看到里面的文件。
            if forbidden::is_forbidden_name(&name) {
                return;
            }
            if is_anchor && is_excluded_child_dir(selector, &name) {
                return;
            }
            if is_anchor && !descend_into(selector, &name) {
                return;
            }
            subdirs.push(dir.join(&entry.name));
            return;
        }

        let name = entry.name.to_string_lossy();
        if is_anchor && !keep_anchor_file(selector, &name) {
            return;
        }

        // 和扫描器口径一致：符号链接这类 reparse 文件不算进任何统计。
        let count = classify_file(&entry);
        if count == FileCount::Skip {
            return;
        }

        let identity = FileIdentity {
            volume_serial,
            file_id: entry.file_id,
        };
        // 硬链接的另一个名字：大小和文件数都只算一次。
        if !seen.insert(identity) {
            return;
        }

        current.files.push(ScannedFile {
            path: dir.join(&entry.name),
            identity,
            logical: entry.logical,
            allocated: entry.allocated,
            counts_logical: count != FileCount::AllocatedOnly,
            creation_time: entry.creation_time,
            last_write_time: entry.last_write_time,
        });
    });

    // 句柄在递归前释放，避免深层目录同时占用大量句柄。
    drop(handle);

    let children: Mutex<Vec<(usize, Vec<DirFiles>)>> = Mutex::new(Vec::new());
    if !subdirs.is_empty() {
        rayon::scope(|scope| {
            for (index, child) in subdirs.into_iter().enumerate() {
                let children = &children;
                scope.spawn(move |_| {
                    let subtree = collect_tree(&child, false, selector, seen);
                    children.lock().unwrap().push((index, subtree));
                });
            }
        });
    }

    // 按枚举顺序还原，结果才和目录里的实际顺序一致。
    let mut collected = children.into_inner().unwrap();
    collected.sort_by_key(|(index, _)| *index);

    let mut result = Vec::with_capacity(collected.len() + 1);
    result.push(current);
    for (_, subtree) in collected {
        result.extend(subtree);
    }

    result
}

/// 展开相对路径里的通配符组件，只保留实际存在的目录。
///
/// 空字符串和 `.` 表示根目录本身。
fn expand_relative(base: &Path, rel: &str, anchors: &mut Vec<PathBuf>) {
    let parts: Vec<&str> = rel
        .split(['\\', '/'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    expand_parts(base, &parts, anchors);
}

fn expand_parts(current: &Path, parts: &[&str], anchors: &mut Vec<PathBuf>) {
    let Some((head, rest)) = parts.split_first() else {
        if dirinfo::is_plain_dir(current) {
            anchors.push(current.to_path_buf());
        }
        return;
    };

    if head.contains('*') || head.contains('?') {
        for name in subdirectory_names(current) {
            if matches_pattern(head, &name) {
                expand_parts(&current.join(&name), rest, anchors);
            }
        }
        return;
    }

    let next = current.join(head);
    if rest.is_empty() {
        if dirinfo::is_plain_dir(&next) {
            anchors.push(next);
        }
    } else if dirinfo::is_plain_dir(&next) {
        expand_parts(&next, rest, anchors);
    }
}

/// 列出目录下的子目录名，跳过 junction 和符号链接。
fn subdirectory_names(dir: &Path) -> Vec<String> {
    let Ok(handle) = dirinfo::open_dir(dir) else {
        return Vec::new();
    };

    let mut names = Vec::new();
    let _ = dirinfo::for_each_entry(&handle, |entry| {
        if entry.is_dir && !entry.is_reparse() {
            names.push(entry.name.to_string_lossy().into_owned());
        }
    });
    names
}

/// 单个组件或文件名的通配符匹配：`*` 匹配任意长度，`?` 匹配单个字符，不区分大小写。
fn matches_pattern(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();

    let mut pattern_at = 0;
    let mut text_at = 0;
    let mut backtrack: Option<(usize, usize)> = None;

    while text_at < text.len() {
        if pattern_at < pattern.len()
            && (pattern[pattern_at] == '?'
                || (pattern[pattern_at] != '*'
                    && crate::win::char_equals_ignore_case(pattern[pattern_at], text[text_at])))
        {
            pattern_at += 1;
            text_at += 1;
            continue;
        }

        if pattern_at < pattern.len() && pattern[pattern_at] == '*' {
            backtrack = Some((pattern_at, text_at));
            pattern_at += 1;
            continue;
        }

        // 失配时回到最近一个 * 的位置，让它多匹配一个字符。
        match backtrack {
            Some((star, start)) => {
                pattern_at = star + 1;
                text_at = start + 1;
                backtrack = Some((star, start + 1));
            }
            None => return false,
        }
    }

    while pattern_at < pattern.len() && pattern[pattern_at] == '*' {
        pattern_at += 1;
    }
    pattern_at == pattern.len()
}

/// 文件年龄（小时）= now − max(CreationTime, LastWriteTime)。
pub(crate) fn age_hours(creation_time: u64, last_write_time: u64, now: u64) -> u64 {
    let newest = creation_time.max(last_write_time);
    now.saturating_sub(newest) / TICKS_PER_SECOND / 3600
}

/// `SystemTime` 换算成 FILETIME 的计时单位，好和目录项里的时间直接比较。
pub(crate) fn system_time_ticks(time: SystemTime) -> u64 {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let ticks = since_epoch.as_nanos() / 100;
    TICKS_TO_UNIX_EPOCH.saturating_add(u64::try_from(ticks).unwrap_or(u64::MAX))
}

/// 规则要求退出的进程里，当前正在运行的那些。
fn running_process_names(rule: &Rule, procs: &[ProcInfo]) -> Vec<String> {
    rule.processes
        .iter()
        .filter(|required| {
            procs
                .iter()
                .any(|proc| crate::win::equals_ignore_case(&proc.name, required))
        })
        .map(|required| (*required).to_string())
        .collect()
}
