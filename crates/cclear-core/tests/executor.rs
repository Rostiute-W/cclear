//! 冻结清单与执行器的集成测试。全部只操作 `tempdir()` 创建的目录。

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use cclear_core::exec::executor::{ExecProgress, ExecReport, execute};
use cclear_core::guard::Forbidden;
use cclear_core::journal::{JournalSink, Record};
use cclear_core::model::{Category, KnownRoot, Phase, Recovery, Tier};
use cclear_core::plan::{FrozenPlan, build_plan};
use cclear_core::rules::catalog::{Rule, SelectKind, Selector, Target};
use cclear_core::rules::resolve::RootMap;
use tempfile::TempDir;

/// 不含任何禁区的护栏集合。
fn no_forbidden() -> Forbidden {
    Forbidden {
        ids: HashSet::new(),
        names: Vec::new(),
    }
}

/// 把 LocalAppData 映射到临时目录，规则锚点就是它下面的 `anchor`。
fn roots_for(temp: &TempDir) -> RootMap {
    let local = temp.path().join("local");
    fs::create_dir_all(&local).unwrap();

    RootMap::with(vec![(KnownRoot::LocalAppData, local)])
}

/// 一条锚点为 `anchor`、递归清理全部内容的测试规则。
fn test_rule() -> Rule {
    Rule {
        id: "test-plan",
        name: "测试清理",
        description: "测试用",
        category: Category::System,
        tier: Tier::Caution,
        recovery: Recovery::IrreversibleFile,
        phase: Phase::M3b,
        admin: false,
        default_selected: false,
        processes: &[],
        layout_markers: &[],
        warning: None,
        target: Target::Paths {
            root: KnownRoot::LocalAppData,
            rels: &["anchor"],
            selector: Selector {
                kind: SelectKind::AllContents,
                min_age_hours: None,
                exclude_newest_per_dir: false,
                exclude_child_dirs: &[],
            },
        },
    }
}

/// 只用内存记录日志，正常路径下不会失败。
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

/// 第 `fail_at` 次 append 起报错，用来验证“日志写失败就停手”。
struct FailingJournal {
    appends: usize,
    fail_at: usize,
}

impl JournalSink for FailingJournal {
    fn append(&mut self, _record: &Record) -> io::Result<()> {
        self.appends += 1;
        if self.appends >= self.fail_at {
            return Err(io::Error::other("注入的日志写入失败"));
        }

        Ok(())
    }
}

/// 用内存日志执行整份清单。
fn run(plan: &FrozenPlan, selected: &[&str], forbidden: &Forbidden) -> ExecReport {
    let mut journal = MemoryJournal::default();

    run_with(plan, selected, forbidden, &mut journal)
}

fn run_with(
    plan: &FrozenPlan,
    selected: &[&str],
    forbidden: &Forbidden,
    journal: &mut dyn JournalSink,
) -> ExecReport {
    let cancel = AtomicBool::new(false);

    execute(
        plan,
        selected,
        forbidden,
        &|| Vec::new(),
        journal,
        &cancel,
        &|_progress: ExecProgress| {},
    )
}

/// 用 `cmd /c mklink /J` 建 junction。测试只对临时目录操作。
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

#[test]
fn deletes_planned_items_only() {
    let temp = TempDir::new().unwrap();
    let roots = roots_for(&temp);
    let anchor = roots.get(KnownRoot::LocalAppData).unwrap().join("anchor");
    fs::create_dir_all(anchor.join("sub")).unwrap();
    fs::write(anchor.join("a.txt"), "a").unwrap();
    fs::write(anchor.join("sub").join("b.txt"), "b").unwrap();

    // 锚点外的哨兵：清单和护栏都不能碰到它。
    let sentinel = temp.path().join("sentinel.txt");
    fs::write(&sentinel, "keep").unwrap();

    let rule = test_rule();
    let forbidden = no_forbidden();
    let plan = build_plan(&[&rule], &roots, &forbidden, &[], SystemTime::now()).unwrap();
    let report = run(&plan, &[rule.id], &forbidden);

    assert!(report.aborted.is_none(), "正常执行不应当中止");
    assert!(!anchor.join("a.txt").exists(), "清单内的文件应当被删除");
    assert!(
        !anchor.join("sub").join("b.txt").exists(),
        "子目录里的文件应当被删除"
    );
    assert!(!anchor.join("sub").exists(), "变空的子目录应当被删除");
    assert!(anchor.exists(), "锚点自己不能进清单");
    assert!(sentinel.exists(), "锚点外的哨兵不能被动到");

    assert_eq!(report.rules[0].deleted_files, 2);
    assert_eq!(report.rules[0].deleted_dirs, 1);
    assert_eq!(report.rules[0].skipped, 0);
}

#[test]
fn skips_changed_file_and_keeps_dir_with_unplanned_file() {
    let temp = TempDir::new().unwrap();
    let roots = roots_for(&temp);
    let anchor = roots.get(KnownRoot::LocalAppData).unwrap().join("anchor");
    fs::create_dir_all(anchor.join("dir")).unwrap();
    fs::write(anchor.join("a.txt"), "a").unwrap();
    fs::write(anchor.join("b.txt"), "b").unwrap();
    fs::write(anchor.join("dir").join("c.txt"), "c").unwrap();

    let rule = test_rule();
    let forbidden = no_forbidden();
    let plan = build_plan(&[&rule], &roots, &forbidden, &[], SystemTime::now()).unwrap();

    // 预览之后改掉 a.txt：大小变了，复核时必须跳过。
    fs::write(anchor.join("a.txt"), "changed-and-longer").unwrap();
    // 预览之后往 dir 里新增一个计划外文件：dir 不能删。
    fs::write(anchor.join("dir").join("new.txt"), "new").unwrap();

    let report = run(&plan, &[rule.id], &forbidden);

    assert!(anchor.join("a.txt").exists(), "变化过的文件必须跳过");
    assert!(!anchor.join("b.txt").exists(), "没变化的文件照常删除");
    assert!(
        !anchor.join("dir").join("c.txt").exists(),
        "计划内的文件照常删除"
    );
    assert!(
        anchor.join("dir").exists(),
        "还有计划外文件的目录不能被删除"
    );
    assert!(anchor.join("dir").join("new.txt").exists());
    assert!(
        report
            .skipped
            .iter()
            .any(|item| item.reason == "目录内有未计划的新文件已保留"),
        "目录非空的原因应当是“目录内有未计划的新文件已保留”，实际：{:?}",
        report
            .skipped
            .iter()
            .map(|item| &item.reason)
            .collect::<Vec<_>>()
    );
}

#[test]
fn stops_at_once_when_journal_fails() {
    let temp = TempDir::new().unwrap();
    let roots = roots_for(&temp);
    let anchor = roots.get(KnownRoot::LocalAppData).unwrap().join("anchor");
    fs::create_dir_all(&anchor).unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(anchor.join(name), name).unwrap();
    }

    let rule = test_rule();
    let forbidden = no_forbidden();
    let plan = build_plan(&[&rule], &roots, &forbidden, &[], SystemTime::now()).unwrap();

    // 第 1 次是 PlanStarted，第 2 次是第一个条目的 ItemDone，就在这里失败。
    let mut journal = FailingJournal {
        appends: 0,
        fail_at: 2,
    };
    let report = run_with(&plan, &[rule.id], &forbidden, &mut journal);

    assert_eq!(journal.appends, 2, "写失败之后不能再写日志");
    assert_eq!(
        report.aborted.as_deref(),
        Some("日志写入失败，已停止"),
        "日志写失败必须立刻中止"
    );

    let left = ["a.txt", "b.txt", "c.txt"]
        .iter()
        .filter(|name| anchor.join(name).exists())
        .count();
    assert_eq!(left, 2, "只处理了第一个条目，其余文件必须还在");
}

#[test]
fn skips_junction_inside_anchor() {
    let temp = TempDir::new().unwrap();
    let roots = roots_for(&temp);
    let anchor = roots.get(KnownRoot::LocalAppData).unwrap().join("anchor");
    fs::create_dir_all(&anchor).unwrap();
    fs::write(anchor.join("keep.txt"), "keep").unwrap();

    // 锚点外的真实目录，junction 指向它。
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("data.txt"), "outside").unwrap();
    create_junction(&anchor.join("link"), &outside);

    let rule = test_rule();
    let forbidden = no_forbidden();
    let plan = build_plan(&[&rule], &roots, &forbidden, &[], SystemTime::now()).unwrap();

    assert!(
        plan.rules[0]
            .skipped_at_build
            .iter()
            .any(|(path, reason)| path.ends_with("link") && reason == "链接，已跳过"),
        "构建清单时应当跳过 junction 并记录原因"
    );

    let report = run(&plan, &[rule.id], &forbidden);

    assert!(report.aborted.is_none());
    assert!(!anchor.join("keep.txt").exists(), "普通文件照常删除");
    assert!(anchor.exists(), "锚点自己不能被删除");
    assert_eq!(
        fs::read_to_string(outside.join("data.txt")).unwrap(),
        "outside",
        "junction 指向的外部目录内容不能变"
    );
    assert!(outside.join("data.txt").exists());
}
