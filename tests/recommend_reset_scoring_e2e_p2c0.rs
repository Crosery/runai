//! P2 e2e tests for `runai recommend reset-scoring`.
//!
//! Covers PLANNING test-plan §1.39: destructive operation that deletes
//! every row in `resource_ai_summary`. We exercise:
//!   1. interactive confirmation prompt + 'y' branch
//!   2. `--yes` flag short-circuits the prompt
//!   3. user types 'n' / empty newline → aborts, DB untouched
//!   4. `RUNE_DATA_DIR` scoping → only that DB wiped, default HOME db untouched
//!
//! All tests sandbox HOME via tempfile and `RUNE_DATA_DIR`. We NEVER touch
//! the real `~/.runai/`. Each test plants AI-summary rows directly via
//! rusqlite after first running `runai list` once so the migrations
//! initialize the schema.
#![cfg(not(target_os = "windows"))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

const RUNAI_BIN: &str = env!("CARGO_BIN_EXE_runai");

// ─── helpers ────────────────────────────────────────────────────────────────

struct TestEnv {
    home: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn default_db_path(&self) -> PathBuf {
        self.home().join(".runai/runai.db")
    }

    /// Spawn the installed binary with isolated HOME, default `RUNE_DATA_DIR`
    /// (i.e. HOME/.runai). Stdin/stdout captured so we can drive prompts.
    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(RUNAI_BIN);
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("spawn runai binary")
    }

    fn run_with_rune_data(&self, rune_data: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(RUNAI_BIN);
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", rune_data)
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("spawn runai binary")
    }

    /// Spawn the binary, write `stdin_bytes` to its stdin, then return the
    /// captured output. Used for the interactive y/N prompt tests.
    fn run_with_stdin(&self, args: &[&str], stdin_bytes: &[u8]) -> std::process::Output {
        let mut child = Command::new(RUNAI_BIN)
            .args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn runai binary with stdin pipe");
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_bytes).expect("write to runai stdin");
            // Drop stdin to close it (EOF).
        }
        child.wait_with_output().expect("wait for runai")
    }

    /// Run any cheap subcommand so DB migrations create the schema. `list`
    /// is read-only on a fresh data dir and exits immediately.
    fn init_schema(&self) {
        let out = self.run(&["list"]);
        assert!(
            out.status.success(),
            "list must succeed to bootstrap DB (stderr={})",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_schema_with_rune_data(&self, rune_data: &Path) {
        let out = self.run_with_rune_data(rune_data, &["list"]);
        assert!(
            out.status.success(),
            "list must succeed to bootstrap DB at {:?} (stderr={})",
            rune_data,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Insert N rows directly into resource_ai_summary; each row contains
    /// distinct name + summary so deletions are observable. Schema v9+:
    /// columns (name, summary, updated_at, llm_score).
    fn plant_summaries(db_path: &Path, count: usize) {
        let conn = rusqlite::Connection::open(db_path).expect("open db to plant summaries");
        for i in 0..count {
            conn.execute(
                "INSERT INTO resource_ai_summary (name, summary, updated_at, llm_score)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    format!("skill-{i}"),
                    format!("planted summary #{i}"),
                    1_700_000_000_i64 + i as i64,
                    5_i64
                ],
            )
            .expect("insert planted summary row");
        }
    }

    fn count_summaries(db_path: &Path) -> i64 {
        let conn = rusqlite::Connection::open(db_path).expect("open db to count summaries");
        conn.query_row("SELECT COUNT(*) FROM resource_ai_summary", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect("count rows")
    }
}

// ─── 1. interactive confirmation prompt + 'y' branch ─────────────────────────

#[test]
fn reset_scoring_prompts_confirmation() {
    let env = TestEnv::new();
    env.init_schema();
    TestEnv::plant_summaries(&env.default_db_path(), 5);
    assert_eq!(
        TestEnv::count_summaries(&env.default_db_path()),
        5,
        "precondition: 5 planted summaries"
    );

    // Without --yes: type 'y\n' to confirm. Binary should prompt, then delete.
    let out = env.run_with_stdin(&["recommend", "reset-scoring"], b"y\n");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(
        out.status.success(),
        "reset-scoring with 'y' must succeed (stdout={stdout} stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("about to wipe all LLM summaries. continue? [y/N]"),
        "stdout must contain the confirmation prompt, got: {stdout}"
    );
    assert!(
        stdout.contains("deleted: 5 summaries"),
        "stdout must report deletion count 5, got: {stdout}"
    );
    assert_eq!(
        TestEnv::count_summaries(&env.default_db_path()),
        0,
        "after confirmation all summaries must be deleted"
    );
}

// ─── 2. --yes skips prompt and deletes immediately ───────────────────────────

#[test]
fn reset_scoring_yes_flag_skips_prompt() {
    let env = TestEnv::new();
    env.init_schema();
    TestEnv::plant_summaries(&env.default_db_path(), 3);
    assert_eq!(
        TestEnv::count_summaries(&env.default_db_path()),
        3,
        "precondition: 3 planted summaries"
    );

    let out = env.run(&["recommend", "reset-scoring", "--yes"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(
        out.status.success(),
        "reset-scoring --yes must succeed (stdout={stdout} stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stdout.contains("continue? [y/N]"),
        "--yes must NOT print the confirmation prompt, got: {stdout}"
    );
    assert!(
        stdout.contains("deleted: 3 summaries"),
        "stdout must report deletion count 3, got: {stdout}"
    );
    assert_eq!(
        TestEnv::count_summaries(&env.default_db_path()),
        0,
        "after --yes all summaries must be deleted"
    );
}

// ─── 3. user types 'n' (or empty newline) → aborts, DB untouched ─────────────

#[test]
fn reset_scoring_user_abort_on_n() {
    let env = TestEnv::new();
    env.init_schema();
    TestEnv::plant_summaries(&env.default_db_path(), 4);
    assert_eq!(TestEnv::count_summaries(&env.default_db_path()), 4);

    let out = env.run_with_stdin(&["recommend", "reset-scoring"], b"n\n");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(
        out.status.success(),
        "reset-scoring with 'n' must exit cleanly (stdout={stdout} stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("aborted"),
        "stdout must say 'aborted' on 'n', got: {stdout}"
    );
    assert!(
        !stdout.contains("deleted:"),
        "stdout must NOT contain a deletion line, got: {stdout}"
    );
    assert_eq!(
        TestEnv::count_summaries(&env.default_db_path()),
        4,
        "DB must be untouched after abort"
    );
}

#[test]
fn reset_scoring_user_abort_on_empty_line() {
    let env = TestEnv::new();
    env.init_schema();
    TestEnv::plant_summaries(&env.default_db_path(), 2);
    assert_eq!(TestEnv::count_summaries(&env.default_db_path()), 2);

    // Just press Enter — empty line, default is N.
    let out = env.run_with_stdin(&["recommend", "reset-scoring"], b"\n");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(
        out.status.success(),
        "reset-scoring with empty line must exit cleanly (stdout={stdout})"
    );
    assert!(
        stdout.contains("aborted"),
        "stdout must say 'aborted' on empty line (N default), got: {stdout}"
    );
    assert_eq!(
        TestEnv::count_summaries(&env.default_db_path()),
        2,
        "empty-line default-N must NOT delete anything"
    );
}

// ─── 4. RUNE_DATA_DIR scoping → only that DB wiped ──────────────────────────

#[test]
fn reset_scoring_respects_rune_data_dir() {
    let env = TestEnv::new();
    // First initialize the default-HOME data dir and plant rows there.
    env.init_schema();
    TestEnv::plant_summaries(&env.default_db_path(), 7);
    assert_eq!(TestEnv::count_summaries(&env.default_db_path()), 7);

    // Second, initialize a *different* data dir via RUNE_DATA_DIR and plant
    // a smaller set of rows in it.
    let alt = tempfile::tempdir().expect("alt data dir");
    let alt_data = alt.path().join("alt-runai");
    std::fs::create_dir_all(alt_data.join("skills")).expect("pre-create alt skills dir");
    env.init_schema_with_rune_data(&alt_data);
    let alt_db = alt_data.join("runai.db");
    TestEnv::plant_summaries(&alt_db, 3);
    assert_eq!(TestEnv::count_summaries(&alt_db), 3);

    // Run reset-scoring --yes targeting ONLY the alt data dir.
    let out = env.run_with_rune_data(&alt_data, &["recommend", "reset-scoring", "--yes"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(
        out.status.success(),
        "reset-scoring against alt data dir must succeed (stdout={stdout} stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("deleted: 3 summaries"),
        "alt DB had 3 rows; output must say 'deleted: 3 summaries', got: {stdout}"
    );

    // Alt DB must now be empty.
    assert_eq!(
        TestEnv::count_summaries(&alt_db),
        0,
        "alt DB must be wiped by the RUNE_DATA_DIR-scoped reset"
    );
    // Default HOME DB must still have its 7 rows.
    assert_eq!(
        TestEnv::count_summaries(&env.default_db_path()),
        7,
        "default HOME DB must NOT be touched by an alt-dir reset"
    );
}
