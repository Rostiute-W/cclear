//! 路径护栏：全项目唯一允许删除、改名和操作 reparse point 的地方。
//!
//! - `external` 校验前端传来的分析路径；
//! - `forbidden` 按文件身份记录禁区目录；
//! - `path` 用父句柄逐级相对打开，并在同一个句柄上复核后删除。

pub mod external;
pub mod forbidden;
pub mod path;

use std::io;
use std::path::PathBuf;

use thiserror::Error;

pub use external::validate_external_path;
pub use forbidden::Forbidden;
pub use path::{
    DirHandle, Entry, Expect, OpenIntent, create_junction, delete, delete_tree, list_dir,
    open_anchor, open_rel, read_junction_target, remove_junction, rename_child, replace_in_dir,
};

/// 路径护栏的错误。
#[derive(Debug, Error)]
pub enum GuardError {
    #[error("路径不合法：{0}")]
    InvalidExternalPath(String),

    #[error("路径中存在 reparse point：{}", .0.display())]
    ReparseInPath(PathBuf),

    #[error("路径落在禁区：{}", .0.display())]
    Forbidden(PathBuf),

    #[error("文件与预览时不一致，需要重新扫描")]
    Changed,

    #[error("文件已不存在")]
    Gone,

    #[error("文件正被其他程序占用")]
    Locked,

    #[error("没有访问权限")]
    Denied,

    #[error("文件带只读属性，未删除")]
    ReadOnly,

    #[error("目录不为空")]
    DirNotEmpty,

    #[error("路径组件不合法")]
    InvalidName,

    #[error("junction 目标不合法：{0}")]
    InvalidTarget(String),

    #[error("链接与预期不符，未做任何改动：{}", .0.display())]
    LinkMismatch(PathBuf),

    #[error("系统调用失败：{0}")]
    Io(#[from] io::Error),
}
