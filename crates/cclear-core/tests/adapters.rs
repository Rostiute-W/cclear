//! 残留适配器的集成测试。所有路径映射到 tempdir，不碰真实用户数据。

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use cclear_core::model::KnownRoot;
use cclear_core::rules::adapters::{VersionProbe, evaluate_adapter};
use cclear_core::rules::catalog::AdapterId;
use cclear_core::rules::resolve::{CandidateStatus, RootMap};
use tempfile::tempdir;

/// 适配器要用到的已知根目录，每个映射到独立的临时子目录。
const ROOTS: &[KnownRoot] = &[KnownRoot::LocalAppData, KnownRoot::Profile];

fn temp_roots(base: &Path) -> RootMap {
    let mut entries = Vec::new();
    for root in ROOTS {
        let dir = base.join(format!("{root:?}"));
        fs::create_dir_all(&dir).unwrap();
        entries.push((*root, dir));
    }
    RootMap::with(entries)
}

/// 版本探针的测试实现：任何可执行文件都报同一个版本。
struct FixedVersion(&'static str);

impl VersionProbe for FixedVersion {
    fn file_version(&self, _exe: &Path) -> Option<String> {
        Some(self.0.to_string())
    }
}

/// 条目名，按适配器给出的顺序。
fn item_names(result: &cclear_core::rules::adapters::AdapterResult) -> Vec<String> {
    result
        .items
        .iter()
        .map(|item| item.rel[0].to_string_lossy().into_owned())
        .collect()
}

/// 建一个 commit 目录，写入该目录的 `resources\app\product.json`。
fn write_commit(base: &Path, name: &str, version: &str, commit: &str) {
    let app = base.join(name).join(r"resources\app");
    fs::create_dir_all(&app).unwrap();
    fs::write(
        app.join("product.json"),
        format!(r#"{{"version":"{version}","commit":"{commit}"}}"#),
    )
    .unwrap();
}

#[test]
fn codex_runtimes_keep_primary_and_skip_newer_install() {
    let temp = tempdir().unwrap();
    let roots = temp_roots(temp.path());
    let base = roots
        .get(KnownRoot::Profile)
        .unwrap()
        .join(".cache\\codex-runtimes");

    // 目录的修改时间随创建顺序递增：先建旧安装目录，再建当前运行时，最后建比它新的安装目录。
    fs::create_dir_all(base.join("codex-runtime-install-old")).unwrap();
    fs::create_dir_all(
        base.join("codex-primary-runtime.previous-11111111-2222-3333-4444-555555555555"),
    )
    .unwrap();
    fs::create_dir_all(base.join("codex-primary-runtime")).unwrap();
    fs::create_dir_all(base.join("codex-runtime-install-new")).unwrap();

    // 把“现在”往后推两天：24 小时的年龄条件才会成立。
    let now = SystemTime::now() + Duration::from_secs(48 * 3600);
    let probe = FixedVersion("1.0.0");

    let previous = evaluate_adapter(AdapterId::CodexPreviousRuntime, &roots, &[], &probe, now);
    assert_eq!(previous.status, CandidateStatus::Ready);
    assert_eq!(
        item_names(&previous),
        vec!["codex-primary-runtime.previous-11111111-2222-3333-4444-555555555555".to_string()],
        "当前运行时不能选中，只剩下 previous-* 目录"
    );

    let install = evaluate_adapter(AdapterId::CodexRuntimeInstall, &roots, &[], &probe, now);
    assert_eq!(install.status, CandidateStatus::Ready);
    assert_eq!(
        item_names(&install),
        vec!["codex-runtime-install-old".to_string()],
        "比当前运行时新的安装目录不能选中"
    );
    assert!(
        install
            .notes
            .iter()
            .any(|note| note.contains("codex-runtime-install-new")),
        "被排除的新安装目录要在 notes 里说明"
    );
}

#[test]
fn vscode_versions_require_idle_updater_and_pick_other_commits() {
    let probe = FixedVersion("1.108.0");

    // 更新进行中：目录内容随时会变，只报证据不足。
    let updating = tempdir().unwrap();
    let updating_roots = temp_roots(updating.path());
    let updating_base = updating_roots
        .get(KnownRoot::LocalAppData)
        .unwrap()
        .join("Programs\\Microsoft VS Code");
    fs::create_dir_all(&updating_base).unwrap();
    fs::write(updating_base.join("updating_version"), "e646868227c3e3a9").unwrap();
    fs::write(updating_base.join("Code.exe"), []).unwrap();
    write_commit(&updating_base, "aaaaaaaaaa", "1.108.0", "aaaaaaaaaa");

    let running = evaluate_adapter(
        AdapterId::VsCodeOldCommit,
        &updating_roots,
        &[],
        &probe,
        SystemTime::now(),
    );
    assert!(
        matches!(running.status, CandidateStatus::Insufficient(_)),
        "存在 updating_version 时必须返回证据不足"
    );

    // 更新结束：只选版本与 Code.exe 不一致的 commit 目录，以及旧的启动器。
    let idle = tempdir().unwrap();
    let idle_roots = temp_roots(idle.path());
    let idle_base = idle_roots
        .get(KnownRoot::LocalAppData)
        .unwrap()
        .join("Programs\\Microsoft VS Code");
    fs::create_dir_all(&idle_base).unwrap();
    fs::write(idle_base.join("Code.exe"), []).unwrap();
    fs::write(idle_base.join("old_Code.exe"), []).unwrap();
    write_commit(&idle_base, "aaaaaaaaaa", "1.108.0", "aaaaaaaaaa");
    write_commit(&idle_base, "bbbbbbbbbb", "1.100.0", "bbbbbbbbbb");

    let result = evaluate_adapter(
        AdapterId::VsCodeOldCommit,
        &idle_roots,
        &[],
        &probe,
        SystemTime::now(),
    );
    assert_eq!(result.status, CandidateStatus::Ready);
    assert_eq!(
        item_names(&result),
        vec!["bbbbbbbbbb".to_string(), "old_Code.exe".to_string()],
        "版本一致的目录是当前版本，不能选中"
    );
}

#[test]
fn editor_extensions_pick_only_obsolete_and_unreferenced() {
    let temp = tempdir().unwrap();
    let roots = temp_roots(temp.path());
    let base = roots
        .get(KnownRoot::Profile)
        .unwrap()
        .join(".vscode\\extensions");

    for dir in ["obsolete-only-1.0.0", "still-referenced-1.0.0", ".1a2b3c4d"] {
        fs::create_dir_all(base.join(dir)).unwrap();
    }
    fs::write(
        base.join(".obsolete"),
        r#"{"obsolete-only-1.0.0":true,"still-referenced-1.0.0":true}"#,
    )
    .unwrap();
    fs::write(
        base.join("extensions.json"),
        r#"[{"relativeLocation":"still-referenced-1.0.0"}]"#,
    )
    .unwrap();

    // 把“现在”往后推两天：临时目录的 24 小时条件才会成立。
    let now = SystemTime::now() + Duration::from_secs(48 * 3600);
    let result = evaluate_adapter(
        AdapterId::VsCodeObsoleteExtensions,
        &roots,
        &[],
        &FixedVersion("1.0.0"),
        now,
    );

    assert_eq!(result.status, CandidateStatus::Ready);
    let mut names = item_names(&result);
    names.sort();
    assert_eq!(
        names,
        vec![".1a2b3c4d".to_string(), "obsolete-only-1.0.0".to_string()],
        "被 extensions.json 引用的目录不能选中"
    );
}

#[test]
fn mathworks_keeps_latest_install_from_info_file() {
    let temp = tempdir().unwrap();
    let roots = temp_roots(temp.path());
    let base = roots
        .get(KnownRoot::LocalAppData)
        .unwrap()
        .join("MathWorks\\ServiceHost");

    fs::create_dir_all(base.join("v2026.10.0.4")).unwrap();
    fs::create_dir_all(base.join("v2026.10.1.1")).unwrap();

    // 本机确认过的格式：逐行“键 值”，路径里的反斜杠是双写的。
    let latest = base.join("v2026.10.1.1");
    let escaped: String = latest.to_string_lossy().replace('\\', r"\\");
    fs::write(
        base.join("LatestInstall.info"),
        format!("LatestDSInstallerVersion 2026.10.1.1\nLatestDSInstallRoot {escaped}\n"),
    )
    .unwrap();

    let result = evaluate_adapter(
        AdapterId::MathWorksServiceHost,
        &roots,
        &[],
        &FixedVersion("1.0.0"),
        SystemTime::now(),
    );

    assert_eq!(result.status, CandidateStatus::Ready);
    assert_eq!(
        item_names(&result),
        vec!["v2026.10.0.4".to_string()],
        "LatestInstall.info 指向的版本必须保留"
    );
}
