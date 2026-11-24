pub mod commands;
pub mod engine;
pub mod forbidden;
pub mod heuristics;
pub mod schema;

use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::model::KnownRoot;
use crate::win::{KnownFolder, known_folder};

pub struct RootMap {
    entries: Vec<(KnownRoot, PathBuf)>,
}

impl RootMap {
    pub fn system() -> Result<Self> {
        let mut entries = Vec::new();
        for (root, folder, label) in [
            (
                KnownRoot::LocalAppData,
                KnownFolder::LocalAppData,
                "本地应用数据目录",
            ),
            (
                KnownRoot::RoamingAppData,
                KnownFolder::RoamingAppData,
                "漫游应用数据目录",
            ),
            (KnownRoot::Profile, KnownFolder::Profile, "用户目录"),
            (
                KnownRoot::ProgramData,
                KnownFolder::ProgramData,
                "ProgramData",
            ),
            (KnownRoot::Windows, KnownFolder::Windows, "Windows"),
        ] {
            let path = known_folder(folder).map_err(|_| CoreError::KnownFolder(label))?;
            if root == KnownRoot::Windows {
                let drive = path.parent().ok_or(CoreError::KnownFolder("系统盘"))?;
                entries.push((KnownRoot::SystemDrive, drive.to_path_buf()));
            }
            entries.push((root, path));
        }
        for (root, folder) in [
            (KnownRoot::Documents, KnownFolder::Documents),
            (KnownRoot::Downloads, KnownFolder::Downloads),
            (KnownRoot::ProgramFiles, KnownFolder::ProgramFiles),
            (KnownRoot::ProgramFilesX86, KnownFolder::ProgramFilesX86),
            (KnownRoot::OneDrive, KnownFolder::SkyDrive),
        ] {
            if let Ok(path) = known_folder(folder) {
                entries.push((root, path));
            }
        }
        Ok(Self { entries })
    }

    pub fn with(entries: Vec<(KnownRoot, PathBuf)>) -> Self {
        Self { entries }
    }

    pub fn get(&self, root: KnownRoot) -> Option<&Path> {
        self.entries
            .iter()
            .find(|(known, _)| *known == root)
            .map(|(_, path)| path.as_path())
    }
}
