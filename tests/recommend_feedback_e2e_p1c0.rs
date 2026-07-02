//! Integration tests for `runai recommend feedback <skill> --note "..."`.
//!
//! The feedback subcommand re-evaluates a single skill's LLM summary and
//! llm_score in light of a user-provided note. The router LLM is contacted
//! through `cfg.base_url` (OpenAI-compat) so we mock it with an in-process
//! TCP listener and point `~/.runai/config.toml` at the mock URL.
//!
//! Test plan (runai-158-test-plan.md §1.38 — recommend feedback):
//!   1. `recommend_feedback_adjusts_score`     — happy path; DB row updated.
//!   2. `recommend_feedback_skill_not_found`   — graceful error, no DB damage.
#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── Helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Test environment: an isolated $HOME tempdir with a pre-created managed
/// `~/.runai/skills` directory. The default `~/.runai/` is NEVER touched.
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

    fn config_path(&self) -> PathBuf {
        self.home().join(".runai/config.toml")
    }

    /// Plant a skill with the given SKILL.md body and register it in the DB
    /// via `runai scan`.
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

    /// Write recommend config pointing to the given mock LLM base_url.
    fn write_config(&self, base_url: &str) {
        let toml = format!(
            "[recommend]\n\
             enabled = true\n\
             provider = \"openai-compat\"\n\
             base_url = \"{base_url}\"\n\
             model = \"mock-model\"\n\
             api_key = \"mock-key\"\n\
             top_k = 8\n\
             min_prompt_len = 0\n\
             summary_lang = \"en\"\n"
        );
        std::fs::write(self.config_path(), toml).expect("write config.toml");
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("RUNAI_RECOMMEND_API_KEY");
        cmd.output().expect("runai binary spawn")
    }

    /// Pre-seed a row in `resource_ai_summary` so feedback has an "old" state
    /// to read.
    fn set_summary(&self, name: &str, summary: &str, llm_score: i64) {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        conn.execute(
            "INSERT INTO resource_ai_summary (owner_user_id, name, summary, llm_score, updated_at)
             VALUES ('', ?1, ?2, ?3, ?4)
             ON CONFLICT(owner_user_id, name) DO UPDATE SET
                summary = excluded.summary,
                llm_score = excluded.llm_score,
                updated_at = excluded.updated_at",
            rusqlite::params![name, summary, llm_score, 1_700_000_000_i64],
        )
        .expect("seed summary");
    }

    fn get_summary(&self, name: &str) -> Option<(String, i64)> {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        conn.query_row(
            "SELECT summary, llm_score FROM resource_ai_summary WHERE name = ?1",
            rusqlite::params![name],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .ok()
    }
}

// ─── Mock LLM server ────────────────────────────────────────────────────────

/// Spin up a tiny TCP listener that serves a fixed OpenAI-compatible
/// chat-completions JSON. Returns the base URL the client should hit
/// (e.g. `http://127.0.0.1:54321`) and a shutdown handle that stops the
/// background thread.
struct MockLlm {
    base_url: String,
    shutdown: Arc<AtomicBool>,
}

impl MockLlm {
    /// `content` is the LLM's reply body (will be JSON-escaped into
    /// `choices[0].message.content`).
    fn start(content: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock LLM");
        listener.set_nonblocking(true).expect("set nonblocking");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{}", addr);
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let body_content = content.to_string();
        thread::spawn(move || {
            // Accept loop: poll until shutdown flips.
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Read until end-of-headers (or up to 64KB), then ignore
                        // any body — we always reply the same canned JSON.
                        stream
                            .set_read_timeout(Some(Duration::from_millis(2000)))
                            .ok();
                        let mut buf = [0u8; 8192];
                        let mut total = Vec::new();
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    total.extend_from_slice(&buf[..n]);
                                    if total.windows(4).any(|w| w == b"\r\n\r\n") {
                                        // Got the headers. Try to drain body
                                        // briefly so the client's write completes.
                                        let _ = stream.read(&mut buf);
                                        break;
                                    }
                                    if total.len() > 1 << 20 {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        // Compose canned chat-completions JSON.
                        let escaped = serde_json::to_string(&body_content).unwrap();
                        let json = format!(
                            "{{\"id\":\"mock-1\",\"object\":\"chat.completion\",\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":{escaped}}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}}}"
                        );
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            json.len(),
                            json
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Self { base_url, shutdown }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for MockLlm {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

/// `runai recommend feedback <skill> --note "..."` re-runs the LLM,
/// updates `resource_ai_summary` with the new summary + adjusted score,
/// and prints `feedback applied to <skill>\n  llm_score: OLD → NEW\n  summary updated: <len> chars`.
#[test]
fn recommend_feedback_adjusts_score() {
    let env = TestEnv::new();
    env.plant_skill("alpha", "alpha skill description");
    env.set_summary("alpha", "task: old\ntriggers: a, b\n", 7);

    // Mock LLM reply: a valid 6-field summary block with score 3 → routing
    // signal should drop from 7 (old) to 3 (new).
    let mock = MockLlm::start(
        "task: refined task line for alpha\n\
         triggers: alpha, beta, gamma\n\
         inputs: text\n\
         outputs: text\n\
         not-for: unsuitable cases\n\
         score: 3\n",
    );
    env.write_config(mock.base_url());

    let out = env.run(&["recommend", "feedback", "alpha", "--note", "too general"]);
    assert!(
        out.status.success(),
        "feedback must exit 0 on happy path (stderr={})\nstdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("feedback applied to alpha"),
        "stdout must announce 'feedback applied to alpha', got:\n{stdout}"
    );
    // Old → new score should both appear; the arrow glyph is "→".
    assert!(
        stdout.contains("llm_score:"),
        "stdout must show llm_score line, got:\n{stdout}"
    );
    assert!(
        stdout.contains("7") && stdout.contains("3"),
        "stdout must mention both old(7) and new(3) scores, got:\n{stdout}"
    );
    assert!(
        stdout.contains("summary updated"),
        "stdout must report new summary length, got:\n{stdout}"
    );

    // DB invariants: summary replaced + score lowered.
    let (new_summary, new_score) = env
        .get_summary("alpha")
        .expect("summary row must exist post-feedback");
    assert_eq!(new_score, 3, "DB score must reflect LLM's new score (3)");
    assert!(
        new_summary.contains("triggers: alpha, beta, gamma"),
        "DB summary must be the regenerated text, got:\n{new_summary}"
    );
    assert!(
        !new_summary.contains("score:"),
        "summary text must have the score: line stripped (it is stored in llm_score column), got:\n{new_summary}"
    );
}

/// Asking for feedback on a non-existent skill must fail with a clear
/// "skill not found" message and not modify the DB.
#[test]
fn recommend_feedback_skill_not_found() {
    let env = TestEnv::new();
    env.plant_skill("real", "a real skill");
    env.set_summary("real", "task: real\n", 5);

    // Mock is started but should never be hit (the not-found bail happens
    // before any LLM call).
    let mock = MockLlm::start("task: should-never-render\nscore: 1\n");
    env.write_config(mock.base_url());

    let out = env.run(&["recommend", "feedback", "ghost", "--note", "missing"]);
    assert!(
        !out.status.success(),
        "feedback on a missing skill must exit non-zero (stdout={})",
        String::from_utf8_lossy(&out.stdout)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("skill not found") || combined.contains("not found: ghost"),
        "error must say 'skill not found' (or include the missing name), got:\n{combined}"
    );

    // DB invariants: the unrelated, pre-existing summary stays put.
    let (kept_summary, kept_score) = env
        .get_summary("real")
        .expect("pre-existing summary must remain intact after a failed feedback call");
    assert_eq!(kept_score, 5, "unrelated skill's score must not move");
    assert!(
        kept_summary.contains("task: real"),
        "unrelated skill's summary must not be rewritten, got:\n{kept_summary}"
    );
    // And the missing skill must NOT have acquired a phantom summary row.
    assert!(
        env.get_summary("ghost").is_none(),
        "no summary row may be created for a non-existent skill"
    );
}
