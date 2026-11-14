//! 批次清单：记录每个条目的来源、哈希和状态，并用句柄原子替换写入。

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf, Prefix};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::guard::{
    DirHandle, Forbidden, GuardError, OpenIntent, create_file_in, delete, open_anchor, open_rel,
    replace_in_dir,
};
use crate::model::FileIdentity;

/// 批次清单的文件名。
pub const MANIFEST_NAME: &str = "manifest.json";

/// 先写这个名字，再用 `replace_in_dir` 换成正式名字，读者永远看不到写了一半的清单。
const MANIFEST_TMP_NAME: &str = "manifest.json.tmp";

/// 批次里放副本的子目录。
pub const DATA_DIR: &str = "data";

/// 批次状态。按事务顺序推进，异常时停在能人工处理的状态上。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BatchStage {
    /// 已经计划好，还没有开始写文件。
    Planned,
    /// 正在往隔离区复制。
    Copying,
    /// 复制完成，源和副本的哈希一致。
    CopiedVerified,
    /// 正在删除源文件。
    DeletingSource,
    /// 全部条目都隔离完成。
    Committed,
    /// 正在往原位还原。
    Restoring,
    /// 已还原。
    Restored,
    /// 正在彻底删除批次。
    Purging,
    /// 已彻底删除。
    Purged,
    /// 已中止：源没有动过，残留的副本已经清掉。
    Aborted,
    /// 需要用户决定继续还是放弃。
    NeedsAttention,
}

impl BatchStage {
    /// 界面显示用的中文名。
    pub fn label(self) -> &'static str {
        match self {
            BatchStage::Planned => "已计划",
            BatchStage::Copying => "复制中",
            BatchStage::CopiedVerified => "已复制校验",
            BatchStage::DeletingSource => "删除源中",
            BatchStage::Committed => "已隔离",
            BatchStage::Restoring => "还原中",
            BatchStage::Restored => "已还原",
            BatchStage::Purging => "删除中",
            BatchStage::Purged => "已彻底删除",
            BatchStage::Aborted => "已中止",
            BatchStage::NeedsAttention => "需要处理",
        }
    }

    /// 机器可读的状态名，和 serde 的 camelCase 一致，界面按它选样式和按钮。
    pub fn key(self) -> &'static str {
        match self {
            BatchStage::Planned => "planned",
            BatchStage::Copying => "copying",
            BatchStage::CopiedVerified => "copiedVerified",
            BatchStage::DeletingSource => "deletingSource",
            BatchStage::Committed => "committed",
            BatchStage::Restoring => "restoring",
            BatchStage::Restored => "restored",
            BatchStage::Purging => "purging",
            BatchStage::Purged => "purged",
            BatchStage::Aborted => "aborted",
            BatchStage::NeedsAttention => "needsAttention",
        }
    }
}

/// 单个条目在事务里的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemState {
    /// 已通过预检，还没有复制。
    Pending,
    /// 副本已写入并通过哈希校验，源还在。
    Copied,
    /// 源已删除，副本是唯一的一份。
    SourceDeleted,
    /// 源发生了变化，源和副本都保留。
    Kept,
    /// 已经还原回原位。
    Restored,
    /// 源和副本都对不上，需要用户自己确认后才能处理。
    NeedsAttention,
}

impl ItemState {
    /// 界面显示用的中文名。
    pub fn label(self) -> &'static str {
        match self {
            ItemState::Pending => "待复制",
            ItemState::Copied => "已复制",
            ItemState::SourceDeleted => "已隔离",
            ItemState::Kept => "源保留",
            ItemState::Restored => "已还原",
            ItemState::NeedsAttention => "需要处理",
        }
    }
}

/// 清单里的一个条目。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestItem {
    /// 原始锚点在清单里的序号，决定副本放在 `data\<序号>` 下。
    pub anchor_index: u16,
    /// 原始锚点路径。只用于展示和重新打开护栏，不参与删除。
    pub anchor: String,
    /// 相对锚点的路径组件。文件名不是 UTF-8 时会有损，这类名字很少见。
    pub rel: Vec<String>,
    /// 源文件的卷序列号。和 `file_id` 一起判断“还是同一个文件”。
    pub volume_serial: u64,
    /// 文件 ID 的十进制字符串。128 位整数放进 JSON 数字会丢精度。
    pub file_id: String,
    pub size: u64,
    pub mtime: u64,
    pub ctime: u64,
    /// 复制后算出的 blake3 哈希，十六进制小写。
    pub hash: String,
    pub state: ItemState,
    /// 跳过原因或异常说明，界面直接显示。
    pub note: String,
}

impl ManifestItem {
    /// 原始位置的完整路径。只用于复制和展示，删除一律走护栏的相对打开。
    pub fn source_path(&self) -> PathBuf {
        let mut path = PathBuf::from(&self.anchor);
        for part in &self.rel {
            path.push(part);
        }
        path
    }

    /// 副本相对批次目录的路径：`data\<锚点序号>\<原始相对路径>`。
    pub fn copy_rel_path(&self) -> Vec<OsString> {
        let mut rel = vec![
            OsString::from(DATA_DIR),
            OsString::from(self.anchor_index.to_string()),
        ];
        rel.extend(self.rel.iter().map(OsString::from));
        rel
    }

    /// 副本的完整路径。
    pub fn copy_path(&self, batch_dir: &Path) -> PathBuf {
        let mut path = batch_dir.to_path_buf();
        for part in self.copy_rel_path() {
            path.push(part);
        }
        path
    }

    /// 源文件的身份。清单里的 ID 解析不出来时返回 None，调用方按“已变化”处理。
    pub fn identity(&self) -> Option<FileIdentity> {
        Some(FileIdentity {
            volume_serial: self.volume_serial,
            file_id: self.file_id.parse().ok()?,
        })
    }
}

/// 一个批次的完整清单。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub batch_id: Uuid,
    /// Unix 秒。界面按它算存放天数。
    pub created_at: i64,
    /// 来源规则 ID，名称由界面查规则目录。
    pub rule_id: String,
    pub stage: BatchStage,
    /// 当前状态说明，异常时写清楚为什么停在这里。
    pub note: String,
    /// 批次由提权 helper 创建，还原和彻底删除也必须交回 helper 执行。
    /// 旧清单没有这个字段，按 false 读。
    #[serde(default)]
    pub requires_admin: bool,
    pub items: Vec<ManifestItem>,
}

impl Manifest {
    /// 读批次目录里的清单。文件不存在或解析失败都按错误返回，由调用方决定怎么处理。
    pub fn load(batch_dir: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(batch_dir.join(MANIFEST_NAME))?;
        serde_json::from_str(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// 原子写入清单：先写临时文件并刷盘，再用护栏的目录句柄替换正式文件。
    ///
    /// 大批次要反复保存（每条规则可能上万条），所以写紧凑 JSON：
    /// 读取端用 serde_json 解析，旧的 pretty 格式照样读得出来。
    pub fn save(&self, batch_dir: &Path) -> Result<(), GuardError> {
        let json =
            serde_json::to_vec(self).map_err(|error| GuardError::Io(io::Error::other(error)))?;

        // 批次清单在隔离区里，临时文件也必须相对目录句柄创建，不走字符串路径。
        let dir = open_anchor(batch_dir, &Forbidden::empty())?;
        // 上次写到一半留下的临时文件会让 `create_file_in` 一直失败，先清掉。
        remove_stale_temp(&dir);

        // 必须先把内容落到磁盘再替换：否则掉电后可能换上一个空文件。
        {
            let mut file = create_file_in(&dir, OsStr::new(MANIFEST_TMP_NAME))?;
            io::Write::write_all(&mut file, &json)?;
            file.sync_all()?;
        }

        replace_in_dir(
            &dir,
            OsStr::new(MANIFEST_TMP_NAME),
            OsStr::new(MANIFEST_NAME),
        )
    }

    /// 校验每个条目的路径。清单在普通用户可写的隔离区里，被动过之后不能照着它动手。
    ///
    /// 只要有一个条目不合格就整批拒绝：漏过一条就可能把文件写到清单之外的位置。
    pub fn validate(&self) -> Result<(), String> {
        for item in &self.items {
            if !is_local_path(&item.anchor) {
                return Err(format!("锚点不是本地盘符路径：{}", item.anchor));
            }

            // 空的 rel 会让源路径退化成锚点目录、副本路径退化成 data\<序号>，都不该出现。
            if item.rel.is_empty() {
                return Err(format!("条目「{}」没有相对路径", item.anchor));
            }

            for part in &item.rel {
                crate::guard::validate_component(OsStr::new(part)).map_err(|_| {
                    format!("条目「{}」包含不合法的路径组件「{part}」", item.anchor)
                })?;
            }
        }

        Ok(())
    }

    /// 条目数。
    pub fn item_count(&self) -> u64 {
        self.items.len() as u64
    }

    /// 条目大小合计，界面按它显示“批次占用”。
    pub fn total_size(&self) -> u64 {
        self.items.iter().map(|item| item.size).sum()
    }

    /// 清单里还有多少个条目停在某个状态。
    pub fn count_state(&self, state: ItemState) -> u64 {
        self.items.iter().filter(|item| item.state == state).count() as u64
    }
}

/// 清掉上次写入残留的临时文件。`create_file_in` 只新建，同名残留会让保存一直失败。
fn remove_stale_temp(dir: &DirHandle) {
    let forbidden = Forbidden::empty();
    let rel = [OsString::from(MANIFEST_TMP_NAME)];

    if let Ok(entry) = open_rel(dir, &rel, OpenIntent::Delete, &forbidden) {
        let expect = entry.expect();
        let _ = delete(entry, &expect);
    }
}

/// 锚点是否是一个普通的本地盘符绝对路径。
///
/// `Prefix::Disk` 只匹配 `X:\...`：`\\?\X:\` 走的是 `VerbatimDisk`，UNC 是 `UNC`，
/// 两者都会被拒绝。`.`、`..` 和带冒号的组件不是 `Normal`，同样被拒绝；
/// 剩下的每一段还要过护栏的组件校验，避免把带 ADS 之类的名字当成正常目录名。
fn is_local_path(text: &str) -> bool {
    let mut components = Path::new(text).components();

    let Some(Component::Prefix(prefix)) = components.next() else {
        return false;
    };
    if !matches!(prefix.kind(), Prefix::Disk(_)) {
        return false;
    }
    if components.next() != Some(Component::RootDir) {
        return false;
    }

    components.all(|component| match component {
        Component::Normal(name) => crate::guard::validate_component(name).is_ok(),
        _ => false,
    })
}

/// 列表里一个批次的概要。
#[derive(Clone, Debug)]
pub struct BatchSummary {
    pub batch_id: Uuid,
    pub created_at: i64,
    pub rule_id: String,
    pub stage: BatchStage,
    pub note: String,
    pub items: u64,
    pub size: u64,
    /// 已经存放了多少天，界面用来提示超过 7 天的批次。
    pub age_days: u64,
    /// 批次来自提权 helper，还原和彻底删除需要管理员权限。
    pub requires_admin: bool,
}
