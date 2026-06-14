//! PLANNING §1.7 — Setup tab content is mode + role aware.
//!
//! Pins three contracts at the wire format level:
//!   1. `{SERVER_URL}` placeholder is substituted with the actual request
//!      origin so users can copy-paste commands verbatim.
//!   2. Admin (or owner-mode synthetic owner) gets ADMIN-only sections;
//!      USER-only sections are stripped.
//!   3. Plain user (or anonymous in team mode) gets USER-only sections;
//!      ADMIN-only sections are stripped. Marker comment lines never leak.

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
fn spawn_server(mode: &str) -> ServerGuard {
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
        .arg(mode)
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
    assert!(wait_for_port(g.port, Duration::from_secs(8)));
    g
}
fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
fn register(s: &ServerGuard, u: &str, p: &str) -> String {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let b: Value = r.json().unwrap();
    b["api_key"].as_str().unwrap().to_string()
}

// ─── {SERVER_URL} substitution ───────────────────────────────────────

#[test]
fn setup_md_substitutes_server_url_placeholder() {
    let s = spawn_server("owner");
    let r = http()
        .get(format!("{}/setup.md", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let body = r.text().unwrap();
    assert!(
        !body.contains("{SERVER_URL}"),
        "{{SERVER_URL}} placeholder must be substituted, got body:\n{body}"
    );
}

#[test]
fn setup_md_uses_request_host_not_lan_ip() {
    // The Setup tab is consumed by the SAME user who reached the
    // dashboard — if they reached `127.0.0.1:17888`, the commands they
    // copy must hit `127.0.0.1:17888`. The old guess_server_url
    // translated loopback Host to LAN IP, which broke `curl` when the
    // server was bound to loopback (LAN IP not listening). This pins
    // the fix: setup substitution echoes the request host verbatim.
    //
    // Use team mode + anonymous so the user variant (which contains the
    // {SERVER_URL} substitution in the curl install command) is what
    // gets rendered.
    let s = spawn_server("team");
    let body = http()
        .get(format!("{}/setup.md", s.base_url()))
        .send()
        .unwrap()
        .text()
        .unwrap();
    let expected_origin = format!("http://127.0.0.1:{}", s.port);
    assert!(
        body.contains(&expected_origin),
        "setup.md must contain the request origin verbatim ({expected_origin}); body excerpt: {}",
        body.lines().take(20).collect::<Vec<_>>().join("\n")
    );
}

// ─── owner mode → admin variant ─────────────────────────────────────

#[test]
fn owner_mode_serves_admin_setup() {
    // Owner mode short-circuits current_user to a synthetic admin, so the
    // setup page should be the admin variant: contains "启动 server" /
    // "公共池" / "垃圾桶", DOES NOT contain the user-only "装客户端 +
    // 配 hook" section header.
    let s = spawn_server("owner");
    let r = http()
        .get(format!("{}/setup.md", s.base_url()))
        .send()
        .unwrap();
    let body = r.text().unwrap();
    assert!(
        body.contains("启动 server"),
        "owner mode setup must include admin 启动 server section"
    );
    assert!(
        body.contains("runai install owner/repo"),
        "owner mode setup must include admin CLI install command"
    );
    assert!(
        !body.contains("装客户端 + 配 hook"),
        "owner mode setup must NOT include user-only 装客户端 + 配 hook section"
    );
    // The marker comments must never leak through to the rendered output.
    assert!(
        !body.contains("<!-- runai:admin-only-"),
        "raw marker lines must be stripped: still in body"
    );
    assert!(
        !body.contains("<!-- runai:user-only-"),
        "raw marker lines must be stripped: still in body"
    );
}

// ─── team mode + admin bearer → admin variant ────────────────────────

#[test]
fn team_mode_admin_bearer_serves_admin_setup() {
    let s = spawn_server("team");
    let admin = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/setup.md", s.base_url()))
        .bearer_auth(&admin)
        .send()
        .unwrap();
    let body = r.text().unwrap();
    assert!(body.contains("启动 server"));
    assert!(body.contains("runai install owner/repo"));
    assert!(
        !body.contains("装客户端 + 配 hook"),
        "team admin must not see the user-only client install section"
    );
}

// ─── team mode + user bearer → user variant ──────────────────────────

#[test]
fn team_mode_user_bearer_serves_user_setup() {
    let s = spawn_server("team");
    let _admin = register(&s, "admin", "pw admin 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    let r = http()
        .get(format!("{}/setup.md", s.base_url()))
        .bearer_auth(&bob)
        .send()
        .unwrap();
    let body = r.text().unwrap();
    assert!(
        body.contains("装客户端 + 配 hook"),
        "regular user must see the client install section"
    );
    assert!(
        !body.contains("启动 server"),
        "regular user must NOT see the admin 启动 server section"
    );
    assert!(
        !body.contains("runai install owner/repo"),
        "regular user must NOT see the admin CLI install command (公共池 install)"
    );
    assert!(!body.contains("<!-- runai:admin-only-"));
    assert!(!body.contains("<!-- runai:user-only-"));
}

// ─── team mode + no cred → user variant ──────────────────────────────

#[test]
fn team_mode_anonymous_serves_user_setup() {
    let s = spawn_server("team");
    let r = http()
        .get(format!("{}/setup.md", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let body = r.text().unwrap();
    assert!(
        body.contains("装客户端 + 配 hook"),
        "anonymous visitor must see the user-flavored setup so they learn to register"
    );
    assert!(!body.contains("启动 server"));
}

// ─── common section (三个池子) is in BOTH variants ───────────────────

#[test]
fn setup_md_common_section_in_both_variants() {
    let s = spawn_server("team");
    let admin = register(&s, "alice", "pw alice 1234");

    let r_admin = http()
        .get(format!("{}/setup.md", s.base_url()))
        .bearer_auth(&admin)
        .send()
        .unwrap();
    let admin_body = r_admin.text().unwrap();
    assert!(admin_body.contains("三个池子"));
    assert!(admin_body.contains("allow_public_recommend"));

    let r_user = http()
        .get(format!("{}/setup.md", s.base_url()))
        .send()
        .unwrap();
    let user_body = r_user.text().unwrap();
    assert!(user_body.contains("三个池子"));
    assert!(user_body.contains("allow_public_recommend"));
}
