use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct AppPaths {
    base: PathBuf,
}

impl AppPaths {
    pub fn default_path() -> Self {
        let base = if cfg!(windows) {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("skill-manager")
        } else {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".skill-manager")
        };
        Self { base }
    }

    pub fn with_base(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn data_dir(&self) -> &Path {
        &self.base
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.base.join("skills")
    }

    pub fn mcps_dir(&self) -> PathBuf {
        self.base.join("mcps")
    }

    pub fn groups_dir(&self) -> PathBuf {
        self.base.join("groups")
    }

    pub fn db_path(&self) -> PathBuf {
        self.base.join("skill-manager.db")
    }

    pub fn config_path(&self) -> PathBuf {
        self.base.join("config.toml")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(self.skills_dir())?;
        std::fs::create_dir_all(self.mcps_dir())?;
        std::fs::create_dir_all(self.groups_dir())?;
        Ok(())
    }
}
