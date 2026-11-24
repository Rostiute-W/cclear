pub mod electron;
pub mod orphans;
pub mod uwp;

use std::path::PathBuf;

use crate::exec::file_cleaner::{FileSelection, Filters};
use crate::model::{CleanCategory, CleanItem, CleanSource, DeleteMode};
use crate::rules::engine::Candidate;
use crate::rules::schema::Target;

fn file_candidate(
    name: String,
    description: &str,
    source: CleanSource,
    paths: Vec<PathBuf>,
    measure: FileSelection,
) -> Candidate {
    let orphan = source == CleanSource::HeuristicOrphan;
    let mut hash = blake3::Hasher::new();
    for path in &paths {
        hash.update(path.to_string_lossy().as_bytes());
        hash.update(&[0]);
    }
    Candidate {
        item: CleanItem {
            id: format!("heuristic-{}", hash.finalize().to_hex()),
            name,
            description: description.into(),
            category: if orphan {
                CleanCategory::Leftover
            } else {
                CleanCategory::App
            },
            source,
            size_bytes: Some(measure.bytes),
            file_count: measure.files,
            default_selected: !orphan,
            requires_admin: false,
            delete_mode: if orphan {
                DeleteMode::RecycleBin
            } else {
                DeleteMode::Direct
            },
            warning: orphan
                .then(|| "可能仍有应用使用此目录。请检查路径，再决定是否移入回收站。".into()),
            running_processes: Vec::new(),
            sample_paths: measure.samples,
            anchors: paths,
            last_modified_at: None,
        },
        target: Target::Paths { paths: Vec::new() },
        filters: Filters::default(),
        processes: Vec::new(),
    }
}
