//! 多版本残留的产品适配器。
//!
//! 判定原则：任何一项证据不成立就返回 `Insufficient`，宁可漏选也不误选；
//! 命中条目的恢复方式一律是隔离，不提供直接删除。
//!
//! 各适配器上方写的是本机只读确认过的真实格式（`LatestInstall.info`、`.obsolete`、
//! `extensions.json`、`product.json` 和 `codex-runtimes` 的目录命名）。

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use time::OffsetDateTime;

use crate::model::KnownRoot;
use crate::rules::catalog::AdapterId;
use crate::rules::resolve::{CandidateStatus, RootMap, age_hours, system_time_ticks};
use crate::win::dirinfo;
use crate::win::process::ProcInfo;
use crate::win::registry::{self, RegistryRoot};

/// 临时目录、安装残留的最小年龄：超过 24 小时。
const STALE_HOURS: u64 = 24;

/// 当前用户的 WSL 发行版注册项。
const LXSS_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

/// Docker Desktop 的相关进程：它们没退出时不碰更新包。
const DOCKER_PROCESSES: &[&str] = &[
    "Docker Desktop.exe",
    "com.docker.backend.exe",
    "Docker Desktop Installer.exe",
];

/// Codex 运行时要求退出的进程。
const CODEX_PROCESSES: &[&str] = &["codex.exe", "codex-code-mode-host.exe"];

/// 联想应用商店的主进程。本机只读核实：`C:\Program Files (x86)\Common Files\LenovoAS\LeASLane.exe`
/// 的 FileDescription 与 ProductName 都是“联想应用商店”；`ProgramData\Lenovo\LeAppStore` 里的
/// 快捷方式指向 `Common Files\LenovoAppStoreNotify\LenovoAppStoreNotify.exe`（联想应用商店通知程序），
/// 它随商店常驻，所以两个都算。
pub const LE_APPSTORE_PROCESSES: &[&str] = &["LeASLane.exe", "LenovoAppStoreNotify.exe"];

/// NVIDIA App 的主进程。本机只读核实：`C:\Program Files\NVIDIA Corporation\NVIDIA App\CEF`
/// 下的可执行文件是 `NVIDIA App.exe` 和 `NVIDIA Overlay.exe`。`nvcontainer.exe` 是常驻容器服务，
/// 不随 App 退出，不能拿它当“应用正在运行”的依据。
pub const NVIDIA_APP_PROCESSES: &[&str] = &["NVIDIA App.exe", "NVIDIA Overlay.exe"];

/// 一个候选残留条目。`rel` 相对 `anchor`，执行时按它逐级相对打开。
pub struct LeftoverItem {
    pub anchor: PathBuf,
    pub rel: Vec<OsString>,
    pub is_dir: bool,
    pub evidence: Vec<String>,
}

/// 一个适配器的判定结果。
pub struct AdapterResult {
    pub status: CandidateStatus,
    pub items: Vec<LeftoverItem>,
    pub notes: Vec<String>,
}

/// 读取可执行文件的版本号。测试注入假实现，线上走 Win32 版本资源。
pub trait VersionProbe {
    fn file_version(&self, exe: &Path) -> Option<String>;
}

/// 生产实现。
pub struct SystemVersionProbe;

impl VersionProbe for SystemVersionProbe {
    fn file_version(&self, exe: &Path) -> Option<String> {
        crate::win::fileversion::file_version(exe)
    }
}

/// 按适配器 id 判定残留。
pub fn evaluate_adapter(
    id: AdapterId,
    roots: &RootMap,
    procs: &[ProcInfo],
    probe: &dyn VersionProbe,
    now: SystemTime,
) -> AdapterResult {
    match id {
        AdapterId::CodexPreviousRuntime => codex_runtimes(roots, procs, now, CodexPart::Previous),
        AdapterId::CodexRuntimeInstall => codex_runtimes(roots, procs, now, CodexPart::Install),
        AdapterId::VsCodeOldCommit => vscode_versions(roots, procs, probe),
        AdapterId::VsCodeObsoleteExtensions => {
            editor_extensions("Code.exe", r".vscode\extensions", roots, procs, now)
        }
        AdapterId::CursorObsoleteExtensions => {
            editor_extensions("Cursor.exe", r".cursor\extensions", roots, procs, now)
        }
        AdapterId::MathWorksServiceHost => mathworks_service_host(roots, procs),
        AdapterId::DroidOldBinary => droid_old_binary(roots),
        AdapterId::QianfanInstaller => qianfan_installer(roots, procs),
        AdapterId::DockerDesktopUpdates => docker_desktop_updates(roots, procs),
        AdapterId::WslUnregisteredVhdx => wsl_unregistered_vhdx(roots),
        AdapterId::LeAppStoreCicada => le_appstore_cicada(roots, procs),
        AdapterId::NvidiaAppOtaArtifacts => nvidia_app_ota_artifacts(roots, procs),
    }
}

/// Codex 运行时里要选哪一类目录。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CodexPart {
    Previous,
    Install,
}

/// Codex 运行时（本机确认的布局）：
/// `Profile\.cache\codex-runtimes` 下同时存在 `codex-primary-runtime`（当前）、
/// `codex-primary-runtime.previous-<uuid>` 和 `codex-runtime-install-<6 位随机串>`。
fn codex_runtimes(
    roots: &RootMap,
    procs: &[ProcInfo],
    now: SystemTime,
    part: CodexPart,
) -> AdapterResult {
    let Some(base) = roots
        .get(KnownRoot::Profile)
        .map(|home| home.join(".cache\\codex-runtimes"))
    else {
        return not_present();
    };
    if !dirinfo::is_plain_dir(&base) {
        return not_present();
    }

    // 没有当前运行时就没有参照物，判断不了哪个版本是残留。
    let primary = base.join("codex-primary-runtime");
    if !dirinfo::is_plain_dir(&primary) {
        return insufficient("没有找到 codex-primary-runtime，无法判断哪些运行时是旧的");
    }

    let (_, primary_mtime) = times(&primary);
    let now_ticks = system_time_ticks(now);
    let mut items = Vec::new();
    let mut notes = Vec::new();

    for (name, path) in child_dirs(&base) {
        if part == CodexPart::Previous {
            if !has_prefix_ignore_case(&name, "codex-primary-runtime.previous-") {
                continue;
            }

            let (_, mtime) = times(&path);
            items.push(LeftoverItem {
                anchor: base.clone(),
                rel: vec![OsString::from(&name)],
                is_dir: true,
                evidence: vec![
                    "目录名匹配 codex-primary-runtime.previous-*".to_string(),
                    format!("最后写入 {}", format_time(mtime)),
                    "当前运行时 codex-primary-runtime 存在".to_string(),
                ],
            });
            continue;
        }

        if !has_prefix_ignore_case(&name, "codex-runtime-install-") {
            continue;
        }

        let (ctime, mtime) = times(&path);
        if age_hours(ctime, mtime, now_ticks) < STALE_HOURS {
            notes.push(format!("{name} 在 24 小时内改动过，本次不选"));
            continue;
        }
        // 比当前运行时新的安装目录可能正是下次启动要用的，不能动。
        if mtime >= primary_mtime {
            notes.push(format!("{name} 不比当前运行时旧，本次不选"));
            continue;
        }

        items.push(LeftoverItem {
            anchor: base.clone(),
            rel: vec![OsString::from(&name)],
            is_dir: true,
            evidence: vec![
                "目录名匹配 codex-runtime-install-*".to_string(),
                format!("最后写入 {}", format_time(mtime)),
                "超过 24 小时没有改动".to_string(),
                "早于当前 codex-primary-runtime 的修改时间".to_string(),
            ],
        });
    }

    with_running(items, notes, running_names(CODEX_PROCESSES, procs))
}

/// `product.json` 里用来判断版本的两个字段。
struct ProductInfo {
    version: String,
    commit: String,
}

/// 读一个 commit 目录里的 `resources\app\product.json`。没有这个文件说明该目录不是安装目录。
fn product_info(dir: &Path) -> Option<ProductInfo> {
    let text = std::fs::read_to_string(dir.join(r"resources\app\product.json")).ok()?;
    let parsed = serde_json::from_str::<serde_json::Value>(&text).ok()?;

    Some(ProductInfo {
        version: parsed.get("version")?.as_str()?.to_string(),
        commit: parsed
            .get("commit")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// VS Code 安装目录（本机确认的布局）：
/// `LocalAppData\Programs\Microsoft VS Code` 下的 `Code.exe` 是当前版本，各 commit 目录
/// （10 位十六进制名）里 `resources\app\product.json` 记着该目录的 `version` 和 `commit`。
/// 更新进行中会出现 `updating_version`、`new_Code.exe` 和 `bin\new_code.cmd`。
fn vscode_versions(roots: &RootMap, procs: &[ProcInfo], probe: &dyn VersionProbe) -> AdapterResult {
    let Some(base) = roots
        .get(KnownRoot::LocalAppData)
        .map(|local| local.join("Programs\\Microsoft VS Code"))
    else {
        return not_present();
    };
    if !dirinfo::is_plain_dir(&base) {
        return not_present();
    }

    // 更新进行中时目录内容随时会变，这时选出来的目录可能马上被用到。
    if base.join("updating_version").exists()
        || base.join("new_Code.exe").exists()
        || base.join("bin\\new_code.cmd").exists()
    {
        return insufficient("VS Code 更新进行中");
    }

    let code_exe = base.join("Code.exe");
    let Some(version) = probe.file_version(&code_exe) else {
        return insufficient("读不到 Code.exe 的版本");
    };

    // 与 Code.exe 版本一致的目录必须恰好一个，否则分不清哪个是当前版本。
    let mut matching: Vec<String> = Vec::new();
    let mut others: Vec<(String, ProductInfo)> = Vec::new();
    for (name, path) in child_dirs(&base) {
        let Some(info) = product_info(&path) else {
            continue;
        };
        if info.version == version {
            matching.push(name);
        } else {
            others.push((name, info));
        }
    }

    if matching.len() != 1 {
        return insufficient(&format!(
            "与 Code.exe 版本 {version} 一致的目录有 {} 个，无法确定当前版本",
            matching.len()
        ));
    }
    let current = &matching[0];

    let mut items = Vec::new();
    for (name, info) in others {
        items.push(LeftoverItem {
            anchor: base.clone(),
            rel: vec![OsString::from(&name)],
            is_dir: true,
            evidence: vec![
                format!(
                    "resources\\app\\product.json 的 version 是 {}，与 Code.exe 的 {version} 不一致",
                    info.version
                ),
                format!("product.json 的 commit 是 {}", info.commit),
                format!("当前版本目录是 {current}"),
            ],
        });
    }

    // 更新时留下的旧启动器：当前版本另有 Code.exe，这个文件不会再被启动。
    if base.join("old_Code.exe").is_file() {
        items.push(LeftoverItem {
            anchor: base.clone(),
            rel: vec![OsString::from("old_Code.exe")],
            is_dir: false,
            evidence: vec![
                "更新时留下的旧启动器 old_Code.exe".to_string(),
                format!("当前版本目录是 {current}"),
            ],
        });
    }

    with_running(items, Vec::new(), running_names(&["Code.exe"], procs))
}

/// VS Code / Cursor 扩展目录（本机确认的格式）：
/// `<扩展目录>\.obsolete` 是 JSON 对象，键是扩展目录名；`extensions.json` 是数组，
/// 元素的 `relativeLocation` 是当前生效的目录名。
fn editor_extensions(
    process: &str,
    next_to_profile: &str,
    roots: &RootMap,
    procs: &[ProcInfo],
    now: SystemTime,
) -> AdapterResult {
    let Some(base) = roots
        .get(KnownRoot::Profile)
        .map(|home| home.join(next_to_profile))
    else {
        return not_present();
    };
    if !dirinfo::is_plain_dir(&base) {
        return not_present();
    }

    // .obsolete 不存在时只是还没淘汰过扩展，可以继续看临时目录；
    // 存在却读不出来时不能当作“没有记录”，按证据不足处理。
    let obsolete = match std::fs::read_to_string(base.join(".obsolete")) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(map)) => map,
            Ok(_) => return insufficient(".obsolete 不是 JSON 对象"),
            Err(_) => return insufficient(".obsolete 解析失败"),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return insufficient(&format!(".obsolete 读取失败：{error}")),
    };

    let Some(referenced) = referenced_extensions(&base) else {
        return insufficient("extensions.json 解析失败");
    };

    let now_ticks = system_time_ticks(now);
    let mut items = Vec::new();

    // 值为 false 的条目表示记录已经作废，不再当作残留。
    let mut names: Vec<&String> = obsolete
        .iter()
        .filter(|(_, value)| value.as_bool() != Some(false))
        .map(|(name, _)| name)
        .collect();
    names.sort();

    for name in names {
        // 仍被 extensions.json 引用的目录可能只是记录没清掉，删了会把在用的扩展带走。
        if referenced.contains(name) {
            continue;
        }
        if !dirinfo::is_plain_dir(&base.join(name)) {
            continue;
        }

        items.push(LeftoverItem {
            anchor: base.clone(),
            rel: vec![OsString::from(name)],
            is_dir: true,
            evidence: vec![
                "在 .obsolete 中列出".to_string(),
                "未被 extensions.json 引用".to_string(),
            ],
        });
    }

    for (name, path) in child_dirs(&base) {
        if !is_temp_dir_name(&name) || referenced.contains(&name) {
            continue;
        }

        let (ctime, mtime) = times(&path);
        if age_hours(ctime, mtime, now_ticks) < STALE_HOURS {
            continue;
        }

        items.push(LeftoverItem {
            anchor: base.clone(),
            rel: vec![OsString::from(&name)],
            is_dir: true,
            evidence: vec![
                ".<十六进制> 形式的临时目录".to_string(),
                format!("最后写入 {}", format_time(mtime)),
                "超过 24 小时没有改动".to_string(),
                "未被 extensions.json 引用".to_string(),
            ],
        });
    }

    with_running(items, Vec::new(), running_names(&[process], procs))
}

/// `extensions.json` 里当前生效的目录名。文件不存在时视为没有生效扩展，
/// 存在但读不出来或解析失败时返回 None，由调用方按证据不足处理。
fn referenced_extensions(base: &Path) -> Option<HashSet<String>> {
    let text = match std::fs::read_to_string(base.join("extensions.json")) {
        Ok(text) => text,
        // 没有这个文件就是没有生效的扩展。
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(HashSet::new()),
        // 存在却读不出来时不能当作“没有任何引用”，否则会把在用的扩展当成残留。
        Err(_) => return None,
    };
    let parsed = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    let list = parsed.as_array()?;

    let mut names = HashSet::new();
    for entry in list {
        if let Some(location) = entry
            .get("relativeLocation")
            .and_then(|value| value.as_str())
        {
            names.insert(location.to_string());
        }
    }

    Some(names)
}

/// MathWorks ServiceHost（本机确认的格式）：
/// `LatestInstall.info` 是逐行的“键 值”，值里的反斜杠是双写的，例如
/// `LatestDSInstallRoot C:\\Users\\<用户>\\AppData\\Local\\MathWorks\\ServiceHost\\v2026.10.1.1`。
fn mathworks_service_host(roots: &RootMap, procs: &[ProcInfo]) -> AdapterResult {
    let Some(base) = roots
        .get(KnownRoot::LocalAppData)
        .map(|local| local.join("MathWorks\\ServiceHost"))
    else {
        return not_present();
    };
    if !dirinfo::is_plain_dir(&base) {
        return not_present();
    }

    let Ok(text) = std::fs::read_to_string(base.join("LatestInstall.info")) else {
        return insufficient("读不到 LatestInstall.info");
    };
    let Some(latest) = latest_install_root(&text) else {
        return insufficient("LatestInstall.info 解析失败");
    };
    let Some(latest_name) = latest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    else {
        return insufficient("LatestInstall.info 里的安装路径没有目录名");
    };

    // 最新版本目录本身必须存在，否则剩下的目录都可能包括它。
    if !dirinfo::is_plain_dir(&latest) {
        return insufficient(&format!("LatestInstall.info 指向的 {latest_name} 不存在"));
    }

    let mut items = Vec::new();
    for (name, path) in child_dirs(&base) {
        if !has_prefix_ignore_case(&name, "v")
            || crate::win::equals_ignore_case(&name, &latest_name)
        {
            continue;
        }

        let (_, mtime) = times(&path);
        items.push(LeftoverItem {
            anchor: base.clone(),
            rel: vec![OsString::from(&name)],
            is_dir: true,
            evidence: vec![
                format!("不是 LatestInstall.info 指向的版本（{latest_name}）"),
                format!("最后写入 {}", format_time(mtime)),
            ],
        });
    }

    with_running(
        items,
        Vec::new(),
        running_names(&["MathWorksServiceHost.exe"], procs),
    )
}

/// 从 `LatestInstall.info` 取 `LatestDSInstallRoot`，并把双写反斜杠还原成单写。
fn latest_install_root(text: &str) -> Option<PathBuf> {
    for line in text.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        if key.trim() != "LatestDSInstallRoot" {
            continue;
        }

        let path = value.trim().replace(r"\\", r"\");
        if path.is_empty() {
            return None;
        }
        return Some(PathBuf::from(path));
    }

    None
}

/// droid 的 bin 目录（本机确认的格式）：当前 `droid.exe` 与新版本同时存在时，
/// 旧版会留下 `droid.exe.old-<pid>-<时间戳>` 形式的文件。
fn droid_old_binary(roots: &RootMap) -> AdapterResult {
    let Some(bin) = roots.get(KnownRoot::Profile).map(|home| home.join("bin")) else {
        return not_present();
    };
    if !dirinfo::is_plain_dir(&bin) {
        return not_present();
    }
    // 没有当前 droid.exe 说明不是 droid 的布局，判断不了哪个是旧版。
    if !bin.join("droid.exe").is_file() {
        return insufficient("bin 下没有 droid.exe，无法判断哪些文件是旧版本");
    }

    let mut items = Vec::new();
    for (name, path) in child_files(&bin) {
        if !has_prefix_ignore_case(&name, "droid.exe.old-") {
            continue;
        }

        items.push(LeftoverItem {
            anchor: bin.clone(),
            rel: vec![OsString::from(&name)],
            is_dir: false,
            evidence: vec![
                "droid 更新时留下的旧可执行文件（droid.exe.old-*）".to_string(),
                format!("大小 {}", format_size(file_size(&path))),
            ],
        });
    }

    ready(items, Vec::new())
}

/// 千帆 updater 的安装包：`LocalAppData\qianfan-desktop-app-updater\installer.exe`。
fn qianfan_installer(roots: &RootMap, procs: &[ProcInfo]) -> AdapterResult {
    let Some(base) = roots
        .get(KnownRoot::LocalAppData)
        .map(|local| local.join("qianfan-desktop-app-updater"))
    else {
        return not_present();
    };
    if !dirinfo::is_plain_dir(&base) {
        return not_present();
    }

    let installer = base.join("installer.exe");
    if !installer.is_file() {
        return not_present();
    }

    // 有进程的映像就在这个目录下时，更新可能正在进行。
    let running: Vec<String> = procs
        .iter()
        .filter(|process| {
            process
                .image_path
                .as_ref()
                .is_some_and(|image| is_under(image, &base))
        })
        .map(|process| process.name.clone())
        .collect();

    with_running(
        vec![LeftoverItem {
            anchor: base,
            rel: vec![OsString::from("installer.exe")],
            is_dir: false,
            evidence: vec![
                "updater 下载的安装包 installer.exe".to_string(),
                format!("大小 {}", format_size(file_size(&installer))),
            ],
        }],
        Vec::new(),
        running,
    )
}

/// Docker Desktop 的更新包目录：`LocalAppData\Temp\DockerDesktopUpdates`。
fn docker_desktop_updates(roots: &RootMap, procs: &[ProcInfo]) -> AdapterResult {
    let Some(base) = roots
        .get(KnownRoot::LocalAppData)
        .map(|local| local.join("Temp\\DockerDesktopUpdates"))
    else {
        return not_present();
    };
    if !dirinfo::is_plain_dir(&base) {
        return not_present();
    }

    let mut items = Vec::new();
    for (name, path) in child_files(&base) {
        items.push(LeftoverItem {
            anchor: base.clone(),
            rel: vec![OsString::from(&name)],
            is_dir: false,
            evidence: vec![
                "Docker Desktop 更新下载目录中的文件".to_string(),
                format!("大小 {}", format_size(file_size(&path))),
            ],
        });
    }
    // 更新目录里也可能有解压出来的子目录，一并纳入；目录会在生成清单时展开成子树。
    for (name, _) in child_dirs(&base) {
        items.push(LeftoverItem {
            anchor: base.clone(),
            rel: vec![OsString::from(&name)],
            is_dir: true,
            evidence: vec!["Docker Desktop 更新下载目录中的子目录".to_string()],
        });
    }

    // Docker Desktop 的进程没退出时不碰更新包：可能正被安装程序使用。
    with_running(items, Vec::new(), running_names(DOCKER_PROCESSES, procs))
}

/// 联想应用商店的 cicada 下载缓存：`ProgramData\Lenovo\LeAppStore\cicada`。
///
/// 该目录在 ProgramData 下，普通权限只能读，清理要交给提权 helper。
fn le_appstore_cicada(roots: &RootMap, procs: &[ProcInfo]) -> AdapterResult {
    admin_data_dir(
        roots,
        procs,
        r"Lenovo\LeAppStore\cicada",
        LE_APPSTORE_PROCESSES,
    )
}

/// NVIDIA App 的 OTA 下载残留：`ProgramData\NVIDIA Corporation\NVIDIA App\UpdateFramework\ota-artifacts`。
fn nvidia_app_ota_artifacts(roots: &RootMap, procs: &[ProcInfo]) -> AdapterResult {
    admin_data_dir(
        roots,
        procs,
        r"NVIDIA Corporation\NVIDIA App\UpdateFramework\ota-artifacts",
        NVIDIA_APP_PROCESSES,
    )
}

/// ProgramData 下需要管理员权限的残留目录：整棵内容都算候选，实际隔离由提权 helper 执行。
///
/// 这里只做只读枚举和统计，主程序不写任何东西。
fn admin_data_dir(
    roots: &RootMap,
    procs: &[ProcInfo],
    rel: &str,
    product: &[&str],
) -> AdapterResult {
    let Some(base) = roots.get(KnownRoot::ProgramData) else {
        return not_present();
    };
    let target = base.join(rel);
    if !dirinfo::is_plain_dir(&target) {
        return not_present();
    }

    let mut items = Vec::new();

    for (name, _) in child_dirs(&target) {
        items.push(LeftoverItem {
            anchor: target.clone(),
            rel: vec![OsString::from(&name)],
            is_dir: true,
            evidence: vec![
                "需要管理员权限，由提权 helper 隔离".to_string(),
                "目录大小按整棵子树统计".to_string(),
            ],
        });
    }

    for (name, path) in child_files(&target) {
        items.push(LeftoverItem {
            anchor: target.clone(),
            rel: vec![OsString::from(&name)],
            is_dir: false,
            evidence: vec![
                "需要管理员权限，由提权 helper 隔离".to_string(),
                format!("大小 {}", format_size(file_size(&path))),
            ],
        });
    }

    if items.is_empty() {
        return not_present();
    }

    let running = running_for_target(&target, product, procs);

    with_running(
        items,
        vec![format!(
            "执行时只弹一次 UAC；相关进程：{}",
            product.join("、")
        )],
        running,
    )
}

/// 目标目录里有进程在运行，或者产品主进程在运行时，返回占用的进程名。
///
/// 映像路径落在目标目录下说明程序正从这个目录读文件；主进程名单独列出，
/// 因为主程序装在别的安装目录里。
pub fn running_for_target(target: &Path, product: &[&str], procs: &[ProcInfo]) -> Vec<String> {
    let mut running: Vec<String> = Vec::new();

    for process in procs {
        let in_target = process
            .image_path
            .as_ref()
            .is_some_and(|image| is_under(image, target));
        let is_product = product
            .iter()
            .any(|name| crate::win::equals_ignore_case(&process.name, name));

        if !in_target && !is_product {
            continue;
        }
        if running
            .iter()
            .any(|known| crate::win::equals_ignore_case(known, &process.name))
        {
            continue;
        }

        running.push(process.name.clone());
    }

    running
}

/// 未关联的 WSL 虚拟磁盘（只做分析）：
/// `LocalAppData\wsl\<{guid}>\ext4.vhdx` 里，目录不在当前用户 Lxss 任何 BasePath 下的那些。
fn wsl_unregistered_vhdx(roots: &RootMap) -> AdapterResult {
    let Some(base) = roots
        .get(KnownRoot::LocalAppData)
        .map(|local| local.join("wsl"))
    else {
        return not_present();
    };
    if !dirinfo::is_plain_dir(&base) {
        return not_present();
    }

    let registered = registered_wsl_bases();
    let mut items = Vec::new();

    for (name, dir) in child_dirs(&base) {
        let disk = dir.join("ext4.vhdx");
        if !disk.is_file() {
            continue;
        }
        if registered.iter().any(|known| same_path(known, &dir)) {
            continue;
        }

        let (_, mtime) = times(&disk);
        items.push(LeftoverItem {
            anchor: base.clone(),
            rel: vec![OsString::from(&name), OsString::from("ext4.vhdx")],
            is_dir: false,
            evidence: vec![
                format!("大小 {}", format_size(file_size(&disk))),
                format!("修改时间 {}", format_time(mtime)),
                "未在当前用户 WSL 注册表中找到关联".to_string(),
            ],
        });
    }

    if items.is_empty() {
        return not_present();
    }

    AdapterResult {
        status: CandidateStatus::Insufficient("只做分析：未关联虚拟磁盘".to_string()),
        items,
        notes: vec!["未关联虚拟磁盘只展示归属线索，可走大文件迁移".to_string()],
    }
}

/// 当前用户已注册 WSL 发行版的 BasePath。读不到注册表时返回空集合，
/// 这样只会把“看起来未关联”的磁盘列成证据，不会拿去执行。
fn registered_wsl_bases() -> Vec<PathBuf> {
    let Some(subkeys) = registry::subkeys(RegistryRoot::CurrentUser, LXSS_KEY) else {
        return Vec::new();
    };

    let mut bases = Vec::new();
    for subkey in subkeys {
        let key = format!("{LXSS_KEY}\\{subkey}");
        if let Some(base) = registry::read_string(RegistryRoot::CurrentUser, &key, "BasePath") {
            bases.push(PathBuf::from(base));
        }
    }

    bases
}

fn not_present() -> AdapterResult {
    AdapterResult {
        status: CandidateStatus::NotPresent,
        items: Vec::new(),
        notes: Vec::new(),
    }
}

fn insufficient(reason: &str) -> AdapterResult {
    AdapterResult {
        status: CandidateStatus::Insufficient(reason.to_string()),
        items: Vec::new(),
        notes: Vec::new(),
    }
}

/// 进程在运行时也保留条目：界面要显示会处理哪些内容，占用检测也要能查。
fn with_running(
    items: Vec<LeftoverItem>,
    notes: Vec<String>,
    running: Vec<String>,
) -> AdapterResult {
    if running.is_empty() {
        return ready(items, notes);
    }

    AdapterResult {
        status: CandidateStatus::ProcessRunning(running),
        items,
        notes,
    }
}

/// 没有条目时是“未找到”，有条目才是可执行的候选。
fn ready(items: Vec<LeftoverItem>, notes: Vec<String>) -> AdapterResult {
    let status = if items.is_empty() {
        CandidateStatus::NotPresent
    } else {
        CandidateStatus::Ready
    };

    AdapterResult {
        status,
        items,
        notes,
    }
}

/// 规则要求的进程里，当前正在运行的那些。
fn running_names(required: &[&str], procs: &[ProcInfo]) -> Vec<String> {
    required
        .iter()
        .filter(|required| {
            procs
                .iter()
                .any(|process| crate::win::equals_ignore_case(&process.name, required))
        })
        .map(|required| (*required).to_string())
        .collect()
}

/// 目录下的普通子目录，按名称排序。junction 和符号链接不算。
fn child_dirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !dirinfo::is_plain_dir(&path) {
            continue;
        }
        dirs.push((entry.file_name().to_string_lossy().into_owned(), path));
    }
    dirs.sort_by(|left, right| left.0.cmp(&right.0));

    dirs
}

/// 目录下的普通文件，按名称排序。符号链接不算。
fn child_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        // symlink_metadata 不跟随链接：链接本身既不是普通文件也不是目录，这里只收普通文件。
        if !metadata.is_file() {
            continue;
        }
        files.push((entry.file_name().to_string_lossy().into_owned(), path));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));

    files
}

/// 创建时间和最后写入时间（FILETIME 计时单位）。取不到时返回 0。
fn times(path: &Path) -> (u64, u64) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return (0, 0);
    };

    (metadata.creation_time(), metadata.last_write_time())
}

fn file_size(path: &Path) -> u64 {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// `.obsolete` 之外的临时目录名：`.` 加至少 8 个十六进制字符或连字符。
fn is_temp_dir_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };

    rest.len() >= 8 && rest.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// 不区分大小写的前缀判断。路径比较不能靠 `to_lowercase`，要用系统的有序比较规则。
fn has_prefix_ignore_case(text: &str, prefix: &str) -> bool {
    let mut text = text.chars();

    prefix.chars().all(|expected| {
        text.next()
            .is_some_and(|actual| crate::win::char_equals_ignore_case(expected, actual))
    })
}

/// 路径是否在某个目录之下（不含目录本身）。比较不区分大小写。
fn is_under(path: &Path, dir: &Path) -> bool {
    let mut prefix = dir
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_string();
    prefix.push('\\');

    has_prefix_ignore_case(&path.to_string_lossy(), &prefix)
}

/// 两个路径是否指同一个目录：去掉等价前缀和末尾分隔符后不区分大小写比较。
fn same_path(left: &Path, right: &Path) -> bool {
    crate::win::equals_ignore_case(&normalize(left), &normalize(right))
}

fn normalize(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\")
        .or_else(|| text.strip_prefix(r"\??\"))
        .unwrap_or(&text);

    text.trim_end_matches(['\\', '/']).to_string()
}

/// FILETIME 换算成本地时间的 `YYYY-MM-DD HH:MM`，只用在证据文字里。
fn format_time(filetime: u64) -> String {
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const SECONDS_TO_1601: i64 = 11_644_473_600;

    let seconds = (filetime / TICKS_PER_SECOND) as i64 - SECONDS_TO_1601;
    let Ok(at) = OffsetDateTime::from_unix_timestamp(seconds) else {
        return "未知".to_string();
    };

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        at.year(),
        at.month() as u8,
        at.day(),
        at.hour(),
        at.minute()
    )
}

/// 证据文字里的体积，二进制单位。
fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;

    let value = bytes as f64;
    if value >= KIB * KIB * KIB {
        format!("{:.2} GB", value / (KIB * KIB * KIB))
    } else if value >= KIB * KIB {
        format!("{:.2} MB", value / (KIB * KIB))
    } else if value >= KIB {
        format!("{:.2} KB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}
