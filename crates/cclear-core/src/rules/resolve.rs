//! 路径模板的候选解析：解析锚点、统计大小、判断状态。

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{CoreError, Result};
use crate::model::{KnownRoot, Phase, SizeInfo, Tier};
use crate::rules::catalog::{ENABLED_PHASES, Rule, SelectKind, Selector, Target};
use crate::rules::forbidden;
use crate::scan::{EntryInfo, FileFilter, ScanOptions, ScanProgress, scan};
use crate::win::process::ProcInfo;
use crate::win::{KnownFolder, dirinfo, known_folder};

/// 候选展示的样例路径条数上限。
const SAMPLE_LIMIT: usize = 20;

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

            let user_data = base.join(user_data);
            let mut anchors = Vec::new();

            for profile in chromium_profiles(&user_data) {
                for leaf in *leaves {
                    let path = user_data.join(&profile).join(leaf);
                    if path.is_dir() {
                        anchors.push(path);
                    }
                }
            }

            for leaf in *root_leaves {
                let path = user_data.join(leaf);
                if path.is_dir() {
                    anchors.push(path);
                }
            }

            anchors
        }
        Target::Adapter(_) | Target::Command(_) | Target::Special(_) | Target::System(_) => {
            Vec::new()
        }
    }
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

    cache.keys().cloned().collect()
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
        .find(|marker| !base.join(marker).is_dir())
        .map(|marker| marker.to_string())
}

/// 路径类规则的选择器。其余目标没有选择器，按全部内容处理。
fn selector_of(rule: &Rule) -> Selector {
    match &rule.target {
        Target::Paths { selector, .. } => *selector,
        _ => Selector {
            kind: SelectKind::AllContents,
            min_age_hours: None,
            exclude_newest_per_dir: false,
        },
    }
}

/// 统计锚点的体积，并取最大的若干文件作为样例。
fn measure(anchors: &[PathBuf], selector: &Selector, now: SystemTime) -> (SizeInfo, Vec<PathBuf>) {
    let options = ScanOptions {
        file_filter: build_filter(anchors, selector, now),
        top_files: SAMPLE_LIMIT,
        ..ScanOptions::default()
    };

    let progress = ScanProgress::default();
    let cancel = AtomicBool::new(false);
    let Ok(result) = scan(anchors, &options, &progress, &cancel) else {
        return (SizeInfo::default(), Vec::new());
    };

    let mut total = SizeInfo::default();
    for id in &result.roots {
        let size = result.nodes[*id as usize].size;
        total.logical += size.logical;
        total.allocated += size.allocated;
        total.files += size.files;
        total.dirs += size.dirs;
    }

    let samples = result
        .top_files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    (total, samples)
}

/// 选择器需要逐文件判断时（限制层级或按时间筛选），先枚举锚点得到允许的文件 ID 集合，
/// 再用 `file_filter` 只放行这些文件。
fn build_filter(anchors: &[PathBuf], selector: &Selector, now: SystemTime) -> Option<FileFilter> {
    let needs_selection = !matches!(selector.kind, SelectKind::AllContents)
        || selector.min_age_hours.is_some()
        || selector.exclude_newest_per_dir;
    if !needs_selection {
        return None;
    }

    let allowed = allowed_files(anchors, selector, now);
    Some(Arc::new(move |entry: &EntryInfo| {
        allowed.contains(&entry.file_id)
    }))
}

/// 计算本次允许统计的文件 ID 集合。
fn allowed_files(anchors: &[PathBuf], selector: &Selector, now: SystemTime) -> HashSet<u128> {
    let now = system_time_ticks(now);
    let mut dirs = Vec::new();
    for anchor in anchors {
        collect_dirs(anchor, true, selector, &mut dirs);
    }

    let mut allowed = HashSet::new();
    for dir in &dirs {
        for file in &dir.files {
            if let Some(hours) = selector.min_age_hours
                && age_hours(file.creation_time, file.last_write_time, now) < u64::from(hours)
            {
                continue;
            }
            allowed.insert(file.id);
        }
    }

    if selector.exclude_newest_per_dir {
        for dir in &dirs {
            let Some(newest) = dir.files.iter().map(|file| file.last_write_time).max() else {
                continue;
            };
            // 每个目录里最新的那个文件始终保留，时间相同的也一起保留。
            for file in &dir.files {
                if file.last_write_time == newest {
                    allowed.remove(&file.id);
                }
            }
        }
    }

    allowed
}

/// 锚点下一层的子目录是否整棵纳入统计。
///
/// `TopFiles` 只匹配锚点直属的文件，所以一个子目录都不进；`ChildDirs` 只进名字匹配的那些。
fn descend_into(selector: &Selector, name: &OsString) -> bool {
    match selector.kind {
        SelectKind::AllContents => true,
        SelectKind::TopFiles(_) => false,
        SelectKind::ChildDirs(patterns) => {
            let name = name.to_string_lossy();
            patterns
                .iter()
                .any(|pattern| matches_pattern(pattern, &name))
        }
    }
}

/// 锚点直属的文件是否纳入统计。`TopFiles` 只收名字匹配的，`ChildDirs` 一个都不收。
fn keep_anchor_file(selector: &Selector, name: &OsString) -> bool {
    match selector.kind {
        SelectKind::AllContents => true,
        SelectKind::ChildDirs(_) => false,
        SelectKind::TopFiles(patterns) => {
            let name = name.to_string_lossy();
            patterns
                .iter()
                .any(|pattern| matches_pattern(pattern, &name))
        }
    }
}

/// 枚举目录树时看到的文件。
struct ScannedFile {
    id: u128,
    creation_time: u64,
    last_write_time: u64,
}

/// 一个目录里的文件列表。
struct DirFiles {
    files: Vec<ScannedFile>,
}

/// 递归枚举锚点子树，只收集选择器选中的文件。读不到的目录直接跳过，和扫描器的处理一致。
fn collect_dirs(dir: &Path, is_anchor: bool, selector: &Selector, dirs: &mut Vec<DirFiles>) {
    let Ok(handle) = dirinfo::open_dir(dir) else {
        return;
    };

    let mut current = DirFiles { files: Vec::new() };
    let mut subdirs = Vec::new();

    let _ = dirinfo::for_each_entry(&handle, |entry| {
        if entry.is_dir {
            // junction 和符号链接不进，否则锚点会跑到别的卷。
            if entry.is_reparse() {
                return;
            }
            if is_anchor && !descend_into(selector, &entry.name) {
                return;
            }
            subdirs.push(dir.join(&entry.name));
            return;
        }

        if is_anchor && !keep_anchor_file(selector, &entry.name) {
            return;
        }

        current.files.push(ScannedFile {
            id: entry.file_id,
            creation_time: entry.creation_time,
            last_write_time: entry.last_write_time,
        });
    });

    // 句柄在递归前释放，避免深层目录同时占用大量句柄。
    drop(handle);
    dirs.push(current);

    for subdir in subdirs {
        collect_dirs(&subdir, false, selector, dirs);
    }
}

/// 展开相对路径里的通配符组件，只保留实际存在的目录。
fn expand_relative(base: &Path, rel: &str, anchors: &mut Vec<PathBuf>) {
    let parts: Vec<&str> = rel
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .collect();
    expand_parts(base, &parts, anchors);
}

fn expand_parts(current: &Path, parts: &[&str], anchors: &mut Vec<PathBuf>) {
    let Some((head, rest)) = parts.split_first() else {
        if current.is_dir() {
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
        if next.is_dir() {
            anchors.push(next);
        }
    } else if next.is_dir() {
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
fn age_hours(creation_time: u64, last_write_time: u64, now: u64) -> u64 {
    let newest = creation_time.max(last_write_time);
    now.saturating_sub(newest) / TICKS_PER_SECOND / 3600
}

/// `SystemTime` 换算成 FILETIME 的计时单位，好和目录项里的时间直接比较。
fn system_time_ticks(time: SystemTime) -> u64 {
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
