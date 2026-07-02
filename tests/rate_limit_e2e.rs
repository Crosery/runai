//! Phase P5 e2e: per-route rate limits (PLANNING.md §2.3 item 6).
//!
//! Covered:
//! 1. `/auth/login` is throttled to 5 / minute / IP. The 6th request from
//!    the same IP within the window returns `429 + empty body`.
//! 2. `/api/community/upload` is throttled to 10 / hour / user. The 11th
//!    request from the same user (Bearer api_key) returns `429 + empty
//!    body`. (We don't try to wait an hour — we just walk to 11 requests
//!    and assert the cliff.)
//!
//! `/skills/get/*` is 20 / sec / IP. We cover that path too (the
//! 21st request inside one second hits 429). On slow CI it's possible
//! that the 20 requests don't all land inside the same 1s window, so the
//! assertion is "we observed at least one 429 within the burst" rather
//! than "request 21 exactly". Same idea for the others — semantics over
//! exact off-by-one.
//!
//! All tests run against the real `runai server` binary, isolated HOME,
//! 127.0.0.1 binding. Each test spawns its own server so rate-limit state
//! is independent — the limiter is process-global, so two tests sharing
//! one process would deadlock on each other's buckets.

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

// ─── tests ──────────────────────────────────────────────────────────────────

/// PLANNING §2.3 item 6: `/auth/login` 5/min/IP. The 6th request from
/// the same IP must be 429 with empty body. We register a real user so
/// the limiter is exercised on the actual hot path (verify_password +
/// db lookup), not the fast 400-bad-input branch.
#[test]
fn login_sixth_request_returns_429_empty_body() {
    let server = spawn_team_server();
    let client = http_client();

    // Register a real account so login can theoretically succeed.
    let reg = client
        .post(format!("{}/users/register", server.base_url()))
        .json(&serde_json::json!({
            "username": "alice",
            "password": "correct horse battery staple",
        }))
        .send()
        .expect("POST /users/register");
    assert_eq!(reg.status().as_u16(), 201);

    let mut statuses = Vec::with_capacity(6);
    let payload = serde_json::json!({
        "username": "alice",
        "password": "wrong-on-purpose",
    });
    for i in 0..6 {
        let resp = client
            .post(format!("{}/auth/login", server.base_url()))
            .json(&payload)
            .send()
            .expect("POST /auth/login");
        let status = resp.status().as_u16();
        let body = resp.text().expect("body");
        statuses.push((status, body));
        // Tight loop to stay inside the 1-minute window.
        std::thread::sleep(Duration::from_millis(2));
        if i < 5 {
            assert_eq!(
                statuses[i].0,
                401,
                "request {} (within quota) must be 401 invalid_credentials; got {:?}",
                i + 1,
                statuses[i]
            );
        }
    }
    let (status6, body6) = &statuses[5];
    assert_eq!(
        *status6, 429,
        "6th login request from same IP must be 429; got {status6}, body={body6:?}",
    );
    assert!(
        body6.is_empty(),
        "429 body must be empty (PLANNING §2.3 item 6); got {body6:?}",
    );
}

/// PLANNING §2.3 item 6: `/api/community/upload` 10/hour/user. The 11th
/// upload from the same Bearer-authenticated user is 429 + empty body.
/// We don't even bother sending a valid multipart body — the limiter
/// runs before the route handler, so the bucket counts regardless of
/// whether the request would have succeeded business-logic-wise. This
/// is important: an attacker hammering the endpoint with garbage gets
/// rate-limited too.
#[test]
fn upload_eleventh_request_returns_429_empty_body() {
    let server = spawn_team_server();
    let client = http_client();

    // Register a user so we have a Bearer to send.
    let reg = client
        .post(format!("{}/users/register", server.base_url()))
        .json(&serde_json::json!({
            "username": "uploader",
            "password": "correct horse battery staple",
        }))
        .send()
        .expect("POST /users/register");
    assert_eq!(reg.status().as_u16(), 201);
    let reg_body: serde_json::Value = reg.json().expect("register body");
    let api_key = reg_body["api_key"].as_str().unwrap().to_string();

    let mut last_status_in_quota = 0u16;
    let start = Instant::now();
    for i in 0..11 {
        let resp = client
            .post(format!("{}/api/community/upload", server.base_url()))
            .bearer_auth(&api_key)
            // Deliberately empty / malformed body — we only care about
            // whether the limiter counted us.
            .body("")
            .send()
            .expect("POST /api/community/upload");
        let status = resp.status().as_u16();
        let body = resp.text().expect("body");
        if i < 10 {
            // Any non-429 is fine here — could be 400 (no multipart), 401,
            // 500 — what matters is the limiter let us through.
            assert_ne!(
                status,
                429,
                "request {} should pass the limiter (under quota); got 429 body={body:?}",
                i + 1
            );
            last_status_in_quota = status;
        } else {
            assert_eq!(
                status, 429,
                "11th upload from same user must be 429; got {status} body={body:?}",
            );
            assert!(
                body.is_empty(),
                "429 body must be empty (PLANNING §2.3 item 6); got {body:?}",
            );
        }
    }
    // Sanity: the first ten responses should all have been *something*
    // (the limiter didn't lie about letting them through).
    assert!(
        last_status_in_quota > 0,
        "should have seen at least one non-429 status before the cliff",
    );
    // Make sure we finished within the hour window — if we somehow took
    // an hour to make 10 HTTP calls, the test is meaningless.
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "upload limit test took too long ({:?}) — bucket may have rolled over",
        start.elapsed()
    );
}

/// PLANNING §2.3 item 6: `/skills/get/*` 20/sec/IP. We hammer a
/// non-existent skill name to keep the handler cheap (`resolve_skill_dir`
/// bails fast) and assert we see at least one 429 inside a 25-request
/// burst. The exact request that crosses the cliff isn't fixed because
/// CI workers vary in scheduling and we don't want a flaky assertion.
#[test]
fn skills_get_burst_hits_429_within_one_second() {
    let server = spawn_team_server();
    let client = http_client();

    let mut saw_429 = false;
    let start = Instant::now();
    for _ in 0..25 {
        let resp = client
            .post(format!(
                "{}/skills/get/no-such-skill-xyz-{}",
                server.base_url(),
                std::process::id()
            ))
            .send()
            .expect("POST /skills/get");
        if resp.status().as_u16() == 429 {
            saw_429 = true;
            let body = resp.text().expect("body");
            assert!(
                body.is_empty(),
                "429 body must be empty (PLANNING §2.3 item 6); got {body:?}",
            );
            break;
        }
    }
    assert!(
        saw_429,
        "25 /skills/get requests in {:?} should have tripped the 20/sec limit",
        start.elapsed()
    );
}

/// Regression: the fixed-window index must advance after the one-second
/// `/skills/get/*` window. A previous implementation keyed every request
/// into window 0, so once a process served 20 get calls, all later skill
/// activations from that IP returned 429 until restart.
#[test]
fn skills_get_limit_recovers_after_one_second_window() {
    let server = spawn_team_server();
    let client = http_client();
    let url = format!(
        "{}/skills/get/no-such-skill-window-rollover-{}",
        server.base_url(),
        std::process::id()
    );

    for i in 0..20 {
        let resp = client.post(&url).send().expect("POST /skills/get");
        assert_ne!(
            resp.status().as_u16(),
            429,
            "request {} should pass the limiter before the cliff",
            i + 1
        );
    }

    let limited = client.post(&url).send().expect("POST /skills/get");
    assert_eq!(
        limited.status().as_u16(),
        429,
        "21st request must hit the limiter before the window rolls",
    );
    let limited_body = limited.text().expect("limited body");
    assert!(
        limited_body.is_empty(),
        "429 body must be empty (PLANNING §2.3 item 6); got {limited_body:?}",
    );

    std::thread::sleep(Duration::from_millis(1100));

    let recovered = client.post(&url).send().expect("POST /skills/get");
    let status = recovered.status().as_u16();
    let body = recovered.text().expect("recovered body");
    assert_eq!(
        status, 404,
        "after the one-second window rolls, the request must reach the handler; body={body:?}",
    );
    assert!(
        body.contains("skill not found:"),
        "recovered response must be the handler's 404 body, got {body:?}",
    );
}
