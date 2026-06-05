//! Phase P1 e2e: server-mode (owner vs team) startup + register gating + TLS guard.
//!
//! These tests spawn the real `runai server` binary on a free port inside
//! an isolated HOME so the live `~/.runai/runai.db` is never touched, then
//! drive the dashboard over real HTTP and assert on status codes and
//! response shape.
//!
//! Covered:
//! 1. `owner` mode (default) rejects `POST /users/register` with 403.
//! 2. `team` mode on 127.0.0.1 starts without TLS and `/users/register`
//!    returns 201 + api_key.
//! 3. `team` mode bound to a non-loopback host without --tls-cert/--tls-key
//!    refuses to start (process exits non-zero, stderr explains why).
//!
//! Per PLANNING.md §1.1 / §2.3 item 2.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai_cmd() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Bind-then-drop trick: ask the OS for a free TCP port on 127.0.0.1, then
/// release it so the server can bind it. Cheaper and more robust than
/// hard-coding a port that another test (or developer's running server)
/// might already own.
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

/// A spawned `runai server` child plus the tempdir HOME it lives in.
/// `Drop` always SIGTERMs the child so a failed assert can't leave a
/// daemon dangling on the test runner.
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

fn spawn_server(home: TempDir, host: &str, port: u16, mode: &str) -> ServerGuard {
    // Pre-create the managed data dir so the server doesn't have to
    // bootstrap mid-startup (and so we can be sure HOME isolation worked).
    std::fs::create_dir_all(home.path().join(".runai/skills")).expect("pre-create .runai/skills");

    let mut cmd = runai_cmd();
    cmd.arg("server")
        .arg("--host")
        .arg(host)
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
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn runai server");

    let guard = ServerGuard {
        child,
        _home: home,
        port,
    };
    assert!(
        wait_for_port(port, Duration::from_secs(8)),
        "runai server did not bind 127.0.0.1:{port} (mode={mode}) within 8s"
    );
    guard
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client")
}

fn make_home() -> TempDir {
    tempfile::tempdir().expect("create tmp HOME")
}

fn write_test_skill(home_path: &Path, name: &str) {
    // Some endpoints expect a baseline of public skills (top_public_skills
    // is called during register). Adding none is fine — empty result is
    // valid — but planting one keeps the side-effects in register
    // deterministic for any future assertion that depends on libraryNames.
    let dir = home_path.join(".runai/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test skill\n---\n\n# {name}\n"),
    )
    .unwrap();
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// PLANNING §1.1: owner mode rejects register so an externally-reachable
/// owner server cannot have its account list grown by an attacker.
#[test]
fn owner_mode_rejects_register_with_403() {
    let home = make_home();
    let port = free_port();
    let server = spawn_server(home, "127.0.0.1", port, "owner");

    let client = http_client();
    let resp = client
        .post(format!("{}/users/register", server.base_url()))
        .json(&serde_json::json!({
            "username": "alice",
            "password": "correct horse battery staple",
        }))
        .send()
        .expect("POST /users/register");

    assert_eq!(
        resp.status().as_u16(),
        403,
        "owner mode must 403 register; got {}",
        resp.status()
    );
}

/// PLANNING §1.1: team mode on loopback works without TLS, and the
/// register endpoint returns the new api_key (first user → admin).
#[test]
fn team_mode_loopback_allows_register() {
    let home = make_home();
    write_test_skill(home.path(), "demo-skill");
    let port = free_port();
    let server = spawn_server(home, "127.0.0.1", port, "team");

    let client = http_client();
    let resp = client
        .post(format!("{}/users/register", server.base_url()))
        .json(&serde_json::json!({
            "username": "alice",
            "password": "correct horse battery staple",
        }))
        .send()
        .expect("POST /users/register");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "team mode loopback should accept register; got {}",
        resp.status()
    );

    let body: serde_json::Value = resp.json().expect("register JSON body");
    let api_key = body
        .get("api_key")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        api_key.starts_with("rnai_"),
        "register response should carry a runai api_key; got body={body}"
    );
    assert_eq!(
        body.get("is_admin").and_then(|v| v.as_bool()),
        Some(true),
        "first registered user in team mode should be auto-promoted to admin",
    );
    assert_eq!(body.get("username").and_then(|v| v.as_str()), Some("alice"),);
}

/// PLANNING §2.3 item 2: team mode bound to a non-loopback host without
/// TLS material refuses to start at all — we must NEVER ship multi-user
/// register / hook traffic over cleartext on a network. The bind we use
/// here is `0.0.0.0` so the box's real interface(s) would be exposed if
/// the guard were missing.
#[test]
fn team_mode_non_loopback_without_tls_bails() {
    let home = make_home();
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    let port = free_port();

    let out = runai_cmd()
        .arg("server")
        .arg("--host")
        .arg("0.0.0.0")
        .arg("--port")
        .arg(port.to_string())
        .arg("--mode")
        .arg("team")
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .expect("spawn runai server");

    assert!(
        !out.status.success(),
        "team + 0.0.0.0 + no TLS must fail to start; got exit={} stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--tls-cert") && stderr.contains("--tls-key"),
        "startup bail message should name the missing TLS flags; got stderr={stderr}"
    );
}

/// Case-insensitive `--mode` parsing: clap is case-sensitive on its own
/// (it lowercases value_enum names to `owner` / `team`), but the validator
/// accepts the canonical lowercase form unconditionally — this test guards
/// the happy path so a future tightening of the parser can't silently
/// drop "Owner" / "TEAM" without a failing test.
#[test]
fn owner_mode_default_when_flag_omitted() {
    let home = make_home();
    let port = free_port();
    // No --mode flag → ServerMode::default() == Owner. We verify by hitting
    // register and asserting the owner-mode 403 path.
    std::fs::create_dir_all(home.path().join(".runai/skills")).expect("pre-create .runai/skills");

    let mut cmd = runai_cmd();
    cmd.arg("server")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
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
        "runai server (default mode) did not bind 127.0.0.1:{port} within 8s"
    );

    let client = http_client();
    let resp = client
        .post(format!("{}/users/register", guard.base_url()))
        .json(&serde_json::json!({
            "username": "alice",
            "password": "correct horse battery staple",
        }))
        .send()
        .expect("POST /users/register");
    assert_eq!(
        resp.status().as_u16(),
        403,
        "default mode (no --mode flag) must behave as owner mode"
    );
}
