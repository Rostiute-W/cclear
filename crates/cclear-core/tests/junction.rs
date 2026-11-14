//! junction 目录迁移的集成测试。全部只在 `tempdir()` 创建的目录里操作。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use cclear_core::exec::executor;
use cclear_core::guard::{self, Forbidden};
use cclear_core::journal::{JournalSink, Record};
use cclear_core::migrate::junction::{BACKUP_RULE_ID, DirStage, JunctionError, JunctionMigration};
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
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].stage, DirStage::Failed);

    assert_eq!(fs::read_to_string(source.join("a.txt")).unwrap(), "a");
    assert!(
        !record.target_path().exists(),
        "目标副本应当已经删除：{}",
        record.target_path().display()
    );
    assert!(!record.backup_path().exists(), "备份目录应当已经改回原名");
}
