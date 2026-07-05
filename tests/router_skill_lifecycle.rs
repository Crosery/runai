//! Integration tests for the `runai recommend get` adoption + counting
//! pipeline. `runai recommend get` is the **only** path through which
//! a skill's SKILL.md leaves runai and the only path that bumps
//! usage_count + writes a session-adoption row — these tests lock that
//! invariant by exercising the real binary against an isolated HOME
//! tempdir (production `~/.runai/` is never touched).
//!
//! Pass criteria are written down here, not in prose:
//!   - `recommend_get_*` tests assert (stdout, stderr, usage_count delta,
//!     session_adoption row) after a single command invocation.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── Helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

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

    fn managed_skills_dir(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    fn db_path(&self) -> PathBuf {
        self.home().join(".runai/runai.db")
    }

    /// Plant a SKILL.md so the binary considers it a managed skill, then
    /// register it in the DB by running `runai scan` (which inserts the
    /// resource row record_usage needs to find).
    fn plant_skill(&self, name: &str, body: &str) {
        let dir = self.managed_skills_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n"),
        )
        .unwrap();
        let out = self.run(&["scan"]);
        assert!(
            out.status.success(),
            "scan must succeed to register planted skill (stderr={})",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("RUNAI_SESSION_ID")
            .env_remove("CLAUDE_SESSION_ID");
        cmd.output().expect("runai binary spawn")
    }

    fn run_with_session(&self, session_id: &str, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env("RUNAI_SESSION_ID", session_id);
        cmd.output().expect("runai binary spawn")
    }

    /// Read `usage_count` for a skill from the test DB. Returns 0 when the
    /// resource row is missing — same default the production query produces.
    fn usage_count(&self, name: &str) -> i64 {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        conn.query_row(
            "SELECT COALESCE(MAX(usage_count), 0) FROM resources WHERE name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    fn has_session_adoption(&self, session_id: &str, skill_name: &str) -> bool {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        conn.query_row(
            "SELECT 1 FROM router_session_adoptions WHERE session_id = ?1 AND skill_name = ?2",
            rusqlite::params![session_id, skill_name],
            |_| Ok(()),
        )
        .is_ok()
    }
}

// ─── `runai recommend get` ──────────────────────────────────────────────────

#[test]
fn recommend_get_returns_skill_md_and_increments_usage_count() {
    let env = TestEnv::new();
    env.plant_skill("alpha", "test skill alpha");
    assert_eq!(env.usage_count("alpha"), 0, "precondition: usage starts 0");

    let session_a = "rnai_sess_44444444444444444444444444444444";
    let out = env.run_with_session(session_a, &["recommend", "get", "alpha"]);
    assert!(
        out.status.success(),
        "get must exit 0 for an existing skill (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // stdout = SKILL.md body verbatim (frontmatter + body).
    assert!(
        stdout.contains("name: alpha"),
        "stdout must contain SKILL.md frontmatter, got:\n{stdout}"
    );
    assert!(
        stdout.contains("test skill alpha"),
        "stdout must contain SKILL.md body, got:\n{stdout}"
    );

    // stderr = bookkeeping receipt.
    assert!(
        stderr.contains("usage_count +1 recorded"),
        "stderr must confirm record, got:\n{stderr}"
    );
    assert!(stderr.contains("alpha"));

    // DB invariants.
    assert_eq!(
        env.usage_count("alpha"),
        1,
        "usage_count must be 1 after one get call"
    );
    assert!(
        env.has_session_adoption(session_a, "alpha"),
        "session_adoptions row must be written when RUNAI_SESSION_ID is set"
    );
}

#[test]
fn recommend_get_idempotent_in_session_increments_each_call() {
    let env = TestEnv::new();
    env.plant_skill("beta", "test skill beta");

    let session_b = "rnai_sess_55555555555555555555555555555555";
    let _ = env.run_with_session(session_b, &["recommend", "get", "beta"]);
    let _ = env.run_with_session(session_b, &["recommend", "get", "beta"]);
    let _ = env.run_with_session(session_b, &["recommend", "get", "beta"]);

    // usage_count is a raw counter: 3 calls → 3 increments.
    // (Session dedup happens at the router-recommend layer, not here.)
    assert_eq!(env.usage_count("beta"), 3);
    assert!(env.has_session_adoption(session_b, "beta"));
}

#[test]
fn recommend_get_without_session_id_still_increments_usage_count() {
    let env = TestEnv::new();
    env.plant_skill("gamma", "test skill gamma");

    let out = env.run(&["recommend", "get", "gamma"]);
    assert!(out.status.success());
    assert_eq!(env.usage_count("gamma"), 1);
    // No session id → no session_adoption row (verified by absence).
    assert!(!env.has_session_adoption("", "gamma"));
}

#[test]
fn recommend_get_missing_skill_exits_nonzero_and_does_not_touch_db() {
    let env = TestEnv::new();
    env.plant_skill("real", "a real skill");

    let out = env.run_with_session("sess-X", &["recommend", "get", "ghost"]);
    assert!(
        !out.status.success(),
        "missing skill must produce non-zero exit"
    );
    assert_eq!(
        env.usage_count("real"),
        0,
        "an unrelated skill's usage_count must not move"
    );
    assert_eq!(env.usage_count("ghost"), 0);
    assert!(!env.has_session_adoption("sess-X", "ghost"));
}

// PostToolUse and `Used` are gone — legacy local `recommend get` remains
// tested here, while new agent-facing adoption goes through runai-client.
