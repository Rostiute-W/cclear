//! 纯数据目录的 junction 迁移与迁回。
//!
//! 迁移把目录整棵复制到 `<X>:\CClearMig\<id>`、校验，把源目录改名为 `<name>.cclear-bak-<id>`，
//! 再在原名位置建一个指向目标的 junction。每个阶段都先落盘再动手，进程被结束后可以靠
//! [`JunctionMigration::recover_pending`] 收到一个确定的状态上。
//!
//! 迁回以目标里的最新数据为准：先复制回来源卷并校验，再删链接本身、把迁回的目录改名回原名。
//! 备份和目标副本都要用户确认之后才删除，任何一步失败都只跳过并报告。

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
    FILE_ATTRIBUTE_RECALL_ON_OPEN,
};

use crate::guard::{self, DirHandle, Forbidden, GuardError, OpenIntent};
use crate::journal::{JournalSink, Record, unix_now};
use crate::migrate::{BACKUP_UNCONFIRMED, candidate_volumes, load_store, save_store};
use crate::model::{FileIdentity, KnownRoot, Recovery};
use crate::plan::{FrozenPlan, build_plan_at};
use crate::quarantine::{copy_tree_verified, hash_hex};
use crate::rules::resolve::{RootMap, measure_paths};
use crate::win::process::running_processes;
use crate::win::restart_manager;
use crate::win::{
    KnownFolder, char_equals_ignore_case, dirinfo, equals_ignore_case, known_folder, volume,
};

/// 目标卷下存放迁移目录的目录名。
pub const TARGET_DIR_NAME: &str = "CClearMig";

/// 清理备份目录时用的临时规则 ID。它不在内置规则目录里，只出现在迁移的清理清单中。
pub const BACKUP_RULE_ID: &str = "migration-dir-backup";

/// 迁回时给系统和其他程序留下的余量。
const RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 经 junction 校验读回的文件数。文档要求 3 个就够。
const VALIDATE_SAMPLE: usize = 3;

/// 占用检测最多查多少个文件。只为给出证据，不必查全部。
const LOCK_CHECK_LIMIT: usize = 256;

/// 资格检查最多收集多少条原因。列出太多只会让界面刷屏。
const PROBLEM_LIMIT: usize = 20;

/// 迁移过程里的错误。每种失败都对应“停下并报告”，不会扩大改动范围。
#[derive(Debug, Error)]
pub enum JunctionError {
    #[error("这个目录不能迁移：{0}")]
    NotEligible(String),

    #[error("这个卷不能作为迁移目标，请从候选卷里选：{0}")]
    BadVolume(String),

    #[error("迁移记录读不出来，请先手工检查：{0}")]
    Store(String),

    #[error("{0}")]
    BackupUnconfirmed(String),

    #[error("没有这条目录迁移记录")]
    Missing,

    #[error("当前状态是「{stage}」，不能做这一步")]
    WrongStage { stage: &'static str },

    #[error("目标目录已经存在，请换一个卷：{}", .0.display())]
    TargetExists(PathBuf),

    #[error("这个目录已经有迁移记录了：{}", .0.display())]
    AlreadyMigrated(PathBuf),

    #[error("来源盘剩余空间不足，迁回至少需要 {needed} 字节")]
    NoSpace { needed: u64 },

    #[error("目录已经不存在：{}", .0.display())]
    Gone(PathBuf),

    #[error("复制失败：{0}")]
    Copy(String),

    #[error("校验不一致，已停止：{}", .0.display())]
    Verify(PathBuf),

    #[error("这些程序正在使用源目录，请先退出：{0}")]
    RunningProcesses(String),

    #[error("目标目录仍在被使用或发生了变化，请关闭相关程序后重试：{0}")]
    ReturnRefused(String),

    #[error("占盘检测不完整，不能确认目录已经稳定：{0}")]
    NotVerified(String),

    #[error("备份目录里还有内容，未删除")]
    BackupNotEmpty(usize),

    #[error("无法解析系统目录：{0}")]
    Roots(String),

    /// 只为测试准备：模拟进程在迁移过程中被强行结束。
    #[error("测试注入的中断")]
    Interrupted,

    #[error("路径护栏拒绝了这个操作：{0}")]
    Guard(#[from] GuardError),

    #[error("文件操作失败：{0}")]
    Io(#[from] io::Error),

    #[error("日志写入失败：{0}")]
    Log(io::Error),
}

/// 目录迁移的阶段。状态机持久化在 `migrations.json` 里，同时写进操作日志。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DirStage {
    /// 资格检查通过，还没动手。
    Precheck,
    /// 正在往目标卷复制。
    Copying,
    /// 复制完成并逐个校验过。
    Verified,
    /// 源目录已改名成备份（或即将改名，状态先落盘）。
    SourceRenamed,
    /// 原名位置已经建好 junction。
    JunctionCreated,
    /// 经 junction 读回的内容和复制时一致。
    Validated,
    /// 等用户打开应用实测，确认后才删除备份。
    AwaitingUserTest,
    /// 备份已经删除。
    BackupDeleted,
    /// 整条流程结束。
    Done,
    /// 正在把目标里的数据复制回来源盘。落盘在复制开始之前，崩溃之后才知道临时目录的来历。
    Returning,
    /// 中断在需要人决定的位置，界面提供“继续”或“迁回”。
    NeedsAttention,
    /// 已经迁回，原位置恢复成普通目录。
    Returned,
    /// 中途失败并自动回滚，源目录保持原样。
    Failed,
}

impl DirStage {
    /// 机器可读的阶段名，界面据此选样式。
    pub fn key(self) -> &'static str {
        match self {
            DirStage::Precheck => "precheck",
            DirStage::Copying => "copying",
            DirStage::Verified => "verified",
            DirStage::SourceRenamed => "sourceRenamed",
            DirStage::JunctionCreated => "junctionCreated",
            DirStage::Validated => "validated",
            DirStage::AwaitingUserTest => "awaitingUserTest",
            DirStage::BackupDeleted => "backupDeleted",
            DirStage::Done => "done",
            DirStage::Returning => "returning",
            DirStage::NeedsAttention => "needsAttention",
            DirStage::Returned => "returned",
            DirStage::Failed => "failed",
        }
    }

    /// 界面上的叫法。
    pub fn label(self) -> &'static str {
        match self {
            DirStage::Precheck => "已通过资格检查",
            DirStage::Copying => "正在复制",
            DirStage::Verified => "复制完成并校验通过",
            DirStage::SourceRenamed => "源目录已改名成备份",
            DirStage::JunctionCreated => "已创建 junction",
            DirStage::Validated => "经 junction 校验通过",
            DirStage::AwaitingUserTest => "等待确认应用正常",
            DirStage::BackupDeleted => "备份已删除",
            DirStage::Done => "完成",
            DirStage::Returning => "正在迁回",
            DirStage::NeedsAttention => "需要处理",
            DirStage::Returned => "已迁回",
            DirStage::Failed => "已回滚",
        }
    }

    /// 是否已经建过 junction。这些状态才谈得上迁回。
    ///
    /// `Returned` 和 `Failed` 都不算：迁回之后原位置已经是普通目录，链接已经被删掉了。
    /// `Done` 算：备份删掉之后链接仍然是活的，用户随时可以要求迁回。
    /// `NeedsAttention` 可能停在“链接建好之后”，也可能停在“改名成功但建链接失败”，
    /// 所以这一项只能和磁盘上的实际情况一起判断（见 `JunctionMigration::source_is_junction`）。
    pub fn has_junction(self) -> bool {
        matches!(
            self,
            DirStage::JunctionCreated
                | DirStage::Validated
                | DirStage::AwaitingUserTest
                | DirStage::BackupDeleted
                | DirStage::Done
                | DirStage::NeedsAttention
        )
    }
}

/// 一条目录迁移记录。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirMigration {
    /// 8 位十六进制的迁移 ID，同时是目标目录名。
    pub id: String,
    /// 源目录原始路径。
    pub source: String,
    /// 目标目录 `<X>:\CClearMig\<id>`。
    pub target: String,
    /// 备份目录 `<源目录>.cclear-bak-<id>`。
    pub backup: String,
    pub stage: DirStage,
    /// 这一步做了什么，或者为什么停在这里。
    pub note: String,
    /// 是否经 junction 读回校验过。只有验证过的记录才允许删除备份。
    #[serde(default)]
    pub verified: bool,
    /// Unix 秒。
    pub updated_at: i64,
}

impl DirMigration {
    /// 源目录路径。
    pub fn source_path(&self) -> PathBuf {
        PathBuf::from(&self.source)
    }

    /// 目标目录路径。迁移成功之后活的数据在这里。
    pub fn target_path(&self) -> PathBuf {
        PathBuf::from(&self.target)
    }

    /// 备份目录路径。
    pub fn backup_path(&self) -> PathBuf {
        PathBuf::from(&self.backup)
    }

    /// 迁回时用的临时目录 `<源目录>.cclear-return-<id>`。
    ///
    /// 界面靠它判断“源已改名、链接还没建”这个现场是不是真的：临时目录还在时
    /// 说明停在迁回中途，不能再当成迁移中断去“继续”。
    pub fn return_temp_path(&self) -> PathBuf {
        return_path(&self.source_path(), &self.id)
    }
}

/// 一次启动恢复的结果。
#[derive(Default)]
pub struct RecoverReport {
    /// 这次真正收到确定状态的记录。
    pub recovered: Vec<DirMigration>,
    /// 连状态都写不进去的记录及其原因。界面要显示，不能静默丢掉。
    pub failures: Vec<String>,
}

/// 源目录里一个条目（文件或目录）的快照。复制前后各取一次，用来确认源在复制期间没有变。
///
/// 目录也进快照：空目录不会出现在文件列表里，只比对文件就会把“多出来的空目录”漏掉。
struct SnapshotEntry {
    rel: Vec<OsString>,
    is_dir: bool,
    identity: FileIdentity,
    size: u64,
    mtime: u64,
}

/// 目录迁移的执行上下文。
///
/// 线上用 [`JunctionMigration::open`] 按真实卷和 Known Folder 构造；测试用
/// [`JunctionMigration::for_test`] 把状态文件、目标根目录和剩余空间都注入到临时目录里。
pub struct JunctionMigration {
    roots: RootMap,
    forbidden: Forbidden,
    /// 状态文件所在目录。None 表示 `%LOCALAPPDATA%\CClear`。
    store_dir: Option<PathBuf>,
    /// 目标根目录。None 表示按选择的目标卷算 `<卷>:\CClearMig`。
    target_root: Option<PathBuf>,
    /// 是否允许源和目标在同一個卷。只有测试会打开它。
    allow_same_volume: bool,
    free_space: Box<dyn Fn(&Path) -> u64 + Send + Sync>,
    /// 测试注入：到达这个阶段后立即返回错误，模拟进程被强行结束。
    stop_at: Option<DirStage>,
}

impl JunctionMigration {
    /// 线上用法：按 Known Folder 和真实卷构造。
    pub fn open() -> Result<Self, JunctionError> {
        let roots = RootMap::system().map_err(|error| JunctionError::Roots(error.to_string()))?;
        let forbidden = Forbidden::from_rules(&roots);

        Ok(Self {
            roots,
            forbidden,
            store_dir: None,
            target_root: None,
            allow_same_volume: false,
            free_space: Box::new(volume::free_space),
            stop_at: None,
        })
    }

    /// 测试用法：状态文件、目标根目录都放在 `dir` 下面，并允许源和目标同卷。
    pub fn for_test(roots: RootMap, dir: PathBuf) -> Self {
        let forbidden = Forbidden::from_rules(&roots);

        Self {
            roots,
            forbidden,
            store_dir: Some(dir.join("store")),
            target_root: Some(dir.join(TARGET_DIR_NAME)),
            allow_same_volume: true,
            free_space: Box::new(|_| 1 << 40),
            stop_at: None,
        }
    }

    /// 测试注入：走到这个阶段就中断，返回 [`JunctionError::Interrupted`]。
    pub fn with_stop_at(mut self, stage: DirStage) -> Self {
        self.stop_at = Some(stage);
        self
    }

    /// 全部目录迁移记录，按更新时间从新到旧。
    pub fn list(&self) -> Result<Vec<DirMigration>, JunctionError> {
        let mut records = self.load_all()?;
        records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
        Ok(records)
    }

    /// 最近一次读记录时的提示：主文件坏掉而退回备份时，界面要能看见。
    pub fn store_notice(&self) -> Option<String> {
        let dir = self.store_dir().ok()?;
        load_store(Some(&dir)).ok().and_then(|store| store.notice)
    }

    /// 记录是从 `.bak` 退回读出来的、用户还没确认时，会动文件或建删链接的入口先拒绝。
    ///
    /// 和工具迁移读的是同一份 `migrations.json`，用同一个 `from_backup` 判断，
    /// 所以两个迁移页看到的是同一个结论。
    fn refuse_without_backup(&self) -> Result<(), JunctionError> {
        let dir = self.store_dir()?;
        let store =
            load_store(Some(&dir)).map_err(|error| JunctionError::Store(error.to_string()))?;
        if store.from_backup {
            return Err(JunctionError::BackupUnconfirmed(
                BACKUP_UNCONFIRMED.to_string(),
            ));
        }

        Ok(())
    }

    /// 按 ID 取一条记录。
    pub fn get(&self, id: &str) -> Result<DirMigration, JunctionError> {
        self.load_one(id)
    }

    /// 源位置现在是不是一个 junction。
    ///
    /// 只看状态判断不出来：`NeedsAttention` 既可能是链接建好之后中断，也可能是改名成功但
    /// 建链接失败。这种时候必须回到磁盘上确认，才不会把用户唯一的一份数据当成可清理的副本。
    pub fn source_is_junction(&self, record: &DirMigration) -> bool {
        let source = record.source_path();
        let Ok(parent_path) = parent_of(&source) else {
            return false;
        };
        let Ok(name) = file_name_of(&source) else {
            return false;
        };
        let Ok(parent) = guard::open_anchor(&parent_path, &self.forbidden) else {
            return false;
        };

        guard::read_junction_target(&parent, name).is_ok()
    }

    /// 资格检查。返回空列表表示可以迁移，否则每一条都是拒绝原因。
    pub fn check_eligibility(&self, source: &Path) -> Vec<String> {
        let mut reasons = Vec::new();

        self.check_location(source, &mut reasons);
        if reasons.is_empty() {
            self.check_contents(source, &mut reasons);
        }

        reasons
    }

    /// 开始迁移：资格检查 → 复制 → 复核 → 改名 → 建 junction → 经 junction 校验。
    pub fn start(
        &self,
        source: &Path,
        volume_root: &str,
        journal: &mut dyn JournalSink,
    ) -> Result<DirMigration, JunctionError> {
        // 记录是从备份退回读出来的话，复制和改名都不能开始。
        self.refuse_without_backup()?;

        let reasons = self.check_eligibility(source);
        if !reasons.is_empty() {
            return Err(JunctionError::NotEligible(reasons.join("；")));
        }

        // 同一个目录只允许有一条还没结束的记录，否则两次迁移会互相覆盖状态。
        if let Some(existing) = self
            .load_all()?
            .into_iter()
            .find(|record| same_path(&record.source_path(), source))
            && !matches!(
                existing.stage,
                DirStage::Done | DirStage::Failed | DirStage::Returned
            )
        {
            return Err(JunctionError::AlreadyMigrated(source.to_path_buf()));
        }

        let target_root = self.target_root(source, volume_root)?;
        let id = self.new_id()?;
        let target = target_root.join(&id);
        if target.exists() {
            return Err(JunctionError::TargetExists(target));
        }
        let backup = backup_path(source, &id);

        let mut record = DirMigration {
            id,
            source: source.to_string_lossy().into_owned(),
            target: target.to_string_lossy().into_owned(),
            backup: backup.to_string_lossy().into_owned(),
            stage: DirStage::Precheck,
            note: String::new(),
            verified: false,
            updated_at: unix_now(),
        };
        self.set_stage(
            &mut record,
            DirStage::Precheck,
            "资格检查通过，准备复制",
            journal,
        )?;

        // 复制之前先记下源文件的身份、大小和修改时间，复制完再比一次。
        let snapshot = self.snapshot(source)?;

        self.set_stage(&mut record, DirStage::Copying, "正在复制到目标卷", journal)?;
        if let Err(error) = self.copy_tree(source, &target, &snapshot) {
            // 复制失败只是这次迁移没成功：源没动，把半成品副本删掉。
            let _ = self.delete_tree(&target);
            self.set_stage(
                &mut record,
                DirStage::Failed,
                &format!("复制失败，已删除目标副本：{error}"),
                journal,
            )?;
            return Err(error);
        }

        self.set_stage(
            &mut record,
            DirStage::Verified,
            "复制完成并逐个校验，正在复核源目录",
            journal,
        )?;
        self.stop_here(DirStage::Verified)?;
        if let Err(error) = self.check_stable(source, &snapshot) {
            let _ = self.delete_tree(&target);
            self.set_stage(
                &mut record,
                DirStage::Failed,
                &format!("源目录在复制期间发生变化，已删除目标副本：{error}"),
                journal,
            )?;
            return Err(error);
        }

        // 改名之前把状态先落盘：中途被结束时，恢复流程能看出“改名可能已经发生”。
        let parent_path = parent_of(source)?;
        let source_name = file_name_of(source)?;
        let backup_name = file_name_of(&backup)?.to_os_string();
        let parent = guard::open_anchor(&parent_path, &self.forbidden)?;

        let opened = guard::open_rel(
            &parent,
            std::slice::from_ref(&source_name.to_os_string()),
            OpenIntent::Inspect,
            &self.forbidden,
        )?;
        let expect = opened.expect();
        drop(opened);

        self.set_stage(
            &mut record,
            DirStage::SourceRenamed,
            "正在把源目录改名成备份",
            journal,
        )?;
        if let Err(error) = guard::rename_child(&parent, source_name, &backup_name, &expect) {
            // 改名没成功，源还在原名位置：删掉多余的目标副本，整次迁移等于没发生。
            let _ = self.delete_tree(&target);
            self.set_stage(
                &mut record,
                DirStage::Failed,
                &format!("源目录改名失败，已删除目标副本，源目录完好：{error}"),
                journal,
            )?;
            return Err(error.into());
        }
        self.stop_here(DirStage::SourceRenamed)?;

        // 从这里开始源目录已经不在原名位置，任何失败都要让用户看见。
        let outcome = self.create_and_validate(
            &mut record,
            &parent,
            source_name,
            &target,
            &snapshot,
            journal,
        );
        match outcome {
            Ok(()) => {
                self.set_stage(
                    &mut record,
                    DirStage::AwaitingUserTest,
                    "迁移已生效，请打开相关应用确认正常，确认之后再删除备份",
                    journal,
                )?;
                Ok(record)
            }
            Err(error) => {
                self.set_stage(
                    &mut record,
                    DirStage::NeedsAttention,
                    &format!("建链接或校验失败，请手工检查：{error}"),
                    journal,
                )?;
                Err(error)
            }
        }
    }

    /// 继续上次中断的迁移。
    ///
    /// 两种情况可以继续：
    /// - 链接已经建好（`NeedsAttention` 但原位置确实是 junction）：重新校验指向和内容即可；
    /// - 源已改名、链接还没建：把建链接和校验补完。
    ///
    /// 其他情况都要用户自己判断，自动继续有可能在不知道现场的情况下动到用户数据。
    pub fn resume(
        &self,
        id: &str,
        journal: &mut dyn JournalSink,
    ) -> Result<DirMigration, JunctionError> {
        // 继续会建链接并改写状态，先挡住未确认的备份。
        self.refuse_without_backup()?;

        let mut record = self.load_one(id)?;
        let source = record.source_path();
        let target = record.target_path();
        let backup = record.backup_path();

        // 迁回中途留下的临时目录还在，说明停在迁回这一步：这时“继续”只会在迁回现场上
        // 再建一次 junction，两份数据反而说不清。界面上已经这样挡，后端也要一致地拒绝。
        if record.return_temp_path().exists() {
            return Err(JunctionError::WrongStage {
                stage: record.stage.label(),
            });
        }

        // 链接已经建好：数据在校验那一刻就已经在目标里，重新确认一遍就能交给用户实测。
        if record.stage == DirStage::NeedsAttention
            && target.exists()
            && self.source_is_junction(&record)
        {
            self.verify_junction(&record)?;
            // 经链接读回校验过了，这条记录才允许删备份；不置这一位，之后永远删不掉备份。
            record.verified = true;
            self.set_stage(
                &mut record,
                DirStage::AwaitingUserTest,
                "已重新校验 junction 指向和内容，请打开相关应用确认正常，确认之后再删除备份",
                journal,
            )?;
            return Ok(record);
        }

        let resumable = record.stage == DirStage::NeedsAttention
            && backup.exists()
            && !source.exists()
            && target.exists();
        if !resumable {
            return Err(JunctionError::WrongStage {
                stage: record.stage.label(),
            });
        }

        let parent_path = parent_of(&source)?;
        let source_name = file_name_of(&source)?.to_os_string();
        let parent = guard::open_anchor(&parent_path, &self.forbidden)?;

        // 校验读的是目标里的文件，和复制时的内容对一次。
        let snapshot = self.snapshot(&target)?;
        self.create_and_validate(
            &mut record,
            &parent,
            &source_name,
            &target,
            &snapshot,
            journal,
        )?;

        self.set_stage(
            &mut record,
            DirStage::AwaitingUserTest,
            "迁移已继续完成，请打开相关应用确认正常，确认之后再删除备份",
            journal,
        )?;

        Ok(record)
    }

    /// 复核一个已经建好的 junction：指向记录的迁移目标、条目对应得上、抽查文件内容一致。
    ///
    /// 条目只要求“备份里有的，目标里也有同样的类型”；大小只在修改时间和备份一致时才比，
    /// 因为迁移之后用户可能已经在原路径改过文件，那些改动只存在于目标里。
    fn verify_junction(&self, record: &DirMigration) -> Result<(), JunctionError> {
        let source = record.source_path();
        let target = record.target_path();

        let parent = guard::open_anchor(&parent_of(&source)?, &self.forbidden)?;
        let linked = guard::read_junction_target(&parent, file_name_of(&source)?)?;
        if !same_path(&linked, &target) {
            return Err(JunctionError::Guard(GuardError::LinkMismatch(linked)));
        }

        let files = self.snapshot(&target)?;

        let backup = record.backup_path();
        if dirinfo::is_plain_dir(&backup) {
            let expected = self.snapshot(&backup)?;

            // 两个快照都按相对路径排好序，合并走一遍就能确认备份里的条目在目标里都在。
            let mut at = 0usize;
            for entry in &expected {
                while at < files.len() && files[at].rel < entry.rel {
                    at += 1;
                }
                let found = files.get(at);
                if found.map(|item| &item.rel) != Some(&entry.rel)
                    || found.map(|item| item.is_dir) != Some(entry.is_dir)
                {
                    return Err(JunctionError::Verify(target.clone()));
                }

                // 修改时间还是迁移时那个，说明这个文件没被改过，大小也应当一样；
                // 时间一样而大小不同只可能是内容被截断或替换过。
                if let Some(found) = found
                    && !entry.is_dir
                    && found.mtime == entry.mtime
                    && found.size != entry.size
                {
                    return Err(JunctionError::Verify(target.clone()));
                }
            }

            // 抽查前几个文件的内容：比的是备份里的文件和目标里的文件，两个不同的物理文件。
            // 经 junction 读回读到的就是目标里的那一份，拿它和目标比什么也证明不了。
            for entry in expected
                .iter()
                .filter(|entry| !entry.is_dir)
                .take(VALIDATE_SAMPLE)
            {
                let copy = join_parts(&backup, &entry.rel);
                let live = join_parts(&target, &entry.rel);

                let copy_hash =
                    hash_hex(&copy).map_err(|error| JunctionError::Copy(error.to_string()))?;
                let live_hash =
                    hash_hex(&live).map_err(|error| JunctionError::Copy(error.to_string()))?;
                if copy_hash != live_hash {
                    return Err(JunctionError::Verify(live));
                }
            }
        }

        Ok(())
    }

    /// 生成清理备份目录的冻结清单。备份的删除由用户在预览里确认后交给执行器。
    ///
    /// 只在“链接确实存在”或“已经迁回”时允许：`NeedsAttention` 可能停在建链接失败的那一刻，
    /// 这时源目录还在备份名下，备份是用户唯一的一份数据，绝不能当作可清理的旧副本。
    /// 链接存在还要求这条记录经过 `Validated`：没有经链接读回校验过的迁移不算成功。
    pub fn backup_plan(&self, id: &str) -> Result<FrozenPlan, JunctionError> {
        let record = self.load_one(id)?;
        let backup = record.backup_path();

        let allowed = record.stage == DirStage::Returned
            || (record.stage.has_junction() && record.verified && self.source_is_junction(&record));
        if !allowed {
            return Err(JunctionError::WrongStage {
                stage: record.stage.label(),
            });
        }
        if !dirinfo::is_plain_dir(&backup) {
            return Err(JunctionError::Gone(backup));
        }

        let plan = build_plan_at(
            BACKUP_RULE_ID,
            Recovery::IrreversibleFile,
            std::slice::from_ref(&backup),
            &self.forbidden,
            &running_processes(),
            SystemTime::now(),
        );

        // 备份是空目录时清单里没有条目，`finish_backup` 会直接把空目录删掉。
        Ok(plan)
    }

    /// 清单执行完之后收尾：备份目录此时应当是空的，删掉它本身，状态推进到“完成”。
    ///
    /// 冻结清单只删目录里的内容，锚点目录会留成空目录；迁移要的是“备份目录消失”，
    /// 所以空的备份目录在这里单独删掉。目录里还有内容时说明清单没跑完或者有人又写进去了，
    /// 这时什么都不删。
    pub fn finish_backup(
        &self,
        id: &str,
        journal: &mut dyn JournalSink,
    ) -> Result<DirMigration, JunctionError> {
        // 这里会先删掉备份目录、再写记录：未确认的备份必须在动手之前挡住，
        // 否则用户会看到“备份没了、状态还没变”。
        self.refuse_without_backup()?;

        let mut record = self.load_one(id)?;
        let backup = record.backup_path();
        // 先把“迁回过”记下来：下面会把状态改成 BackupDeleted，改完就判断不出来了。
        let returned = record.stage == DirStage::Returned;
        let mut removed = false;

        if dirinfo::is_plain_dir(&backup) {
            let anchor = guard::open_anchor(&backup, &self.forbidden)?;
            let left = guard::list_dir(&anchor, &[], &self.forbidden)?;
            drop(anchor);

            if !left.is_empty() {
                return Err(JunctionError::BackupNotEmpty(left.len()));
            }
            self.delete_tree(&backup)?;
            removed = true;
        }

        // 迁回之后目标副本是旧数据，也要删掉才算结束；这时必须停在 Returned，
        // 否则界面上的“删除旧副本”（要求 stage === "returned"）就再也点不到了。
        let target_gone = !record.target_path().exists();
        if returned && !target_gone {
            self.set_stage(
                &mut record,
                DirStage::Returned,
                "备份已经删除；目标里的旧副本确认之后再删除",
                journal,
            )?;
            return Ok(record);
        }

        // 先记下“备份已删除”再推进到“完成”：步骤条和日志要能看出这一步真的走过了。
        if removed {
            self.set_stage(
                &mut record,
                DirStage::BackupDeleted,
                "备份目录已经删除",
                journal,
            )?;
        }

        self.set_stage(
            &mut record,
            DirStage::Done,
            "备份已经删除，迁移完成",
            journal,
        )?;
        Ok(record)
    }

    /// 迁回：以目标里的最新数据为准，把目录搬回原来的位置。
    pub fn return_home(
        &self,
        id: &str,
        journal: &mut dyn JournalSink,
    ) -> Result<DirMigration, JunctionError> {
        // 迁回会复制整棵目录并删链接，先挡住未确认的备份。
        self.refuse_without_backup()?;

        let mut record = self.load_one(id)?;
        let source = record.source_path();
        let target = record.target_path();

        // 状态之外还要确认源位置现在真的是链接：迁回过的记录再点一次迁回，
        // 既没有可删的链接，也不该在来源盘上再复制出一份多余的数据。
        if !record.stage.has_junction() || !self.source_is_junction(&record) {
            return Err(JunctionError::WrongStage {
                stage: record.stage.label(),
            });
        }
        if !target.exists() {
            return Err(JunctionError::Gone(target));
        }

        // 1. 来源盘要放得下目标里的数据，再留 2 GiB 余量。
        //
        // 这里必须查来源盘上的普通父目录，不能查 `source` 本身：它是指向目标盘的 junction，
        // `GetDiskFreeSpaceExW` 会跟着链接走，得到的是目标盘的剩余空间。
        let needed = measure_paths(std::slice::from_ref(&target))
            .logical
            .saturating_add(RESERVE_BYTES);
        let volume_anchor = parent_of(&source)?;
        if (self.free_space)(&volume_anchor) < needed {
            return Err(JunctionError::NoSpace { needed });
        }

        // 2. 复制之前先确认目标没被占用，并记下它的内容。
        let before = self.snapshot(&target)?;
        let returning = return_path(&source, &record.id);
        // 迁回前的阶段要留着：复制失败或复核不通过时状态要回到这里。
        let previous = record.stage;

        self.set_stage(
            &mut record,
            DirStage::Returning,
            "正在把目标目录复制回来源盘",
            journal,
        )?;
        self.stop_here(DirStage::Returning)?;

        if let Err(error) = self.check_stable(&target, &before) {
            return self.abort_return(
                &mut record,
                previous,
                &returning,
                JunctionError::ReturnRefused(error.to_string()),
                journal,
            );
        }

        // 3. 复制到来源卷上的临时目录，逐个文件校验。
        if let Err(error) = self.copy_into(&target, &returning) {
            return self.abort_return(&mut record, previous, &returning, error, journal);
        }

        // 复制期间有人往目标里写，迁回的结果就不是“目标里的最新数据”，宁可整次作废。
        if let Err(error) = self.check_stable(&target, &before) {
            return self.abort_return(
                &mut record,
                previous,
                &returning,
                JunctionError::ReturnRefused(error.to_string()),
                journal,
            );
        }

        // 4. 删链接本身。目标对不上说明这个链接不是 CClear 建的，宁可不删。
        //
        // 从这里开始状态已经是“正在迁回”，任何失败都要经 abort_return 收尾：直接用 ?
        // 返回会把记录停在 Returning 上，界面既不能继续也不能迁回。
        let opened = match self.open_return_parts(&source, &returning) {
            Ok(parts) => parts,
            Err(error) => {
                return self.abort_return(&mut record, previous, &returning, error, journal);
            }
        };
        let (parent, source_name, returning_name, expect) = opened;

        if let Err(error) = guard::remove_junction(&parent, &source_name, &target) {
            return self.abort_return(&mut record, previous, &returning, error.into(), journal);
        }

        // 5. 把迁回的目录改名回原名。改名失败时不能让原位置空着。
        if let Err(error) = guard::rename_child(&parent, &returning_name, &source_name, &expect) {
            return self.handle_rename_failure(&mut record, previous, error, journal);
        }

        self.set_stage(
            &mut record,
            DirStage::Returned,
            "已经迁回，原位置恢复成普通目录；备份和目标里的旧副本确认之后删除",
            journal,
        )?;
        Ok(record)
    }

    /// 迁回失败时的收尾：删掉临时副本，状态回到迁回之前，再把原来的错误报给界面。
    ///
    /// 目标里的数据一直没动过，临时目录只是多出来的一份，留着只会占来源盘的空间。
    fn abort_return(
        &self,
        record: &mut DirMigration,
        previous: DirStage,
        returning: &Path,
        error: JunctionError,
        journal: &mut dyn JournalSink,
    ) -> Result<DirMigration, JunctionError> {
        // 删不掉也要把真正的失败原因报出去，不能让它被清理错误盖掉；
        // 说明里必须如实写清临时目录还在，并把完整路径给出来，用户才知道去哪清理。
        let note = match self.delete_tree(returning) {
            Ok(()) => format!("迁回没有完成，已删除临时目录，数据没有变化：{error}"),
            Err(cleanup) => format!(
                "迁回没有完成，临时目录 {} 没能删除，数据没有变化，请手工清理：{error}；{cleanup}",
                returning.display()
            ),
        };

        self.set_stage(record, previous, &note, journal)?;

        Err(error)
    }

    /// 打开迁回需要的父目录、原位置名字和临时目录条目。失败时由调用方走 [`Self::abort_return`]。
    fn open_return_parts(
        &self,
        source: &Path,
        returning: &Path,
    ) -> Result<(DirHandle, OsString, OsString, guard::Expect), JunctionError> {
        let parent = guard::open_anchor(&parent_of(source)?, &self.forbidden)?;
        let source_name = file_name_of(source)?.to_os_string();
        let returning_name = file_name_of(returning)?.to_os_string();

        let opened = guard::open_rel(
            &parent,
            std::slice::from_ref(&returning_name),
            OpenIntent::Inspect,
            &self.forbidden,
        )?;
        let expect = opened.expect();
        drop(opened);

        Ok((parent, source_name, returning_name, expect))
    }

    /// 链接已经删掉、目录改名却失败：先把 junction 建回去，建不回来就把临时目录留给用户。
    ///
    /// 原位置空着会让用户以为数据丢了，所以重建链接是这里的首选；
    /// 重建也失败时保留临时目录，并在说明里写清它的完整路径，界面上还要看得到。
    fn handle_rename_failure(
        &self,
        record: &mut DirMigration,
        previous: DirStage,
        error: GuardError,
        journal: &mut dyn JournalSink,
    ) -> Result<DirMigration, JunctionError> {
        let source = record.source_path();
        let target = record.target_path();
        let returning = return_path(&source, &record.id);

        // 链接已经删掉，这一步再打不开父目录就没法重建 junction：同样交给 abort_return
        // 收尾，状态落在 NeedsAttention，界面上的“继续”还能把链接补回来。
        let opened = parent_of(&source).and_then(|path| {
            let name = file_name_of(&source)?.to_os_string();
            let parent = guard::open_anchor(&path, &self.forbidden)?;
            Ok((parent, name))
        });
        let (parent, source_name) = match opened {
            Ok(parts) => parts,
            Err(error) => {
                return self.abort_return(
                    record,
                    DirStage::NeedsAttention,
                    &returning,
                    error,
                    journal,
                );
            }
        };

        match guard::create_junction(&parent, &source_name, &target) {
            Ok(()) => {
                let _ = self.delete_tree(&returning);
                self.set_stage(
                    record,
                    previous,
                    &format!("迁回时目录改名失败，已重建 junction，数据没有变化：{error}"),
                    journal,
                )?;
            }
            Err(rebuild) => {
                self.set_stage(
                    record,
                    DirStage::NeedsAttention,
                    &format!(
                        "迁回时目录改名失败，重建 junction 也失败，临时目录保留在 {}，请手工处理：{error}；{rebuild}",
                        returning.display()
                    ),
                    journal,
                )?;
            }
        }

        Err(JunctionError::Guard(error))
    }

    /// 迁回之后删除目标里的旧副本。必须由用户确认之后调用。
    pub fn discard_target(
        &self,
        id: &str,
        journal: &mut dyn JournalSink,
    ) -> Result<DirMigration, JunctionError> {
        // 会删掉目标里的旧副本，必须在动手之前挡住未确认的备份。
        self.refuse_without_backup()?;

        let mut record = self.load_one(id)?;
        if record.stage != DirStage::Returned {
            return Err(JunctionError::WrongStage {
                stage: record.stage.label(),
            });
        }

        self.delete_tree(&record.target_path())?;

        // 备份还在时要停在“已迁回”，让界面继续提供“确认删除备份”。
        if record.backup_path().exists() {
            self.set_stage(
                &mut record,
                DirStage::Returned,
                "目标里的旧副本已经删除；备份确认之后再删除",
                journal,
            )?;
            return Ok(record);
        }

        self.set_stage(
            &mut record,
            DirStage::Done,
            "旧数据都已删除，迁回完成",
            journal,
        )?;
        Ok(record)
    }

    /// 启动时调用：把上次没有做完的目录迁移收到一个确定的状态上。
    ///
    /// 一条记录恢复失败只影响它自己：把它记成 `NeedsAttention` 并继续处理下一条，
    /// 不能让一条坏记录挡住其他记录的恢复。连状态都写不进去时把错误放进报告，
    /// 由界面显示，而不是悄悄跳过。
    pub fn recover_pending(
        &self,
        journal: &mut dyn JournalSink,
    ) -> Result<RecoverReport, JunctionError> {
        let mut report = RecoverReport::default();

        for record in self.load_all()? {
            let id = record.id.clone();
            match self.recover_one(record, journal) {
                Ok(Some(record)) => report.recovered.push(record),
                Ok(None) => {}
                Err(error) => report.failures.push(format!("记录 {id} 恢复失败：{error}")),
            }
        }

        Ok(report)
    }

    /// 恢复一条记录。返回 `None` 表示这条记录不需要恢复。
    fn recover_one(
        &self,
        mut record: DirMigration,
        journal: &mut dyn JournalSink,
    ) -> Result<Option<DirMigration>, JunctionError> {
        let source = record.source_path();
        let target = record.target_path();
        let backup = record.backup_path();

        match record.stage {
            // 复制没走完（含复制完成后复核前中断）：源一个都没动，删掉半成品副本就回到了起点。
            DirStage::Precheck | DirStage::Copying | DirStage::Verified => {
                if let Err(error) = self.delete_tree(&target) {
                    return self.mark_attention(
                        record,
                        format!("上次迁移在复制阶段被中断，目标副本没能删除，请手工检查：{error}"),
                        journal,
                    );
                }
                self.set_stage(
                    &mut record,
                    DirStage::Failed,
                    "上次迁移在复制阶段被中断，已删除目标副本，源目录完好，可以重新迁移",
                    journal,
                )?;
            }
            // 源已经改名但链接还没建：把名字改回去，再删掉目标副本。
            DirStage::SourceRenamed => {
                // 建链接时先创建一个空目录再写 reparse 数据，中途崩溃会留下一个空目录。
                // 只删这种空目录；里面有东西说明不是 CClear 建的，交给用户判断。
                if backup.exists() && dirinfo::is_plain_dir(&source) {
                    match self.remove_empty_dir(&source) {
                        Ok(true) => {}
                        Ok(false) => {
                            return self.mark_attention(
                                record,
                                "上次迁移停在改名之后，原位置出现了一个非空目录，请手工检查"
                                    .to_string(),
                                journal,
                            );
                        }
                        Err(error) => {
                            return self.mark_attention(
                                record,
                                format!("上次迁移停在改名之后，原位置的空目录没能删除，请手工检查：{error}"),
                                journal,
                            );
                        }
                    }
                }

                if let Err(error) = self.undo_rename(&source, &backup) {
                    return self.mark_attention(
                        record,
                        format!("上次迁移停在改名之后，源目录没能改回原名，请手工检查：{error}"),
                        journal,
                    );
                }

                if let Err(error) = self.delete_tree(&target) {
                    return self.mark_attention(
                        record,
                        format!("上次迁移在改名之后被中断，源目录已经改回原名，但目标副本没能删除：{error}"),
                        journal,
                    );
                }

                self.set_stage(
                    &mut record,
                    DirStage::Failed,
                    "上次迁移在改名之后被中断，已把源目录改回原名并删除目标副本",
                    journal,
                )?;
            }
            // 迁回中途被中断：链接还在就删临时目录，链接已经没了就把临时目录改名回去。
            DirStage::Returning => return self.recover_returning(record, journal),
            // 链接可能已经建好：继续还是迁回都要用户决定，不能自己动。
            DirStage::JunctionCreated | DirStage::Validated | DirStage::BackupDeleted => {
                self.set_stage(
                    &mut record,
                    DirStage::NeedsAttention,
                    "上次迁移被中断，请在目录迁移里选择继续或迁回",
                    journal,
                )?;
            }
            // AwaitingUserTest 本身就是稳定的等待状态：它等的是用户打开应用实测，
            // 改成 NeedsAttention 只会让“确认删除备份”前面多一道没意义的人工处理。
            // Returning 由上面单独处理，其余状态不需要恢复。
            _ => return Ok(None),
        }

        Ok(Some(record))
    }

    /// 恢复一次中断的迁回。返回 `None` 表示已经处理完，不需要再报给界面。
    fn recover_returning(
        &self,
        mut record: DirMigration,
        journal: &mut dyn JournalSink,
    ) -> Result<Option<DirMigration>, JunctionError> {
        let source = record.source_path();
        let target = record.target_path();
        let backup = record.backup_path();
        let returning = return_path(&source, &record.id);

        // 原位置还是 junction：复制没走完，删掉临时目录就回到迁回之前。
        if self.source_is_junction(&record) {
            // 临时目录删不掉时要如实写进说明，不能报告成“已删除临时目录”。
            let note = match self.delete_tree(&returning) {
                Ok(()) => {
                    "上次迁回在复制阶段被中断，已删除临时目录，链接和数据都没有变化".to_string()
                }
                Err(error) => format!(
                    "上次迁回在复制阶段被中断，临时目录 {} 没能删除，链接和数据没有变化，请手工清理：{error}",
                    returning.display()
                ),
            };
            let stage = if backup.exists() {
                DirStage::AwaitingUserTest
            } else {
                DirStage::Done
            };
            self.set_stage(&mut record, stage, &note, journal)?;
            return Ok(Some(record));
        }

        // 原位置已经空出来：临时目录完整就把名字改回去，不完整就留给用户处理。
        if !source.exists() && dirinfo::is_plain_dir(&returning) && target.exists() {
            let complete = match (self.snapshot(&returning), self.snapshot(&target)) {
                (Ok(copied), Ok(live)) => trees_match(&copied, &live),
                _ => false,
            };

            if complete {
                let renamed = self.rename_back(&returning, &source);
                match renamed {
                    Ok(()) => {
                        self.set_stage(
                            &mut record,
                            DirStage::Returned,
                            "上次迁回在改名之前被中断，已把迁回的目录改名回原名",
                            journal,
                        )?;
                        return Ok(Some(record));
                    }
                    Err(error) => {
                        return self.mark_attention(
                            record,
                            format!(
                                "上次迁回停在改名之前，临时目录没能改名回原名，请手工检查：{error}"
                            ),
                            journal,
                        );
                    }
                }
            }
        }

        self.mark_attention(
            record,
            format!(
                "上次迁回被中断，现场需要手工确认：原位置{}，目标目录{}，临时目录{}",
                if source.exists() {
                    "存在"
                } else {
                    "不存在"
                },
                if target.exists() {
                    "存在"
                } else {
                    "不存在"
                },
                if returning.exists() {
                    "还存在"
                } else {
                    "不存在"
                }
            ),
            journal,
        )
    }

    /// 把记录记成 `NeedsAttention` 并带上现场说明。
    fn mark_attention(
        &self,
        mut record: DirMigration,
        note: String,
        journal: &mut dyn JournalSink,
    ) -> Result<Option<DirMigration>, JunctionError> {
        self.set_stage(&mut record, DirStage::NeedsAttention, &note, journal)?;
        Ok(Some(record))
    }

    /// 把 `from` 改名成 `to`，两者在同一个父目录里。
    fn rename_back(&self, from: &Path, to: &Path) -> Result<(), JunctionError> {
        let parent_path = parent_of(from)?;
        let from_name = file_name_of(from)?;
        let to_name = file_name_of(to)?;
        let parent = guard::open_anchor(&parent_path, &self.forbidden)?;

        let opened = guard::open_rel(
            &parent,
            std::slice::from_ref(&from_name.to_os_string()),
            OpenIntent::Inspect,
            &self.forbidden,
        )?;
        let expect = opened.expect();
        drop(opened);

        guard::rename_child(&parent, from_name, to_name, &expect)?;
        Ok(())
    }

    /// 只删除一个空目录。目录里还有内容时返回 `false`，绝不连带删掉里面的东西。
    fn remove_empty_dir(&self, path: &Path) -> Result<bool, JunctionError> {
        let parent = guard::open_anchor(&parent_of(path)?, &self.forbidden)?;
        let name = file_name_of(path)?;

        if !guard::list_dir(
            &parent,
            std::slice::from_ref(&name.to_os_string()),
            &self.forbidden,
        )?
        .is_empty()
        {
            return Ok(false);
        }

        guard::delete_tree(&parent, name, &self.forbidden)?;
        Ok(true)
    }

    // ---- 内部实现 ----

    /// 建 junction 并校验：链路建成之后必须能从原路径读回复制时的内容。
    fn create_and_validate(
        &self,
        record: &mut DirMigration,
        parent: &DirHandle,
        source_name: &std::ffi::OsStr,
        target: &Path,
        snapshot: &[SnapshotEntry],
        journal: &mut dyn JournalSink,
    ) -> Result<(), JunctionError> {
        guard::create_junction(parent, source_name, target)?;
        self.set_stage(
            record,
            DirStage::JunctionCreated,
            "原名位置已经建好 junction",
            journal,
        )?;

        let source = record.source_path();
        // 只抽查文件：目录没有内容可比，读哈希会直接失败。
        for entry in snapshot
            .iter()
            .filter(|entry| !entry.is_dir)
            .take(VALIDATE_SAMPLE)
        {
            let through_link = join_parts(&source, &entry.rel);
            let direct = join_parts(target, &entry.rel);

            let through_hash =
                hash_hex(&through_link).map_err(|error| JunctionError::Copy(error.to_string()))?;
            let direct_hash =
                hash_hex(&direct).map_err(|error| JunctionError::Copy(error.to_string()))?;
            if through_hash != direct_hash {
                return Err(JunctionError::Verify(through_link));
            }
        }

        // 这一条是按“复制时的内容”读回来的，之后只有它才允许删备份。
        record.verified = true;
        self.set_stage(
            record,
            DirStage::Validated,
            "经 junction 读回的内容和复制时一致",
            journal,
        )
    }

    /// 复制整棵目录并逐个文件校验。
    fn copy_tree(
        &self,
        source: &Path,
        target: &Path,
        snapshot: &[SnapshotEntry],
    ) -> Result<(), JunctionError> {
        std::fs::create_dir_all(target)?;
        if snapshot.is_empty() {
            return Ok(());
        }

        // 目录也交给复制流程：空目录只有这样才能在目标侧被建出来。
        let rels: Vec<Vec<OsString>> = snapshot.iter().map(|entry| entry.rel.clone()).collect();
        copy_tree_verified(source, &rels, target)
            .map_err(|error| JunctionError::Copy(error.to_string()))?;

        Ok(())
    }

    /// 把 `from` 的内容整棵复制到 `to` 并校验。目标不存在时先建出来。
    fn copy_into(&self, from: &Path, to: &Path) -> Result<(), JunctionError> {
        let anchor = guard::open_anchor(from, &self.forbidden)?;
        let mut entries = Vec::new();
        self.collect_entries(&anchor, &mut Vec::new(), &mut entries)?;

        self.copy_tree(from, to, &entries)
    }

    /// 复制之后复查：目录没变、没有进程在用、没有文件被占用。
    fn check_stable(&self, source: &Path, snapshot: &[SnapshotEntry]) -> Result<(), JunctionError> {
        let now = self.snapshot(source)?;
        if !same_tree(snapshot, &now) {
            return Err(JunctionError::Verify(source.to_path_buf()));
        }

        self.check_in_use(source, snapshot)
    }

    /// 占用检查：映像在目录里的进程、Restart Manager 报告占用目录里文件的程序。
    fn check_in_use(&self, source: &Path, snapshot: &[SnapshotEntry]) -> Result<(), JunctionError> {
        // 映像在源目录里的进程：改名之后它们会找不到自己的文件。
        let running: Vec<String> = running_processes()
            .into_iter()
            .filter(|proc| {
                proc.image_path
                    .as_ref()
                    .is_some_and(|image| is_under(image, source))
            })
            .map(|proc| proc.name)
            .collect();
        if !running.is_empty() {
            return Err(JunctionError::RunningProcesses(running.join("、")));
        }

        // 前 256 个文件的占用检测。查不完整时按“不能确认”处理，绝不当成没有占用。
        // 目录本身不探测：Restart Manager 只对文件有意义。
        let probe: Vec<PathBuf> = snapshot
            .iter()
            .filter(|entry| !entry.is_dir)
            .take(LOCK_CHECK_LIMIT)
            .map(|entry| join_parts(source, &entry.rel))
            .collect();
        let report = restart_manager::query_lockers(&probe);
        if !report.complete {
            return Err(JunctionError::NotVerified(
                report
                    .error
                    .unwrap_or_else(|| "占用检测没有覆盖全部探测文件".to_string()),
            ));
        }
        if !report.lockers.is_empty() {
            let names: Vec<String> = report
                .lockers
                .iter()
                .map(|locker| match &locker.service {
                    Some(service) => format!("{}（{service}）", locker.app_name),
                    None => locker.app_name.clone(),
                })
                .collect();
            return Err(JunctionError::RunningProcesses(names.join("、")));
        }

        Ok(())
    }

    /// 位置检查：必须在用户目录之下，不能是 Known Folder 的根，也不能落在禁区。
    fn check_location(&self, source: &Path, reasons: &mut Vec<String>) {
        let Some(profile) = self.roots.get(KnownRoot::Profile) else {
            reasons.push("无法解析用户目录，不能判断源路径".to_string());
            return;
        };

        if !is_under(source, profile) {
            reasons.push(format!("只能迁移用户目录（{}）下的目录", profile.display()));
            return;
        }

        if same_path(source, profile) {
            reasons.push("不能迁移用户目录本身".to_string());
        }
        // AppData 本身不在 KnownRoot 里，它是用户目录下的固定子目录。
        if same_path(source, &profile.join("AppData")) {
            reasons.push("不能迁移 AppData 目录本身".to_string());
        }

        for root in [
            KnownRoot::LocalAppData,
            KnownRoot::RoamingAppData,
            KnownRoot::Documents,
            KnownRoot::Downloads,
            KnownRoot::ProgramData,
            KnownRoot::Windows,
            KnownRoot::SystemDrive,
        ] {
            if let Some(path) = self.roots.get(root)
                && same_path(source, path)
            {
                reasons.push(format!("不能迁移系统目录本身：{}", path.display()));
            }
        }

        // 这两处由系统管理，改名会让相关功能直接失效。
        if let Some(local) = self.roots.get(KnownRoot::LocalAppData) {
            for name in ["Packages", "Microsoft"] {
                let banned = local.join(name);
                if same_path(source, &banned) || is_under(source, &banned) {
                    reasons.push(format!("{} 下的目录不能迁移", banned.display()));
                }
            }
        }

        // 用户目录里的已知文件夹都是资源管理器直接展示的位置，改名之后用户找不到自己的文件。
        // 这些位置可能被重定向到别的盘，所以按 SHGetKnownFolderPath 的解析结果比较。
        for (folder, label) in [
            (KnownFolder::Desktop, "桌面"),
            (KnownFolder::Pictures, "图片"),
            (KnownFolder::Music, "音乐"),
            (KnownFolder::Videos, "视频"),
            (KnownFolder::SavedGames, "保存的游戏"),
            (KnownFolder::LocalAppDataLow, "AppData\\LocalLow"),
        ] {
            if let Ok(path) = known_folder(folder)
                && same_path(source, &path)
            {
                reasons.push(format!("不能迁移{label}目录本身：{}", path.display()));
            }
        }

        // 用户目录下以 OneDrive 开头的目录都是同步根，其中包括 “OneDrive - 公司名”。
        if let Some(relative) = strip_profile_prefix(source, profile)
            && let Some(Component::Normal(name)) = relative.components().next()
            && starts_with_ignore_case(&name.to_string_lossy(), "OneDrive")
        {
            reasons.push("OneDrive 同步目录不能迁移".to_string());
        }

        if let Ok(onedrive) = known_folder(KnownFolder::SkyDrive)
            && (same_path(source, &onedrive) || is_under(source, &onedrive))
        {
            reasons.push("OneDrive 同步目录不能迁移".to_string());
        }

        // 护栏打不开的路径（含按身份记录的禁区）一律不迁移。
        if let Err(error) = guard::open_anchor(source, &self.forbidden) {
            reasons.push(error.to_string());
        }
    }

    /// 内容检查：不能有链接、程序文件、EFS 加密文件和云占位符。
    fn check_contents(&self, source: &Path, reasons: &mut Vec<String>) {
        let Ok(anchor) = guard::open_anchor(source, &self.forbidden) else {
            // 位置检查已经报过同一件事的原因，这里不重复。
            return;
        };

        self.inspect_dir(&anchor, &mut Vec::new(), reasons);
    }

    /// 递归检查一个目录。原因收集到上限就停，避免为一个几万文件的目录刷屏。
    fn inspect_dir(&self, anchor: &DirHandle, rel: &mut Vec<OsString>, reasons: &mut Vec<String>) {
        if reasons.len() >= PROBLEM_LIMIT {
            return;
        }

        let entries = match guard::list_dir(anchor, rel, &self.forbidden) {
            Ok(entries) => entries,
            Err(error) => {
                reasons.push(format!(
                    "{} 读不出来：{error}",
                    join_parts(anchor.path(), rel).display()
                ));
                return;
            }
        };

        for entry in entries {
            if reasons.len() >= PROBLEM_LIMIT {
                return;
            }

            rel.push(entry.name.clone());
            let path = join_parts(anchor.path(), rel);

            if entry.is_reparse() {
                reasons.push(format!(
                    "{} 是链接，目录里有链接不能整体迁移",
                    path.display()
                ));
            } else if entry.is_dir {
                self.inspect_dir(anchor, rel, reasons);
            } else if is_program_file(&entry.name) {
                reasons.push(format!("{} 是程序文件（.exe/.dll）", path.display()));
            } else if entry.attributes & FILE_ATTRIBUTE_READONLY.0 != 0 {
                reasons.push(format!(
                    "{} 是只读文件；目录里有只读文件，CClear 不修改只读属性，无法保证出错时能清理干净",
                    path.display()
                ));
            } else if entry.attributes & FILE_ATTRIBUTE_ENCRYPTED.0 != 0 {
                reasons.push(format!("{} 是 EFS 加密文件", path.display()));
            } else if is_cloud_placeholder(entry.attributes) {
                reasons.push(format!("{} 是云占位符，内容还没下载到本地", path.display()));
            }

            rel.pop();
        }
    }

    /// 取源目录里全部条目（文件与目录）的快照，按相对路径排序，方便两次快照逐项比对。
    fn snapshot(&self, source: &Path) -> Result<Vec<SnapshotEntry>, JunctionError> {
        let anchor = guard::open_anchor(source, &self.forbidden)?;
        let mut entries = Vec::new();
        self.collect_entries(&anchor, &mut Vec::new(), &mut entries)?;
        entries.sort_by(|left, right| left.rel.cmp(&right.rel));

        Ok(entries)
    }

    /// 递归收集条目（文件和目录）。遇到链接直接报错：资格检查已经拒绝了链接，
    /// 走到这里还有链接说明目录在检查之后被改过，不能继续。
    fn collect_entries(
        &self,
        anchor: &DirHandle,
        rel: &mut Vec<OsString>,
        out: &mut Vec<SnapshotEntry>,
    ) -> Result<(), JunctionError> {
        let volume_serial = anchor.identity().volume_serial;

        for entry in guard::list_dir(anchor, rel, &self.forbidden)? {
            if entry.is_reparse() {
                return Err(JunctionError::Guard(GuardError::ReparseInPath(
                    join_parts(anchor.path(), rel).join(&entry.name),
                )));
            }

            rel.push(entry.name.clone());
            out.push(SnapshotEntry {
                rel: rel.clone(),
                is_dir: entry.is_dir,
                identity: FileIdentity {
                    volume_serial,
                    file_id: entry.file_id,
                },
                // 目录没有有意义的大小，统一记 0，比较时不会随子项数量跳动。
                size: if entry.is_dir { 0 } else { entry.logical },
                mtime: entry.last_write_time,
            });

            if entry.is_dir {
                self.collect_entries(anchor, rel, out)?;
            }
            rel.pop();
        }

        Ok(())
    }

    /// 把源目录从备份名改回原名。名字已经在原名位置时什么也不做。
    fn undo_rename(&self, source: &Path, backup: &Path) -> Result<(), JunctionError> {
        if source.exists() && !backup.exists() {
            return Ok(());
        }
        if !backup.exists() {
            // 两边都不在：没法自动恢复，交给用户。
            return Err(JunctionError::Gone(source.to_path_buf()));
        }
        if source.exists() {
            // 两边都在：可能是迁回留下的目录，自动改名会覆盖，必须人工判断。
            return Err(JunctionError::TargetExists(source.to_path_buf()));
        }

        let parent_path = parent_of(backup)?;
        let backup_name = file_name_of(backup)?;
        let source_name = file_name_of(source)?;
        let parent = guard::open_anchor(&parent_path, &self.forbidden)?;

        let opened = guard::open_rel(
            &parent,
            std::slice::from_ref(&backup_name.to_os_string()),
            OpenIntent::Inspect,
            &self.forbidden,
        )?;
        let expect = opened.expect();
        drop(opened);

        guard::rename_child(&parent, backup_name, source_name, &expect)?;
        Ok(())
    }

    /// 目标根目录：线上按选中的卷算，测试用注入的目录。
    fn target_root(&self, source: &Path, volume_root: &str) -> Result<PathBuf, JunctionError> {
        if let Some(root) = &self.target_root {
            return Ok(root.clone());
        }

        let volume = candidate_volumes()
            .into_iter()
            .find(|info| equals_ignore_case(&info.root.to_string_lossy(), volume_root))
            .ok_or_else(|| JunctionError::BadVolume(volume_root.to_string()))?;

        // 源自己就在目标卷上时，迁移既腾不出空间也没有意义。
        if !self.allow_same_volume
            && volume::path_volume_serial(source) == volume::path_volume_serial(&volume.root)
        {
            return Err(JunctionError::BadVolume(volume_root.to_string()));
        }

        Ok(volume.root.join(TARGET_DIR_NAME))
    }

    /// 生成一个没被用过的 8 位十六进制 ID。
    fn new_id(&self) -> Result<String, JunctionError> {
        let existing = self.load_all()?;

        for _ in 0..8 {
            let candidate = Uuid::new_v4().simple().to_string();
            let candidate = candidate[..8].to_string();
            if !existing
                .iter()
                .any(|record| equals_ignore_case(&record.id, &candidate))
            {
                return Ok(candidate);
            }
        }

        Err(JunctionError::Io(io::Error::other("生成迁移 ID 失败")))
    }

    /// 删掉一棵完全由 CClear 自己创建的目录树。目录不存在时什么也不做。
    fn delete_tree(&self, path: &Path) -> Result<(), JunctionError> {
        if !path.exists() {
            return Ok(());
        }

        let parent = parent_of(path)?;
        let name = file_name_of(path)?;
        let anchor = guard::open_anchor(&parent, &self.forbidden)?;
        guard::delete_tree(&anchor, name, &self.forbidden)?;

        Ok(())
    }

    /// 测试注入的断点。走到指定阶段之后直接报错，模拟进程被强行结束。
    fn stop_here(&self, stage: DirStage) -> Result<(), JunctionError> {
        if self.stop_at == Some(stage) {
            return Err(JunctionError::Interrupted);
        }

        Ok(())
    }

    /// 阶段推进：改记录、落盘，再写日志。
    fn set_stage(
        &self,
        record: &mut DirMigration,
        stage: DirStage,
        note: &str,
        journal: &mut dyn JournalSink,
    ) -> Result<(), JunctionError> {
        record.stage = stage;
        record.note = note.to_string();
        record.updated_at = unix_now();
        self.save_one(record)?;

        journal
            .append(&Record::JunctionStage {
                at: record.updated_at,
                id: record.id.clone(),
                source: record.source.clone(),
                stage: stage.label().to_string(),
                detail: record.note.clone(),
            })
            .map_err(JunctionError::Log)
    }

    /// 状态文件所在目录。
    fn store_dir(&self) -> Result<PathBuf, JunctionError> {
        if let Some(dir) = &self.store_dir {
            return Ok(dir.clone());
        }

        known_folder(KnownFolder::LocalAppData)
            .map(|path| path.join("CClear"))
            .map_err(|error| {
                JunctionError::Io(io::Error::other(format!(
                    "无法定位本地应用数据目录：{error}"
                )))
            })
    }

    fn load_all(&self) -> Result<Vec<DirMigration>, JunctionError> {
        let dir = self.store_dir()?;
        let store =
            load_store(Some(&dir)).map_err(|error| JunctionError::Store(error.to_string()))?;

        Ok(store.directories)
    }

    fn load_one(&self, id: &str) -> Result<DirMigration, JunctionError> {
        self.load_all()?
            .into_iter()
            .find(|record| equals_ignore_case(&record.id, id))
            .ok_or(JunctionError::Missing)
    }

    /// 保存一条记录，其他记录原样保留。
    fn save_one(&self, record: &DirMigration) -> Result<(), JunctionError> {
        let dir = self.store_dir()?;
        let mut store =
            load_store(Some(&dir)).map_err(|error| JunctionError::Store(error.to_string()))?;
        store
            .directories
            .retain(|item| !equals_ignore_case(&item.id, &record.id));
        store.directories.push(record.clone());

        save_store(Some(&dir), &store).map_err(|error| JunctionError::Store(error.to_string()))?;
        Ok(())
    }
}

/// 备份目录的名字：`<name>.cclear-bak-<id>`，和源目录同级。
fn backup_path(source: &Path, id: &str) -> PathBuf {
    source.with_file_name(format!("{}.cclear-bak-{id}", file_name_text(source)))
}

/// 迁回时用的临时目录名字：`<name>.cclear-return-<id>`，和源目录同级。
fn return_path(source: &Path, id: &str) -> PathBuf {
    source.with_file_name(format!("{}.cclear-return-{id}", file_name_text(source)))
}

/// 目录名，取不到时用固定的兜底名。
fn file_name_text(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cclear".to_string())
}

fn parent_of(path: &Path) -> Result<PathBuf, JunctionError> {
    path.parent().map(Path::to_path_buf).ok_or_else(|| {
        JunctionError::Io(io::Error::other(format!(
            "路径没有父目录：{}",
            path.display()
        )))
    })
}

fn file_name_of(path: &Path) -> Result<&std::ffi::OsStr, JunctionError> {
    path.file_name().ok_or_else(|| {
        JunctionError::Io(io::Error::other(format!(
            "路径没有名字：{}",
            path.display()
        )))
    })
}

/// 拼出目录下的完整路径，只用于复制、校验和展示。
fn join_parts(base: &Path, rel: &[OsString]) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in rel {
        path.push(part);
    }

    path
}

/// 同一个路径的两次快照是否一致。文件身份也一起比：同名替换过的文件必须能看出来。
fn same_tree(before: &[SnapshotEntry], after: &[SnapshotEntry]) -> bool {
    before.len() == after.len()
        && before.iter().zip(after.iter()).all(|(left, right)| {
            left.rel == right.rel
                && left.is_dir == right.is_dir
                && left.identity == right.identity
                && left.size == right.size
                && left.mtime == right.mtime
        })
}

/// 两棵目录树的内容是否一致。这里不看文件身份：两边本来就是不同的副本。
///
/// 目录只比相对路径和类型：复制流程创建目录时不会复制目录的时间戳，
/// 子目录的修改时间在两边必然不同，比时间会把每一次迁回都判成“不完整”。
fn trees_match(left: &[SnapshotEntry], right: &[SnapshotEntry]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right.iter()).all(|(one, other)| {
            one.rel == other.rel
                && one.is_dir == other.is_dir
                && (one.is_dir || (one.size == other.size && one.mtime == other.mtime))
        })
}

/// 两个路径是否相同。按组件逐个比较，不区分大小写；末尾多一个反斜杠也算同一个路径。
fn same_path(left: &Path, right: &Path) -> bool {
    let mut left = left.components();
    let mut right = right.components();

    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(one), Some(other)) if component_equals(one, other) => {}
            _ => return false,
        }
    }
}

/// 单个路径组件是否相同，比较走 `CompareStringOrdinal`。
fn component_equals(left: Component<'_>, right: Component<'_>) -> bool {
    match (left, right) {
        (Component::Prefix(one), Component::Prefix(other)) => equals_ignore_case(
            &one.as_os_str().to_string_lossy(),
            &other.as_os_str().to_string_lossy(),
        ),
        (Component::Normal(one), Component::Normal(other)) => {
            equals_ignore_case(&one.to_string_lossy(), &other.to_string_lossy())
        }
        (Component::RootDir, Component::RootDir) => true,
        (Component::CurDir, Component::CurDir) => true,
        (Component::ParentDir, Component::ParentDir) => true,
        _ => false,
    }
}

/// `text` 是否以 `prefix` 开头（不区分大小写）。
fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    let mut actual = text.chars();
    prefix.chars().all(|expected| {
        actual
            .next()
            .is_some_and(|ch| char_equals_ignore_case(ch, expected))
    })
}

/// `path` 是否在 `base` 之下（不含 base 自己）。逐组件比较，不区分大小写。
fn is_under(path: &Path, base: &Path) -> bool {
    let mut parts = path.components();

    for expected in base.components() {
        match parts.next() {
            Some(actual) if component_equals(actual, expected) => {}
            _ => return false,
        }
    }

    parts.next().is_some()
}

/// 按组件不区分大小写地剥掉 `profile` 前缀，返回剩下的相对路径。
///
/// 不能用 `Path::strip_prefix`：它区分大小写，用户目录的大小写和扫描结果
/// 不一致时会漏判。
fn strip_profile_prefix(path: &Path, profile: &Path) -> Option<PathBuf> {
    let mut parts = path.components();
    for expected in profile.components() {
        match parts.next() {
            Some(actual) if component_equals(actual, expected) => {}
            _ => return None,
        }
    }

    let rest: PathBuf = parts.collect();
    if rest.as_os_str().is_empty() {
        None
    } else {
        Some(rest)
    }
}

/// 是否是可以直接运行的程序文件。迁移后原路径变成链接，系统加载器可能找不到它们。
fn is_program_file(name: &std::ffi::OsStr) -> bool {
    let Some(extension) = Path::new(name).extension().and_then(|ext| ext.to_str()) else {
        return false;
    };

    equals_ignore_case(extension, "exe") || equals_ignore_case(extension, "dll")
}

/// 是否是云占位符。这类文件的内容还没落到本地，复制过去只是一个空壳。
fn is_cloud_placeholder(attributes: u32) -> bool {
    attributes & FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0 != 0
        || attributes & FILE_ATTRIBUTE_RECALL_ON_OPEN.0 != 0
}
