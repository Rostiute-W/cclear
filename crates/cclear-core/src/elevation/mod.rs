mod args;
mod helper;
mod launch;
pub mod system_ops;

use std::ffi::OsStr;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::exec::file_cleaner::FileReport;
use crate::rules::RootMap;
use crate::scan::{ScanProgress, ScanResult};
use crate::settings;

pub use args::{ElevationError, HelperArgs, SystemOp, parse_helper_args};
pub use helper::helper_main;
pub use launch::command_line;

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperResult {
    pub outcomes: Vec<OpOutcome>,
    pub scan: Option<ScanResult>,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpOutcome {
    pub op: SystemOp,
    pub success: bool,
    pub detail: String,
    pub files: FileReport,
    pub reboot_scheduled: u64,
}

impl OpOutcome {
    pub fn new(op: SystemOp, success: bool, detail: impl Into<String>) -> Self {
        Self {
            op,
            success,
            detail: detail.into(),
            files: FileReport::default(),
            reboot_scheduled: 0,
        }
    }
}

pub struct RunOptions<'a> {
    pub temp_age: u32,
    pub reboot_delete: bool,
    pub mft: Option<(char, &'a ScanProgress)>,
    pub cancel: Option<&'a AtomicBool>,
}

impl Default for RunOptions<'_> {
    fn default() -> Self {
        Self {
            temp_age: 24,
            reboot_delete: false,
            mft: None,
            cancel: None,
        }
    }
}

pub fn run_elevated(
    roots: &RootMap,
    ops: &[SystemOp],
    options: RunOptions<'_>,
) -> Result<HelperResult, ElevationError> {
    let base = settings::data_dir(roots).map_err(ElevationError::Launch)?;
    let directory = settings::child_dir(&base, "elevated").map_err(ElevationError::Launch)?;
    let result_id = Uuid::new_v4();
    let executable =
        std::env::current_exe().map_err(|error| ElevationError::Launch(error.to_string()))?;
    let request = format!("{result_id}.request");
    crate::guard::create_file_in(&directory, OsStr::new(&request))
        .map_err(|error| ElevationError::Launch(error.to_string()))?;
    let mut args = vec![
        "--elevated".into(),
        "--ops".into(),
        ops.iter()
            .map(|op| op.token())
            .collect::<Vec<_>>()
            .join(","),
        "--result".into(),
        result_id.to_string(),
        "--temp-age".into(),
        options.temp_age.to_string(),
    ];
    if options.reboot_delete {
        args.push("--reboot-delete".into());
    }
    if let Some((drive, _)) = options.mft {
        args.extend(["--mft-drive".into(), drive.to_string()]);
    }
    let result = launch::runas_and_wait(&executable, &args, &directory, result_id, &options);
    let forbidden = crate::guard::Forbidden::empty();
    for extension in ["request", "json", "cancel", "progress"] {
        let filename = format!("{result_id}.{extension}");
        if let Ok(entry) = crate::guard::open_rel(
            &directory,
            &[filename.into()],
            crate::guard::OpenIntent::Delete,
            &forbidden,
        ) && !entry.is_dir
        {
            let expected = entry.expect();
            let _ = crate::guard::delete(entry, &expected);
        }
    }
    result
}
