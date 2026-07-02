//! Real e2e for the local machine-owner CLI `runai admin reset-password`.
//!
//! This is the正规 replacement for the "forgot password → hand-edit the
//! users table via SQL" fallback. It operates directly on the local
//! `runai.db` (machine-owner identity, no HTTP) and rotates the api_key so
//! previously-installed clients must re-login.
//!
//! Flow: spawn a real team-mode server in an isolated HOME, register bob
//! over HTTP, then run the CLI against the SAME HOME (same `~/.runai/runai.db`)
//! to reset bob's password. Verify via the live server that the new password
//! logs in and the old one is rejected, and the pre-reset api_key is dead.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
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
    home: TempDir,
    port: u16,
}

impl ServerGuard {
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn home_path(&self) -> &Path {
        self.home.path()
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
    let g = ServerGuard { child, home, port };
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

fn register(server: &ServerGuard, username: &str, password: &str) -> (String, String) {
    let resp = http_client()
        .post(format!("{}/users/register", server.base_url()))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .expect("register");
    assert_eq!(resp.status().as_u16(), 201, "register {username} → 201");
    let body: Value = resp.json().unwrap();
    (
        body["user_id"].as_str().unwrap().to_string(),
        body["api_key"].as_str().unwrap().to_string(),
    )
}

fn login_status(server: &ServerGuard, username: &str, password: &str) -> u16 {
    http_client()
        .post(format!("{}/auth/login", server.base_url()))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .expect("login")
        .status()
        .as_u16()
}

/// Run `runai admin reset-password <username> --password <pw>` against the
/// SAME isolated HOME the server uses so it hits the same `~/.runai/runai.db`.
fn cli_reset_password(home: &Path, username: &str, password: &str) -> std::process::Output {
    runai_cmd()
        .arg("admin")
        .arg("reset-password")
        .arg(username)
        .arg("--password")
        .arg(password)
        .env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .expect("run runai admin reset-password")
}

// ─── tests ─────────────────────────────────────────────────────────────────

#[test]
fn cli_reset_password_changes_login_credentials() {
    let server = spawn_team_server();
    let (_bob_uid, bob_old_key) = register(&server, "bob", "pw bob original");

    let out = cli_reset_password(server.home_path(), "bob", "cli reset pw");
    assert!(
        out.status.success(),
        "CLI reset should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // New password logs in; old password rejected.
    assert_eq!(
        login_status(&server, "bob", "cli reset pw"),
        200,
        "new password logs in after CLI reset"
    );
    assert_eq!(
        login_status(&server, "bob", "pw bob original"),
        401,
        "old password rejected after CLI reset"
    );

    // Pre-reset api_key is dead (CLI rotates api_key_hash).
    let me = http_client()
        .get(format!("{}/api/me", server.base_url()))
        .bearer_auth(&bob_old_key)
        .send()
        .unwrap();
    assert_eq!(
        me.status().as_u16(),
        401,
        "old api_key must 401 after CLI reset rotates it"
    );
}

#[test]
fn cli_reset_password_unknown_user_errors_without_panic() {
    let server = spawn_team_server();
    let _ = register(&server, "bob", "pw bob 1234");
    let out = cli_reset_password(server.home_path(), "nosuchuser", "whatever pw");
    assert!(
        !out.status.success(),
        "resetting an unknown user must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("nosuchuser") || stderr.to_lowercase().contains("no such user"),
        "error should name the missing user; got stderr={stderr}"
    );
    // Non-panic: a Rust panic prints "panicked at"; a clean anyhow error does not.
    assert!(
        !stderr.contains("panicked at"),
        "must be a clean error, not a panic; stderr={stderr}"
    );
}

#[test]
fn cli_reset_password_short_password_errors() {
    let server = spawn_team_server();
    let _ = register(&server, "bob", "pw bob 1234");
    let out = cli_reset_password(server.home_path(), "bob", "12345");
    assert!(
        !out.status.success(),
        "a <6-char password must be rejected by the CLI"
    );
}
