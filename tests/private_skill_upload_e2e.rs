//! PLANNING §1.4 (rewrite) — private skill upload + publish-status field.
//!
//! New endpoint: POST /api/users/me/skills/upload (multipart name + bundle)
//! - Lands the skill at <data>/users/<uid>/skills/<name>/ (NOT community).
//! - Stamps owner_user_id = uid on the resources row.
//! - Sets publish_status = 'draft' so the publish-request workflow knows
//!   this is a fresh upload that hasn't been submitted for community yet.
//!
//! Contract:
//!   - 401 with no credential
//!   - 200 + json {name, uploader_uid, bytes} on valid upload
//!   - 400 on malformed name / missing SKILL.md / empty bundle
//!   - File exists at <data>/users/<uid>/skills/<name>/SKILL.md
//!   - DB row exists with publish_status = 'draft' and owner_user_id = uid

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
    let g = ServerGuard { child, home, port };
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

fn do_upload(
    s: &ServerGuard,
    actor: &Account,
    name: &str,
    skill_md: &str,
) -> reqwest::blocking::Response {
    let bundle = make_bundle(name, skill_md);
    let boundary = format!("----runai-pvt-{}", actor.user_id);
    let body = multipart_body(name, &bundle, &boundary);
    http()
        .post(format!("{}/api/users/me/skills/upload", s.base_url()))
        .bearer_auth(&actor.api_key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .unwrap()
}

// ─── auth ────────────────────────────────────────────────────────────

#[test]
fn private_upload_requires_auth() {
    let s = spawn_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let bundle = make_bundle("foo", "# foo\nplaceholder\n");
    let boundary = "----runai-test";
    let body = multipart_body("foo", &bundle, boundary);
    let r = http()
        .post(format!("{}/api/users/me/skills/upload", s.base_url()))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

// ─── happy path ──────────────────────────────────────────────────────

#[test]
fn private_upload_succeeds_lands_under_user_dir_with_publish_status_draft() {
    let s = spawn_server();
    let alice = register(&s, "alice", "pw alice 1234");
    let skill_md = "---\nname: my-skill\ndescription: A test skill for upload\n---\n\nbody\n";
    let r = do_upload(&s, &alice, "my-skill", skill_md);
    let status = r.status().as_u16();
    let body: Value = r.json().unwrap_or(json!({}));
    assert_eq!(status, 200, "upload failed: {body}");
    assert_eq!(body["name"].as_str().unwrap(), "my-skill");
    assert_eq!(body["uploader_uid"].as_str().unwrap(), alice.user_id);
    assert!(body["bytes"].as_u64().unwrap() > 0);

    // Physical layout: <home>/.runai/users/<uid>/skills/my-skill/SKILL.md
    let skill_md_path = s
        .home
        .path()
        .join(".runai/users")
        .join(&alice.user_id)
        .join("skills/my-skill/SKILL.md");
    assert!(
        skill_md_path.exists(),
        "SKILL.md must land under user dir, expected {skill_md_path:?}"
    );

    // Verify publish_status = 'draft' via /api/users/me/skills (which C9e
    // will add). For now, hit /api/me + assert the upload returns
    // publish_status in its response body.
    assert_eq!(
        body["publish_status"].as_str().unwrap_or(""),
        "draft",
        "fresh upload must have publish_status='draft', got {body}"
    );
}

// ─── input validation ────────────────────────────────────────────────

#[test]
fn private_upload_rejects_empty_name() {
    let s = spawn_server();
    let alice = register(&s, "alice", "pw alice 1234");
    // Build a valid bundle (so tar doesn't choke), but send empty name
    // in the multipart `name` field. Server must 400 on the name check.
    let bundle = make_bundle("placeholder", "# skill\n");
    let boundary = "----runai-empty-name";
    let body = multipart_body("", &bundle, boundary);
    let r = http()
        .post(format!("{}/api/users/me/skills/upload", s.base_url()))
        .bearer_auth(&alice.api_key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
}

#[test]
fn private_upload_rejects_bundle_without_skill_md() {
    let s = spawn_server();
    let alice = register(&s, "alice", "pw alice 1234");
    // Bundle that contains a different file instead of SKILL.md.
    let buf: Vec<u8> = Vec::new();
    let gz = GzEncoder::new(buf, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let mut h = tar::Header::new_gnu();
    h.set_size(3);
    h.set_mode(0o644);
    h.set_cksum();
    tar.append_data(&mut h, "notskill/README.md", &b"hi\n"[..])
        .unwrap();
    let gz = tar.into_inner().unwrap();
    let bundle = gz.finish().unwrap();
    let boundary = "----runai-noskill";
    let body = multipart_body("noskill", &bundle, boundary);
    let r = http()
        .post(format!("{}/api/users/me/skills/upload", s.base_url()))
        .bearer_auth(&alice.api_key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
}

// ─── isolation: alice's upload doesn't show as bob's owner ─────────────

#[test]
fn private_upload_isolates_owner_user_id_per_user() {
    let s = spawn_server();
    let alice = register(&s, "alice", "pw alice 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    do_upload(
        &s,
        &alice,
        "alice-skill",
        "---\nname: alice-skill\n---\nbody\n",
    );
    do_upload(&s, &bob, "bob-skill", "---\nname: bob-skill\n---\nbody\n");
    // Layout: each user's skill lives under their own subtree.
    assert!(
        s.home
            .path()
            .join(".runai/users")
            .join(&alice.user_id)
            .join("skills/alice-skill/SKILL.md")
            .exists(),
        "alice's skill missing under alice's user dir"
    );
    assert!(
        s.home
            .path()
            .join(".runai/users")
            .join(&bob.user_id)
            .join("skills/bob-skill/SKILL.md")
            .exists(),
        "bob's skill missing under bob's user dir"
    );
    // Cross-contamination guard: alice's skill must NOT exist under bob's
    // dir and vice versa.
    assert!(
        !s.home
            .path()
            .join(".runai/users")
            .join(&bob.user_id)
            .join("skills/alice-skill")
            .exists()
    );
    assert!(
        !s.home
            .path()
            .join(".runai/users")
            .join(&alice.user_id)
            .join("skills/bob-skill")
            .exists()
    );
}
