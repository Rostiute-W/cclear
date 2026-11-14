//! junction 目录迁移的集成测试。全部只在 `tempdir()` 创建的目录里操作。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use cclear_core::exec::executor;
use cclear_core::guard::{self, Forbidden};
use cclear_core::journal::{JournalSink, Record};
use cclear_core::migrate::junction::{
    BACKUP_RULE_ID, DirMigration, DirStage, JunctionError, JunctionMigration,
};
use cclear_core::model::KnownRoot;
use cclear_core::plan::FrozenPlan;
use cclear_core::rules::resolve::RootMap;
use cclear_core::win::dirinfo;
use tempfile::TempDir;

/// 测试里选中的“目标卷”。目标根目录由 `for_test` 注入，这个值只用来占位。
const TEST_VOLUME: &str = r"C:\";

/// 只把记录留在内存里的日志出口，测试不需要真的写文件。
#[derive(Default)]
struct MemoryJournal {
    records: Vec<Record>,
}

impl JournalSink for MemoryJournal {
    fn append(&mut self, record: &Record) -> io::Result<()> {
        self.records.push(record.clone());
        Ok(())
    }
}

/// 临时目录里的一套用户目录结构。
struct Fixture {
    temp: TempDir,
    profile: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let profile = temp.path().join("profile");
        let local = profile.join("AppData").join("Local");

        // 资格检查会用到这几个根目录，先把它们建出来。
        for path in [
            &local,
            &profile.join("AppData").join("Roaming"),
            &profile.join("Documents"),
            &profile.join("Downloads"),
            &local.join("Packages"),
        ] {
            fs::create_dir_all(path).unwrap();
        }

        Self { temp, profile }
    }

    /// 把 Known 根目录映射到临时目录，模拟真实的用户目录布局。
    fn roots(&self) -> RootMap {
        RootMap::with(vec![
            (KnownRoot::Profile, self.profile.clone()),
            (
                KnownRoot::LocalAppData,
                self.profile.join("AppData").join("Local"),
            ),
            (
                KnownRoot::RoamingAppData,
                self.profile.join("AppData").join("Roaming"),
            ),
            (KnownRoot::Documents, self.profile.join("Documents")),
            (KnownRoot::Downloads, self.profile.join("Downloads")),
        ])
    }

    fn migration(&self) -> JunctionMigration {
        JunctionMigration::for_test(self.roots(), self.temp.path().to_path_buf())
    }

    /// 建一个待迁移的纯数据目录。
    fn data_dir(&self) -> PathBuf {
        let source = self.profile.join("data");
        fs::create_dir_all(source.join("sub")).unwrap();
        fs::write(source.join("a.txt"), "a").unwrap();
        fs::write(source.join("sub").join("b.txt"), "b").unwrap();

        source
    }
}

/// 用 `cmd /c mklink /J` 建 junction。测试代码只保证操作临时目录。
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

/// 只删掉 junction 本身。`rmdir` 不会跟着链接删目标里的内容。
fn remove_junction_link(link: &Path) {
    let output = Command::new("cmd")
        .args(["/c", "rmdir"])
        .arg(link)
        .output()
        .expect("启动 cmd 失败");

    assert!(
        output.status.success(),
        "删除 junction 失败：{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// 递归复制目录内容，模拟“迁回已经复制完、还没改名”的那一刻。
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();

    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// 把迁移记录改成指定阶段，模拟进程在那一刻被结束。
fn write_stage(fixture: &Fixture, record: &DirMigration, stage: &str, note: &str) {
    let store = serde_json::json!({
        "directories": [{
            "id": record.id,
            "source": record.source,
            "target": record.target,
            "backup": record.backup,
            "stage": stage,
            "note": note,
            "verified": record.verified,
            "updatedAt": record.updated_at,
        }],
    });

    let dir = fixture.temp.path().join("store");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("migrations.json"),
        serde_json::to_string_pretty(&store).unwrap(),
    )
    .unwrap();
}

/// 原路径现在是 junction，并且指向 `expect`。
fn assert_points_to(path: &Path, expect: &Path) {
    let parent = path.parent().unwrap();
    let name = path.file_name().unwrap();
    let anchor = guard::open_anchor(parent, &Forbidden::empty()).unwrap();
    let target = guard::read_junction_target(&anchor, name).expect("原路径应当是一个 junction");

    assert!(
        cclear_core::win::equals_ignore_case(&target.to_string_lossy(), &expect.to_string_lossy()),
        "junction 指向 {}，期望 {}",
        target.display(),
        expect.display()
    );
}

/// 按冻结清单执行一次删除。备份目录的清理走这条路。
fn run_plan(plan: &FrozenPlan) {
    let mut journal = MemoryJournal::default();
    let cancel = AtomicBool::new(false);

    let report = executor::execute(
        plan,
        &[BACKUP_RULE_ID],
        &Forbidden::empty(),
        &Vec::new,
        &mut journal,
        &cancel,
        &|_| {},
    );

    assert!(report.aborted.is_none(), "执行被中止：{:?}", report.aborted);
    assert_eq!(
        report.skipped_total, 0,
        "清单里的条目应当全部删掉，实际跳过 {}",
        report.skipped_total
    );
}

#[test]
fn migrates_directory_into_junction() {
    let fixture = Fixture::new();
    let source = fixture.data_dir();
    let migration = fixture.migration();
    let mut journal = MemoryJournal::default();

    let record = migration.start(&source, TEST_VOLUME, &mut journal).unwrap();
    assert_eq!(record.stage, DirStage::AwaitingUserTest);

    // 目标里的内容和源一致。
    let target = record.target_path();
    assert_eq!(fs::read_to_string(target.join("a.txt")).unwrap(), "a");
    assert_eq!(
        fs::read_to_string(target.join("sub").join("b.txt")).unwrap(),
        "b"
    );

    // 原路径变成 junction，经它能读到内容。
    assert_points_to(&source, &target);
    assert_eq!(fs::read_to_string(source.join("a.txt")).unwrap(), "a");
    assert!(
        !dirinfo::is_plain_dir(&source),
        "原路径应当已经是链接，不再是普通目录"
    );

    // 源目录已经改名成备份。
    let backup = record.backup_path();
    assert!(backup.exists(), "备份目录应当还在");

    // 确认删除备份：走冻结清单，备份目录最后应当消失。
    let plan = migration.backup_plan(&record.id).unwrap();
    run_plan(&plan);
    migration.finish_backup(&record.id, &mut journal).unwrap();

    assert!(!backup.exists(), "确认删除之后备份目录应当消失");
    assert_eq!(migration.get(&record.id).unwrap().stage, DirStage::Done);
}

#[test]
fn returns_latest_data_from_target() {
    let fixture = Fixture::new();
    let source = fixture.data_dir();
    let migration = fixture.migration();
    let mut journal = MemoryJournal::default();

    let record = migration.start(&source, TEST_VOLUME, &mut journal).unwrap();

    // 迁移之后经 junction 写入新文件，它会落到目标卷上。
    fs::write(source.join("new.txt"), "new").unwrap();
    assert!(
        record.target_path().join("new.txt").exists(),
        "新文件应当写进了目标目录"
    );

    // 迁回以目标里的最新数据为准。
    let returned = migration.return_home(&record.id, &mut journal).unwrap();
    assert_eq!(returned.stage, DirStage::Returned);

    assert!(
        dirinfo::is_plain_dir(&source),
        "迁回之后原路径应当是普通目录"
    );
    assert_eq!(fs::read_to_string(source.join("new.txt")).unwrap(), "new");
    assert_eq!(fs::read_to_string(source.join("a.txt")).unwrap(), "a");
}

#[test]
fn rejects_ineligible_directories() {
    let fixture = Fixture::new();
    let migration = fixture.migration();

    // 含程序文件。
    let with_exe = fixture.profile.join("with-exe");
    fs::create_dir_all(&with_exe).unwrap();
    fs::write(with_exe.join("app.exe"), "MZ").unwrap();
    let reasons = migration.check_eligibility(&with_exe);
    assert!(
        reasons.iter().any(|reason| reason.contains(".exe")),
        "含 .exe 的目录应当被拒绝，实际：{reasons:?}"
    );

    // 含 junction。
    let with_link = fixture.profile.join("with-link");
    let outside = fixture.temp.path().join("outside");
    fs::create_dir_all(&with_link).unwrap();
    fs::create_dir_all(&outside).unwrap();
    create_junction(&with_link.join("link"), &outside);
    let reasons = migration.check_eligibility(&with_link);
    assert!(
        reasons.iter().any(|reason| reason.contains("链接")),
        "含 junction 的目录应当被拒绝，实际：{reasons:?}"
    );

    // 位于 AppData\Local\Packages 之下。
    let packages = fixture
        .profile
        .join("AppData")
        .join("Local")
        .join("Packages")
        .join("app");
    fs::create_dir_all(&packages).unwrap();
    let reasons = migration.check_eligibility(&packages);
    assert!(
        reasons.iter().any(|reason| reason.contains("Packages")),
        "Packages 下的目录应当被拒绝，实际：{reasons:?}"
    );
}

#[test]
fn recovers_interrupted_rename() {
    let fixture = Fixture::new();
    let source = fixture.data_dir();

    // 在改名之后中断，模拟进程被强行结束。
    let migration = fixture.migration().with_stop_at(DirStage::SourceRenamed);
    let mut journal = MemoryJournal::default();

    let error = migration
        .start(&source, TEST_VOLUME, &mut journal)
        .unwrap_err();
    assert!(
        matches!(error, JunctionError::Interrupted),
        "应当停在注入的断点上，实际：{error:?}"
    );

    let record = migration.get(&migration.list().unwrap()[0].id).unwrap();
    assert_eq!(record.stage, DirStage::SourceRenamed);
    assert!(!source.exists(), "源目录这时已经改名成备份");

    // 恢复：把源改回原名，并删掉目标副本。
    let recovered = migration.recover_pending(&mut journal).unwrap();
    assert!(recovered.failures.is_empty(), "恢复不该失败");
    assert_eq!(recovered.recovered.len(), 1);
    assert_eq!(recovered.recovered[0].stage, DirStage::Failed);

    assert_eq!(fs::read_to_string(source.join("a.txt")).unwrap(), "a");
    assert!(
        !record.target_path().exists(),
        "目标副本应当已经删除：{}",
        record.target_path().display()
    );
    assert!(!record.backup_path().exists(), "备份目录应当已经改回原名");
}

#[test]
fn returns_after_backup_deleted() {
    let fixture = Fixture::new();
    let source = fixture.data_dir();
    let migration = fixture.migration();
    let mut journal = MemoryJournal::default();

    // 走完整条迁移，把备份也删掉，状态变成完成。
    let record = migration.start(&source, TEST_VOLUME, &mut journal).unwrap();
    let plan = migration.backup_plan(&record.id).unwrap();
    run_plan(&plan);
    let done = migration.finish_backup(&record.id, &mut journal).unwrap();
    assert_eq!(done.stage, DirStage::Done);
    assert!(!record.backup_path().exists());

    // 备份删掉之后链接仍然是活的，迁回照样要能用。
    let returned = migration.return_home(&record.id, &mut journal).unwrap();
    assert_eq!(returned.stage, DirStage::Returned);

    assert!(
        dirinfo::is_plain_dir(&source),
        "迁回之后原位置应当是普通目录"
    );
    assert_eq!(fs::read_to_string(source.join("a.txt")).unwrap(), "a");
    assert_eq!(
        fs::read_to_string(source.join("sub").join("b.txt")).unwrap(),
        "b"
    );
}

#[test]
fn recovers_interrupted_verified_and_returning() {
    let fixture = Fixture::new();
    let mut journal = MemoryJournal::default();

    // 一、停在“复制完成并校验”：目标副本已经写好，源目录还没动。
    let source = fixture.data_dir();
    let stopping = fixture.migration().with_stop_at(DirStage::Verified);
    let error = stopping
        .start(&source, TEST_VOLUME, &mut journal)
        .unwrap_err();
    assert!(
        matches!(error, JunctionError::Interrupted),
        "应当停在注入的断点上，实际：{error:?}"
    );

    let verified = stopping.get(&stopping.list().unwrap()[0].id).unwrap();
    assert_eq!(verified.stage, DirStage::Verified);
    assert!(verified.target_path().exists(), "这时目标副本已经写好");

    // 重新打开之后按“复制阶段中断”处理：删掉目标副本，源目录完好，可以重新迁移。
    let reopened = fixture.migration();
    let recovered = reopened.recover_pending(&mut journal).unwrap();
    assert!(recovered.failures.is_empty(), "恢复不该失败");
    assert_eq!(recovered.recovered.len(), 1);
    assert_eq!(recovered.recovered[0].stage, DirStage::Failed);
    assert_eq!(fs::read_to_string(source.join("a.txt")).unwrap(), "a");
    assert!(
        !verified.target_path().exists(),
        "目标副本应当已经删除：{}",
        verified.target_path().display()
    );

    // 二、停在迁回开始：链接还在，只是“正在迁回”已经落盘。
    let second = fixture.profile.join("data2");
    fs::create_dir_all(&second).unwrap();
    fs::write(second.join("c.txt"), "c").unwrap();

    let started = reopened.start(&second, TEST_VOLUME, &mut journal).unwrap();
    assert_eq!(started.stage, DirStage::AwaitingUserTest);

    let returning = fixture.migration().with_stop_at(DirStage::Returning);
    let error = returning
        .return_home(&started.id, &mut journal)
        .unwrap_err();
    assert!(
        matches!(error, JunctionError::Interrupted),
        "应当停在注入的断点上，实际：{error:?}"
    );
    assert_eq!(
        fixture.migration().get(&started.id).unwrap().stage,
        DirStage::Returning
    );

    // 重新打开：原位置还是 junction，删掉临时目录就回到迁回之前的状态。
    let recovered = fixture.migration().recover_pending(&mut journal).unwrap();
    assert!(recovered.failures.is_empty(), "恢复不该失败");
    assert_eq!(recovered.recovered.len(), 1);
    assert_eq!(recovered.recovered[0].id, started.id);
    assert_eq!(recovered.recovered[0].stage, DirStage::AwaitingUserTest);

    assert_points_to(&second, &started.target_path());
    assert_eq!(fs::read_to_string(second.join("c.txt")).unwrap(), "c");

    // 三、停在“链接已删、临时目录完整”：恢复要把临时目录改名回原名，记为已迁回。
    let third = fixture.profile.join("data3");
    fs::create_dir_all(third.join("sub")).unwrap();
    fs::write(third.join("d.txt"), "d").unwrap();
    fs::write(third.join("sub").join("e.txt"), "e").unwrap();

    let started = fixture
        .migration()
        .start(&third, TEST_VOLUME, &mut journal)
        .unwrap();
    let temp = started.return_temp_path();
    copy_tree(&started.target_path(), &temp);

    // 删掉链接本身（`rmdir` 只删 junction，不碰目标里的数据）。
    remove_junction_link(&third);
    assert!(!third.exists(), "原位置这时是空的");
    assert!(dirinfo::is_plain_dir(&temp), "临时目录里还有子目录");

    write_stage(
        &fixture,
        &started,
        "returning",
        "测试注入：复制完成、改名之前被中断",
    );

    let recovered = fixture.migration().recover_pending(&mut journal).unwrap();
    assert!(recovered.failures.is_empty(), "恢复不该失败");
    assert_eq!(recovered.recovered.len(), 1);
    assert_eq!(recovered.recovered[0].stage, DirStage::Returned);
    assert!(
        dirinfo::is_plain_dir(&third),
        "恢复应当把临时目录改名回原名"
    );
    assert_eq!(fs::read_to_string(third.join("d.txt")).unwrap(), "d");
    assert_eq!(
        fs::read_to_string(third.join("sub").join("e.txt")).unwrap(),
        "e",
        "含子目录的临时目录也应当被认作完整"
    );
    assert!(!temp.exists(), "改名之后临时目录应当不在了");
}

#[test]
fn preserves_empty_directories() {
    let fixture = Fixture::new();
    let migration = fixture.migration();
    let mut journal = MemoryJournal::default();

    // 源里有多层空目录，还有一个普通文件。
    let source = fixture.profile.join("data-empty");
    fs::create_dir_all(source.join("empty").join("nested")).unwrap();
    fs::create_dir_all(source.join("other")).unwrap();
    fs::write(source.join("keep.txt"), "k").unwrap();

    let record = migration.start(&source, TEST_VOLUME, &mut journal).unwrap();
    let target = record.target_path();
    assert!(
        target.join("empty").join("nested").is_dir(),
        "迁移之后目标里也要有空目录"
    );

    // 删备份，再迁回。
    let plan = migration.backup_plan(&record.id).unwrap();
    run_plan(&plan);
    migration.finish_backup(&record.id, &mut journal).unwrap();

    let returned = migration.return_home(&record.id, &mut journal).unwrap();
    assert_eq!(returned.stage, DirStage::Returned);

    assert!(
        dirinfo::is_plain_dir(&source.join("empty").join("nested")),
        "迁回之后多层空目录都应当还在"
    );
    assert!(dirinfo::is_plain_dir(&source.join("other")));
    assert_eq!(fs::read_to_string(source.join("keep.txt")).unwrap(), "k");
}

#[test]
fn return_copy_failure_cleans_temp_dir() {
    let fixture = Fixture::new();
    let source = fixture.data_dir();
    let migration = fixture.migration();
    let mut journal = MemoryJournal::default();

    let record = migration.start(&source, TEST_VOLUME, &mut journal).unwrap();
    let target = record.target_path();

    // 迁回临时目录里先放一个同名文件：复制到它时必然失败。
    let returning = source.with_file_name(format!("data.cclear-return-{}", record.id));
    fs::create_dir_all(&returning).unwrap();
    fs::write(returning.join("a.txt"), "占位").unwrap();

    let error = migration.return_home(&record.id, &mut journal).unwrap_err();
    assert!(
        matches!(error, JunctionError::Copy(_)),
        "应当是复制失败，实际：{error:?}"
    );

    // 临时目录被删掉，链接和目标数据都没动，状态回到迁回之前。
    assert!(!returning.exists(), "临时目录应当被删除");
    assert_points_to(&source, &target);
    assert_eq!(fs::read_to_string(source.join("a.txt")).unwrap(), "a");
    assert_eq!(fs::read_to_string(target.join("a.txt")).unwrap(), "a");

    let after = migration.get(&record.id).unwrap();
    assert_eq!(after.stage, DirStage::AwaitingUserTest);
    assert!(
        after.note.contains("迁回"),
        "说明里要写清迁回没有完成：{}",
        after.note
    );
}
