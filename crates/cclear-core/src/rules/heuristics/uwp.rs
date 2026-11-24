use crate::exec::file_cleaner::{Cleaner, Filters};
use crate::model::{CleanSource, KnownRoot};
use crate::rules::{
    RootMap,
    engine::{Candidate, detect_paths},
};

pub fn detect(roots: &RootMap, cleaner: &Cleaner<'_>) -> Option<Candidate> {
    let root = roots.get(KnownRoot::LocalAppData)?;
    let paths = detect_paths(
        root,
        &[r"Packages\*\TempState", r"Packages\*\AC\INetCache"],
        cleaner.forbidden,
    );
    let measure = cleaner.measure(&paths, &Filters::default());
    if measure.bytes < 1024 * 1024 {
        return None;
    }
    Some(super::file_candidate(
        "Microsoft Store 应用临时缓存".into(),
        "应用包的临时目录和网络缓存。",
        CleanSource::HeuristicUwp,
        paths,
        measure,
    ))
}
