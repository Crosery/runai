//! Issue #26 — `POST /feedback` had zero auth: any anonymous caller could
//! trigger `reevaluate_skill` (an LLM call, real token cost) against any
//! skill, and the "who did this" field trusted a fully client-controlled
//! `X-Runai-User` header.
//!
//! Fix under test (`src/server/recommend.rs::handle_feedback`):
//!   - Anonymous callers (no Bearer / no session cookie) get `401` with an
//!     EMPTY body — aligned with the anti-enumeration style used elsewhere
//!     (`empty_404`, the `/recommend` fail-closed-Bearer lane), not the
//!     JSON `{"error":"unauthorized"}` shape `ApiError::Unauthorized`
//!     produces for the dashboard JSON API.
//!   - A forged `X-Runai-User` header grants nothing on its own — same
//!     401 as no header at all.
//!   - Once authenticated (valid Bearer), the "applied by" attribution in
//!     the response text is the AUTHENTICATED username, never the
//!     caller-supplied header value (which is not even read anymore).
//!
//! Owner mode (the default `runai server` with no `--mode` flag) already
//! auto-authenticates every request as the synthetic implicit-admin user
//! (PLANNING §1.1), so it is intentionally NOT re-tested here — that
//! contract is covered by the pre-existing `tests/router_skill_lifecycle_p1c0.rs`
//! Feature-3 block, which calls `/feedback` with zero auth headers and
//! expects it to reach business logic (400, not 401). This file only
//! covers the NEW team-mode gate.

#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};
use tempfile::TempDir;

// ─── generic server harness (mirrors tests/private_skill_upload_e2e.rs) ────

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
        thread::sleep(Duration::from_millis(50));
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
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `runai server --mode team` against a fresh isolated HOME. Never
/// touches the real `~/.runai/`.
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
    let g = ServerGuard { child, home, port };
    assert!(
        wait_for_port(g.port, Duration::from_secs(8)),
        "runai server (team mode) never came up"
    );
    g
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

struct Account {
    api_key: String,
    username: String,
}

fn register(s: &ServerGuard, u: &str, p: &str) -> Account {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        201,
        "register must succeed: {}",
        r.text().unwrap_or_default()
    );
    let b: Value = r.json().unwrap();
    Account {
        api_key: b["api_key"].as_str().unwrap().into(),
        username: u.to_string(),
    }
}

/// Plant a SKILL.md and run `runai scan` (subprocess, same HOME) so the
/// resource row exists before the server (a separate process) opens the
/// DB per-request.
fn plant_and_scan(home: &std::path::Path, name: &str, desc: &str) {
    let dir = home.join(".runai/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {desc}\n---\n\n# {name}\n\n{desc}\n"),
    )
    .unwrap();
    let out = runai_cmd()
        .arg("scan")
        .env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "scan must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Write `<home>/.runai/config.toml` pointing recommend at a mock LLM base
/// url, mirroring `tests/recommend_feedback_e2e_p1c0.rs::TestEnv::write_config`.
fn write_recommend_config(home: &std::path::Path, base_url: &str) {
    let toml = format!(
        "[recommend]\n\
         enabled = true\n\
         provider = \"openai-compat\"\n\
         base_url = \"{base_url}\"\n\
         model = \"mock-model\"\n\
         api_key = \"mock-key\"\n\
         top_k = 8\n\
         min_prompt_len = 0\n\
         summary_lang = \"en\"\n"
    );
    std::fs::write(home.join(".runai/config.toml"), toml).unwrap();
}

// ─── tiny mock LLM (mirrors tests/recommend_feedback_e2e_p1c0.rs::MockLlm) ──

struct MockLlm {
    base_url: String,
    shutdown: Arc<AtomicBool>,
}

impl MockLlm {
    fn start(content: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock LLM");
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let body_content = content.to_string();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(2000)))
                            .ok();
                        let mut buf = [0u8; 8192];
                        let mut total = Vec::new();
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    total.extend_from_slice(&buf[..n]);
                                    if total.windows(4).any(|w| w == b"\r\n\r\n") {
                                        let _ = stream.read(&mut buf);
                                        break;
                                    }
                                    if total.len() > 1 << 20 {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let escaped = serde_json::to_string(&body_content).unwrap();
                        let json_body = format!(
                            "{{\"id\":\"mock-1\",\"object\":\"chat.completion\",\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":{escaped}}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}}}"
                        );
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            json_body.len(),
                            json_body
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Self { base_url, shutdown }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for MockLlm {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// Anonymous POST /feedback (team mode, no Bearer, no session cookie) must
/// be rejected with 401 and a completely empty body — not the JSON
/// `{"error":"unauthorized"}` shape used by dashboard `/api/*` endpoints.
#[test]
fn feedback_anonymous_returns_401_empty_body() {
    let s = spawn_team_server();
    let r = http()
        .post(format!("{}/feedback", s.base_url()))
        .json(&json!({"skill": "does-not-exist", "note": "anon probe"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401, "anonymous feedback must be 401");
    let body = r.text().unwrap();
    assert!(
        body.is_empty(),
        "401 body must be empty (anti-enumeration style), got: {body:?}"
    );
}

/// A forged `X-Runai-User` header with NO Bearer must not grant access —
/// same 401 + empty body as the fully anonymous case. Locks that the
/// header is not a trust boundary.
#[test]
fn feedback_forged_header_without_bearer_still_401() {
    let s = spawn_team_server();
    let r = http()
        .post(format!("{}/feedback", s.base_url()))
        .header("X-Runai-User", "root@admin-box")
        .json(&json!({"skill": "does-not-exist", "note": "forged header probe"}))
        .send()
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        401,
        "forged X-Runai-User header alone must still be rejected"
    );
    assert!(
        r.text().unwrap().is_empty(),
        "401 body must stay empty even with a forged header present"
    );
}

/// Valid Bearer must pass the auth gate AND the attribution in the
/// response text must be the authenticated username — never the
/// caller-supplied (and here deliberately forged, different-looking)
/// `X-Runai-User` header value.
#[test]
fn feedback_valid_bearer_succeeds_and_uses_authenticated_identity_not_forged_header() {
    let s = spawn_team_server();
    let alice = register(&s, "alice", "pw alice correct horse");

    let mock = MockLlm::start(
        "task: refined task line for alpha\n\
         triggers: alpha, beta, gamma\n\
         inputs: text\n\
         outputs: text\n\
         not-for: unsuitable cases\n\
         score: 3\n",
    );
    write_recommend_config(s.home.path(), mock.base_url());
    plant_and_scan(s.home.path(), "alpha", "alpha skill description");

    let r = http()
        .post(format!("{}/feedback", s.base_url()))
        .bearer_auth(&alice.api_key)
        .header("X-Runai-User", "totally-fake-identity@evil-host")
        .json(&json!({"skill": "alpha", "note": "too general"}))
        .send()
        .unwrap();

    let status = r.status().as_u16();
    let body = r.text().unwrap();
    assert_eq!(
        status, 200,
        "authenticated feedback with a real skill + configured LLM must succeed, got {status}: {body}"
    );
    assert!(
        body.contains(&format!("feedback applied by {}", alice.username)),
        "response must attribute the change to the AUTHENTICATED username 'alice', got: {body:?}"
    );
    assert!(
        !body.contains("totally-fake-identity"),
        "response must NEVER echo the forged X-Runai-User header value, got: {body:?}"
    );
}
