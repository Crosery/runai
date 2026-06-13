//! L0 e2e for `core::linker` (PLANNING test plan §5.4).
//!
//! Each test exercises the public `Linker` API in an isolated `tempfile::TempDir`
//! sandbox — no real `~/.runai/` writes, no shared filesystem state. These guard
//! the contracts that scanner / manager / enable / disable depend on:
//!
//! - `create_link` auto-creates parent dirs and never silently overwrites
//! - `remove_link` is symlink-only (no-op on real dirs — protects against
//!   the 2026-04-27 incident where a remove call hit a real skill dir)
//! - `is_symlink` uses `symlink_metadata` so dangling links still count
//! - `detect_entry_type` classifies correctly across 4 states
//! - `move_dir` cross-filesystem fallback preserves the directory tree
//! - `create_link_force` clobbers symlinks but spares real dirs
//! - `adopt_to_managed` atomically relocates source + repoints link
//!
//! Skipped on Windows: symlinks require Developer Mode / Admin there, and
//! the in-module `tests` block in `linker.rs` is gated the same way.

#![cfg(not(target_os = "windows"))]

use std::path::Path;

use runai::core::linker::{EntryType, Linker};
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn sandbox() -> TempDir {
    tempfile::tempdir().expect("create sandbox tempdir")
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents).expect("write file");
}

// ─── §5.4 test 1 ────────────────────────────────────────────────────────────

#[test]
fn linker_create_link_parent_dirs_and_symlink() {
    let tmp = sandbox();

    // target dir exists, but link's parent does NOT yet
    let target = tmp.path().join("skills/foo");
    std::fs::create_dir_all(&target).expect("create target");

    let link = tmp.path().join(".claude/skills/foo");
    assert!(
        !link.parent().unwrap().exists(),
        "precondition: link parent must not exist yet — create_link should create it"
    );

    Linker::create_link(&target, &link).expect("create_link");

    assert!(
        link.parent().unwrap().is_dir(),
        "create_link must auto-create parent dirs"
    );
    assert!(Linker::is_symlink(&link), "link must be a symlink");
    let resolved = std::fs::read_link(&link).expect("read_link");
    assert_eq!(resolved, target, "symlink must point at the target");
}

// ─── §5.4 test 2 ────────────────────────────────────────────────────────────

#[test]
fn linker_remove_link_idempotent_on_non_symlink() {
    let tmp = sandbox();

    // a real symlink → remove_link must remove it
    let target = tmp.path().join("target");
    std::fs::create_dir_all(&target).expect("create target");
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");
    assert!(Linker::is_symlink(&link), "precondition: link is a symlink");

    Linker::remove_link(&link).expect("remove_link on symlink");
    assert!(
        !Linker::is_symlink(&link),
        "symlink must be gone after remove_link"
    );
    assert!(
        !link.exists() && link.symlink_metadata().is_err(),
        "symlink path must no longer exist"
    );

    // a real dir → remove_link must be a no-op (protect user data!)
    let realdir = tmp.path().join("realdir");
    std::fs::create_dir_all(&realdir).expect("create realdir");
    let canary = realdir.join("canary.txt");
    write_file(&canary, "do not delete me");

    Linker::remove_link(&realdir).expect("remove_link on real dir — must be no-op");
    assert!(
        realdir.is_dir(),
        "remove_link on a real dir MUST be a no-op — anything else risks user data loss"
    );
    assert!(canary.is_file(), "real dir contents must remain untouched");
}

// ─── §5.4 test 3 ────────────────────────────────────────────────────────────

#[test]
fn linker_is_symlink_detects_dangling() {
    let tmp = sandbox();

    // dangling symlink pointing at a non-existent target
    let dangling_target = tmp.path().join("does-not-exist");
    let dangling_link = tmp.path().join("dangling");
    std::os::unix::fs::symlink(&dangling_target, &dangling_link).expect("create dangling");

    assert!(!dangling_target.exists(), "precondition: target absent");
    assert!(
        dangling_link.metadata().is_err(),
        "metadata() follows the symlink and must fail when target is gone"
    );
    assert!(
        Linker::is_symlink(&dangling_link),
        "is_symlink must use symlink_metadata and report true on dangling"
    );

    // regular file should NOT be classified as symlink
    let regular = tmp.path().join("regular.txt");
    write_file(&regular, "hi");
    assert!(
        !Linker::is_symlink(&regular),
        "regular file is not a symlink"
    );

    // real directory is also not a symlink
    let realdir = tmp.path().join("realdir");
    std::fs::create_dir_all(&realdir).expect("create realdir");
    assert!(!Linker::is_symlink(&realdir), "real dir is not a symlink");
}

// ─── §5.4 test 4 ────────────────────────────────────────────────────────────

#[test]
fn linker_detect_entry_type_classifies_correctly() {
    let tmp = sandbox();
    let our_base = tmp.path().join("managed");
    std::fs::create_dir_all(&our_base).expect("create managed base");

    // OurSymlink: link points inside our_base
    let our_target = our_base.join("foo");
    std::fs::create_dir_all(&our_target).expect("create our_target");
    let our_link = tmp.path().join("link_to_ours");
    std::os::unix::fs::symlink(&our_target, &our_link).expect("our symlink");
    assert_eq!(
        Linker::detect_entry_type(&our_link, &our_base),
        EntryType::OurSymlink,
        "symlink to inside our_base must classify as OurSymlink"
    );

    // ForeignSymlink: link points outside our_base
    let foreign_target = tmp.path().join("outside");
    std::fs::create_dir_all(&foreign_target).expect("create foreign target");
    let foreign_link = tmp.path().join("link_to_other");
    std::os::unix::fs::symlink(&foreign_target, &foreign_link).expect("foreign symlink");
    assert_eq!(
        Linker::detect_entry_type(&foreign_link, &our_base),
        EntryType::ForeignSymlink,
        "symlink outside our_base must classify as ForeignSymlink"
    );

    // RealDir: actual directory at the path
    let realdir = tmp.path().join("realdir");
    std::fs::create_dir_all(&realdir).expect("create realdir");
    assert_eq!(
        Linker::detect_entry_type(&realdir, &our_base),
        EntryType::RealDir,
        "real directory must classify as RealDir"
    );

    // NotExists: nothing at the path
    let nothing = tmp.path().join("never-created");
    assert_eq!(
        Linker::detect_entry_type(&nothing, &our_base),
        EntryType::NotExists,
        "missing path must classify as NotExists"
    );
}

// ─── §5.4 test 5 ────────────────────────────────────────────────────────────

#[test]
fn linker_move_dir_preserves_tree_via_rename() {
    let tmp = sandbox();
    let src = tmp.path().join("from");
    write_file(&src.join("a.txt"), "alpha");
    write_file(&src.join("subdir/b.txt"), "beta");
    write_file(&src.join("subdir/deep/c.txt"), "gamma");

    let dst = tmp.path().join("to");
    Linker::move_dir(&src, &dst).expect("move_dir");

    assert!(!src.exists(), "source must be gone after move");
    assert!(dst.is_dir(), "destination must exist after move");
    assert_eq!(
        std::fs::read_to_string(dst.join("a.txt")).expect("read a.txt"),
        "alpha",
        "shallow file preserved"
    );
    assert_eq!(
        std::fs::read_to_string(dst.join("subdir/b.txt")).expect("read b.txt"),
        "beta",
        "nested file preserved"
    );
    assert_eq!(
        std::fs::read_to_string(dst.join("subdir/deep/c.txt")).expect("read c.txt"),
        "gamma",
        "deeply nested file preserved"
    );
}

#[test]
fn linker_move_dir_fallback_copy_on_existing_dst_or_xfs() {
    // We can't actually mock std::fs::rename, but we CAN exercise the
    // fallback path indirectly via `copy_dir_recursive` (the same routine
    // the cross-filesystem fallback uses). This locks in the contract that
    // copy_dir_recursive preserves arbitrary nested trees bit-for-bit.
    let tmp = sandbox();
    let src = tmp.path().join("src");
    write_file(&src.join("a.txt"), "alpha");
    write_file(&src.join("nested/deep/leaf.txt"), "delta");

    let dst = tmp.path().join("dst");
    Linker::copy_dir_recursive(&src, &dst).expect("copy_dir_recursive");

    // After copy, BOTH src and dst should be present (this is what move_dir's
    // fallback path executes before remove_dir_all on src).
    assert!(src.is_dir(), "src remains until caller removes");
    assert!(dst.is_dir(), "dst is fully populated by copy_dir_recursive");
    assert_eq!(
        std::fs::read_to_string(dst.join("a.txt")).expect("dst/a.txt"),
        "alpha",
    );
    assert_eq!(
        std::fs::read_to_string(dst.join("nested/deep/leaf.txt")).expect("dst nested leaf"),
        "delta",
    );

    // Then verify the full move_dir flow with rename ALSO works (the happy
    // path). copy + the documented fallback semantics ensure the cross-fs
    // case is symmetric.
    let src2 = tmp.path().join("src2");
    write_file(&src2.join("x.txt"), "xray");
    let dst2 = tmp.path().join("dst2");
    Linker::move_dir(&src2, &dst2).expect("happy-path move");
    assert!(!src2.exists(), "src2 gone");
    assert_eq!(
        std::fs::read_to_string(dst2.join("x.txt")).expect("dst2/x.txt"),
        "xray",
    );
}

// ─── §5.4 test 6 ────────────────────────────────────────────────────────────

#[test]
fn linker_create_link_force_clobbers_symlink_not_realdir() {
    let tmp = sandbox();

    // Case A: pre-existing (dangling) symlink — must be clobbered and replaced.
    let new_target = tmp.path().join("new-target");
    std::fs::create_dir_all(&new_target).expect("create new_target");
    let link_a = tmp.path().join("link_a");
    std::os::unix::fs::symlink(tmp.path().join("nope"), &link_a).expect("stale link");
    assert!(Linker::is_symlink(&link_a), "precondition: dangling link");

    Linker::create_link_force(&new_target, &link_a).expect("create_link_force on symlink");
    assert!(Linker::is_symlink(&link_a), "link_a still a symlink");
    let resolved = std::fs::read_link(&link_a).expect("read_link");
    assert_eq!(
        resolved, new_target,
        "stale link must be repointed to new_target"
    );

    // Case B: pre-existing REAL dir at the link path → create_link_force must
    // NOT remove it, and the underlying create_link should surface an error.
    let realdir = tmp.path().join("realdir");
    std::fs::create_dir_all(&realdir).expect("create realdir");
    let canary = realdir.join("canary.txt");
    write_file(&canary, "do not destroy");

    let err = Linker::create_link_force(&new_target, &realdir)
        .expect_err("create_link_force MUST fail rather than nuke a real directory");
    let msg = format!("{err:?}").to_lowercase();
    assert!(
        msg.contains("symlink") || msg.contains("exist") || msg.contains("file exists"),
        "expected symlink/exists error, got: {err:?}"
    );

    assert!(
        realdir.is_dir(),
        "real dir at the link path MUST remain — clobbering it would lose user data"
    );
    assert!(canary.is_file(), "canary file inside real dir preserved");
}

// ─── §5.4 test 7 ────────────────────────────────────────────────────────────

#[test]
fn linker_adopt_to_managed_relocates_and_relinks() {
    let tmp = sandbox();

    // Source skill living somewhere outside the managed dir.
    let source = tmp.path().join("source-skill");
    write_file(&source.join("SKILL.md"), "---\nname: foo\n---\nbody");
    write_file(&source.join("scripts/run.sh"), "#!/bin/sh\necho hi");

    // Pre-existing stale symlink at the link path (the typical adopt scenario:
    // user had a hand-crafted symlink in `~/.claude/skills/foo` pointing
    // somewhere else, and adopt must repoint it to the managed copy).
    let link_path = tmp.path().join(".claude/skills/foo");
    std::fs::create_dir_all(link_path.parent().unwrap()).expect("create link parent");
    let old_target = tmp.path().join("old-target");
    std::fs::create_dir_all(&old_target).expect("create old target");
    std::os::unix::fs::symlink(&old_target, &link_path).expect("pre-existing link");
    assert!(Linker::is_symlink(&link_path));

    // Destination inside the managed area (doesn't exist yet).
    let managed = tmp.path().join("managed/foo");

    Linker::adopt_to_managed(&source, &managed, &link_path).expect("adopt_to_managed");

    // 1. source moved to managed location, with full subtree intact
    assert!(!source.exists(), "source dir must be moved away");
    assert!(managed.is_dir(), "managed dir must exist");
    assert_eq!(
        std::fs::read_to_string(managed.join("SKILL.md")).expect("SKILL.md"),
        "---\nname: foo\n---\nbody",
        "SKILL.md content preserved",
    );
    assert!(
        managed.join("scripts/run.sh").is_file(),
        "nested script preserved through adopt"
    );

    // 2. link now points at the managed location, NOT old_target
    assert!(Linker::is_symlink(&link_path), "link_path is a symlink");
    let resolved = std::fs::read_link(&link_path).expect("read_link");
    assert_eq!(
        resolved, managed,
        "link must be repointed to the managed dir (not the old stale target)"
    );

    // 3. old_target untouched (adopt only swaps the symlink, doesn't delete arbitrary peers)
    assert!(
        old_target.is_dir(),
        "old symlink TARGET is left alone — adopt only swaps the link, not the target"
    );
}
