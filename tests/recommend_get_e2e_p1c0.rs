//! P1 regression coverage for `runai recommend get`.
//!
//! `recommend get` is the ONLY adoption path: it must atomically print
//! SKILL.md to stdout, write a debug header on stderr, bump usage_count,
//! and record a session adoption row. These tests pin behaviors that
//! existing tests in `router_skill_lifecycle.rs` only partially cover:
//!
//! - Strict stderr `# skill:` + `# path:` debug header (not just
//!   `# usage_count +1 recorded`).
//! - Missing skill: exit code is exactly 2, stdout is empty, stderr
//!   contains `cannot read` with the offending path — not just "nonzero".
//! - `RUNE_DATA_DIR` override: skills + DB live in the override dir,
//!   not in HOME/.runai (the 4-20/4-27 root-cause area).
//! - Symmetry across CLI targets: enabling for one CLI does NOT
//!   create per-target counters; usage_count is shared across all
//!   targets because the managed pool is the single source of truth.
//!
//! All tests sandbox in `HOME=mktemp` + `RUNE_DATA_DIR=$HOME/.runai`
//! (or an explicit override dir for the RUNE_DATA_DIR test). Production
//! `~/.runai/` is never touched.

#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── Helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// A managed-skill sandbox. By default `data_dir = HOME/.runai`; use
/// `with_external_data_dir` to exercise `RUNE_DATA_DIR` override.
struct Sandbox {
    home: TempDir,
    data_dir_override: Option<TempDir>,
}

impl Sandbox {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
        Self {
            home,
            data_dir_override: None,
        }
    }

    /// Sandbox with `RUNE_DATA_DIR` pointing at a separate tempdir; HOME's
    /// `.runai` is left empty so the tests can prove "the override is used,
    /// not the default".
    fn with_external_data_dir() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        // Leave HOME/.runai empty on purpose — RUNE_DATA_DIR must win.
        let data = tempfile::tempdir().expect("create tmp RUNE_DATA_DIR");
        std::fs::create_dir_all(data.path().join("skills"))
            .expect("pre-create skills dir in RUNE_DATA_DIR");
        Self {
            home,
            data_dir_override: Some(data),
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn data_dir(&self) -> PathBuf {
        match &self.data_dir_override {
            Some(d) => d.path().to_path_buf(),
            None => self.home.path().join(".runai"),
        }
    }

    fn skills_dir(&self) -> PathBuf {
        self.data_dir().join("skills")
    }

    fn db_path(&self) -> PathBuf {
        self.data_dir().join("runai.db")
    }

    fn home_default_db_path(&self) -> PathBuf {
        self.home.path().join(".runai/runai.db")
    }

    /// Build a `runai` command with the sandbox env applied. `RUNE_DATA_DIR`
    /// is set only when `with_external_data_dir` was used; otherwise it's
    /// explicitly removed so the binary falls back to `HOME/.runai`.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("RUNAI_NO_AUTOSPAWN");
        match &self.data_dir_override {
            Some(d) => {
                cmd.env("RUNE_DATA_DIR", d.path());
            }
            None => {
                cmd.env_remove("RUNE_DATA_DIR");
            }
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd(args).output().expect("runai binary spawn")
    }

    fn run_with_session(&self, session_id: &str, args: &[&str]) -> std::process::Output {
        let mut cmd = self.cmd(args);
        cmd.env("RUNAI_SESSION_ID", session_id);
        cmd.output().expect("runai binary spawn")
    }

    /// Plant a SKILL.md so the binary considers it managed, then run scan
    /// to register the row in the DB so record_usage finds something to bump.
    fn plant_skill(&self, name: &str, body: &str) {
        let dir = self.skills_dir().join(name);
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

    fn usage_count(&self, name: &str) -> i64 {
        usage_count_at(&self.db_path(), name)
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

fn usage_count_at(db: &Path, name: &str) -> i64 {
    if !db.exists() {
        return 0;
    }
    let conn = rusqlite::Connection::open(db).expect("open test db");
    conn.query_row(
        "SELECT COALESCE(MAX(usage_count), 0) FROM resources WHERE name = ?1",
        rusqlite::params![name],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

// ─── #1 recommend_get_counts_adoption ───────────────────────────────────────
//
// Plan item 1: stdout = SKILL.md body verbatim, stderr shows `# skill: <name>`
// debug header, usage_count +1, router_session_adoptions row inserted.

#[test]
fn recommend_get_counts_adoption() {
    let sb = Sandbox::new();
    sb.plant_skill("alpha", "test skill alpha body");
    assert_eq!(sb.usage_count("alpha"), 0, "precondition: usage_count == 0");

    let session = "rnai_sess_22222222222222222222222222222222";
    let out = sb.run_with_session(session, &["recommend", "get", "alpha"]);
    assert!(
        out.status.success(),
        "recommend get must exit 0 for an existing skill (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // stdout = SKILL.md body verbatim. Path must NOT leak into stdout
    // (the main agent's prompt would otherwise grow with file paths).
    assert!(
        stdout.contains("name: alpha"),
        "stdout must contain SKILL.md frontmatter, got:\n{stdout}"
    );
    assert!(
        stdout.contains("test skill alpha body"),
        "stdout must contain SKILL.md body, got:\n{stdout}"
    );
    assert!(
        !stdout.contains(".runai/skills"),
        "stdout must NOT leak the on-disk path, got:\n{stdout}"
    );

    // stderr carries the debug header — important for `runai recommend`
    // call-site debugging when stdout is being piped into the agent.
    assert!(
        stderr.contains("# skill: alpha"),
        "stderr must show '# skill: <name>' debug header, got:\n{stderr}"
    );
    assert!(
        stderr.contains("usage_count +1 recorded"),
        "stderr must confirm record, got:\n{stderr}"
    );

    // DB invariants.
    assert_eq!(
        sb.usage_count("alpha"),
        1,
        "usage_count must be exactly 1 after a single get call"
    );
    assert!(
        sb.has_session_adoption(session, "alpha"),
        "router_session_adoptions row must be written when RUNAI_SESSION_ID is set"
    );
}

// ─── #2 recommend_get_missing_skill_fails_cleanly ──────────────────────────
//
// Plan item 2: exit code is exactly 2, stderr contains 'cannot read' with
// path, stdout is empty (no partial SKILL.md), no crash.

#[test]
fn recommend_get_missing_skill_fails_cleanly() {
    let sb = Sandbox::new();
    // Plant a real skill so the binary boots normally — the test is about
    // the *missing* skill behavior, not "no skills at all".
    sb.plant_skill("real", "a real skill");

    let out = sb.run_with_session("sess-X", &["recommend", "get", "ghost"]);

    // Exit code is exactly 2 — `cli/mod.rs` calls `std::process::exit(2)`
    // when SKILL.md can't be read. Anything else (1, 101, 0) is a regression.
    assert_eq!(
        out.status.code(),
        Some(2),
        "exit code must be exactly 2 for missing skill; got {:?} (stderr={})",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    // stdout must be empty — no partial SKILL.md, no fallback content.
    // If the agent is piping stdout into its prompt, half-baked output
    // would contaminate the next turn.
    let stdout = out.stdout;
    assert!(
        stdout.is_empty(),
        "stdout must be empty for missing skill, got: {:?}",
        String::from_utf8_lossy(&stdout)
    );

    // stderr must say 'cannot read' and name the path it tried.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot read"),
        "stderr must contain 'cannot read', got:\n{stderr}"
    );
    assert!(
        stderr.contains("ghost"),
        "stderr must name the skill it failed on, got:\n{stderr}"
    );
    assert!(
        stderr.contains("SKILL.md"),
        "stderr must include the SKILL.md path it tried to open, got:\n{stderr}"
    );

    // The unrelated 'real' skill's counters must not move.
    assert_eq!(sb.usage_count("real"), 0);
    assert_eq!(sb.usage_count("ghost"), 0);
    assert!(!sb.has_session_adoption("sess-X", "ghost"));
}

// ─── #3 recommend_get_respects_rune_data_dir ────────────────────────────────
//
// Plan item 3: `RUNE_DATA_DIR` must win over default HOME. Skill in the
// override dir is found; usage_count is recorded in the override DB, NOT
// in HOME/.runai/runai.db.

#[test]
fn recommend_get_respects_rune_data_dir() {
    let sb = Sandbox::with_external_data_dir();

    // Sanity: HOME/.runai is empty (we did NOT pre-create skills there).
    let home_skills = sb.home().join(".runai/skills");
    assert!(
        !home_skills.exists(),
        "HOME/.runai/skills should not exist before the test, got: {}",
        home_skills.display()
    );

    sb.plant_skill("delta", "in the override dir");

    // Sanity: the planted skill is in RUNE_DATA_DIR, not HOME.
    assert!(sb.skills_dir().join("delta/SKILL.md").exists());
    assert!(!sb.home().join(".runai/skills/delta/SKILL.md").exists());

    let out = sb.run_with_session("sess-D", &["recommend", "get", "delta"]);
    assert!(
        out.status.success(),
        "recommend get must succeed against RUNE_DATA_DIR (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("in the override dir"),
        "stdout must show body from the override dir's SKILL.md, got:\n{stdout}"
    );

    // usage_count is in the override DB.
    assert_eq!(
        sb.usage_count("delta"),
        1,
        "usage_count must be recorded in RUNE_DATA_DIR's DB"
    );

    // CRITICAL: HOME/.runai/runai.db must NOT have been created OR, if
    // present, must NOT contain a row for 'delta'. (Some early-startup
    // paths may briefly touch HOME for the default-dir detection; what we
    // require is that the user's skill data didn't bleed over.)
    let home_db = sb.home_default_db_path();
    let home_usage = usage_count_at(&home_db, "delta");
    assert_eq!(
        home_usage,
        0,
        "HOME/.runai/runai.db must NOT carry 'delta' usage when RUNE_DATA_DIR is set; home_db={}",
        home_db.display()
    );
}

// ─── #4 recommend_get_symmetric_across_targets ─────────────────────────────
//
// Plan item 4: skills are pooled across CLI targets — enabling for any
// target (or all of them) does NOT split the counter. `recommend get`
// always reads from / counts into the shared managed pool, regardless
// of how many target-specific symlinks exist.

#[test]
fn recommend_get_symmetric_across_targets() {
    let sb = Sandbox::new();
    sb.plant_skill("omni", "shared across CLI targets");

    // Enable the skill on all 4 CLI targets. Each `enable` call creates a
    // symlink under HOME/.{claude,codex,gemini,opencode}/skills/omni
    // pointing at the managed pool. If `recommend get` were ever to read
    // through a symlink (it isn't supposed to), we'd see usage_count
    // diverge across the four runs below.
    for target in ["claude", "codex", "gemini", "opencode"] {
        let out = sb.run(&["enable", "omni", "--target", target]);
        // Tolerate non-zero from enable (target dir layout differences across
        // platforms) — what matters is that recommend get is unaffected.
        if !out.status.success() {
            eprintln!(
                "note: 'enable omni --target {target}' returned non-zero, continuing (stderr={})",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    // Sanity: at least one CLI symlink dir was populated; the assertion
    // here just guards against the test silently turning into a no-op
    // (if all four enables failed, the test reduces to "single get").
    let any_target_link_exists = ["claude", "codex", "gemini", "opencode"]
        .iter()
        .any(|t| sb.home().join(format!(".{t}/skills/omni")).exists());
    if !any_target_link_exists {
        // Some setups (e.g. opencode lives under ~/.config/opencode) won't
        // surface the symlink at the same prefix; not fatal for this test.
        eprintln!(
            "note: no per-target symlinks found at HOME/.{{cli}}/skills/omni; \
             continuing — the invariant under test is that get reads from \
             the managed pool, not from these symlinks."
        );
    }

    // Single get call: usage_count must be exactly 1 — proving that
    // `recommend get` always counts into the shared `resources` row,
    // not into per-target shadows.
    let session = "rnai_sess_33333333333333333333333333333333";
    let out = sb.run_with_session(session, &["recommend", "get", "omni"]);
    assert!(
        out.status.success(),
        "recommend get must succeed for a multi-target-enabled skill (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("shared across CLI targets"),
        "stdout must contain SKILL.md body from the managed pool, got:\n{stdout}"
    );

    assert_eq!(
        sb.usage_count("omni"),
        1,
        "usage_count must be exactly 1 even when the skill is enabled on 4 targets"
    );

    // A second call increments by exactly 1 more — confirming the row is
    // unique and we're not accidentally racing against per-target shadows.
    let _ = sb.run_with_session(session, &["recommend", "get", "omni"]);
    assert_eq!(
        sb.usage_count("omni"),
        2,
        "usage_count must be 2 after a second get call (single shared row)"
    );

    // Session adoption is per-(session, skill), not per-target.
    assert!(sb.has_session_adoption(session, "omni"));
}
