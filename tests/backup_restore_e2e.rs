//! Physical e2e regression tests for `core::backup` — timestamped backup/restore
//! of managed data (`~/.runai/{skills,mcps}/`), CLI skill symlinks, and the 4 CLI
//! config files.
//!
//! Each test runs in an isolated `HOME=$(mktemp -d)` sandbox; the public
//! `create_backup` / `restore_backup` API resolves HOME via `dirs::home_dir()`
//! (which is the `$HOME` env var on unix), so we drive that here.
//!
//! Skipped on Windows: backup test patterns rely on `HOME` env mocking +
//! symlinks, which require unix semantics (Developer Mode / Admin on Windows).
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};

use runai::core::backup::{create_backup, list_backups, restore_backup};
use runai::core::paths::AppPaths;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

/// Set $HOME and (optionally) RUNE_DATA_DIR for the current process.
/// Tests run serially (`--test-threads=1`), so process-wide env mutation is
/// safe — but the helper always sets both vars to keep tests independent.
fn set_env_home(home: &Path, data_dir: Option<&Path>) {
    unsafe {
        std::env::set_var("HOME", home);
        match data_dir {
            Some(d) => std::env::set_var("RUNE_DATA_DIR", d),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
        std::env::remove_var("SKILL_MANAGER_DATA_DIR");
    }
}

/// Build a paths handle anchored at $HOME/.runai (the data_dir is independent
/// of the public APIs reading HOME for CLI configs — `AppPaths::with_base`
/// pins it explicitly).
fn paths_at(home: &Path) -> AppPaths {
    let paths = AppPaths::with_base(home.join(".runai"));
    paths.ensure_dirs().unwrap();
    paths
}

fn write_file(p: &Path, body: &str) {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::os::unix::fs::symlink(target, link).unwrap();
}

fn newest_backup_ts(paths: &AppPaths) -> String {
    list_backups(paths)
        .into_iter()
        .next()
        .expect("at least one backup")
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// 1) Symlinks in `~/.claude/skills/` must be copied **as symlinks** into the
/// backup, not dereferenced into a copy of the target.
#[test]
fn backup_preserves_symlinks_not_dereferenced() {
    let home_tmp = TempDir::new().unwrap();
    let home = home_tmp.path();
    set_env_home(home, Some(&home.join(".runai")));

    let paths = paths_at(home);

    // Symlink target is an arbitrary real directory inside the sandbox.
    let real_target = home.join(".runai/skills/some-skill");
    std::fs::create_dir_all(&real_target).unwrap();
    write_file(&real_target.join("SKILL.md"), "# real");

    // Now wire `~/.claude/skills/link-skill -> real_target`.
    let link_path = home.join(".claude/skills/link-skill");
    symlink(&real_target, &link_path);

    let backup_dir = create_backup(&paths).unwrap();

    let backed_up = backup_dir.join("claude-skills").join("link-skill");
    let meta = std::fs::symlink_metadata(&backed_up)
        .expect("backup of symlinked CLI skill must exist");
    assert!(
        meta.file_type().is_symlink(),
        "backup must preserve the symlink as a symlink (not dereferenced); found {:?}",
        meta.file_type()
    );

    // The link target stored inside the backup must equal the original target.
    let resolved = std::fs::read_link(&backed_up).unwrap();
    assert_eq!(
        resolved, real_target,
        "preserved symlink target must equal the original target"
    );
}

/// 2) `create_backup` snapshots managed `skills/` + `mcps/` + the 4 CLI config
/// files + writes a `timestamp` marker.
#[test]
fn backup_snapshots_all_managed_data_and_configs() {
    let home_tmp = TempDir::new().unwrap();
    let home = home_tmp.path();
    set_env_home(home, Some(&home.join(".runai")));

    let paths = paths_at(home);

    // managed skill + managed MCP backup
    write_file(
        &paths.skills_dir().join("my-skill/SKILL.md"),
        "# my managed skill",
    );
    write_file(
        &paths.mcps_dir().join("pencil.json"),
        r#"{"command":"pencil"}"#,
    );

    // 4 CLI config files — paths match the source's `configs` table
    write_file(&home.join(".claude.json"), r#"{"mcpServers":{}}"#);
    write_file(&home.join(".codex/settings.json"), r#"{"codex":true}"#);
    write_file(&home.join(".gemini/settings.json"), r#"{"gemini":true}"#);
    write_file(
        &home.join(".opencode/settings.json"),
        r#"{"opencode":true}"#,
    );

    let backup_dir = create_backup(&paths).unwrap();

    // Timestamp marker present and equals dir name
    let ts_marker_path = backup_dir.join("timestamp");
    assert!(ts_marker_path.exists(), "timestamp marker must be written");
    let ts_marker = std::fs::read_to_string(&ts_marker_path).unwrap();
    assert_eq!(
        ts_marker,
        backup_dir.file_name().unwrap().to_string_lossy(),
        "timestamp marker must match backup directory name"
    );

    // Managed data
    assert!(
        backup_dir.join("managed-skills/my-skill/SKILL.md").exists(),
        "managed-skills/ must contain my-skill"
    );
    assert!(
        backup_dir.join("managed-mcps/pencil.json").exists(),
        "managed-mcps/ must contain pencil.json"
    );

    // 4 CLI config files
    assert!(
        backup_dir.join("claude.json").exists(),
        "claude.json must be in backup"
    );
    assert!(
        backup_dir.join("codex-settings.json").exists(),
        "codex-settings.json must be in backup"
    );
    assert!(
        backup_dir.join("gemini-settings.json").exists(),
        "gemini-settings.json must be in backup"
    );
    assert!(
        backup_dir.join("opencode-settings.json").exists(),
        "opencode-settings.json must be in backup"
    );
}

/// 3) `list_backups` returns timestamps newest-first.
#[test]
fn list_backups_newest_first() {
    let home_tmp = TempDir::new().unwrap();
    let home = home_tmp.path();
    set_env_home(home, Some(&home.join(".runai")));

    let paths = paths_at(home);

    // Manually create 3 backup dirs — list_backups only inspects directory names.
    let backups_dir = paths.data_dir().join("backups");
    for ts in ["20260101_100000", "20260102_100000", "20260101_150000"] {
        std::fs::create_dir_all(backups_dir.join(ts)).unwrap();
    }

    let list = list_backups(&paths);
    assert_eq!(
        list,
        vec![
            "20260102_100000".to_string(),
            "20260101_150000".to_string(),
            "20260101_100000".to_string(),
        ],
        "newest-first ordering must be enforced"
    );
}

/// 4) Restore overlay semantics — managed config files restore by overwrite,
/// but live state that's NOT in the backup must remain untouched. Re-running
/// restore yields the same end state (idempotent).
#[test]
fn restore_backup_overlays_idempotent() {
    let home_tmp = TempDir::new().unwrap();
    let home = home_tmp.path();
    set_env_home(home, Some(&home.join(".runai")));

    let paths = paths_at(home);

    // Seed live state, then backup.
    write_file(
        &paths.skills_dir().join("orig-skill/SKILL.md"),
        "# original",
    );
    write_file(&home.join(".claude.json"), r#"{"original":true}"#);

    let backup_dir = create_backup(&paths).unwrap();
    let ts = backup_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // Modify live state — change config and add an UNRELATED dotfile elsewhere
    // that is NOT covered by backup at all.
    write_file(&home.join(".claude.json"), r#"{"modified":true}"#);
    write_file(&home.join(".not-covered.txt"), "untouched");

    let restored = restore_backup(&paths, &ts).unwrap();
    assert!(
        restored >= 1,
        "restore must return at least the number of items restored; got {restored}"
    );

    // Backup files restored
    let config = std::fs::read_to_string(home.join(".claude.json")).unwrap();
    assert!(
        config.contains("original"),
        "config restored to original content"
    );

    // Files not covered by backup remain untouched
    assert_eq!(
        std::fs::read_to_string(home.join(".not-covered.txt")).unwrap(),
        "untouched",
        "files not in backup must NOT be deleted or modified by restore"
    );

    // Re-running restore yields same state (idempotent)
    let restored2 = restore_backup(&paths, &ts).unwrap();
    assert_eq!(
        restored2, restored,
        "restore must be idempotent (same return count on re-run)"
    );
    let config2 = std::fs::read_to_string(home.join(".claude.json")).unwrap();
    assert!(
        config2.contains("original"),
        "config still restored after second restore call"
    );
}

/// 5) Cross-`RUNE_DATA_DIR`: backup written under one data dir is correctly
/// read back when the backup tree has been migrated to a different data dir.
/// (Critical: per safety contract bug-class 2026-04-27, data-dir override must
/// be honored end-to-end.)
#[test]
fn backup_restore_cross_rune_data_dir_override() {
    let home_tmp = TempDir::new().unwrap();
    let home = home_tmp.path();

    // First data dir = default ($HOME/.runai)
    let default_data = home.join(".runai");
    set_env_home(home, Some(&default_data));

    let paths_default = paths_at(home);
    write_file(
        &paths_default.skills_dir().join("cross-skill/SKILL.md"),
        "# cross-dir",
    );
    write_file(&home.join(".claude.json"), r#"{"src":"default"}"#);

    let backup_dir = create_backup(&paths_default).unwrap();
    let ts = backup_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        backup_dir.starts_with(&default_data),
        "backup must land under the default data dir, got {backup_dir:?}"
    );

    // Second data dir — distinct tempdir, NOT under default
    let custom_tmp = TempDir::new().unwrap();
    let custom_data = custom_tmp.path().join("custom-runai");
    let paths_custom = AppPaths::with_base(custom_data.clone());
    paths_custom.ensure_dirs().unwrap();

    // Physically copy the timestamped backup tree to the custom data dir so
    // restore_backup(paths_custom, ts) can find it. This proves restore reads
    // paths.data_dir() (not a baked-in path), honoring the override.
    let src_ts_dir = default_data.join("backups").join(&ts);
    let dst_ts_dir = custom_data.join("backups").join(&ts);
    copy_dir(&src_ts_dir, &dst_ts_dir);

    set_env_home(home, Some(&custom_data));

    // Wipe live config to prove restore actually wrote new content.
    write_file(&home.join(".claude.json"), r#"{"src":"WIPED"}"#);

    let restored = restore_backup(&paths_custom, &ts).unwrap();
    assert!(restored >= 1, "cross-data-dir restore should restore items");

    // Managed skill under custom data dir is restored
    assert!(
        paths_custom
            .skills_dir()
            .join("cross-skill/SKILL.md")
            .exists(),
        "managed skill must be restored under the custom RUNE_DATA_DIR target"
    );

    let cfg = std::fs::read_to_string(home.join(".claude.json")).unwrap();
    assert!(
        cfg.contains("default"),
        "config restored from backup, got {cfg:?}"
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let ft = entry.metadata().unwrap().file_type();
        if ft.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&src).unwrap(), &dst).unwrap();
        } else if ft.is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// 6) All 4 CLI targets (claude / codex / gemini / opencode) round-trip
/// symmetrically through backup + restore — same sequence, no target skipped.
#[test]
fn backup_restore_all_4_cli_targets_symmetric() {
    let home_tmp = TempDir::new().unwrap();
    let home = home_tmp.path();
    set_env_home(home, Some(&home.join(".runai")));

    let paths = paths_at(home);

    // The symlink target is a real directory under the managed skills tree.
    let managed_target = paths.skills_dir().join("shared-skill");
    std::fs::create_dir_all(&managed_target).unwrap();
    write_file(&managed_target.join("SKILL.md"), "# shared");

    let clis = ["claude", "codex", "gemini", "opencode"];
    let mut symlinks: Vec<PathBuf> = Vec::new();
    for cli in clis {
        let link = home.join(format!(".{cli}/skills/shared-skill"));
        symlink(&managed_target, &link);
        symlinks.push(link);
    }

    let backup_dir = create_backup(&paths).unwrap();
    let ts = backup_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // All 4 CLIs have a backup folder
    for cli in clis {
        let cli_backup = backup_dir.join(format!("{cli}-skills/shared-skill"));
        let meta = std::fs::symlink_metadata(&cli_backup).unwrap_or_else(|e| {
            panic!("missing backup for {cli}: {cli_backup:?} ({e})");
        });
        assert!(
            meta.file_type().is_symlink(),
            "{cli} backup must preserve symlink (not deref)"
        );
    }

    // Wipe live symlinks across all 4 CLIs
    for link in &symlinks {
        std::fs::remove_file(link).unwrap();
        assert!(!link.exists(), "wiped {link:?}");
    }

    let restored = restore_backup(&paths, &ts).unwrap();
    assert!(restored >= 4, "restore must touch all 4 CLI dirs");

    // All 4 CLI symlinks are back, and each one is a symlink pointing to the
    // managed target — proves the restore loop runs the same sequence per CLI.
    for cli in clis {
        let link = home.join(format!(".{cli}/skills/shared-skill"));
        let meta = std::fs::symlink_metadata(&link).unwrap_or_else(|e| {
            panic!("{cli} symlink not restored: {e}");
        });
        assert!(
            meta.file_type().is_symlink(),
            "{cli} restored as symlink (not dereferenced into a copy)"
        );
        let target = std::fs::read_link(&link).unwrap();
        assert_eq!(
            target, managed_target,
            "{cli} symlink target preserved across backup/restore"
        );
    }
}

/// 7) Restore against a nonexistent timestamp returns Err and doesn't panic.
#[test]
fn restore_backup_nonexistent_timestamp_err() {
    let home_tmp = TempDir::new().unwrap();
    let home = home_tmp.path();
    set_env_home(home, Some(&home.join(".runai")));

    let paths = paths_at(home);

    // Seed live state — restore must not touch it on the error path.
    write_file(
        &paths.skills_dir().join("alive/SKILL.md"),
        "# must not be touched",
    );
    write_file(&home.join(".claude.json"), r#"{"untouched":true}"#);

    let result = restore_backup(&paths, "nonexistent_timestamp_does_not_exist");
    assert!(
        result.is_err(),
        "restore must error when timestamp is unknown"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("Backup not found") || err_msg.to_lowercase().contains("not found"),
        "error message should indicate the backup was not found, got: {err_msg}"
    );

    // Live state untouched
    assert!(
        paths.skills_dir().join("alive/SKILL.md").exists(),
        "live skill must remain untouched on error path"
    );
    let cfg = std::fs::read_to_string(home.join(".claude.json")).unwrap();
    assert!(
        cfg.contains("untouched"),
        "live config must remain untouched on error path"
    );

    let ts_newest = list_backups(&paths);
    assert!(
        ts_newest.is_empty(),
        "no backups should have been created by an error-path restore"
    );
}

// Ensure newest_backup_ts helper is used / not dead (defensive against the
// dead-code lint while still keeping the helper available for future tests).
#[test]
fn _helper_smoke_newest_backup_ts() {
    let home_tmp = TempDir::new().unwrap();
    let home = home_tmp.path();
    set_env_home(home, Some(&home.join(".runai")));
    let paths = paths_at(home);

    let backups_dir = paths.data_dir().join("backups");
    std::fs::create_dir_all(backups_dir.join("20260101_000000")).unwrap();
    let ts = newest_backup_ts(&paths);
    assert_eq!(ts, "20260101_000000");
}
