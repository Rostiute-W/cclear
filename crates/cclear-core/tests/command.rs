//! 命令执行器的集成测试。PATH 只在临时目录里造，不碰真实用户目录。

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use cclear_core::exec::command::resolve_tool_in;
use tempfile::tempdir;

#[test]
fn resolve_tool_in_follows_path_order_and_ignores_ps1() {
    let base = tempdir().unwrap();
    let first = base.path().join("dir1");
    let second = base.path().join("dir2");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();

    // dir1 里只有批处理脚本，dir2 里是可执行文件：按 PATH 顺序应该先找到 dir1 的。
    fs::write(first.join("foo.cmd"), "@echo off\r\n").unwrap();
    fs::write(second.join("foo.exe"), "").unwrap();
    let path_var = join_paths(&[&first, &second]);

    assert_eq!(
        resolve_tool_in("foo", &path_var),
        Some(first.join("foo.cmd")),
        "没有按 PATH 顺序找到 dir1 的 foo.cmd"
    );

    // 只剩 .ps1 时不认：脚本执行策略常常禁用，调用了也跑不起来。
    fs::remove_file(first.join("foo.cmd")).unwrap();
    fs::remove_file(second.join("foo.exe")).unwrap();
    fs::write(first.join("foo.ps1"), "").unwrap();

    assert_eq!(resolve_tool_in("foo", &path_var), None);
}

/// 按 Windows 的 PATH 格式把多个目录拼成一个值。
fn join_paths(dirs: &[&Path]) -> OsString {
    let mut joined = OsString::new();

    for (index, dir) in dirs.iter().enumerate() {
        if index > 0 {
            joined.push(";");
        }
        joined.push(dir.as_os_str());
    }

    joined
}
