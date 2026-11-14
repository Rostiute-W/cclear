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
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
};

use crate::guard::{self, DirHandle, Forbidden, GuardError, OpenIntent};
use crate::journal::{JournalSink, Record, unix_now};
use crate::migrate::{candidate_volumes, load_store, save_store};
use crate::model::{FileIdentity, KnownRoot, Recovery};
use crate::plan::{FrozenPlan, build_plan_at};
use crate::quarantine::{copy_tree_verified, hash_hex};
use crate::rules::resolve::{RootMap, measure_paths};
use crate::win::process::running_processes;
use crate::win::restart_manager;
use crate::win::{KnownFolder, dirinfo, equals_ignore_case, known_folder, volume};

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
            DirStage::NeedsAttention => "需要处理",
            DirStage::Returned => "已迁回",
            DirStage::Failed => "已回滚",
        }
    }

    /// 是否已经建过 junction。这些状态才谈得上迁回。
    ///
    /// `Returned` 和 `Failed` 都不算：迁回之后原位置已经是普通目录，链接已经被删掉了。
    /// `NeedsAttention` 可能停在“链接建好之后”，也可能停在“改名成功但建链接失败”，
    /// 所以这一项只能和磁盘上的实际情况一起判断（见 `JunctionMigration::source_is_junction`）。
    fn has_junction(self) -> bool {
        matches!(
            self,
            DirStage::JunctionCreated
                | DirStage::Validated
                | DirStage::AwaitingUserTest
                | DirStage::BackupDeleted
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
}

/// 源目录里一个文件的快照。复制前后各取一次，用来确认源在复制期间没有变。
struct FileSnapshot {
    rel: Vec<OsString>,
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

    /// 继续上次中断的迁移：源目录已经在备份名下时，把建链接和校验补完。
    ///
    /// 只处理“复制已经完成、源已改名、链接还没建”这一种情况；其他情况都要用户自己判断，
    /// 自动继续有可能在不知道现场的情况下动到用户数据。
    pub fn resume(
        &self,
        id: &str,
        journal: &mut dyn JournalSink,
    ) -> Result<DirMigration, JunctionError> {
        let mut record = self.load_one(id)?;
        let source = record.source_path();
        let target = record.target_path();
        let backup = record.backup_path();

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

    /// 生成清理备份目录的冻结清单。备份的删除由用户在预览里确认后交给执行器。
    ///
    /// 只在“链接确实存在”或“已经迁回”时允许：`NeedsAttention` 可能停在建链接失败的那一刻，
    /// 这时源目录还在备份名下，备份是用户唯一的一份数据，绝不能当作可清理的旧副本。
    pub fn backup_plan(&self, id: &str) -> Result<FrozenPlan, JunctionError> {
        let record = self.load_one(id)?;
        let backup = record.backup_path();

        let allowed = record.stage == DirStage::Returned
            || (record.stage.has_junction() && self.source_is_junction(&record));
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
        let needed = measure_paths(std::slice::from_ref(&target))
            .logical
            .saturating_add(RESERVE_BYTES);
        if (self.free_space)(&source) < needed {
            return Err(JunctionError::NoSpace { needed });
        }

        // 2. 复制到来源卷上的临时目录，逐个文件校验。
        let returning = return_path(&source, &record.id);
        self.copy_into(&target, &returning)?;

        // 3. 删链接本身。目标对不上说明这个链接不是 CClear 建的，宁可不删。
        let parent_path = parent_of(&source)?;
        let source_name = file_name_of(&source)?;
        let parent = guard::open_anchor(&parent_path, &self.forbidden)?;
        let returning_name = file_name_of(&returning)?;

        // 换名之前任何一步失败都要把刚复制出来的临时目录删掉：目标里的数据还在，
        // 这份临时副本只是多余的一份，留在来源盘上只会占空间。
        let swapped = (|| -> Result<(), JunctionError> {
            let opened = guard::open_rel(
                &parent,
                std::slice::from_ref(&returning_name.to_os_string()),
                OpenIntent::Inspect,
                &self.forbidden,
            )?;
            let expect = opened.expect();
            drop(opened);

            guard::remove_junction(&parent, source_name, &target)?;

            // 4. 把迁回的目录改名回原名。
            guard::rename_child(&parent, returning_name, source_name, &expect)?;
            Ok(())
        })();

        if let Err(error) = swapped {
            let _ = self.delete_tree(&returning);
            return Err(error);
        }

        self.set_stage(
            &mut record,
            DirStage::Returned,
            "已经迁回，原位置恢复成普通目录；备份和目标里的旧副本确认之后删除",
            journal,
        )?;
        Ok(record)
    }

    /// 迁回之后删除目标里的旧副本。必须由用户确认之后调用。
    pub fn discard_target(
        &self,
        id: &str,
        journal: &mut dyn JournalSink,
    ) -> Result<DirMigration, JunctionError> {
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
    pub fn recover_pending(
        &self,
        journal: &mut dyn JournalSink,
    ) -> Result<Vec<DirMigration>, JunctionError> {
        let mut recovered = Vec::new();

        for mut record in self.load_all()? {
            let source = record.source_path();
            let target = record.target_path();
            let backup = record.backup_path();

            match record.stage {
                // 复制还没走到改名：源一个都没动，删掉半成品副本就回到了起点。
                DirStage::Precheck | DirStage::Copying => {
                    self.delete_tree(&target)?;
                    self.set_stage(
                        &mut record,
                        DirStage::Failed,
                        "上次迁移在复制阶段被中断，已删除目标副本，源目录完好，可以重新迁移",
                        journal,
                    )?;
                }
                // 源已经改名但链接还没建：把名字改回去，再删掉目标副本。
                DirStage::SourceRenamed => {
                    if let Err(error) = self.undo_rename(&source, &backup) {
                        self.set_stage(
                            &mut record,
                            DirStage::NeedsAttention,
                            &format!(
                                "上次迁移停在改名之后，源目录没能改回原名，请手工检查：{error}"
                            ),
                            journal,
                        )?;
                        recovered.push(record);
                        continue;
                    }

                    self.delete_tree(&target)?;
                    self.set_stage(
                        &mut record,
                        DirStage::Failed,
                        "上次迁移在改名之后被中断，已把源目录改回原名并删除目标副本",
                        journal,
                    )?;
                }
                // 链接可能已经建好：继续还是迁回都要用户决定，不能自己动。
                DirStage::JunctionCreated
                | DirStage::Validated
                | DirStage::AwaitingUserTest
                | DirStage::BackupDeleted => {
                    self.set_stage(
                        &mut record,
                        DirStage::NeedsAttention,
                        "上次迁移被中断，请在目录迁移里选择继续或迁回",
                        journal,
                    )?;
                }
                _ => continue,
            }

            recovered.push(record);
        }

        Ok(recovered)
    }

    // ---- 内部实现 ----

    /// 建 junction 并校验：链路建成之后必须能从原路径读回复制时的内容。
    fn create_and_validate(
        &self,
        record: &mut DirMigration,
        parent: &DirHandle,
        source_name: &std::ffi::OsStr,
        target: &Path,
        snapshot: &[FileSnapshot],
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
        for file in snapshot.iter().take(VALIDATE_SAMPLE) {
            let through_link = join_parts(&source, &file.rel);
            let direct = join_parts(target, &file.rel);

            let through_hash =
                hash_hex(&through_link).map_err(|error| JunctionError::Copy(error.to_string()))?;
            let direct_hash =
                hash_hex(&direct).map_err(|error| JunctionError::Copy(error.to_string()))?;
            if through_hash != direct_hash {
                return Err(JunctionError::Verify(through_link));
            }
        }

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
        snapshot: &[FileSnapshot],
    ) -> Result<(), JunctionError> {
        std::fs::create_dir_all(target)?;
        if snapshot.is_empty() {
            return Ok(());
        }

        let rels: Vec<Vec<OsString>> = snapshot.iter().map(|file| file.rel.clone()).collect();
        copy_tree_verified(source, &rels, target)
            .map_err(|error| JunctionError::Copy(error.to_string()))?;

        Ok(())
    }

    /// 把 `from` 的内容整棵复制到 `to` 并校验。目标不存在时先建出来。
    fn copy_into(&self, from: &Path, to: &Path) -> Result<(), JunctionError> {
        let anchor = guard::open_anchor(from, &self.forbidden)?;
        let mut files = Vec::new();
        self.collect_files(&anchor, &mut Vec::new(), &mut files)?;

        self.copy_tree(from, to, &files)
    }

    /// 复制之后复查：源没变、没有进程在用、经链接读回的内容一致。
    fn check_stable(&self, source: &Path, snapshot: &[FileSnapshot]) -> Result<(), JunctionError> {
        let now = self.snapshot(source)?;
        let unchanged = now.len() == snapshot.len()
            && snapshot.iter().zip(now.iter()).all(|(before, after)| {
                before.rel == after.rel
                    && before.identity == after.identity
                    && before.size == after.size
                    && before.mtime == after.mtime
            });
        if !unchanged {
            return Err(JunctionError::Verify(source.to_path_buf()));
        }

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
        let probe: Vec<PathBuf> = snapshot
            .iter()
            .take(LOCK_CHECK_LIMIT)
            .map(|file| join_parts(source, &file.rel))
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
            } else if entry.attributes & FILE_ATTRIBUTE_ENCRYPTED.0 != 0 {
                reasons.push(format!("{} 是 EFS 加密文件", path.display()));
            } else if is_cloud_placeholder(entry.attributes) {
                reasons.push(format!("{} 是云占位符，内容还没下载到本地", path.display()));
            }

            rel.pop();
        }
    }

    /// 取源目录里全部文件的快照，按相对路径排序，方便两次快照逐项比对。
    fn snapshot(&self, source: &Path) -> Result<Vec<FileSnapshot>, JunctionError> {
        let anchor = guard::open_anchor(source, &self.forbidden)?;
        let mut files = Vec::new();
        self.collect_files(&anchor, &mut Vec::new(), &mut files)?;
        files.sort_by(|left, right| left.rel.cmp(&right.rel));

        Ok(files)
    }

    /// 递归收集文件。遇到链接直接报错：资格检查已经拒绝了链接，走到这里还有链接
    /// 说明目录在检查之后被改过，不能继续。
    fn collect_files(
        &self,
        anchor: &DirHandle,
        rel: &mut Vec<OsString>,
        out: &mut Vec<FileSnapshot>,
    ) -> Result<(), JunctionError> {
        let volume_serial = anchor.identity().volume_serial;

        for entry in guard::list_dir(anchor, rel, &self.forbidden)? {
            if entry.is_reparse() {
                return Err(JunctionError::Guard(GuardError::ReparseInPath(
                    join_parts(anchor.path(), rel).join(&entry.name),
                )));
            }

            rel.push(entry.name.clone());
            if entry.is_dir {
                self.collect_files(anchor, rel, out)?;
            } else {
                out.push(FileSnapshot {
                    rel: rel.clone(),
                    identity: FileIdentity {
                        volume_serial,
                        file_id: entry.file_id,
                    },
                    size: entry.logical,
                    mtime: entry.last_write_time,
                });
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

/// 两个路径是否相同。Windows 路径不区分大小写。
fn same_path(left: &Path, right: &Path) -> bool {
    equals_ignore_case(&left.to_string_lossy(), &right.to_string_lossy())
}

/// `path` 是否在 `base` 之下（不含 base 自己）。
fn is_under(path: &Path, base: &Path) -> bool {
    let path_text = path.to_string_lossy();
    let mut base_text = base.to_string_lossy().into_owned();
    if !base_text.ends_with('\\') {
        base_text.push('\\');
    }

    // 用 get 而不是直接切片：中文目录名会让 base_text 的长度落在多字节字符中间，
    // 直接切片会 panic，这里按“切不出来就不在下面”处理。
    match path_text.get(..base_text.len()) {
        Some(prefix) => path_text.len() > base_text.len() && equals_ignore_case(prefix, &base_text),
        None => false,
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
