//! Cross-platform symlink wrapper — the ONLY place that creates / removes /
//! introspects the links that encode "skill enabled".
//!
//! runai's truth for an enabled skill is "a symlink exists pointing into
//! `~/.runai/skills/<name>`". All such links flow through this module.
//!
//! ## Public surface
//! - `create_link(target, link)` — symlinks a directory (unix `symlink`,
//!   windows `symlink_dir`); creates the link's parent first.
//! - `create_link_force(target, link)` — removes a pre-existing symlink at
//!   `link` then `create_link`, so a stale link is clobbered instead of EEXIST.
//! - `remove_link(link)` — removes only if `link` is a symlink (unix
//!   `remove_file`, windows `remove_dir`); no-op otherwise.
//! - `is_symlink(path)` — via `symlink_metadata`, does NOT follow links.
//! - `is_our_symlink(path, our_base)` — reads the target (joining relative
//!   targets against the link's parent) and checks `starts_with(our_base)`.
//! - `detect_entry_type(path, our_base) -> EntryType`
//!   (`OurSymlink` / `ForeignSymlink` / `RealDir` / `NotExists`).
//! - `adopt_to_managed(src, managed_dir, link_path)` — move src → managed_dir
//!   (clobbering an existing managed_dir), then relink. Scanner adoption.
//! - `move_dir` / `copy_dir_recursive` — rename with cross-filesystem fallback.
//!
//! ## Invariants / gotchas
//! - Symlink target is always the managed DIR, never a file — `symlink_dir`
//!   (Windows) is required; `symlink_file` fails silently on dir targets.
//! - A dangling symlink reports as `OurSymlink` / `ForeignSymlink`, never
//!   `NotExists` (truth = link exists, dangling included).
//! - `adopt_to_managed` always removes a pre-existing link first, even if it
//!   already points correctly — simpler and cheap.
//! - Windows `symlink_dir` needs Developer Mode or Administrator, else
//!   `ERROR_PRIVILEGE_NOT_HELD`.
//! - `move_dir` falls back to recursive-copy-then-delete when `std::fs::rename`
//!   fails across filesystems — do NOT remove the fallback.

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

    /// Like `create_link` but if `link` already exists as a symlink (including a
    /// dangling one), unlink it first and recreate. Used by `enable_resource`
    /// to recover from prior failed scan / smoke-test states where a stale
    /// symlink at the target path would otherwise cause the underlying
    /// `fs::symlink` syscall to fail with EEXIST.
    ///
    /// We only clobber symlinks — never real directories — so a hand-crafted
    /// real dir at the link path is still left alone (the bare `fs::symlink`
    /// call below will then fail loudly, which is the right behavior).
    pub fn create_link_force(target: &Path, link: &Path) -> Result<()> {
        if Self::is_symlink(link) {
            Self::remove_link(link)?;
        }
        Self::create_link(target, link)
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

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::*;

    #[test]
    fn create_link_force_overwrites_dangling_symlink() {
        // Regression for bug 4 from 2026-04-27 incident — `enable_resource`
        // was hitting EEXIST because a stale symlink from a prior session
        // already occupied the link path. `create_link_force` must clobber.
        let tmp = tempfile::tempdir().unwrap();
        let real_target = tmp.path().join("real-target");
        std::fs::create_dir_all(&real_target).unwrap();
        let stale_target = tmp.path().join("does-not-exist");
        let link = tmp.path().join("the-link");

        // Pre-existing dangling symlink — `link.exists()` returns false here.
        std::os::unix::fs::symlink(&stale_target, &link).unwrap();
        assert!(Linker::is_symlink(&link));
        assert!(!link.exists(), "link must be dangling for this test");

        Linker::create_link_force(&real_target, &link).unwrap();

        assert!(Linker::is_symlink(&link));
        let resolved = std::fs::read_link(&link).unwrap();
        assert_eq!(
            resolved, real_target,
            "link must be repointed to real target"
        );
        assert!(link.exists(), "link must now resolve");
    }

    #[test]
    fn create_link_without_force_fails_on_existing_symlink() {
        // Confirms why we needed create_link_force in the first place: the
        // bare create_link call wraps fs::symlink, which fails with EEXIST
        // even when the existing entry is a dangling symlink.
        let tmp = tempfile::tempdir().unwrap();
        let real_target = tmp.path().join("real-target");
        std::fs::create_dir_all(&real_target).unwrap();
        let link = tmp.path().join("the-link");
        std::os::unix::fs::symlink(tmp.path().join("nope"), &link).unwrap();

        let err = Linker::create_link(&real_target, &link).unwrap_err();
        assert!(
            err.to_string().contains("symlink") || err.to_string().to_lowercase().contains("exist"),
            "expected symlink/exists error, got: {err}"
        );
    }
}
