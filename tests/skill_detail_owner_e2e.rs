//! Admin-gated `?owner=<uid>` resolution for skill detail / files / file.
//!
//! Background: the dashboard "用户库" (admin) drills into a specific user's
//! private skill. The detail pane reuses `/api/skill/{name}` + `/files` +
//! `/file`. Without an explicit owner those endpoints resolve ownership from
//! the *viewer*:
//!   - `/api/skill/{name}` uses `"*"` for admin → with two users owning a
//!     same-named private skill it picks the freshest, not the one clicked.
//!   - `/api/skill/{name}/files` + `/file` use `resolve_skill_dir` →
//!     `current_owner_id` (the admin's OWN uid) → can't find another user's
//!     private skill at all → broken file tree.
//!
//! Contract pinned here:
//!   - admin `?owner=<uid>` resolves EXACTLY that user's private skill
//!     (detail content + files skill_dir under `<data>/users/<uid>/skills/`).
//!   - same-named privates across users disambiguate by `?owner=`.
//!   - a non-admin's `?owner=` is IGNORED (no privilege escalation) — they
//!     keep resolving within their own scope.

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
fn upload_private(s: &ServerGuard, actor: &Account, name: &str, skill_md: &str) {
    let bundle = make_bundle(name, skill_md);
    let boundary = format!("----runai-{}-{name}", actor.user_id);
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
    assert_eq!(r.status().as_u16(), 200, "private upload failed");
}

// ─── detail: admin ?owner= resolves the exact user's private skill ────────

#[test]
fn admin_owner_param_resolves_exact_user_private_detail() {
    let s = spawn_server();
    let admin = register(&s, "admin", "pw admin 1234");
    let alice = register(&s, "alice", "pw alice 1234");
    let bob = register(&s, "bob", "pw bob 1234");

    // Same skill NAME owned by two different users with distinct content.
    upload_private(
        &s,
        &alice,
        "shared",
        "---\nname: shared\ndescription: alice copy\n---\nALICE_BODY_MARKER\n",
    );
    upload_private(
        &s,
        &bob,
        "shared",
        "---\nname: shared\ndescription: bob copy\n---\nBOB_BODY_MARKER\n",
    );

    // admin ?owner=alice → alice's copy
    let r = http()
        .get(format!(
            "{}/api/skill/shared?owner={}",
            s.base_url(),
            alice.user_id
        ))
        .bearer_auth(&admin.api_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert!(
        b["skill_md_content"]
            .as_str()
            .unwrap()
            .contains("ALICE_BODY_MARKER"),
        "owner=alice must return alice's SKILL.md, got {b}"
    );
    assert!(
        b["skill_md_path"]
            .as_str()
            .unwrap()
            .contains(&alice.user_id),
        "skill_md_path must live under alice's user dir"
    );

    // admin ?owner=bob → bob's copy
    let r = http()
        .get(format!(
            "{}/api/skill/shared?owner={}",
            s.base_url(),
            bob.user_id
        ))
        .bearer_auth(&admin.api_key)
        .send()
        .unwrap();
    let b: Value = r.json().unwrap();
    assert!(
        b["skill_md_content"]
            .as_str()
            .unwrap()
            .contains("BOB_BODY_MARKER"),
        "owner=bob must return bob's SKILL.md, got {b}"
    );
    assert!(b["skill_md_path"].as_str().unwrap().contains(&bob.user_id));
}

// ─── files: admin ?owner= points the file tree at the user's private dir ──

#[test]
fn admin_owner_param_resolves_exact_user_private_files() {
    let s = spawn_server();
    let admin = register(&s, "admin", "pw admin 1234");
    let alice = register(&s, "alice", "pw alice 1234");

    upload_private(
        &s,
        &alice,
        "filetest",
        "---\nname: filetest\ndescription: x\n---\nbody\n",
    );

    let r = http()
        .get(format!(
            "{}/api/skill/filetest/files?owner={}",
            s.base_url(),
            alice.user_id
        ))
        .bearer_auth(&admin.api_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "files endpoint must resolve");
    let b: Value = r.json().unwrap();
    assert!(
        b["skill_dir"].as_str().unwrap().contains(&alice.user_id),
        "skill_dir must be under alice's private subtree, got {b}"
    );
    let entries = b["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|e| e["path"] == "SKILL.md"),
        "file tree must list SKILL.md"
    );
}

// ─── non-admin ?owner= is ignored (no escalation) ─────────────────────────

#[test]
fn non_admin_owner_param_is_ignored() {
    let s = spawn_server();
    let _admin = register(&s, "admin", "pw admin 1234");
    let alice = register(&s, "alice", "pw alice 1234");
    let bob = register(&s, "bob", "pw bob 1234");

    // alice + bob both own a same-named private skill with distinct content.
    upload_private(
        &s,
        &alice,
        "secret",
        "---\nname: secret\ndescription: alice\n---\nALICE_SECRET\n",
    );
    upload_private(
        &s,
        &bob,
        "secret",
        "---\nname: secret\ndescription: bob\n---\nBOB_SECRET\n",
    );

    // bob (non-admin) tries to peek at alice's copy via ?owner=alice.
    // The param must be ignored: bob keeps his own scope and sees HIS copy.
    let r = http()
        .get(format!(
            "{}/api/skill/secret?owner={}",
            s.base_url(),
            alice.user_id
        ))
        .bearer_auth(&bob.api_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert!(
        b["skill_md_content"]
            .as_str()
            .unwrap()
            .contains("BOB_SECRET"),
        "non-admin ?owner= must be ignored — bob must see his own copy, got {b}"
    );
    assert!(
        !b["skill_md_content"]
            .as_str()
            .unwrap()
            .contains("ALICE_SECRET"),
        "non-admin must NOT be able to read another user's private skill"
    );
    assert!(b["skill_md_path"].as_str().unwrap().contains(&bob.user_id));
}
