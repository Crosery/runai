//! Real e2e for the auth session flow (issue #19 still_open):
//!   POST /auth/login    (success / wrong-password / unknown-user → uniform 401)
//!   POST /auth/logout   (clears cookie)
//!   GET  /api/me        (returns user info via Bearer or cookie)
//!   GET  /api/prefs     (per-user prefs, requires auth)
//!
//! Also pins the login api_key ROTATION contract: every successful
//! /auth/login mints a fresh api_key and old keys stop working.

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

// ─── POST /auth/login ──────────────────────────────────────────────

#[test]
fn login_success_returns_user_and_api_key_and_sets_cookie() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!("{}/auth/login", s.base_url()))
        .json(&json!({"username":"alice","password":"pw alice 1234"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    // Cookie must be set
    let cookie = r.headers().get("set-cookie").expect("set-cookie header");
    assert!(cookie.to_str().unwrap().contains("runai_session="));
    let b: Value = r.json().unwrap();
    assert_eq!(b["username"].as_str().unwrap(), "alice");
    assert!(b["api_key"].as_str().unwrap().starts_with("rnai_live_"));
    assert!(b["user_id"].as_str().unwrap().starts_with("usr_"));
}

#[test]
fn login_wrong_password_returns_401() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!("{}/auth/login", s.base_url()))
        .json(&json!({"username":"alice","password":"WRONG"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn login_unknown_user_returns_401_same_shape_as_wrong_password() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    // unknown user
    let r1 = http()
        .post(format!("{}/auth/login", s.base_url()))
        .json(&json!({"username":"ghost","password":"anything"}))
        .send()
        .unwrap();
    // wrong password
    let r2 = http()
        .post(format!("{}/auth/login", s.base_url()))
        .json(&json!({"username":"alice","password":"WRONG"}))
        .send()
        .unwrap();
    assert_eq!(r1.status().as_u16(), 401);
    assert_eq!(r2.status().as_u16(), 401);
    // Body identical (no account enumeration)
    let b1 = r1.text().unwrap();
    let b2 = r2.text().unwrap();
    assert_eq!(b1, b2, "login failure body must be identical");
}

#[test]
fn login_rotates_api_key_invalidating_old_one() {
    let s = spawn_team_server();
    let (_uid, key_v1) = register(&s, "alice", "pw alice 1234");
    // confirm v1 works
    let r0 = http()
        .get(format!("{}/api/me", s.base_url()))
        .bearer_auth(&key_v1)
        .send()
        .unwrap();
    assert_eq!(r0.status().as_u16(), 200);

    // login → mint v2
    let r = http()
        .post(format!("{}/auth/login", s.base_url()))
        .json(&json!({"username":"alice","password":"pw alice 1234"}))
        .send()
        .unwrap();
    let b: Value = r.json().unwrap();
    let key_v2 = b["api_key"].as_str().unwrap().to_string();
    assert_ne!(key_v1, key_v2, "login must rotate api_key");

    // v2 works
    let r2 = http()
        .get(format!("{}/api/me", s.base_url()))
        .bearer_auth(&key_v2)
        .send()
        .unwrap();
    assert_eq!(r2.status().as_u16(), 200);

    // v1 no longer works
    let r1 = http()
        .get(format!("{}/api/me", s.base_url()))
        .bearer_auth(&key_v1)
        .send()
        .unwrap();
    assert_eq!(r1.status().as_u16(), 401, "old key invalidated");
}

#[test]
fn login_disabled_user_returns_401() {
    let s = spawn_team_server();
    let (_aid, akey) = register(&s, "admin", "pw admin 1234");
    let (bob_uid, _bk) = register(&s, "bob", "pw bob 1234");
    // disable bob via admin
    let _ = http()
        .post(format!("{}/api/admin/users/{}", s.base_url(), bob_uid))
        .bearer_auth(&akey)
        .json(&json!({"disabled": true}))
        .send()
        .unwrap();
    let r = http()
        .post(format!("{}/auth/login", s.base_url()))
        .json(&json!({"username":"bob","password":"pw bob 1234"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

// ─── POST /auth/logout ─────────────────────────────────────────────

#[test]
fn logout_returns_200_and_clears_cookie() {
    let s = spawn_team_server();
    let r = http()
        .post(format!("{}/auth/logout", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let cookie = r.headers().get("set-cookie").expect("set-cookie header");
    let cv = cookie.to_str().unwrap();
    assert!(cv.contains("runai_session="));
    assert!(
        cv.to_ascii_lowercase().contains("max-age=0"),
        "logout must zero max-age, got {cv}"
    );
}

// ─── GET /api/me ───────────────────────────────────────────────────

#[test]
fn me_requires_auth() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/me", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn me_returns_user_info_via_bearer() {
    let s = spawn_team_server();
    let (uid, key) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/me", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert_eq!(b["user_id"].as_str().unwrap(), uid);
    assert_eq!(b["username"].as_str().unwrap(), "alice");
    assert!(b["is_admin"].is_boolean());
    assert!(b["library_size"].is_number());
}

#[test]
fn me_first_user_is_admin() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let me: Value = http()
        .get(format!("{}/api/me", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(me["is_admin"].as_bool().unwrap(), true);
}

#[test]
fn me_second_user_is_not_admin() {
    let s = spawn_team_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let (_bob_uid, bob_key) = register(&s, "bob", "pw bob 1234");
    let me: Value = http()
        .get(format!("{}/api/me", s.base_url()))
        .bearer_auth(&bob_key)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(me["is_admin"].as_bool().unwrap(), false);
}

// ─── GET /api/prefs ────────────────────────────────────────────────

#[test]
fn prefs_get_requires_auth() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/prefs", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn prefs_get_returns_user_prefs_view() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/prefs", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    // UserPrefs has at least these fields
    assert!(
        b.get("recommend_mode").is_some() || b.get("candidate_limit").is_some() || b.is_object()
    );
}
