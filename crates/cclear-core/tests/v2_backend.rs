use std::fs::{self, File, FileTimes, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use cclear_core::elevation::system_ops::validate_reboot_path;
use cclear_core::exec::file_cleaner::{Cleaner, Filters};
use cclear_core::guard::{self, Forbidden, GuardError};
use cclear_core::journal;
use cclear_core::model::{CleanCategory, DeleteMode, KnownRoot};
use cclear_core::orchestrator::{CleanItemReport, CleanReport, ItemStatus};
use cclear_core::rules::{RootMap, heuristics, schema};
use cclear_core::settings::{self, AppSettingsPatch};
use cclear_core::win::registry::InstalledSoftware;
use serde_json::{Value, json};
use tempfile::tempdir;

#[test]
fn chromium_profiles_and_file_filters_select_only_eligible_cache_files() {
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let base = root.join(r"Browser\User Data");
    let cache = base.join(r"Work Profile\Cache");
    fs::create_dir_all(&cache).unwrap();
    fs::write(
        base.join("Local State"),
        r#"{"profile":{"info_cache":{"Work Profile":{},"..\\Outside":{}}}}"#,
    )
    .unwrap();
    let now = SystemTime::now();
    for (name, age_hours) in [
        ("newest.log", 3),
        ("old.log", 6),
        ("skip-old.log", 9),
        ("old.db", 9),
        ("recent.log", 0),
    ] {
        let file = File::create(cache.join(name)).unwrap();
        file.set_times(FileTimes::new().set_modified(now - Duration::from_secs(age_hours * 3600)))
            .unwrap();
    }
    let roots = RootMap::with(vec![(KnownRoot::LocalAppData, root.to_path_buf())]);
    let forbidden = Forbidden::from_rules(&roots);
    let cancel = AtomicBool::new(false);
    let cleaner = Cleaner {
        forbidden: &forbidden,
        whitelist: &[],
        cancel: &cancel,
    };
    let rule = serde_json::from_value(json!({
        "id": "test", "name": "测试浏览器", "description": "规则过滤器", "category": "browser",
        "deleteMode":"direct", "minAgeHours": 2, "excludeNewestPerDir": 1,
        "excludePatterns":["skip-*"], "extensions":["log"],
        "target":{"kind":"chromiumProfiles","base":{"root":"localAppData","rels":[r"Browser\User Data"]},"rels":["Cache"]}
    })).unwrap();
    let candidate =
        cclear_core::rules::engine::evaluate(&rule, "test".into(), &roots, &cleaner, true, 24)
            .unwrap();
    assert_eq!(
        candidate.item.sample_paths,
        [cache.join("newest.log"), cache.join("old.log")]
    );
}

fn external_pack(target: Value) -> Value {
    json!({
        "id": "local-caches",
        "name": "缓存",
        "version": "1",
        "description": "临时测试数据",
        "rules": [{"id": "sample", "name": "样例", "description": "测试缓存", "category": "app",
            "deleteMode": "direct", "target": target}]
    })
}

fn external_paths(root: &str, rels: Value) -> Value {
    json!({"kind":"paths", "paths":[{"root":root, "rels":rels}]})
}

#[test]
fn external_rules_reject_privileged_targets_and_unsafe_paths() {
    let fixture = tempdir().unwrap();
    let roots = RootMap::with(vec![(
        KnownRoot::LocalAppData,
        fixture.path().to_path_buf(),
    )]);
    assert!(schema::builtin().rules.len() >= 45);
    let profile = external_paths("profile", json!([r"ExampleApp\Cache"]));
    assert!(schema::parse_external(&external_pack(profile).to_string(), &roots).is_err());
    for invalid in [
        json!([]),
        json!([""]),
        json!(["*"]),
        json!([r"*\Cache"]),
        json!([r"Example?pp\Cache"]),
        json!([".ssh"]),
        json!([r"Microsoft\Protect"]),
        json!([r"ExampleApp\.gnupg\Cache"]),
        json!([r"Cache\..\outside"]),
        json!([r"\\server\share"]),
        json!([r"\\.\X:\Cache"]),
        json!([r"GLOBALROOT\Cache"]),
        json!(["file:stream"]),
        json!([r"Chrome\Default\Cookies"]),
        json!([r"CClear\webview"]),
    ] {
        let mut pack = external_pack(external_paths("localAppData", invalid.clone()));
        pack["rules"][0]["deleteMode"] = json!("recycleBin");
        assert!(
            schema::parse_external(&pack.to_string(), &roots).is_err(),
            "{invalid}"
        );
    }
    for invalid in [
        json!([r"SomeApp\UserData"]),
        json!([r"Sample\Cache*"]),
        json!([r"Sample\Different\reports"]),
        json!([r"Sample\Crash*\reports"]),
        json!([r"Sample\Cache", r"Sample\UserData"]),
    ] {
        let pack = external_pack(external_paths("localAppData", invalid.clone()));
        assert!(
            schema::parse_external(&pack.to_string(), &roots).is_err(),
            "{invalid}"
        );
    }
    for target in [
        json!({"kind":"command","command":"npm"}),
        json!({"kind":"systemOp","op":"windows-temp"}),
        json!({"kind":"recycleBin"}),
    ] {
        assert!(schema::parse_external(&external_pack(target).to_string(), &roots).is_err());
    }
    assert!(schema::parse_external(&" ".repeat(schema::MAX_PACK_BYTES + 1), &roots).is_err());
}

#[test]
fn external_rules_reject_protected_processes_and_overlong_text() {
    let fixture = tempdir().unwrap();
    let roots = RootMap::with(vec![(
        KnownRoot::LocalAppData,
        fixture.path().to_path_buf(),
    )]);
    let valid = external_pack(external_paths("localAppData", json!([r"ExampleApp\Cache"])));
    for process in [
        "explorer.exe",
        "CONHOST.EXE",
        r"App\sample.exe",
        "sample?.exe",
        "sample\n.exe",
        ".exe",
        "sample.com",
    ] {
        let mut pack = valid.clone();
        pack["rules"][0]["processes"] = json!([process]);
        assert!(
            schema::parse_external(&pack.to_string(), &roots).is_err(),
            "{process:?}"
        );
    }
    for (pointer, limit) in [
        ("/name", 120),
        ("/version", 32),
        ("/description", 500),
        ("/rules/0/name", 120),
        ("/rules/0/description", 500),
        ("/rules/0/warning", 500),
    ] {
        let mut pack = valid.clone();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        pack.pointer_mut(parent).unwrap()[field] = json!("字".repeat(limit));
        assert!(
            schema::parse_external(&pack.to_string(), &roots).is_ok(),
            "{pointer}"
        );
        pack.pointer_mut(parent).unwrap()[field] = json!("字".repeat(limit + 1));
        assert!(
            schema::parse_external(&pack.to_string(), &roots).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn external_rules_accept_safe_targets_and_report_their_summary() {
    let fixture = tempdir().unwrap();
    let roots = RootMap::with(vec![(
        KnownRoot::LocalAppData,
        fixture.path().to_path_buf(),
    )]);
    let mut pack = external_pack(external_paths("localAppData", json!([r"ExampleApp\Cache"])));
    let mut recycle = pack["rules"][0].clone();
    recycle["id"] = json!("old-data");
    recycle["name"] = json!("旧数据");
    recycle["deleteMode"] = json!("recycleBin");
    recycle["target"] = external_paths("localAppData", json!([r"ExampleApp\OldData"]));
    pack["rules"][0]["processes"] = json!(["Example App.EXE"]);
    pack["rules"].as_array_mut().unwrap().push(recycle);
    let info = schema::parse_external(&pack.to_string(), &roots)
        .unwrap()
        .info(false);
    assert_eq!(
        serde_json::to_value(info).unwrap()["rulesSummary"],
        json!([
            {"id":"sample", "name":"样例", "deleteMode":"direct", "targets":[r"localAppData\ExampleApp\Cache"]},
            {"id":"old-data", "name":"旧数据", "deleteMode":"recycleBin", "targets":[r"localAppData\ExampleApp\OldData"]},
        ])
    );
    for relative in [
        r"ExampleApp\cAcHe",
        r"ExampleApp\Crashpad\REPORTS",
        r"ExampleApp\Profile *\GPUCache",
    ] {
        let pack = external_pack(external_paths("roamingAppData", json!([relative])));
        assert!(
            schema::parse_external(&pack.to_string(), &roots).is_ok(),
            "{relative}"
        );
    }
    let pack = external_pack(json!({
        "kind": "chromiumProfiles",
        "base": {"root": "localAppData", "rels": [r"ExampleApp\User Data", r"ExampleApp\Profiles"]},
        "rels": [r"Storage\Cache", r"Storage\GPUCache"],
    }));
    let info = schema::parse_external(&pack.to_string(), &roots)
        .unwrap()
        .info(false);
    assert_eq!(
        info.rules_summary[0].targets,
        [
            r"基目录：localAppData\ExampleApp\User Data",
            r"基目录：localAppData\ExampleApp\Profiles",
            r"<基目录或配置目录>\Storage\Cache",
            r"<基目录或配置目录>\Storage\GPUCache",
        ]
    );
}

#[test]
fn corrupt_settings_fall_back_to_default_preferences() {
    let fixture = tempdir().unwrap();
    let roots = RootMap::with(vec![(
        KnownRoot::LocalAppData,
        fixture.path().to_path_buf(),
    )]);
    let data = settings::data_dir(&roots).unwrap();
    let file = data.path().join("settings-v2.json");
    for contents in [b"{ broken JSON".as_slice(), &[0xff, 0xfe]] {
        fs::write(&file, contents).unwrap();
        let actual = settings::get(&roots).unwrap().preferences;
        let expected = settings::Preferences::default();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(fs::read(&file).unwrap(), contents);
    }
}

#[test]
fn cache_detectors_and_orphan_evidence_stay_within_safe_leaves() {
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let roots = RootMap::with(vec![(KnownRoot::LocalAppData, root.to_path_buf())]);
    let forbidden = Forbidden::from_rules(&roots);
    let cancel = AtomicBool::new(false);
    let cleaner = Cleaner {
        forbidden: &forbidden,
        whitelist: &[],
        cancel: &cancel,
    };
    for relative in [
        r"SampleElectron\Cache",
        r"SampleElectron\Code Cache",
        r"SampleElectron\Local Storage",
        r"Packages\Demo\TempState",
        r"Packages\Demo\LocalState",
        "UnusedProduct",
        "MicrosoftSample",
        "ExistingProduct",
    ] {
        let dir = root.join(relative);
        fs::create_dir_all(&dir).unwrap();
        let size = if relative.contains('\\') { 2 } else { 11 };
        fs::write(dir.join("payload.bin"), vec![5; size * 1024 * 1024]).unwrap();
    }
    fs::write(root.join(r"SampleElectron\Preferences"), b"{}").unwrap();
    let electron = heuristics::electron::detect(&roots, &cleaner, &[]);
    assert_eq!(electron.len(), 1);
    assert_eq!(electron[0].item.anchors.len(), 2);
    assert!(
        electron[0]
            .item
            .anchors
            .iter()
            .all(|path| !path.ends_with("Local Storage"))
    );
    assert!(
        heuristics::electron::detect(&roots, &cleaner, &[root.join(r"SampleElectron\Cache")])
            .is_empty()
    );
    let uwp = heuristics::uwp::detect(&roots, &cleaner).unwrap();
    assert_eq!(uwp.item.anchors, [root.join(r"Packages\Demo\TempState")]);
    let installed = [InstalledSoftware {
        display_name: "Existing Product".into(),
        ..InstalledSoftware::default()
    }];
    let future = journal::unix_now() + 31 * 86_400;
    let orphans = heuristics::orphans::detect(&roots, &cleaner, &installed, &[], &[], future);
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].item.anchors, [root.join("UnusedProduct")]);
    assert_eq!(orphans[0].item.delete_mode, DeleteMode::RecycleBin);
    assert!(!orphans[0].item.default_selected);
    let loaded = [root.join(r"UnusedProduct\module.dll")];
    assert!(
        heuristics::orphans::detect(&roots, &cleaner, &installed, &[], &loaded, future).is_empty()
    );
}

#[test]
fn file_cleanup_skips_lock_and_keeps_whitelist() {
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let locked = root.join("locked.bin");
    let unlocked = root.join("unlocked.bin");
    let protected = root.join("protected.bin");
    fs::write(&locked, b"locked").unwrap();
    fs::write(&unlocked, b"free").unwrap();
    fs::write(&protected, b"protected").unwrap();
    let _lock = OpenOptions::new()
        .write(true)
        .share_mode(3)
        .open(&locked)
        .unwrap();
    let forbidden = Forbidden::empty();
    let cancel = AtomicBool::new(false);
    let cleaner = Cleaner {
        forbidden: &forbidden,
        whitelist: std::slice::from_ref(&protected),
        cancel: &cancel,
    };
    let report = cleaner.clean(&[root.to_path_buf()], &Filters::default());
    assert_eq!((report.deleted_files, report.skipped_files), (1, 1));
    assert_eq!(report.locked.as_slice(), std::slice::from_ref(&locked));
    assert!(locked.exists() && protected.exists());
    assert!(!unlocked.exists());
}

#[test]
fn recycle_and_elevated_validation_drop_protected_targets() {
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let windows = root.join("Windows");
    let logs = windows.join("Logs");
    let protected = windows.join("System32");
    fs::create_dir_all(&logs).unwrap();
    fs::create_dir_all(&protected).unwrap();
    let eligible = logs.join("old.log");
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    File::create(&eligible)
        .unwrap()
        .set_times(FileTimes::new().set_modified(old))
        .unwrap();
    let forbidden_file = protected.join("test.log");
    fs::write(&forbidden_file, b"never touch").unwrap();
    let other = root.join("outside.log");
    fs::write(&other, b"never touch").unwrap();
    let junction = logs.join("shortcut");
    let logs_handle = guard::open_anchor(&logs, &Forbidden::empty()).unwrap();
    guard::create_junction(&logs_handle, "shortcut".as_ref(), &protected).unwrap();
    let roots = RootMap::with(vec![(KnownRoot::Windows, windows)]);
    let forbidden = Forbidden::from_rules(&roots);
    let cancel = AtomicBool::new(false);
    let cleaner = Cleaner {
        forbidden: &forbidden,
        whitelist: &[],
        cancel: &cancel,
    };
    assert_eq!(
        validate_reboot_path(&eligible, &roots, &cleaner, 24).unwrap(),
        eligible
    );
    assert!(validate_reboot_path(&other, &roots, &cleaner, 24).is_err());
    assert!(validate_reboot_path(&junction.join("test.log"), &roots, &cleaner, 24).is_err());
    assert!(matches!(
        guard::recycle::send_to_recycle_bin(&forbidden_file, &forbidden),
        Err(GuardError::Forbidden(_))
    ));
    assert!(matches!(
        guard::recycle::send_to_recycle_bin(&junction, &forbidden),
        Err(GuardError::ReparseInPath(_))
    ));
    assert!(matches!(
        guard::recycle::send_to_recycle_bin(&junction.join("test.log"), &forbidden),
        Err(GuardError::ReparseInPath(_))
    ));
    assert!(forbidden_file.exists() && eligible.exists());
}

#[test]
fn preferences_rule_packs_and_history_round_trip() {
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let roots = RootMap::with(vec![(KnownRoot::LocalAppData, root.to_path_buf())]);
    let protected = root.join("Workspace");
    let saved = settings::save(
        &roots,
        AppSettingsPatch {
            whitelist_paths: Some(vec![protected.to_string_lossy().into_owned()]),
            schedule_reboot_delete_default: Some(true),
            min_temp_age_hours: Some(72),
        },
    )
    .unwrap();
    assert_eq!(saved.preferences.whitelist_paths, [protected]);
    assert_eq!(
        settings::load_preferences(&roots)
            .unwrap()
            .min_temp_age_hours,
        72
    );
    assert!(saved.preferences.schedule_reboot_delete_default);
    let pack = r#"{"id":"custom","name":"自定义","version":"1","description":"缓存规则","rules":[{"id":"test","name":"缓存","description":"测试","category":"app","deleteMode":"direct","target":{"kind":"paths","paths":[{"root":"localAppData","rels":["MyApp\\Cache"]}]}}]}"#;
    assert_eq!(settings::import_pack(&roots, pack).unwrap().id, "custom");
    assert_eq!(settings::get(&roots).unwrap().rule_packs.len(), 2);
    settings::remove_pack(&roots, "custom").unwrap();
    assert_eq!(settings::get(&roots).unwrap().rule_packs.len(), 1);
    let item = CleanItemReport {
        item_id: "test".into(),
        name: "测试".into(),
        category: CleanCategory::App,
        status: ItemStatus::Done,
        freed_bytes: 10,
        deleted_files: 1,
        deleted_dirs: 0,
        recycled_items: 0,
        skipped_files: 0,
        reboot_scheduled: 0,
        detail: "完成".into(),
    };
    let run = |id: &str| CleanReport {
        run_id: id.into(),
        started_at: journal::unix_now(),
        elapsed_ms: 1,
        freed_bytes: 10,
        free_before: 0,
        free_after: 10,
        deleted_files: 1,
        deleted_dirs: 0,
        recycled_items: 0,
        skipped_files: 0,
        reboot_scheduled: 0,
        items: vec![item.clone()],
        cancelled: false,
        error: None,
    };
    journal::append(&roots, &run("first")).unwrap();
    journal::append(&roots, &run("second")).unwrap();
    let history = journal::list(&roots, 7).unwrap();
    assert_eq!(history.runs.len(), 2);
    assert_eq!(history.skipped_lines, 0);
    assert_eq!(
        journal::detail(&roots, "first", 7).unwrap().items[0].item_id,
        "test"
    );
    assert!(Path::new(root).join(r"CClear\history-v2.jsonl").exists());
}
