//! 规则目录与候选解析的集成测试。所有路径映射到 tempdir，不碰真实用户数据。

use std::collections::HashSet;
use std::fs;
use std::fs::FileTimes;
use std::os::windows::fs::FileTimesExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use cclear_core::model::{Category, KnownRoot, Phase, Recovery, Tier};
use cclear_core::rules::catalog::{Rule, SelectKind, Selector, Target, catalog};
use cclear_core::rules::forbidden::is_forbidden;
use cclear_core::rules::resolve::{RootMap, evaluate, resolve_anchors};
use tempfile::tempdir;

/// 所有已知根目录，每个都映射到独立的临时子目录。
const ROOTS: &[KnownRoot] = &[
    KnownRoot::LocalAppData,
    KnownRoot::RoamingAppData,
    KnownRoot::Profile,
    KnownRoot::Documents,
    KnownRoot::Downloads,
    KnownRoot::ProgramData,
    KnownRoot::Windows,
    KnownRoot::SystemDrive,
];

fn temp_roots(base: &Path) -> RootMap {
    let mut entries = Vec::new();
    for root in ROOTS {
        let dir = base.join(format!("{root:?}"));
        fs::create_dir_all(&dir).unwrap();
        entries.push((*root, dir));
    }
    RootMap::with(entries)
}

/// 建一个指定大小、指定年龄（小时）的文件。
fn write_aged(path: &Path, size: u64, age_hours: u64) {
    let file = fs::File::create(path).unwrap();
    file.set_len(size).unwrap();

    let when = SystemTime::now() - Duration::from_secs(age_hours * 3600);
    file.set_times(
        FileTimes::new()
            .set_accessed(when)
            .set_modified(when)
            .set_created(when),
    )
    .unwrap();
}

#[test]
fn catalog_ids_unique_and_executable_anchors_outside_forbidden() {
    let rules = catalog();

    let mut ids = HashSet::new();
    for rule in rules {
        assert!(ids.insert(rule.id), "规则 id 重复：{}", rule.id);
    }

    // manual 行只做展示：不能带恢复方式，也不能挂到执行阶段上。
    for rule in rules.iter().filter(|rule| rule.tier == Tier::Manual) {
        assert_eq!(
            rule.phase,
            Phase::Analysis,
            "{} 的阶段不是 Analysis",
            rule.id
        );
        assert_eq!(rule.recovery, Recovery::None, "{} 带恢复方式", rule.id);
    }

    let temp = tempdir().unwrap();
    let roots = temp_roots(temp.path());

    // 让不带通配符的锚点真实存在，否则“锚点不在禁区”会变成空断言。
    for rule in rules {
        if !matches!(rule.tier, Tier::Safe | Tier::Caution | Tier::Leftover) {
            continue;
        }

        let Target::Paths { root, rels, .. } = &rule.target else {
            continue;
        };

        let base = roots.get(*root).expect("根目录没有映射");
        for rel in *rels {
            if !rel.contains(['*', '?']) {
                let _ = fs::create_dir_all(base.join(rel));
            }
        }
    }

    for rule in rules {
        if !matches!(rule.tier, Tier::Safe | Tier::Caution | Tier::Leftover) {
            continue;
        }

        for anchor in resolve_anchors(rule, &roots) {
            assert!(
                is_forbidden(&anchor, &roots).is_none(),
                "{} 的锚点落在禁区：{}",
                rule.id,
                anchor.display()
            );
        }
    }
}

#[test]
fn chromium_profiles_come_from_local_state() {
    let temp = tempdir().unwrap();
    let roots = temp_roots(temp.path());
    let user_data = roots
        .get(KnownRoot::LocalAppData)
        .unwrap()
        .join("Microsoft\\Edge\\User Data");
    fs::create_dir_all(&user_data).unwrap();
    fs::write(
        user_data.join("Local State"),
        r#"{"profile":{"info_cache":{"Default":{},"Profile 1":{}}}}"#,
    )
    .unwrap();

    for profile in ["Default", "Profile 1"] {
        for leaf in ["Cache", "Code Cache", "GPUCache"] {
            fs::create_dir_all(user_data.join(profile).join(leaf)).unwrap();
        }
    }

    let rule = catalog()
        .iter()
        .find(|rule| rule.id == "browser-edge-cache")
        .expect("目录里没有 Edge 缓存规则");

    let anchors = resolve_anchors(rule, &roots);
    assert_eq!(anchors.len(), 6, "两个 Profile 的三个叶子目录都要解析出来");
    assert!(anchors.contains(&user_data.join("Profile 1").join("Code Cache")));
    assert!(anchors.contains(&user_data.join("Default").join("GPUCache")));
}

#[test]
fn age_filter_and_newest_file_per_dir() {
    let temp = tempdir().unwrap();
    let roots = temp_roots(temp.path());
    let logs = roots.get(KnownRoot::LocalAppData).unwrap().join("logs");
    let archive = logs.join("archive");
    fs::create_dir_all(&archive).unwrap();

    // 锚点目录：全部超过 24 小时，但最新的 40 小时那个要保留。
    write_aged(&logs.join("a-50h.log"), 100, 50);
    write_aged(&logs.join("b-48h.log"), 200, 48);
    write_aged(&logs.join("c-40h.log"), 300, 40);

    // 子目录：1 小时的文件既不够老，也正好是该目录最新的那个。
    write_aged(&archive.join("d-30h.log"), 150, 30);
    write_aged(&archive.join("e-1h.log"), 50, 1);

    let rule = Rule {
        id: "test-log",
        name: "测试日志",
        description: "测试用",
        category: Category::App,
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
            rels: &["logs"],
            selector: Selector {
                kind: SelectKind::AllContents,
                min_age_hours: Some(24),
                exclude_newest_per_dir: true,
            },
        },
    };

    let candidate = evaluate(&rule, &roots, &[], SystemTime::now());

    // 只剩 a-50h、b-48h、d-30h：c-40h 是锚点目录最新，e-1h 太新。
    assert_eq!(candidate.size.files, 3);
    assert_eq!(candidate.size.logical, 450);
    assert!(!candidate.executable, "ENABLED_PHASES 为空时不能有执行资格");
}

#[test]
fn wildcard_component_expands() {
    let temp = tempdir().unwrap();
    let roots = temp_roots(temp.path());
    let tencent = roots
        .get(KnownRoot::Documents)
        .unwrap()
        .join("Tencent Files");

    let cache_one = tencent.join("123").join("users").join("abc").join("Cache");
    let cache_two = tencent.join("999").join("users").join("def").join("Cache");
    fs::create_dir_all(&cache_one).unwrap();
    fs::create_dir_all(&cache_two).unwrap();
    // 不匹配的目录不能被当成锚点。
    fs::create_dir_all(tencent.join("123").join("users").join("abc").join("Other")).unwrap();

    let rule = Rule {
        id: "test-wildcard",
        name: "测试通配符",
        description: "测试用",
        category: Category::App,
        tier: Tier::Caution,
        recovery: Recovery::Regenerable,
        phase: Phase::M3b,
        admin: false,
        default_selected: false,
        processes: &[],
        layout_markers: &[],
        warning: None,
        target: Target::Paths {
            root: KnownRoot::Documents,
            rels: &["Tencent Files\\*\\users\\*\\Cache"],
            selector: Selector {
                kind: SelectKind::AllContents,
                min_age_hours: None,
                exclude_newest_per_dir: false,
            },
        },
    };

    let mut anchors = resolve_anchors(&rule, &roots);
    anchors.sort();

    let mut expected = vec![cache_one, cache_two];
    expected.sort();
    assert_eq!(anchors, expected);
}
