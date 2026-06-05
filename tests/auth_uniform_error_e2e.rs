//! Phase P5 e2e: `/auth/login` failure responses are byte-identical for
//! "user does not exist" vs "wrong password" (PLANNING.md §2.3 item 5).
//!
//! Threat model: an attacker walks a wordlist against `/auth/login`. If
//! the response for "user not found" differs from "user exists but bad
//! password" by even a single byte (different status, different body,
//! different `Content-Length`, different header order), the attacker can
//! enumerate the account list. The fix is to collapse both failure modes
//! to the same `401 + {"error":"invalid_credentials"}` response.
//!
//! Test asserts byte-level equality of (status, body) for both cases. We
//! don't assert header-level equality because axum auto-adds
//! `Content-Length`, `Date`, and the order of headers is implementation-
//! defined; status + body are what an attacker would diff in practice.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

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

fn spawn_team_server() -> ServerGuard {
    let home = tempfile::tempdir().expect("create tmp HOME");
    std::fs::create_dir_all(home.path().join(".runai/skills")).expect("pre-create .runai/skills");
    let port = free_port();
    let mut cmd = runai_cmd();
    cmd.arg("server")
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
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn runai server");
    let guard = ServerGuard {
        child,
        _home: home,
        port,
    };
    assert!(
        wait_for_port(port, Duration::from_secs(8)),
        "runai team server did not bind 127.0.0.1:{port} within 8s"
    );
    guard
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client")
}

fn post_login(
    client: &reqwest::blocking::Client,
    base: &str,
    username: &str,
    password: &str,
) -> (u16, String) {
    let resp = client
        .post(format!("{base}/auth/login"))
        .json(&serde_json::json!({
            "username": username,
            "password": password,
        }))
        .send()
        .expect("POST /auth/login");
    let status = resp.status().as_u16();
    let body = resp.text().expect("body");
    (status, body)
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// PLANNING §2.3 item 5: "no such user" and "wrong password" must be
/// indistinguishable on the wire — same status, same response body.
#[test]
fn login_failure_is_byte_identical_for_unknown_user_vs_bad_password() {
    let server = spawn_team_server();
    let client = http_client();

    // Register one real user so we have a known-good account to attempt
    // bad-password against.
    let reg = client
        .post(format!("{}/users/register", server.base_url()))
        .json(&serde_json::json!({
            "username": "alice",
            "password": "correct horse battery staple",
        }))
        .send()
        .expect("POST /users/register");
    assert_eq!(reg.status().as_u16(), 201, "register alice should succeed");

    // Case A: user does not exist.
    let (status_a, body_a) = post_login(
        &client,
        &server.base_url(),
        "bob-does-not-exist",
        "anything",
    );
    // Case B: user exists, wrong password.
    let (status_b, body_b) = post_login(&client, &server.base_url(), "alice", "wrong-password");

    assert_eq!(
        status_a, 401,
        "unknown-user must be 401; got {status_a} body={body_a}"
    );
    assert_eq!(
        status_b, 401,
        "wrong-password must be 401; got {status_b} body={body_b}"
    );
    assert_eq!(
        body_a, body_b,
        "failure bodies must be byte-identical to avoid account enumeration",
    );
    // Anchor the wire format so a careless future edit cannot silently
    // start leaking distinguishable error messages.
    assert_eq!(
        body_a, r#"{"error":"invalid_credentials"}"#,
        "PLANNING §2.3 item 5 mandates this exact body",
    );
}
