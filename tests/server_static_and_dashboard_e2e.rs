//! Real e2e for the dashboard's static + telemetry endpoints (issue #19):
//!   GET /            (HTML, cache-bust query strings injected)
//!   GET /app.css     (text/css)
//!   GET /app.js      (text/javascript)
//!   GET /api/summary (router stats summary, auth-gated when users exist)
//!   GET /api/timeline (per-hour buckets)
//!   GET /api/events  (router event list)
//!   GET /api/event/:id (event detail, 404 unknown, scoped to caller)

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use runai::core::db::RouterEvent;
use runai::core::manager::SkillManager;
use serde_json::{Value, json};
use tempfile::TempDir;

fn runai_cmd() -> Command {
    Command::cargo_bin("runai").expect("runai binary")
}
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}
fn wait_for_port(port: u16, t: Duration) -> bool {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let d = Instant::now() + t;
    while Instant::now() < d {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}
struct ServerGuard {
    child: Child,
    home: TempDir,
    port: u16,
}
impl ServerGuard {
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn data_dir(&self) -> std::path::PathBuf {
        self.home.path().join(".runai")
    }
}
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn spawn_team_server() -> ServerGuard {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    let port = free_port();
    let child = runai_cmd()
        .arg("server")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--mode")
        .arg("team")
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let g = ServerGuard { child, home, port };
    assert!(wait_for_port(port, Duration::from_secs(8)));
    g
}
fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
fn register(s: &ServerGuard, u: &str, p: &str) -> String {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let b: Value = r.json().unwrap();
    b["api_key"].as_str().unwrap().to_string()
}

fn register_with_uid(s: &ServerGuard, u: &str, p: &str) -> (String, String) {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let b: Value = r.json().unwrap();
    (
        b["api_key"].as_str().unwrap().to_string(),
        b["user_id"].as_str().unwrap().to_string(),
    )
}

fn seed_router_event(s: &ServerGuard, user_id: Option<&str>) -> i64 {
    let mgr = SkillManager::with_base(s.data_dir()).expect("manager");
    let ev = RouterEvent {
        id: None,
        ts: 123,
        provider: "test-provider".into(),
        model: "test-model".into(),
        prompt_tokens: 1,
        completion_tokens: 2,
        reasoning_tokens: 0,
        total_tokens: 3,
        cache_hit_tokens: 0,
        cache_miss_tokens: 1,
        latency_ms: 10,
        chosen_skills_json: r#"["alpha"]"#.into(),
        candidate_count: 2,
        status: "ok".into(),
        error_msg: None,
        session_id: "seed-session".into(),
        mode: "exclusive".into(),
        user_prompt: "seed prompt".into(),
        cwd: "/tmp".into(),
        bm25_kept: 1,
        llm_raw_response: "EXCLUSIVE\nreasoning: seed\nalpha\n".into(),
        hook_output: "seed hook".into(),
        llm_input: "seed llm input".into(),
        user_id: user_id.map(str::to_string),
    };
    mgr.db().insert_router_event(&ev).unwrap();
    mgr.db()
        .router_events_paged_filtered(None, 1, 0, None, false, user_id)
        .unwrap()[0]
        .id
        .unwrap()
}

// ─── GET / ────────────────────────────────────────────────────────────

#[test]
fn root_serves_html_with_cachebust() {
    let s = spawn_team_server();
    let r = http().get(format!("{}/", s.base_url())).send().unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let ct = r.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.starts_with("text/html"), "got {ct}");
    let body = r.text().unwrap();
    // Cache-busting: /app.css and /app.js should carry ?v=<build_id>
    assert!(
        body.contains("/app.css?v="),
        "missing cache-bust on app.css"
    );
    assert!(body.contains("/app.js?v="), "missing cache-bust on app.js");
    // doctype
    assert!(body.to_ascii_lowercase().contains("<!doctype html"));
}

#[test]
fn root_is_unauthenticated() {
    // Static HTML must be reachable without any credential.
    let s = spawn_team_server();
    let r = http().get(format!("{}/", s.base_url())).send().unwrap();
    assert_eq!(r.status().as_u16(), 200);
}

// ─── GET /app.css ────────────────────────────────────────────────────

#[test]
fn app_css_serves_text_css() {
    let s = spawn_team_server();
    let r = http()
        .get(format!("{}/app.css", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let ct = r.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.starts_with("text/css"), "got {ct}");
    assert!(
        !r.text().unwrap().is_empty(),
        "css body should not be empty"
    );
}

// ─── GET /app.js ─────────────────────────────────────────────────────

#[test]
fn app_js_serves_javascript() {
    let s = spawn_team_server();
    let r = http()
        .get(format!("{}/app.js", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let ct = r.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(
        ct.contains("javascript"),
        "expected js content-type, got {ct}"
    );
    assert!(!r.text().unwrap().is_empty());
}

// ─── GET /api/summary ─────────────────────────────────────────────────

#[test]
fn api_summary_requires_auth_once_users_exist() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/summary", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn api_summary_returns_shape_for_admin() {
    let s = spawn_team_server();
    let key = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/summary", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    // SummaryResp has at least totals (zero on fresh db)
    assert!(b.is_object());
}

// ─── GET /api/timeline ────────────────────────────────────────────────

#[test]
fn api_timeline_requires_auth() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/timeline", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn api_timeline_returns_buckets_for_admin() {
    let s = spawn_team_server();
    let key = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/timeline", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    // shape: object with at least a buckets array (empty on fresh db)
    assert!(b.is_object() || b.is_array());
}

// ─── GET /api/events ──────────────────────────────────────────────────

#[test]
fn api_events_requires_auth() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/events", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn api_events_empty_for_fresh_server() {
    let s = spawn_team_server();
    let key = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/events", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    // Either array or object with items[], both should be empty on fresh server
    let count = if b.is_array() {
        b.as_array().unwrap().len()
    } else {
        b.get("items")
            .and_then(|x| x.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    };
    assert_eq!(count, 0);
}

#[test]
fn api_events_exposes_authenticated_boolean_without_user_id() {
    let s = spawn_team_server();
    let (key, uid) = register_with_uid(&s, "alice", "pw alice 1234");
    seed_router_event(&s, Some(&uid));

    let r = http()
        .get(format!("{}/api/events", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    let events = b["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["authenticated"], json!(true));
    assert!(
        events[0].get("user_id").is_none(),
        "telemetry DTO must not expose raw user_id: {:?}",
        events[0]
    );
}

// ─── GET /api/event/:id ───────────────────────────────────────────────

#[test]
fn api_event_by_id_404_for_unknown() {
    let s = spawn_team_server();
    let key = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/event/999999", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
}

#[test]
fn api_event_by_id_requires_auth() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/event/1", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

// (no runai-dashboard signature header in this HEAD; ensure_running uses
// other heuristics — skip that test.)
