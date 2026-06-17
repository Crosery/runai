//! Served-asset guard for the ownership-based Library scope bar.
//!
//! The "已安装" list separates skills by OWNERSHIP — 公共库 (owner_user_id
//! NULL, `<data>/skills/`) vs 私有库 (owner present, `<data>/users/<uid>/`) —
//! and "我的库" = 私有库 ∪ 已订阅公共. This is a frontend (web/js + index.html)
//! behavior; the project's real-browser Playwright harness is pending
//! (issue #20), so this test pins the SHIPPED assets: the dashboard HTML must
//! carry the 私有库 scope button and app.js must filter by ownership
//! (`dataset.owner` + a `private` scope branch). It is a structural
//! regression gate, NOT a rendering assertion.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
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
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
fn get_text(s: &ServerGuard, path: &str) -> String {
    let r = http()
        .get(format!("{}{}", s.base_url(), path))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "GET {path} should 200");
    r.text().unwrap()
}

#[test]
fn dashboard_html_ships_private_scope_button() {
    let s = spawn_server();
    let html = get_text(&s, "/");
    assert!(
        html.contains(r#"data-scope="private""#),
        "Library scope bar must ship a 私有库 (data-scope=\"private\") button"
    );
    // The public scope button keeps its data-scope but is now ownership-based.
    assert!(html.contains(r#"data-scope="public""#));
    assert!(html.contains(r#"id="scope-private-count""#));
}

#[test]
fn app_css_does_not_hide_scope_segment_for_admin() {
    // Regression: an obsolete rule hid `.scope-group:not(#select-mode-group)`
    // for team-mode admin, leaving a blank band in the scope bar and hiding
    // the ownership scope buttons. The segment is now ownership-based and
    // meaningful for admin, so that hide must stay gone.
    let s = spawn_server();
    let css = get_text(&s, "/app.css");
    assert!(
        !css.contains("is-admin:not(.mode-owner) .scope-group"),
        "the obsolete admin scope-group hide must not be reintroduced"
    );
}

#[test]
fn app_js_filters_installed_list_by_ownership() {
    let s = spawn_server();
    let js = get_text(&s, "/app.js");
    // Rows carry their owner so the scope filter doesn't have to guess.
    assert!(
        js.contains("dataset.owner"),
        "skill rows must tag owner via dataset.owner for ownership scope filtering"
    );
    // The filter has a dedicated private branch (owner present) and the
    // public branch is keyed on ownership, not library subscription.
    assert!(
        js.contains("'private'") || js.contains("\"private\""),
        "renderSkills must handle a 'private' scope"
    );
}
