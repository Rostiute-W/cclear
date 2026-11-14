//! 追加式操作日志：每一步都先落盘，写不进去就停手。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::win::{KnownFolder, known_folder};

/// 每条记录都立即写入并 flush；每这么多条再 fsync 一次，兼顾可靠性和磁盘寿命。
const SYNC_EVERY: u32 = 256;

/// 详情里的跳过明细上限。跳过可能上万条，界面只需要看到一部分就能判断原因。
const MAX_SKIPPED_DETAILS: usize = 200;

/// 一条日志记录。后续任务会继续添加变体。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Record {
    PlanStarted {
        plan_id: Uuid,
        /// Unix 秒，方便按时间排序和显示。
        at: i64,
        rules: Vec<String>,
    },
    ItemDone {
        plan_id: Uuid,
        item_id: u32,
        rule_id: String,
        path: String,
        is_dir: bool,
        allocated: u64,
    },
    ItemSkipped {
        plan_id: Uuid,
        item_id: u32,
        rule_id: String,
        path: String,
        reason: String,
    },
    RuleSkipped {
        plan_id: Uuid,
        rule_id: String,
        reason: String,
    },
    PlanFinished {
        plan_id: Uuid,
        free_before: u64,
        free_after: u64,
        deleted_files: u64,
        deleted_dirs: u64,
        freed_allocated: u64,
        skipped: u64,
        /// 用户取消了这次执行。旧日志行没有这个字段，按 false 读。
        #[serde(default)]
        cancelled: bool,
    },
    /// 不走冻结清单的特殊操作，例如清空 C 盘回收站。
    SpecialAction {
        /// Unix 秒，方便按时间排序和显示。
        at: i64,
        /// 操作名，界面按它选文案。
        action: String,
        /// 结果说明，写清楚做了什么、影响多少内容。
        detail: String,
    },
    /// 开发工具官方清理命令的执行记录。命令不走冻结清单，只记结果。
    CommandRun {
        /// Unix 秒，方便按时间排序和显示。
        at: i64,
        rule_id: String,
        /// 退出码。命令没跑起来时为 None。
        exit_code: Option<i32>,
        /// 等待超时。超时后命令仍在运行，这里只停止等待。
        timed_out: bool,
        /// 执行前后的缓存占盘大小，用来判断命令是否真的清掉了东西。
        allocated_before: u64,
        allocated_after: u64,
    },
    /// 隔离批次的状态变化。隔离区不按清理计划执行，单独记一条。
    QuarantineStage {
        batch_id: Uuid,
        /// Unix 秒，方便按时间排序和显示。
        at: i64,
        /// 批次状态的中文名，界面直接显示。
        stage: String,
        /// 这一步做了什么，或者为什么停在这里。
        detail: String,
    },
    /// 工具缓存目录迁移的状态变化。迁移不按清理计划执行，单独记一条。
    MigrationStage {
        /// Unix 秒，方便按时间排序和显示。
        at: i64,
        /// 工具名，例如 uv、npm。
        tool: String,
        /// 迁移阶段的中文名，界面直接显示。
        stage: String,
        /// 这一步做了什么，或者为什么失败。
        detail: String,
    },
    /// 目录迁移（junction）的状态变化。
    JunctionStage {
        /// Unix 秒，方便按时间排序和显示。
        at: i64,
        /// 迁移记录 ID。
        id: String,
        /// 源目录路径。
        source: String,
        /// 迁移阶段的中文名，界面直接显示。
        stage: String,
        /// 这一步做了什么，或者为什么停下。
        detail: String,
    },
}

/// 日志出口。执行器只认这个接口，日志写失败就立刻中止。
pub trait JournalSink {
    fn append(&mut self, record: &Record) -> io::Result<()>;

    /// 计划结束时的强制落盘。只在内存里记的 sink 不需要实现。
    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 一次执行的历史摘要。
pub struct PlanSummary {
    pub plan_id: Uuid,
    pub started_at: OffsetDateTime,
    /// 这次执行涉及的规则 ID。日志只记 ID，名称由界面查规则目录。
    pub rules: Vec<String>,
    pub free_before: u64,
    pub free_after: u64,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub freed_allocated: u64,
    pub skipped: u64,
    /// 计划没有写完 PlanFinished 时是中断的执行。
    pub finished: bool,
    /// 这次执行被用户取消。
    pub cancelled: bool,
}

/// 一次执行的详情：每条规则删了什么、跳过了什么。
pub struct PlanDetail {
    pub plan_id: Uuid,
    pub started_at: OffsetDateTime,
    pub free_before: u64,
    pub free_after: u64,
    pub finished: bool,
    /// 这次执行被用户取消。
    pub cancelled: bool,
    pub rules: Vec<RuleDetail>,
    /// 跳过明细，最多 `MAX_SKIPPED_DETAILS` 条；`skipped_total` 是实际总数。
    pub skipped: Vec<SkippedItem>,
    pub skipped_total: u64,
}

/// 详情里一条规则的统计。
pub struct RuleDetail {
    pub rule_id: String,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub freed_allocated: u64,
    pub skipped: u64,
}

/// 详情里一条跳过记录。整条规则被跳过时没有具体路径。
pub struct SkippedItem {
    pub rule_id: String,
    pub path: String,
    pub reason: String,
}

/// 追加式文件日志，按天分文件写在 `%LOCALAPPDATA%\CClear\journal`。
pub struct FileJournal {
    dir: PathBuf,
    file: Option<File>,
    /// 当前打开文件对应的日期，跨天后要换文件。
    date: String,
    since_sync: u32,
}

impl FileJournal {
    /// 打开当天的日志文件。目录不存在时创建。
    pub fn new() -> io::Result<Self> {
        Self::at(journal_dir()?)
    }

    /// 在指定目录下打开当天的日志文件。提权 helper 的日志写在 ProgramData 下。
    pub fn at(dir: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;

        let mut journal = Self {
            dir,
            file: None,
            date: String::new(),
            since_sync: 0,
        };
        journal.ensure_file()?;

        Ok(journal)
    }

    /// 换天时切到新文件。一次执行跨过午夜也不会写错日期。
    fn ensure_file(&mut self) -> io::Result<()> {
        let today = today_string();
        if self.file.is_some() && self.date == today {
            return Ok(());
        }

        let path = self.dir.join(format!("{today}.jsonl"));
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        self.file = Some(file);
        self.date = today;
        self.since_sync = 0;

        Ok(())
    }
}

impl JournalSink for FileJournal {
    fn append(&mut self, record: &Record) -> io::Result<()> {
        self.ensure_file()?;

        let Some(file) = &mut self.file else {
            return Err(io::Error::other("日志文件没有打开"));
        };

        let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
        line.push(b'\n');
        file.write_all(&line)?;
        file.flush()?;

        self.since_sync += 1;
        if self.since_sync >= SYNC_EVERY {
            file.sync_data()?;
            self.since_sync = 0;
        }

        Ok(())
    }

    /// 计划结束时强制落盘一次。
    fn finish(&mut self) -> io::Result<()> {
        match &self.file {
            Some(file) => file.sync_data(),
            None => Ok(()),
        }
    }
}

/// 读最近 `days` 天的执行摘要，返回按时间倒序的计划列表和解析失败的行数。
///
/// 任务书给的签名是只返回列表，这里多返回一个计数：解析失败说明日志被截断或损坏，
/// 界面需要如实告诉用户读到的不完整，而不是假装历史只有这些。
pub fn read_summaries(days: usize) -> io::Result<(Vec<PlanSummary>, usize)> {
    let mut plans: Vec<PlanSummary> = Vec::new();
    let mut index: HashMap<Uuid, usize> = HashMap::new();
    let mut skipped_lines = 0usize;

    for file in journal_files(days)? {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };

        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }

            // 截断或损坏的行只跳过并计数，不能因为一行坏掉就看不到整段历史。
            let Ok(record) = serde_json::from_str::<Record>(line) else {
                skipped_lines += 1;
                continue;
            };

            // 回收站这类特殊操作不属于任何清理计划，不进历史列表。
            let Some(plan_id) = record_plan_id(&record) else {
                continue;
            };
            apply(&mut plans, &mut index, plan_id, record);
        }
    }

    plans.reverse();

    Ok((plans, skipped_lines))
}

/// 读某一次执行的详情。日志里找不到这个计划时返回 None。
pub fn read_detail(days: usize, plan_id: Uuid) -> io::Result<Option<PlanDetail>> {
    let mut detail: Option<PlanDetail> = None;

    for file in journal_files(days)? {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };

        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }

            // 详情页只关心这一个计划，坏行和其他计划的记录都直接略过。
            let Ok(record) = serde_json::from_str::<Record>(line) else {
                continue;
            };
            if record_plan_id(&record) != Some(plan_id) {
                continue;
            }

            apply_detail(
                detail.get_or_insert_with(|| PlanDetail {
                    plan_id,
                    started_at: OffsetDateTime::UNIX_EPOCH,
                    free_before: 0,
                    free_after: 0,
                    finished: false,
                    cancelled: false,
                    rules: Vec::new(),
                    skipped: Vec::new(),
                    skipped_total: 0,
                }),
                record,
            );
        }
    }

    Ok(detail)
}

/// 最近 `days` 天要读的日志文件，按日期升序。还没有日志时返回空列表。
fn journal_files(days: usize) -> io::Result<Vec<PathBuf>> {
    let dir = journal_dir()?;

    // 文件名是 ISO 日期，按名字排序就是按日期排序。
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .collect(),
        // 还没产生过日志时不是错误。
        Err(_) => return Ok(Vec::new()),
    };

    files.sort();
    if files.len() > days {
        files.drain(..files.len() - days);
    }

    Ok(files)
}

/// 把一条记录并入对应的计划摘要。
fn apply(
    plans: &mut Vec<PlanSummary>,
    index: &mut HashMap<Uuid, usize>,
    plan_id: Uuid,
    record: Record,
) {
    let position = *index.entry(plan_id).or_insert_with(|| {
        plans.push(PlanSummary {
            plan_id,
            started_at: OffsetDateTime::UNIX_EPOCH,
            rules: Vec::new(),
            free_before: 0,
            free_after: 0,
            deleted_files: 0,
            deleted_dirs: 0,
            freed_allocated: 0,
            skipped: 0,
            finished: false,
            cancelled: false,
        });
        plans.len() - 1
    });
    let summary = &mut plans[position];

    match record {
        Record::PlanStarted { at, rules, .. } => {
            summary.started_at =
                OffsetDateTime::from_unix_timestamp(at).unwrap_or(OffsetDateTime::UNIX_EPOCH);
            summary.rules = rules;
        }
        Record::ItemDone {
            is_dir, allocated, ..
        } => {
            if is_dir {
                summary.deleted_dirs += 1;
            } else {
                summary.deleted_files += 1;
            }
            summary.freed_allocated += allocated;
        }
        Record::ItemSkipped { .. } | Record::RuleSkipped { .. } => summary.skipped += 1,
        // PlanFinished 里的合计是执行结束时算的，以它为准。
        Record::PlanFinished {
            free_before,
            free_after,
            deleted_files,
            deleted_dirs,
            freed_allocated,
            skipped,
            cancelled,
            ..
        } => {
            summary.free_before = free_before;
            summary.free_after = free_after;
            summary.deleted_files = deleted_files;
            summary.deleted_dirs = deleted_dirs;
            summary.freed_allocated = freed_allocated;
            summary.skipped = skipped;
            summary.finished = true;
            summary.cancelled = cancelled;
        }
        // 特殊操作、官方命令、隔离批次和迁移不属于任何计划，不会走到这里。
        Record::SpecialAction { .. }
        | Record::CommandRun { .. }
        | Record::QuarantineStage { .. }
        | Record::MigrationStage { .. }
        | Record::JunctionStage { .. } => {}
    }
}

/// 记录所属的计划 ID。回收站和官方命令这类操作不属于任何计划，返回 None。
fn record_plan_id(record: &Record) -> Option<Uuid> {
    match record {
        Record::PlanStarted { plan_id, .. }
        | Record::ItemDone { plan_id, .. }
        | Record::ItemSkipped { plan_id, .. }
        | Record::RuleSkipped { plan_id, .. }
        | Record::PlanFinished { plan_id, .. } => Some(*plan_id),
        Record::SpecialAction { .. }
        | Record::CommandRun { .. }
        | Record::QuarantineStage { .. }
        | Record::MigrationStage { .. }
        | Record::JunctionStage { .. } => None,
    }
}

/// 把一条记录并入执行详情。
fn apply_detail(detail: &mut PlanDetail, record: Record) {
    match record {
        Record::PlanStarted { at, rules, .. } => {
            detail.started_at =
                OffsetDateTime::from_unix_timestamp(at).unwrap_or(OffsetDateTime::UNIX_EPOCH);
            // 计划里的规则先全部列出来，没删任何文件的规则也要能显示成 0。
            detail.rules = rules
                .into_iter()
                .map(|rule_id| RuleDetail::new(&rule_id))
                .collect();
        }
        Record::ItemDone {
            rule_id,
            is_dir,
            allocated,
            ..
        } => {
            let rule = rule_detail(detail, &rule_id);
            if is_dir {
                rule.deleted_dirs += 1;
            } else {
                rule.deleted_files += 1;
            }
            rule.freed_allocated += allocated;
        }
        Record::ItemSkipped {
            rule_id,
            path,
            reason,
            ..
        } => {
            rule_detail(detail, &rule_id).skipped += 1;
            detail.skipped_total += 1;
            if detail.skipped.len() < MAX_SKIPPED_DETAILS {
                detail.skipped.push(SkippedItem {
                    rule_id,
                    path,
                    reason,
                });
            }
        }
        Record::RuleSkipped {
            rule_id, reason, ..
        } => {
            rule_detail(detail, &rule_id).skipped += 1;
            detail.skipped_total += 1;
            if detail.skipped.len() < MAX_SKIPPED_DETAILS {
                detail.skipped.push(SkippedItem {
                    rule_id,
                    // 整条规则被跳过，没有具体条目路径。
                    path: String::new(),
                    reason,
                });
            }
        }
        Record::PlanFinished {
            free_before,
            free_after,
            cancelled,
            ..
        } => {
            detail.free_before = free_before;
            detail.free_after = free_after;
            detail.finished = true;
            detail.cancelled = cancelled;
        }
        // 特殊操作、官方命令、隔离批次和迁移不属于任何计划，不会走到这里。
        Record::SpecialAction { .. }
        | Record::CommandRun { .. }
        | Record::QuarantineStage { .. }
        | Record::MigrationStage { .. }
        | Record::JunctionStage { .. } => {}
    }
}

/// 取某条规则的详情统计，没有就先补一条。
fn rule_detail<'a>(detail: &'a mut PlanDetail, rule_id: &str) -> &'a mut RuleDetail {
    let index = match detail.rules.iter().position(|rule| rule.rule_id == rule_id) {
        Some(index) => index,
        None => {
            detail.rules.push(RuleDetail::new(rule_id));
            detail.rules.len() - 1
        }
    };

    &mut detail.rules[index]
}

impl RuleDetail {
    fn new(rule_id: &str) -> Self {
        Self {
            rule_id: rule_id.to_string(),
            deleted_files: 0,
            deleted_dirs: 0,
            freed_allocated: 0,
            skipped: 0,
        }
    }
}

/// 日志目录，和 CClear 自身的其他数据一起放在 LocalAppData 下。
pub fn journal_dir() -> io::Result<PathBuf> {
    known_folder(KnownFolder::LocalAppData)
        .map(|path| path.join("CClear").join("journal"))
        .map_err(|error| io::Error::other(error.to_string()))
}

/// 当天日期，用作日志文件名。
fn today_string() -> String {
    let now = local_now();
    format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    )
}

/// 本地时间。多线程进程里取本地时区可能失败，失败就退回 UTC，只影响文件名和日期。
fn local_now() -> OffsetDateTime {
    OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc())
}

/// Unix 秒。
pub fn unix_now() -> i64 {
    local_now().unix_timestamp()
}
