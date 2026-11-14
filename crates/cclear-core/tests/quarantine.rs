//! 隔离区事务的集成测试。全部只操作 `tempdir()` 创建的目录。

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use cclear_core::elevation::validate_admin_batch;
use cclear_core::guard::{Forbidden, OpenIntent, open_anchor, open_rel};
use cclear_core::journal::{JournalSink, Record};
use cclear_core::model::KnownRoot;
use cclear_core::quarantine::{
    BatchStage, ItemState, Manifest, QuarantineError, QuarantineItem, QuarantineRoot, ResolveAction,
};
use cclear_core::rules::resolve::RootMap;
use tempfile::TempDir;

/// 1 GiB。测试里当“空间充足”的注入值。
const GIB: u64 = 1024 * 1024 * 1024;

/// 只接收记录、不写文件的日志出口。隔离流程只需要日志能接收。
struct NullJournal;

impl JournalSink for NullJournal {
    fn append(&mut self, _record: &Record) -> io::Result<()> {
        Ok(())
    }
}

/// 递归列出目录下的全部文件，用来确认目标目录里到底写了什么。
fn collect_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return files;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(collect_files(&path));
        } else {
            files.push(path);
        }
    }

    files.sort();
    files
}

/// 隔离根下唯一的那个批次目录。
fn only_batch_dir(root: &Path) -> PathBuf {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(dirs.len(), 1, "应当只有一个批次目录");
    dirs.pop().unwrap()
}

#[test]
fn quarantines_then_restores_with_same_content_and_mtime() {
    let temp = TempDir::new().unwrap();
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).unwrap();

    let file = source_dir.join("a.txt");
    fs::write(&file, "隔离之后要能原样还原").unwrap();
    let content = fs::read(&file).unwrap();
    let mtime = fs::metadata(&file).unwrap().modified().unwrap();

    let engine =
        QuarantineRoot::for_test(temp.path().join("quarantine")).with_free_space(|_| 64 * GIB);

    let anchors = vec![source_dir.clone()];
    let items = vec![QuarantineItem::new(0, &["a.txt"])];
    let mut journal = NullJournal;

    let outcome = engine
        .quarantine(
            "test-rule",
            &Forbidden::empty(),
            &anchors,
            &items,
            &mut journal,
            &AtomicBool::new(false),
        )
        .unwrap();

    assert_eq!(outcome.stage, BatchStage::Committed);
    assert_eq!(outcome.moved, 1);
    assert!(!file.exists(), "隔离之后源文件应当已经移走");

    let copy = engine
        .path()
        .join(outcome.batch_id.to_string())
        .join("data")
        .join("0")
        .join("a.txt");
    assert_eq!(fs::read(&copy).unwrap(), content, "副本内容要和源一致");

    let restored = engine.restore(outcome.batch_id, &mut journal).unwrap();
    assert_eq!(restored.stage, BatchStage::Restored);
    assert_eq!(restored.restored, 1);
    assert!(restored.conflicts.is_empty());

    assert!(file.exists(), "还原之后源文件应当回到原位");
    assert_eq!(fs::read(&file).unwrap(), content, "内容哈希必须与原来一致");
    assert_eq!(
        fs::metadata(&file).unwrap().modified().unwrap(),
        mtime,
        "修改时间必须与原来一致"
    );
    assert!(!copy.exists(), "还原之后副本必须被清除");

    let batches = engine.batches().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].stage, BatchStage::Restored);
}

#[test]
fn changed_source_is_kept_with_its_copy() {
    let temp = TempDir::new().unwrap();
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).unwrap();

    let file = source_dir.join("a.txt");
    fs::write(&file, "原始内容要够长一点").unwrap();

    // 复制校验完成之后、删除源之前改动源文件，模拟窗口期被别的程序写入。
    let file_for_hook = file.clone();
    let engine = QuarantineRoot::for_test(temp.path().join("quarantine"))
        .with_free_space(|_| 64 * GIB)
        .with_after_copy(move || {
            fs::write(&file_for_hook, "changed").unwrap();
        });

    let anchors = vec![source_dir.clone()];
    let items = vec![QuarantineItem::new(0, &["a.txt"])];
    let mut journal = NullJournal;

    let outcome = engine
        .quarantine(
            "test-rule",
            &Forbidden::empty(),
            &anchors,
            &items,
            &mut journal,
            &AtomicBool::new(false),
        )
        .unwrap();

    assert_eq!(
        outcome.stage,
        BatchStage::NeedsAttention,
        "批次应当是部分完成"
    );
    assert_eq!(outcome.kept, 1);
    assert_eq!(outcome.moved, 0);

    assert!(file.exists(), "源已变化时必须保留源");
    assert_eq!(fs::read_to_string(&file).unwrap(), "changed");

    let copy = engine
        .path()
        .join(outcome.batch_id.to_string())
        .join("data")
        .join("0")
        .join("a.txt");
    assert!(copy.exists(), "源已变化时副本也要保留");

    let manifest = Manifest::load(&only_batch_dir(engine.path())).unwrap();
    assert_eq!(manifest.items.len(), 1);
    assert_eq!(
        manifest.items[0].state,
        cclear_core::quarantine::ItemState::Kept
    );
}

#[test]
fn recovers_interrupted_copy() {
    let temp = TempDir::new().unwrap();
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).unwrap();

    let first = source_dir.join("a.txt");
    let second = source_dir.join("b.txt");
    fs::write(&first, "第一个文件").unwrap();
    fs::write(&second, "第二个文件").unwrap();

    // 复制完第一个文件后中断，批次停在 Copying。
    let engine = QuarantineRoot::for_test(temp.path().join("quarantine"))
        .with_free_space(|_| 64 * GIB)
        .with_crash_after_files(1);

    let anchors = vec![source_dir.clone()];
    let items = vec![
        QuarantineItem::new(0, &["a.txt"]),
        QuarantineItem::new(0, &["b.txt"]),
    ];
    let mut journal = NullJournal;

    let result = engine.quarantine(
        "test-rule",
        &Forbidden::empty(),
        &anchors,
        &items,
        &mut journal,
        &AtomicBool::new(false),
    );
    assert!(result.is_err(), "注入的中断应当让这次隔离失败");

    // 源文件一个都不能少。
    assert_eq!(fs::read_to_string(&first).unwrap(), "第一个文件");
    assert_eq!(fs::read_to_string(&second).unwrap(), "第二个文件");

    let batch_dir = only_batch_dir(engine.path());
    let manifest = Manifest::load(&batch_dir).unwrap();
    assert_eq!(
        manifest.stage,
        BatchStage::Copying,
        "中断时批次应当停在复制中"
    );

    let reports = engine.recover_pending(&mut journal).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].stage, BatchStage::Aborted);

    assert_eq!(fs::read_to_string(&first).unwrap(), "第一个文件");
    assert_eq!(fs::read_to_string(&second).unwrap(), "第二个文件");

    let left = collect_files(&batch_dir);
    assert_eq!(
        left,
        vec![batch_dir.join("manifest.json")],
        "半成品副本必须被删除，只剩批次清单"
    );

    let batches = engine.batches().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].stage, BatchStage::Aborted);
}

#[test]
fn rejects_batch_when_volume_has_no_space() {
    let temp = TempDir::new().unwrap();
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).unwrap();

    let file = source_dir.join("a.txt");
    fs::write(&file, "空间不足时一个字节都不能写").unwrap();

    let root = temp.path().join("quarantine");
    // 剩余空间远小于 2 GiB 的预留，整批应当被拒绝。
    let engine = QuarantineRoot::for_test(root.clone()).with_free_space(|_| 1024);

    let anchors = vec![source_dir.clone()];
    let items = vec![QuarantineItem::new(0, &["a.txt"])];
    let mut journal = NullJournal;

    let error = engine
        .quarantine(
            "test-rule",
            &Forbidden::empty(),
            &anchors,
            &items,
            &mut journal,
            &AtomicBool::new(false),
        )
        .unwrap_err();

    assert!(
        matches!(error, QuarantineError::NoSpace { .. }),
        "空间不足应当整批拒绝，实际：{error}"
    );
    assert!(!root.exists(), "整批拒绝时目标目录里不能有任何写入");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "空间不足时一个字节都不能写",
        "源文件不能被改动"
    );
}

#[test]
fn interrupted_source_deletion_keeps_only_copies() {
    let temp = TempDir::new().unwrap();
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).unwrap();

    let first = source_dir.join("a.txt");
    let second = source_dir.join("b.txt");
    fs::write(&first, "第一个文件的内容").unwrap();
    fs::write(&second, "第二个文件的内容").unwrap();

    let root = temp.path().join("quarantine");
    // 删掉第一个源之后就中断，模拟删源阶段进程被强行结束。
    let engine = QuarantineRoot::for_test(root.clone())
        .with_free_space(|_| 64 * GIB)
        .with_crash_after_deletions(1);

    let anchors = vec![source_dir.clone()];
    let items = vec![
        QuarantineItem::new(0, &["a.txt"]),
        QuarantineItem::new(0, &["b.txt"]),
    ];
    let mut journal = NullJournal;

    let error = engine
        .quarantine(
            "test-rule",
            &Forbidden::empty(),
            &anchors,
            &items,
            &mut journal,
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(
        matches!(error, QuarantineError::Injected),
        "注入的中断应当让这次隔离失败"
    );
    assert!(!first.exists(), "第一个源应当已经删除");
    assert_eq!(fs::read_to_string(&second).unwrap(), "第二个文件的内容");

    // 重新打开隔离区：逐条核对之后停在“需要处理”，两个条目的状态分开记。
    let engine = QuarantineRoot::for_test(root).with_free_space(|_| 64 * GIB);
    let reports = engine.recover_pending(&mut journal).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].stage, BatchStage::NeedsAttention);

    let batch_dir = only_batch_dir(engine.path());
    let manifest = Manifest::load(&batch_dir).unwrap();
    assert_eq!(
        manifest.items[0].state,
        ItemState::SourceDeleted,
        "源已删除的条目要如实记成已隔离"
    );
    assert_eq!(
        manifest.items[1].state,
        ItemState::Copied,
        "源还在的条目仍停在已复制"
    );

    // 放弃：只删源还在原位的副本。
    let outcome = engine
        .resolve(
            manifest.batch_id,
            ResolveAction::Abandon,
            &Forbidden::empty(),
            &mut journal,
        )
        .unwrap();
    assert_eq!(outcome.moved, 1);

    let copy_first = batch_dir.join("data").join("0").join("a.txt");
    let copy_second = batch_dir.join("data").join("0").join("b.txt");
    assert!(
        copy_first.exists(),
        "源已不在的条目，副本是唯一的一份，不能删"
    );
    assert!(!copy_second.exists(), "源还在的条目副本应当被放弃");
    assert_eq!(fs::read_to_string(&second).unwrap(), "第二个文件的内容");

    // 唯一剩下的那份副本还能原样还原。
    let restored = engine.restore(manifest.batch_id, &mut journal).unwrap();
    assert_eq!(restored.stage, BatchStage::Restored);
    assert_eq!(fs::read_to_string(&first).unwrap(), "第一个文件的内容");
}

#[test]
fn restore_never_overwrites_or_follows_junction() {
    let temp = TempDir::new().unwrap();
    let engine =
        QuarantineRoot::for_test(temp.path().join("quarantine")).with_free_space(|_| 64 * GIB);
    let mut journal = NullJournal;

    // 情形一：还原前原位已经有同名文件。
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).unwrap();
    let file = source_dir.join("a.txt");
    fs::write(&file, "隔离前的内容").unwrap();

    let outcome = engine
        .quarantine(
            "test-rule",
            &Forbidden::empty(),
            std::slice::from_ref(&source_dir),
            &[QuarantineItem::new(0, &["a.txt"])],
            &mut journal,
            &AtomicBool::new(false),
        )
        .unwrap();

    fs::write(&file, "用户后来新建的文件").unwrap();

    let restored = engine.restore(outcome.batch_id, &mut journal).unwrap();
    assert_eq!(restored.restored, 0, "原位已有同名文件时不能还原");
    assert_eq!(restored.conflicts.len(), 1);
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "用户后来新建的文件",
        "绝不能覆盖用户已有文件"
    );
    let copy = engine
        .path()
        .join(outcome.batch_id.to_string())
        .join("data")
        .join("0")
        .join("a.txt");
    assert!(copy.exists(), "冲突时副本必须保留");

    // 情形二：原父目录被换成指向别处的 junction。
    let other = temp.path().join("other-source");
    fs::create_dir_all(other.join("sub")).unwrap();
    fs::write(other.join("sub").join("b.txt"), "子目录里的内容").unwrap();

    let second = engine
        .quarantine(
            "test-rule",
            &Forbidden::empty(),
            std::slice::from_ref(&other),
            &[QuarantineItem::new(0, &["sub", "b.txt"])],
            &mut journal,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(second.moved, 1);

    // 把原父目录换成 junction，目标在别处。
    fs::remove_dir(other.join("sub")).unwrap();
    let target = temp.path().join("junction-target");
    fs::create_dir(&target).unwrap();
    let status = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(other.join("sub"))
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success(), "创建 junction 失败");

    let after = engine.restore(second.batch_id, &mut journal).unwrap();
    assert_eq!(after.restored, 0, "路径上出现链接时必须拒绝还原");
    assert!(
        fs::read_dir(&target).unwrap().next().is_none(),
        "junction 目标目录里不能出现任何新文件"
    );
    let copy_b = engine
        .path()
        .join(second.batch_id.to_string())
        .join("data")
        .join("0")
        .join("sub")
        .join("b.txt");
    assert!(copy_b.exists(), "被拒绝时副本必须保留");
}

#[test]
fn skips_item_changed_after_freeze() {
    let temp = TempDir::new().unwrap();
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).unwrap();
    let file = source_dir.join("a.txt");
    fs::write(&file, "原始内容").unwrap();

    // 按预览时的元数据冻结条目。
    let forbidden = Forbidden::empty();
    let anchor = open_anchor(&source_dir, &forbidden).unwrap();
    let entry = open_rel(
        &anchor,
        &[OsString::from("a.txt")],
        OpenIntent::Inspect,
        &forbidden,
    )
    .unwrap();
    let item =
        QuarantineItem::new(0, &["a.txt"]).with_frozen(entry.identity, entry.size, entry.mtime);
    drop(entry);
    drop(anchor);

    // 预览之后把文件整个换掉：同名但已经是另一个文件。
    fs::remove_file(&file).unwrap();
    fs::write(&file, "换掉之后的新内容，长度也不一样").unwrap();

    let engine =
        QuarantineRoot::for_test(temp.path().join("quarantine")).with_free_space(|_| 64 * GIB);
    let mut journal = NullJournal;

    let error = engine
        .quarantine(
            "test-rule",
            &forbidden,
            std::slice::from_ref(&source_dir),
            &[item],
            &mut journal,
            &AtomicBool::new(false),
        )
        .unwrap_err();

    assert!(
        matches!(error, QuarantineError::NothingToDo),
        "预览之后变化的条目应当被跳过，实际：{error}"
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "换掉之后的新内容，长度也不一样",
        "被跳过的条目原样保留"
    );
}

#[test]
fn rejects_tampered_manifest() {
    let temp = TempDir::new().unwrap();
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).unwrap();
    let file = source_dir.join("a.txt");
    fs::write(&file, "要还原的内容").unwrap();

    let engine =
        QuarantineRoot::for_test(temp.path().join("quarantine")).with_free_space(|_| 64 * GIB);
    let mut journal = NullJournal;

    let outcome = engine
        .quarantine(
            "test-rule",
            &Forbidden::empty(),
            std::slice::from_ref(&source_dir),
            &[QuarantineItem::new(0, &["a.txt"])],
            &mut journal,
            &AtomicBool::new(false),
        )
        .unwrap();

    let batch_dir = only_batch_dir(engine.path());
    let copy_path = Manifest::load(&batch_dir).unwrap().items[0].copy_path(&batch_dir);

    // rel 里塞一个 `..`：整批拒绝，一个文件都不动。
    let mut manifest = Manifest::load(&batch_dir).unwrap();
    manifest.items[0].rel = vec!["..".to_string(), "evil.txt".to_string()];
    manifest.save(&batch_dir).unwrap();

    let error = engine.restore(outcome.batch_id, &mut journal).unwrap_err();
    assert!(
        matches!(error, QuarantineError::ManifestRejected),
        "清单被改过时还原必须整批拒绝，实际：{error}"
    );
    assert!(!file.exists(), "被拒绝时源位置不能出现任何文件");
    assert!(copy_path.exists(), "被拒绝时副本必须保留");

    let error = engine.purge(outcome.batch_id, &mut journal).unwrap_err();
    assert!(
        matches!(error, QuarantineError::ManifestRejected),
        "清单被改过时彻底删除必须整批拒绝，实际：{error}"
    );
    assert!(batch_dir.exists(), "被拒绝时批次目录不能被删除");

    // anchor 换成 UNC 路径，同样整批拒绝。
    let mut manifest = Manifest::load(&batch_dir).unwrap();
    manifest.items[0].anchor = r"\\server\share".to_string();
    manifest.save(&batch_dir).unwrap();
    assert!(matches!(
        engine.restore(outcome.batch_id, &mut journal).unwrap_err(),
        QuarantineError::ManifestRejected
    ));

    // helper 的白名单校验：规则不在白名单、或者锚点在基目录之外都拒绝。
    let program_data = temp.path().join("programdata");
    let roots = RootMap::with(vec![(KnownRoot::ProgramData, program_data)]);
    let base = roots
        .get(KnownRoot::ProgramData)
        .unwrap()
        .join(r"Lenovo\LeAppStore\cicada");

    let mut admin = Manifest::load(&batch_dir).unwrap();
    admin.requires_admin = true;
    admin.rule_id = "adapter-leappstore-cicada".to_string();
    admin.items[0].anchor = base.to_string_lossy().into_owned();
    admin.items[0].rel = vec!["a.txt".to_string()];
    assert!(
        validate_admin_batch(&admin, &roots).is_ok(),
        "落在基目录里的合法批次应当通过校验"
    );

    admin.rule_id = "adapter-codex-previous-runtime".to_string();
    assert!(
        validate_admin_batch(&admin, &roots).is_err(),
        "白名单之外的规则必须拒绝"
    );

    admin.rule_id = "adapter-leappstore-cicada".to_string();
    admin.items[0].anchor = temp.path().join("elsewhere").to_string_lossy().into_owned();
    assert!(
        validate_admin_batch(&admin, &roots).is_err(),
        "锚点不在基目录下必须拒绝"
    );
}
