//! 扫描器集成测试。全部只在 tempdir 中操作。

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use cclear_core::scan::{EntryInfo, ScanOptions, ScanProgress, ScanResult, scan};
use tempfile::tempdir;

const MIB: u64 = 1024 * 1024;

fn run_scan(root: &Path, options: ScanOptions) -> ScanResult {
    let progress = ScanProgress::default();
    let cancel = AtomicBool::new(false);
    scan(&[root.to_path_buf()], &options, &progress, &cancel).expect("扫描失败")
}

fn create_junction(link: &Path, target: &Path) {
    use std::os::windows::process::CommandExt;

    let command = format!("mklink /J \"{}\" \"{}\"", link.display(), target.display());
    let status = std::process::Command::new("cmd")
        .arg("/c")
        .raw_arg(command)
        .status()
        .expect("无法执行 mklink");
    assert!(status.success(), "创建 junction 失败");
}

#[test]
fn hard_link_counted_once() {
    let temp = tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();

    let original = first.join("data.bin");
    fs::write(&original, vec![7u8; MIB as usize]).unwrap();
    fs::hard_link(&original, second.join("link.bin")).unwrap();

    let result = run_scan(temp.path(), ScanOptions::default());
    let root = &result.nodes[result.roots[0] as usize];

    // 同一个文件有第二个名字，大小和文件数都只能算一次。
    assert_eq!(root.size.logical, MIB);
    assert_eq!(root.size.files, 1);
}

#[test]
fn junction_not_followed() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("A");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("data.bin"), vec![7u8; MIB as usize]).unwrap();
    create_junction(&temp.path().join("J"), &target);

    let result = run_scan(temp.path(), ScanOptions::default());
    let root = &result.nodes[result.roots[0] as usize];

    // J 指向 A，只有 A 里的文件被统计一次。
    assert_eq!(root.size.logical, MIB);
    assert_eq!(root.size.files, 1);
    assert!(root.reparse_skipped >= 1);
}

#[test]
fn long_path_with_chinese_names() {
    let temp = tempdir().unwrap();
    let mut dir = temp.path().to_path_buf();

    // 逐级加深直到路径超过 MAX_PATH，验证长路径和中文名都能正常枚举。
    let mut level = 0;
    while dir.as_os_str().len() <= 300 {
        dir = dir.join(format!("中文目录{level}_{}", "长".repeat(20)));
        fs::create_dir(&dir).unwrap();
        level += 1;
    }

    let file_size = 4096u64;
    fs::write(dir.join("数据.bin"), vec![7u8; file_size as usize]).unwrap();

    let result = run_scan(temp.path(), ScanOptions::default());
    let root = &result.nodes[result.roots[0] as usize];
    assert_eq!(root.size.files, 1);
    assert_eq!(root.size.logical, file_size);

    let deepest = result
        .nodes
        .iter()
        .find(|node| result.path_of(node.id) == dir)
        .expect("没有扫到最深的目录");

    assert_eq!(result.path_of(deepest.id), dir);
    assert!(result.path_of(deepest.id).as_os_str().len() > 300);
}

#[test]
fn file_filter_excludes_matching_files() {
    let temp = tempdir().unwrap();
    fs::write(temp.path().join("keep.bin"), vec![7u8; MIB as usize]).unwrap();
    fs::write(temp.path().join("skip.tmp"), vec![7u8; MIB as usize]).unwrap();

    let options = ScanOptions {
        file_filter: Some(Arc::new(|entry: &EntryInfo| {
            entry.name.to_string_lossy() != "skip.tmp"
        })),
        ..ScanOptions::default()
    };

    let result = run_scan(temp.path(), options);
    let root = &result.nodes[result.roots[0] as usize];

    // 被 filter 排除的文件不计入大小和文件数。
    assert_eq!(root.size.files, 1);
    assert_eq!(root.size.logical, MIB);
}
