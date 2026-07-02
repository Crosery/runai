//! Real e2e for the v15 admin endpoints (issue #19 still_open):
//!   GET    /api/admin/users
//!   POST   /api/admin/users/:user_id   (patch is_admin / disabled)
//!   DELETE /api/admin/users/:user_id   (also cascades library_clear)
//!
//! Drives a real `runai server --mode team` in an isolated HOME, registers
//! users via /users/register (first one is auto-promoted to admin), then
//! exercises the admin surface with Bearer auth.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};
use tempfile::TempDir;

// ─── helpers (copied from tests/user_prefs_e2e.rs pattern) ─────────────────

fn runai_cmd() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let deadline = Instant::now() + timeout;
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
    let home = tempfile::tempdir().expect("tmp HOME");
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
        .expect("spawn runai server");
    let g = ServerGuard {
        child,
        _home: home,
        port,
    };
    assert!(
        wait_for_port(port, Duration::from_secs(8)),
        "server did not bind 127.0.0.1:{port} within 8s"
    );
    g
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

/// Register a user. First call on a fresh server → that user auto-becomes admin.
/// Returns (user_id, api_key).
fn register(server: &ServerGuard, username: &str, password: &str) -> (String, String) {
    let resp = http_client()
        .post(format!("{}/users/register", server.base_url()))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .expect("register");
    assert_eq!(
        resp.status().as_u16(),
        201,
        "register {username} should 201"
    );
    let body: Value = resp.json().unwrap();
    let api_key = body["api_key"].as_str().unwrap().to_string();
    let user_id = body["user_id"].as_str().unwrap().to_string();
    (user_id, api_key)
}

// ─── /api/admin/users tests ────────────────────────────────────────────────

#[test]
fn admin_users_list_requires_auth() {
    let server = spawn_team_server();
    // Register one user so the table isn't empty (avoids any compat carve-out).
    let _ = register(&server, "alice", "pw alice 1234");
    let resp = http_client()
        .get(format!("{}/api/admin/users", server.base_url()))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401, "no auth → 401");
}

#[test]
fn admin_users_list_non_admin_forbidden() {
    let server = spawn_team_server();
    let (_admin_uid, _admin_key) = register(&server, "admin", "pw admin 1234");
    let (_bob_uid, bob_key) = register(&server, "bob", "pw bob 1234"); // non-admin
    let resp = http_client()
        .get(format!("{}/api/admin/users", server.base_url()))
        .bearer_auth(&bob_key)
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403, "non-admin → 403");
}

#[test]
fn admin_users_list_admin_sees_all() {
    let server = spawn_team_server();
    let (_aid, akey) = register(&server, "admin", "pw admin 1234");
    let _ = register(&server, "bob", "pw bob 1234");
    let _ = register(&server, "carol", "pw carol 1234");
    let resp = http_client()
        .get(format!("{}/api/admin/users", server.base_url()))
        .bearer_auth(&akey)
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().unwrap();
    assert_eq!(body["total"].as_u64().unwrap(), 3);
    let items = body["items"].as_array().unwrap();
    let names: Vec<&str> = items
        .iter()
        .map(|x| x["username"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"admin"));
    assert!(names.contains(&"bob"));
    assert!(names.contains(&"carol"));
    // first user is admin
    let admin_row = items
        .iter()
        .find(|x| x["username"] == "admin")
        .expect("admin row");
    assert!(admin_row["is_admin"].as_bool().unwrap());
    let bob_row = items.iter().find(|x| x["username"] == "bob").unwrap();
    assert!(!bob_row["is_admin"].as_bool().unwrap());
}

// ─── POST /api/admin/users/:id (patch) ─────────────────────────────────────

#[test]
fn admin_users_update_promote_to_admin() {
    let server = spawn_team_server();
    let (_aid, akey) = register(&server, "admin", "pw admin 1234");
    let (bob_uid, _bk) = register(&server, "bob", "pw bob 1234");
    let resp = http_client()
        .post(format!("{}/api/admin/users/{}", server.base_url(), bob_uid))
        .bearer_auth(&akey)
        .json(&json!({ "is_admin": true }))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().unwrap();
    assert!(body["is_admin"].as_bool().unwrap());
    assert_eq!(body["user_id"].as_str().unwrap(), bob_uid);
}

#[test]
fn admin_users_update_disable_user() {
    let server = spawn_team_server();
    let (_aid, akey) = register(&server, "admin", "pw admin 1234");
    let (bob_uid, _bk) = register(&server, "bob", "pw bob 1234");
    let resp = http_client()
        .post(format!("{}/api/admin/users/{}", server.base_url(), bob_uid))
        .bearer_auth(&akey)
        .json(&json!({ "disabled": true }))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().unwrap();
    assert!(body["disabled"].as_bool().unwrap());
}

#[test]
fn admin_users_update_cannot_self_demote() {
    let server = spawn_team_server();
    let (aid, akey) = register(&server, "admin", "pw admin 1234");
    let resp = http_client()
        .post(format!("{}/api/admin/users/{}", server.base_url(), aid))
        .bearer_auth(&akey)
        .json(&json!({ "is_admin": false }))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400, "self-demote rejected");
}

#[test]
fn admin_users_update_cannot_self_disable() {
    let server = spawn_team_server();
    let (aid, akey) = register(&server, "admin", "pw admin 1234");
    let resp = http_client()
        .post(format!("{}/api/admin/users/{}", server.base_url(), aid))
        .bearer_auth(&akey)
        .json(&json!({ "disabled": true }))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400, "self-disable rejected");
}

#[test]
fn admin_users_update_404_nonexistent() {
    let server = spawn_team_server();
    let (_aid, akey) = register(&server, "admin", "pw admin 1234");
    let resp = http_client()
        .post(format!("{}/api/admin/users/usr_ghost", server.base_url()))
        .bearer_auth(&akey)
        .json(&json!({ "is_admin": true }))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}

#[test]
fn admin_users_update_non_admin_forbidden() {
    let server = spawn_team_server();
    let (_aid, _akey) = register(&server, "admin", "pw admin 1234");
    let (bob_uid, bob_key) = register(&server, "bob", "pw bob 1234");
    let resp = http_client()
        .post(format!("{}/api/admin/users/{}", server.base_url(), bob_uid))
        .bearer_auth(&bob_key) // bob is not admin
        .json(&json!({ "is_admin": true }))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
}

// ─── DELETE /api/admin/users/:id ───────────────────────────────────────────

#[test]
fn admin_users_delete_removes_user() {
    let server = spawn_team_server();
    let (_aid, akey) = register(&server, "admin", "pw admin 1234");
    let (bob_uid, _bk) = register(&server, "bob", "pw bob 1234");
    let resp = http_client()
        .delete(format!("{}/api/admin/users/{}", server.base_url(), bob_uid))
        .bearer_auth(&akey)
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().unwrap();
    assert_eq!(body["user_id"].as_str().unwrap(), bob_uid);
    // Bob really gone from the list.
    let list: Value = http_client()
        .get(format!("{}/api/admin/users", server.base_url()))
        .bearer_auth(&akey)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(list["total"].as_u64().unwrap(), 1, "only admin left");
}

#[test]
fn admin_users_delete_cannot_self() {
    let server = spawn_team_server();
    let (aid, akey) = register(&server, "admin", "pw admin 1234");
    let resp = http_client()
        .delete(format!("{}/api/admin/users/{}", server.base_url(), aid))
        .bearer_auth(&akey)
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400, "cannot delete self");
}

#[test]
fn admin_users_delete_404_nonexistent() {
    let server = spawn_team_server();
    let (_aid, akey) = register(&server, "admin", "pw admin 1234");
    let resp = http_client()
        .delete(format!("{}/api/admin/users/usr_ghost", server.base_url()))
        .bearer_auth(&akey)
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}

#[test]
fn admin_users_delete_non_admin_forbidden() {
    let server = spawn_team_server();
    let (_aid, _akey) = register(&server, "admin", "pw admin 1234");
    let (bob_uid, bob_key) = register(&server, "bob", "pw bob 1234");
    let resp = http_client()
        .delete(format!("{}/api/admin/users/{}", server.base_url(), bob_uid))
        .bearer_auth(&bob_key)
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
}
