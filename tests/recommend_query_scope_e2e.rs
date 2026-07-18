//! Physical regression coverage for the bounded `/recommend` query lane.
//!
//! The fast JSON path must preserve the same owner boundary as the general
//! router: anonymous callers see only public skills, while an authenticated
//! caller sees the public pool plus that caller's private skills. The test uses
//! an isolated HOME and the real server binary so no live RunAI assets move.

#![cfg(not(target_os = "windows"))]

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use runai::core::db::Database;
use runai::core::resource::{Resource, ResourceKind, Source};
use serde_json::{Value, json};
use tempfile::TempDir;

fn runai_cmd() -> Command {
    Command::cargo_bin("runai").expect("runai binary")
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let address: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
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

    fn data_dir(&self) -> std::path::PathBuf {
        self.home.path().join(".runai")
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server_mode(mode: &str) -> ServerGuard {
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
        .env("RUNAI_DISABLE_SKILL_WATCHER", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let guard = ServerGuard { child, home, port };
    assert!(wait_for_port(guard.port, Duration::from_secs(8)));
    guard
}

fn spawn_server() -> ServerGuard {
    spawn_server_mode("team")
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

fn register(server: &ServerGuard, username: &str) -> (String, String) {
    let response = http()
        .post(format!("{}/users/register", server.base_url()))
        .json(&json!({
            "username": username,
            "password": format!("pw {username} 1234"),
        }))
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 201);
    let body: Value = response.json().unwrap();
    (
        body["user_id"].as_str().unwrap().to_string(),
        body["api_key"].as_str().unwrap().to_string(),
    )
}

fn insert_skill(db: &Database, name: &str, owner: Option<&str>) {
    let source = Source::Local {
        path: std::path::PathBuf::from("/isolated-test"),
    };
    db.insert_resource(&Resource {
        id: Resource::generate_id(&source, name, owner),
        name: name.to_string(),
        kind: ResourceKind::Skill,
        description: format!("{name} presentation helper"),
        directory: std::path::PathBuf::from(format!("/isolated-test/{name}")),
        source,
        installed_at: 0,
        enabled: HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: owner.map(str::to_string),
        publish_status: "draft".to_string(),
    })
    .unwrap();
}

fn query_response(
    server: &ServerGuard,
    bearer: Option<&str>,
    session: &str,
) -> reqwest::blocking::Response {
    let mut request = http()
        .post(format!("{}/recommend", server.base_url()))
        .json(&json!({
            "query": "制作中文 PPT 演示文稿",
            "session_id": session,
            "client_kind": "pi",
        }));
    if let Some(key) = bearer {
        request = request.bearer_auth(key);
    }
    request.send().unwrap()
}

fn query(server: &ServerGuard, bearer: Option<&str>, session: &str) -> Value {
    let response = query_response(server, bearer, session);
    assert_eq!(response.status().as_u16(), 200);
    response.json().unwrap()
}

fn candidate_names(body: &Value) -> Vec<&str> {
    body["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["name"].as_str().unwrap())
        .collect()
}

#[test]
fn owner_query_lane_sees_public_pool_and_honors_kill_switch() {
    let server = spawn_server_mode("owner");
    let db = Database::open(&server.data_dir().join("runai.db")).unwrap();
    insert_skill(&db, "public-ppt", None);
    db.set_app_setting(
        "owner_prefs",
        r#"{"allow_public_recommend":false,"recommend_enabled":true}"#,
    )
    .unwrap();
    drop(db);

    let visible = query(&server, None, "owner-query-visible");
    assert_eq!(candidate_names(&visible), vec!["public-ppt"]);

    let db = Database::open(&server.data_dir().join("runai.db")).unwrap();
    db.set_app_setting(
        "owner_prefs",
        r#"{"allow_public_recommend":true,"recommend_enabled":false}"#,
    )
    .unwrap();
    drop(db);
    let disabled = query(&server, None, "owner-query-disabled");
    assert!(candidate_names(&disabled).is_empty());
}

#[test]
fn query_candidates_preserve_public_and_private_owner_boundaries() {
    let server = spawn_server();
    let (alice_id, alice_key) = register(&server, "alice");
    let (bob_id, bob_key) = register(&server, "bob");

    let db = Database::open(&server.data_dir().join("runai.db")).unwrap();
    insert_skill(&db, "public-ppt", None);
    insert_skill(&db, "alice-ppt", Some(&alice_id));
    insert_skill(&db, "bob-ppt", Some(&bob_id));
    db.library_add(&alice_id, "public-ppt").unwrap();
    drop(db);

    let anonymous = query(&server, None, "anon-session");
    assert_eq!(candidate_names(&anonymous), vec!["public-ppt"]);

    let alice = query(&server, Some(&alice_key), "alice-session");
    assert_eq!(candidate_names(&alice), vec!["alice-ppt", "public-ppt"]);
    assert!(
        alice["runai_session_id"]
            .as_str()
            .is_some_and(|value| value.starts_with("rnai_sess_"))
    );

    let bob = query(&server, Some(&bob_key), "bob-session");
    assert_eq!(candidate_names(&bob), vec!["bob-ppt"]);

    let db = Database::open(&server.data_dir().join("runai.db")).unwrap();
    db.update_user_prefs(
        &bob_id,
        r#"{"allow_public_recommend":true,"recommend_enabled":true}"#,
    )
    .unwrap();
    db.update_user_prefs(
        &alice_id,
        r#"{"allow_public_recommend":true,"recommend_enabled":false}"#,
    )
    .unwrap();
    drop(db);

    let bob_public = query(&server, Some(&bob_key), "bob-public-session");
    assert_eq!(candidate_names(&bob_public), vec!["bob-ppt", "public-ppt"]);

    let alice_disabled = query(&server, Some(&alice_key), "alice-disabled-session");
    assert!(candidate_names(&alice_disabled).is_empty());

    let stale = query_response(
        &server,
        Some("rnai_live_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "stale-session",
    );
    assert_eq!(stale.status().as_u16(), 200);
    assert_eq!(stale.headers()["content-type"], "text/plain; charset=utf-8");
    assert!(stale.text().unwrap().is_empty());
}
