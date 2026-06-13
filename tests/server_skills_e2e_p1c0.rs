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

// ──────────────────── /api/skill/{name} ────────────────────────

#[test]
fn skill_detail_returns_skill_md_content() {
    let srv = Server::spawn();
    let body_text = "x".repeat(10_000);
    let md = format!(
        "---\nname: test-skill\ndescription: small\n---\n\n# test-skill\n\n{body_text}\n"
    );
    let md_len = md.len();
    srv.plant_skill_with_md_body("test-skill", &md);

    let (status, body) = http_get(&srv.url("/api/skill/test-skill"));
    assert_eq!(status, 200, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        !v["skill_md_content"].as_str().unwrap().is_empty(),
        "expected SKILL.md content"
    );
    assert_eq!(v["skill_md_truncated"].as_bool().unwrap(), false);
    assert_eq!(
        v["skill_md_size"].as_u64().unwrap(),
        md_len as u64,
        "skill_md_size = exact bytes"
    );
}

#[test]
fn skill_detail_truncates_large_skill_md() {
    let srv = Server::spawn();
    // 100KB body of ascii so byte len == char count and the > 60_000
    // truncation branch is provably hit.
    let big_body = "a".repeat(100_000);
    let md = format!("---\nname: big\ndescription: big skill\n---\n\n{big_body}\n");
    let md_len = md.len();
    assert!(md_len > 60_000);
    srv.plant_skill_with_md_body("big", &md);

    let (status, body) = http_get(&srv.url("/api/skill/big"));
    assert_eq!(status, 200, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["skill_md_truncated"].as_bool().unwrap(), true);
    let content = v["skill_md_content"].as_str().unwrap();
    // Server cuts at 60_000 chars; ascii ⇒ len == char count.
    assert!(
        content.len() <= 60_000,
        "truncated content must be <= 60_000 bytes, got {}",
        content.len()
    );
    assert_eq!(
        v["skill_md_size"].as_u64().unwrap(),
        md_len as u64,
        "original (pre-truncation) byte count surfaces in skill_md_size"
    );
}

#[test]
fn skill_detail_includes_related_events() {
    let srv = Server::spawn();
    srv.plant_skill("hot", "hot skill");

    // Insert 3 router_events where 'hot' was in chosen_skills_json,
    // ascending ts so we can verify "newest first" ordering.
    srv.insert_router_event(1_000, &["hot", "other"]);
    srv.insert_router_event(2_000, &["hot"]);
    srv.insert_router_event(3_000, &["hot", "another"]);

    let (status, body) = http_get(&srv.url("/api/skill/hot"));
    assert_eq!(status, 200, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let events = v["events"].as_array().expect("events array");
    assert_eq!(v["events_total"].as_u64().unwrap(), 3);
    assert_eq!(events.len(), 3);

    // Each event's chosen_skills must include 'hot'.
    for ev in events {
        // RouterEvent serialises chosen_skills_json into the "chosen"
        // field as a Vec<String> (see EventJson::from in src/server.rs:335).
        let chosen = ev["chosen"].as_array().expect("chosen array");
        let chosen_names: Vec<&str> = chosen.iter().map(|s| s.as_str().unwrap()).collect();
        assert!(
            chosen_names.contains(&"hot"),
            "event {ev:?} should contain 'hot'"
        );
    }

    // Newest-first ordering: ts descending.
    let ts_values: Vec<i64> = events
        .iter()
        .map(|e| e["ts"].as_i64().expect("ts is i64"))
        .collect();
    let mut sorted = ts_values.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(ts_values, sorted, "events must be ts-descending");
}

#[test]
fn skill_detail_not_found() {
    let srv = Server::spawn();
    srv.plant_skill("exists", "real one");

    let (status, _body) = http_get(&srv.url("/api/skill/nonexistent"));
    assert_eq!(status, 404, "missing skill ⇒ 404");
}

// ─────────────────── /api/skill/{name}/files ───────────────────

#[test]
fn skill_files_recursive_listing() {
    let srv = Server::spawn();
    let dir = srv.skills_dir().join("multi");
    std::fs::create_dir_all(dir.join("scripts/subdir")).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: multi\ndescription: x\n---\n\n# multi\n",
    )
    .unwrap();
    std::fs::write(dir.join("scripts/foo.sh"), "echo hi\n").unwrap();
    std::fs::write(dir.join("scripts/subdir/bar.py"), "print('hi')\n").unwrap();
    srv.run_scan();

    let (status, body) = http_get(&srv.url("/api/skill/multi/files"));
    assert_eq!(status, 200, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let entries = v["entries"].as_array().expect("entries");
    let paths: Vec<&str> = entries
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(entries.len(), 3, "expected 3 files, got {paths:?}");
    // Alphabetical sort by path, forward slashes only.
    assert_eq!(
        paths,
        vec!["SKILL.md", "scripts/foo.sh", "scripts/subdir/bar.py"],
        "paths must be alphabetical, forward-slash"
    );
    // Each entry has shape {path, size, is_text}.
    for e in entries {
        assert!(e["path"].is_string());
        assert!(e["size"].is_u64() || e["size"].is_number());
        assert!(e["is_text"].is_boolean());
    }
}

#[test]
fn skill_files_text_detection() {
    let srv = Server::spawn();
    let dir = srv.skills_dir().join("mixed");
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: mixed\ndescription: x\n---\n",
    )
    .unwrap();
    std::fs::write(dir.join("scripts/foo.sh"), "#!/bin/sh\necho hi\n").unwrap();
    // Real PNG magic bytes for the binary case (extension-based is_text
    // checker uses the suffix not the contents, but writing real PNG
    // bytes guards against any future content-sniffing branch).
    let png_magic = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
    std::fs::write(dir.join("logo.png"), png_magic).unwrap();
    std::fs::write(dir.join("config.json"), "{\"a\":1}\n").unwrap();
    srv.run_scan();

    let (status, body) = http_get(&srv.url("/api/skill/mixed/files"));
    assert_eq!(status, 200, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mut is_text: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    for e in v["entries"].as_array().unwrap() {
        is_text.insert(
            e["path"].as_str().unwrap().to_string(),
            e["is_text"].as_bool().unwrap(),
        );
    }
    assert_eq!(is_text.get("SKILL.md"), Some(&true), "md is text");
    assert_eq!(
        is_text.get("scripts/foo.sh"),
        Some(&true),
        ".sh is text"
    );
    assert_eq!(is_text.get("logo.png"), Some(&false), ".png is binary");
    assert_eq!(is_text.get("config.json"), Some(&true), ".json is text");
}

#[test]
fn skill_files_excludes_hidden_files() {
    let srv = Server::spawn();
    let dir = srv.skills_dir().join("hidden");
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: hidden\ndescription: x\n---\n",
    )
    .unwrap();
    std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
    std::fs::write(dir.join(".env"), "SECRET=hunter2\n").unwrap();
    std::fs::write(dir.join("scripts/code.sh"), "#!/bin/sh\n").unwrap();
    srv.run_scan();

    let (status, body) = http_get(&srv.url("/api/skill/hidden/files"));
    assert_eq!(status, 200, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let paths: Vec<&str> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        vec!["SKILL.md", "scripts/code.sh"],
        "hidden dot-files must be excluded"
    );
    for p in &paths {
        assert!(
            !p.split('/').any(|seg| seg.starts_with('.')),
            "no dot-prefix segment in {p}"
        );
    }
}

#[test]
fn skill_files_skill_not_found() {
    let srv = Server::spawn();
    let (status, _body) = http_get(&srv.url("/api/skill/nonexistent/files"));
    assert_eq!(status, 404);
}

// ─────────────────── /api/skill/{name}/file ────────────────────

#[test]
fn skill_file_returns_content_for_valid_path() {
    let srv = Server::spawn();
    let dir = srv.skills_dir().join("docs");
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    let body_2k = "x".repeat(2048);
    let md = format!("---\nname: docs\ndescription: x\n---\n\n{body_2k}");
    let md_len = md.len();
    std::fs::write(dir.join("SKILL.md"), &md).unwrap();
    std::fs::write(dir.join("scripts/setup.sh"), "#!/bin/sh\necho hi\n").unwrap();
    srv.run_scan();

    let (status, body) = http_get(&srv.url("/api/skill/docs/file?path=SKILL.md"));
    assert_eq!(status, 200, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["path"].as_str().unwrap(), "SKILL.md");
    assert_eq!(v["is_text"].as_bool().unwrap(), true);
    assert_eq!(v["truncated"].as_bool().unwrap(), false);
    assert_eq!(v["size"].as_u64().unwrap(), md_len as u64);
    let content = v["content"].as_str().unwrap();
    assert_eq!(content.len(), md_len, "full file content returned");
}

#[test]
fn skill_file_truncates_large_files() {
    let srv = Server::spawn();
    let dir = srv.skills_dir().join("big");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: big\ndescription: x\n---\n",
    )
    .unwrap();
    // 100KB pure-ascii so byte count == char count.
    let big = "a".repeat(100_000);
    std::fs::write(dir.join("huge.md"), &big).unwrap();
    srv.run_scan();

    let (status, body) = http_get(&srv.url("/api/skill/big/file?path=huge.md"));
    assert_eq!(status, 200, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["truncated"].as_bool().unwrap(), true);
    // Server caps at 80_000 chars; ascii so byte count == char count.
    let content = v["content"].as_str().unwrap();
    assert!(
        content.len() <= 80_000,
        "truncated to <= 80_000 bytes, got {}",
        content.len()
    );
    assert_eq!(
        v["size"].as_u64().unwrap(),
        100_000,
        "size = pre-truncation byte count"
    );
}

#[test]
fn skill_file_path_traversal_prevention() {
    let srv = Server::spawn();
    let dir = srv.skills_dir().join("safe");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: safe\ndescription: x\n---\n",
    )
    .unwrap();
    // Plant a sibling file outside the skill dir that should NOT be
    // reachable via ../ traversal.
    std::fs::write(srv.home().join("secret.txt"), "do-not-leak").unwrap();
    srv.run_scan();

    let traversals = ["../secret.txt", "../../etc/passwd", "../../.bash_history"];
    for t in traversals {
        let url = format!(
            "/api/skill/safe/file?path={}",
            urlencoded(t.as_bytes())
        );
        let (status, _body) = http_get(&srv.url(&url));
        assert_eq!(
            status, 404,
            "traversal attempt {t} must return 404, got {status}"
        );
    }
}

#[test]
fn skill_file_handles_binary_files() {
    let srv = Server::spawn();
    let dir = srv.skills_dir().join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: bin\ndescription: x\n---\n\n# bin\n",
    )
    .unwrap();
    let png_magic = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01";
    std::fs::write(dir.join("logo.png"), png_magic).unwrap();
    srv.run_scan();

    // Binary: is_text=false, content="" (no binary bytes leaked into JSON).
    let (status, body) = http_get(&srv.url("/api/skill/bin/file?path=logo.png"));
    assert_eq!(status, 200, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["is_text"].as_bool().unwrap(), false);
    assert_eq!(v["content"].as_str().unwrap(), "");
    assert_eq!(v["size"].as_u64().unwrap(), png_magic.len() as u64);
    assert_eq!(v["path"].as_str().unwrap(), "logo.png");

    // Text counterpart returns full content.
    let (status, body) = http_get(&srv.url("/api/skill/bin/file?path=SKILL.md"));
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["is_text"].as_bool().unwrap(), true);
    assert!(!v["content"].as_str().unwrap().is_empty());
}

#[test]
fn skill_file_nonexistent_file_404() {
    let srv = Server::spawn();
    let dir = srv.skills_dir().join("exists");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: exists\ndescription: x\n---\n",
    )
    .unwrap();
    srv.run_scan();

    let (status, _body) = http_get(&srv.url("/api/skill/exists/file?path=nonexistent.txt"));
    assert_eq!(status, 404);
}

// Minimal URL-encoder for path query values. Only encodes the byte set
// reqwest's blocking client refuses to pass through (specifically `/`
// and `?` would change semantic meaning — but actually for path
// traversal probes we want the literal `../` to reach the handler so
// canonicalize can prove it escaped the skill dir; URL-encoding it
// would just hide the test from itself). So we only percent-encode
// space and other obvious shell-unsafe bytes; the test inputs are pure
// ascii path strings with `..` and `/` and dots, all safe in a URL
// query value.
fn urlencoded(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
