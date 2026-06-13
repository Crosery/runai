//! Coverage tests for `src/server.rs` public surface (chunk c0).
//!
//! Targets in this file (signatures grepped from `src/server.rs`):
//!   - `pub enum EnsureStatus { AlreadyRunning, Started }`
//!   - `pub fn ensure_running(host: &str, port: u16) -> Result<EnsureStatus>`
//!   - `pub async fn serve(host: &str, port: u16) -> Result<()>`
//!
//! `api_tls_fingerprint` is SKIPPED — that endpoint does not exist in this
//! tree (no route, no handler, no symbol). See returned `skipped[]`.
//!
//! Test isolation rules (must hold for every test in this file):
//!   - Random port via `TcpListener::bind("127.0.0.1:0")` + immediate drop;
//!     never hard-code a port (port conflicts killed earlier rounds).
//!   - Per-test sandboxed HOME via `tempfile::TempDir`.
//!   - `RUNE_DATA_DIR` points inside the sandbox HOME.
//!   - `RUNAI_NO_AUTOSPAWN=1` so a stray TUI run never forks an extra server.
//!
//! Skipped on Windows: the existing server-side test harnesses are all
//! unix-only because HOME mocking via env vars does not work there (see the
//! Windows constraint in `AGENTS.md`).
#![cfg(not(target_os = "windows"))]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use runai::server::{EnsureStatus, ensure_running};

// ─── helpers for spawning `runai server` (used by serve tests) ──────────────

/// Locate the cargo-built `runai` binary, mirroring `tests/mcp_stdio_test.rs`.
fn runai_binary() -> PathBuf {
    let bin_name = format!("runai{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(&bin_name);
    assert!(
        path.exists(),
        "runai binary missing at {} — cargo test should build it",
        path.display()
    );
    path
}

/// Reserve an ephemeral port and immediately drop the listener so a child
/// process can rebind it. There is a small TOCTOU window but it is the same
/// pattern axum / hyper integration tests use.
fn reserve_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

/// Sandbox for spawning `runai server` against a tempdir HOME.
struct ServerSandbox {
    home: TempDir,
    child: Option<Child>,
    port: u16,
}

impl ServerSandbox {
    fn spawn(port: u16, host: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let data_dir = home.path().join(".runai");
        std::fs::create_dir_all(&data_dir).unwrap();

        let child = Command::new(runai_binary())
            .arg("server")
            .arg("--port")
            .arg(port.to_string())
            .arg("--host")
            .arg(host)
            .env("HOME", home.path())
            .env("RUNE_DATA_DIR", &data_dir)
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn runai server");

        Self {
            home,
            child: Some(child),
            port,
        }
    }

    /// Poll `GET /` until 200 or timeout. Returns whether the server came up.
    fn wait_until_ready(&self, max: Duration) -> bool {
        let deadline = Instant::now() + max;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let url = format!("http://127.0.0.1:{}/", self.port);
        while Instant::now() < deadline {
            if let Ok(r) = client.get(&url).send() {
                if r.status().is_success() {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn home_path(&self) -> &std::path::Path {
        self.home.path()
    }
}

impl Drop for ServerSandbox {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

// ─── EnsureStatus (enum) ────────────────────────────────────────────────────

#[test]
fn ensurestatus_alreadyrunning_variant_is_constructible_and_eq() {
    let v = EnsureStatus::AlreadyRunning;
    assert_eq!(v, EnsureStatus::AlreadyRunning);
    assert_ne!(v, EnsureStatus::Started);
}

#[test]
fn ensurestatus_started_variant_is_constructible_and_eq() {
    let v = EnsureStatus::Started;
    assert_eq!(v, EnsureStatus::Started);
    assert_ne!(v, EnsureStatus::AlreadyRunning);
}

#[test]
fn ensurestatus_pattern_match_exhaustively_covers_both_arms() {
    // Force both arms to be reachable; if a new variant is ever added this
    // test will fail to compile (good — fail loudly on enum growth).
    for v in [EnsureStatus::AlreadyRunning, EnsureStatus::Started] {
        let label = match v {
            EnsureStatus::AlreadyRunning => "already",
            EnsureStatus::Started => "started",
        };
        assert!(label == "already" || label == "started");
    }
}

#[test]
fn ensurestatus_debug_contains_variant_name() {
    // The enum derives Debug; both variant names should appear in the
    // formatted output (used by anyhow context strings in the wild).
    assert!(format!("{:?}", EnsureStatus::AlreadyRunning).contains("AlreadyRunning"));
    assert!(format!("{:?}", EnsureStatus::Started).contains("Started"));
}

// ─── ensure_running (function) ──────────────────────────────────────────────

#[test]
fn ensure_running_returns_alreadyrunning_when_port_already_serves() {
    // Hold an ephemeral port open in this process — `ensure_running` only
    // does a TCP connect probe, so any listener accepts and short-circuits
    // before any spawn happens.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind probe listener");
    let port = listener.local_addr().unwrap().port();

    let out = ensure_running("127.0.0.1", port).expect("ensure_running");
    assert_eq!(
        out,
        EnsureStatus::AlreadyRunning,
        "any TCP listener on the port must look 'already running'"
    );

    drop(listener);
}

#[test]
fn ensure_running_rejects_unparseable_host_port() {
    // "not.an.ip" is not parseable as a SocketAddr — the `addr_str.parse()`
    // step at the top of `ensure_running` must surface the error.
    let err = ensure_running("not.an.ip", 17888).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("parse") || msg.contains("not.an.ip"),
        "expected parse-error context, got: {msg}"
    );
}

#[test]
fn ensure_running_returns_alreadyrunning_for_localhost_v4_string() {
    // The function does NOT do DNS — "localhost" string would fail SocketAddr
    // parse. Using "127.0.0.1" is the canonical happy path; pair with a held
    // listener to lock in the AlreadyRunning fast path.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind probe");
    let port = listener.local_addr().unwrap().port();

    let out = ensure_running("127.0.0.1", port).expect("ensure_running");
    assert_eq!(out, EnsureStatus::AlreadyRunning);

    drop(listener);
}

// ─── serve (function, via real binary) ──────────────────────────────────────

#[test]
fn serve_binds_random_port_and_answers_root() {
    // `serve` is `async` and would block this test indefinitely on `axum::serve`.
    // Spawning the real binary covers the same code path (the CLI handler calls
    // `server::serve(host, port).await`) and lets us assert on observable HTTP
    // behavior + clean shutdown.
    let port = reserve_port();
    let sandbox = ServerSandbox::spawn(port, "127.0.0.1");

    assert!(
        sandbox.wait_until_ready(Duration::from_secs(10)),
        "spawned `runai server --port {port}` never answered GET /"
    );

    // Also confirm the dashboard root returns the bundled HTML.
    let body = reqwest::blocking::get(format!("http://127.0.0.1:{port}/"))
        .unwrap()
        .text()
        .unwrap();
    assert!(
        body.contains("<html") || body.contains("<!doctype") || body.contains("<!DOCTYPE"),
        "GET / should return HTML, got first 120 bytes: {}",
        &body[..body.len().min(120)]
    );

    // Sandbox HOME should already have a `~/.runai/` because the server
    // touched it (DB open writes the file). Check BEFORE dropping the
    // sandbox — TempDir cleanup happens on drop, after which the path is
    // gone and the assertion would falsely fail.
    let data_dir = sandbox.home_path().join(".runai");
    assert!(
        data_dir.exists(),
        "server must have populated sandbox HOME at {}",
        data_dir.display()
    );
}

#[test]
fn serve_exposes_dashboard_static_assets() {
    let port = reserve_port();
    let sandbox = ServerSandbox::spawn(port, "127.0.0.1");
    assert!(sandbox.wait_until_ready(Duration::from_secs(10)));

    // app.js + app.css are the bundled dashboard assets — serving these is
    // a core route registered inside `serve`'s Router::new().
    for path in ["/app.js", "/app.css"] {
        let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}{path}")).unwrap();
        assert!(
            resp.status().is_success(),
            "GET {path} should 200, got {}",
            resp.status()
        );
    }
}

#[test]
fn serve_exposes_summary_api_route() {
    let port = reserve_port();
    let sandbox = ServerSandbox::spawn(port, "127.0.0.1");
    assert!(sandbox.wait_until_ready(Duration::from_secs(10)));

    // `/api/summary` returns JSON; a fresh sandbox has no events so the
    // response body is just `{"total":0,...}`. We only need it to 200 + parse.
    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/api/summary")).unwrap();
    assert!(
        resp.status().is_success(),
        "/api/summary status: {}",
        resp.status()
    );
    let v: serde_json::Value = resp.json().expect("body parses as JSON");
    assert!(
        v.get("total").is_some(),
        "summary response missing `total`: {v}"
    );
}

#[test]
fn ensure_running_against_spawned_serve_sees_alreadyrunning() {
    // End-to-end: spawn `runai server` (the same code path `ensure_running`
    // would otherwise fork), then call `ensure_running` from this test. With
    // the port already serving, the call must take the fast TCP-probe path
    // and return AlreadyRunning without spawning anything else.
    let port = reserve_port();
    let sandbox = ServerSandbox::spawn(port, "127.0.0.1");
    assert!(sandbox.wait_until_ready(Duration::from_secs(10)));

    let out = ensure_running("127.0.0.1", port).expect("ensure_running");
    assert_eq!(out, EnsureStatus::AlreadyRunning);
}
