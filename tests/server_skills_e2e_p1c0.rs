//! P1 e2e regression for runai dashboard skill endpoints:
//!   - GET /api/skills
//!   - GET /api/skill/{name}
//!   - GET /api/skill/{name}/files
//!   - GET /api/skill/{name}/file?path=...
//!
//! Strategy: spawn the real `runai server` binary against a sandboxed
//! HOME tempdir so production `~/.runai/` is never touched. Plant skills
//! by writing SKILL.md + (for /files and /file) auxiliary files into
//! `<HOME>/.runai/skills/<name>/`, then run `runai scan` so the DB
//! resource rows exist (needed by `/api/skills*` list/detail paths).
//!
//! The cloud HEAD `src/server.rs` lookups are **anonymous**: there is no
//! `current_owner_id`, no Bearer auth, and no `owner_user_id` field on
//! resources. Plan tests that depend on per-user tenant isolation,
//! auth-required gating, the `owner_user_id` serialized field, or the
//! cold-server compat carve-out for auth are NOT expressible here and
//! are skipped at the orchestrator level (phase=src_missing).

#![cfg(not(target_os = "windows"))]
// Some helpers in the Server impl are only used by tests added in later
// commits in this same file; suppress the unused-method warning so the
// first commit (which only exercises /api/skills) still compiles
// cleanly.
#![allow(dead_code)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

// Use the worktree-built binary (matches the source tree under test).
// The orchestrator originally pointed at `/Users/crosery/.cargo/bin/runai`
// but that is a globally-installed beta.5 with auth gates the cloud HEAD
// source does NOT have; for this test file we want the binary built from
// the same `src/server.rs` we are exercising — produced by `cargo build`
// into the shared target dir.
const RUNAI_BIN: &str = "/tmp/runai-p0-shared-target/debug/runai";

struct Server {
    child: Child,
    port: u16,
    home: TempDir,
}

impl Server {
    fn spawn() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");

        // Pick an unused TCP port. Closing the listener immediately drops
        // the bind so the child can grab it. There's a tiny race window —
        // good enough for tests on a developer machine, and we retry via
        // wait_ready below.
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").expect("pick free port");
            let p = l.local_addr().unwrap().port();
            drop(l);
            p
        };

        let data_dir = home.path().join(".runai");
        let child = Command::new(RUNAI_BIN)
            .args([
                "server",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .env("HOME", home.path())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", &data_dir)
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn runai server");

        let srv = Self { child, port, home };
        srv.wait_ready();
        srv
    }

    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let url = format!("http://127.0.0.1:{}/api/summary", self.port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        loop {
            if let Ok(resp) = client.get(&url).send() {
                if resp.status().is_success() || resp.status() == 500 {
                    // 200 or 500 both indicate the server is bound; we
                    // accept either for readiness (some endpoints will
                    // 500 until DB is initialised by SkillManager).
                    return;
                }
            }
            if Instant::now() >= deadline {
                panic!("runai server did not respond on port {}", self.port);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn skills_dir(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    fn db_path(&self) -> PathBuf {
        self.home().join(".runai/runai.db")
    }

    /// Plant a skill on disk and register it in the DB by running
    /// `runai scan` against the same sandboxed HOME / RUNE_DATA_DIR.
    fn plant_skill(&self, name: &str, body: &str) {
        let dir = self.skills_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n"),
        )
        .unwrap();
        self.run_scan();
    }

    fn plant_skill_with_md_body(&self, name: &str, md_body: &str) {
        let dir = self.skills_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), md_body).unwrap();
        self.run_scan();
    }

    fn run_scan(&self) {
        let out = Command::new(RUNAI_BIN)
            .args(["scan"])
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.home().join(".runai"))
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .output()
            .expect("runai scan spawn");
        assert!(
            out.status.success(),
            "scan failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Direct-write an LLM score row into the DB so we can exercise the
    /// sort-by-score branch without running the real enrich pipeline.
    fn set_llm_score(&self, name: &str, score: i64) {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        // INSERT into resource_ai_summary with an empty summary, then set
        // llm_score. The "enriched" field counts only non-empty summaries,
        // so a non-empty summary here also lets us test that branch.
        conn.execute(
            "INSERT INTO resource_ai_summary (name, summary, llm_score, updated_at)
             VALUES (?1, '', ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET llm_score = excluded.llm_score",
            rusqlite::params![name, score, chrono::Utc::now().timestamp()],
        )
        .expect("set llm_score");
    }

    fn insert_router_event(&self, ts: i64, chosen_skills: &[&str]) {
        let chosen_json = serde_json::to_string(chosen_skills).unwrap();
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        conn.execute(
            "INSERT INTO router_events (
                ts, provider, model,
                prompt_tokens, completion_tokens, reasoning_tokens, total_tokens,
                cache_hit_tokens, cache_miss_tokens, latency_ms,
                chosen_skills_json, candidate_count, status, error_msg,
                session_id, mode, user_prompt, cwd, bm25_kept,
                llm_raw_response, hook_output, llm_input
             ) VALUES (?1, 'test', 'test-model',
                0, 0, 0, 0, 0, 0, 10,
                ?2, 3, 'ok', NULL,
                '', 'exclusive', 'unit prompt', '/tmp', 0,
                '', '', '')",
            rusqlite::params![ts, chosen_json],
        )
        .expect("insert router_event");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http_get(url: &str) -> (u16, String) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client.get(url).send().expect("http get");
    let status = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    (status, body)
}

// ───────────────────────── /api/skills ─────────────────────────

#[test]
fn skills_sorted_by_llm_score_then_name() {
    let srv = Server::spawn();
    srv.plant_skill("skill-a", "alpha");
    srv.plant_skill("skill-b", "bravo");
    srv.plant_skill("skill-c", "charlie");
    srv.set_llm_score("skill-a", 5);
    srv.set_llm_score("skill-b", 10);
    srv.set_llm_score("skill-c", 5);

    let (status, body) = http_get(&srv.url("/api/skills"));
    assert_eq!(status, 200, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).expect("parse json");
    let skills = v["skills"].as_array().expect("skills is array");
    let names: Vec<&str> = skills.iter().map(|s| s["name"].as_str().unwrap()).collect();
    // Plan: first is skill-b (10), then skill-a and skill-c (5, sorted by
    // name asc).
    assert_eq!(
        names,
        vec!["skill-b", "skill-a", "skill-c"],
        "expected score-desc then name-asc"
    );
    assert_eq!(v["total"].as_u64().unwrap(), 3);
}

#[test]
fn skills_lists_planted_skills_with_metadata() {
    // Cold-server compat carve-out: cloud HEAD does not implement
    // auth on /api/skills, so the plan's "no users / no events ⇒ empty
    // response + 200" reduces to "200 + empty when nothing planted".
    let srv = Server::spawn();
    let (status, body) = http_get(&srv.url("/api/skills"));
    assert_eq!(status, 200, "cold body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).expect("parse json");
    assert_eq!(v["total"].as_u64().unwrap(), 0, "cold = empty skills");
    assert_eq!(v["enriched"].as_u64().unwrap(), 0);
    assert!(v["skills"].as_array().unwrap().is_empty());

    // Now plant one skill and re-query — server must surface it.
    srv.plant_skill("planted", "desc-here");
    let (status, body) = http_get(&srv.url("/api/skills"));
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["total"].as_u64().unwrap(), 1);
    let names: Vec<&str> = v["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["planted"]);
}

