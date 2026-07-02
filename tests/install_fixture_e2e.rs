//! Offline physical e2e for the GitHub / market **install write path**
//! (issue #28).
//!
//! The install pipeline (`core::market::{fetch,install,github_mirror}` →
//! `manager::install_github_repo_filtered_for`) is high-risk: it downloads
//! from GitHub and writes to `<data>/skills/<name>/` (public pool) or
//! `<data>/users/<uid>/skills/<name>/` (private pool). The only prior
//! coverage was `#[ignore]`-gated and needed live GitHub — so `cargo test`
//! never exercised the落盘 path.
//!
//! This suite stands up a **local axum fixture server** that serves a minimal
//! GitHub tree API + Contents API + raw-file responses, then points the real
//! `runai` binary at it via the test-only env overrides
//! `RUNAI_GITHUB_API_BASE` / `RUNAI_GITHUB_RAW_BASE`. No network required, so
//! it runs in default CI. The `#[ignore]`d live-network suites
//! (`tui_market_install_e2e`, `install_test`) stay as-is for pre-release
//! manual smoke.
//!
//! Assertions per acceptance criteria (#28):
//! - Physical file lands at the right pool path (public vs per-user private).
//! - Downloaded `SKILL.md` bytes match the fixture exactly.
//! - `resources.owner_user_id` is `NULL` for public, `= uid` for private.
//! - Nothing leaks into the *other* pool.
//!
//! Skipped on Windows (matches the rest of the integration suite: symlinks +
//! HOME mocking).
#![cfg(not(target_os = "windows"))]

use std::collections::BTreeSet;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};
use tempfile::TempDir;

// ─── fixture repo contents ────────────────────────────────────────────────

const OWNER: &str = "fixture-owner";
const REPO: &str = "fixture-repo";
const BRANCH: &str = "main";
const SKILL: &str = "hello-skill";

const SKILL_MD: &str = "---\nname: hello-skill\ndescription: offline fixture skill for the install e2e\n---\n\n# Hello Skill\n\nThis is the fixture SKILL.md body.\n";
const RUN_SH: &str = "#!/usr/bin/env bash\necho fixture-helper\n";
const README: &str = "# fixture repo root readme (must NOT be pulled into the skill dir)\n";

/// (repo-relative path, file body). The tree API, Contents API, and raw
/// endpoints are all derived from this single map so the three views stay
/// consistent.
fn fixture_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("README.md", README),
        ("hello-skill/SKILL.md", SKILL_MD),
        ("hello-skill/scripts/run.sh", RUN_SH),
    ]
}

fn lookup_file(path: &str) -> Option<&'static str> {
    fixture_files()
        .into_iter()
        .find(|(p, _)| *p == path)
        .map(|(_, body)| body)
}

// ─── local axum fixture server (started once for the whole test binary) ────

struct Mock {
    api_base: String,
    raw_base: String,
}

fn mock() -> &'static Mock {
    static MOCK: OnceLock<Mock> = OnceLock::new();
    MOCK.get_or_init(start_mock)
}

fn start_mock() -> Mock {
    use axum::Router;
    use axum::extract::Path as AxPath;
    use axum::http::StatusCode;
    use axum::routing::get;

    // Tree API: GET /api/repos/{owner}/{repo}/git/trees/{branch}?recursive=1
    async fn tree(AxPath((_owner, _repo, _branch)): AxPath<(String, String, String)>) -> String {
        let nodes: Vec<Value> = fixture_files()
            .into_iter()
            .map(|(p, _)| json!({ "path": p }))
            .collect();
        json!({ "tree": nodes }).to_string()
    }

    // Contents API (root): GET /api/repos/{owner}/{repo}/contents?ref=...
    async fn contents_root(AxPath((_owner, _repo)): AxPath<(String, String)>) -> String {
        contents_for("")
    }

    // Contents API (sub-path): GET /api/repos/{owner}/{repo}/contents/{*path}?ref=...
    async fn contents_path(
        AxPath((_owner, _repo, path)): AxPath<(String, String, String)>,
    ) -> String {
        contents_for(&path)
    }

    // Raw file: GET /raw/{owner}/{repo}/{branch}/{*path}
    async fn raw(
        AxPath((_owner, _repo, _branch, path)): AxPath<(String, String, String, String)>,
    ) -> Result<String, StatusCode> {
        lookup_file(&path)
            .map(|b| b.to_string())
            .ok_or(StatusCode::NOT_FOUND)
    }

    let app = Router::new()
        .route("/api/repos/{owner}/{repo}/git/trees/{branch}", get(tree))
        .route("/api/repos/{owner}/{repo}/contents", get(contents_root))
        .route(
            "/api/repos/{owner}/{repo}/contents/{*path}",
            get(contents_path),
        )
        .route("/raw/{owner}/{repo}/{branch}/{*path}", get(raw));

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("mock runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("mock bind");
            let port = listener.local_addr().unwrap().port();
            tx.send(port).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    let port = rx.recv().expect("mock port");
    assert!(
        wait_for_port(port, Duration::from_secs(5)),
        "fixture mock server never bound"
    );
    Mock {
        api_base: format!("http://127.0.0.1:{port}/api"),
        raw_base: format!("http://127.0.0.1:{port}/raw"),
    }
}

/// Build the GitHub Contents API JSON array for the immediate children of
/// `dir` (dir "" == repo root).
fn contents_for(dir: &str) -> String {
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    let mut files: BTreeSet<String> = BTreeSet::new();
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for (path, _) in fixture_files() {
        if !prefix.is_empty() && !path.starts_with(&prefix) {
            continue;
        }
        let rest = &path[prefix.len()..];
        match rest.split_once('/') {
            Some((seg, _)) => {
                dirs.insert(seg.to_string());
            }
            None => {
                files.insert(rest.to_string());
            }
        }
    }
    let mut items: Vec<Value> = Vec::new();
    for name in &dirs {
        items.push(json!({
            "name": name,
            "path": format!("{prefix}{name}"),
            "type": "dir",
        }));
    }
    for name in &files {
        items.push(json!({
            "name": name,
            "path": format!("{prefix}{name}"),
            "type": "file",
        }));
    }
    Value::Array(items).to_string()
}

// ─── generic helpers ──────────────────────────────────────────────────────

fn runai_cmd() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
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

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Row (directory, owner_user_id) for the freshest resource of `name`.
fn db_row(db_path: &Path, name: &str) -> Option<(String, Option<String>)> {
    let conn = rusqlite::Connection::open(db_path).ok()?;
    conn.query_row(
        "SELECT directory, owner_user_id FROM resources WHERE name = ?1 \
         ORDER BY installed_at DESC LIMIT 1",
        rusqlite::params![name],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
    )
    .ok()
}

fn make_home() -> TempDir {
    let home = tempfile::tempdir().expect("tmp HOME");
    for cli in ["claude", "codex", "gemini", "opencode"] {
        std::fs::create_dir_all(home.path().join(format!(".{cli}/skills"))).unwrap();
    }
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    home
}

/// A file planted OUTSIDE the isolated HOME. Its content + mtime are
/// snapshotted at plant time; `assert_untouched` proves the install pipeline
/// never wrote anywhere but the test HOME — the third acceptance assertion in
/// issue #28 ("不越界写测试 HOME 之外任何路径"). Mirrors the sentinel pattern in
/// `tests/safety_e2e.rs::scan_does_not_touch_paths_outside_isolated_home`.
struct Sentinel {
    _dir: TempDir,
    file: PathBuf,
    mtime: std::time::SystemTime,
}

impl Sentinel {
    fn plant() -> Self {
        let dir = tempfile::tempdir().expect("sentinel tmp");
        let file = dir.path().join("dont-touch-me.txt");
        std::fs::write(&file, "untouchable\n").unwrap();
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        Sentinel {
            _dir: dir,
            file,
            mtime,
        }
    }

    fn assert_untouched(&self) {
        assert_eq!(
            std::fs::read_to_string(&self.file).unwrap(),
            "untouchable\n",
            "install wrote to a file OUTSIDE the isolated HOME"
        );
        assert_eq!(
            std::fs::metadata(&self.file).unwrap().modified().unwrap(),
            self.mtime,
            "install touched the mtime of a file OUTSIDE the isolated HOME"
        );
    }
}

// ─── PUBLIC POOL: real `runai install owner/repo` (tree API path) ──────────

#[test]
fn install_lands_in_public_pool_with_null_owner() {
    let m = mock();
    let home = make_home();
    let data = home.path().join(".runai");
    let sentinel = Sentinel::plant();

    let out = runai_cmd()
        .args(["install", &format!("{OWNER}/{REPO}")])
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNAI_GITHUB_API_BASE", &m.api_base)
        .env("RUNAI_GITHUB_RAW_BASE", &m.raw_base)
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .expect("spawn runai install");
    dump(&out, "install (public pool)");
    assert!(
        out.status.success(),
        "install must exit 0 against fixture server"
    );

    // Physical files landed in the public pool, byte-exact.
    let skill_dir = data.join("skills").join(SKILL);
    assert_eq!(
        std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        SKILL_MD,
        "SKILL.md content must match the fixture exactly"
    );
    assert_eq!(
        std::fs::read_to_string(skill_dir.join("scripts/run.sh")).unwrap(),
        RUN_SH,
        "nested skill asset must be downloaded too"
    );
    // Root README must NOT be pulled into the skill directory.
    assert!(
        !skill_dir.join("README.md").exists(),
        "repo-root README must not leak into the skill dir"
    );

    // DB row: public pool ⇒ owner_user_id IS NULL, directory == managed path.
    let (dir, owner) = db_row(&data.join("runai.db"), SKILL).expect("resources row after install");
    assert_eq!(owner, None, "public install must have NULL owner_user_id");
    assert_eq!(
        PathBuf::from(&dir).canonicalize().unwrap(),
        skill_dir.canonicalize().unwrap(),
        "DB directory must point at the managed public-pool dir"
    );

    // No private pool created as a side effect.
    let users_root = data.join("users");
    assert!(
        !users_root.exists()
            || std::fs::read_dir(&users_root)
                .map(|d| d.count())
                .unwrap_or(0)
                == 0,
        "public install must not create any <data>/users/<uid>/ subtree"
    );

    // No writes escaped the isolated HOME.
    sentinel.assert_untouched();
}

// ─── PUBLIC POOL: real `runai market-install` (Contents API path) ──────────

fn plant_market_cache(data_dir: &Path) {
    let sources = json!([{
        "owner": OWNER,
        "repo": REPO,
        "branch": BRANCH,
        "skill_prefix": "",
        "label": "Fixture Source",
        "description": "offline fixture source",
        "builtin": false,
        "enabled": true,
    }]);
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::write(
        data_dir.join("market-sources.json"),
        serde_json::to_string_pretty(&sources).unwrap(),
    )
    .unwrap();

    let cache_dir = data_dir.join("market-cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let cache = json!([{
        "name": SKILL,
        "repo_path": SKILL,
        "source_label": "Fixture Source",
        "source_repo": format!("{OWNER}/{REPO}"),
        "branch": BRANCH,
    }]);
    std::fs::write(
        cache_dir.join(format!("{OWNER}_{REPO}.json")),
        cache.to_string(),
    )
    .unwrap();
}

#[test]
fn market_install_lands_in_public_pool_via_contents_api() {
    let m = mock();
    let home = make_home();
    let data = home.path().join(".runai");
    plant_market_cache(&data);
    let sentinel = Sentinel::plant();

    let out = runai_cmd()
        .args(["market-install", SKILL])
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNAI_GITHUB_API_BASE", &m.api_base)
        .env("RUNAI_GITHUB_RAW_BASE", &m.raw_base)
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .expect("spawn runai market-install");
    dump(&out, "market-install (public pool)");
    assert!(out.status.success(), "market-install must exit 0");

    let skill_dir = data.join("skills").join(SKILL);
    assert_eq!(
        std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        SKILL_MD,
    );
    assert_eq!(
        std::fs::read_to_string(skill_dir.join("scripts/run.sh")).unwrap(),
        RUN_SH,
        "Contents-API recursion must fetch the nested scripts/run.sh",
    );

    let (dir, owner) = db_row(&data.join("runai.db"), SKILL).expect("resources row");
    assert_eq!(owner, None);
    assert_eq!(
        PathBuf::from(&dir).canonicalize().unwrap(),
        skill_dir.canonicalize().unwrap(),
    );

    // No writes escaped the isolated HOME.
    sentinel.assert_untouched();
}

// ─── PRIVATE POOL: real server team-mode POST /api/install/github ──────────

struct ServerGuard {
    child: Child,
    home: TempDir,
    port: u16,
}

impl ServerGuard {
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn data_dir(&self) -> PathBuf {
        self.home.path().join(".runai")
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_team_server(m: &Mock) -> ServerGuard {
    let home = make_home();
    let port = free_port();
    let child = runai_cmd()
        .args([
            "server",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--mode",
            "team",
        ])
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNAI_GITHUB_API_BASE", &m.api_base)
        .env("RUNAI_GITHUB_RAW_BASE", &m.raw_base)
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn runai server");
    let guard = ServerGuard { child, home, port };
    assert!(
        wait_for_port(guard.port, Duration::from_secs(10)),
        "runai server never bound"
    );
    guard
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
}

#[test]
fn server_github_install_lands_in_private_pool_with_owner_stamp() {
    let m = mock();
    let sentinel = Sentinel::plant();
    let s = spawn_team_server(m);

    // Register alice → capture uid + api_key.
    let reg = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": "alice", "password": "pw alice 1234"}))
        .send()
        .unwrap();
    assert_eq!(
        reg.status().as_u16(),
        201,
        "register must succeed in team mode"
    );
    let body: Value = reg.json().unwrap();
    let uid = body["user_id"].as_str().unwrap().to_string();
    let key = body["api_key"].as_str().unwrap().to_string();

    // Install into alice's private pool via the dashboard github endpoint.
    let resp = http()
        .post(format!("{}/api/install/github", s.base_url()))
        .bearer_auth(&key)
        .json(&json!({"source": format!("{OWNER}/{REPO}")}))
        .send()
        .unwrap();
    let status = resp.status().as_u16();
    let install_body: Value = resp.json().unwrap();
    assert_eq!(
        status, 200,
        "install/github must succeed (body={install_body})"
    );
    let installed = install_body["installed"].as_array().unwrap();
    assert!(
        installed.iter().any(|v| v.as_str() == Some(SKILL)),
        "response must report the installed skill, got {install_body}"
    );

    // Physical file under the per-user private pool, byte-exact.
    let data = s.data_dir();
    let private_dir = data.join("users").join(&uid).join("skills").join(SKILL);
    assert_eq!(
        std::fs::read_to_string(private_dir.join("SKILL.md")).unwrap(),
        SKILL_MD,
        "private install SKILL.md must match fixture"
    );
    assert_eq!(
        std::fs::read_to_string(private_dir.join("scripts/run.sh")).unwrap(),
        RUN_SH,
    );

    // The public pool must be untouched — no cross-pool leak.
    assert!(
        !data.join("skills").join(SKILL).exists(),
        "private install must NEVER write to the public <data>/skills/ pool"
    );

    // DB row: owner_user_id == uid, directory inside the private subtree.
    let (dir, owner) = db_row(&data.join("runai.db"), SKILL).expect("resources row after install");
    assert_eq!(
        owner.as_deref(),
        Some(uid.as_str()),
        "private install row must be stamped with the owner uid"
    );
    assert_eq!(
        PathBuf::from(&dir).canonicalize().unwrap(),
        private_dir.canonicalize().unwrap(),
        "DB directory must point at the per-user private dir"
    );

    // No writes escaped the server's isolated HOME.
    sentinel.assert_untouched();
}
