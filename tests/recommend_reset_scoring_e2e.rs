//! Physical end-to-end tests for `runai recommend reset-scoring`.
//!
//! The command wipes every row from `resource_ai_summary` in the runai DB.
//! It is destructive, so an interactive `y/N` confirmation gates it unless
//! `--yes` is passed. These tests cover the four scenarios from the test
//! plan (§1.39):
//!
//!   1. interactive confirmation flow ('y' proceeds, prints `deleted: N`)
//!   2. `--yes` skips the prompt entirely
//!   3. user aborts ('n' or empty line) — DB untouched
//!   4. `RUNE_DATA_DIR` is respected — only the targeted DB is wiped, the
//!      default home DB stays intact (this is the 4-20 / 4-27 root-cause
//!      regression area: deletes must obey the active data dir).
//!
//! Each test spawns the real `runai` binary built by cargo test, with HOME
//! pointed at a fresh tempdir and `RUNE_DATA_DIR` / `SKILL_MANAGER_DATA_DIR`
//! either cleared or explicitly pointed at another tempdir.
//!
//! Skipped on Windows: the existing `safety_e2e` suite is also gated this
//! way because HOME mocking is unix-only on `dirs` 6.x.
#![cfg(not(target_os = "windows"))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use rusqlite::Connection;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Spawn a runai command with HOME set to `home` and the data-dir env vars
/// cleared (so the binary picks `<home>/.runai` as default).
fn run_default_home(home: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = runai();
    cmd.args(args)
        .env("HOME", home)
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR");
    cmd.output().expect("runai spawn")
}

/// Spawn a runai command with HOME and RUNE_DATA_DIR explicitly set.
fn run_with_data_dir(home: &Path, data: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = runai();
    cmd.args(args)
        .env("HOME", home)
        .env("RUNE_DATA_DIR", data)
        .env_remove("SKILL_MANAGER_DATA_DIR");
    cmd.output().expect("runai spawn")
}

/// Spawn `runai recommend reset-scoring` (no `--yes`) and pipe `input`
/// into its stdin to drive the interactive prompt. Returns full Output.
fn run_reset_scoring_interactive(
    home: &Path,
    data: Option<&Path>,
    input: &str,
) -> std::process::Output {
    let mut cmd = runai();
    cmd.args(["recommend", "reset-scoring"])
        .env("HOME", home)
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = data {
        cmd.env("RUNE_DATA_DIR", d);
    } else {
        cmd.env_remove("RUNE_DATA_DIR");
    }
    let mut child = cmd.spawn().expect("spawn recommend reset-scoring");
    {
        let stdin = child.stdin.as_mut().expect("stdin pipe");
        stdin
            .write_all(input.as_bytes())
            .expect("write to stdin");
    }
    child
        .wait_with_output()
        .expect("wait_with_output recommend reset-scoring")
}

/// Resolve the runai DB path for a given HOME + optional override.
fn db_path(home: &Path, data: Option<&Path>) -> PathBuf {
    match data {
        Some(d) => d.join("runai.db"),
        None => home.join(".runai/runai.db"),
    }
}

/// Bootstrap the data dir so `Database::open` migrations apply. Easiest way
/// is to spawn `runai list` once — it always runs `SkillManager::new()` /
/// `with_base()` which calls `Database::open()` and runs every `init_schema`
/// migration.
fn bootstrap_db(home: &Path, data: Option<&Path>) {
    let out = match data {
        Some(d) => run_with_data_dir(home, d, &["list"]),
        None => run_default_home(home, &["list"]),
    };
    assert!(
        out.status.success(),
        "bootstrap `runai list` failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let db = db_path(home, data);
    assert!(
        db.exists(),
        "expected runai.db to be created at {} after `runai list`",
        db.display()
    );
}

/// Plant `count` rows into `resource_ai_summary` of the given DB.
/// Returns the names planted.
fn plant_summaries(db: &Path, count: usize) -> Vec<String> {
    let conn = Connection::open(db).expect("open runai.db");
    // The schema was created by the migration on prior `runai list` so the
    // table must exist by this point.
    let mut names = Vec::new();
    for i in 0..count {
        let name = format!("planted-skill-{i}");
        conn.execute(
            "INSERT INTO resource_ai_summary (name, summary, llm_score, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                name.clone(),
                format!("summary body for {name}"),
                5_i64,
                1_700_000_000_i64 + i as i64,
            ],
        )
        .expect("insert summary");
        names.push(name);
    }
    names
}

fn count_summaries(db: &Path) -> i64 {
    let conn = Connection::open(db).expect("reopen runai.db");
    conn.query_row(
        "SELECT COUNT(*) FROM resource_ai_summary",
        [],
        |r| r.get::<_, i64>(0),
    )
    .expect("count rows")
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// Plan §1.39.1: interactive `y` confirms and deletes all summaries.
///
/// - Prompt string is printed to stdout: `about to wipe all LLM summaries.
///   continue? [y/N]`.
/// - Typing `y` (followed by newline) proceeds.
/// - On completion stdout reports `deleted: 5 summaries` and the table is
///   empty.
#[test]
fn reset_scoring_prompts_confirmation() {
    let home = TempDir::new().expect("home tempdir");

    bootstrap_db(home.path(), None);
    let db = db_path(home.path(), None);
    let names = plant_summaries(&db, 5);
    assert_eq!(count_summaries(&db), 5, "5 summaries pre-condition");

    let out = run_reset_scoring_interactive(home.path(), None, "y\n");
    dump(&out, "reset-scoring with stdin=y");

    assert!(
        out.status.success(),
        "reset-scoring should succeed when user types y"
    );

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("about to wipe all LLM summaries. continue?"),
        "prompt string missing from stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("[y/N]"),
        "prompt should show [y/N] hint: {stdout:?}"
    );
    assert!(
        stdout.contains("deleted: 5 summaries"),
        "should report deletion of 5 summaries: {stdout:?}"
    );

    assert_eq!(
        count_summaries(&db),
        0,
        "all summaries should be wiped after y-confirm"
    );

    // sanity: names that existed should no longer be reachable
    let conn = Connection::open(&db).unwrap();
    for n in &names {
        let row: Result<i64, _> = conn.query_row(
            "SELECT COUNT(*) FROM resource_ai_summary WHERE name = ?1",
            rusqlite::params![n],
            |r| r.get(0),
        );
        assert_eq!(row.unwrap(), 0, "summary {n} should be gone");
    }
}

/// Plan §1.39.2: `--yes` skips the prompt and deletes immediately. Needed
/// for non-interactive automation / CI hooks.
#[test]
fn reset_scoring_yes_flag_skips_prompt() {
    let home = TempDir::new().expect("home tempdir");

    bootstrap_db(home.path(), None);
    let db = db_path(home.path(), None);
    plant_summaries(&db, 3);
    assert_eq!(count_summaries(&db), 3);

    let out = run_default_home(home.path(), &["recommend", "reset-scoring", "--yes"]);
    dump(&out, "reset-scoring --yes");

    assert!(out.status.success(), "reset-scoring --yes should succeed");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !stdout.contains("about to wipe all LLM summaries"),
        "--yes should NOT print the confirmation prompt: {stdout:?}"
    );
    assert!(
        stdout.contains("deleted: 3 summaries"),
        "should report deletion of 3 summaries: {stdout:?}"
    );

    assert_eq!(count_summaries(&db), 0, "table should be empty after --yes");
}

/// Plan §1.39.3a: user types `n` — command aborts, DB untouched.
#[test]
fn reset_scoring_user_abort_with_n() {
    let home = TempDir::new().expect("home tempdir");

    bootstrap_db(home.path(), None);
    let db = db_path(home.path(), None);
    plant_summaries(&db, 4);
    assert_eq!(count_summaries(&db), 4);

    let out = run_reset_scoring_interactive(home.path(), None, "n\n");
    dump(&out, "reset-scoring with stdin=n");

    assert!(
        out.status.success(),
        "aborting should still exit cleanly (0)"
    );

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("aborted"),
        "stdout should announce 'aborted': {stdout:?}"
    );
    assert!(
        !stdout.contains("deleted:"),
        "deletion line must NOT appear on abort: {stdout:?}"
    );

    assert_eq!(
        count_summaries(&db),
        4,
        "DB must be unchanged after abort (still 4 summaries)"
    );
}

/// Plan §1.39.3b: user submits an empty line (just <Enter>) — same as
/// declining; command aborts, DB untouched.
#[test]
fn reset_scoring_user_abort_with_empty_line() {
    let home = TempDir::new().expect("home tempdir");

    bootstrap_db(home.path(), None);
    let db = db_path(home.path(), None);
    plant_summaries(&db, 2);
    assert_eq!(count_summaries(&db), 2);

    let out = run_reset_scoring_interactive(home.path(), None, "\n");
    dump(&out, "reset-scoring with stdin=<empty>");

    assert!(
        out.status.success(),
        "empty-line abort should exit cleanly (0)"
    );

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("aborted"),
        "stdout should announce 'aborted' on empty input: {stdout:?}"
    );

    assert_eq!(
        count_summaries(&db),
        2,
        "DB must be unchanged after empty-line abort"
    );
}

/// Plan §1.39.4: `RUNE_DATA_DIR` is respected — running `reset-scoring`
/// against a custom data dir only wipes that DB, while the default
/// `~/.runai/runai.db` stays intact.
///
/// This guards the 4-20 / 4-27 root cause class: destructive ops must
/// honor the active data dir resolver, never silently fall through to the
/// default home location.
#[test]
fn reset_scoring_respects_rune_data_dir() {
    let home = TempDir::new().expect("home tempdir");
    let custom = TempDir::new().expect("custom data tempdir");

    // Both DBs get their schema set up (different sentinel summary counts
    // so we can tell them apart).
    bootstrap_db(home.path(), None);
    bootstrap_db(home.path(), Some(custom.path()));

    let default_db = db_path(home.path(), None);
    let custom_db = db_path(home.path(), Some(custom.path()));
    assert_ne!(
        default_db.canonicalize().unwrap(),
        custom_db.canonicalize().unwrap(),
        "default and custom DBs must be different files"
    );

    plant_summaries(&default_db, 7);
    plant_summaries(&custom_db, 3);
    assert_eq!(count_summaries(&default_db), 7);
    assert_eq!(count_summaries(&custom_db), 3);

    // Wipe ONLY the custom data dir.
    let out = run_with_data_dir(
        home.path(),
        custom.path(),
        &["recommend", "reset-scoring", "--yes"],
    );
    dump(&out, "reset-scoring --yes RUNE_DATA_DIR=custom");

    assert!(out.status.success(), "reset-scoring on custom dir should succeed");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("deleted: 3 summaries"),
        "should delete the 3 from custom dir: {stdout:?}"
    );

    // Custom is wiped, default is intact — the regression guard.
    assert_eq!(
        count_summaries(&custom_db),
        0,
        "custom dir's summaries should be wiped"
    );
    assert_eq!(
        count_summaries(&default_db),
        7,
        "default ~/.runai DB must NOT be touched when RUNE_DATA_DIR overrides"
    );
}
