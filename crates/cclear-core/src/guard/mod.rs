//! 路径护栏：全项目唯一允许删除、改名和操作 reparse point 的地方。

pub mod external;

pub use external::{GuardError, validate_external_path};
