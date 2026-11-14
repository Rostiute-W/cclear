//! 公共错误类型。

use thiserror::Error;

/// 核心库的公共错误。
#[derive(Debug, Error)]
pub enum CoreError {
    /// 扫描还没开始就失败了（例如线程池创建失败）。单个目录读不到不算错误，按“拒绝访问”计数。
    #[error("扫描无法开始：{0}")]
    ScanStart(String),

    /// 系统目录解析失败。缺根目录会让规则静默失效，所以直接报错。
    #[error("无法解析系统目录「{0}」，请重新启动 CClear")]
    KnownFolder(&'static str),
}

/// 核心库统一使用的结果类型。
pub type Result<T> = std::result::Result<T, CoreError>;
