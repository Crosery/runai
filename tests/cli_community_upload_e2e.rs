//! Issue #29: `runai community upload` (the CLI entrypoint, distinct from
//! `runai-client upload`'s bash script) must land the skill in the
//! caller's PRIVATE pool (`publish_status='draft'`), never the shared
//! community pool via the now admin-only `POST /api/community/upload`.
//!
//! Spawns a real `runai server --mode team` inside an isolated HOME, then
//! drives a second `runai` invocation (`community upload` / `community
//! publish`) against it as an ordinary HTTP client — exactly the sequence
//! a remote teammate's shell would run. Asserts on:
//!   - physical landing site: `<server HOME>/.runai/users/<uid>/skills/<name>/`
//!     (never `<server HOME>/.runai/community/...`).
//!   - `resources.publish_status = 'draft'` immediately after upload.
//!   - `community publish` flips it to `pending` (once the enrich gate is
//!     force-satisfied via direct sqlite write — the test sandbox has no
//!     LLM provider, so real enrichment never completes).

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

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
    fn db_path(&self) -> std::path::PathBuf {
        self.home_path().join(".runai/runai.db")
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
    assert_eq!(resp.status().as_u16(), 201, "register {username}");
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

/// Run `runai community <args>` against `server`, authenticated as
/// `account` via the `RUNAI_API_KEY` env var (skips needing a real
/// `~/.runai-identity` file on the CLI-caller side). The CLI process gets
/// its OWN throwaway HOME — it must never touch the server's HOME/DB
/// directly, only over HTTP, exactly like a real remote teammate's shell.
fn run_community_cli(
    caller_home: &std::path::Path,
    server: &ServerGuard,
    account: &Account,
    args: &[&str],
) -> std::process::Output {
    let mut cmd = runai_cmd();
    cmd.arg("community")
        .args(args)
        .arg("--server")
        .arg(server.base_url())
        .env("HOME", caller_home)
        .env("RUNAI_API_KEY", &account.api_key)
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR");
    cmd.output().expect("spawn runai community")
}

#[test]
fn cli_upload_lands_in_private_pool_as_draft() {
    let server = spawn_team_server();
    let alice = register(&server, "alice", "correct horse battery staple");

    // The CLI-caller side needs its own scratch HOME (unrelated to the
    // server's) plus a real skill directory with SKILL.md to tar up.
    let caller_home = tempfile::tempdir().expect("caller HOME");
    let skill_dir = caller_home.path().join("my-cli-skill");
    std::fs::create_dir_all(&skill_dir).expect("mkdir skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-cli-skill\n---\n\n# my-cli-skill\n\nuploaded via CLI\n",
    )
    .expect("write SKILL.md");

    let out = run_community_cli(
        caller_home.path(),
        &server,
        &alice,
        &[
            "upload",
            "--path",
            skill_dir.to_str().unwrap(),
            "--name",
            "my-cli-skill",
        ],
    );
    assert!(
        out.status.success(),
        "runai community upload must succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("private pool") && stdout.contains("draft"),
        "stdout must confirm private/draft landing, not community pool; got {stdout:?}"
    );
    assert!(
        stdout.contains("publish"),
        "stdout must hint at the `community publish` follow-up; got {stdout:?}"
    );

    // Physical assertion: lands under the SERVER's per-user private pool,
    // never the community pool.
    let private_dir = server
        .home_path()
        .join(".runai/users")
        .join(&alice.user_id)
        .join("skills/my-cli-skill");
    assert!(
        private_dir.join("SKILL.md").is_file(),
        "expected private-pool SKILL.md at {:?}",
        private_dir
    );
    let community_dir = server
        .home_path()
        .join(".runai/community")
        .join(&alice.user_id)
        .join("my-cli-skill");
    assert!(
        !community_dir.exists(),
        "CLI upload must NOT write to the community pool directly; found {:?}",
        community_dir
    );

    // DB assertion: publish_status='draft' on the resources row.
    let conn = rusqlite::Connection::open(server.db_path()).expect("open server db");
    let publish_status: String = conn
        .query_row(
            "SELECT publish_status FROM resources WHERE name = ?1 AND owner_user_id = ?2",
            rusqlite::params!["my-cli-skill", alice.user_id],
            |r| r.get(0),
        )
        .expect("resources row for uploaded skill");
    assert_eq!(publish_status, "draft", "fresh CLI upload must be draft");
}

#[test]
fn cli_publish_flips_draft_to_pending_once_enrich_gate_satisfied() {
    let server = spawn_team_server();
    let alice = register(&server, "alice", "correct horse battery staple");

    let caller_home = tempfile::tempdir().expect("caller HOME");
    let skill_dir = caller_home.path().join("publishable");
    std::fs::create_dir_all(&skill_dir).expect("mkdir skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: publishable\n---\n\n# publishable\n",
    )
    .expect("write SKILL.md");

    let upload_out = run_community_cli(
        caller_home.path(),
        &server,
        &alice,
        &[
            "upload",
            "--path",
            skill_dir.to_str().unwrap(),
            "--name",
            "publishable",
        ],
    );
    assert!(upload_out.status.success(), "upload must succeed");

    // Before enrichment lands, publish must 400 — the CLI surfaces that as
    // a non-zero exit with the server's error body on stderr.
    let early_publish = run_community_cli(
        caller_home.path(),
        &server,
        &alice,
        &["publish", "publishable"],
    );
    assert!(
        !early_publish.status.success(),
        "publish before enrich must fail"
    );
    let early_stderr = String::from_utf8_lossy(&early_publish.stderr);
    assert!(
        early_stderr.contains("400") || early_stderr.to_lowercase().contains("enrich"),
        "pre-enrich publish failure should mention the gate; got {early_stderr:?}"
    );

    // Force-satisfy the enrich gate the same way community_market_e2e.rs
    // does — the test sandbox has no LLM provider configured.
    let conn = rusqlite::Connection::open(server.db_path()).expect("open server db");
    conn.execute(
        "INSERT INTO resource_ai_summary
            (owner_user_id, name, summary, updated_at, llm_score, search_doc, router_card, source_hash, prompt_hash, format_key)
         VALUES (?1, ?2, 'forced test summary', ?3, 8, 'forced test summary', 'forced test summary', 'test', 'test', 'test')
         ON CONFLICT(owner_user_id, name) DO UPDATE SET summary = excluded.summary",
        rusqlite::params![alice.user_id, "publishable", chrono::Utc::now().timestamp()],
    )
    .expect("force-insert resource_ai_summary");
    drop(conn);

    let publish_out = run_community_cli(
        caller_home.path(),
        &server,
        &alice,
        &["publish", "publishable"],
    );
    assert!(
        publish_out.status.success(),
        "publish after enrich must succeed; stderr={}",
        String::from_utf8_lossy(&publish_out.stderr)
    );

    let conn = rusqlite::Connection::open(server.db_path()).expect("open server db");
    let publish_status: String = conn
        .query_row(
            "SELECT publish_status FROM resources WHERE name = ?1 AND owner_user_id = ?2",
            rusqlite::params!["publishable", alice.user_id],
            |r| r.get(0),
        )
        .expect("resources row for published skill");
    assert_eq!(
        publish_status, "pending",
        "publish-request must flip draft to pending"
    );
}
