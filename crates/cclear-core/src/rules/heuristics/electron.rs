use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crate::exec::file_cleaner::{Cleaner, Filters};
use crate::model::CleanSource;
use crate::rules::{
    RootMap,
    engine::{APP_ROOTS, Candidate, detect_paths, ordinary_children},
    forbidden::same_or_under,
};

const SIGNATURES: &[&str] = &[
    "Code Cache",
    "GPUCache",
    r"Cache\Cache_Data",
    r"Crashpad\reports",
    "Local State",
    "Preferences",
    "Network",
];
const CACHE_LEAVES: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "GrShaderCache",
    "ShaderCache",
    "DawnCache",
    r"Crashpad\reports",
];

pub fn detect(roots: &RootMap, cleaner: &Cleaner<'_>, covered: &[PathBuf]) -> Vec<Candidate> {
    let mut result = Vec::new();
    for root in APP_ROOTS.into_iter().filter_map(|root| roots.get(root)) {
        for parent in ordinary_children(root, cleaner.forbidden) {
            let children = ordinary_children(&parent, cleaner.forbidden);
            for app in std::iter::once(parent).chain(children) {
                if cleaner.cancel.load(Ordering::Relaxed) {
                    return result;
                }
                if cleaner.whitelisted(&app)
                    || covered
                        .iter()
                        .any(|path| same_or_under(path, &app) || same_or_under(&app, path))
                {
                    continue;
                }
                if detect_paths(&app, SIGNATURES, cleaner.forbidden).len() < 2 {
                    continue;
                }
                let paths = detect_paths(&app, CACHE_LEAVES, cleaner.forbidden);
                let measure = cleaner.measure(&paths, &Filters::default());
                if measure.bytes >= 1024 * 1024 {
                    let name = app.file_name().unwrap_or_default().to_string_lossy();
                    result.push(super::file_candidate(
                        format!("{name} 缓存"),
                        "Chromium 应用中可重新生成的缓存。",
                        CleanSource::HeuristicElectron,
                        paths,
                        measure,
                    ));
                }
            }
        }
    }
    result
}
