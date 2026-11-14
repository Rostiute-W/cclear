//! 工具缓存迁移的集成测试。全部只在 `tempdir()` 创建的目录里操作：
//! 根目录、候选卷、用户级配置和迁移记录都由测试注入，不写真实环境变量和注册表。

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use cclear_core::journal::{JournalSink, Record};
use cclear_core::migrate::tools::ToolId;
use cclear_core::migrate::{Env, EnvStore, MigrateError, Migration, Mode, OldValue, Stage};
use cclear_core::model::KnownRoot;
use cclear_core::rules::resolve::RootMap;
use cclear_core::win::volume::VolumeInfo;
use tempfile::TempDir;

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

/// 内存里的用户环境变量，代替真实注册表。
struct MemoryEnv {
    values: Mutex<HashMap<String, String>>,
}

impl MemoryEnv {
    /// 按测试给定的初始值建一份配置，值都是临时目录里的路径。
    fn with(values: Vec<(&str, PathBuf)>) -> Self {
        let mut map = HashMap::new();
        for (name, path) in values {
            map.insert(name.to_string(), path.to_string_lossy().into_owned());
        }

        Self {
            values: Mutex::new(map),
        }
    }
}

impl EnvStore for MemoryEnv {
    fn read(&self, name: &str) -> Option<String> {
        self.values.lock().unwrap().get(name).cloned()
    }

    fn write(&self, name: &str, value: &str) -> Result<(), String> {
        self.values
            .lock()
            .unwrap()
            .insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        self.values.lock().unwrap().remove(name);
        Ok(())
    }
}

/// 临时目录里的一套用户目录结构，外加一个“其他盘”的目录当作目标卷。
struct Fixture {
    temp: TempDir,
    profile: PathBuf,
    /// 候选卷的根。注入的卷不需要真的是另一个盘。
    volume: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let profile = temp.path().join("profile");
        let local = profile.join("AppData").join("Local");

        for path in [
            &local,
            &profile.join("AppData").join("Roaming"),
            &profile.join("Documents"),
            &profile.join("Downloads"),
        ] {
            fs::create_dir_all(path).unwrap();
        }

        let volume = temp.path().join("volume");
        fs::create_dir_all(&volume).unwrap();

        Self {
            temp,
            profile,
            volume,
        }
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

    /// 注入环境：根目录、卷、配置读写都走临时目录，记录文件放在 `store` 下。
    fn env(&self, values: Vec<(&str, PathBuf)>) -> Env {
        let volumes = vec![VolumeInfo {
            root: self.volume.clone(),
            label: "测试卷".to_string(),
            fs: "NTFS".to_string(),
            total: 1 << 40,
            free: 1 << 40,
            is_system: false,
        }];

        Env::for_test(
            self.roots(),
            self.temp.path().join("store"),
            volumes,
            Box::new(MemoryEnv::with(values)),
        )
    }

    fn store_dir(&self) -> PathBuf {
        self.temp.path().join("store")
    }
}

#[test]
fn rejects_overbroad_old_paths() {
    let fixture = Fixture::new();
    let mut journal = MemoryJournal::default();
    let volume_root = fixture.volume.to_string_lossy().into_owned();

    // 盘符根目录：清理清单会指向整个盘，不允许迁移。
    let env = fixture.env(vec![("GRADLE_USER_HOME", PathBuf::from(r"Z:\"))]);
    let error = env
        .start(ToolId::Gradle, &volume_root, Mode::Migrate, &mut journal)
        .unwrap_err();
    assert!(
        matches!(error, MigrateError::BadOldPath(_)),
        "盘符根目录应当被拒绝，实际：{error:?}"
    );
    assert!(error.to_string().contains("盘符根目录"));

    // 用户目录本身：清单会指向整个用户目录。
    let env = fixture.env(vec![("GRADLE_USER_HOME", fixture.profile.clone())]);
    let error = env
        .start(ToolId::Gradle, &volume_root, Mode::Migrate, &mut journal)
        .unwrap_err();
    assert!(
        matches!(error, MigrateError::BadOldPath(_)),
        "用户目录本身应当被拒绝，实际：{error:?}"
    );
    assert!(error.to_string().contains("系统目录"));

    // 新缓存目录的上级目录：清单会指向活的缓存。
    let env = fixture.env(vec![("GRADLE_USER_HOME", fixture.volume.clone())]);
    let error = env
        .start(ToolId::Gradle, &volume_root, Mode::Migrate, &mut journal)
        .unwrap_err();
    assert!(
        matches!(error, MigrateError::BadOldPath(_)),
        "目标目录的上级应当被拒绝，实际：{error:?}"
    );
    assert!(error.to_string().contains("新的缓存目录"));

    // 普通的缓存目录照常可以迁移：拒绝只针对范围太宽的路径。
    let old = fixture.profile.join("gradle-cache");
    fs::create_dir_all(old.join("caches")).unwrap();
    fs::write(old.join("caches").join("module.bin"), "m").unwrap();

    let env = fixture.env(vec![("GRADLE_USER_HOME", old.clone())]);
    let started = env
        .start(ToolId::Gradle, &volume_root, Mode::Migrate, &mut journal)
        .unwrap();
    assert_eq!(started.stage, Stage::Verified);

    // 清理清单再查一次：与新缓存目录重叠的锚点全部排除，只留真正的旧目录。
    let base = fixture.volume.join("CClearData");
    let target = base.join("gradle");
    fs::create_dir_all(target.join("sub")).unwrap();

    let record = Migration {
        tool: "gradle".to_string(),
        stage: Stage::Verified,
        mode: Mode::Migrate,
        volume_root: volume_root.clone(),
        target: base.to_string_lossy().into_owned(),
        old_paths: vec![
            // 新缓存目录本身、它的上级和它的下级都不算旧目录。
            target.to_string_lossy().into_owned(),
            base.to_string_lossy().into_owned(),
            target.join("sub").to_string_lossy().into_owned(),
            // 真正的旧目录。
            old.to_string_lossy().into_owned(),
        ],
        old_values: vec![OldValue {
            name: "GRADLE_USER_HOME".to_string(),
            value: None,
        }],
        note: "测试用记录".to_string(),
        updated_at: 0,
    };

    fs::create_dir_all(fixture.store_dir()).unwrap();
    let store = serde_json::json!({ "tools": [record] });
    fs::write(
        fixture.store_dir().join("migrations.json"),
        serde_json::to_string_pretty(&store).unwrap(),
    )
    .unwrap();

    // 当前生效的配置指向新位置，旧目录才谈得上清理。
    let env = fixture.env(vec![("GRADLE_USER_HOME", target.clone())]);
    let plan = env.cleanup_plan(ToolId::Gradle, &mut journal).unwrap();

    assert_eq!(plan.rules[0].anchors, vec![old.clone()]);
    assert!(
        plan.items
            .iter()
            .any(|item| item.rel == vec![OsString::from("caches"), OsString::from("module.bin")]),
        "旧目录里的文件应当进清单"
    );
}
