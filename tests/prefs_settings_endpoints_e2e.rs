//! Real e2e for settings / providers HTTP CRUD (issue #19 still_open):
//!   GET    /api/settings
//!   POST   /api/settings
//!   POST   /api/providers
//!   DELETE /api/providers/:id
//!   POST   /api/providers/:id/activate

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
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
    let deadline = Instant::now() + t;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

struct ServerGuard {
    child: Child,
    _home: TempDir,
    port: u16,
}
impl ServerGuard {
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
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
    let g = ServerGuard {
        child,
        _home: home,
        port,
    };
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

/// Seed the server's DB with a single router_event row without ever
/// registering a user — reproduces "team server with users deleted/never
/// created but historical telemetry present" (issue #32). Goes through the
/// public Rust API so the test stays valid across schema migrations.
fn seed_one_router_event(s: &ServerGuard) {
    use runai::core::db::Database;
    use runai::core::db::RouterEvent;
    let db_path = s._home.path().join(".runai/runai.db");
    let db = Database::open(&db_path).expect("open db");
    let ev = RouterEvent {
        id: None,
        ts: chrono::Utc::now().timestamp(),
        provider: "openai-compat".into(),
        model: "fake/model".into(),
        prompt_tokens: 1,
        completion_tokens: 1,
        reasoning_tokens: 0,
        total_tokens: 2,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        latency_ms: 1,
        chosen_skills_json: "[]".into(),
        candidate_count: 0,
        status: "ok".into(),
        error_msg: None,
        session_id: "synthetic".into(),
        mode: "exclusive".into(),
        user_prompt: "synthetic user prompt".into(),
        cwd: "/tmp".into(),
        bm25_kept: 0,
        llm_raw_response: "synthetic raw response".into(),
        hook_output: "synthetic hook output".into(),
        llm_input: "synthetic llm input".into(),
        user_id: None,
    };
    db.insert_router_event(&ev).expect("seed router event");
}

// ─── issue #32: cold-start compat carve-out must match private_data_locked ─

#[test]
fn settings_get_anonymous_rejected_once_router_events_exist_but_users_empty() {
    let s = spawn_team_server();
    seed_one_router_event(&s);
    let r = http()
        .get(format!("{}/api/settings", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        401,
        "anonymous GET /api/settings must 401 once router_events is non-empty, even with users table empty"
    );
}

#[test]
fn settings_post_anonymous_rejected_once_router_events_exist_but_users_empty() {
    let s = spawn_team_server();
    seed_one_router_event(&s);
    let r = http()
        .post(format!("{}/api/settings", s.base_url()))
        .json(&json!({"enabled": false}))
        .send()
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        401,
        "anonymous POST /api/settings must 401 once router_events is non-empty, even with users table empty"
    );
}

#[test]
fn providers_upsert_anonymous_rejected_once_router_events_exist_but_users_empty() {
    let s = spawn_team_server();
    seed_one_router_event(&s);
    let r = http()
        .post(format!("{}/api/providers", s.base_url()))
        .json(&json!({
            "id": "leak-probe",
            "label": "leak probe",
            "kind": "openai-compat",
            "base_url": "https://api.example.com",
            "model": "test-model",
            "api_key": "sk-should-not-be-writable"
        }))
        .send()
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        401,
        "anonymous POST /api/providers must 401 once router_events is non-empty, even with users table empty"
    );
}

#[test]
fn settings_get_anonymous_still_open_on_truly_cold_server() {
    // No users, no router_events — the legacy first-run compat window must
    // stay open so the dashboard setup wizard keeps working on a genuinely
    // fresh deployment. This pins the non-regression side of issue #32.
    let s = spawn_team_server();
    let r = http()
        .get(format!("{}/api/settings", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        200,
        "anonymous GET /api/settings must stay open on a truly cold server (no users, no events)"
    );
}

// ─── /api/settings ─────────────────────────────────────────────────────

#[test]
fn settings_get_admin_returns_view() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let r = http()
        .get(format!("{}/api/settings", s.base_url()))
        .bearer_auth(&akey)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    // SettingsView shape — basic fields present
    assert!(b.get("enabled").is_some());
    assert!(b.get("top_k").is_some());
    assert!(b["providers"].is_array());
}

#[test]
fn settings_get_non_admin_forbidden() {
    let s = spawn_team_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    let r = http()
        .get(format!("{}/api/settings", s.base_url()))
        .bearer_auth(&bob)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}

#[test]
fn settings_post_persists_enabled_toggle() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!("{}/api/settings", s.base_url()))
        .bearer_auth(&akey)
        .json(&json!({"enabled": false}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert!(!b["enabled"].as_bool().unwrap());
    // Read back
    let g: Value = http()
        .get(format!("{}/api/settings", s.base_url()))
        .bearer_auth(&akey)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert!(!g["enabled"].as_bool().unwrap());
}

#[test]
fn settings_post_updates_top_k_and_session_history_limit() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!("{}/api/settings", s.base_url()))
        .bearer_auth(&akey)
        .json(&json!({"top_k": 10, "session_history_limit": 25}))
        .send()
        .unwrap();
    let b: Value = r.json().unwrap();
    assert_eq!(b["top_k"].as_u64().unwrap(), 10);
    assert_eq!(b["session_history_limit"].as_u64().unwrap(), 25);
}

#[test]
fn settings_post_non_admin_forbidden() {
    let s = spawn_team_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    let r = http()
        .post(format!("{}/api/settings", s.base_url()))
        .bearer_auth(&bob)
        .json(&json!({"enabled": false}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}

// ─── /api/providers ────────────────────────────────────────────────────

#[test]
fn providers_upsert_creates_new_entry() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!("{}/api/providers", s.base_url()))
        .bearer_auth(&akey)
        .json(&json!({
            "id": "test1",
            "label": "Test Provider 1",
            "kind": "openai-compat",
            "base_url": "https://api.example.com",
            "model": "test-model",
            "api_key": "sk-test-xxx"
        }))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    let providers = b["providers"].as_array().unwrap();
    let new_p = providers
        .iter()
        .find(|p| p["id"] == "test1")
        .expect("test1 provider");
    assert_eq!(new_p["label"].as_str().unwrap(), "Test Provider 1");
    assert!(new_p["has_api_key"].as_bool().unwrap());
    // api_key bytes never returned
    assert!(new_p.get("api_key").is_none());
}

#[test]
fn providers_upsert_preserves_existing_api_key_when_empty_sent() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let _ = http()
        .post(format!("{}/api/providers", s.base_url()))
        .bearer_auth(&akey)
        .json(&json!({"id":"p1","label":"L","kind":"openai-compat","base_url":"u","model":"m","api_key":"original-key"}))
        .send()
        .unwrap();
    // Update with empty api_key → should preserve
    let r = http()
        .post(format!("{}/api/providers", s.base_url()))
        .bearer_auth(&akey)
        .json(&json!({"id":"p1","label":"L2","kind":"openai-compat","base_url":"u","model":"m","api_key":""}))
        .send()
        .unwrap();
    let b: Value = r.json().unwrap();
    let p = b["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "p1")
        .unwrap();
    assert_eq!(p["label"].as_str().unwrap(), "L2");
    assert!(p["has_api_key"].as_bool().unwrap(), "api_key preserved");
}

#[test]
fn providers_upsert_rejects_empty_id() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!("{}/api/providers", s.base_url()))
        .bearer_auth(&akey)
        .json(&json!({"id":"","label":"l","kind":"openai-compat","base_url":"u","model":"m","api_key":"k"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
}

#[test]
fn providers_upsert_rejects_unknown_kind() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!("{}/api/providers", s.base_url()))
        .bearer_auth(&akey)
        .json(&json!({"id":"p","label":"l","kind":"ufo","base_url":"u","model":"m","api_key":"k"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
}

#[test]
fn providers_delete_removes_entry() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let _ = http()
        .post(format!("{}/api/providers", s.base_url()))
        .bearer_auth(&akey)
        .json(&json!({"id":"x","label":"l","kind":"openai-compat","base_url":"u","model":"m","api_key":"k"}))
        .send()
        .unwrap();
    let r = http()
        .delete(format!("{}/api/providers/x", s.base_url()))
        .bearer_auth(&akey)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    let provs = b["providers"].as_array().unwrap();
    assert!(!provs.iter().any(|p| p["id"] == "x"));
}

#[test]
fn providers_delete_404_for_unknown() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let r = http()
        .delete(format!("{}/api/providers/ghost", s.base_url()))
        .bearer_auth(&akey)
        .send()
        .unwrap();
    // BadRequest mapped to 400 in this codebase for "not found" path
    assert!(
        matches!(r.status().as_u16(), 400 | 404),
        "got {}",
        r.status()
    );
}

#[test]
fn providers_activate_marks_active() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let _ = http()
        .post(format!("{}/api/providers", s.base_url()))
        .bearer_auth(&akey)
        .json(&json!({"id":"a","label":"A","kind":"openai-compat","base_url":"u","model":"m","api_key":"k"}))
        .send()
        .unwrap();
    let r = http()
        .post(format!("{}/api/providers/a/activate", s.base_url()))
        .bearer_auth(&akey)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
}

#[test]
fn providers_activate_404_for_unknown() {
    let s = spawn_team_server();
    let akey = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!("{}/api/providers/ghost/activate", s.base_url()))
        .bearer_auth(&akey)
        .send()
        .unwrap();
    assert!(matches!(r.status().as_u16(), 400 | 404));
}

#[test]
fn providers_upsert_non_admin_forbidden() {
    let s = spawn_team_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    let r = http()
        .post(format!("{}/api/providers", s.base_url()))
        .bearer_auth(&bob)
        .json(&json!({"id":"p","label":"l","kind":"openai-compat","base_url":"u","model":"m","api_key":"k"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}
