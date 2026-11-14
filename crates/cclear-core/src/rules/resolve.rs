//! 路径模板的候选解析：解析锚点、统计大小、判断状态。

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{CoreError, Result};
use crate::model::{FileIdentity, KnownRoot, Phase, SizeInfo, Tier};
use crate::rules::adapters::{self, LeftoverItem, SystemVersionProbe};
use crate::rules::catalog::{
    AdapterId, CommandId, ENABLED_PHASES, Rule, SelectKind, Selector, Target, catalog,
};
use crate::rules::commands::{self, CommandCheck};
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
    /// 判定依据，一条一个条目。适配器规则用它向用户说明为什么选中这些目录。
    pub evidence: Vec<String>,
    /// 没有选中某些项的原因，例如“比当前运行时新的安装目录本次不选”。
    pub notes: Vec<String>,
    pub executable: bool,
}

/// 解析规则锚点：只返回实际存在的目录。
///
/// 命令、特殊和系统操作没有可删除的锚点，返回空；命令类规则的
/// 缓存路径单独由 `rules::commands` 查询，不参与按清单执行。
pub fn resolve_anchors(rule: &Rule, roots: &RootMap) -> Vec<PathBuf> {
    // 适配器规则没有路径模板，锚点就是适配器给出的条目。
    // 这里只用于展示和打开位置，时间取当前时刻即可。
    if let Target::Adapter(id) = &rule.target {
        return adapter_items(*id, roots, SystemTime::now())
            .iter()
            .map(leftover_path)
            .collect();
    }

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
        // 适配器规则在上面已经提前返回。
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
    evaluate_with_checks(rule, roots, procs, now, None)
}

/// 评估一条规则。`checks` 有值时命令类规则直接用已经查好的结果，不再重复查询和统计。
fn evaluate_with_checks(
    rule: &Rule,
    roots: &RootMap,
    procs: &[ProcInfo],
    now: SystemTime,
    checks: Option<&CommandChecks>,
) -> Candidate {
    // 命令类规则不看目录，改成查工具和缓存路径，单独走一条评估。
    if let Target::Command(id) = &rule.target {
        return evaluate_command(rule, *id, checks.and_then(|checks| checks.get(*id)));
    }

    // 多版本残留的候选由适配器按证据给出，不走路径模板。
    if let Target::Adapter(id) = &rule.target {
        return evaluate_adapter_rule(rule, *id, roots, procs, now);
    }

    let mut candidate = Candidate {
        rule_id: rule.id,
        status: CandidateStatus::NotPresent,
        anchors: Vec::new(),
        size: SizeInfo::default(),
        samples: Vec::new(),
        evidence: Vec::new(),
        notes: Vec::new(),
        executable: false,
    };

    // 这些目标的具体逻辑属于后续任务，现在只能报“阶段未开放”。
    if matches!(rule.target, Target::Special(_) | Target::System(_)) {
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

/// 适配器规则的评估：判定交给适配器，这里只把结果整理成候选。
///
/// 进程和证据由适配器自己判断，所以这里不再跑通用的进程检查；条目路径由适配器
/// 在 Known Folder 下拼出，生成清单和执行时还会各过一遍路径护栏。
fn evaluate_adapter_rule(
    rule: &Rule,
    id: AdapterId,
    roots: &RootMap,
    procs: &[ProcInfo],
    now: SystemTime,
) -> Candidate {
    let mut candidate = Candidate {
        rule_id: rule.id,
        status: CandidateStatus::NotPresent,
        anchors: Vec::new(),
        size: SizeInfo::default(),
        samples: Vec::new(),
        evidence: Vec::new(),
        notes: Vec::new(),
        executable: false,
    };

    // manual 的分析类适配器不受阶段限制，其余适配器按开放阶段判断。
    if rule.tier != Tier::Manual && !ENABLED_PHASES.contains(&rule.phase) {
        candidate.status = CandidateStatus::PhaseLocked(rule.phase);
        return candidate;
    }

    let result = adapters::evaluate_adapter(id, roots, procs, &SystemVersionProbe, now);
    candidate.status = result.status;
    candidate.notes = result.notes;

    for item in &result.items {
        let path = leftover_path(item);
        candidate
            .evidence
            .push(format!("{}：{}", path.display(), item.evidence.join("；")));
    }

    // 体积按条目里的文件统计：目录展开成整棵子树，单文件就是它自己。
    let (mut files, dirs) = adapter_files(&result.items);
    candidate.size = SizeInfo {
        logical: files
            .iter()
            .filter(|file| file.counts_logical)
            .map(|file| file.logical)
            .sum(),
        allocated: files.iter().map(|file| file.allocated).sum(),
        files: files.len() as u64,
        dirs,
    };

    // 样例取占盘最大的文件；“打开样例”按同一套统计重算才能对上序号。
    sort_adapter_files(&mut files);
    candidate.samples = files
        .into_iter()
        .take(SAMPLE_LIMIT)
        .map(|file| file.path)
        .collect();

    candidate.anchors = result.items.iter().map(leftover_path).collect();

    candidate.executable = matches!(rule.tier, Tier::Safe | Tier::Caution | Tier::Leftover)
        && ENABLED_PHASES.contains(&rule.phase)
        && candidate.status == CandidateStatus::Ready;

    candidate
}

/// 适配器给出的条目。这里只用于展示、打开位置和占用检测：
/// 进程列表传空，即便应用正在运行也要能看到会处理哪些条目。
fn adapter_items(id: AdapterId, roots: &RootMap, now: SystemTime) -> Vec<LeftoverItem> {
    adapters::evaluate_adapter(id, roots, &[], &SystemVersionProbe, now).items
}

/// 一个适配器条目里的文件。
struct AdapterFile {
    path: PathBuf,
    logical: u64,
    allocated: u64,
    /// 云占位符本地没有数据，逻辑大小不计入。
    counts_logical: bool,
}

/// 适配器条目里的文件。目录按整棵子树展开，单文件从父目录的枚举结果里取，
/// 两种情况共用同一套硬链接去重：体积、样例和占用检测看到的文件集合必须一致。
fn adapter_files(items: &[LeftoverItem]) -> (Vec<AdapterFile>, u64) {
    let seen = HardLinkSet::new(HARD_LINK_SHARDS);
    let selector = all_contents_selector();
    let mut files: Vec<AdapterFile> = Vec::new();
    let mut dirs = 0u64;

    for item in items {
        let path = leftover_path(item);

        if item.is_dir {
            let tree = collect_tree(&path, true, &selector, &seen);
            dirs += tree.len() as u64;
            for dir in &tree {
                for file in &dir.files {
                    files.push(AdapterFile {
                        path: file.path.clone(),
                        logical: file.logical,
                        allocated: file.allocated,
                        counts_logical: file.counts_logical,
                    });
                }
            }
            continue;
        }

        // 单文件：父目录的枚举结果里才有它的文件 ID 和大小。
        let Some(parent) = path.parent() else {
            continue;
        };
        let Some(name) = path.file_name() else {
            continue;
        };
        let Some((volume_serial, entry)) = directory_entry(parent, name) else {
            continue;
        };
        if entry.is_dir {
            continue;
        }
        // 和目录统计口径一致：符号链接这类 reparse 文件不算进任何统计。
        let count = classify_file(&entry);
        if count == FileCount::Skip {
            continue;
        }

        let identity = FileIdentity {
            volume_serial,
            file_id: entry.file_id,
        };
        if !seen.insert(identity) {
            continue;
        }
        files.push(AdapterFile {
            path,
            logical: entry.logical,
            allocated: entry.allocated,
            counts_logical: count != FileCount::AllocatedOnly,
        });
    }

    (files, dirs)
}

/// 从父目录的枚举结果里找出指定名字的条目。
///
/// 名字比较不区分大小写：条目名来自适配器的目录枚举，个别名字（例如 `ext4.vhdx`）
/// 是写死的，磁盘上的大小写可能和写法不同。
fn directory_entry(parent: &Path, name: &OsStr) -> Option<(u64, dirinfo::EntryInfo)> {
    let handle = dirinfo::open_dir(parent).ok()?;
    let volume_serial = dirinfo::volume_serial(&handle).unwrap_or(0);
    let expected = name.to_string_lossy();

    let mut found = None;
    let _ = dirinfo::for_each_entry(&handle, |entry| {
        if crate::win::equals_ignore_case(&entry.name.to_string_lossy(), &expected) {
            found = Some(entry);
        }
    });

    found.map(|entry| (volume_serial, entry))
}

/// 占盘大的排前面，大小相同时按路径排，保证样例和占用检查的顺序稳定。
fn sort_adapter_files(files: &mut [AdapterFile]) {
    files.sort_by(|left, right| {
        right
            .allocated
            .cmp(&left.allocated)
            .then_with(|| left.path.cmp(&right.path))
    });
}

/// 适配器条目的完整路径，只用于展示和统计。
fn leftover_path(item: &LeftoverItem) -> PathBuf {
    let mut path = item.anchor.clone();
    for part in &item.rel {
        path.push(part);
    }

    path
}

/// 命令类规则的评估：工具装没装、缓存在不在 C 盘、缓存多大。
///
/// 这里会启动工具查询缓存路径，正常几百毫秒；查询失败只让这条规则不可执行。
fn evaluate_command(
    rule: &Rule,
    id: CommandId,
    precomputed: Option<(&CommandCheck, SizeInfo)>,
) -> Candidate {
    let mut candidate = Candidate {
        rule_id: rule.id,
        status: CandidateStatus::NotPresent,
        anchors: Vec::new(),
        size: SizeInfo::default(),
        samples: Vec::new(),
        evidence: Vec::new(),
        notes: Vec::new(),
        executable: false,
    };

    if !ENABLED_PHASES.contains(&rule.phase) {
        candidate.status = CandidateStatus::PhaseLocked(rule.phase);
        return candidate;
    }

    // 一次评估里已经查过并统计过，直接取结果；单独评估时现场查询。
    let queried;
    let (check, size) = match precomputed {
        Some(precomputed) => precomputed,
        None => {
            queried = commands::check(id);
            (&queried, measured_size(&queried))
        }
    };

    match check {
        CommandCheck::NotPresent => candidate.status = CandidateStatus::NotPresent,
        CommandCheck::Unusable(reason) => {
            candidate.status = CandidateStatus::Insufficient(reason.clone())
        }
        CommandCheck::Ready { paths } => {
            candidate.anchors = paths.clone();
            candidate.size = size;
            candidate.status = CandidateStatus::Ready;
        }
    }

    // 阶段已经在上面确认过，这里只按分级和当前状态判断能不能执行。
    candidate.executable = candidate.status == CandidateStatus::Ready
        && matches!(rule.tier, Tier::Safe | Tier::Caution | Tier::Leftover);

    candidate
}

/// 现场统计一条命令规则的缓存大小，口径和一次评估里预先统计的相同。
fn measured_size(check: &CommandCheck) -> SizeInfo {
    match check {
        CommandCheck::Ready { paths } => measure_paths(paths),
        CommandCheck::NotPresent | CommandCheck::Unusable(_) => SizeInfo::default(),
    }
}

/// 评估全部内置规则。规则之间互不依赖，用 rayon 跨规则并行，输出顺序与 `catalog()` 相同。
pub fn evaluate_all(roots: &RootMap, procs: &[ProcInfo], now: SystemTime) -> Vec<Candidate> {
    use rayon::prelude::*;

    // 命令类规则的缓存路径要先在规则之间去重再统计，文件类规则则直接枚举目录。
    // 两组工作互不依赖，同时跑，总耗时取较慢的那一边。
    let (checks, file_candidates) = rayon::join(CommandChecks::collect, || {
        catalog()
            .par_iter()
            .filter(|rule| !matches!(rule.target, Target::Command(_)))
            .map(|rule| evaluate_with_checks(rule, roots, procs, now, None))
            .collect::<Vec<Candidate>>()
    });

    // 合并回规则目录的顺序。命令类规则取预先查好、统计好的结果，不再重复查询。
    let mut file_candidates = file_candidates.into_iter();
    catalog()
        .iter()
        .map(|rule| match &rule.target {
            Target::Command(_) => evaluate_with_checks(rule, roots, procs, now, Some(&checks)),
            _ => file_candidates
                .next()
                .expect("文件类规则的数量与上面过滤后一致"),
        })
        .collect()
}

/// 一次评估里所有命令规则的结果，按命令 ID 索引。
struct CommandChecks {
    entries: Vec<(CommandId, CommandCheck, SizeInfo)>,
}

impl CommandChecks {
    /// 并行查询所有命令规则的缓存路径，去重后并行统计。
    fn collect() -> Self {
        use rayon::prelude::*;

        // 1. 并行查询各条命令规则的缓存路径。每次查询都要启动一次工具，规则之间互不依赖。
        let queried: Vec<(CommandId, CommandCheck)> = catalog()
            .par_iter()
            .filter(|rule| ENABLED_PHASES.contains(&rule.phase))
            .filter_map(|rule| match &rule.target {
                Target::Command(id) => Some((*id, commands::check(*id))),
                _ => None,
            })
            .collect();

        // 2. 同一个缓存目录只统计一遍。
        let mut unique: Vec<PathBuf> = Vec::new();
        for (_, check) in &queried {
            let CommandCheck::Ready { paths } = check else {
                continue;
            };
            for path in paths {
                if !unique.iter().any(|known| same_path(known, path)) {
                    unique.push(path.clone());
                }
            }
        }
        let measured: Vec<SizeInfo> = unique
            .par_iter()
            .map(|path| measure_paths(std::slice::from_ref(path)))
            .collect();

        // 3. 把统计好的大小分发给各条规则。
        let entries = queried
            .into_iter()
            .map(|(id, check)| {
                let size = match &check {
                    CommandCheck::Ready { paths } => sum_paths(paths, &unique, &measured),
                    CommandCheck::NotPresent | CommandCheck::Unusable(_) => SizeInfo::default(),
                };
                (id, check, size)
            })
            .collect();

        Self { entries }
    }

    /// 按命令 ID 取已经查好的结果。工具没装或路径不可用时也有记录。
    fn get(&self, id: CommandId) -> Option<(&CommandCheck, SizeInfo)> {
        self.entries
            .iter()
            .find(|(known, _, _)| *known == id)
            .map(|(_, check, size)| (check, *size))
    }
}

/// 一组缓存路径的合计大小。同一个目录只算一次。
fn sum_paths(paths: &[PathBuf], unique: &[PathBuf], measured: &[SizeInfo]) -> SizeInfo {
    let mut total = SizeInfo::default();
    let mut counted: Vec<&Path> = Vec::new();

    for path in paths {
        // 不同子命令可能报出大小写不同的同一个目录。
        if counted.iter().any(|known| same_path(known, path)) {
            continue;
        }
        counted.push(path);

        let Some(index) = unique.iter().position(|known| same_path(known, path)) else {
            continue;
        };

        let size = measured[index];
        total.logical += size.logical;
        total.allocated += size.allocated;
        total.files += size.files;
        total.dirs += size.dirs;
    }

    total
}

/// 两个路径是否相同。Windows 路径不区分大小写，pip 输出的就是小写盘符。
fn same_path(left: &Path, right: &Path) -> bool {
    crate::win::equals_ignore_case(
        &left.as_os_str().to_string_lossy(),
        &right.as_os_str().to_string_lossy(),
    )
}

/// 候选的样例路径：占盘最大的前 `SAMPLE_LIMIT` 个文件。
///
/// 界面上的样例序号就是这里的下标，打开样例时按同一套统计重算才能对上。
pub fn sample_paths(rule: &Rule, roots: &RootMap, now: SystemTime) -> Vec<PathBuf> {
    // 适配器规则的样例是条目里的文件，和评估时用同一套统计。
    if let Target::Adapter(id) = &rule.target {
        let (mut files, _) = adapter_files(&adapter_items(*id, roots, now));
        sort_adapter_files(&mut files);

        return files
            .into_iter()
            .take(SAMPLE_LIMIT)
            .map(|file| file.path)
            .collect();
    }

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
    // 适配器规则的文件集合由适配器条目展开，口径和体积统计一致。
    if let Target::Adapter(id) = &rule.target {
        return adapter_lock_files(*id, roots, now);
    }

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

/// 适配器候选的占用检查文件：占盘最大的前 `LOCK_CHECK_LIMIT` 个。
fn adapter_lock_files(id: AdapterId, roots: &RootMap, now: SystemTime) -> LockFiles {
    let (mut files, _) = adapter_files(&adapter_items(id, roots, now));
    sort_adapter_files(&mut files);

    LockFiles {
        total: files.len(),
        largest: files
            .into_iter()
            .take(LOCK_CHECK_LIMIT)
            .map(|file| file.path)
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
        _ => all_contents_selector(),
    }
}

/// 全部内容的选择器：不带年龄过滤和排除项。命令类规则的缓存目录按它统计。
fn all_contents_selector() -> Selector {
    Selector {
        kind: SelectKind::AllContents,
        min_age_hours: None,
        exclude_newest_per_dir: false,
        exclude_child_dirs: &[],
    }
}

/// 统计一组目录的占盘大小。命令类规则的缓存路径走这里，和文件类规则用同一套枚举。
pub(crate) fn measure_paths(anchors: &[PathBuf]) -> SizeInfo {
    let dirs = collect_all(anchors, &all_contents_selector());

    let mut total = SizeInfo {
        dirs: dirs.len() as u64,
        ..SizeInfo::default()
    };
    for dir in &dirs {
        for file in &dir.files {
            // 云占位符本地没有数据，逻辑大小没有意义，只计入占盘大小。
            if file.counts_logical {
                total.logical += file.logical;
            }
            total.allocated += file.allocated;
            total.files += 1;
        }
    }

    total
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
