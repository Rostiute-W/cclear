//! 隔离区事务的集成测试。全部只操作 `tempdir()` 创建的目录。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use cclear_core::guard::Forbidden;
use cclear_core::journal::{JournalSink, Record};
use cclear_core::quarantine::{
    BatchStage, Manifest, QuarantineError, QuarantineItem, QuarantineRoot,
};
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
