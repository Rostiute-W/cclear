use std::ffi::OsStr;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::exec::file_cleaner::Cleaner;
use crate::win;
use crate::{guard::Forbidden, rules::RootMap, scan::ScanProgress, settings};

use super::{HelperArgs, HelperResult, parse_helper_args, system_ops};

pub fn helper_main() -> i32 {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !win::is_elevated() {
        return 1;
    }
    let Ok(args) = parse_helper_args(&args) else {
        return 2;
    };
    let Ok(roots) = RootMap::system() else {
        return 3;
    };
    let Ok(data) = settings::data_dir(&roots) else {
        return 4;
    };
    let Ok(directory) = settings::child_dir(&data, "elevated") else {
        return 4;
    };
    let id = args.result;
    let forbidden = Forbidden::empty();
    let valid_request =
        crate::guard::path::read_file(directory.path(), &format!("{id}.request"), &forbidden)
            .and_then(|file| Ok(file.metadata()?.len() == 0))
            .unwrap_or(false);
    if !valid_request {
        return 6;
    }
    let name = format!("{}.json", args.result);
    let Ok(output) = crate::guard::create_file_in(&directory, OsStr::new(&name)) else {
        return 4;
    };
    let result = match settings::load_preferences(&roots) {
        Ok(preferences) => {
            let control = Control {
                cancel: AtomicBool::new(false),
                progress: ScanProgress::default(),
            };
            let mut progress_file = args.mft_drive.and_then(|_| {
                crate::guard::create_file_in(&directory, OsStr::new(&format!("{id}.progress"))).ok()
            });
            let cancel_path = directory.path().join(format!("{id}.cancel"));
            std::thread::scope(|scope| {
                let worker = scope.spawn(|| run(&args, &roots, &preferences, &control));
                while !worker.is_finished() {
                    if !control.cancel.load(Ordering::Relaxed)
                        && crate::guard::path::inspect_path(&cancel_path, &forbidden)
                            .is_ok_and(|entry| !entry.is_dir)
                    {
                        control.cancel.store(true, Ordering::Relaxed);
                    }
                    if let Some(file) = &mut progress_file {
                        let progress = &control.progress;
                        let counts = [
                            progress.dirs.load(Ordering::Relaxed),
                            progress.files.load(Ordering::Relaxed),
                            progress.allocated.load(Ordering::Relaxed),
                        ];
                        if writeln!(file, "{counts:?}").is_err() {
                            progress_file = None;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
                worker.join().unwrap_or_else(|_| HelperResult {
                    error: Some("管理员操作异常退出".into()),
                    ..HelperResult::default()
                })
            })
        }
        Err(error) => HelperResult {
            error: Some(error),
            ..HelperResult::default()
        },
    };
    let mut output = BufWriter::new(output);
    if serde_json::to_writer(&mut output, &result).is_err() || output.flush().is_err() {
        return 5;
    }
    0
}

struct Control {
    cancel: AtomicBool,
    progress: ScanProgress,
}

fn run(
    args: &HelperArgs,
    roots: &RootMap,
    preferences: &settings::Preferences,
    control: &Control,
) -> HelperResult {
    if let Some(drive) = args.mft_drive {
        let root = PathBuf::from(format!("{drive}:\\"));
        return match win::mft::scan_volume(&root, &control.progress, &control.cancel) {
            Ok(scan) => HelperResult {
                scan: Some(scan),
                ..HelperResult::default()
            },
            Err(error) => HelperResult {
                error: Some(error.to_string()),
                ..HelperResult::default()
            },
        };
    }
    let mut forbidden = Forbidden::from_rules(roots);
    forbidden.protect_paths(&preferences.whitelist_paths);
    let cleaner = Cleaner {
        forbidden: &forbidden,
        whitelist: &preferences.whitelist_paths,
        cancel: &control.cancel,
    };
    let temp_age = preferences.min_temp_age_hours.max(args.temp_age);
    let mut result = HelperResult::default();
    for &op in &args.ops {
        if control.cancel.load(Ordering::Relaxed) {
            break;
        }
        result.outcomes.push(system_ops::execute(
            op,
            roots,
            &cleaner,
            temp_age,
            args.reboot_delete,
        ));
    }
    result
}
