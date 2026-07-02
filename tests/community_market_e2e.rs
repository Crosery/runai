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

/// PLANNING §1.4 rewrite / issue #29: `POST /api/community/upload` is now
/// admin-only, so a non-admin user can no longer land a skill directly in
/// the community pool via HTTP. Tests that need a "bob-owned community
/// skill" fixture must go through the real workflow instead:
///   1. `bob` uploads to his PRIVATE pool via `/api/users/me/skills/upload`.
///   2. Force-write a `resource_ai_summary` row directly via sqlite — the
///      test sandbox has no LLM provider configured, so real enrichment
///      never completes and `publish-request` would 400 forever otherwise
///      (same "force the gate open via direct DB write" pattern already
///      used by `tests/admin_publish_approve_e2e.rs`'s doc comment).
///   3. `bob` calls `publish-request` (draft → pending).
///   4. `admin` looks up the resulting `resource_id` via
///      `GET /api/admin/publish-requests` and calls `approve` (pending →
///      approved + physical copy into `<data>/community/<uid>/<name>/`).
///
/// Returns the `resource_id` string so callers can assert on it if needed.
fn private_upload(
    server: &ServerGuard,
    actor: &Account,
    skill_name: &str,
    skill_md: &str,
) -> serde_json::Value {
    let client = http_client();
    let boundary = format!(
        "----runai-test-priv-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let bundle = make_bundle_bytes(skill_name, skill_md);
    let body = build_multipart_body(skill_name, &bundle, &boundary);
    let resp = client
        .post(format!("{}/api/users/me/skills/upload", server.base_url()))
        .bearer_auth(&actor.api_key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .expect("POST /api/users/me/skills/upload");
    let status = resp.status().as_u16();
    let json: serde_json::Value = resp.json().unwrap_or(serde_json::json!({}));
    assert_eq!(status, 200, "private upload status={status} body={json}");
    assert_eq!(
        json["publish_status"].as_str(),
        Some("draft"),
        "fresh private upload must be draft; body={json}"
    );
    json
}

/// Force-complete enrichment for `(owner_user_id, name)` by writing a
/// `resource_ai_summary` row directly via sqlite, bypassing the real LLM
/// call the test sandbox has no provider for. Mirrors the schema-v21 shape
/// in `src/core/db/schema.rs` (`PRIMARY KEY (owner_user_id, name)`).
fn force_enrich_summary(server: &ServerGuard, owner_user_id: &str, name: &str) {
    let db_path = server.home_path().join(".runai/runai.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open test db for force-enrich");
    conn.execute(
        "INSERT INTO resource_ai_summary
            (owner_user_id, name, summary, updated_at, llm_score, search_doc, router_card, source_hash, prompt_hash, format_key)
         VALUES (?1, ?2, ?3, ?4, 8, ?3, ?3, 'test', 'test', 'test')
         ON CONFLICT(owner_user_id, name) DO UPDATE SET summary = excluded.summary",
        rusqlite::params![
            owner_user_id,
            name,
            format!("test-forced-summary for {name}"),
            chrono::Utc::now().timestamp(),
        ],
    )
    .expect("insert forced resource_ai_summary row");
}

fn publish_request(server: &ServerGuard, actor: &Account, name: &str) -> serde_json::Value {
    let client = http_client();
    let resp = client
        .post(format!(
            "{}/api/users/me/skills/{}/publish-request",
            server.base_url(),
            name
        ))
        .bearer_auth(&actor.api_key)
        .send()
        .expect("POST publish-request");
    let status = resp.status().as_u16();
    let json: serde_json::Value = resp.json().unwrap_or(serde_json::json!({}));
    assert_eq!(status, 200, "publish-request status={status} body={json}");
    assert_eq!(json["publish_status"].as_str(), Some("pending"));
    json
}

/// Admin approves `owner_username`'s pending publish-request for `name`,
/// resolving the `resource_id` via `GET /api/admin/publish-requests` first
/// (the approve endpoint takes a resource_id, not a name).
fn admin_approve(server: &ServerGuard, admin: &Account, owner_username: &str, name: &str) {
    let client = http_client();
    let list_resp = client
        .get(format!("{}/api/admin/publish-requests", server.base_url()))
        .bearer_auth(&admin.api_key)
        .send()
        .expect("GET /api/admin/publish-requests");
    assert_eq!(list_resp.status().as_u16(), 200);
    let list_json: serde_json::Value = list_resp.json().expect("publish-requests JSON");
    let items = list_json["items"].as_array().expect("items array");
    let resource_id = items
        .iter()
        .find(|it| {
            it["name"].as_str() == Some(name)
                && it["uploader_username"].as_str() == Some(owner_username)
        })
        .and_then(|it| it["resource_id"].as_str())
        .unwrap_or_else(|| {
            panic!("no pending publish-request for {owner_username}/{name}; list={list_json}")
        })
        .to_string();

    let approve_resp = client
        .post(format!(
            "{}/api/admin/publish-requests/{resource_id}/approve",
            server.base_url()
        ))
        .bearer_auth(&admin.api_key)
        .send()
        .expect("POST approve");
    let status = approve_resp.status().as_u16();
    let body = approve_resp.text().unwrap_or_default();
    assert_eq!(status, 200, "approve status={status} body={body}");
}

#[derive(Debug)]
struct AccountWithUsername {
    account: Account,
    username: String,
}

fn register_named(server: &ServerGuard, username: &str, password: &str) -> AccountWithUsername {
    AccountWithUsername {
        account: register(server, username, password),
        username: username.to_string(),
    }
}

/// End-to-end: private upload → forced enrich → publish-request → admin
/// approve. Lands `name` in the community pool with `uploader_uid =
/// uploader.account.user_id`, exactly like the pre-#29 direct-upload
/// fixture did — but through the real workflow instead of the now
/// admin-only `/api/community/upload`.
fn land_in_community_via_workflow(
    server: &ServerGuard,
    uploader: &AccountWithUsername,
    admin: &Account,
    name: &str,
    skill_md: &str,
) {
    private_upload(server, &uploader.account, name, skill_md);
    force_enrich_summary(server, &uploader.account.user_id, name);
    publish_request(server, &uploader.account, name);
    admin_approve(server, admin, &uploader.username, name);
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
    // alice is first → auto-admin.
    let alice = register(&server, "alice", "correct horse battery staple");
    let bob = register_named(&server, "bob", "another long passphrase here");
    // Carol is third user — NOT first, so not auto-admin.
    let carol = register(&server, "carol", "yet another good password!!!");

    land_in_community_via_workflow(&server, &bob, &alice, "bob-private", "# bob's skill");

    let client = http_client();
    let resp = client
        .delete(format!(
            "{}/api/community/skill/{}/{}",
            server.base_url(),
            bob.account.user_id,
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
            bob.account.user_id,
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
    let bob = register_named(&server, "bob", "another long passphrase here");

    land_in_community_via_workflow(&server, &bob, &alice, "bob-stuff", "# bob");

    let client = http_client();
    let resp = client
        .delete(format!(
            "{}/api/community/skill/{}/{}",
            server.base_url(),
            bob.account.user_id,
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
            bob.account.user_id,
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
        .join(&bob.account.user_id)
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

/// PLANNING §1.4 rewrite / issue #29: `POST /api/community/upload` used to
/// be reachable by any authenticated user, letting them bypass the
/// draft → enrich → publish-request → approve workflow entirely. It is now
/// `require_admin`-gated, matching every other admin-only route.
#[test]
fn direct_community_upload_requires_admin() {
    let server = spawn_team_server();
    // alice is first → auto-admin.
    let alice = register(&server, "alice", "correct horse battery staple");
    let bob = register(&server, "bob", "another long passphrase here");

    let client = http_client();
    let boundary = format!(
        "----runai-test-admin-gate-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let bundle = make_bundle_bytes("bob-direct-attempt", "# bob tries to bypass review");
    let body = build_multipart_body("bob-direct-attempt", &bundle, &boundary);

    // Non-admin (bob) must 403.
    let resp = client
        .post(format!("{}/api/community/upload", server.base_url()))
        .bearer_auth(&bob.api_key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body.clone())
        .send()
        .expect("POST /api/community/upload as non-admin");
    assert_eq!(
        resp.status().as_u16(),
        403,
        "non-admin direct upload must 403; body={:?}",
        resp.text()
    );

    // Nothing landed anywhere — neither the community pool nor bob's
    // private pool. This endpoint has never written to a caller's
    // private pool, so a regression that silently redirected instead of
    // rejecting would still show up here.
    let community_payload = server
        .home_path()
        .join(".runai/community")
        .join(&bob.user_id)
        .join("bob-direct-attempt");
    assert!(
        !community_payload.exists(),
        "rejected upload must not write to the community pool; found {:?}",
        community_payload
    );
    let private_payload = server
        .home_path()
        .join(".runai/users")
        .join(&bob.user_id)
        .join("skills/bob-direct-attempt");
    assert!(
        !private_payload.exists(),
        "rejected upload must not write to bob's private pool either; found {:?}",
        private_payload
    );

    // Admin (alice) can still use the direct endpoint.
    let boundary2 = format!(
        "----runai-test-admin-gate-ok-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let bundle2 = make_bundle_bytes("admin-direct", "# admin seeds directly");
    let body2 = build_multipart_body("admin-direct", &bundle2, &boundary2);
    let resp2 = client
        .post(format!("{}/api/community/upload", server.base_url()))
        .bearer_auth(&alice.api_key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary2}"),
        )
        .body(body2)
        .send()
        .expect("POST /api/community/upload as admin");
    assert_eq!(
        resp2.status().as_u16(),
        200,
        "admin direct upload must still 200; body={:?}",
        resp2.text()
    );
}
