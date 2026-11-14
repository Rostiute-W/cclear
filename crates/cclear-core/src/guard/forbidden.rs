//! 禁区集合：把禁区目录打开后记录文件身份，执行阶段按身份和名字拒绝。
//!
//! 只按路径字符串比较会被 8.3 短名和大小写差异绕过，所以这里以 `FileIdentity` 为准。

use std::collections::HashSet;
use std::ffi::OsStr;

use crate::model::FileIdentity;
use crate::rules::forbidden as rules;
use crate::rules::resolve::RootMap;
use crate::win::{dirinfo, equals_ignore_case};

use super::path;

/// 禁区：`ids` 是禁区目录自身的文件身份，`names` 是任意位置都不许碰的目录名。
pub struct Forbidden {
    pub ids: HashSet<FileIdentity>,
    pub names: Vec<String>,
}

impl Forbidden {
    /// 空禁区集合。隔离区这类完全由 CClear 自己写入的目录用它，
    /// 用户目录的禁区规则不该挡住隔离区内部的相对打开。
    pub fn empty() -> Self {
        Self {
            ids: HashSet::new(),
            names: Vec::new(),
        }
    }

    /// 从规则里的禁区根目录建集合。不存在的目录直接跳过：没装的应用不该挡住启动。
    pub fn from_rules(roots: &RootMap) -> Self {
        let mut ids = HashSet::new();

        for dir in rules::forbidden_dirs(roots) {
            let Ok(handle) = dirinfo::open_dir(&dir) else {
                continue;
            };
            if let Ok(identity) = path::identity_of(&handle) {
                ids.insert(identity);
            }
        }

        Self {
            ids,
            names: rules::forbidden_names()
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        }
    }

    /// 组件的名字或身份命中禁区。
    pub fn matches(&self, name: &OsStr, identity: &FileIdentity) -> bool {
        if self.ids.contains(identity) {
            return true;
        }

        let name = name.to_string_lossy();
        self.names
            .iter()
            .any(|forbidden| equals_ignore_case(&name, forbidden))
    }
}
