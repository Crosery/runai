//! PLANNING §1.3 — POST /api/prefs partial-merge + full-replace + auth.
//!
//! Drives the real `runai server` binary in `team` mode inside an isolated
//! HOME, registers a user via `POST /users/register` to obtain an api_key,
//! then hits `POST /api/prefs` with `Authorization: Bearer <api_key>` and
//! reads back through `GET /api/prefs` + the on-disk SQLite DB to assert
//! the merge / replace / null-removal semantics.
//!
//! Per PLANNING.md §1.3 the `prompt_injection_flags` sub-object is merged
//! per-key rather than replaced wholesale, and a JSON `null` value drops
//! the override (revert-to-default). The dashboard's per-prompt toggle UI
//! depends on this contract; breaking it silently clobbers sibling flags
//! whenever the user flips one switch.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};
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
    _data_override: Option<TempDir>,
    port: u16,
    data_dir: std::path::PathBuf,
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

/// Spawn `runai server --mode team` bound to 127.0.0.1 inside an isolated
/// HOME. Each invocation creates its OWN HOME tempdir, so the SQLite DB,
/// users, and prefs are physically separated from any other server in the
/// suite. The data dir always resolves to `HOME/.runai/` because the server
/// reads `AppPaths::default_path()` (HOME-anchored) for its `db_path`;
/// `RUNE_DATA_DIR` is explicitly cleared so a stray ambient env var from
/// the test runner cannot leak through.
fn spawn_team_server() -> ServerGuard {
    let home = tempfile::tempdir().expect("create tmp HOME");
    let data_dir = home.path().join(".runai");
    std::fs::create_dir_all(data_dir.join("skills")).expect("pre-create skills dir");

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
        _data_override: None,
        port,
        data_dir,
    };
    assert!(
        wait_for_port(port, Duration::from_secs(8)),
        "runai server did not bind 127.0.0.1:{port} within 8s"
    );
    guard
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client")
}

/// Register a user and return its api_key (the only credential the dashboard
/// will hand back to a client — it's never stored plaintext server-side).
fn register_user(server: &ServerGuard, username: &str, password: &str) -> String {
    let client = http_client();
    let resp = client
        .post(format!("{}/users/register", server.base_url()))
        .json(&json!({
            "username": username,
            "password": password,
        }))
        .send()
        .expect("POST /users/register");
    assert_eq!(
        resp.status().as_u16(),
        201,
        "register should 201; got {}",
        resp.status()
    );
    let body: Value = resp.json().expect("register body JSON");
    body.get("api_key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .expect("register response should include api_key")
}

/// Read the SQLite `users.prefs_json` column directly for assertion — proves
/// the bytes really landed in the database, not just that the API echoed
/// them back from memory.
fn read_prefs_json_from_db(data_dir: &Path, username: &str) -> Value {
    let db_path = data_dir.join("runai.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let row: String = conn
        .query_row(
            "SELECT prefs_json FROM users WHERE username = ?1",
            [username],
            |r| r.get(0),
        )
        .expect("find user row");
    if row.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(&row).unwrap_or_else(|_| json!({}))
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// Plan 4.28 #1: a full UserPrefs body sent to POST /api/prefs replaces every
/// field, lands in the DB, and round-trips through GET /api/prefs.
#[test]
fn post_prefs_full_replace() {
    let server = spawn_team_server();
    let api_key = register_user(&server, "alice", "correct horse battery staple");
    let client = http_client();

    // Full replacement: every top-level UserPrefs field set to a non-default
    // value, including a fresh prompt_injection_flags map.
    let full_body = json!({
        "show_tradeoff": false,
        "show_session_history": false,
        "show_feedback_protocol": false,
        "recommend_mode": "exclusive",
        "candidate_limit": 5,
        "allow_public_recommend": true,
        "recommend_enabled": false,
        "read_claude_md": false,
        "skip_reminder_enabled": true,
        "skip_reminder_template": "use sparingly",
        "prompt_injection_flags": {
            "recommend_history_prefix": false,
            "recommend_cwd_prefix": false,
        }
    });

    let resp = client
        .post(format!("{}/api/prefs", server.base_url()))
        .bearer_auth(&api_key)
        .json(&full_body)
        .send()
        .expect("POST /api/prefs");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "full-replace POST /api/prefs should 200; got {}",
        resp.status()
    );
    let echoed: Value = resp.json().expect("response is UserPrefs JSON");
    assert_eq!(echoed["show_tradeoff"], json!(false));
    assert_eq!(echoed["recommend_mode"], json!("exclusive"));
    assert_eq!(echoed["candidate_limit"], json!(5));
    assert_eq!(echoed["allow_public_recommend"], json!(true));
    assert_eq!(echoed["skip_reminder_template"], json!("use sparingly"));
    assert_eq!(
        echoed["prompt_injection_flags"]["recommend_history_prefix"],
        json!(false)
    );

    // GET /api/prefs returns the same shape (proves persistence).
    let get_resp = client
        .get(format!("{}/api/prefs", server.base_url()))
        .bearer_auth(&api_key)
        .send()
        .expect("GET /api/prefs");
    assert_eq!(get_resp.status().as_u16(), 200);
    let read_back: Value = get_resp.json().expect("GET body JSON");
    assert_eq!(read_back["show_tradeoff"], json!(false));
    assert_eq!(read_back["recommend_mode"], json!("exclusive"));
    assert_eq!(read_back["candidate_limit"], json!(5));
    assert_eq!(read_back["recommend_enabled"], json!(false));
    assert_eq!(
        read_back["prompt_injection_flags"]["recommend_cwd_prefix"],
        json!(false)
    );

    // Direct DB check — bytes actually landed in users.prefs_json.
    let stored = read_prefs_json_from_db(&server.data_dir, "alice");
    assert_eq!(stored["recommend_mode"], json!("exclusive"));
    assert_eq!(stored["candidate_limit"], json!(5));
    assert_eq!(
        stored["prompt_injection_flags"]["recommend_history_prefix"],
        json!(false)
    );
}

/// Plan 4.28 #2: per-key merge — flipping one flag must NOT clobber siblings.
#[test]
fn post_prefs_partial_merge() {
    let server = spawn_team_server();
    let api_key = register_user(&server, "alice", "correct horse battery staple");
    let client = http_client();

    // Seed flags = {a:true, b:true}. We use real prompt names so the toggle
    // semantics are realistic, but the merge logic is name-agnostic.
    let seed_resp = client
        .post(format!("{}/api/prefs", server.base_url()))
        .bearer_auth(&api_key)
        .json(&json!({
            "prompt_injection_flags": {
                "recommend_history_prefix": true,
                "recommend_cwd_prefix": true,
            }
        }))
        .send()
        .expect("seed flags");
    assert_eq!(seed_resp.status().as_u16(), 200);

    // Patch: flip ONLY recommend_history_prefix to false. The merge logic
    // must keep recommend_cwd_prefix=true intact.
    let patch_resp = client
        .post(format!("{}/api/prefs", server.base_url()))
        .bearer_auth(&api_key)
        .json(&json!({
            "prompt_injection_flags": {
                "recommend_history_prefix": false,
            }
        }))
        .send()
        .expect("flip one flag");
    assert_eq!(patch_resp.status().as_u16(), 200);

    let echoed: Value = patch_resp.json().expect("response JSON");
    let flags = &echoed["prompt_injection_flags"];
    assert_eq!(
        flags["recommend_history_prefix"],
        json!(false),
        "flipped key must be false"
    );
    assert_eq!(
        flags["recommend_cwd_prefix"],
        json!(true),
        "sibling key must survive the merge"
    );

    // DB-backed check — bytes on disk, not just the response.
    let stored = read_prefs_json_from_db(&server.data_dir, "alice");
    let stored_flags = &stored["prompt_injection_flags"];
    assert_eq!(stored_flags["recommend_history_prefix"], json!(false));
    assert_eq!(stored_flags["recommend_cwd_prefix"], json!(true));
}

/// Plan 4.28 #3: a JSON `null` value inside `prompt_injection_flags` removes
/// that key (revert-to-default = "missing means enabled").
#[test]
fn post_prefs_null_removes_flag() {
    let server = spawn_team_server();
    let api_key = register_user(&server, "alice", "correct horse battery staple");
    let client = http_client();

    // Seed: explicitly disable recommend_history_prefix.
    let seed_resp = client
        .post(format!("{}/api/prefs", server.base_url()))
        .bearer_auth(&api_key)
        .json(&json!({
            "prompt_injection_flags": {
                "recommend_history_prefix": false,
            }
        }))
        .send()
        .expect("seed");
    assert_eq!(seed_resp.status().as_u16(), 200);
    let seeded = read_prefs_json_from_db(&server.data_dir, "alice");
    assert_eq!(
        seeded["prompt_injection_flags"]["recommend_history_prefix"],
        json!(false),
        "seed must land in DB before we test removal"
    );

    // Null-out the override: should DELETE the key from the map.
    let null_resp = client
        .post(format!("{}/api/prefs", server.base_url()))
        .bearer_auth(&api_key)
        .json(&json!({
            "prompt_injection_flags": {
                "recommend_history_prefix": Value::Null,
            }
        }))
        .send()
        .expect("null patch");
    assert_eq!(null_resp.status().as_u16(), 200);

    // Echo: key removed from response.
    let echoed: Value = null_resp.json().expect("response JSON");
    let flags = echoed["prompt_injection_flags"]
        .as_object()
        .expect("flags object");
    assert!(
        !flags.contains_key("recommend_history_prefix"),
        "null value must drop the key from the response map; got {flags:?}"
    );

    // DB: key removed from prefs_json on disk.
    let stored = read_prefs_json_from_db(&server.data_dir, "alice");
    let stored_flags = stored["prompt_injection_flags"]
        .as_object()
        .expect("flags object in DB");
    assert!(
        !stored_flags.contains_key("recommend_history_prefix"),
        "DB prefs_json should no longer carry the override; got {stored_flags:?}"
    );
}

/// Plan 4.28 #4: two separate `runai server` processes living in different
/// isolated HOMEs keep their prefs in independent SQLite DBs — changing
/// user1's prefs on server A must NOT show up on server B.
///
/// Note: the data dir resolution at server startup is HOME-anchored (the
/// server reads `AppPaths::default_path()` which goes through `dirs::home_dir`
/// rather than `RUNE_DATA_DIR`), so HOME isolation is the load-bearing
/// boundary the test asserts on. The "across data dirs" name in the plan
/// refers to the user-visible outcome — two physically distinct data
/// directories with two physically distinct DBs — which HOME isolation
/// produces deterministically.
#[test]
fn post_prefs_across_data_dirs_isolation() {
    let server_a = spawn_team_server();
    let server_b = spawn_team_server();
    let path_a = server_a.data_dir.clone();
    let path_b = server_b.data_dir.clone();

    let api_key_a = register_user(&server_a, "alice", "correct horse battery staple");
    let api_key_b = register_user(&server_b, "alice", "totally different password 42");

    let client = http_client();

    // On A: switch recommend_mode to exclusive + flag x=false.
    let resp_a = client
        .post(format!("{}/api/prefs", server_a.base_url()))
        .bearer_auth(&api_key_a)
        .json(&json!({
            "recommend_mode": "exclusive",
            "prompt_injection_flags": {
                "recommend_history_prefix": false,
            }
        }))
        .send()
        .expect("POST /api/prefs on A");
    assert_eq!(resp_a.status().as_u16(), 200);

    // On B: switch recommend_mode to off + flag y=false.
    let resp_b = client
        .post(format!("{}/api/prefs", server_b.base_url()))
        .bearer_auth(&api_key_b)
        .json(&json!({
            "recommend_mode": "off",
            "prompt_injection_flags": {
                "recommend_cwd_prefix": false,
            }
        }))
        .send()
        .expect("POST /api/prefs on B");
    assert_eq!(resp_b.status().as_u16(), 200);

    // Direct DB reads (bypasses the HTTP path entirely — proves the two
    // SQLite files diverged on disk, not just in API responses).
    let stored_a = read_prefs_json_from_db(&path_a, "alice");
    let stored_b = read_prefs_json_from_db(&path_b, "alice");

    assert_eq!(
        stored_a["recommend_mode"],
        json!("exclusive"),
        "A's DB must have exclusive mode"
    );
    assert_eq!(
        stored_b["recommend_mode"],
        json!("off"),
        "B's DB must have off mode (no cross-leak)"
    );

    let flags_a = stored_a["prompt_injection_flags"]
        .as_object()
        .expect("A flags");
    let flags_b = stored_b["prompt_injection_flags"]
        .as_object()
        .expect("B flags");

    assert_eq!(
        flags_a.get("recommend_history_prefix"),
        Some(&json!(false)),
        "A: history flag false"
    );
    assert!(
        !flags_a.contains_key("recommend_cwd_prefix"),
        "A must NOT have B's cwd flag"
    );

    assert_eq!(
        flags_b.get("recommend_cwd_prefix"),
        Some(&json!(false)),
        "B: cwd flag false"
    );
    assert!(
        !flags_b.contains_key("recommend_history_prefix"),
        "B must NOT have A's history flag"
    );

    // Sanity: the two DB files are literally different paths.
    assert_ne!(path_a, path_b, "test setup bug: data dirs must be distinct");
}

/// Plan 4.28 #5: hitting POST /api/prefs without any auth credential returns
/// 401 — the route is gated by `require_user`.
#[test]
fn post_prefs_unauthenticated_401() {
    let server = spawn_team_server();
    let client = http_client();

    let resp = client
        .post(format!("{}/api/prefs", server.base_url()))
        .json(&json!({"recommend_mode": "exclusive"}))
        .send()
        .expect("POST /api/prefs unauth");

    assert_eq!(
        resp.status().as_u16(),
        401,
        "unauthenticated POST /api/prefs must return 401, got {}",
        resp.status()
    );
}

/// Plan 4.28 #6: pin the observable consequence of the `merge_prefs_patch`
/// non-object branch via the HTTP boundary — sending a JSON array body must
/// not 5xx and must not corrupt the user's prefs row (a subsequent GET
/// returns a valid UserPrefs shape with all default fields present).
///
/// The pure unit-tests of `merge_prefs_patch` (replace / merge / null-drop)
/// already live alongside the function in `src/server/prefs.rs::tests` —
/// they cannot be exercised from this integration suite because the
/// function is `pub(super)`. This test pins the integration-level contract
/// that the merge code does not panic / corrupt under hostile shapes.
#[test]
fn post_prefs_non_object_body_does_not_corrupt() {
    let server = spawn_team_server();
    let api_key = register_user(&server, "alice", "correct horse battery staple");
    let client = http_client();

    let resp = client
        .post(format!("{}/api/prefs", server.base_url()))
        .bearer_auth(&api_key)
        .json(&json!([1, 2, 3]))
        .send()
        .expect("POST non-object body");

    let status = resp.status().as_u16();
    assert!(
        status == 200 || (400..500).contains(&status),
        "non-object body must produce 2xx or 4xx, not 5xx; got {status}"
    );

    // GET /api/prefs MUST still return a valid UserPrefs shape regardless of
    // whether the array body was accepted (hard-replace path) or rejected
    // (extractor path). Defaults are filled in by UserPrefs::from_json_str.
    let get_resp = client
        .get(format!("{}/api/prefs", server.base_url()))
        .bearer_auth(&api_key)
        .send()
        .expect("GET after non-object POST");
    assert_eq!(get_resp.status().as_u16(), 200);
    let prefs: Value = get_resp.json().expect("GET body");
    assert!(
        prefs.get("recommend_mode").is_some(),
        "recommend_mode field must be present (defaults applied)"
    );
    assert!(
        prefs.get("candidate_limit").is_some(),
        "candidate_limit field must be present (defaults applied)"
    );
    assert!(
        prefs.get("prompt_injection_flags").is_some(),
        "prompt_injection_flags field must be present (defaults applied)"
    );
}
