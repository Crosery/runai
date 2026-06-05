//! Phase §1.4 e2e: community-market endpoints (upload, list, install, delete).
//!
//! Each test spawns the real `runai server` binary in `team` mode inside an
//! isolated HOME so the live `~/.runai/runai.db` is never touched. We drive
//! the dashboard over real HTTP and assert on:
//!
//! - Cross-user discovery (A uploads → B sees it in /api/community/list).
//! - Physical install isolation (B installs A's skill → it lands inside
//!   `<HOME>/.runai/users/<B_uid>/skills/<name>/`, never the public pool,
//!   never under A's per-user subtree).
//! - Permission gating on delete (non-uploader non-admin = 403,
//!   uploader = 200, admin = 200).
//! - Re-upload behaviour (same uploader + name = version bump,
//!   `installs_total` preserved).
//!
//! Pattern mirrors `tests/server_mode_e2e.rs` and `tests/install_script_e2e.rs`.

#![cfg(not(target_os = "windows"))]

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use flate2::write::GzEncoder;
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
    home: TempDir,
    port: u16,
}

impl ServerGuard {
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn home_path(&self) -> &std::path::Path {
        self.home.path()
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
    let guard = ServerGuard { child, home, port };
    assert!(
        wait_for_port(port, Duration::from_secs(8)),
        "runai server did not bind 127.0.0.1:{port} within 8s"
    );
    guard
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client")
}

#[derive(Debug)]
struct Account {
    user_id: String,
    api_key: String,
}

fn register(server: &ServerGuard, username: &str, password: &str) -> Account {
    let client = http_client();
    let resp = client
        .post(format!("{}/users/register", server.base_url()))
        .json(&serde_json::json!({
            "username": username,
            "password": password,
        }))
        .send()
        .expect("POST /users/register");
    assert_eq!(
        resp.status().as_u16(),
        201,
        "register {username}: status={}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().expect("register JSON body");
    Account {
        user_id: body
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        api_key: body
            .get("api_key")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    }
}

/// Build a minimal gz-tar bundle wrapping a single skill directory:
/// `<name>/SKILL.md`. Returns the raw bytes ready to feed the multipart
/// `bundle` field. The archive layout matches what `move_skill_payload`
/// (and `find_skill_root`) expect — a top-level `<name>/` entry.
fn make_bundle_bytes(skill_name: &str, skill_md: &str) -> Vec<u8> {
    let buf: Vec<u8> = Vec::new();
    let gz = GzEncoder::new(buf, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);

    // Top-level skill directory entry.
    let mut dir_header = tar::Header::new_gnu();
    dir_header.set_entry_type(tar::EntryType::Directory);
    dir_header.set_mode(0o755);
    dir_header.set_size(0);
    dir_header.set_cksum();
    tar.append_data(&mut dir_header, format!("{skill_name}/"), std::io::empty())
        .expect("append dir");

    // SKILL.md file.
    let mut header = tar::Header::new_gnu();
    header.set_size(skill_md.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(
        &mut header,
        format!("{skill_name}/SKILL.md"),
        skill_md.as_bytes(),
    )
    .expect("append SKILL.md");

    let gz = tar.into_inner().expect("close tar");
    let mut out = gz.finish().expect("finish gzip");
    out.flush().expect("flush");
    out
}

/// Construct a manual multipart/form-data body for the upload endpoint.
/// (Using `reqwest`'s blocking multipart::Form keeps the test independent
/// of the upstream feature flag — this builder is just bytes.)
fn build_multipart_body(skill_name: &str, bundle: &[u8], boundary: &str) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"name\"\r\n\r\n");
    body.extend_from_slice(skill_name.as_bytes());
    body.extend_from_slice(b"\r\n");

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"bundle\"; filename=\"{skill_name}.tar.gz\"\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: application/gzip\r\n\r\n");
    body.extend_from_slice(bundle);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

fn upload(
    server: &ServerGuard,
    actor: &Account,
    skill_name: &str,
    skill_md: &str,
) -> serde_json::Value {
    let client = http_client();
    let boundary = format!(
        "----runai-test-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let bundle = make_bundle_bytes(skill_name, skill_md);
    let body = build_multipart_body(skill_name, &bundle, &boundary);
    let resp = client
        .post(format!("{}/api/community/upload", server.base_url()))
        .bearer_auth(&actor.api_key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .expect("POST /api/community/upload");
    let status = resp.status().as_u16();
    let json: serde_json::Value = resp.json().unwrap_or(serde_json::json!({}));
    assert_eq!(status, 200, "upload status={status} body={json}");
    json
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[test]
fn alice_uploads_bob_sees_it_in_list() {
    let server = spawn_team_server();
    let alice = register(&server, "alice", "correct horse battery staple");
    let bob = register(&server, "bob", "another long passphrase here");

    upload(
        &server,
        &alice,
        "shared-foo",
        "# shared-foo\nalice wrote this",
    );

    // Bob hits /api/community/list.
    let client = http_client();
    let resp = client
        .get(format!("{}/api/community/list", server.base_url()))
        .bearer_auth(&bob.api_key)
        .send()
        .expect("GET /api/community/list");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().expect("list JSON");
    let items = body["items"].as_array().expect("items array");
    let found = items.iter().any(|it| {
        it["name"].as_str() == Some("shared-foo")
            && it["uploader_uid"].as_str() == Some(alice.user_id.as_str())
    });
    assert!(
        found,
        "bob must see alice's uploaded skill in /list; body={body}"
    );
    assert_eq!(body["total"].as_u64().unwrap_or(0), 1, "total=1");
}

#[test]
fn bob_installs_alice_skill_lands_in_bob_private_pool() {
    let server = spawn_team_server();
    let alice = register(&server, "alice", "correct horse battery staple");
    let bob = register(&server, "bob", "another long passphrase here");

    upload(&server, &alice, "shareable", "# shareable\nfrom alice");

    let client = http_client();
    let resp = client
        .post(format!(
            "{}/api/community/install/{}/{}",
            server.base_url(),
            alice.user_id,
            "shareable"
        ))
        .bearer_auth(&bob.api_key)
        .send()
        .expect("POST /api/community/install");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "install status; body={:?}",
        resp.text()
    );

    // Physical landing site MUST be inside bob's per-user skills dir.
    let bob_skill = server
        .home_path()
        .join(".runai/users")
        .join(&bob.user_id)
        .join("skills/shareable");
    assert!(
        bob_skill.is_dir(),
        "expected bob's private install at {:?}",
        bob_skill
    );
    let content = std::fs::read_to_string(bob_skill.join("SKILL.md")).expect("read SKILL.md");
    assert!(content.contains("from alice"), "payload preserved");

    // Public pool MUST be untouched.
    let public_skill = server.home_path().join(".runai/skills/shareable");
    assert!(
        !public_skill.exists(),
        "community install must NEVER write to the public pool, found {:?}",
        public_skill
    );

    // Alice's per-user dir MUST not gain the skill either — install copies
    // from `community/`, not from any per-user subtree.
    let alice_skill = server
        .home_path()
        .join(".runai/users")
        .join(&alice.user_id)
        .join("skills/shareable");
    assert!(
        !alice_skill.exists(),
        "install must not leak into alice's private pool, found {:?}",
        alice_skill
    );

    // installs_total bumped on the source row.
    let detail = client
        .get(format!(
            "{}/api/community/skill/{}/{}",
            server.base_url(),
            alice.user_id,
            "shareable"
        ))
        .bearer_auth(&bob.api_key)
        .send()
        .expect("GET detail");
    let body: serde_json::Value = detail.json().expect("detail JSON");
    assert_eq!(
        body["installs_total"].as_i64().unwrap_or(0),
        1,
        "installs_total bumped to 1; body={body}"
    );
}

#[test]
fn non_uploader_non_admin_cannot_delete_others_upload() {
    let server = spawn_team_server();
    let alice = register(&server, "alice", "correct horse battery staple");
    let bob = register(&server, "bob", "another long passphrase here");
    // Carol is third user — NOT first, so not auto-admin.
    let carol = register(&server, "carol", "yet another good password!!!");

    upload(&server, &bob, "bob-private", "# bob's skill");

    let client = http_client();
    let resp = client
        .delete(format!(
            "{}/api/community/skill/{}/{}",
            server.base_url(),
            bob.user_id,
            "bob-private"
        ))
        .bearer_auth(&carol.api_key)
        .send()
        .expect("DELETE as carol");
    assert_eq!(
        resp.status().as_u16(),
        403,
        "non-uploader non-admin must 403; got {} body={:?}",
        resp.status(),
        resp.text()
    );

    // Skill still exists.
    let detail = client
        .get(format!(
            "{}/api/community/skill/{}/{}",
            server.base_url(),
            bob.user_id,
            "bob-private"
        ))
        .bearer_auth(&alice.api_key)
        .send()
        .expect("GET detail post-failed-delete");
    assert_eq!(
        detail.status().as_u16(),
        200,
        "skill should still exist after rejected delete"
    );
}

#[test]
fn admin_can_delete_any_upload() {
    let server = spawn_team_server();
    // alice is first → auto-admin.
    let alice = register(&server, "alice", "correct horse battery staple");
    let bob = register(&server, "bob", "another long passphrase here");

    upload(&server, &bob, "bob-stuff", "# bob");

    let client = http_client();
    let resp = client
        .delete(format!(
            "{}/api/community/skill/{}/{}",
            server.base_url(),
            bob.user_id,
            "bob-stuff"
        ))
        .bearer_auth(&alice.api_key)
        .send()
        .expect("DELETE as admin");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "admin delete must 200; got {} body={:?}",
        resp.status(),
        resp.text()
    );

    // Skill gone from DB.
    let detail = client
        .get(format!(
            "{}/api/community/skill/{}/{}",
            server.base_url(),
            bob.user_id,
            "bob-stuff"
        ))
        .bearer_auth(&alice.api_key)
        .send()
        .expect("GET detail post-admin-delete");
    assert_eq!(
        detail.status().as_u16(),
        404,
        "deleted skill should 404 on detail"
    );

    // Physical payload gone too.
    let payload = server
        .home_path()
        .join(".runai/community")
        .join(&bob.user_id)
        .join("bob-stuff");
    assert!(
        !payload.exists(),
        "physical payload must be removed; still at {:?}",
        payload
    );
}

#[test]
fn uploader_can_delete_their_own_upload() {
    let server = spawn_team_server();
    let alice = register(&server, "alice", "correct horse battery staple");
    let _bob = register(&server, "bob", "another long passphrase here");

    upload(&server, &alice, "alice-own", "# mine");

    let client = http_client();
    let resp = client
        .delete(format!(
            "{}/api/community/skill/{}/{}",
            server.base_url(),
            alice.user_id,
            "alice-own"
        ))
        .bearer_auth(&alice.api_key)
        .send()
        .expect("DELETE as uploader");
    assert_eq!(resp.status().as_u16(), 200);
}

#[test]
fn re_upload_same_name_bumps_version_preserves_installs_total() {
    let server = spawn_team_server();
    let alice = register(&server, "alice", "correct horse battery staple");
    let bob = register(&server, "bob", "another long passphrase here");

    let v1 = upload(&server, &alice, "evolving", "# v1\nfirst version");
    let version_v1 = v1["version"].as_str().expect("v1 version").to_string();

    // Bob installs v1 to bump installs_total.
    let client = http_client();
    let r = client
        .post(format!(
            "{}/api/community/install/{}/{}",
            server.base_url(),
            alice.user_id,
            "evolving"
        ))
        .bearer_auth(&bob.api_key)
        .send()
        .expect("install v1");
    assert_eq!(r.status().as_u16(), 200);

    // Wait long enough that the timestamp-derived version differs.
    std::thread::sleep(Duration::from_secs(1));

    let v2 = upload(&server, &alice, "evolving", "# v2\nsecond version");
    let version_v2 = v2["version"].as_str().expect("v2 version").to_string();
    assert_ne!(
        version_v1, version_v2,
        "re-upload must bump version, both={version_v1}"
    );

    // installs_total survived the bump.
    let detail = client
        .get(format!(
            "{}/api/community/skill/{}/{}",
            server.base_url(),
            alice.user_id,
            "evolving"
        ))
        .bearer_auth(&bob.api_key)
        .send()
        .expect("GET detail post re-upload");
    let body: serde_json::Value = detail.json().expect("detail JSON");
    assert_eq!(
        body["installs_total"].as_i64().unwrap_or(0),
        1,
        "installs_total preserved across version bump; body={body}"
    );

    // README updated to v2 content.
    assert!(
        body["readme"]
            .as_str()
            .unwrap_or_default()
            .contains("second version"),
        "v2 readme content visible; readme={}",
        body["readme"]
    );
}
