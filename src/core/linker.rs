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
//! - `remove_link`'s safety boundary: it ONLY ever unlinks a symlink dentry.
//!   A real directory or regular file at the same path is left completely
//!   untouched (no `is_symlink` match => no-op) — it can never degrade into
//!   `rm -rf`. It also never follows through to the symlink's *target*: only
//!   the link dentry disappears, the thing it pointed at survives (this is
//!   what makes it safe to call on a `ForeignSymlink` too).
//! - `adopt_to_managed` DOES delete whatever real directory/content is
//!   already sitting at `managed_dir` (via `remove_dir_all`) before moving
//!   the source in — this is intentional (adopt = "this name's managed
//!   content is now exactly this source"), not a bug, but it means callers
//!   must be sure `managed_dir` is disposable before calling.
//! - `move_dir`'s fallback (`copy_dir_recursive` + `remove_dir_all(from)`)
//!   does NOT clear pre-existing content at `to` first — it merges the
//!   source's files into whatever is already there. In the one production
//!   call site (`adopt_to_managed`) this never matters because the caller
//!   already wiped `managed_dir`, but a new caller of `move_dir` directly
//!   must not assume `to` ends up looking exactly like `from`.
//!
//! ## Tests (issue #27)
//! `#[cfg(test)] mod tests` covers every function above in a `tempfile`
//! sandbox: `remove_link`'s three "never touch real content" boundaries
//! (real dir / real file / foreign symlink's target survives), `is_our_symlink`
//! (absolute/relative/dangling target resolution, false for non-symlinks and
//! out-of-base targets), `detect_entry_type`'s four variants plus the
//! dangling-symlink-is-never-`NotExists` invariant, `adopt_to_managed`'s
//! move+link / managed_dir-overwrite / stale-link-replace / same-path-skip
//! branches, `move_dir`'s rename-fast-path and a REAL (not mocked)
//! rename-failure fallback forced via `ENOTEMPTY`, and `copy_dir_recursive`'s
//! nested-subdir + empty-dir handling.

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

    // =====================================================================
    //  issue #27 — remove_link / is_our_symlink / detect_entry_type /
    //  adopt_to_managed / move_dir / copy_dir_recursive.
    //
    //  These are the "危险 syscall" primitives named in AGENTS.md's safety
    //  contract (root cause module of the 2026-04-20 / 2026-04-27 permanent
    //  data-loss incidents). Every test below runs inside a fresh
    //  `tempfile::tempdir()` sandbox — never touches a path outside it.
    // =====================================================================

    #[test]
    fn remove_link_is_noop_on_missing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        // Must not error just because there is nothing there.
        Linker::remove_link(&missing).unwrap();
        assert!(!missing.exists());
    }

    #[test]
    fn remove_link_never_deletes_a_real_directory() {
        // The core safety boundary: remove_link only ever unlinks a symlink
        // dentry. A real directory sitting at the same path (e.g. a
        // not-yet-adopted skill dir) must survive untouched — remove_link
        // must never degrade into `rm -rf`.
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real-skill-dir");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(real_dir.join("SKILL.md"), "content").unwrap();

        Linker::remove_link(&real_dir).unwrap();

        assert!(real_dir.exists(), "real directory must survive remove_link");
        assert!(
            real_dir.join("SKILL.md").exists(),
            "its contents must survive too"
        );
    }

    #[test]
    fn remove_link_never_deletes_a_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let real_file = tmp.path().join("not-a-symlink.txt");
        std::fs::write(&real_file, "keep me").unwrap();

        Linker::remove_link(&real_file).unwrap();

        assert!(real_file.exists(), "regular file must survive remove_link");
    }

    #[test]
    fn remove_link_removes_a_foreign_symlink_without_touching_its_target() {
        // "非本项目 symlink 不误删" boundary: remove_link unlinks the symlink
        // dentry itself but must never reach through to delete what it
        // points at, even when the target is completely unrelated to any
        // managed dir.
        let tmp = tempfile::tempdir().unwrap();
        let foreign_target = tmp.path().join("someone-elses-real-dir");
        std::fs::create_dir_all(&foreign_target).unwrap();
        std::fs::write(foreign_target.join("keep.txt"), "important").unwrap();
        let link = tmp.path().join("foreign-link");
        std::os::unix::fs::symlink(&foreign_target, &link).unwrap();

        Linker::remove_link(&link).unwrap();

        assert!(
            !link.exists() && !Linker::is_symlink(&link),
            "the link dentry is gone"
        );
        assert!(
            foreign_target.exists(),
            "the foreign target directory must survive"
        );
        assert!(
            foreign_target.join("keep.txt").exists(),
            "the foreign target's contents must survive"
        );
    }

    #[test]
    fn remove_link_removes_a_dangling_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("dangling-link");
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), &link).unwrap();
        assert!(Linker::is_symlink(&link));

        Linker::remove_link(&link).unwrap();

        assert!(!Linker::is_symlink(&link));
    }

    #[test]
    fn is_our_symlink_true_for_absolute_target_under_base() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("managed");
        std::fs::create_dir_all(base.join("foo")).unwrap();
        let link = tmp.path().join("link-to-foo");
        std::os::unix::fs::symlink(base.join("foo"), &link).unwrap();

        assert!(Linker::is_our_symlink(&link, &base));
    }

    #[test]
    fn is_our_symlink_false_for_target_outside_base() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("managed");
        std::fs::create_dir_all(&base).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let link = tmp.path().join("link-to-elsewhere");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();

        assert!(!Linker::is_our_symlink(&link, &base));
    }

    #[test]
    fn is_our_symlink_false_for_non_symlink_path() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("managed");
        std::fs::create_dir_all(&base).unwrap();
        let real_dir = tmp.path().join("just-a-dir");
        std::fs::create_dir_all(&real_dir).unwrap();

        assert!(!Linker::is_our_symlink(&real_dir, &base));
    }

    #[test]
    fn is_our_symlink_resolves_relative_target_against_link_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("managed");
        std::fs::create_dir_all(base.join("foo")).unwrap();
        let link = tmp.path().join("rel-link");
        // Relative target: resolved against the link's parent dir per the
        // implementation, so "managed/foo" from tmp.path() must resolve
        // correctly.
        std::os::unix::fs::symlink(Path::new("managed/foo"), &link).unwrap();

        assert!(Linker::is_our_symlink(&link, &base));
    }

    #[test]
    fn is_our_symlink_true_for_dangling_symlink_recorded_under_base() {
        // Per the module invariant: "A dangling symlink reports as
        // OurSymlink/ForeignSymlink, never NotExists" — is_our_symlink must
        // decide based on the *recorded* target path, not whether it
        // resolves to something that exists.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("managed");
        std::fs::create_dir_all(&base).unwrap();
        let link = tmp.path().join("dangling-but-ours");
        std::os::unix::fs::symlink(base.join("gone"), &link).unwrap();
        assert!(
            !base.join("gone").exists(),
            "target must not exist for this test"
        );

        assert!(Linker::is_our_symlink(&link, &base));
    }

    #[test]
    fn detect_entry_type_covers_all_four_variants() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("managed");
        std::fs::create_dir_all(base.join("foo")).unwrap();

        let missing = tmp.path().join("missing");
        assert_eq!(
            Linker::detect_entry_type(&missing, &base),
            EntryType::NotExists
        );

        let our_link = tmp.path().join("our-link");
        std::os::unix::fs::symlink(base.join("foo"), &our_link).unwrap();
        assert_eq!(
            Linker::detect_entry_type(&our_link, &base),
            EntryType::OurSymlink
        );

        let foreign_dir = tmp.path().join("foreign-real-dir");
        std::fs::create_dir_all(&foreign_dir).unwrap();
        let foreign_link = tmp.path().join("foreign-link");
        std::os::unix::fs::symlink(&foreign_dir, &foreign_link).unwrap();
        assert_eq!(
            Linker::detect_entry_type(&foreign_link, &base),
            EntryType::ForeignSymlink
        );

        let real_dir = tmp.path().join("plain-real-dir");
        std::fs::create_dir_all(&real_dir).unwrap();
        assert_eq!(
            Linker::detect_entry_type(&real_dir, &base),
            EntryType::RealDir
        );
    }

    #[test]
    fn detect_entry_type_dangling_symlink_is_never_not_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("managed");
        std::fs::create_dir_all(&base).unwrap();

        let dangling_ours = tmp.path().join("dangling-ours");
        std::os::unix::fs::symlink(base.join("gone"), &dangling_ours).unwrap();
        assert_eq!(
            Linker::detect_entry_type(&dangling_ours, &base),
            EntryType::OurSymlink,
            "dangling but recorded-target-under-base must still be OurSymlink"
        );

        let dangling_foreign = tmp.path().join("dangling-foreign");
        std::os::unix::fs::symlink(tmp.path().join("also-gone"), &dangling_foreign).unwrap();
        assert_eq!(
            Linker::detect_entry_type(&dangling_foreign, &base),
            EntryType::ForeignSymlink,
            "dangling and outside base must still be ForeignSymlink, not NotExists"
        );
    }

    #[test]
    fn detect_entry_type_regular_file_reports_not_exists() {
        // Documents the current implementation's behavior for a plain file
        // (neither a symlink nor a directory): the function only recognizes
        // symlinks and directories as "an entry", so a bare file falls
        // through to NotExists. Not a bug in scope for this module — every
        // real caller only ever points this at skill directories/symlinks —
        // but pinned here so a future refactor can't silently change it
        // without a test noticing.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("managed");
        std::fs::create_dir_all(&base).unwrap();
        let file = tmp.path().join("plain-file.txt");
        std::fs::write(&file, "hi").unwrap();

        assert_eq!(
            Linker::detect_entry_type(&file, &base),
            EntryType::NotExists
        );
    }

    #[test]
    fn adopt_to_managed_moves_source_and_creates_link() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("unmanaged-skill");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("SKILL.md"), "hello").unwrap();
        let managed_dir = tmp.path().join("managed").join("skill-name");
        let link_path = tmp.path().join("cli-skills").join("skill-name");

        Linker::adopt_to_managed(&source, &managed_dir, &link_path).unwrap();

        assert!(
            !source.exists(),
            "source must be moved away, not copied-and-kept"
        );
        assert!(
            managed_dir.join("SKILL.md").exists(),
            "content now lives under managed_dir"
        );
        assert!(
            Linker::is_symlink(&link_path),
            "link_path must be a symlink"
        );
        assert_eq!(std::fs::read_link(&link_path).unwrap(), managed_dir);
        assert_eq!(
            std::fs::read_to_string(link_path.join("SKILL.md")).unwrap(),
            "hello",
            "link resolves through to the moved content"
        );
    }

    #[test]
    fn adopt_to_managed_overwrites_pre_existing_managed_dir_content() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("unmanaged-skill");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("SKILL.md"), "new-content").unwrap();

        let managed_dir = tmp.path().join("managed").join("skill-name");
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("SKILL.md"), "stale-content").unwrap();
        std::fs::write(managed_dir.join("stale-only-file.txt"), "must be gone").unwrap();

        let link_path = tmp.path().join("cli-skills").join("skill-name");

        Linker::adopt_to_managed(&source, &managed_dir, &link_path).unwrap();

        assert_eq!(
            std::fs::read_to_string(managed_dir.join("SKILL.md")).unwrap(),
            "new-content",
            "stale managed_dir content must be replaced by the fresh adopt"
        );
        assert!(
            !managed_dir.join("stale-only-file.txt").exists(),
            "old managed_dir must be fully cleared before the new content lands"
        );
    }

    #[test]
    fn adopt_to_managed_replaces_a_stale_symlink_at_link_path() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("unmanaged-skill");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("SKILL.md"), "content").unwrap();
        let managed_dir = tmp.path().join("managed").join("skill-name");
        let link_path = tmp.path().join("cli-skills").join("skill-name");
        std::fs::create_dir_all(link_path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("stale-target"), &link_path).unwrap();

        Linker::adopt_to_managed(&source, &managed_dir, &link_path).unwrap();

        assert!(Linker::is_symlink(&link_path));
        assert_eq!(std::fs::read_link(&link_path).unwrap(), managed_dir);
    }

    #[test]
    fn adopt_to_managed_skips_the_move_when_source_equals_managed_dir() {
        // Already-managed skill re-adopted (e.g. re-scanning a healed link):
        // source_path == managed_dir means there is nothing to move, only
        // the link needs to exist.
        let tmp = tempfile::tempdir().unwrap();
        let managed_dir = tmp.path().join("managed").join("skill-name");
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("SKILL.md"), "already-managed").unwrap();
        let link_path = tmp.path().join("cli-skills").join("skill-name");

        Linker::adopt_to_managed(&managed_dir, &managed_dir, &link_path).unwrap();

        assert!(
            managed_dir.join("SKILL.md").exists(),
            "managed_dir content untouched"
        );
        assert!(Linker::is_symlink(&link_path));
        assert_eq!(std::fs::read_link(&link_path).unwrap(), managed_dir);
    }

    #[test]
    fn move_dir_uses_rename_when_possible() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("from");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("f.txt"), "content").unwrap();
        let to = tmp.path().join("to"); // does not exist yet -> plain rename succeeds

        Linker::move_dir(&from, &to).unwrap();

        assert!(!from.exists(), "source removed by rename");
        assert_eq!(
            std::fs::read_to_string(to.join("f.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn move_dir_falls_back_to_copy_and_delete_when_rename_fails() {
        // Force a genuine std::fs::rename failure (not mocked): renaming a
        // directory onto a NON-EMPTY existing directory returns ENOTEMPTY on
        // both macOS and Linux. This exercises the exact `if
        // std::fs::rename(..).is_ok()` failure branch that the cross-device
        // (EXDEV) case also falls into — EXDEV itself isn't reproducible
        // inside a single tempdir sandbox, but the fallback code path is
        // identical for any rename() failure.
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("from");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("new-file.txt"), "fresh").unwrap();

        let to = tmp.path().join("to");
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("pre-existing.txt"), "old").unwrap();

        // Sanity: confirm the plain rename really would fail here, so this
        // test is actually exercising the fallback and not the fast path.
        assert!(
            std::fs::rename(&from, &to).is_err(),
            "renaming onto a non-empty dir must fail on this platform for the test to be valid"
        );

        Linker::move_dir(&from, &to).unwrap();

        assert!(!from.exists(), "source removed after fallback copy");
        assert_eq!(
            std::fs::read_to_string(to.join("new-file.txt")).unwrap(),
            "fresh",
            "fallback copied the source's content into `to`"
        );
        assert!(
            to.join("pre-existing.txt").exists(),
            "fallback copy does not clear pre-existing unrelated content in `to`"
        );
    }

    #[test]
    fn copy_dir_recursive_copies_nested_subdirs_and_empty_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("from");
        std::fs::create_dir_all(from.join("sub/nested")).unwrap();
        std::fs::create_dir_all(from.join("empty-dir")).unwrap();
        std::fs::write(from.join("top.txt"), "top").unwrap();
        std::fs::write(from.join("sub/mid.txt"), "mid").unwrap();
        std::fs::write(from.join("sub/nested/deep.txt"), "deep").unwrap();

        let to = tmp.path().join("to");
        Linker::copy_dir_recursive(&from, &to).unwrap();

        assert_eq!(std::fs::read_to_string(to.join("top.txt")).unwrap(), "top");
        assert_eq!(
            std::fs::read_to_string(to.join("sub/mid.txt")).unwrap(),
            "mid"
        );
        assert_eq!(
            std::fs::read_to_string(to.join("sub/nested/deep.txt")).unwrap(),
            "deep"
        );
        assert!(
            to.join("empty-dir").is_dir(),
            "empty subdirectories are still created"
        );
        // Source must be untouched — copy, not move.
        assert!(from.join("top.txt").exists());
    }
}
