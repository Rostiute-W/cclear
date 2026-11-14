//! helper 模式的入口：校验提权状态、解析内置操作、执行并写结果文件。

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use uuid::Uuid;

use crate::guard::{self, Forbidden};
use crate::journal::{FileJournal, JournalSink, Record, unix_now};
use crate::model::{KnownRoot, Recovery};
use crate::plan;
use crate::quarantine::{BatchStage, QuarantineItem, QuarantineRoot};
use crate::rules::adapters::{self, LE_APPSTORE_PROCESSES, NVIDIA_APP_PROCESSES};
use crate::rules::forbidden as rules_forbidden;
use crate::rules::resolve::RootMap;
use crate::win::process::running_processes;
use crate::win::volume;
use crate::win::{KnownFolder, dirinfo, equals_ignore_case, known_folder};

use super::args::{HelperArgs, SystemOp};
use super::{ElevationError, HelperResult, OpOutcome, maintenance, windows_files};

/// 联想应用商店的 cicada 目录（相对 ProgramData）。路径由 helper 自己解析，不接受命令行传入。
const LE_APPSTORE_CICADA: &str = r"Lenovo\LeAppStore\cicada";
/// NVIDIA App 的 OTA 下载残留（相对 ProgramData）。
const NVIDIA_OTA_ARTIFACTS: &str = r"NVIDIA Corporation\NVIDIA App\UpdateFramework\ota-artifacts";

/// 提权后仍然不许碰的 Windows 子目录（相对 Windows 目录）。
const WINDOWS_PROTECTED: &[&str] = &[
    r"System32",
    r"SysWOW64",
    r"WinSxS",
    r"Installer",
    r"servicing",
    r"System32\DriverStore",
];

/// helper 进程的入口。返回进程退出码。
///
/// 参数里带 `--elevated` 时 `src-tauri` 的 main 直接调用它，不创建窗口。
pub fn helper_main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // 提权状态不对时一个系统路径都不能碰；能解析出结果路径就留一条失败结果给主程序。
    if !crate::win::is_elevated() {
        if let Ok(parsed) = super::parse_helper_args(&args) {
            let result = failed_result("没有以管理员权限运行，helper 已退出");
            let _ = write_result(&parsed.result, &result);
        }
        return 1;
    }

    let args = match super::parse_helper_args(&args) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    match run_ops(&args) {
        Ok(result) => match write_result(&args.result, &result) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("提权结果写入失败：{error}");
                3
            }
        },
        Err(error) => {
            let result = failed_result(&error.to_string());
            let _ = write_result(&args.result, &result);
            eprintln!("{error}");
            4
        }
    }
}

/// 按命令行里的顺序执行全部操作。
fn run_ops(args: &HelperArgs) -> Result<HelperResult, ElevationError> {
    let roots = RootMap::system()?;
    let forbidden = helper_forbidden(&roots);

    // 参数里的格式检查管不了卷本身，执行前再查一次隔离根的卷属性。
    let quarantine_root = match &args.quarantine_root {
        Some(path) => {
            validate_quarantine_root(path)?;
            Some(path.as_path())
        }
        None => None,
    };

    // helper 的日志和主程序分开写，放在 ProgramData 下；目录不存在时创建。
    let journal_dir = known_folder(KnownFolder::ProgramData)
        .map_err(|_| ElevationError::KnownFolder("ProgramData"))?
        .join("CClear")
        .join("journal");
    let mut journal =
        FileJournal::at(journal_dir).map_err(|error| ElevationError::Journal(error.to_string()))?;

    let mut outcomes = Vec::with_capacity(args.ops.len());
    for op in &args.ops {
        let outcome = run_op(
            *op,
            &forbidden,
            &roots,
            quarantine_root,
            args.progress.as_deref(),
            &mut journal,
        );

        // 每个操作都先写日志再交结果：日志写不进去就停手，不留没有记录的操作。
        journal
            .append(&Record::SpecialAction {
                at: unix_now(),
                action: format!("elevated:{}", op.token()),
                detail: outcome.detail.clone(),
            })
            .map_err(|error| ElevationError::Journal(error.to_string()))?;

        outcomes.push(outcome);
    }

    Ok(HelperResult { ops: outcomes })
}

/// 执行一个内置操作。
///
/// 文件类操作在 helper 里现生成冻结清单，再交给执行器逐项删除；
/// 长耗时操作走 DISM 和官方 cmdlet，进度写进 `--progress` 指定的文件。
fn run_op(
    op: SystemOp,
    forbidden: &Forbidden,
    roots: &RootMap,
    quarantine_root: Option<&Path>,
    progress: Option<&Path>,
    journal: &mut dyn JournalSink,
) -> OpOutcome {
    match op {
        SystemOp::LeAppStoreCicada => quarantine_program_data(
            op,
            LE_APPSTORE_CICADA,
            "adapter-leappstore-cicada",
            LE_APPSTORE_PROCESSES,
            forbidden,
            quarantine_root,
            journal,
        ),
        SystemOp::NvidiaOta => quarantine_program_data(
            op,
            NVIDIA_OTA_ARTIFACTS,
            "adapter-nvidia-app-ota-artifacts",
            NVIDIA_APP_PROCESSES,
            forbidden,
            quarantine_root,
            journal,
        ),
        SystemOp::RestoreBatch(id) => restore_batch(op, id, quarantine_root, journal),
        SystemOp::PurgeBatch(id) => purge_batch(op, id, quarantine_root, journal),
        SystemOp::WindowsTemp | SystemOp::WindowsLogs | SystemOp::SysReset => {
            windows_files::run(op, forbidden, roots, journal)
        }
        SystemOp::DismAnalyze | SystemOp::DismComponentCleanup | SystemOp::DeliveryOptimization => {
            maintenance::run(op, roots, progress)
        }
    }
}

/// 把 ProgramData 下的目标目录内容隔离。路径由 helper 用 Known Folder 解析。
fn quarantine_program_data(
    op: SystemOp,
    rel: &str,
    rule_id: &'static str,
    product: &[&str],
    forbidden: &Forbidden,
    quarantine_root: Option<&Path>,
    journal: &mut dyn JournalSink,
) -> OpOutcome {
    let token = op.token();

    let Some(root_path) = quarantine_root else {
        return OpOutcome::failed(token, "缺少隔离根目录".to_string());
    };
    let Ok(program_data) = known_folder(KnownFolder::ProgramData) else {
        return OpOutcome::failed(token, "无法解析 ProgramData 目录".to_string());
    };

    let target = program_data.join(rel);
    if !dirinfo::is_plain_dir(&target) {
        return OpOutcome::skipped(token, format!("目录不存在：{}", target.display()));
    }

    // 主程序退出、并且没有进程从目标目录里运行的时候才处理。
    let procs = running_processes();
    let running = adapters::running_for_target(&target, product, &procs);
    if !running.is_empty() {
        return OpOutcome::skipped(
            token,
            format!("相关程序正在运行，已跳过：{}", running.join("、")),
        );
    }

    // 清单按当前状态重新生成：哪些文件能隔离、哪些因链接或禁区跳过，都由它决定。
    let anchors = [target];
    let frozen = plan::build_plan_at(
        rule_id,
        Recovery::Quarantine,
        &anchors,
        forbidden,
        &procs,
        SystemTime::now(),
    );
    let plan_rule = &frozen.rules[0];

    let mut items = Vec::new();
    for item in &frozen.items[plan_rule.items.start as usize..plan_rule.items.end as usize] {
        // 隔离引擎只复制文件；目录留在原处，界面会说明这一点。
        if item.is_dir {
            continue;
        }
        items.push(QuarantineItem {
            anchor_index: item.anchor_index,
            rel: item.rel.clone(),
        });
    }

    if items.is_empty() {
        return OpOutcome::skipped(token, "没有可隔离的文件".to_string());
    }

    // helper 没有取消按钮，取消开关恒为 false。
    let cancel = AtomicBool::new(false);
    let root = QuarantineRoot::open(root_path.to_path_buf()).with_requires_admin(true);

    match root.quarantine(
        rule_id,
        forbidden,
        &plan_rule.anchors,
        &items,
        journal,
        &cancel,
    ) {
        Ok(outcome) => {
            let detail = format!(
                "隔离批次 {}：{}，已隔离 {} 个文件",
                outcome.batch_id,
                outcome.stage.label(),
                outcome.moved
            );

            let mut result = if outcome.stage == BatchStage::Committed {
                OpOutcome::done(token, detail)
            } else {
                OpOutcome::skipped(token, detail)
            };
            result.batch_id = Some(outcome.batch_id.to_string());
            result.items = outcome.moved;
            result
        }
        Err(error) => OpOutcome::failed(token, error.to_string()),
    }
}

/// 还原一个隔离批次。批次可能是主程序创建的，但源在 ProgramData 下，必须提权。
fn restore_batch(
    op: SystemOp,
    batch_id: Uuid,
    quarantine_root: Option<&Path>,
    journal: &mut dyn JournalSink,
) -> OpOutcome {
    let token = op.token();

    let Some(root_path) = quarantine_root else {
        return OpOutcome::failed(token, "缺少隔离根目录".to_string());
    };

    let root = QuarantineRoot::open(root_path.to_path_buf());
    match root.restore(batch_id, journal) {
        Ok(outcome) => {
            let detail = format!(
                "还原批次 {batch_id}：{}，已还原 {} 个文件，{} 个因原位置已有同名文件跳过",
                outcome.stage.label(),
                outcome.restored,
                outcome.conflicts.len()
            );

            let mut result = if outcome.stage == BatchStage::Restored {
                OpOutcome::done(token, detail)
            } else {
                OpOutcome::skipped(token, detail)
            };
            result.batch_id = Some(batch_id.to_string());
            result.items = outcome.restored;
            result
        }
        Err(error) => OpOutcome::failed(token, error.to_string()),
    }
}

/// 彻底删除一个隔离批次。
fn purge_batch(
    op: SystemOp,
    batch_id: Uuid,
    quarantine_root: Option<&Path>,
    journal: &mut dyn JournalSink,
) -> OpOutcome {
    let token = op.token();

    let Some(root_path) = quarantine_root else {
        return OpOutcome::failed(token, "缺少隔离根目录".to_string());
    };

    let root = QuarantineRoot::open(root_path.to_path_buf());
    match root.purge(batch_id, journal) {
        Ok(()) => {
            let mut result = OpOutcome::done(token, format!("已彻底删除批次 {batch_id}"));
            result.batch_id = Some(batch_id.to_string());
            result
        }
        Err(error) => OpOutcome::failed(token, error.to_string()),
    }
}

/// 提权 helper 的禁区：目录规则的禁区去掉整个 Windows 根目录，
/// 再显式加入系统关键子目录。helper 只处理明确的目标目录，不能顺手碰这些位置。
fn helper_forbidden(roots: &RootMap) -> Forbidden {
    let windows = roots.get(KnownRoot::Windows);
    let mut ids = HashSet::new();

    for dir in rules_forbidden::forbidden_dirs(roots) {
        // Windows 根整体禁止会让 M5b 的 Windows\Temp 这类操作没法执行，改成下面的窄范围子目录。
        if let Some(base) = windows
            && equals_ignore_case(&dir.to_string_lossy(), &base.to_string_lossy())
        {
            continue;
        }

        if let Ok(handle) = dirinfo::open_dir(&dir)
            && let Ok(identity) = guard::path::identity_of(&handle)
        {
            ids.insert(identity);
        }
    }

    for rel in WINDOWS_PROTECTED {
        let Some(base) = windows else {
            break;
        };

        let path = base.join(*rel);
        if let Ok(handle) = dirinfo::open_dir(&path)
            && let Ok(identity) = guard::path::identity_of(&handle)
        {
            ids.insert(identity);
        }
    }

    Forbidden {
        ids,
        names: rules_forbidden::forbidden_names()
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
    }
}

/// 隔离根目录必须是固定 NTFS 的非系统卷。格式已经在参数解析里校验过。
fn validate_quarantine_root(path: &Path) -> Result<(), ElevationError> {
    let text = path.to_string_lossy();
    let Some(drive_root) = text.get(..3) else {
        return Err(ElevationError::QuarantineRoot(format!(
            "路径不是盘符根目录：{}",
            path.display()
        )));
    };

    let volume = volume::list_fixed_volumes()
        .into_iter()
        .find(|info| equals_ignore_case(&info.root.to_string_lossy(), drive_root));

    let Some(volume) = volume else {
        return Err(ElevationError::QuarantineRoot(format!(
            "{} 不存在或不是本地固定盘",
            drive_root
        )));
    };

    if !equals_ignore_case(&volume.fs, "NTFS") {
        return Err(ElevationError::QuarantineRoot(format!(
            "{} 不是 NTFS 卷",
            drive_root
        )));
    }
    if volume.is_system {
        return Err(ElevationError::QuarantineRoot(format!(
            "{} 是系统盘，不能作为隔离区",
            drive_root
        )));
    }

    Ok(())
}

/// 把结果写到 `--result` 指定的文件。目录不存在时创建。
fn write_result(path: &Path, result: &HelperResult) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let json = serde_json::to_vec_pretty(result).map_err(io::Error::other)?;
    std::fs::write(path, json)
}

/// 整个 helper 无法开始时的结果。
fn failed_result(detail: &str) -> HelperResult {
    HelperResult {
        ops: vec![OpOutcome::failed("helper".to_string(), detail.to_string())],
    }
}
