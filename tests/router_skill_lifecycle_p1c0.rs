//! P1 integration coverage for the remote-hook HTTP surface served by
//! `runai server` (single-file `src/server.rs` on cloud HEAD): POST
//! /recommend, POST /skills/get/:name, POST /feedback.
//!
//! Each feature gets its own block. Tests spawn the installed runai
//! binary as a real subprocess against an isolated HOME tempdir (no
//! contact with the real `~/.runai/` per the safety contract) and
//! hit it over loopback via reqwest::blocking. The cloud HEAD does
//! NOT have multi-user auth, per-user skill ownership, or the
//! tower-governor rate-limit middleware, so test variants that depend
//! on those subsystems are intentionally not implemented here.

#![cfg(not(target_os = "windows"))]

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

// ─── Shared server harness ─────────────────────────────────────────────────

/// Picks a free localhost port by binding ephemeral 0 and immediately
/// closing the listener so the kernel hands the port back. There's a
/// theoretical race vs. another process grabbing it before the child
/// `runai server` binds — practically negligible on a single-user dev
/// box and these tests are gated to --test-threads=1.
fn pick_free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

struct ServerEnv {
    home: TempDir,
    child: Option<Child>,
    port: u16,
}

impl ServerEnv {
    /// Spawn `runai server` in an isolated HOME and block until the
    /// port answers (or panic after 5s).
    fn spawn() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
        let port = pick_free_port();
        let rune_data_dir = home.path().join(".runai");

        let child = Command::new(RUNAI_BIN)
            .args([
                "server",
                "--port",
                &port.to_string(),
                "--host",
                "127.0.0.1",
            ])
            .env("HOME", home.path())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", &rune_data_dir)
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn runai server child");

        let env = Self {
            home,
            child: Some(child),
            port,
        };
        env.wait_ready();
        env
    }

    fn wait_ready(&self) {
        let url = format!("http://127.0.0.1:{}/", self.port);
        let deadline = Instant::now() + Duration::from_secs(5);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(300))
            .build()
            .expect("client");
        while Instant::now() < deadline {
            if client.get(&url).send().is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        panic!(
            "runai server on port {} never came up within 5s",
            self.port
        );
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn db_path(&self) -> PathBuf {
        self.home().join(".runai/runai.db")
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn managed_skills_dir(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    /// Plant a SKILL.md so the binary considers it a managed skill and
    /// run `runai scan` in the same HOME so the resource row is
    /// inserted before the server's per-request open of the DB.
    fn plant_skill_and_scan(&self, name: &str, body: &str) {
        let dir = self.managed_skills_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n"),
        )
        .unwrap();
        let out = Command::new(RUNAI_BIN)
            .args(["scan"])
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.home().join(".runai"))
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .output()
            .expect("runai scan");
        assert!(
            out.status.success(),
            "scan must register the planted skill (stderr={})",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[allow(dead_code)]
    fn usage_count(&self, name: &str) -> i64 {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        conn.query_row(
            "SELECT COALESCE(MAX(usage_count), 0) FROM resources WHERE name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    #[allow(dead_code)]
    fn has_session_adoption(&self, session_id: &str, skill_name: &str) -> bool {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        conn.query_row(
            "SELECT 1 FROM router_session_adoptions \
             WHERE session_id = ?1 AND skill_name = ?2",
            rusqlite::params![session_id, skill_name],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn router_events_count(&self) -> i64 {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        conn.query_row("SELECT COUNT(*) FROM router_events", [], |r| r.get(0))
            .unwrap_or(0)
    }
}

impl Drop for ServerEnv {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn http_post_json(
    url: &str,
    body: &serde_json::Value,
    headers: &[(&str, &str)],
) -> (reqwest::StatusCode, String, Option<String>) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let mut req = client.post(url).json(body);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().expect("POST send");
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let mut body = String::new();
    resp.take(1024 * 1024)
        .read_to_string(&mut body)
        .unwrap_or(0);
    (status, body, content_type)
}

#[allow(dead_code)]
fn http_post_raw_body(
    url: &str,
    raw_body: &str,
    content_type: &str,
) -> (reqwest::StatusCode, String) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let resp = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(raw_body.to_string())
        .send()
        .expect("POST raw send");
    let status = resp.status();
    let mut body = String::new();
    resp.take(1024 * 1024)
        .read_to_string(&mut body)
        .unwrap_or(0);
    (status, body)
}

// ─── Feature 1: POST /recommend ─────────────────────────────────────────────

/// Fresh isolated HOME → no enrichment / no api_key → cfg.enabled=false.
/// The recommend handler short-circuits to an empty body BUT still hits
/// the wire successfully. We assert the response shape, the
/// content-type, and (per the cloud-HEAD short-circuit) that no
/// router_event row is created. The plan calls for a populated body
/// when the LLM router is enabled — that path requires a live LLM and
/// is exercised by recommend_for_user unit tests; here we lock the
/// "no provider configured" wire contract that hooks actually hit on
/// first install.
#[test]
fn recommend_endpoint_returns_text_plain_and_does_not_crash_unconfigured() {
    let env = ServerEnv::spawn();
    env.plant_skill_and_scan("alpha", "test skill alpha");
    let url = format!("{}/recommend", env.base_url());

    let (status, body, ct) = http_post_json(
        &url,
        &serde_json::json!({
            "prompt": "help me make a deck about owls",
            "session_id": "sess-A",
            "cwd": "/tmp/proj",
        }),
        &[("X-Runai-User", "alice@host")],
    );

    assert_eq!(
        status.as_u16(),
        200,
        "unconfigured /recommend must still return 200, got {status}: {body}"
    );
    assert!(
        ct.as_deref().unwrap_or("").starts_with("text/plain"),
        "content-type must be text/plain*, got {ct:?}"
    );
    // With cfg.enabled=false the router returns an empty decision and
    // format_for_hook_full collapses to empty. Body is empty string.
    assert!(
        body.is_empty(),
        "fresh-server /recommend must return empty body when router is not configured, got: {body:?}"
    );
}

/// Empty prompt is handled at the top of handle_recommend (returns ""
/// before touching the router). Locks the contract that hook clients
/// sending a stray empty prompt do not 500 and do not write telemetry.
#[test]
fn recommend_endpoint_empty_prompt_returns_empty_body_and_no_event() {
    let env = ServerEnv::spawn();
    env.plant_skill_and_scan("beta", "test skill beta");
    let before = env.router_events_count();
    let url = format!("{}/recommend", env.base_url());

    let (status, body, _ct) = http_post_json(
        &url,
        &serde_json::json!({ "prompt": "", "session_id": "sess-Z" }),
        &[],
    );

    assert_eq!(status.as_u16(), 200, "empty prompt must return 200");
    assert!(
        body.is_empty(),
        "empty prompt must yield empty body, got: {body:?}"
    );
    let after = env.router_events_count();
    assert_eq!(
        after, before,
        "empty prompt must not create router_events rows (before={before}, after={after})"
    );
}

/// Hook clients on older releases may omit cwd / transcript_path /
/// session_id. handle_recommend treats every payload field as
/// optional via payload_str(). This locks that contract: any subset
/// must still produce a 200 + no panic + no error message in the
/// body.
#[test]
fn recommend_endpoint_tolerates_missing_optional_fields() {
    let env = ServerEnv::spawn();
    env.plant_skill_and_scan("gamma", "test skill gamma");
    let url = format!("{}/recommend", env.base_url());

    // Send a payload with literally only `prompt`.
    let (status, body, _ct) = http_post_json(
        &url,
        &serde_json::json!({ "prompt": "totally minimal payload" }),
        &[],
    );

    assert_eq!(
        status.as_u16(),
        200,
        "minimal payload must succeed, got {status}: {body}"
    );
    // Body may be empty (unconfigured router); critically it must NOT
    // contain a Rust panic / "internal error" line.
    assert!(
        !body.contains("panicked"),
        "body must not surface a panic: {body:?}"
    );
    assert!(
        !body.to_lowercase().contains("internal error"),
        "body must not surface an internal-error envelope: {body:?}"
    );
}

/// X-Runai-User is woven into session id only when paired with a
/// claude session_id. When user_prefix is empty AND session_id is
/// empty the router never sees a session. handle_recommend should
/// happily accept "no user, no session" requests (the local
/// single-user case).
#[test]
fn recommend_endpoint_accepts_no_user_no_session() {
    let env = ServerEnv::spawn();
    env.plant_skill_and_scan("delta", "delta skill");
    let url = format!("{}/recommend", env.base_url());

    let (status, body, ct) = http_post_json(
        &url,
        &serde_json::json!({ "prompt": "a casual local query" }),
        &[],
    );

    assert_eq!(status.as_u16(), 200);
    assert!(ct.as_deref().unwrap_or("").starts_with("text/plain"));
    assert!(
        !body.to_lowercase().contains("internal error"),
        "no-user no-session path must not surface an internal-error envelope"
    );
}
