use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum EntryType {
    OurSymlink,
    ForeignSymlink,
    RealDir,
    NotExists,
}

pub struct Linker;

impl Linker {
    pub fn create_link(target: &Path, link: &Path) -> Result<()> {
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent)?;
        }

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).with_context(|| {
                format!(
                    "failed to symlink {} -> {}",
                    link.display(),
                    target.display()
                )
            })?;
        }

        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link).with_context(|| {
                format!(
                    "failed to symlink {} -> {}",
                    link.display(),
                    target.display()
                )
            })?;
        }

        Ok(())
    }

    pub fn remove_link(link: &Path) -> Result<()> {
        if Self::is_symlink(link) {
            #[cfg(unix)]
            std::fs::remove_file(link)?;
            #[cfg(windows)]
            std::fs::remove_dir(link)?;
        }
        Ok(())
    }

    pub fn is_symlink(path: &Path) -> bool {
        path.symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    }

    pub fn is_our_symlink(path: &Path, our_base: &Path) -> bool {
        if !Self::is_symlink(path) {
            return false;
        }
        match std::fs::read_link(path) {
            Ok(target) => {
                let resolved = if target.is_absolute() {
                    target
                } else {
                    path.parent().unwrap_or(Path::new(".")).join(&target)
                };
                resolved.starts_with(our_base)
            }
            Err(_) => false,
        }
    }

    pub fn detect_entry_type(path: &Path, our_base: &Path) -> EntryType {
        if !path.exists() && !Self::is_symlink(path) {
            return EntryType::NotExists;
        }

        if Self::is_symlink(path) {
            if Self::is_our_symlink(path, our_base) {
                EntryType::OurSymlink
            } else {
                EntryType::ForeignSymlink
            }
        } else if path.is_dir() {
            EntryType::RealDir
        } else {
            EntryType::NotExists
        }
    }

    pub fn adopt_to_managed(
        source_path: &Path,
        managed_dir: &Path,
        link_path: &Path,
    ) -> Result<()> {
        if source_path != managed_dir {
            if managed_dir.exists() {
                std::fs::remove_dir_all(managed_dir)?;
            }
            Self::move_dir(source_path, managed_dir)?;
        }
        if link_path.exists() || Self::is_symlink(link_path) {
            Self::remove_link(link_path)?;
            if link_path.is_dir() {
                std::fs::remove_dir_all(link_path)?;
            }
        }
        Self::create_link(managed_dir, link_path)?;
        Ok(())
    }

    pub fn move_dir(from: &Path, to: &Path) -> Result<()> {
        if std::fs::rename(from, to).is_ok() {
            return Ok(());
        }
        Self::copy_dir_recursive(from, to)?;
        std::fs::remove_dir_all(from)?;
        Ok(())
    }

    pub fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let dest = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                Self::copy_dir_recursive(&entry.path(), &dest)?;
            } else {
                std::fs::copy(entry.path(), dest)?;
            }
        }
        Ok(())
    }
}
