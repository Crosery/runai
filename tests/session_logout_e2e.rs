//! E1: `POST /api/me/logout-everywhere` rotates the api_key, invalidating every
//! existing copy (other browsers / proxies / `~/.runai-identity`) — unlike
//! plain `/auth/logout` which only clears the current cookie.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};
use tempfile::TempDir;

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
    let child = Command::cargo_bin("runai")
        .unwrap()
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

#[test]
fn logout_everywhere_rotates_and_invalidates_old_key() {
    let s = spawn_server();
    // register → returns the api_key (k1).
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": "alice", "password": "pw alice 1234"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let k1: String = r.json::<Value>().unwrap()["api_key"]
        .as_str()
        .unwrap()
        .into();

    // k1 works.
    assert_eq!(
        http()
            .get(format!("{}/api/me", s.base_url()))
            .bearer_auth(&k1)
            .send()
            .unwrap()
            .status()
            .as_u16(),
        200
    );

    // anon cannot logout-everywhere.
    assert_eq!(
        http()
            .post(format!("{}/api/me/logout-everywhere", s.base_url()))
            .send()
            .unwrap()
            .status()
            .as_u16(),
        401
    );

    // logout-everywhere with k1 → rotates the key.
    assert_eq!(
        http()
            .post(format!("{}/api/me/logout-everywhere", s.base_url()))
            .bearer_auth(&k1)
            .send()
            .unwrap()
            .status()
            .as_u16(),
        200
    );

    // k1 is now dead.
    assert_eq!(
        http()
            .get(format!("{}/api/me", s.base_url()))
            .bearer_auth(&k1)
            .send()
            .unwrap()
            .status()
            .as_u16(),
        401,
        "the old api_key must be invalid after logout-everywhere rotated it"
    );

    // re-login mints a fresh working key.
    let r = http()
        .post(format!("{}/auth/login", s.base_url()))
        .json(&json!({"username": "alice", "password": "pw alice 1234"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let k2: String = r.json::<Value>().unwrap()["api_key"]
        .as_str()
        .unwrap()
        .into();
    assert_ne!(k1, k2, "re-login must mint a different key");
    assert_eq!(
        http()
            .get(format!("{}/api/me", s.base_url()))
            .bearer_auth(&k2)
            .send()
            .unwrap()
            .status()
            .as_u16(),
        200
    );
}
