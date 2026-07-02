//! issue #24 — data-directory resolution must be consistent across a single
//! `runai` process. Before the fix, `src/main.rs` resolved the data dir via
//! `paths::data_dir()` (honors `RUNE_DATA_DIR`) while `src/server/app.rs`
//! (`serve_with`) resolved it via `AppPaths::default_path()` (ignores env),
//! so a `RUNE_DATA_DIR=B runai server` split its state: the server read/wrote
//! `HOME/.runai` while the enrich child it spawned honored `B`. This suite
//! pins the honor-env decision (see `.loops/.../log.md` A3): the server AND
//! every subprocess it spawns resolve to the SAME env-honored data dir.
//!
//! Safety contract (AGENTS.md): every `runai` invocation runs inside an
//! isolated tempdir HOME + tempdir `RUNE_DATA_DIR`; the real `$HOME/.runai`
//! is snapshotted and asserted untouched. No test here binds 17888 (a real
//! server runs there on the dev box) — every server picks a free port.

#![cfg(not(target_os = "windows"))]

use std::collections::BTreeSet;
use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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
fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// A running team server plus the two tempdirs that isolate it. `data` is the
/// data dir the server is EXPECTED to use after the honor-env fix: the
/// `RUNE_DATA_DIR` override when set, else `HOME/.runai`.
struct ServerGuard {
    child: Child,
    _home: TempDir,
    _rune: Option<TempDir>,
    /// Path that `HOME/.runai` would resolve to (the "wrong" dir when a
    /// divergent `RUNE_DATA_DIR` is set — asserted NOT used by the server).
    home_runai: PathBuf,
    /// The env-honored data dir the server must actually use.
    data: PathBuf,
    port: u16,
}
impl ServerGuard {
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn db_path(&self) -> PathBuf {
        self.data.join("runai.db")
    }
}
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn a team-mode server. When `rune_data` is `Some`, `RUNE_DATA_DIR` is
/// set to a SEPARATE tempdir (divergent from `HOME/.runai`) — the honor-env
/// contract requires the server to use it. When `None`, no env override is
/// set and the server must use `HOME/.runai`.
fn spawn_server(with_divergent_rune: bool) -> ServerGuard {
    let home = tempfile::tempdir().unwrap();
    let home_runai = home.path().join(".runai");
    let rune = if with_divergent_rune {
        Some(tempfile::tempdir().unwrap())
    } else {
        None
    };
    // The data dir the server is expected to use.
    let data = match &rune {
        Some(d) => d.path().to_path_buf(),
        None => home_runai.clone(),
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
    match &rune {
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
        _rune: rune,
        home_runai,
        data,
        port,
    };
    assert!(
        wait_for_port(g.port, Duration::from_secs(8)),
        "server did not bind"
    );
    g
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
    assert_eq!(r.status().as_u16(), 201, "register {u} failed");
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
/// Upload a private skill (also fires `spawn_enrich` server-side — the exact
/// path issue #24 flagged, where the enrich child could land in a different
/// data dir than the server itself).
fn upload_private(s: &ServerGuard, actor: &Account, name: &str) {
    let bundle = make_bundle(name, &format!("---\nname: {name}\n---\nbody\n"));
    let boundary = format!("----runai-ddc-{}", actor.user_id);
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
    assert_eq!(r.status().as_u16(), 200, "upload {name} failed");
}

fn count(db: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

// ── real-home snapshot ────────────────────────────────────────────────
// The dev box runs a live team server against the real `~/.runai`, which
// mutates `runai.db` (router_events) continuously. So we do NOT assert
// exact mtime/size equality (that would flake on the live server's own
// writes, unrelated to this test). Instead we assert the structural safety
// invariant the 2026-04-27 incident violated: the real `.runai` still
// EXISTS (was not renamed away), no top-level entry was removed, and none
// of this test's uniquely-named artifacts leaked into it.

fn real_home_runai() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("real HOME")).join(".runai")
}
fn top_entries(dir: &Path) -> Option<BTreeSet<String>> {
    let rd = std::fs::read_dir(dir).ok()?;
    Some(
        rd.flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
    )
}
struct RealHomeSnapshot {
    existed: bool,
    entries: Option<BTreeSet<String>>,
}
fn snapshot_real_home() -> RealHomeSnapshot {
    let p = real_home_runai();
    RealHomeSnapshot {
        existed: p.exists(),
        entries: top_entries(&p),
    }
}
/// Assert the test did not touch the real `~/.runai`. `unique_skill` is a
/// name unique to this test run; it must not appear in the real skill pools.
fn assert_real_home_untouched(before: &RealHomeSnapshot, unique_skill: &str) {
    let p = real_home_runai();
    if before.existed {
        assert!(
            p.exists(),
            "real ~/.runai must still exist (was renamed away? 2026-04-27 signature)"
        );
        let after = top_entries(&p);
        if let (Some(b), Some(a)) = (&before.entries, &after) {
            for e in b {
                assert!(a.contains(e), "real ~/.runai lost a top-level entry: {e}");
            }
        }
    }
    // Our uniquely-named skill must never have been written into real home.
    for sub in ["skills", "users"] {
        let hit = walk_contains_dir(&p.join(sub), unique_skill);
        assert!(
            !hit,
            "test skill {unique_skill:?} leaked into real ~/.runai/{sub}"
        );
    }
}
fn walk_contains_dir(root: &Path, target: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(root) else {
        return false;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        if e.file_name().to_string_lossy() == target {
            return true;
        }
        if walk_contains_dir(&p, target) {
            return true;
        }
    }
    false
}

/// A2 / A31(i): the default (no `RUNE_DATA_DIR`) path does not regress —
/// register + a spawn-enrich action both land in `HOME/.runai`.
#[test]
fn default_home_no_env_register_and_enrich_colocate() {
    let before = snapshot_real_home();
    let s = spawn_server(false);
    assert_eq!(s.data, s.home_runai, "sanity: no env → HOME/.runai");

    let alice = register(&s, "alice_ddc", "pw ddc alice 1");
    let skill = format!("ddc-def-{}", &alice.user_id[..8.min(alice.user_id.len())]);
    upload_private(&s, &alice, &skill);

    let db = s.db_path();
    assert!(db.exists(), "server DB must exist at HOME/.runai/runai.db");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM users WHERE username='alice_ddc'"),
        1,
        "registered user must live in HOME/.runai DB"
    );
    let skill_dir = s
        .data
        .join("users")
        .join(&alice.user_id)
        .join("skills")
        .join(&skill);
    assert!(
        skill_dir.join("SKILL.md").exists(),
        "uploaded private skill must physically land in HOME/.runai, got {skill_dir:?}"
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT COUNT(*) FROM resources WHERE name='{skill}'")
        ),
        1,
        "enrich-target resource row must be in the SAME DB as the user"
    );
    assert_real_home_untouched(&before, &skill);
}

/// A31(ii) + A31(b): with a divergent `RUNE_DATA_DIR=B`, the server writes
/// register products AND the spawn-enrich action's products to B, and never
/// touches `HOME/.runai`. This is the load-bearing anchor for issue #24 —
/// before the fix the server used `HOME/.runai` and the enrich child used B.
#[test]
fn server_honors_rune_data_dir_and_colocates_enrich() {
    let before = snapshot_real_home();
    let s = spawn_server(true);
    assert_ne!(
        s.data, s.home_runai,
        "sanity: env B differs from HOME/.runai"
    );

    let alice = register(&s, "alice_ddc2", "pw ddc alice 2");
    let skill = format!("ddc-env-{}", &alice.user_id[..8.min(alice.user_id.len())]);
    upload_private(&s, &alice, &skill);

    // Register product landed in B.
    let db_b = s.db_path();
    assert!(
        db_b.exists(),
        "server DB must exist at RUNE_DATA_DIR/runai.db"
    );
    assert_eq!(
        count(
            &db_b,
            "SELECT COUNT(*) FROM users WHERE username='alice_ddc2'"
        ),
        1,
        "registered user must live in RUNE_DATA_DIR DB (honor-env)"
    );

    // Enrich-triggering upload product ALSO landed in B, colocated with the
    // user — the resource row (the enrich target) and the user share DB B.
    let skill_dir = s
        .data
        .join("users")
        .join(&alice.user_id)
        .join("skills")
        .join(&skill);
    assert!(
        skill_dir.join("SKILL.md").exists(),
        "uploaded skill must physically land under RUNE_DATA_DIR, got {skill_dir:?}"
    );
    assert_eq!(
        count(
            &db_b,
            &format!("SELECT COUNT(*) FROM resources WHERE name='{skill}'")
        ),
        1,
        "enrich-target resource must be colocated with register products in B"
    );

    // HOME/.runai must NOT have been used as a data dir at all (no split).
    assert!(
        !s.home_runai.join("runai.db").exists(),
        "server must not have split state into HOME/.runai (found a DB there)"
    );

    assert_real_home_untouched(&before, &skill);
}

/// A32: a honor-env CLI subcommand and the server resolve to the SAME data
/// dir under one `RUNE_DATA_DIR`. Run `runai list` first (creates B), then
/// the server registers into B; the CLI-created DB is the one the server
/// wrote into, and `HOME/.runai` is never used.
#[test]
fn cli_subcommand_and_server_share_one_data_dir() {
    let before = snapshot_real_home();
    let home = tempfile::tempdir().unwrap();
    let rune = tempfile::tempdir().unwrap();
    let home_runai = home.path().join(".runai");
    let data = rune.path().to_path_buf();

    // 1) CLI `list` under RUNE_DATA_DIR=B — honor-env, already correct.
    let out = runai_cmd()
        .arg("list")
        .env("HOME", home.path())
        .env("RUNE_DATA_DIR", rune.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .unwrap();
    assert!(out.status.success(), "runai list failed: {out:?}");
    assert!(
        data.join("runai.db").exists(),
        "CLI must create the DB under RUNE_DATA_DIR"
    );
    assert!(
        !home_runai.exists(),
        "CLI must not create HOME/.runai when RUNE_DATA_DIR is set"
    );

    // 2) Server under the SAME env. Register a user; it must land in the
    //    same B DB the CLI created — not in HOME/.runai.
    let port = free_port();
    let mut child = runai_cmd()
        .arg("server")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--mode")
        .arg("team")
        .env("HOME", home.path())
        .env("RUNE_DATA_DIR", rune.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_for_port(port, Duration::from_secs(8)), "server bind");

    let base = format!("http://127.0.0.1:{port}");
    let r = http()
        .post(format!("{base}/users/register"))
        .json(&json!({"username": "cli_share", "password": "pw share 1234"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        count(
            &data.join("runai.db"),
            "SELECT COUNT(*) FROM users WHERE username='cli_share'"
        ),
        1,
        "server must register into the SAME data dir the CLI used (B)"
    );
    assert!(
        !home_runai.join("runai.db").exists(),
        "server must not split into HOME/.runai"
    );

    assert_real_home_untouched(&before, "cli_share");
}
