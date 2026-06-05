//! Phase P5 e2e: anti-explore probe rejection (PLANNING.md §2.3 item 4).
//!
//! Background: an automated agent that doesn't know runai's API surface
//! will fingerprint the server by probing the canonical "tell me about
//! yourself" endpoints — `/openapi.json`, `/swagger`, `/docs`,
//! `/__schema`, etc. Any one of these returning anything but a vanilla
//! 404 leaks routing structure. This suite walks the canonical probe
//! list and asserts every response is `404 + empty body`.
//!
//! The fallback handler is also exercised by probing a completely made-up
//! path; same contract.
//!
//! All tests run against the real `runai server` binary, isolated HOME,
//! 127.0.0.1 binding — same pattern as `server_mode_e2e.rs`.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers (mirror of server_mode_e2e.rs) ─────────────────────────────────

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

    let mut cmd = runai_cmd();
    cmd.arg("server")
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

// The canonical "self-describing API" probe list. Any one of these
// returning anything but `404 + empty body` is a regression.
const PROBE_PATHS: &[&str] = &[
    "/openapi.json",
    "/openapi.yaml",
    "/swagger",
    "/swagger.json",
    "/swagger-ui",
    "/swagger-ui/",
    "/swagger-ui/index.html",
    "/docs",
    "/docs/",
    "/docs/index.html",
    "/api-docs",
    "/api-docs/",
    "/api-docs/index.html",
    "/__schema",
    "/graphql",
    "/redoc",
];

// ─── tests ──────────────────────────────────────────────────────────────────

/// PLANNING §2.3 item 4: every known self-describing-API probe path
/// returns 404 + empty body in BOTH owner and team modes. Loop over modes
/// inside one #[test] so a regression on either is visible in one report
/// line.
#[test]
fn anti_explore_probes_return_empty_404() {
    for mode in ["owner", "team"] {
        let server = spawn_server(mode);
        let client = http_client();
        for path in PROBE_PATHS {
            let url = format!("{}{}", server.base_url(), path);
            let resp = client.get(&url).send().expect("GET probe");
            let status = resp.status().as_u16();
            let body = resp.text().expect("body");
            assert_eq!(
                status, 404,
                "[mode={mode}] {path} must be 404; got {status}"
            );
            assert!(
                body.is_empty(),
                "[mode={mode}] {path} must have empty body; got {body:?}"
            );
        }
    }
}

/// PLANNING §2.3 item 4: an entirely made-up path falls through to the
/// global fallback handler with the same 404 + empty body contract.
#[test]
fn unknown_path_returns_empty_404() {
    let server = spawn_server("team");
    let client = http_client();
    let url = format!(
        "{}/this/route/does/not/exist/{}",
        server.base_url(),
        std::process::id()
    );
    let resp = client.get(&url).send().expect("GET unknown path");
    assert_eq!(resp.status().as_u16(), 404, "unknown path must be 404");
    let body = resp.text().expect("body");
    assert!(
        body.is_empty(),
        "unknown path must have empty body; got {body:?}"
    );
}

/// Probe paths must be method-agnostic 404 — an attacker switching to POST
/// or HEAD or OPTIONS to elicit a different response is the same threat
/// surface. The route table uses `any(...)` for the probe list specifically
/// to catch this.
#[test]
fn anti_explore_probes_404_on_other_methods() {
    let server = spawn_server("team");
    let client = http_client();
    for path in ["/openapi.json", "/swagger", "/docs", "/__schema"] {
        let url = format!("{}{}", server.base_url(), path);
        for method in ["POST", "HEAD", "OPTIONS"] {
            let req = client
                .request(method.parse().unwrap(), &url)
                .body("{}")
                .send()
                .expect("probe non-GET");
            let status = req.status().as_u16();
            assert_eq!(
                status, 404,
                "[{method}] {path} must be 404; got {status} (method-agnostic 404 contract)"
            );
        }
    }
}
