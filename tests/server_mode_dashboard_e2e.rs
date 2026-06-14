//! PLANNING §1.1 owner-mode dashboard cut: implicit-admin auth bypass +
//! `mode-owner` body class + `MeResp.mode` field.
//!
//! Pins the user-facing contract for the owner-mode dashboard:
//!   - `GET /api/me` succeeds with NO credential, returns synthetic
//!     `owner` user with `is_admin: true` and `mode: "owner"`.
//!   - `GET /api/skills` succeeds with NO credential (owner mode bypasses
//!     `private_data_locked` — local user is implicit admin).
//!   - `GET /` body carries the `mode-owner` class so CSS can hide the
//!     team-only chrome (account pill / login modal / userlib tab /
//!     scope selectors / community tab).
//!
//! Team-mode regression checks ensure the owner-mode short-circuits do
//! NOT leak into team mode:
//!   - `GET /api/me` without credential still returns 401.
//!   - `GET /` body does NOT carry the `mode-owner` class.
//!   - `GET /api/me` with bearer returns `mode: "team"`.
//!
//! All tests spawn the real binary inside an isolated HOME — the live
//! `~/.runai/runai.db` is never touched (AGENTS.md safety contract).

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
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
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

fn spawn_server(mode: &str) -> ServerGuard {
    let home = tempfile::tempdir().expect("create tmp HOME");
    std::fs::create_dir_all(home.path().join(".runai/skills")).expect("pre-create .runai/skills");
    let port = free_port();
    let child = runai_cmd()
        .arg("server")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--mode")
        .arg(mode)
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
        wait_for_port(g.port, Duration::from_secs(8)),
        "runai server (mode={mode}) did not bind 127.0.0.1:{} in 8s",
        g.port
    );
    g
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client")
}

fn register_first(s: &ServerGuard, username: &str, password: &str) -> String {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": username, "password": password}))
        .send()
        .expect("POST /users/register");
    assert_eq!(r.status().as_u16(), 201);
    let body: Value = r.json().expect("register JSON");
    body["api_key"].as_str().expect("api_key").to_string()
}

// ─── owner-mode dashboard contract ───────────────────────────────────────

#[test]
fn owner_me_no_credential_returns_implicit_admin_200() {
    let s = spawn_server("owner");
    let r = http()
        .get(format!("{}/api/me", s.base_url()))
        .send()
        .expect("GET /api/me");
    assert_eq!(
        r.status().as_u16(),
        200,
        "owner mode: /api/me must succeed without any credential"
    );
    let me: Value = r.json().expect("me JSON");
    assert_eq!(
        me["is_admin"].as_bool(),
        Some(true),
        "owner mode: synthetic owner must be implicit admin, got {me}"
    );
    assert_eq!(
        me["mode"].as_str(),
        Some("owner"),
        "owner mode: MeResp.mode must be \"owner\", got {me}"
    );
    assert!(
        me["user_id"].as_str().is_some(),
        "owner mode: synthetic owner must carry a user_id, got {me}"
    );
    assert!(
        me["username"].as_str().is_some(),
        "owner mode: synthetic owner must carry a username, got {me}"
    );
}

#[test]
fn owner_skills_no_credential_returns_200() {
    let s = spawn_server("owner");
    let r = http()
        .get(format!("{}/api/skills", s.base_url()))
        .send()
        .expect("GET /api/skills");
    assert_eq!(
        r.status().as_u16(),
        200,
        "owner mode: /api/skills must succeed without any credential \
         (private_data_locked bypass)"
    );
}

#[test]
fn owner_root_body_carries_mode_owner_class() {
    let s = spawn_server("owner");
    let r = http()
        .get(format!("{}/", s.base_url()))
        .send()
        .expect("GET /");
    assert_eq!(r.status().as_u16(), 200);
    let body = r.text().expect("HTML body");
    assert!(
        body.contains("mode-owner"),
        "owner mode: served HTML <body> must include the `mode-owner` class \
         so CSS can hide the team-only chrome. body excerpt: {}",
        body.lines().take(15).collect::<Vec<_>>().join("\n")
    );
}

// ─── team-mode regression ────────────────────────────────────────────────

#[test]
fn team_me_no_credential_returns_401_regression() {
    let s = spawn_server("team");
    // register one user so private_data_locked = true (mirrors realistic
    // team-mode deployment that already has an account).
    let _ = register_first(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/me", s.base_url()))
        .send()
        .expect("GET /api/me");
    assert_eq!(
        r.status().as_u16(),
        401,
        "team mode regression: /api/me without credential must still 401 \
         (owner-mode bypass must not leak)"
    );
}

#[test]
fn team_root_body_does_not_carry_mode_owner_class() {
    let s = spawn_server("team");
    let r = http()
        .get(format!("{}/", s.base_url()))
        .send()
        .expect("GET /");
    assert_eq!(r.status().as_u16(), 200);
    let body = r.text().expect("HTML body");
    assert!(
        !body.contains("mode-owner"),
        "team mode regression: served HTML <body> must NOT carry \
         `mode-owner` (would unhide team-only chrome inappropriately)"
    );
}

#[test]
fn team_me_with_bearer_returns_mode_team() {
    let s = spawn_server("team");
    let key = register_first(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/me", s.base_url()))
        .bearer_auth(&key)
        .send()
        .expect("GET /api/me");
    assert_eq!(r.status().as_u16(), 200);
    let me: Value = r.json().expect("me JSON");
    assert_eq!(
        me["mode"].as_str(),
        Some("team"),
        "team mode: MeResp.mode must be \"team\", got {me}"
    );
}
