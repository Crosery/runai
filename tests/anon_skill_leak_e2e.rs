//! A1: the compat disk-fallback in `resolve_skill_dir_scoped` must not let a
//! stray public-pool directory shadow a PRIVATE skill row and serve it to
//! anonymous callers.
//!
//! Live bug this pins: `agent-browser` had a DB row private to a (deleted) user
//! AND a leftover physical dir in the public pool `<data>/skills/agent-browser`.
//! The fallback `if paths.skills_dir().join(name).exists()` served that dir to
//! anyone, with no owner/auth check → anon `/skills/get|file|bundle` → 200.
//!
//! Contract:
//!   - a name whose DB row is PRIVATE → anon get/file/bundle = 404, even if a
//!     stray public-pool dir of that name exists on disk.
//!   - a genuine public skill (DB row OR bare on-disk dir with no private
//!     shadow) → anon still 200 (the recommend-hook compat window stays open).

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::json;
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
    fn data(&self) -> std::path::PathBuf {
        self.home.path().join(".runai")
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
fn register(s: &ServerGuard, u: &str, p: &str) {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
}
fn plant_skill(dir: &std::path::Path, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), body).unwrap();
}
fn anon_get(s: &ServerGuard, path: &str) -> u16 {
    http()
        .get(format!("{}{}", s.base_url(), path))
        .send()
        .unwrap()
        .status()
        .as_u16()
}
fn anon_post(s: &ServerGuard, path: &str) -> u16 {
    http()
        .post(format!("{}{}", s.base_url(), path))
        .send()
        .unwrap()
        .status()
        .as_u16()
}

#[test]
fn anon_cannot_read_private_skill_via_stray_public_dir() {
    use runai::core::db::Database;
    use runai::core::resource::{Resource, ResourceKind, Source};

    let s = spawn_server();
    register(&s, "admin", "pw admin 1234"); // users non-empty → private_data_locked

    let data = s.data();
    let db = Database::open(&data.join("runai.db")).unwrap();

    let mk = |id: &str, name: &str, dir: std::path::PathBuf, owner: Option<&str>| Resource {
        id: id.into(),
        name: name.into(),
        kind: ResourceKind::Skill,
        description: "d".into(),
        directory: dir,
        source: Source::Local {
            path: std::path::PathBuf::from("/tmp"),
        },
        installed_at: 0,
        enabled: std::collections::HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: owner.map(String::from),
        publish_status: "draft".into(),
    };

    // (1) PRIVATE skill "secret" owned by usr_ghost, real private dir + a STRAY
    //     public-pool dir of the same name (the leak vector).
    let priv_dir = data.join("users/usr_ghost/skills/secret");
    plant_skill(&priv_dir, "---\nname: secret\n---\nSECRET_BODY\n");
    db.insert_resource(&mk(
        "u:usr_ghost:local:secret",
        "secret",
        priv_dir,
        Some("usr_ghost"),
    ))
    .unwrap();
    plant_skill(
        &data.join("skills/secret"),
        "---\nname: secret\n---\nSTRAY_PUBLIC_SHADOW\n",
    );

    // (2) genuine PUBLIC skill with a DB row.
    let pub_dir = data.join("skills/pubrow");
    plant_skill(&pub_dir, "---\nname: pubrow\n---\nPUBLIC_BODY\n");
    db.insert_resource(&mk("local:pubrow", "pubrow", pub_dir, None))
        .unwrap();

    // (3) compat: a bare on-disk public dir with NO DB row (un-scanned tree).
    plant_skill(
        &data.join("skills/pubnorow"),
        "---\nname: pubnorow\n---\nPUBLIC_NOROW\n",
    );
    drop(db);

    // ── the leak: private skill must be 404 for anon on all three routes ──
    assert_eq!(
        anon_post(&s, "/skills/get/secret"),
        404,
        "anon must NOT read a private skill via a stray public-pool dir"
    );
    assert_eq!(anon_get(&s, "/skills/file/secret/SKILL.md"), 404);
    assert_eq!(anon_get(&s, "/skills/bundle/secret"), 404);

    // ── compat preserved: genuine public skills stay anon-readable ──
    assert_eq!(
        anon_post(&s, "/skills/get/pubrow"),
        200,
        "public skill with a DB row must stay anon-readable (hook compat)"
    );
    assert_eq!(
        anon_post(&s, "/skills/get/pubnorow"),
        200,
        "bare on-disk public dir (no private shadow) must stay anon-readable"
    );
}
