//! 外部命令的启动。

use std::io;
use std::os::windows::process::CommandExt;
use std::path::Path;

use crate::win::{KnownFolder, known_folder};

/// 在资源管理器中打开并选中目标。
///
/// 不等待 explorer 退出：它会把命令行转交给已经运行的实例后马上返回。
pub fn open_in_explorer(path: &Path) -> io::Result<()> {
    let explorer = known_folder(KnownFolder::Windows)
        .map_err(io::Error::other)?
        .join("explorer.exe");

    // 用 raw_arg 原样拼参数：/select 后面的引号必须由我们自己控制，
    // 交给 std 的默认转义会改掉 explorer 看到的命令行。
    let argument = format!("/select,\"{}\"", path.display());
    std::process::Command::new(explorer)
        .raw_arg(argument)
        .spawn()?;

    Ok(())
}
