//! 隔离区事务引擎：复制 → 校验 → 删除源。
//!
//! 整个过程持久化在批次 manifest 里，进程中途被结束也能靠 `recover_pending` 找回状态：
//! 源文件只有在副本写好、哈希对得上、并在同一个句柄上复核过之后才会被删除。

pub mod manifest;
pub mod settings;
pub mod volumes;

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use uuid::Uuid;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_READONLY};

use crate::guard::{self, DirHandle, Expect, Forbidden, GuardError, OpenIntent};
use crate::journal::{JournalSink, Record, unix_now};
use crate::win;
use crate::win::volume;

pub use manifest::{BatchStage, BatchSummary, DATA_DIR, ItemState, Manifest, ManifestItem};
pub use settings::{load_quarantine_root, save_quarantine_root, settings_path};
pub use volumes::{QUARANTINE_DIR_NAME, candidate_volumes, default_root};

/// 整批最多用到源文件大小之和，再预留 2 GiB 给系统和其他程序。
const RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 复制过程中每写这么多字节回头看一眼目标卷剩余空间。
const SPACE_CHECK_BYTES: u64 = 256 * 1024 * 1024;

/// 复制过程中目标卷剩余空间低于这个值就停止并回滚。
const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// 批次存放超过这么多天就提示用户处理。
pub const OVERDUE_DAYS: u64 = 7;

/// 隔离引擎的错误。每种失败都对应“跳过并报告”，不会扩大删除范围。
#[derive(Debug, Error)]
pub enum QuarantineError {
    #[error("隔离区所在卷剩余空间不足，整批未执行（至少需要 {needed} 字节）")]
    NoSpace { needed: u64 },

    #[error("隔离区不能和源在同一个卷，请在隔离区页选择其他卷")]
    SameVolume,

    #[error("没有可以隔离的条目")]
    NothingToDo,

    #[error("隔离批次不存在或记录已损坏")]
    MissingBatch,

    #[error("批次当前状态是「{stage}」，不能{action}")]
    WrongStage {
        stage: &'static str,
        action: &'static str,
    },

    #[error("这个批次没有需要还原的文件：源都还在原位")]
    NothingToRestore,

    #[error("这个批次没有等待删除源的条目，请改用还原或彻底删除")]
    NothingToContinue,

    #[error("隔离副本已丢失，不能删除源文件：{}", .0.display())]
    CopyMissing(PathBuf),

    #[error("隔离副本校验不一致，不能删除源文件：{}", .0.display())]
    CopyCorrupted(PathBuf),

    #[error("源文件在复制过程中发生了变化：{}", .0.display())]
    Verify(PathBuf),

    #[error("路径护栏拒绝了这个操作：{0}")]
    Guard(#[from] GuardError),

    #[error("文件操作失败：{0}")]
    Io(#[from] io::Error),

    /// 只为测试准备：模拟进程在复制过程中被强行结束。
    #[error("测试注入的中断")]
    Injected,
}

/// 交给隔离引擎的一个条目。元数据在预检时从源文件重新读取，不信任调用方的缓存。
#[derive(Clone, Debug)]
pub struct QuarantineItem {
    pub anchor_index: u16,
    pub rel: Vec<OsString>,
}

impl QuarantineItem {
    /// 按锚点序号和相对路径构造条目。
    pub fn new(anchor_index: u16, rel: &[&str]) -> Self {
        Self {
            anchor_index,
            rel: rel.iter().map(OsString::from).collect(),
        }
    }
}

/// 一次隔离的结果。
#[derive(Clone, Debug)]
pub struct QuarantineOutcome {
    pub batch_id: Uuid,
    pub stage: BatchStage,
    /// 源已删除、只留在隔离区的条目数。
    pub moved: u64,
    /// 这些条目的逻辑大小合计。
    pub moved_size: u64,
    /// 源发生变化、源和副本都保留的条目数。
    pub kept: u64,
    /// 预检就跳过的条目和原因。
    pub skipped: Vec<(PathBuf, String)>,
}

/// 一次还原的结果。
#[derive(Clone, Debug)]
pub struct RestoreOutcome {
    pub batch_id: Uuid,
    pub stage: BatchStage,
    pub restored: u64,
    /// 原位已经有同名文件、因此没有还原的路径。
    pub conflicts: Vec<PathBuf>,
}

/// 启动恢复时对某个批次采取的动作。
#[derive(Clone, Debug)]
pub struct RecoveryReport {
    pub batch_id: Uuid,
    pub stage: BatchStage,
    pub detail: String,
}

/// 异常批次的人工处理方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveAction {
    /// 逐项复核源文件和副本，能确认的继续隔离。
    Continue,
    /// 放弃副本，源保持不变。
    Abandon,
}

/// 一次复制校验后的结果，供 T13、T16 这类复用隔离流程的任务使用。
#[derive(Clone, Debug)]
pub struct CopiedFile {
    pub source: PathBuf,
    pub target: PathBuf,
    pub hash: String,
}

/// 隔离区的根目录，以及它的容量策略。
///
/// 线上用 `open` 按真实卷查询空间；测试用 `for_test` 放开“不能和源同卷”的限制，
/// 并用 `with_free_space` 注入剩余空间，避免测试真的写满一块盘。
pub struct QuarantineRoot {
    root: PathBuf,
    /// 是否允许隔离区和源在同一个卷。只有测试会打开它。
    allow_same_volume: bool,
    /// 首次创建根目录时是否设置自保护 DACL。测试目录不需要。
    apply_acl: bool,
    free_space: Box<dyn Fn(&Path) -> u64 + Send + Sync>,
    /// 测试注入：复制完第 N 个文件后中断，模拟进程被强行结束。
    crash_after_files: Option<usize>,
    /// 测试注入：复制校验完成后、删除源之前调用一次。
    after_copy: Option<Box<dyn Fn() + Send + Sync>>,
}

impl QuarantineRoot {
    /// 线上用法：按真实卷查询剩余空间。
    pub fn open(root: PathBuf) -> Self {
        Self {
            root,
            allow_same_volume: false,
            apply_acl: true,
            free_space: Box::new(volume::free_space),
            crash_after_files: None,
            after_copy: None,
        }
    }

    /// 测试用法：允许隔离根和源在同一个卷，也不去改隔离目录的 DACL。
    pub fn for_test(root: PathBuf) -> Self {
        Self {
            root,
            allow_same_volume: true,
            apply_acl: false,
            free_space: Box::new(volume::free_space),
            crash_after_files: None,
            after_copy: None,
        }
    }

    /// 线上用法：读设置里保存的隔离根。还没设置过时返回 None。
    pub fn from_settings() -> Option<Self> {
        let root = load_quarantine_root().ok().flatten()?;
        Some(Self::open(root))
    }

    /// 注入剩余空间来源。测试用它给出“空间不足”或“空间充足”。
    pub fn with_free_space(
        mut self,
        provider: impl Fn(&Path) -> u64 + Send + Sync + 'static,
    ) -> Self {
        self.free_space = Box::new(provider);
        self
    }

    /// 注入崩溃点：复制完第 `count` 个文件后直接返回错误，不写回滚状态。
    pub fn with_crash_after_files(mut self, count: usize) -> Self {
        self.crash_after_files = Some(count);
        self
    }

    /// 注入“复制之后、删源之前”的动作。测试用它改动源文件。
    pub fn with_after_copy(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.after_copy = Some(Box::new(hook));
        self
    }

    /// 隔离区的根目录。
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// 建根目录。首次创建时把 DACL 收紧到 SYSTEM、管理员和当前用户。
    pub fn ensure_root(&self) -> Result<(), QuarantineError> {
        if self.root.exists() {
            return Ok(());
        }

        fs::create_dir_all(&self.root)?;

        if self.apply_acl {
            win::acl::protect_quarantine_root(&self.root)
                .map_err(|error| QuarantineError::Io(io::Error::other(error)))?;
        }

        Ok(())
    }

    /// 隔离一批条目：预检 → 容量检查 → 复制 → 校验 → 删除源。
    pub fn quarantine(
        &self,
        rule_id: &str,
        forbidden: &Forbidden,
        anchors: &[PathBuf],
        items: &[QuarantineItem],
        journal: &mut dyn JournalSink,
        cancel: &AtomicBool,
    ) -> Result<QuarantineOutcome, QuarantineError> {
        // 1. 预检。这一步只读：源打不开、是目录、是链接或 EFS 加密的条目都跳过，源保持不动。
        let opened: Vec<Result<DirHandle, String>> = anchors
            .iter()
            .map(|path| guard::open_anchor(path, forbidden).map_err(|error| error.to_string()))
            .collect();

        let mut prepared: Vec<ManifestItem> = Vec::new();
        let mut skipped: Vec<(PathBuf, String)> = Vec::new();
        let mut needed = 0u64;

        for item in items {
            let index = item.anchor_index as usize;
            let Some(anchor_path) = anchors.get(index) else {
                skipped.push((PathBuf::new(), "锚点序号超出清单".to_string()));
                continue;
            };
            let path = join_parts(anchor_path, &item.rel);

            let anchor = match &opened[index] {
                Ok(anchor) => anchor,
                Err(reason) => {
                    skipped.push((path, reason.clone()));
                    continue;
                }
            };

            let entry = match guard::open_rel(anchor, &item.rel, OpenIntent::Inspect, forbidden) {
                Ok(entry) => entry,
                Err(error) => {
                    skipped.push((path, error.to_string()));
                    continue;
                }
            };

            if entry.is_dir {
                skipped.push((path, "目录不隔离，已跳过".to_string()));
                continue;
            }
            // 只读属性会随复制带进隔离区，之后删除源会被护栏拒绝，
            // 而清只读属性是硬性禁止的，所以这类文件在预检就跳过。
            if entry.attributes & FILE_ATTRIBUTE_READONLY.0 != 0 {
                skipped.push((path, "只读文件，已跳过".to_string()));
                continue;
            }
            // EFS 加密的内容复制到别的卷上会失去加密保护，直接拒绝。
            if entry.attributes & FILE_ATTRIBUTE_ENCRYPTED.0 != 0 {
                skipped.push((path, "EFS 加密文件，已跳过".to_string()));
                continue;
            }

            needed = needed.saturating_add(entry.size);
            prepared.push(ManifestItem {
                anchor_index: item.anchor_index,
                anchor: anchor_path.to_string_lossy().into_owned(),
                rel: item
                    .rel
                    .iter()
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect(),
                volume_serial: entry.identity.volume_serial,
                file_id: entry.identity.file_id.to_string(),
                size: entry.size,
                mtime: entry.mtime,
                ctime: entry.ctime,
                hash: String::new(),
                state: ItemState::Pending,
                note: String::new(),
            });
        }

        if prepared.is_empty() {
            return Err(QuarantineError::NothingToDo);
        }

        // 隔离区和源同卷时，清理既腾不出空间也挡不住误删，直接拒绝。
        if !self.allow_same_volume
            && volume::path_volume_serial(&self.root) == Some(prepared[0].volume_serial)
        {
            return Err(QuarantineError::SameVolume);
        }

        // 2. 容量检查。整批拒绝时一行都不写，目标目录里不会留下任何东西。
        let needed_total = needed.saturating_add(RESERVE_BYTES);
        if self.free_space_at(&self.root) < needed_total {
            return Err(QuarantineError::NoSpace {
                needed: needed_total,
            });
        }

        self.ensure_root()?;

        let batch_id = Uuid::new_v4();
        let batch_dir = self.batch_dir(batch_id);
        fs::create_dir_all(batch_dir.join(DATA_DIR))?;

        let mut manifest = Manifest {
            batch_id,
            created_at: unix_now(),
            rule_id: rule_id.to_string(),
            stage: BatchStage::Planned,
            note: String::new(),
            items: prepared,
        };
        manifest.save(&batch_dir)?;

        let count = manifest.items.len();
        let note = format!("开始隔离 {count} 个文件");
        self.set_stage(
            &batch_dir,
            &mut manifest,
            BatchStage::Copying,
            &note,
            journal,
        )?;

        // 3. 复制。目标文件已存在就报错，绝不覆盖。
        let mut copied_bytes = 0u64;
        let mut checked_bytes = 0u64;
        let mut copied = 0usize;

        for index in 0..manifest.items.len() {
            if cancel.load(Ordering::SeqCst) {
                self.rollback(
                    &batch_dir,
                    &mut manifest,
                    "用户取消了隔离，已删除刚复制出来的副本",
                    journal,
                )?;
                return Ok(outcome_of(&manifest, skipped));
            }

            let item = &manifest.items[index];
            let source = item.source_path();
            let target = item.copy_path(&batch_dir);

            if let Err(error) = copy_one_verified(&source, &target) {
                self.rollback(
                    &batch_dir,
                    &mut manifest,
                    "复制失败，已删除刚复制出来的副本",
                    journal,
                )?;
                return Err(error);
            }
            // 复制保留了内容，时间戳要单独对齐源文件；对齐失败同样按复制失败回滚。
            if let Err(error) = win::set_file_times(&target, item.ctime, item.mtime) {
                self.rollback(
                    &batch_dir,
                    &mut manifest,
                    "复制后无法对齐时间戳，已删除刚复制出来的副本",
                    journal,
                )?;
                return Err(QuarantineError::Io(error));
            }

            copied += 1;
            copied_bytes = copied_bytes.saturating_add(item.size);

            // 每写 256 MiB 看一次剩余空间：别的程序把卷写满是真实存在的失败场景。
            if copied_bytes - checked_bytes >= SPACE_CHECK_BYTES {
                checked_bytes = copied_bytes;
                if self.free_space_at(&self.root) < MIN_FREE_BYTES {
                    self.rollback(
                        &batch_dir,
                        &mut manifest,
                        "目标卷剩余空间不足，已删除刚复制出来的副本",
                        journal,
                    )?;
                    return Err(QuarantineError::NoSpace {
                        needed: MIN_FREE_BYTES,
                    });
                }
            }

            // 测试注入：模拟进程在这里被强行结束，manifest 停在 Copying。
            if self.crash_after_files == Some(copied) {
                return Err(QuarantineError::Injected);
            }
        }

        // 4. 逐个比对源和副本的 blake3。任何一边读不出来都按“两份都保留”处理，
        //    不能因为读失败就带着没校验的副本去删源。
        for index in 0..manifest.items.len() {
            let item = &mut manifest.items[index];
            let source_hash = hash_hex(&item.source_path());
            let copy_hash = hash_hex(&item.copy_path(&batch_dir));

            let (Ok(source_hash), Ok(copy_hash)) = (source_hash, copy_hash) else {
                item.state = ItemState::Kept;
                item.note = "源或副本无法读取，未做校验，源和副本都保留".to_string();
                continue;
            };

            item.hash = source_hash;

            if item.hash != copy_hash {
                item.state = ItemState::Kept;
                item.note = "复制后校验不一致，源和副本都保留".to_string();
                continue;
            }

            item.state = ItemState::Copied;
        }

        let verified = manifest.count_state(ItemState::Copied);
        let note = format!("{verified} 个文件的副本已校验通过");
        self.set_stage(
            &batch_dir,
            &mut manifest,
            BatchStage::CopiedVerified,
            &note,
            journal,
        )?;

        // 测试注入：在复制校验之后、删除源之前让源发生变化。
        if let Some(hook) = &self.after_copy {
            hook();
        }

        // 5. 删除源。每次都在同一个句柄上复核身份、大小和修改时间。
        self.set_stage(
            &batch_dir,
            &mut manifest,
            BatchStage::DeletingSource,
            "逐个复核后删除源文件",
            journal,
        )?;

        let mut cancelled = false;
        for index in 0..manifest.items.len() {
            if manifest.items[index].state != ItemState::Copied {
                continue;
            }
            if cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }

            let item = &mut manifest.items[index];
            match delete_source(&opened, item, forbidden) {
                Ok(()) => {
                    item.state = ItemState::SourceDeleted;
                    item.note.clear();
                }
                Err(error) => {
                    // 源已经变了：不删源，副本留着，批次等下按“部分完成”处理。
                    item.state = ItemState::Kept;
                    item.note = format!("源已变化，源和副本都保留：{error}");
                }
            }
        }

        let kept = manifest.count_state(ItemState::Kept);
        let (stage, note) = if cancelled {
            (
                BatchStage::NeedsAttention,
                "隔离被取消，已复制的副本仍保留在隔离区".to_string(),
            )
        } else if kept > 0 {
            (
                BatchStage::NeedsAttention,
                format!("部分完成：{kept} 个文件在复制后发生变化，源和副本都保留"),
            )
        } else {
            (
                BatchStage::Committed,
                format!("已隔离 {} 个文件", manifest.items.len()),
            )
        };
        self.set_stage(&batch_dir, &mut manifest, stage, &note, journal)?;

        Ok(outcome_of(&manifest, skipped))
    }

    /// 还原整个批次。原位已有同名文件时跳过该条目，绝不覆盖。
    pub fn restore(
        &self,
        batch_id: Uuid,
        journal: &mut dyn JournalSink,
    ) -> Result<RestoreOutcome, QuarantineError> {
        let batch_dir = self.batch_dir(batch_id);
        let mut manifest = self.load_manifest(&batch_dir)?;

        // 只有已经隔离完成的批次才谈得上还原：中止的批次源一个都没动。
        if !matches!(
            manifest.stage,
            BatchStage::Committed | BatchStage::NeedsAttention
        ) {
            return Err(QuarantineError::WrongStage {
                stage: manifest.stage.label(),
                action: "还原",
            });
        }

        // 源都还在原位时没有可还原的东西，不能把批次误标成“已还原”。
        if manifest.count_state(ItemState::SourceDeleted) == 0 {
            return Err(QuarantineError::NothingToRestore);
        }

        self.set_stage(
            &batch_dir,
            &mut manifest,
            BatchStage::Restoring,
            "开始还原",
            journal,
        )?;

        let mut restored = 0u64;
        let mut conflicts: Vec<PathBuf> = Vec::new();

        for index in 0..manifest.items.len() {
            if manifest.items[index].state != ItemState::SourceDeleted {
                continue;
            }

            let item = &mut manifest.items[index];
            let source = item.source_path();
            let copy = item.copy_path(&batch_dir);

            // 原位已经有同名文件：用户的文件不能被覆盖。
            if source.exists() {
                conflicts.push(source);
                item.note = "原位置已有同名文件，已跳过".to_string();
                continue;
            }
            if !copy.exists() {
                item.note = "隔离副本已丢失，无法还原".to_string();
                continue;
            }

            // 先确认副本本身没坏，再往原位写。
            if hash_hex(&copy)? != item.hash {
                item.note = "隔离副本校验不一致，未还原".to_string();
                continue;
            }

            if let Some(parent) = source.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&copy, &source)?;
            win::set_file_times(&source, item.ctime, item.mtime)?;

            if hash_hex(&source)? != item.hash {
                item.note = "还原后校验不一致，副本仍保留".to_string();
                continue;
            }

            remove_file_verified(&batch_dir, &item.copy_rel_path())?;
            item.state = ItemState::Restored;
            item.note.clear();
            restored += 1;
        }

        let pending = manifest.count_state(ItemState::SourceDeleted);
        let (stage, note) = if pending == 0 {
            (BatchStage::Restored, format!("已还原 {restored} 个文件"))
        } else {
            (
                BatchStage::NeedsAttention,
                format!("还有 {pending} 个文件没有还原，处理之后可以再试"),
            )
        };
        self.set_stage(&batch_dir, &mut manifest, stage, &note, journal)?;

        Ok(RestoreOutcome {
            batch_id,
            stage,
            restored,
            conflicts,
        })
    }

    /// 彻底删除一个批次：整棵批次目录用护栏后序删除。
    pub fn purge(
        &self,
        batch_id: Uuid,
        journal: &mut dyn JournalSink,
    ) -> Result<(), QuarantineError> {
        let batch_dir = self.batch_dir(batch_id);
        let mut manifest = self.load_manifest(&batch_dir)?;

        self.set_stage(
            &batch_dir,
            &mut manifest,
            BatchStage::Purging,
            "彻底删除批次",
            journal,
        )?;

        // 批次目录是隔离根的直接子目录，以它的父目录为锚点整棵删除。
        remove_dir_tree(&batch_dir)?;

        // 目录已经不存在了，Purged 只能记在日志里。
        journal.append(&Record::QuarantineStage {
            batch_id,
            at: unix_now(),
            stage: BatchStage::Purged.label().to_string(),
            detail: format!("已彻底删除批次，条目 {} 个", manifest.items.len()),
        })?;

        Ok(())
    }

    /// 处理异常批次：继续逐项复核，或者放弃副本。
    pub fn resolve(
        &self,
        batch_id: Uuid,
        action: ResolveAction,
        forbidden: &Forbidden,
        journal: &mut dyn JournalSink,
    ) -> Result<QuarantineOutcome, QuarantineError> {
        let batch_dir = self.batch_dir(batch_id);
        let mut manifest = self.load_manifest(&batch_dir)?;

        // 只有停在“需要处理”的批次才需要人工决定继续还是放弃。
        if manifest.stage != BatchStage::NeedsAttention {
            return Err(QuarantineError::WrongStage {
                stage: manifest.stage.label(),
                action: "处理异常",
            });
        }

        match action {
            ResolveAction::Continue => {
                // 没有“副本已就绪、源还没删”的条目时不能继续：例如还原到一半被打断的批次，
                // 这里什么也做不了，误标成“已隔离”会让用户以为源已经删掉了。
                if manifest.count_state(ItemState::Copied) == 0 {
                    return Err(QuarantineError::NothingToContinue);
                }

                self.set_stage(
                    &batch_dir,
                    &mut manifest,
                    BatchStage::DeletingSource,
                    "继续处理：逐项复核后删除源",
                    journal,
                )?;

                for index in 0..manifest.items.len() {
                    if manifest.items[index].state != ItemState::Copied {
                        continue;
                    }

                    let item = &mut manifest.items[index];
                    match continue_item(&batch_dir, item, forbidden) {
                        Ok(()) => {
                            item.state = ItemState::SourceDeleted;
                            item.note.clear();
                        }
                        Err(error) => {
                            // 复核没通过就两份都留下，绝不拿副本去换源。
                            item.state = ItemState::Kept;
                            item.note = format!("复核未通过，源和副本都保留：{error}");
                        }
                    }
                }

                let kept = manifest.count_state(ItemState::Kept);
                let (stage, note) = if kept > 0 {
                    (
                        BatchStage::NeedsAttention,
                        format!("部分完成：{kept} 个文件复核未通过，源和副本都保留"),
                    )
                } else {
                    (
                        BatchStage::Committed,
                        format!("已隔离 {} 个文件", manifest.items.len()),
                    )
                };
                self.set_stage(&batch_dir, &mut manifest, stage, &note, journal)?;
            }
            ResolveAction::Abandon => {
                self.abandon_copies(&batch_dir, &mut manifest, journal)?;
            }
        }

        Ok(outcome_of(&manifest, Vec::new()))
    }

    /// 放弃副本：只删源还在原位的那些副本。
    ///
    /// 源已经删除的条目，副本是唯一的一份，必须原样保留——否则“放弃”
    /// 就是把用户的数据删掉。这类条目留着，由用户在隔离区页还原或彻底删除。
    fn abandon_copies(
        &self,
        batch_dir: &Path,
        manifest: &mut Manifest,
        journal: &mut dyn JournalSink,
    ) -> Result<(), QuarantineError> {
        let mut removed = 0u64;

        for index in 0..manifest.items.len() {
            // 只有源仍在原位的条目能放弃副本；其余状态一律不动。
            if !matches!(
                manifest.items[index].state,
                ItemState::Pending | ItemState::Copied | ItemState::Kept
            ) {
                continue;
            }

            let item = &mut manifest.items[index];
            let copy = item.copy_path(batch_dir);

            // 副本已经不在了也算处理过：源还在原位，没有任何损失。
            item.state = ItemState::Pending;
            if !copy.exists() {
                item.note = "副本已不存在，源保持不变".to_string();
                continue;
            }

            remove_file_verified(batch_dir, &item.copy_rel_path())?;
            item.note = "已放弃副本".to_string();
            removed += 1;
        }

        // 源已删除的条目必须留在隔离区里，批次也要停在能继续操作的状态上。
        let isolated = manifest.count_state(ItemState::SourceDeleted);
        let (stage, note) = if isolated == 0 {
            (
                BatchStage::Aborted,
                format!("已放弃 {removed} 个副本，源文件保持不变"),
            )
        } else if removed == 0 {
            (
                BatchStage::Committed,
                format!("没有可放弃的副本；{isolated} 个文件只剩隔离副本，仍保留在隔离区"),
            )
        } else {
            (
                BatchStage::Committed,
                format!("已放弃 {removed} 个副本；{isolated} 个文件源已删除，副本仍保留在隔离区"),
            )
        };
        self.set_stage(batch_dir, manifest, stage, &note, journal)?;

        if isolated == 0 {
            // 副本都删掉了，顺手清掉空的 data 目录；清不掉也不影响数据安全，
            // 交给“彻底删除”处理。
            let _ = remove_dir_tree(&batch_dir.join(DATA_DIR));
        }

        Ok(())
    }

    /// 应用启动时调用：把上次没有做完的批次收到一个确定的状态上。
    pub fn recover_pending(
        &self,
        journal: &mut dyn JournalSink,
    ) -> Result<Vec<RecoveryReport>, QuarantineError> {
        let mut reports = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            // 还没建隔离根，谈不上恢复。
            return Ok(reports);
        };

        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let batch_dir = entry.path();
            let Ok(mut manifest) = Manifest::load(&batch_dir) else {
                // 清单读不出来就没法判断进度，原样留着让用户自己处理。
                continue;
            };

            match manifest.stage {
                // 计划好还没开始写、或者写了一半被打断：副本都是半成品，源没有动过。
                BatchStage::Planned | BatchStage::Copying => {
                    remove_dir_tree(&batch_dir.join(DATA_DIR))?;
                    for item in manifest.items.iter_mut() {
                        item.state = ItemState::Pending;
                        item.hash.clear();
                    }
                    self.set_stage(
                        &batch_dir,
                        &mut manifest,
                        BatchStage::Aborted,
                        "上次隔离过程被中断，已删除半成品副本，源文件完好",
                        journal,
                    )?;
                }
                BatchStage::CopiedVerified | BatchStage::DeletingSource | BatchStage::Restoring => {
                    self.set_stage(
                        &batch_dir,
                        &mut manifest,
                        BatchStage::NeedsAttention,
                        "上次操作被中断，请在隔离区页继续或放弃",
                        journal,
                    )?;
                }
                _ => continue,
            }

            reports.push(RecoveryReport {
                batch_id: manifest.batch_id,
                stage: manifest.stage,
                detail: manifest.note.clone(),
            });
        }

        Ok(reports)
    }

    /// 所有批次的概要，按创建时间从新到旧。
    pub fn batches(&self) -> Result<Vec<BatchSummary>, QuarantineError> {
        self.read_batches()
    }

    /// 存放超过 `OVERDUE_DAYS` 天的批次。界面启动时用它提示用户处理。
    pub fn overdue(&self) -> Result<Vec<BatchSummary>, QuarantineError> {
        Ok(self
            .read_batches()?
            .into_iter()
            .filter(|batch| batch.age_days > OVERDUE_DAYS)
            .collect())
    }

    /// 读隔离根下的全部批次。
    fn read_batches(&self) -> Result<Vec<BatchSummary>, QuarantineError> {
        let mut batches = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Ok(batches);
        };

        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let Ok(manifest) = Manifest::load(&entry.path()) else {
                continue;
            };

            let now = unix_now();
            let age_days = now.saturating_sub(manifest.created_at) as u64 / 86_400;
            let items = manifest.item_count();
            let size = manifest.total_size();
            batches.push(BatchSummary {
                batch_id: manifest.batch_id,
                created_at: manifest.created_at,
                rule_id: manifest.rule_id,
                stage: manifest.stage,
                note: manifest.note,
                items,
                size,
                age_days,
            });
        }

        batches.sort_by_key(|batch| std::cmp::Reverse(batch.created_at));
        Ok(batches)
    }

    /// 某个批次的目录。
    fn batch_dir(&self, batch_id: Uuid) -> PathBuf {
        self.root.join(batch_id.to_string())
    }

    /// 读批次清单，读不出来时统一报“批次不存在或记录已损坏”。
    fn load_manifest(&self, batch_dir: &Path) -> Result<Manifest, QuarantineError> {
        Manifest::load(batch_dir).map_err(|_| QuarantineError::MissingBatch)
    }

    /// 状态变化：先写 manifest（恢复的唯一依据），再写日志。
    fn set_stage(
        &self,
        batch_dir: &Path,
        manifest: &mut Manifest,
        stage: BatchStage,
        detail: &str,
        journal: &mut dyn JournalSink,
    ) -> Result<(), QuarantineError> {
        manifest.stage = stage;
        manifest.note = detail.to_string();
        manifest.save(batch_dir)?;

        // 日志写不进去算失败：宁可停下，也不留下没有记录的状态变化。
        journal.append(&Record::QuarantineStage {
            batch_id: manifest.batch_id,
            at: unix_now(),
            stage: stage.label().to_string(),
            detail: detail.to_string(),
        })?;

        Ok(())
    }

    /// 回滚：删掉刚复制出来的副本，源一个都不动。
    fn rollback(
        &self,
        batch_dir: &Path,
        manifest: &mut Manifest,
        detail: &str,
        journal: &mut dyn JournalSink,
    ) -> Result<(), QuarantineError> {
        remove_dir_tree(&batch_dir.join(DATA_DIR))?;
        self.set_stage(batch_dir, manifest, BatchStage::Aborted, detail, journal)
    }

    /// 目标卷的剩余空间。路径还不存在（第一次使用）时退到最近存在的上级目录。
    fn free_space_at(&self, path: &Path) -> u64 {
        let mut current = Some(path);

        while let Some(candidate) = current {
            let free = (self.free_space)(candidate);
            if free > 0 {
                return free;
            }
            current = candidate.parent();
        }

        0
    }
}

/// 复制一个文件并用 blake3 校验，返回副本的哈希。目标已存在时报错，不覆盖。
fn copy_one_verified(source: &Path, target: &Path) -> Result<String, QuarantineError> {
    if target.exists() {
        return Err(QuarantineError::Io(io::Error::other(format!(
            "隔离目标已存在，未覆盖：{}",
            target.display()
        ))));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::copy(source, target)?;

    let hash = hash_hex(target)?;
    if hash != hash_hex(source)? {
        return Err(QuarantineError::Verify(source.to_path_buf()));
    }

    Ok(hash)
}

/// 把源锚点下的一组条目复制到 `dst`，逐个用 blake3 校验。
///
/// 只负责复制和校验：预检、容量检查和源删除由调用方按自己的事务流程处理。
/// 隔离区自己用它，T13 和 T16 复用同一套复制口径。
pub fn copy_tree_verified(
    src_anchor: &Path,
    rel_items: &[Vec<OsString>],
    dst: &Path,
) -> Result<Vec<CopiedFile>, QuarantineError> {
    let mut copied = Vec::with_capacity(rel_items.len());

    for rel in rel_items {
        let source = join_parts(src_anchor, rel);
        let target = join_parts(dst, rel);
        let hash = copy_one_verified(&source, &target)?;

        copied.push(CopiedFile {
            source,
            target,
            hash,
        });
    }

    Ok(copied)
}

/// 计算文件的 blake3 哈希，十六进制小写。
pub fn hash_hex(path: &Path) -> Result<String, QuarantineError> {
    let file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file)?;

    Ok(hasher.finalize().to_hex().to_string())
}

/// 删除源文件：相对锚点按删除意图打开，在同一个句柄上复核之后再删。
fn delete_source(
    opened: &[Result<DirHandle, String>],
    item: &ManifestItem,
    forbidden: &Forbidden,
) -> Result<(), GuardError> {
    let anchor = opened
        .get(item.anchor_index as usize)
        .and_then(|entry| entry.as_ref().ok())
        .ok_or(GuardError::Gone)?;

    let Some(identity) = item.identity() else {
        return Err(GuardError::Changed);
    };

    let rel: Vec<OsString> = item.rel.iter().map(OsString::from).collect();
    let entry = guard::open_rel(anchor, &rel, OpenIntent::Delete, forbidden)?;

    guard::delete(
        entry,
        &Expect {
            identity,
            size: item.size,
            mtime: item.mtime,
        },
    )
}

/// 继续处理一个“副本已就绪、源还没删”的条目。
///
/// 先确认副本还在、内容与记录的哈希一致，再确认源没变，最后才删源：
/// 副本缺失或损坏时删源等于把唯一一份数据丢掉。
fn continue_item(
    batch_dir: &Path,
    item: &ManifestItem,
    forbidden: &Forbidden,
) -> Result<(), QuarantineError> {
    let copy = item.copy_path(batch_dir);
    if !copy.exists() {
        return Err(QuarantineError::CopyMissing(copy));
    }
    if item.hash.is_empty() || hash_hex(&copy)? != item.hash {
        return Err(QuarantineError::CopyCorrupted(copy));
    }

    let anchor_path = PathBuf::from(&item.anchor);
    let anchor = guard::open_anchor(&anchor_path, forbidden)?;

    let source = item.source_path();
    if !source.exists() {
        return Err(QuarantineError::Guard(GuardError::Gone));
    }
    if hash_hex(&source)? != item.hash {
        return Err(QuarantineError::Verify(source));
    }

    let Some(identity) = item.identity() else {
        return Err(QuarantineError::Guard(GuardError::Changed));
    };

    let rel: Vec<OsString> = item.rel.iter().map(OsString::from).collect();
    let entry = guard::open_rel(&anchor, &rel, OpenIntent::Delete, forbidden)?;
    guard::delete(
        entry,
        &Expect {
            identity,
            size: item.size,
            mtime: item.mtime,
        },
    )?;

    Ok(())
}

/// 通过护栏删除批次目录里的一个文件。
fn remove_file_verified(batch_dir: &Path, rel: &[OsString]) -> Result<(), QuarantineError> {
    let forbidden = Forbidden::empty();
    let anchor = guard::open_anchor(batch_dir, &forbidden)?;

    let entry = guard::open_rel(&anchor, rel, OpenIntent::Delete, &forbidden)?;
    let expect = entry.expect();
    guard::delete(entry, &expect)?;

    Ok(())
}

/// 删掉一棵完全由 CClear 自己创建的目录树。目录不存在时什么也不做。
fn remove_dir_tree(path: &Path) -> Result<(), QuarantineError> {
    if !path.exists() {
        return Ok(());
    }

    let forbidden = Forbidden::empty();
    let parent = path
        .parent()
        .ok_or_else(|| QuarantineError::Io(io::Error::other("隔离目录没有父目录")))?;
    let name = path
        .file_name()
        .ok_or_else(|| QuarantineError::Io(io::Error::other("隔离目录没有名字")))?;

    let anchor = guard::open_anchor(parent, &forbidden)?;
    guard::delete_tree(&anchor, name, &forbidden)?;

    Ok(())
}

/// 拼出锚点下的完整路径，只用于复制和展示。
fn join_parts(base: &Path, rel: &[OsString]) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in rel {
        path.push(part);
    }
    path
}

/// 按 manifest 里的条目状态汇总一次隔离的结果。
fn outcome_of(manifest: &Manifest, skipped: Vec<(PathBuf, String)>) -> QuarantineOutcome {
    let moved = manifest.count_state(ItemState::SourceDeleted);
    let moved_size = manifest
        .items
        .iter()
        .filter(|item| item.state == ItemState::SourceDeleted)
        .map(|item| item.size)
        .sum();

    QuarantineOutcome {
        batch_id: manifest.batch_id,
        stage: manifest.stage,
        moved,
        moved_size,
        kept: manifest.count_state(ItemState::Kept),
        skipped,
    }
}
