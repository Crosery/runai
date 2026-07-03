//! Real e2e for the admin "reset any user's password" endpoint:
//!   POST /api/admin/users/:user_id/reset-password  body {"new_password": "..."}
//!
//! Security-sensitive: it writes the `users` table argon2 password_hash AND
//! rotates the target's api_key_hash (forcing a fresh login). Drives a real
//! `runai server --mode team` in an isolated HOME, registers users via
//! /users/register (first one auto-admin), then exercises the reset surface.
//!
//! Contract asserted here:
//!   - admin reset → 200; target logs in with the NEW password (200) and the
//!     OLD password fails (401).
//!   - non-admin caller → 403; unauthenticated → 401.
//!   - empty / <6-char new_password → 400.
//!   - unknown user_id → 404.
//!   - reset rotates the api_key: the target's pre-reset Bearer 401s on /api/me.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};
use tempfile::TempDir;

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

/// Register a user. First call on a fresh server → auto-admin. Returns
/// (user_id, api_key).
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

/// Attempt login (script-style, rotate_api_key so a 200 carries the key).
/// Returns (status_code, Option<api_key>).
fn login(server: &ServerGuard, username: &str, password: &str) -> (u16, Option<String>) {
    let resp = http_client()
        .post(format!("{}/auth/login", server.base_url()))
        .json(&json!({
            "username": username,
            "password": password,
            "rotate_api_key": true
        }))
        .send()
        .expect("login request");
    let status = resp.status().as_u16();
    let key = if status == 200 {
        resp.json::<Value>()
            .ok()
            .and_then(|v| v["api_key"].as_str().map(str::to_string))
    } else {
        None
    };
    (status, key)
}

fn reset_password(
    server: &ServerGuard,
    admin_key: &str,
    target_uid: &str,
    new_password: &str,
) -> u16 {
    http_client()
        .post(format!(
            "{}/api/admin/users/{}/reset-password",
            server.base_url(),
            target_uid
        ))
        .bearer_auth(admin_key)
        .json(&json!({ "new_password": new_password }))
        .send()
        .expect("reset-password request")
        .status()
        .as_u16()
}

// ─── tests ─────────────────────────────────────────────────────────────────

#[test]
fn reset_password_admin_resets_target_then_new_password_works_old_fails() {
    let server = spawn_team_server();
    let (_aid, akey) = register(&server, "admin", "pw admin 1234");
    let (bob_uid, _bk) = register(&server, "bob", "pw bob original");

    assert_eq!(
        reset_password(&server, &akey, &bob_uid, "brand new pw"),
        200,
        "admin reset should 200"
    );

    // New password logs in.
    let (new_status, new_key) = login(&server, "bob", "brand new pw");
    assert_eq!(new_status, 200, "new password should log in");
    assert!(new_key.is_some(), "login mints a fresh key");

    // Old password rejected.
    let (old_status, _) = login(&server, "bob", "pw bob original");
    assert_eq!(old_status, 401, "old password must be rejected after reset");
}

#[test]
fn reset_password_admin_can_reset_self() {
    let server = spawn_team_server();
    let (aid, akey) = register(&server, "admin", "pw admin orig");
    assert_eq!(
        reset_password(&server, &akey, &aid, "admin new pw"),
        200,
        "admin resetting self should 200"
    );
    let (status, _) = login(&server, "admin", "admin new pw");
    assert_eq!(status, 200, "admin new password works");
}

#[test]
fn reset_password_rotates_target_api_key() {
    let server = spawn_team_server();
    let (_aid, akey) = register(&server, "admin", "pw admin 1234");
    let (bob_uid, bob_old_key) = register(&server, "bob", "pw bob 1234");

    // Pre-reset the old key is valid.
    let pre = http_client()
        .get(format!("{}/api/me", server.base_url()))
        .bearer_auth(&bob_old_key)
        .send()
        .unwrap();
    assert_eq!(pre.status().as_u16(), 200, "bob key valid before reset");

    assert_eq!(reset_password(&server, &akey, &bob_uid, "rotated now"), 200);

    // The pre-reset Bearer must be dead — reset rotates api_key_hash.
    let post = http_client()
        .get(format!("{}/api/me", server.base_url()))
        .bearer_auth(&bob_old_key)
        .send()
        .unwrap();
    assert_eq!(
        post.status().as_u16(),
        401,
        "old api_key must 401 after reset rotates it"
    );
}

#[test]
fn reset_password_non_admin_forbidden() {
    let server = spawn_team_server();
    let (admin_uid, _akey) = register(&server, "admin", "pw admin 1234");
    let (_bob_uid, bob_key) = register(&server, "bob", "pw bob 1234");
    let status = reset_password(&server, &bob_key, &admin_uid, "hijack attempt");
    assert_eq!(status, 403, "non-admin caller → 403");
}

#[test]
fn reset_password_requires_auth() {
    let server = spawn_team_server();
    let (bob_uid, _bk) = register(&server, "bob", "pw bob 1234");
    let resp = http_client()
        .post(format!(
            "{}/api/admin/users/{}/reset-password",
            server.base_url(),
            bob_uid
        ))
        .json(&json!({ "new_password": "no auth here" }))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401, "unauthenticated → 401");
}

#[test]
fn reset_password_short_password_400() {
    let server = spawn_team_server();
    let (_aid, akey) = register(&server, "admin", "pw admin 1234");
    let (bob_uid, _bk) = register(&server, "bob", "pw bob 1234");
    assert_eq!(
        reset_password(&server, &akey, &bob_uid, "12345"),
        400,
        "<6-char password → 400"
    );
    assert_eq!(
        reset_password(&server, &akey, &bob_uid, ""),
        400,
        "empty password → 400"
    );
}

#[test]
fn reset_password_unknown_user_404() {
    let server = spawn_team_server();
    let (_aid, akey) = register(&server, "admin", "pw admin 1234");
    assert_eq!(
        reset_password(&server, &akey, "usr_ghost", "valid password"),
        404,
        "unknown user_id → 404"
    );
}
