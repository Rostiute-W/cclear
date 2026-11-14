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
    pub free_before: u64,
    pub free_after: u64,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub freed_allocated: u64,
    pub skipped: u64,
    /// 计划没有写完 PlanFinished 时是中断的执行。
    pub finished: bool,
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
        let dir = journal_dir()?;
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
    let dir = journal_dir()?;

    // 文件名是 ISO 日期，按名字排序就是按日期排序。
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .collect(),
        // 还没产生过日志时不是错误。
        Err(_) => return Ok((Vec::new(), 0)),
    };

    files.sort();
    if files.len() > days {
        files.drain(..files.len() - days);
    }

    let mut plans: Vec<PlanSummary> = Vec::new();
    let mut index: HashMap<Uuid, usize> = HashMap::new();
    let mut skipped_lines = 0usize;

    for file in files {
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

            apply(&mut plans, &mut index, record);
        }
    }

    plans.reverse();

    Ok((plans, skipped_lines))
}

/// 把一条记录并入对应的计划摘要。
fn apply(plans: &mut Vec<PlanSummary>, index: &mut HashMap<Uuid, usize>, record: Record) {
    let plan_id = record_plan_id(&record);

    let position = *index.entry(plan_id).or_insert_with(|| {
        plans.push(PlanSummary {
            plan_id,
            started_at: OffsetDateTime::UNIX_EPOCH,
            free_before: 0,
            free_after: 0,
            deleted_files: 0,
            deleted_dirs: 0,
            freed_allocated: 0,
            skipped: 0,
            finished: false,
        });
        plans.len() - 1
    });
    let summary = &mut plans[position];

    match record {
        Record::PlanStarted { at, .. } => {
            summary.started_at =
                OffsetDateTime::from_unix_timestamp(at).unwrap_or(OffsetDateTime::UNIX_EPOCH);
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
            ..
        } => {
            summary.free_before = free_before;
            summary.free_after = free_after;
            summary.deleted_files = deleted_files;
            summary.deleted_dirs = deleted_dirs;
            summary.freed_allocated = freed_allocated;
            summary.skipped = skipped;
            summary.finished = true;
        }
    }
}

/// 记录所属的计划 ID。
fn record_plan_id(record: &Record) -> Uuid {
    match record {
        Record::PlanStarted { plan_id, .. }
        | Record::ItemDone { plan_id, .. }
        | Record::ItemSkipped { plan_id, .. }
        | Record::RuleSkipped { plan_id, .. }
        | Record::PlanFinished { plan_id, .. } => *plan_id,
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
