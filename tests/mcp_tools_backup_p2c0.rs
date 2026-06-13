//! Physical e2e + cargo integration tests for sm_backup / sm_backups / sm_restore.
//!
//! Drives the real installed binary at /Users/crosery/.cargo/bin/runai through
//! its CLI (`runai backup` / `runai backups` / `runai restore`) — these are the
//! same `core::backup::{create_backup, list_backups, restore_backup}` entry
//! points the MCP `sm_backup` / `sm_backups` / `sm_restore` tools wrap, so a
//! CLI-driven test fully exercises the tool's logic without spinning up the
//! stdio MCP protocol.
//!
//! Every test runs inside an isolated `HOME=$(mktemp -d)` + `RUNE_DATA_DIR`
//! sandbox per the AGENTS.md safety contract. No test touches the real
//! `~/.runai/` or any CLI config under the real `~/`.

use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

/// Sandbox: isolated HOME + RUNE_DATA_DIR pointing inside that HOME.
struct Sandbox {
    home: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let home = TempDir::new().expect("mktemp -d for sandbox HOME");
        Self { home }
    }

    fn home_path(&self) -> &Path {
        self.home.path()
    }

    fn data_dir(&self) -> PathBuf {
        self.home.path().join(".runai")
    }

    /// Build a `runai` invocation pre-pointed at this sandbox.
    fn runai(&self) -> Command {
        let mut c = Command::new(RUNAI_BIN);
        c.env("HOME", self.home.path())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.data_dir());
        c
    }

    /// Same as `runai()` but with RUNE_DATA_DIR pointed at a custom location
    /// (for the cross-RUNE_DATA_DIR tests).
    fn runai_with_data(&self, data: &Path) -> Command {
        let mut c = Command::new(RUNAI_BIN);
        c.env("HOME", self.home.path())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", data);
        c
    }
}

fn write_file(p: &Path, body: &str) {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(p, body).expect("write file");
}

fn run_ok(mut cmd: Command) -> String {
    let out = cmd.output().expect("spawn runai");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "runai exit={:?}\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code()
    );
    stdout + &stderr
}

/// Run runai, accept either status (some flows print to stderr even on success).
fn run_capture(mut cmd: Command) -> String {
    let out = cmd.output().expect("spawn runai");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    stdout + &stderr
}

// =============================================================================
// 3.17 [P1] sm_backup — physical e2e + cargo integration
// =============================================================================

/// sm_backup_creates_complete_snapshot:
/// preinstalled skill + disabled MCP backup + ~/.claude.json must all end up in
/// the backup directory under managed-skills / managed-mcps / claude.json, plus
/// a `timestamp` marker file.
#[test]
fn sm_backup_creates_complete_snapshot() {
    let s = Sandbox::new();

    // Preinstall a managed skill, a disabled MCP backup, a Claude config.
    write_file(
        &s.data_dir().join("skills/my-skill/SKILL.md"),
        "# my-skill\n",
    );
    write_file(
        &s.data_dir().join("mcps/disabled-mcp.json"),
        r#"{"command":"x","args":[]}"#,
    );
    write_file(&s.home_path().join(".claude.json"), r#"{"mcpServers":{}}"#);

    // Run backup
    let mut cmd = s.runai();
    cmd.arg("backup");
    let out = run_ok(cmd);

    // Output must announce backup creation with a path.
    assert!(
        out.contains("Backup created:"),
        "expected 'Backup created:' in output, got: {out}"
    );

    // Find the timestamped backup dir.
    let backups_root = s.data_dir().join("backups");
    assert!(backups_root.is_dir(), "backups root not created");
    let ts_dirs: Vec<_> = std::fs::read_dir(&backups_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(
        ts_dirs.len(),
        1,
        "expected exactly one backup dir, got {}",
        ts_dirs.len()
    );
    let bd = &ts_dirs[0];

    // Required files / dirs:
    assert!(bd.join("timestamp").is_file(), "missing timestamp marker");
    assert!(bd.join("claude.json").is_file(), "missing claude.json");
    assert!(
        bd.join("managed-skills/my-skill/SKILL.md").is_file(),
        "missing managed-skills/my-skill/SKILL.md"
    );
    assert!(
        bd.join("managed-mcps/disabled-mcp.json").is_file(),
        "missing managed-mcps/disabled-mcp.json"
    );

    // timestamp file content must match the dir name (YYYYMMDD_HHMMSS).
    let ts_content = std::fs::read_to_string(bd.join("timestamp")).unwrap();
    let dirname = bd.file_name().unwrap().to_string_lossy().to_string();
    assert_eq!(
        ts_content.trim(),
        dirname,
        "timestamp marker should match dir name"
    );
}

/// sm_backup_covers_all_cli_targets:
/// For claude / codex / gemini / opencode, each `~/.<target>/skills/` dir gets
/// backed up under `<target>-skills/`, preserving symlinks as symlinks (not
/// dereferenced into real files).
#[test]
fn sm_backup_covers_all_cli_targets() {
    let s = Sandbox::new();

    // Real skill that the CLI symlinks point to (anywhere outside the CLI skill dir).
    let real = s.home_path().join("real-skill-src");
    std::fs::create_dir_all(&real).unwrap();
    write_file(&real.join("SKILL.md"), "# real\n");

    for target in ["claude", "codex", "gemini", "opencode"] {
        let dir = s.home_path().join(format!(".{target}/skills"));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, dir.join("real-skill")).unwrap();
    }

    let mut cmd = s.runai();
    cmd.arg("backup");
    let out = run_ok(cmd);
    assert!(out.contains("Backup created:"), "got: {out}");

    let backups_root = s.data_dir().join("backups");
    let bd = std::fs::read_dir(&backups_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_dir())
        .unwrap()
        .path();

    for target in ["claude", "codex", "gemini", "opencode"] {
        let target_dir = bd.join(format!("{target}-skills"));
        assert!(
            target_dir.is_dir(),
            "missing {target}-skills under backup"
        );
        let link = target_dir.join("real-skill");
        // Should be a symlink, NOT a real dir/file (symlink preservation).
        let meta = std::fs::symlink_metadata(&link).unwrap_or_else(|e| {
            panic!("{target}-skills/real-skill missing: {e}");
        });
        assert!(
            meta.file_type().is_symlink(),
            "{target}-skills/real-skill should be a symlink, got {:?}",
            meta.file_type()
        );
    }
}

/// sm_backup_respects_rune_data_dir:
/// With `RUNE_DATA_DIR=<alt>`, the backup must land under `<alt>/backups/...`,
/// not under `$HOME/.runai/backups/`. Validates the env passes all the way
/// through to AppPaths::data_dir().
#[test]
fn sm_backup_respects_rune_data_dir() {
    let s = Sandbox::new();
    let alt = TempDir::new().unwrap();
    let alt_data = alt.path().join("alt_runai");

    // Preinstall a skill *inside the alt data dir*.
    write_file(&alt_data.join("skills/altskill/SKILL.md"), "# alt\n");

    let mut cmd = s.runai_with_data(&alt_data);
    cmd.arg("backup");
    let out = run_ok(cmd);
    assert!(out.contains("Backup created:"), "got: {out}");

    // Backup lives under alt_data/backups/, not default ~/.runai/backups/
    let alt_backups = alt_data.join("backups");
    assert!(alt_backups.is_dir(), "alt backups dir missing");
    let bd = std::fs::read_dir(&alt_backups)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_dir())
        .unwrap()
        .path();
    assert!(
        bd.join("managed-skills/altskill/SKILL.md").is_file(),
        "alt skill not in backup"
    );

    // The default $HOME/.runai/backups must NOT have been created.
    assert!(
        !s.data_dir().join("backups").exists(),
        "default backups dir should not be touched when RUNE_DATA_DIR is set"
    );
}

/// sm_backup_returns_valid_path:
/// `runai backup` prints `Backup created: <path>` — the path is a real
/// directory, contains a `timestamp` marker, and matches the dir name.
#[test]
fn sm_backup_returns_valid_path() {
    let s = Sandbox::new();
    let mut cmd = s.runai();
    cmd.arg("backup");
    let out = run_ok(cmd);

    // Parse "Backup created: <path>" line.
    let line = out
        .lines()
        .find(|l| l.contains("Backup created:"))
        .unwrap_or_else(|| panic!("no 'Backup created:' line in: {out}"));
    let path_str = line
        .split("Backup created:")
        .nth(1)
        .unwrap()
        .trim()
        .to_string();
    let p = PathBuf::from(&path_str);
    assert!(
        p.is_dir(),
        "returned path is not a directory: {}",
        p.display()
    );
    let ts_marker = p.join("timestamp");
    assert!(
        ts_marker.is_file(),
        "returned path has no timestamp marker"
    );
    let ts_in_marker = std::fs::read_to_string(&ts_marker).unwrap();
    let dirname = p.file_name().unwrap().to_string_lossy().to_string();
    assert_eq!(
        ts_in_marker.trim(),
        dirname,
        "timestamp marker should equal backup dir name"
    );
    // YYYYMMDD_HHMMSS = 15 chars: 8 digit + _ + 6 digit.
    assert_eq!(dirname.len(), 15, "unexpected timestamp format: {dirname}");
    assert_eq!(
        dirname.chars().nth(8).unwrap(),
        '_',
        "expected underscore in pos 8: {dirname}"
    );
}

// =============================================================================
// 3.18 [P2] sm_backups — list backups, newest first
// =============================================================================

/// sm_backups_lists_newest_first:
/// Create 3 backups with > 1s spacing, list them, and verify the order is
/// newest first.
#[test]
fn sm_backups_lists_newest_first() {
    let s = Sandbox::new();

    // Create 3 backups, separated by > 1s so the YYYYMMDD_HHMMSS keys differ.
    let mut created: Vec<String> = Vec::new();
    for _ in 0..3 {
        let mut cmd = s.runai();
        cmd.arg("backup");
        let out = run_ok(cmd);
        let line = out
            .lines()
            .find(|l| l.contains("Backup created:"))
            .expect("Backup created line");
        let path = line.split("Backup created:").nth(1).unwrap().trim();
        let ts = PathBuf::from(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        created.push(ts);
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }

    // List them.
    let mut cmd = s.runai();
    cmd.arg("backups");
    let out = run_ok(cmd);

    // CLI prints "  <ts>\n  <ts>\n  <ts>\n\nTotal: 3 backups".
    let listed: Vec<String> = out
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            if t.len() == 15 && t.chars().nth(8) == Some('_') {
                Some(t.to_string())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(listed.len(), 3, "expected 3 timestamps listed, got: {out}");

    // The CLI surface is the same `list_backups` core::backup helper that
    // sm_backups uses — and it sorts newest first.
    let mut expected = created.clone();
    expected.sort_unstable();
    expected.reverse();
    assert_eq!(
        listed, expected,
        "backups should be listed newest first; got {listed:?} expected {expected:?}"
    );

    // Sanity: total count line is present.
    assert!(
        out.contains("Total: 3 backups"),
        "missing 'Total: 3 backups' in: {out}"
    );
}

/// sm_backups_no_backups_message:
/// No backups → CLI prints "No backups found.", not an empty result.
/// (Matches the sm_backups MCP tool which returns "No backups found".)
#[test]
fn sm_backups_no_backups_message() {
    let s = Sandbox::new();
    let mut cmd = s.runai();
    cmd.arg("backups");
    let out = run_ok(cmd);
    assert!(
        out.contains("No backups found"),
        "expected 'No backups found' in: {out}"
    );
}

/// sm_backups_timestamp_format_restore_compatible:
/// Each listed timestamp matches YYYYMMDD_HHMMSS, and feeding any one of them
/// into `runai restore --timestamp <ts>` succeeds (not "Backup not found").
/// This proves the listing format is the canonical restore key.
#[test]
fn sm_backups_timestamp_format_restore_compatible() {
    let s = Sandbox::new();

    // Make one backup with non-empty managed-skills so restore has something to do.
    write_file(
        &s.data_dir().join("skills/probe/SKILL.md"),
        "# probe\n",
    );
    let mut cmd = s.runai();
    cmd.arg("backup");
    let _ = run_ok(cmd);

    // List + parse the timestamp.
    let mut list_cmd = s.runai();
    list_cmd.arg("backups");
    let out = run_ok(list_cmd);
    let ts = out
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            if t.len() == 15 && t.chars().nth(8) == Some('_') {
                Some(t.to_string())
            } else {
                None
            }
        })
        .next()
        .unwrap_or_else(|| panic!("no timestamp line in: {out}"));

    // Verify format: 8 digits + '_' + 6 digits.
    assert_eq!(ts.len(), 15);
    assert!(
        ts.chars().take(8).all(|c| c.is_ascii_digit()),
        "first 8 chars must be digits: {ts}"
    );
    assert_eq!(ts.chars().nth(8).unwrap(), '_', "underscore at pos 8: {ts}");
    assert!(
        ts.chars().skip(9).all(|c| c.is_ascii_digit()),
        "last 6 chars must be digits: {ts}"
    );

    // Feed back into restore — must succeed, not "Backup not found".
    let mut restore_cmd = s.runai();
    restore_cmd.arg("restore").arg("--timestamp").arg(&ts);
    let restore_out = run_capture(restore_cmd);
    assert!(
        !restore_out.contains("Backup not found"),
        "restore rejected the listed timestamp: {restore_out}"
    );
    assert!(
        restore_out.contains("Restored") && restore_out.contains("items"),
        "expected 'Restored N items' from restore: {restore_out}"
    );
}
