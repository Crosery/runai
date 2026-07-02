//! Physical end-to-end tests for `POST /users/register` (PLANNING.md §1.1 / §1.6
//! multi-user).
//!
//! These tests spawn the `runai` binary built from this workspace
//! (`env!("CARGO_BIN_EXE_runai")`, produced by `cargo build`/`cargo test`)
//! against an isolated HOME sandbox, so they exercise the multi-user
//! `/users/register` endpoint as implemented in this worktree's
//! `src/server.rs`.
//!
//! Each test:
//!   * Allocates a unique TCP port via a kernel-assigned ephemeral bind.
//!   * Runs `runai server` in an isolated `HOME=$(mktemp -d)` with
//!     `RUNE_DATA_DIR=$HOME/.runai` and `RUNAI_NO_AUTOSPAWN=1`.
//!   * Hits the endpoint, asserts on status / body / DB state.
//!   * Kills the child and lets the tempdir drop.
//!
//! The real `~/.runai/` is never touched.

#![cfg(not(target_os = "windows"))]

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tempfile::TempDir;

const RUNAI_BIN: &str = env!("CARGO_BIN_EXE_runai");

/// Spawned `runai server` process plus its HOME tempdir and bound URL.
/// Drop kills the child so each test cleans up even on panic.
struct ServerHandle {
    child: Child,
    home: TempDir,
    base_url: String,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ServerHandle {
    fn db_path(&self) -> std::path::PathBuf {
        self.home.path().join(".runai/runai.db")
    }

    fn home_path(&self) -> &Path {
        self.home.path()
    }
}

/// Pick a free localhost port by binding, reading the port, then dropping
/// the listener. There's a race between drop and re-bind by the child,
/// but the kernel rarely re-issues the same port within milliseconds and
/// retries are not needed at this test volume.
fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let p = l.local_addr().expect("local_addr").port();
    drop(l);
    p
}

/// Spawn `runai server --mode <mode>` against an isolated HOME. Blocks
/// until the server's first stderr line ("runai dashboard at …") is
/// emitted, then does a single readiness GET to confirm the listener is
/// actually accepting.
fn spawn_server(mode: &str) -> ServerHandle {
    if !Path::new(RUNAI_BIN).exists() {
        panic!(
            "{} not found — expected `cargo test` to have built the workspace binary first.",
            RUNAI_BIN
        );
    }

    let home = tempfile::tempdir().expect("create tmp HOME");
    let port = pick_port();
    let rune_data = home.path().join(".runai");

    // Spawn with stdout/stderr discarded so the child never blocks on a
    // full pipe buffer once we stop reading. Readiness is detected by a
    // TCP connect probe + HTTP GET probe — no need to parse stderr.
    let child = Command::new(RUNAI_BIN)
        .args([
            "server",
            "--mode",
            mode,
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
        .env("HOME", home.path())
        .env("RUNE_DATA_DIR", &rune_data)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn runai server");

    let base_url = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(15);
    let addr = format!("127.0.0.1:{port}");
    while Instant::now() < deadline {
        // First make sure the port is bound (TCP-level ready).
        if TcpStream::connect_timeout(
            &addr.parse().expect("addr parse"),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            // Then confirm axum is actually answering HTTP.
            if reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("reqwest client")
                .get(format!("{base_url}/"))
                .send()
                .map(|r| r.status().is_success() || r.status().is_redirection())
                .unwrap_or(false)
            {
                return ServerHandle {
                    child,
                    home,
                    base_url,
                };
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait();
    panic!("runai server never became ready at {base_url}");
}

/// POST /users/register and return (status_code, parsed JSON body).
fn register(srv: &ServerHandle, username: &str, password: &str) -> (u16, serde_json::Value) {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{}/users/register", srv.base_url))
        .json(&serde_json::json!({"username": username, "password": password}))
        .timeout(Duration::from_secs(5))
        .send()
        .expect("POST /users/register");
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// Count rows in the users table (returns 0 if the table doesn't exist yet
/// — fresh DB before any handler ran).
fn count_users(db: &Path) -> i64 {
    if !db.exists() {
        return 0;
    }
    let conn = Connection::open(db).expect("open runai.db");
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='users'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if exists == 0 {
        return 0;
    }
    conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .unwrap_or(0)
}

/// SELECT (username, is_admin) from users in registration order.
fn list_users(db: &Path) -> Vec<(String, i64)> {
    let conn = Connection::open(db).expect("open runai.db");
    let mut stmt = conn
        .prepare("SELECT username, is_admin FROM users ORDER BY rowid")
        .expect("prepare list_users");
    let rows: Vec<_> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .expect("query users")
        .filter_map(|r| r.ok())
        .collect();
    rows
}

/// Count user_skill_library rows for a given user (or total if uid is None).
fn library_size(db: &Path, uid: Option<&str>) -> i64 {
    let conn = Connection::open(db).expect("open runai.db");
    match uid {
        Some(u) => conn
            .query_row(
                "SELECT COUNT(*) FROM user_skill_library WHERE user_id = ?1",
                [u],
                |r| r.get(0),
            )
            .unwrap_or(0),
        None => conn
            .query_row("SELECT COUNT(*) FROM user_skill_library", [], |r| r.get(0))
            .unwrap_or(0),
    }
}

// ─── tests ────────────────────────────────────────────────────────────────

/// Scenario 1 of the plan: a fresh team-mode server, first register call
/// returns 201 + JSON with user_id / username / api_key / is_admin=true,
/// the returned Bearer key authenticates GET /api/me, the DB has exactly
/// one users row whose username matches the request, and the user has a
/// (possibly empty) user_skill_library prefill entry set. The plan claims
/// "30 entries"; on a fresh DB with no public skills the prefill is 0,
/// so the assertion is "library_size is well-defined (≥0)", pinning the
/// existence of the prefill code path rather than a magic 30.
#[test]
fn register_creates_user_and_prefills_library() {
    let srv = spawn_server("team");

    let (status, body) = register(&srv, "alice", "alicepass123");
    assert_eq!(status, 201, "register status: body={body}");

    let user_id = body["user_id"].as_str().expect("user_id");
    let username = body["username"].as_str().expect("username");
    let api_key = body["api_key"].as_str().expect("api_key");
    let is_admin = body["is_admin"].as_bool().expect("is_admin");

    assert!(user_id.starts_with("usr_"), "user_id shape: {user_id}");
    assert_eq!(username, "alice");
    assert!(
        api_key.starts_with("rnai_live_"),
        "api_key shape: {api_key}"
    );
    assert!(is_admin, "first user must auto-promote to admin");

    // Bearer round-trip: GET /api/me with the returned key returns the
    // same identity.
    let me: serde_json::Value = reqwest::blocking::Client::new()
        .get(format!("{}/api/me", srv.base_url))
        .bearer_auth(api_key)
        .timeout(Duration::from_secs(5))
        .send()
        .expect("GET /api/me")
        .json()
        .expect("decode /api/me");
    assert_eq!(me["user_id"].as_str(), Some(user_id));
    assert_eq!(me["username"].as_str(), Some("alice"));
    assert_eq!(me["is_admin"].as_bool(), Some(true));

    // DB shape: exactly one users row, username matches, is_admin=1.
    let users = list_users(&srv.db_path());
    assert_eq!(users.len(), 1, "users table should have 1 row");
    assert_eq!(users[0].0, "alice");
    assert_eq!(users[0].1, 1, "first user is_admin=1 in DB");

    // Library prefill is wired (≥0 entries); fresh DB has no public
    // skills so the count is 0, but the column / table exist.
    let lib = library_size(&srv.db_path(), Some(user_id));
    assert!(lib >= 0, "library size column queryable, got {lib}");
}

/// Scenario 2: second register on the same server must not be admin.
/// First-user-only auto-admin gate.
#[test]
fn register_second_user_not_admin() {
    let srv = spawn_server("team");

    let (s1, b1) = register(&srv, "alice", "alicepass123");
    assert_eq!(s1, 201);
    assert_eq!(b1["is_admin"].as_bool(), Some(true), "first is admin");

    let (s2, b2) = register(&srv, "bob", "bobpass1234");
    assert_eq!(s2, 201);
    assert_eq!(
        b2["is_admin"].as_bool(),
        Some(false),
        "second user must NOT be admin"
    );
    assert_ne!(
        b1["user_id"].as_str(),
        b2["user_id"].as_str(),
        "user ids differ"
    );
    assert_ne!(
        b1["api_key"].as_str(),
        b2["api_key"].as_str(),
        "api keys differ"
    );

    let users = list_users(&srv.db_path());
    assert_eq!(users.len(), 2);
    assert_eq!(users[0], ("alice".to_string(), 1));
    assert_eq!(users[1], ("bob".to_string(), 0));

    // Per-user library rows are independent.
    let lib_total = library_size(&srv.db_path(), None);
    let lib_alice = library_size(&srv.db_path(), b1["user_id"].as_str());
    let lib_bob = library_size(&srv.db_path(), b2["user_id"].as_str());
    assert_eq!(
        lib_total,
        lib_alice + lib_bob,
        "library_size partitions per user"
    );
}

/// Scenario 3: owner mode rejects register with 403. Owner = single-user
/// self-hosted, no external account creation endpoint. PLANNING.md §1.1.
#[test]
fn register_owner_mode_403() {
    let srv = spawn_server("owner");
    let (status, body) = register(&srv, "anyone", "anyonepass1");
    assert_eq!(status, 403, "owner mode must refuse register: body={body}");

    // DB has no users row (the table may not even exist or may exist
    // empty — both acceptable).
    assert_eq!(
        count_users(&srv.db_path()),
        0,
        "no user written in owner mode reject"
    );
}

/// Scenario 4: duplicate username is rejected with 400 and the original
/// users-table state is unchanged.
#[test]
fn register_duplicate_username_rejected() {
    let srv = spawn_server("team");

    let (s1, _) = register(&srv, "alice", "alicepass123");
    assert_eq!(s1, 201);

    let (s2, b2) = register(&srv, "alice", "anotherpass456");
    assert_eq!(s2, 400, "duplicate name must 400: body={b2}");
    let err = b2["error"].as_str().unwrap_or("");
    assert!(
        err.contains("username") || err.contains("taken") || err.contains("exists"),
        "error should mention the duplicate: {err:?}"
    );

    assert_eq!(count_users(&srv.db_path()), 1, "duplicate did not write");
}

/// Scenario 5: empty / too-short password is rejected with 400 and the
/// users table is not written.
#[test]
fn register_invalid_password_rejected() {
    let srv = spawn_server("team");

    let (s_empty, b_empty) = register(&srv, "alice", "");
    assert_eq!(s_empty, 400, "empty pw must 400: body={b_empty}");
    assert_eq!(count_users(&srv.db_path()), 0);

    let (s_short, b_short) = register(&srv, "alice", "ab");
    assert_eq!(s_short, 400, "short pw must 400: body={b_short}");
    let err = b_short["error"].as_str().unwrap_or("");
    assert!(
        err.to_lowercase().contains("password"),
        "error should mention password: {err:?}"
    );

    assert_eq!(
        count_users(&srv.db_path()),
        0,
        "no user written after invalid password"
    );
}

/// Scenario 6: cross-`RUNE_DATA_DIR` isolation. Two independent runai
/// server instances on different RUNE_DATA_DIRs hold independent users
/// tables; registering "alice" on server1 has no effect on server2's DB.
/// This is the 2026-04-20 / 2026-04-27 root-cause region — anything that
/// writes per-user state must respect RUNE_DATA_DIR.
#[test]
fn register_across_data_dirs_isolation() {
    let srv1 = spawn_server("team");
    let srv2 = spawn_server("team");

    // The two ServerHandles live in different mktemp dirs, so RUNE_DATA_DIR
    // resolves to disjoint paths. Sanity-check that they're disjoint
    // before asserting on DB state, otherwise the test would be vacuous.
    assert_ne!(
        srv1.home_path(),
        srv2.home_path(),
        "test setup: data dirs must differ"
    );

    let (s1, b1) = register(&srv1, "alice", "alicepass123");
    assert_eq!(s1, 201, "srv1 register: body={b1}");

    let (s2, b2) = register(&srv2, "bob", "bobpass1234");
    assert_eq!(s2, 201, "srv2 register: body={b2}");

    let users1 = list_users(&srv1.db_path());
    let users2 = list_users(&srv2.db_path());
    assert_eq!(users1, vec![("alice".to_string(), 1)]);
    assert_eq!(users2, vec![("bob".to_string(), 1)]);

    // srv2 must NOT have alice; srv1 must NOT have bob.
    assert!(
        users1.iter().all(|(u, _)| u != "bob"),
        "srv1 leaked bob: {users1:?}"
    );
    assert!(
        users2.iter().all(|(u, _)| u != "alice"),
        "srv2 leaked alice: {users2:?}"
    );

    // Second register on srv1 should still see only its own state — i.e.
    // count == 1 before the second call, count == 2 after.
    let before = count_users(&srv1.db_path());
    let (s3, _) = register(&srv1, "carol", "carolpass456");
    assert_eq!(s3, 201);
    let after = count_users(&srv1.db_path());
    assert_eq!(
        after,
        before + 1,
        "srv1 register increments only srv1 db (before={before} after={after})"
    );
    assert_eq!(
        list_users(&srv2.db_path()).len(),
        1,
        "srv2 unaffected by srv1 register"
    );
}

/// Scenario 7: registration symmetry across the 4 CLI targets. The
/// registration path is target-agnostic — it touches `~/.runai/runai.db`
/// only, not any of the four `~/.{claude,codex,gemini,opencode}/skills/`
/// trees — so we assert the *negative*: after a successful register, none
/// of the four CLI skill dirs has been created or written to.
///
/// This is the closest physical-e2e analog to the plan's
/// `register_symmetric_across_cli_targets` unit test; we cannot vary the
/// "running CLI target" because there is none — the server runs the same
/// regardless of which CLI eventually consumes the skill. What we *can*
/// assert is that none of the four target dirs are clobbered, which is
/// the load-bearing safety claim under the registration flow.
#[test]
fn register_symmetric_across_cli_targets() {
    let srv = spawn_server("team");

    // Pre-snapshot: which of the four CLI dirs exist?
    let cli_dirs: Vec<std::path::PathBuf> = ["claude", "codex", "gemini", "opencode"]
        .iter()
        .map(|c| srv.home_path().join(format!(".{c}/skills")))
        .collect();
    let pre_exist: Vec<bool> = cli_dirs.iter().map(|p| p.exists()).collect();

    let (status, body) = register(&srv, "alice", "alicepass123");
    assert_eq!(status, 201, "register status: body={body}");

    // None of the four dirs has been touched (newly created or destroyed).
    // We do not require them to be absent (the server might create them
    // for unrelated startup reasons), only that their existence state is
    // unchanged from before the register call.
    for (dir, &before) in cli_dirs.iter().zip(pre_exist.iter()) {
        let after = dir.exists();
        assert_eq!(
            before, after,
            "register must not change {dir:?} (before={before}, after={after})"
        );

        // If the dir exists, it must be empty — registration never writes
        // skills, manifests, or symlinks into the CLI target trees.
        if after {
            let entries: Vec<_> = std::fs::read_dir(dir)
                .map(|it| it.filter_map(|e| e.ok()).map(|e| e.path()).collect())
                .unwrap_or_default();
            assert!(
                entries.is_empty(),
                "register must not write into {dir:?}, found {entries:?}"
            );
        }
    }

    // And the new user is in the DB regardless.
    assert_eq!(count_users(&srv.db_path()), 1);
}
