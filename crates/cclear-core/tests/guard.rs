//! 路径护栏的集成测试。全部只操作 `tempdir()` 创建的目录。

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use cclear_core::guard::{Forbidden, GuardError, OpenIntent, delete, open_anchor, open_rel};
use tempfile::TempDir;

/// 不含任何禁区的护栏集合。
fn no_forbidden() -> Forbidden {
    Forbidden {
        ids: HashSet::new(),
        names: Vec::new(),
    }
}

fn rel(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

/// 用 `cmd /c mklink /J` 建 junction。测试代码只需保证只操作临时目录。
fn create_junction(link: &Path, target: &Path) {
    let output = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("启动 cmd 失败");

    assert!(
        output.status.success(),
        "创建 junction 失败：{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// 取路径的 8.3 短名。卷上没启用短名或者路径带空格时返回 None，调用方跳过断言。
fn short_path(path: &Path) -> Option<PathBuf> {
    let text = path.display().to_string();
    if text.contains(' ') || text.contains('"') {
        return None;
    }

    let output = Command::new("cmd")
        .args(["/c", &format!("for %A in ({text}) do @echo %~sA")])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let short = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if short.is_empty() {
        return None;
    }

    Some(PathBuf::from(short))
}

#[test]
fn deletes_only_the_target_file() {
    let temp = TempDir::new().unwrap();
    let anchor_path = temp.path().join("anchor");
    fs::create_dir(&anchor_path).unwrap();

    let target = anchor_path.join("f.txt");
    let sentinel = anchor_path.join("sentinel.txt");
    fs::write(&target, "target").unwrap();
    fs::write(&sentinel, "sentinel").unwrap();

    let forbidden = no_forbidden();
    let anchor = open_anchor(&anchor_path, &forbidden).unwrap();

    let entry = open_rel(&anchor, &rel(&["f.txt"]), OpenIntent::Delete, &forbidden).unwrap();
    let expect = entry.expect();
    delete(entry, &expect).unwrap();

    assert!(!target.exists(), "目标文件应当被删除");
    assert!(sentinel.exists(), "同级哨兵文件不能被动到");
}

#[test]
fn rejects_replaced_ancestor_with_junction() {
    let temp = TempDir::new().unwrap();
    let anchor_path = temp.path().join("anchor");
    let real_dir = anchor_path.join("a").join("b");
    fs::create_dir_all(&real_dir).unwrap();

    let inside = real_dir.join("f.txt");
    fs::write(&inside, "inside").unwrap();

    // 锚点外的同名哨兵：junction 一旦被跟随，删除就会落到它身上。
    let outside_dir = temp.path().join("outside").join("b");
    fs::create_dir_all(&outside_dir).unwrap();
    let outside = outside_dir.join("f.txt");
    fs::write(&outside, "outside").unwrap();

    let forbidden = no_forbidden();
    let anchor = open_anchor(&anchor_path, &forbidden).unwrap();
    let path = rel(&["a", "b", "f.txt"]);

    let entry = open_rel(&anchor, &path, OpenIntent::Delete, &forbidden).unwrap();
    // 预览时记录的元数据；后面路径被换掉，删除前应当先卡在打开这一步。
    let _expect = entry.expect();
    // 预览早就结束了，句柄不能一直攥着，否则祖先目录改不了名。
    drop(entry);

    // 预览之后把祖先换成指向外部目录的 junction。
    fs::rename(anchor_path.join("a"), anchor_path.join("a_moved")).unwrap();
    create_junction(&anchor_path.join("a"), &temp.path().join("outside"));

    let Err(error) = open_rel(&anchor, &path, OpenIntent::Delete, &forbidden) else {
        panic!("祖先被换成 junction 后不应当还能打开");
    };
    assert!(
        matches!(error, GuardError::ReparseInPath(_)),
        "应当识别出路径中的 junction，实际：{error:?}"
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside");
}

#[test]
fn rejects_same_name_replacement() {
    let temp = TempDir::new().unwrap();
    let anchor_path = temp.path().join("anchor");
    fs::create_dir(&anchor_path).unwrap();

    let target = anchor_path.join("f.txt");
    fs::write(&target, "old").unwrap();

    let forbidden = no_forbidden();
    let anchor = open_anchor(&anchor_path, &forbidden).unwrap();

    let entry = open_rel(&anchor, &rel(&["f.txt"]), OpenIntent::Delete, &forbidden).unwrap();
    let expect = entry.expect();

    // 删除前换成同名的新文件：身份变了，大小和修改时间也可能一样。
    fs::remove_file(&target).unwrap();
    fs::write(&target, "new").unwrap();

    let error = delete(entry, &expect).unwrap_err();
    assert!(
        matches!(error, GuardError::Changed),
        "同名新文件应当返回 Changed，实际：{error:?}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "new");
}

#[test]
fn deleting_one_hard_link_keeps_the_other() {
    let temp = TempDir::new().unwrap();
    let anchor_path = temp.path().join("anchor");
    fs::create_dir(&anchor_path).unwrap();

    let target = anchor_path.join("f.txt");
    fs::write(&target, "shared").unwrap();

    // 锚点外的硬链接指向同一个文件。
    let other = temp.path().join("g.txt");
    fs::hard_link(&target, &other).unwrap();

    let forbidden = no_forbidden();
    let anchor = open_anchor(&anchor_path, &forbidden).unwrap();

    let entry = open_rel(&anchor, &rel(&["f.txt"]), OpenIntent::Delete, &forbidden).unwrap();
    let expect = entry.expect();
    delete(entry, &expect).unwrap();

    assert!(!target.exists(), "锚点内的名字应当消失");
    assert_eq!(
        fs::read_to_string(&other).unwrap(),
        "shared",
        "另一个硬链接的内容不能被影响"
    );
}

#[test]
fn refuses_read_only_file() {
    let temp = TempDir::new().unwrap();
    let anchor_path = temp.path().join("anchor");
    fs::create_dir(&anchor_path).unwrap();

    let target = anchor_path.join("f.txt");
    fs::write(&target, "locked").unwrap();
    let mut permissions = target.metadata().unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&target, permissions).unwrap();

    let forbidden = no_forbidden();
    let anchor = open_anchor(&anchor_path, &forbidden).unwrap();

    let entry = open_rel(&anchor, &rel(&["f.txt"]), OpenIntent::Delete, &forbidden).unwrap();
    let expect = entry.expect();

    let error = delete(entry, &expect).unwrap_err();
    assert!(
        matches!(error, GuardError::ReadOnly),
        "只读文件应当返回 ReadOnly，实际：{error:?}"
    );
    assert!(target.exists(), "只读文件不能被删除");
    assert!(
        target.metadata().unwrap().permissions().readonly(),
        "只读属性不能被修改"
    );

    // 交还只读属性，免得临时目录清理不掉。项目只在 Windows 上跑，这里的警告不适用。
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut permissions = target.metadata().unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&target, permissions).unwrap();
    }
}

#[test]
fn refuses_file_locked_without_delete_sharing() {
    let temp = TempDir::new().unwrap();
    let anchor_path = temp.path().join("anchor");
    fs::create_dir(&anchor_path).unwrap();

    let target = anchor_path.join("f.txt");
    fs::write(&target, "busy").unwrap();

    // FILE_SHARE_READ | FILE_SHARE_WRITE，不含 FILE_SHARE_DELETE。
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&target)
        .unwrap();

    let forbidden = no_forbidden();
    let anchor = open_anchor(&anchor_path, &forbidden).unwrap();

    // 删除流程在申请 DELETE 权限时就会撞上共享冲突，所以整条流程都要报 Locked。
    let result =
        open_rel(&anchor, &rel(&["f.txt"]), OpenIntent::Delete, &forbidden).and_then(|entry| {
            let expect = entry.expect();
            delete(entry, &expect)
        });

    assert!(
        matches!(result, Err(GuardError::Locked)),
        "被占用的文件应当返回 Locked，实际：{result:?}"
    );
    assert!(target.exists(), "被占用的文件不能被删除");
}

#[test]
fn deletes_file_with_long_path() {
    let temp = TempDir::new().unwrap();
    let anchor_path = temp.path().join("anchor");
    fs::create_dir(&anchor_path).unwrap();

    // 5 层 70 字符的目录，拼起来超过 300 字符。相对打开不经过 MAX_PATH 限制。
    let mut path = anchor_path.clone();
    let mut parts = Vec::new();
    for _ in 0..5 {
        let name = "d".repeat(70);
        path.push(&name);
        parts.push(name);
    }
    let long_dir = cclear_core::win::to_verbatim(&path);
    fs::create_dir_all(&long_dir).unwrap();
    fs::write(long_dir.join("f.txt"), "long").unwrap();

    assert!(path.as_os_str().len() > 300, "路径应当超过 300 字符");

    let forbidden = no_forbidden();
    let anchor = open_anchor(&anchor_path, &forbidden).unwrap();

    let mut names: Vec<OsString> = parts.into_iter().map(OsString::from).collect();
    names.push(OsString::from("f.txt"));

    let entry = open_rel(&anchor, &names, OpenIntent::Delete, &forbidden).unwrap();
    let expect = entry.expect();
    delete(entry, &expect).unwrap();

    assert!(!path.join("f.txt").exists(), "长路径下的文件应当被删除");
    assert!(path.exists(), "父目录不能被误删");
}

#[test]
fn refuses_forbidden_directory_by_identity_and_short_name() {
    let temp = TempDir::new().unwrap();
    // 名字长一点，卷上才会生成可用的 8.3 短名。
    let forbidden_dir = temp.path().join("cclear-forbidden-long-name");
    let sub = forbidden_dir.join("sub");
    fs::create_dir_all(&sub).unwrap();

    let mut forbidden = no_forbidden();
    let anchor = open_anchor(temp.path(), &forbidden).unwrap();

    // 先用空禁区拿到目录身份，再把它加进禁区。
    let entry = open_rel(
        &anchor,
        &rel(&["cclear-forbidden-long-name"]),
        OpenIntent::Inspect,
        &forbidden,
    )
    .unwrap();
    forbidden.ids.insert(entry.identity);

    let Err(error) = open_anchor(&sub, &forbidden) else {
        panic!("禁区目录的锚点不应当能打开");
    };
    assert!(
        matches!(error, GuardError::Forbidden(_)),
        "禁区目录应当返回 Forbidden，实际：{error:?}"
    );

    // 8.3 短名是同一个对象的别名，按身份比较必须同样拒绝。
    if let Some(short) = short_path(&forbidden_dir)
        && short != forbidden_dir
    {
        let Err(error) = open_anchor(&short.join("sub"), &forbidden) else {
            panic!("短名路径的锚点不应当能打开");
        };
        assert!(
            matches!(error, GuardError::Forbidden(_)),
            "短名路径应当返回 Forbidden，实际：{error:?}"
        );
    }
}

#[test]
fn refuses_non_empty_directory() {
    let temp = TempDir::new().unwrap();
    let anchor_path = temp.path().join("anchor");
    let dir = anchor_path.join("d");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("sub.txt"), "kept").unwrap();

    let forbidden = no_forbidden();
    let anchor = open_anchor(&anchor_path, &forbidden).unwrap();

    let entry = open_rel(&anchor, &rel(&["d"]), OpenIntent::Delete, &forbidden).unwrap();
    let expect = entry.expect();

    let error = delete(entry, &expect).unwrap_err();
    assert!(
        matches!(error, GuardError::DirNotEmpty),
        "非空目录应当返回 DirNotEmpty，实际：{error:?}"
    );
    assert!(dir.join("sub.txt").exists());
}
