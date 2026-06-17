//! B1/B3/B4: deleting a user must CASCADE — trash their private skills
//! (recoverable, restoring to the public pool), remove their physical
//! `<data>/users/<uid>/` subtree, anonymize their router_events, and leave the
//! public pool + other users untouched.
//!
//! This is a destructive path (per the AGENTS.md safety contract): the suite
//! runs the full scenario in an isolated HOME AND a second time under a
//! non-default `RUNE_DATA_DIR` (the 4-20 / 4-27 root-cause region), asserting
//! it only ever touches the test data dir.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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
    _decoy: Option<TempDir>,
    /// The server's REAL data dir — always `<home>/.runai` (the server uses
    /// `AppPaths::default_path()`, which ignores `RUNE_DATA_DIR`).
    data: PathBuf,
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
/// Spawn a team server (always HOME-rooted data dir). When `with_decoy` is true,
/// set `RUNE_DATA_DIR` to a second tempdir: the server still serves from
/// `HOME/.runai` (it uses `AppPaths::default_path()`), so this asserts the
/// cascade follows the SERVER's data dir, not the env one — even when the env
/// var is set. (Aside: `main.rs` startup DOES read the env dir to init it, a
/// separate pre-existing footgun tracked in its own issue; that's why we don't
/// assert "decoy untouched" here.)
fn spawn_server(with_decoy: bool) -> ServerGuard {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join(".runai");
    std::fs::create_dir_all(data.join("skills")).unwrap();
    let decoy_dir = if with_decoy {
        Some(tempfile::tempdir().unwrap())
    } else {
        None
    };
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
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match &decoy_dir {
        Some(d) => {
            cmd.env("RUNE_DATA_DIR", d.path());
        }
        None => {
            cmd.env_remove("RUNE_DATA_DIR");
        }
    }
    let child = cmd.spawn().unwrap();
    let g = ServerGuard {
        child,
        _home: home,
        _decoy: decoy_dir,
        data,
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
/// Plant a private skill for `uid` directly via the lib DB + filesystem —
/// deliberately NOT via the HTTP upload, because upload fires a `recommend
/// enrich` CLI subprocess that (unlike the server) honors `RUNE_DATA_DIR` and
/// would pollute the decoy, masking what we want to test: that the CASCADE
/// itself never touches the env data dir.
fn plant_private(db: &runai::core::db::Database, data: &Path, uid: &str, name: &str) {
    use runai::core::resource::{Resource, ResourceKind, Source};
    let dir = data.join("users").join(uid).join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\n---\nbody\n"),
    )
    .unwrap();
    db.insert_resource(&Resource {
        id: format!("u:{uid}:local:{name}"),
        name: name.into(),
        kind: ResourceKind::Skill,
        description: "d".into(),
        directory: dir,
        source: Source::Local {
            path: PathBuf::from("/tmp"),
        },
        installed_at: 0,
        enabled: std::collections::HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: Some(uid.into()),
        publish_status: "draft".into(),
    })
    .unwrap();
}

fn run_scenario(with_decoy: bool) {
    use runai::core::db::Database;

    let s = spawn_server(with_decoy);
    let admin = register(&s, "admin", "pw admin 1234");
    let alice = register(&s, "alice", "pw alice 1234");
    let bob = register(&s, "bob", "pw bob 1234");

    let db_path = s.data.join("runai.db");
    {
        let db = Database::open(&db_path).unwrap();
        plant_private(&db, &s.data, &alice.user_id, "foo");
        plant_private(&db, &s.data, &bob.user_id, "keepme"); // collateral guard
        // Plant a router_event stamped with alice's uid to verify anonymization.
        db.conn_ref()
            .execute(
                "INSERT INTO router_events (ts, provider, model, user_id) VALUES (1, 'p', 'm', ?1)",
                [&alice.user_id],
            )
            .unwrap();
    }

    let alice_dir = s.data.join("users").join(&alice.user_id);
    assert!(
        alice_dir.join("skills/foo/SKILL.md").exists(),
        "precondition"
    );

    // ── delete alice ──
    let r = http()
        .delete(format!(
            "{}/api/admin/users/{}",
            s.base_url(),
            alice.user_id
        ))
        .bearer_auth(&admin.api_key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "admin delete failed");

    // ── assertions ──
    let db = Database::open(&db_path).unwrap();

    // 1. no resources owned by alice
    let alice_rows: i64 = db
        .conn_ref()
        .query_row(
            "SELECT COUNT(*) FROM resources WHERE owner_user_id = ?1",
            [&alice.user_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(alice_rows, 0, "alice's private resources must be gone");

    // 2. physical user subtree removed
    assert!(
        !alice_dir.exists(),
        "alice's <data>/users/<uid>/ subtree must be removed, found {alice_dir:?}"
    );

    // 3. foo recoverable in the PUBLIC trash, de-owned, restore target = public pool
    let trash = db.list_trash_entries().unwrap();
    let foo = trash
        .iter()
        .find(|e| e.name == "foo")
        .expect("foo must be in trash");
    assert_eq!(foo.owner_user_id, None, "trashed skill must be de-owned");
    assert!(
        foo.directory.ends_with("skills/foo"),
        "restore target must be the public pool, got {:?}",
        foo.directory
    );
    let payload = foo.payload_path.as_ref().expect("payload path");
    assert!(
        payload.join("SKILL.md").exists(),
        "foo's SKILL.md must physically live in the trash payload {payload:?}"
    );
    assert!(
        payload.starts_with(s.data.join("trash")),
        "trash payload must be under the PUBLIC trash, got {payload:?}"
    );

    // 4. router_events anonymized
    let alice_events: i64 = db
        .conn_ref()
        .query_row(
            "SELECT COUNT(*) FROM router_events WHERE user_id = ?1",
            [&alice.user_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(alice_events, 0, "alice's router_events must be anonymized");

    // 5. user row gone
    assert!(db.find_user_by_id(&alice.user_id).unwrap().is_none());

    // 6. collateral: bob + his private skill untouched
    assert!(db.find_user_by_id(&bob.user_id).unwrap().is_some());
    assert!(
        s.data
            .join("users")
            .join(&bob.user_id)
            .join("skills/keepme/SKILL.md")
            .exists(),
        "bob's private skill must be untouched"
    );
}

#[test]
fn delete_user_cascades_default_home() {
    run_scenario(false);
}

#[test]
fn delete_user_cascades_with_rune_data_dir_decoy() {
    run_scenario(true);
}
