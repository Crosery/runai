//! Issue #35: dashboard (browser) login must NOT rotate the api_key.
//!
//! The old model minted a fresh api_key on EVERY successful /auth/login and
//! set the cookie to that same key. Consequence: each browser login silently
//! revoked every installed hook client's `~/.runai-identity` — the hook then
//! skipped every prompt with no visible error.
//!
//! New contract:
//! - Plain `POST /auth/login {username,password}` (dashboard path) verifies
//!   the password, mints an independent SESSION token (`rnai_sess_...`,
//!   stored as `users.session_key_hash`), sets it as the cookie, and leaves
//!   `api_key_hash` untouched. The response body carries NO api_key.
//! - `POST /auth/login {..., "rotate_api_key": true}` (install-script path,
//!   which persists the key to disk) keeps the old rotate-and-return
//!   semantics. It does not touch the session slot, so an active browser
//!   session survives a script re-login.
//! - `POST /api/me/logout-everywhere` and admin password reset revoke BOTH
//!   the api_key and the session.

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
/// POST /auth/login without the rotate flag (dashboard path).
/// Returns (body, cookie-token) — the cookie token is the value of the
/// `runai_session=` pair from Set-Cookie.
fn dashboard_login(s: &ServerGuard, u: &str, p: &str) -> (Value, String) {
    let r = http()
        .post(format!("{}/auth/login", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let set_cookie = r
        .headers()
        .get("set-cookie")
        .expect("set-cookie header on dashboard login")
        .to_str()
        .unwrap()
        .to_string();
    let token = set_cookie
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("runai_session=")
        .expect("runai_session cookie")
        .to_string();
    let b: Value = r.json().unwrap();
    (b, token)
}
fn me_with_bearer(s: &ServerGuard, key: &str) -> u16 {
    http()
        .get(format!("{}/api/me", s.base_url()))
        .bearer_auth(key)
        .send()
        .unwrap()
        .status()
        .as_u16()
}
fn me_with_cookie(s: &ServerGuard, token: &str) -> u16 {
    http()
        .get(format!("{}/api/me", s.base_url()))
        .header("Cookie", format!("runai_session={token}"))
        .send()
        .unwrap()
        .status()
        .as_u16()
}

/// THE issue #35 fix: a plain (dashboard) login leaves the existing api_key
/// valid, omits any api_key from the response body, and the cookie it mints
/// authenticates on its own.
#[test]
fn dashboard_login_keeps_existing_api_key_and_omits_key_from_body() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    assert_eq!(me_with_bearer(&s, &key), 200, "fresh key works");

    let (body, cookie_token) = dashboard_login(&s, "alice", "pw alice 1234");
    assert!(
        body.get("api_key").is_none() || body["api_key"].is_null(),
        "dashboard login must not hand out an api_key, got: {body}"
    );
    assert_eq!(body["username"].as_str().unwrap(), "alice");

    // The hook's stored key must survive the browser login.
    assert_eq!(
        me_with_bearer(&s, &key),
        200,
        "existing api_key must stay valid after a dashboard login"
    );
    // The freshly minted session cookie authenticates too.
    assert_eq!(me_with_cookie(&s, &cookie_token), 200);
}

/// The session token is an independent secret, not the api_key: it must not
/// double as a Bearer credential on the hook lane.
#[test]
fn session_token_is_not_a_valid_bearer() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let (_body, cookie_token) = dashboard_login(&s, "alice", "pw alice 1234");
    assert_eq!(
        me_with_bearer(&s, &cookie_token),
        401,
        "session cookie token must be rejected on the Bearer lane"
    );
}

/// The install-script path opts in with rotate_api_key=true and keeps the
/// old rotate-and-return contract.
#[test]
fn login_with_rotate_flag_rotates_api_key() {
    let s = spawn_team_server();
    let (_uid, key_v1) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!("{}/auth/login", s.base_url()))
        .json(&json!({
            "username": "alice",
            "password": "pw alice 1234",
            "rotate_api_key": true
        }))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    let key_v2 = b["api_key"].as_str().expect("rotate login returns api_key");
    assert!(key_v2.starts_with("rnai_live_"));
    assert_ne!(key_v1, key_v2);
    assert_eq!(me_with_bearer(&s, key_v2), 200, "new key works");
    assert_eq!(me_with_bearer(&s, &key_v1), 401, "old key revoked");
}

/// A script re-login (rotate) must not clobber an active browser session.
#[test]
fn rotate_login_preserves_dashboard_session() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let (_body, cookie_token) = dashboard_login(&s, "alice", "pw alice 1234");
    assert_eq!(me_with_cookie(&s, &cookie_token), 200);

    let r = http()
        .post(format!("{}/auth/login", s.base_url()))
        .json(&json!({
            "username": "alice",
            "password": "pw alice 1234",
            "rotate_api_key": true
        }))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);

    assert_eq!(
        me_with_cookie(&s, &cookie_token),
        200,
        "browser session must survive a script re-login"
    );
}

/// logout-everywhere is the real revoke: kills the api_key AND the session.
#[test]
fn logout_everywhere_revokes_both_key_and_session() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let (_body, cookie_token) = dashboard_login(&s, "alice", "pw alice 1234");

    let r = http()
        .post(format!("{}/api/me/logout-everywhere", s.base_url()))
        .header("Cookie", format!("runai_session={cookie_token}"))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);

    assert_eq!(me_with_bearer(&s, &key), 401, "api_key revoked");
    assert_eq!(me_with_cookie(&s, &cookie_token), 401, "session revoked");
}

/// Admin password reset implies "old secrets are compromised": it must
/// revoke the target's active session alongside the api_key.
#[test]
fn admin_reset_password_revokes_session() {
    let s = spawn_team_server();
    let (_aid, admin_key) = register(&s, "admin", "pw admin 1234");
    let (bob_uid, bob_key) = register(&s, "bob", "pw bob 1234");
    let (_body, bob_cookie) = dashboard_login(&s, "bob", "pw bob 1234");
    assert_eq!(me_with_cookie(&s, &bob_cookie), 200);

    let r = http()
        .post(format!(
            "{}/api/admin/users/{}/reset-password",
            s.base_url(),
            bob_uid
        ))
        .bearer_auth(&admin_key)
        .json(&json!({"new_password": "pw bob 5678"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);

    assert_eq!(me_with_bearer(&s, &bob_key), 401, "bob's key revoked");
    assert_eq!(
        me_with_cookie(&s, &bob_cookie),
        401,
        "bob's session revoked"
    );
}
