//! PLANNING §1.6 Model B C6a — admin userlib browse endpoints.
//!
//! Pins the contract for:
//!   - GET /api/admin/userlib           (list non-admin users + per-user
//!                                       private_count / imported_count /
//!                                       last_active_ts, sorted last_active DESC)
//!   - GET /api/admin/userlib/{uid}     (one user's private skills +
//!                                       imported public skills, with usage)
//!
//! Auth contract:
//!   - 401 with no credential
//!   - 403 for non-admin
//!   - 200 for admin
//!   - 404 for unknown user_id on detail

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
fn register(s: &ServerGuard, u: &str, p: &str) -> (String, String) {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let b: Value = r.json().unwrap();
    (
        b["user_id"].as_str().unwrap().into(),
        b["api_key"].as_str().unwrap().into(),
    )
}

// ─── /api/admin/userlib (list) ───────────────────────────────────────

#[test]
fn admin_userlib_list_requires_auth() {
    let s = spawn_team_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let r = http()
        .get(format!("{}/api/admin/userlib", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn admin_userlib_list_non_admin_forbidden() {
    let s = spawn_team_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let (_bob_uid, bob_key) = register(&s, "bob", "pw bob 1234");
    let r = http()
        .get(format!("{}/api/admin/userlib", s.base_url()))
        .bearer_auth(&bob_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}

#[test]
fn admin_userlib_list_filters_out_admin_users() {
    let s = spawn_team_server();
    let (_admin_uid, admin_key) = register(&s, "admin", "pw admin 1234");
    let _ = register(&s, "alice", "pw alice 1234");
    let _ = register(&s, "bob", "pw bob 1234");
    let r = http()
        .get(format!("{}/api/admin/userlib", s.base_url()))
        .bearer_auth(&admin_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert!(b["items"].is_array());
    let items = b["items"].as_array().unwrap();
    // admin (the first registered, auto-promoted to admin) MUST NOT appear.
    let usernames: Vec<&str> = items
        .iter()
        .map(|i| i["username"].as_str().unwrap())
        .collect();
    assert!(
        !usernames.contains(&"admin"),
        "admin should be filtered out of userlib, got {usernames:?}"
    );
    assert!(usernames.contains(&"alice"));
    assert!(usernames.contains(&"bob"));
    // total reflects non-admin count, not raw users.len()
    assert_eq!(b["total"].as_u64().unwrap(), 2);
}

#[test]
fn admin_userlib_list_row_shape() {
    let s = spawn_team_server();
    let (_admin_uid, admin_key) = register(&s, "admin", "pw admin 1234");
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/admin/userlib", s.base_url()))
        .bearer_auth(&admin_key)
        .send()
        .unwrap();
    let b: Value = r.json().unwrap();
    let alice = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["username"] == "alice")
        .expect("alice row");
    // contract: every row carries these fields
    assert!(alice["user_id"].as_str().is_some());
    assert_eq!(alice["username"].as_str().unwrap(), "alice");
    assert!(alice["disabled"].is_boolean());
    assert!(alice["created_at"].is_number());
    assert!(alice["private_count"].is_number());
    assert!(alice["imported_count"].is_number());
    // alice was pre-filled with top_public_skills(30) at register time,
    // but the test HOME has zero public skills installed, so imported is 0.
    assert_eq!(alice["imported_count"].as_u64().unwrap(), 0);
    assert_eq!(alice["private_count"].as_u64().unwrap(), 0);
    // last_active_ts: 0 means "never sent a router event"
    assert!(alice["last_active_ts"].is_number());
    assert_eq!(alice["last_active_ts"].as_i64().unwrap(), 0);
}

// ─── /api/admin/userlib/{uid} (detail) ───────────────────────────────

#[test]
fn admin_userlib_detail_requires_auth() {
    let s = spawn_team_server();
    let (alice_uid, _) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/admin/userlib/{alice_uid}", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn admin_userlib_detail_non_admin_forbidden() {
    let s = spawn_team_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let (bob_uid, bob_key) = register(&s, "bob", "pw bob 1234");
    let r = http()
        .get(format!("{}/api/admin/userlib/{bob_uid}", s.base_url()))
        .bearer_auth(&bob_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}

#[test]
fn admin_userlib_detail_404_for_unknown_user() {
    let s = spawn_team_server();
    let (_admin_uid, admin_key) = register(&s, "admin", "pw admin 1234");
    let r = http()
        .get(format!(
            "{}/api/admin/userlib/usr_nonexistent",
            s.base_url()
        ))
        .bearer_auth(&admin_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
}

#[test]
fn admin_userlib_detail_returns_user_and_lists() {
    let s = spawn_team_server();
    let (_admin_uid, admin_key) = register(&s, "admin", "pw admin 1234");
    let (alice_uid, _alice_key) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/admin/userlib/{alice_uid}", s.base_url()))
        .bearer_auth(&admin_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert_eq!(b["user_id"].as_str().unwrap(), alice_uid);
    assert_eq!(b["username"].as_str().unwrap(), "alice");
    // private + imported are arrays (empty for a fresh HOME)
    assert!(b["private"].is_array());
    assert!(b["imported"].is_array());
    assert_eq!(b["private"].as_array().unwrap().len(), 0);
    assert_eq!(b["imported"].as_array().unwrap().len(), 0);
    // headline stats
    assert!(b["recent_events_count"].is_number());
    assert!(b["last_active_ts"].is_number());
}
