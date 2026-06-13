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

// ─── Feature 2: POST /skills/get/:name ──────────────────────────────────────

/// Happy path: a planted+scanned skill is reachable, SKILL.md flows
/// out verbatim, sibling files surface in the curl appendix,
/// usage_count is bumped, and a session_adoptions row appears under
/// the bare session_id (no user header here).
#[test]
fn skill_get_returns_md_appendix_and_records_adoption() {
    let env = ServerEnv::spawn();
    env.plant_skill_and_scan("demo", "test skill demo");
    // Plant a sibling file so the appendix has something to surface.
    let sibling_dir = env.managed_skills_dir().join("demo").join("references");
    std::fs::create_dir_all(&sibling_dir).unwrap();
    std::fs::write(sibling_dir.join("ref-a.md"), "auxiliary reference").unwrap();
    assert_eq!(env.usage_count("demo"), 0, "precondition: usage starts 0");

    let url = format!(
        "{}/skills/get/demo?session_id=sess-A",
        env.base_url()
    );
    let (status, body, ct) = http_post_json(&url, &serde_json::json!({}), &[]);

    assert_eq!(
        status.as_u16(),
        200,
        "skill get must return 200 for an existing skill, got {status}: {body}"
    );
    assert!(
        ct.as_deref().unwrap_or("").starts_with("text/plain"),
        "content-type must be text/plain, got {ct:?}"
    );
    assert!(
        body.contains("name: demo"),
        "body must contain SKILL.md frontmatter verbatim, got:\n{body}"
    );
    assert!(
        body.contains("test skill demo"),
        "body must contain SKILL.md description text, got:\n{body}"
    );
    // Appendix lists sibling files as curl commands.
    assert!(
        body.contains("references/ref-a.md"),
        "appendix must list the planted sibling file, got:\n{body}"
    );
    assert!(
        body.contains("curl -s"),
        "appendix must include curl invocations, got:\n{body}"
    );
    // DB side-effects.
    assert_eq!(
        env.usage_count("demo"),
        1,
        "usage_count must increment by 1 after one /skills/get"
    );
    assert!(
        env.has_session_adoption("sess-A", "demo"),
        "session_adoptions row must be written for the bare session_id when no user header"
    );
}

/// X-Runai-User + session_id ⇒ session id = `{user}:{session}` so
/// concurrent teammates don't collide in the per-session router
/// memory. Lock that wiring on both the DB row and the appendix
/// (each curl line must carry the same user header back).
#[test]
fn skill_get_with_user_header_prefixes_session_id() {
    let env = ServerEnv::spawn();
    env.plant_skill_and_scan("test-skill", "user-prefixed get target");

    let url = format!(
        "{}/skills/get/test-skill?session_id=local-id",
        env.base_url()
    );
    let (status, body, _ct) =
        http_post_json(&url, &serde_json::json!({}), &[("X-Runai-User", "alice@host")]);

    assert_eq!(status.as_u16(), 200, "request must succeed");
    // The appendix-empty case still works (no siblings planted) and
    // the file response itself does not embed user_header_arg, so we
    // only assert it when the appendix exists. Either way the DB
    // session_id must be the prefixed form.
    assert!(
        env.has_session_adoption("alice@host:local-id", "test-skill"),
        "session_adoptions must be keyed by `{{user}}:{{session}}`, body:\n{body}"
    );
    assert!(
        !env.has_session_adoption("local-id", "test-skill"),
        "the bare session_id must NOT be written when a user header is present"
    );
}

/// X-Runai-User with no session_id ⇒ session id = user_prefix alone.
/// Also exercises the appendix's user_header_arg interpolation when
/// sibling files are present, since the empty-session case used to
/// trip an "alice@host:" key by accident.
#[test]
fn skill_get_appendix_propagates_user_header_into_curl_lines() {
    let env = ServerEnv::spawn();
    env.plant_skill_and_scan("with-refs", "skill with siblings");
    let dir = env.managed_skills_dir().join("with-refs");
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("scripts").join("run.py"), "print('hi')\n").unwrap();

    let url = format!("{}/skills/get/with-refs", env.base_url());
    let (status, body, _ct) =
        http_post_json(&url, &serde_json::json!({}), &[("X-Runai-User", "bob@host")]);

    assert_eq!(status.as_u16(), 200, "request must succeed");
    assert!(
        body.contains("scripts/run.py"),
        "appendix must list the planted sibling, body:\n{body}"
    );
    assert!(
        body.contains("X-Runai-User: bob@host"),
        "appendix curl lines must propagate the user header, body:\n{body}"
    );
    assert!(
        env.has_session_adoption("bob@host", "with-refs"),
        "session_id must collapse to the bare user prefix when no claude session id is given"
    );
}

/// Nonexistent skill ⇒ 404 + no side-effects. The handler reports the
/// error via stderr and surfaces a 404 with a `skill not found:`
/// human-readable body — both shape and absence of DB writes are
/// covered.
#[test]
fn skill_get_returns_404_for_missing_skill() {
    let env = ServerEnv::spawn();
    env.plant_skill_and_scan("present", "this one exists");
    let pre_usage = env.usage_count("present");

    let url = format!("{}/skills/get/nonexistent?session_id=sess-X", env.base_url());
    let (status, body, _ct) = http_post_json(&url, &serde_json::json!({}), &[]);

    assert_eq!(
        status.as_u16(),
        404,
        "missing skill must return 404, got {status}: {body}"
    );
    assert!(
        body.to_lowercase().contains("skill not found"),
        "404 body must include a 'skill not found' hint, got: {body:?}"
    );
    // No collateral damage on a sibling skill or on phantom session
    // rows.
    assert_eq!(
        env.usage_count("present"),
        pre_usage,
        "an unrelated skill's usage_count must not move"
    );
    assert_eq!(
        env.usage_count("nonexistent"),
        0,
        "the missing name must never produce a resources row"
    );
    assert!(
        !env.has_session_adoption("sess-X", "nonexistent"),
        "no session_adoptions row must be written for a missing skill"
    );
}
