//! PLANNING §1.4 C9c — publish-request endpoint.
//!
//! POST /api/users/me/skills/{name}/publish-request
//!   - Caller must own the skill (private row with owner_user_id = caller).
//!   - Pre-condition: enrich must be done (resource_ai_summary row non-empty)
//!     — until then publish-request returns 400 with a clear message.
//!   - On success: publish_status row flips draft → pending.
//!   - 401 / 404 / 400 contract pinned here.
//!
//! The "happy path enrich completes + publish-request succeeds" assertion
//! lives in manual / real-binary verification — the test spawns a fresh
//! server with a clean tempdir, so `runai recommend enrich --name X`
//! has no provider and never produces a summary. The gate is what we
//! protect at the wire layer.

#![cfg(not(target_os = "windows"))]

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use flate2::write::GzEncoder;
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
fn spawn_server() -> ServerGuard {
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
    assert!(wait_for_port(g.port, Duration::from_secs(8)));
    g
}
fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}
struct Account {
    user_id: String,
    api_key: String,
}
fn register(s: &ServerGuard, u: &str, p: &str) -> Account {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let b: Value = r.json().unwrap();
    Account {
        user_id: b["user_id"].as_str().unwrap().into(),
        api_key: b["api_key"].as_str().unwrap().into(),
    }
}
fn make_bundle(name: &str, skill_md: &str) -> Vec<u8> {
    let buf: Vec<u8> = Vec::new();
    let gz = GzEncoder::new(buf, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let mut dh = tar::Header::new_gnu();
    dh.set_entry_type(tar::EntryType::Directory);
    dh.set_mode(0o755);
    dh.set_size(0);
    dh.set_cksum();
    tar.append_data(&mut dh, format!("{name}/"), std::io::empty())
        .unwrap();
    let mut fh = tar::Header::new_gnu();
    fh.set_size(skill_md.len() as u64);
    fh.set_mode(0o644);
    fh.set_cksum();
    tar.append_data(&mut fh, format!("{name}/SKILL.md"), skill_md.as_bytes())
        .unwrap();
    let gz = tar.into_inner().unwrap();
    let mut out = gz.finish().unwrap();
    out.flush().unwrap();
    out
}
fn multipart_body(name: &str, bundle: &[u8], boundary: &str) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    b.extend_from_slice(b"Content-Disposition: form-data; name=\"name\"\r\n\r\n");
    b.extend_from_slice(name.as_bytes());
    b.extend_from_slice(b"\r\n");
    b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    b.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"bundle\"; filename=\"{name}.tar.gz\"\r\n")
            .as_bytes(),
    );
    b.extend_from_slice(b"Content-Type: application/gzip\r\n\r\n");
    b.extend_from_slice(bundle);
    b.extend_from_slice(b"\r\n");
    b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    b
}
fn upload(s: &ServerGuard, actor: &Account, name: &str, skill_md: &str) {
    let bundle = make_bundle(name, skill_md);
    let boundary = format!("----pubreq-{}", actor.user_id);
    let body = multipart_body(name, &bundle, &boundary);
    let r = http()
        .post(format!("{}/api/users/me/skills/upload", s.base_url()))
        .bearer_auth(&actor.api_key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "upload prerequisite failed");
}

// ─── auth ────────────────────────────────────────────────────────────

#[test]
fn publish_request_requires_auth() {
    let s = spawn_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!(
            "{}/api/users/me/skills/anything/publish-request",
            s.base_url()
        ))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

// ─── unknown skill ──────────────────────────────────────────────────

#[test]
fn publish_request_unknown_skill_returns_404() {
    let s = spawn_server();
    let alice = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!(
            "{}/api/users/me/skills/no-such-skill/publish-request",
            s.base_url()
        ))
        .bearer_auth(&alice.api_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
}

// ─── enrich pre-condition ───────────────────────────────────────────

#[test]
fn publish_request_blocks_when_enrich_not_done() {
    let s = spawn_server();
    let alice = register(&s, "alice", "pw alice 1234");
    upload(
        &s,
        &alice,
        "my-skill",
        "---\nname: my-skill\ndescription: t\n---\n\nbody\n",
    );
    // In the test sandbox, `recommend enrich` has no provider configured
    // so resource_ai_summary stays empty. The gate must 400.
    let r = http()
        .post(format!(
            "{}/api/users/me/skills/my-skill/publish-request",
            s.base_url()
        ))
        .bearer_auth(&alice.api_key)
        .send()
        .unwrap();
    let status = r.status().as_u16();
    let body = r.text().unwrap_or_default();
    assert_eq!(
        status, 400,
        "must reject publish-request when enrich is not complete; body: {body}"
    );
    assert!(
        body.contains("富集") || body.to_lowercase().contains("enrich"),
        "error message should mention enrich state; got {body}"
    );
}

// ─── ownership guard ────────────────────────────────────────────────

#[test]
fn publish_request_cannot_target_other_users_skill() {
    let s = spawn_server();
    let alice = register(&s, "alice", "pw alice 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    upload(
        &s,
        &alice,
        "alice-only",
        "---\nname: alice-only\ndescription: t\n---\nbody\n",
    );
    // bob asks to publish alice's skill — must NOT find it under bob's owner
    // scope, returns 404 to avoid leaking the existence of someone else's
    // private skill name (the userlib admin pane is the only place that
    // can see cross-user privates).
    let r = http()
        .post(format!(
            "{}/api/users/me/skills/alice-only/publish-request",
            s.base_url()
        ))
        .bearer_auth(&bob.api_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
}
