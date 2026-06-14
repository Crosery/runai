//! PLANNING §1.4 C9e — GET /api/users/me/skills (list-mine).
//!
//! Each row carries: name, description, uploaded_at, enrich_status
//! (done / pending), publish_status (draft / pending / approved /
//! rejected), publish_reason (Some when rejected).
//!
//! Auth: 401 without bearer; 200 + empty list for fresh user; isolation:
//! alice's upload does NOT appear in bob's list-mine.

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
    let boundary = format!("----lm-{}", actor.user_id);
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
    assert_eq!(r.status().as_u16(), 200, "upload prereq failed");
}

#[test]
fn list_mine_requires_auth() {
    let s = spawn_server();
    let r = http()
        .get(format!("{}/api/users/me/skills", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn list_mine_empty_for_fresh_user() {
    let s = spawn_server();
    let alice = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/users/me/skills", s.base_url()))
        .bearer_auth(&alice.api_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert_eq!(b["total"].as_u64().unwrap(), 0);
    assert!(b["items"].as_array().unwrap().is_empty());
}

#[test]
fn list_mine_returns_uploaded_rows_with_workflow_state() {
    let s = spawn_server();
    let alice = register(&s, "alice", "pw alice 1234");
    upload(
        &s,
        &alice,
        "first-skill",
        "---\nname: first-skill\ndescription: my first\n---\n\nbody\n",
    );
    upload(
        &s,
        &alice,
        "second-skill",
        "---\nname: second-skill\ndescription: my second\n---\n\nbody\n",
    );
    let r = http()
        .get(format!("{}/api/users/me/skills", s.base_url()))
        .bearer_auth(&alice.api_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    let items = b["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    for it in items {
        // Each row carries the contract fields.
        assert!(it["name"].as_str().is_some());
        assert!(it["uploaded_at"].as_i64().is_some());
        assert!(it["enrich_status"].is_string());
        // Fresh upload in sandbox → enrich never ran → pending.
        assert_eq!(it["enrich_status"].as_str().unwrap(), "pending");
        assert_eq!(it["publish_status"].as_str().unwrap(), "draft");
    }
}

#[test]
fn list_mine_isolates_uploaders() {
    let s = spawn_server();
    let alice = register(&s, "alice", "pw alice 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    upload(
        &s,
        &alice,
        "alice-only",
        "---\nname: alice-only\ndescription: x\n---\nbody\n",
    );
    let r = http()
        .get(format!("{}/api/users/me/skills", s.base_url()))
        .bearer_auth(&bob.api_key)
        .send()
        .unwrap();
    let b: Value = r.json().unwrap();
    assert_eq!(b["total"].as_u64().unwrap(), 0);
    assert!(b["items"].as_array().unwrap().is_empty());
}
