//! Phase 3 e2e: the runai-client activation/file/feedback/sync/flush protocol
//! (PLANNING §1.3) — bash companion. Covers activate, cached support-file
//! reads, feedback, sync, flush, the client-cache layout invariant (NEVER
//! ~/.runai/skills/), and the durable outbox.
//!
//! All tests run against an isolated HOME tempdir + RUNE_DATA_DIR; the
//! real `~/.runai/` is never touched. The server runs in team mode so
//! /skills/use + /feedback enforce real auth (the client registers a
//! user and carries the api_key).

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use sha2::{Digest, Sha256};
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

struct Server {
    child: Child,
    home: TempDir,
    port: u16,
}

impl Server {
    fn spawn() -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
        let port = free_port();
        let data_dir = home.path().join(".runai");
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
            .env("RUNE_DATA_DIR", &data_dir)
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn runai server");
        let s = Self { child, home, port };
        assert!(wait_for_port(port, Duration::from_secs(8)));
        s
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn plant(&self, name: &str, body: &str, extras: &[(&str, &str)]) {
        let dir = self.home().join(".runai/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n"),
        )
        .unwrap();
        for (rel, content) in extras {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let out = runai_cmd()
            .arg("scan")
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.home().join(".runai"))
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "scan failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn usage_count(&self, name: &str) -> i64 {
        let conn = rusqlite::Connection::open(self.home().join(".runai/runai.db")).unwrap();
        conn.query_row(
            "SELECT usage_count FROM resources WHERE name=?1",
            rusqlite::params![name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

fn rewrite_server_url(body: &str, new_url: &str) -> String {
    body.lines()
        .map(|line| {
            if let Some(rest) = line.trim_start().strip_prefix("SERVER_URL=\"")
                && rest.ends_with('"')
            {
                return format!("SERVER_URL=\"{new_url}\"");
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// Install the runai-client companion into a fresh client HOME, registering
/// `username`. Returns (client_home, api_key). The server must already be
/// running and have a planted skill set if the test needs one.
fn register_key(server: &Server, username: &str) -> String {
    let resp = http()
        .post(format!("{}/users/register", server.base()))
        .json(&serde_json::json!({
            "username": username,
            "password": "correct-horse-battery-staple"
        }))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let v: serde_json::Value = resp.json().unwrap();
    v["api_key"].as_str().unwrap().to_string()
}

fn install_client(server: &Server, username: &str) -> (TempDir, String) {
    let body = http()
        .get(format!("{}/install", server.base()))
        .send()
        .unwrap()
        .text()
        .unwrap();
    let script_body = rewrite_server_url(&body, &server.base());
    let script_dir = tempfile::tempdir().unwrap();
    let script_path = script_dir.path().join("install.sh");
    std::fs::write(&script_path, script_body).unwrap();

    let client_home = tempfile::tempdir().unwrap();
    let output = Command::new("bash")
        .arg(script_path.as_os_str())
        .env("HOME", client_home.path())
        .env("RUNAI_USERNAME", username)
        .env("RUNAI_PASSWORD", "correct-horse-battery-staple")
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install.sh failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let identity = std::fs::read_to_string(client_home.path().join(".runai-identity")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&identity).unwrap();
    let api_key = v["api_key"].as_str().unwrap().to_string();
    (client_home, api_key)
}

fn client_bin(home: &Path) -> PathBuf {
    home.join(".local/bin/runai-client")
}

/// Run the runai-client companion with creds injected via env.
fn run_client(
    client_home: &Path,
    server: &Server,
    api_key: &str,
    args: &[&str],
) -> (bool, String, String) {
    let out = Command::new("bash")
        .arg(client_bin(client_home))
        .args(args)
        .env("HOME", client_home)
        .env("RUNAI_SERVER", server.base())
        .env("RUNAI_API_KEY", api_key)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn cache_key(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn cache_dir(home: &Path, server: &Server, skill: &str) -> PathBuf {
    home.join(".runai/client-cache/servers")
        .join(cache_key(&server.base()))
        .join("skills")
        .join(cache_key(skill))
}

// ─── activate ────────────────────────────────────────────────────────────

#[test]
fn activate_warm_path_prints_skill_md_and_records_usage() {
    let server = Server::spawn();
    server.plant("foo", "foo body", &[("references/ref-a.md", "ref body")]);
    let (home, key) = install_client(&server, &format!("act-{}", std::process::id()));

    let (ok, stdout, stderr) = run_client(
        home.path(),
        &server,
        &key,
        &[
            "activate",
            "foo",
            "--session-id",
            "rnai_sess_11111111111111111111111111111111",
        ],
    );
    assert!(ok, "activate should succeed: stderr=\n{stderr}");
    assert!(
        stdout.contains("# foo"),
        "stdout must print SKILL.md: {stdout}"
    );
    // server recorded usage
    assert_eq!(server.usage_count("foo"), 1);
    // client cached SKILL.md
    let cached =
        std::fs::read_to_string(cache_dir(home.path(), &server, "foo").join("SKILL.md")).unwrap();
    assert!(cached.contains("# foo"));
    let cached_ref = cache_dir(home.path(), &server, "foo").join("files/references/ref-a.md");
    assert!(
        cached_ref.exists(),
        "activate must cache the whole skill, including references: {cached_ref:?}"
    );
    assert_eq!(std::fs::read_to_string(cached_ref).unwrap(), "ref body");
    // 铁律: NEVER write to ~/.runai/skills/ on the client
    assert!(
        !home.path().join(".runai/skills").exists(),
        "client must NOT write to ~/.runai/skills/"
    );
}

#[test]
fn activate_default_caches_entire_skill_bundle() {
    let server = Server::spawn();
    server.plant(
        "impeccable",
        "needs supporting references",
        &[
            ("references/rubric.md", "rubric body"),
            ("scripts/check.sh", "#!/bin/sh\necho ok\n"),
        ],
    );
    let (home, key) = install_client(&server, &format!("bundle-{}", std::process::id()));

    let (ok, stdout, stderr) = run_client(home.path(), &server, &key, &["activate", "impeccable"]);
    assert!(ok, "activate should succeed: stderr=\n{stderr}");
    assert!(stdout.contains("# impeccable"));

    let files = cache_dir(home.path(), &server, "impeccable").join("files");
    assert_eq!(
        std::fs::read_to_string(files.join("references/rubric.md")).unwrap(),
        "rubric body"
    );
    assert_eq!(
        std::fs::read_to_string(files.join("scripts/check.sh")).unwrap(),
        "#!/bin/sh\necho ok\n"
    );
    assert!(
        !home.path().join(".runai/skills").exists(),
        "client cache must never use ~/.runai/skills"
    );
}

#[test]
fn file_subcommand_prints_cached_support_file_without_server() {
    let mut server = Server::spawn();
    server.plant(
        "impeccable",
        "needs supporting references",
        &[("references/rubric.md", "rubric body")],
    );
    let (home, key) = install_client(&server, &format!("file-{}", std::process::id()));
    let (ok, _, stderr) = run_client(home.path(), &server, &key, &["activate", "impeccable"]);
    assert!(ok, "activate should succeed: stderr=\n{stderr}");

    let _ = server.child.kill();
    let _ = server.child.wait();

    let (ok2, stdout, stderr2) = run_client(
        home.path(),
        &server,
        &key,
        &["file", "impeccable", "references/rubric.md"],
    );
    assert!(
        ok2,
        "runai-client file should read cached support files offline: stderr=\n{stderr2}"
    );
    assert_eq!(stdout, "rubric body");
}

#[test]
fn file_subcommand_missing_bundle_file_does_not_blame_activation() {
    let server = Server::spawn();
    server.plant(
        "ppt-anything",
        "runtime assets live elsewhere",
        &[("tools/README.md", "tool docs")],
    );
    let (home, key) = install_client(&server, &format!("filemiss-{}", std::process::id()));
    let (ok, _stdout, stderr) = run_client(
        home.path(),
        &server,
        &key,
        &[
            "file",
            "ppt-anything",
            "styles/anime-chibi-default/outline_template.md",
        ],
    );
    assert!(!ok, "missing bundle file must fail");
    assert!(
        stderr.contains("not found in skill bundle"),
        "stderr should explain the real boundary, got: {stderr}"
    );
    assert!(
        !stderr.contains("run runai-client activate ppt-anything first"),
        "missing server-side files are not fixed by activation: {stderr}"
    );
}

#[test]
fn file_subcommand_rejects_cache_traversal() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[("references/ref-a.md", "ref")]);
    let (home, key) = install_client(&server, &format!("filetr-{}", std::process::id()));
    let (ok, _stdout, stderr) =
        run_client(home.path(), &server, &key, &["file", "foo", "../SKILL.md"]);
    assert!(!ok, "traversal file read must fail");
    assert!(
        stderr.contains("traversal") || stderr.contains("refusing"),
        "stderr should explain traversal rejection: {stderr}"
    );
}

#[test]
fn cache_is_scoped_by_server_for_same_skill_name() {
    let server_a = Server::spawn();
    server_a.plant("foo", "server A body", &[]);
    let server_b = Server::spawn();
    server_b.plant("foo", "server B body", &[]);
    let (home, key_a) = install_client(&server_a, &format!("scope-a-{}", std::process::id()));
    let key_b = register_key(&server_b, &format!("scope-b-{}", std::process::id()));

    let (ok_a, stdout_a, stderr_a) =
        run_client(home.path(), &server_a, &key_a, &["activate", "foo"]);
    assert!(ok_a, "server A activate failed: {stderr_a}");
    assert!(stdout_a.contains("server A body"));

    let (ok_b, stdout_b, stderr_b) =
        run_client(home.path(), &server_b, &key_b, &["activate", "foo"]);
    assert!(ok_b, "server B activate failed: {stderr_b}");
    assert!(
        stdout_b.contains("server B body"),
        "server B must not reuse server A cache: {stdout_b}"
    );
    assert!(
        cache_dir(home.path(), &server_a, "foo")
            .join("SKILL.md")
            .exists()
    );
    assert!(
        cache_dir(home.path(), &server_b, "foo")
            .join("SKILL.md")
            .exists()
    );
    assert_ne!(
        cache_dir(home.path(), &server_a, "foo"),
        cache_dir(home.path(), &server_b, "foo")
    );
}

#[test]
fn activate_cache_hit_still_sends_or_queues_usage() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("ch-{}", std::process::id()));
    let (ok1, _, _) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(ok1);
    assert_eq!(server.usage_count("foo"), 1);
    // second activate — new event_id, cache hit, but usage still sent
    let (ok2, stdout, _) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(ok2);
    assert!(stdout.contains("# foo"));
    assert_eq!(
        server.usage_count("foo"),
        2,
        "second activate must bump usage too"
    );
}

#[test]
fn activate_server_down_with_warm_cache_queues_outbox_and_prints() {
    let mut server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("off-{}", std::process::id()));
    // warm the cache
    let (ok, _, _) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(ok);
    // kill the server
    let _ = server.child.kill();
    let _ = server.child.wait();
    // now activate offline — warm cache, should queue + print
    let (ok2, stdout, stderr) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(
        ok2,
        "offline activate with warm cache should succeed: stderr=\n{stderr}"
    );
    assert!(stdout.contains("# foo"), "should print cached SKILL.md");
    let outbox = cache_dir(home.path(), &server, "foo").join(".outbox");
    let entries: Vec<_> = std::fs::read_dir(&outbox)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(
        !entries.is_empty(),
        "outbox should have at least one queued event"
    );
    // verify the entry has required fields
    let f = entries[0].path();
    let raw = std::fs::read_to_string(&f).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["kind"], "usage");
    assert!(v["event_id"].as_str().is_some());
    assert!(v["skill"].as_str().is_some());
}

#[test]
fn activate_server_down_with_cold_cache_fails_without_printing() {
    let mut server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("cold-{}", std::process::id()));
    // kill server before any warm-up
    let _ = server.child.kill();
    let _ = server.child.wait();
    let (ok, stdout, _stderr) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(!ok, "cold-cache offline activate must fail");
    assert!(
        !stdout.contains("# foo"),
        "must NOT print SKILL.md when content unavailable: {stdout}"
    );
    // outbox still has the queued event (durable queue formed)
    let outbox = cache_dir(home.path(), &server, "foo").join(".outbox");
    let entries: Vec<_> = std::fs::read_dir(&outbox)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(!entries.is_empty(), "outbox should still record the event");
}

#[test]
fn activate_refresh_refetches_skill_md() {
    let server = Server::spawn();
    server.plant("foo", "v1", &[]);
    let (home, key) = install_client(&server, &format!("ref-{}", std::process::id()));
    let (ok, _, _) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(ok);
    // mutate server skill content + rescan
    let dir = server.home().join(".runai/skills/foo");
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: foo\ndescription: v2\n---\n\n# foo v2\n",
    )
    .unwrap();
    let out = runai_cmd()
        .arg("scan")
        .env("HOME", server.home())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", server.home().join(".runai"))
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let (ok2, stdout, _) = run_client(
        home.path(),
        &server,
        &key,
        &["activate", "foo", "--refresh"],
    );
    assert!(ok2);
    assert!(
        stdout.contains("foo v2"),
        "refresh should fetch new content: {stdout}"
    );
    let cached =
        std::fs::read_to_string(cache_dir(home.path(), &server, "foo").join("SKILL.md")).unwrap();
    assert!(cached.contains("foo v2"));
}

#[test]
fn activate_refresh_replaces_cached_support_files() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[("references/ref-a.md", "v1")]);
    let (home, key) = install_client(&server, &format!("refextra-{}", std::process::id()));
    let (ok, _, stderr) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(ok, "initial activate should succeed: stderr=\n{stderr}");

    std::fs::write(
        server.home().join(".runai/skills/foo/references/ref-a.md"),
        "v2",
    )
    .unwrap();
    let (ok2, _, stderr2) = run_client(
        home.path(),
        &server,
        &key,
        &["activate", "foo", "--refresh"],
    );
    assert!(ok2, "refresh should succeed: stderr=\n{stderr2}");
    let cached = cache_dir(home.path(), &server, "foo").join("files/references/ref-a.md");
    assert_eq!(std::fs::read_to_string(cached).unwrap(), "v2");
}

#[test]
fn activate_include_relpath_fetches_single_file_into_files_dir() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[("references/ref-a.md", "ref body content")]);
    let (home, key) = install_client(&server, &format!("inc-{}", std::process::id()));
    let (ok, _, stderr) = run_client(
        home.path(),
        &server,
        &key,
        &["activate", "foo", "--include", "references/ref-a.md"],
    );
    assert!(ok, "stderr=\n{stderr}");
    let fetched = cache_dir(home.path(), &server, "foo").join("files/references/ref-a.md");
    assert!(fetched.exists(), "included file should be cached");
    assert_eq!(
        std::fs::read_to_string(&fetched).unwrap(),
        "ref body content"
    );
}

#[test]
fn activate_all_fetches_bundle() {
    let server = Server::spawn();
    server.plant(
        "foo",
        "foo",
        &[
            ("references/ref-a.md", "a"),
            ("scripts/run.sh", "#!/bin/sh\n"),
        ],
    );
    let (home, key) = install_client(&server, &format!("all-{}", std::process::id()));
    let (ok, _, stderr) = run_client(home.path(), &server, &key, &["activate", "foo", "--all"]);
    assert!(ok, "stderr=\n{stderr}");
    let files = cache_dir(home.path(), &server, "foo").join("files");
    assert!(
        files.join("references/ref-a.md").exists() || files.join("ref-a.md").exists(),
        "bundle should include sibling files: listing = {:?}",
        std::fs::read_dir(&files)
            .map(|rd| rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
}

#[test]
fn activate_explicit_event_id_replay_is_noop_on_server() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("rep-{}", std::process::id()));
    let (ok1, _, _) = run_client(
        home.path(),
        &server,
        &key,
        &["activate", "foo", "--event-id", "e-fixed"],
    );
    assert!(ok1);
    assert_eq!(server.usage_count("foo"), 1);
    // replay same event_id + same payload (no --include either time)
    let (ok2, stdout, _) = run_client(
        home.path(),
        &server,
        &key,
        &["activate", "foo", "--event-id", "e-fixed"],
    );
    assert!(ok2, "replay should succeed (no-op)");
    assert!(stdout.contains("# foo"));
    assert_eq!(server.usage_count("foo"), 1, "replay must not bump usage");
}

#[test]
fn activate_explicit_event_id_conflict_returns_409_client_fails() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("conf-{}", std::process::id()));
    let (ok1, _, _) = run_client(
        home.path(),
        &server,
        &key,
        &["activate", "foo", "--event-id", "e-c", "--include", "a.md"],
    );
    assert!(ok1);
    // second call: same event_id, different payload (different --include)
    let (ok2, stdout, _) = run_client(
        home.path(),
        &server,
        &key,
        &["activate", "foo", "--event-id", "e-c", "--include", "b.md"],
    );
    assert!(!ok2, "conflict must fail the client");
    assert!(
        !stdout.contains("# foo"),
        "conflict must NOT print SKILL.md"
    );
    // outbox must not gain a conflict entry
    let outbox = cache_dir(home.path(), &server, "foo").join(".outbox");
    let entries: Vec<_> = std::fs::read_dir(&outbox)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(entries.is_empty(), "409 must not queue: {entries:?}");
}

#[test]
fn activate_traversal_include_is_rejected() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("tr-{}", std::process::id()));
    let (ok, _stdout, _stderr) = run_client(
        home.path(),
        &server,
        &key,
        &["activate", "foo", "--include", "../../etc/passwd"],
    );
    // fetch_file rejects traversal; activate still prints SKILL.md (the
    // include is best-effort), but the traversal file must NOT land.
    // We assert the dangerous file is absent regardless of exit.
    let bad = cache_dir(home.path(), &server, "foo").join("files/etc/passwd");
    assert!(
        !bad.exists(),
        "traversal include must not escape cache: {bad:?}"
    );
    let _ = ok;
}

// ─── feedback ────────────────────────────────────────────────────────────

#[test]
fn feedback_missing_note_exits_2() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("fbn-{}", std::process::id()));
    let (ok, _stdout, stderr) = run_client(home.path(), &server, &key, &["feedback", "foo"]);
    assert!(!ok, "feedback without --note must fail");
    assert!(
        stderr.contains("--note required"),
        "stderr must mention --note: {stderr}"
    );
}

#[test]
fn feedback_network_failure_queues_outbox() {
    let mut server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("fbq-{}", std::process::id()));
    // kill server → network failure → queue
    let _ = server.child.kill();
    let _ = server.child.wait();
    let (ok, _stdout, stderr) = run_client(
        home.path(),
        &server,
        &key,
        &["feedback", "foo", "--note", "x"],
    );
    assert!(
        ok,
        "offline feedback should queue and exit 0: stderr=\n{stderr}"
    );
    let outbox = cache_dir(home.path(), &server, "foo").join(".outbox");
    let entries: Vec<_> = std::fs::read_dir(&outbox)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(!entries.is_empty(), "offline feedback should queue");
    let raw = std::fs::read_to_string(entries[0].path()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["kind"], "feedback");
    assert!(v["note"].as_str().unwrap_or("").contains("x"));
}

#[test]
fn feedback_stale_bearer_does_not_queue() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, _key) = install_client(&server, &format!("fbs-{}", std::process::id()));
    // use a stale bearer — server returns 401, client must NOT queue
    let (ok, _stdout, _stderr) = run_client_with_key(
        home.path(),
        &server,
        "rnai_live_stale",
        &["feedback", "foo", "--note", "x"],
    );
    assert!(!ok, "stale-bearer feedback must fail");
    let outbox = cache_dir(home.path(), &server, "foo").join(".outbox");
    let entries: Vec<_> = std::fs::read_dir(&outbox)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(entries.is_empty(), "401 must NOT queue: {entries:?}");
}

fn run_client_with_key(
    client_home: &Path,
    server: &Server,
    api_key: &str,
    args: &[&str],
) -> (bool, String, String) {
    run_client(client_home, server, api_key, args)
}

// ─── sync ────────────────────────────────────────────────────────────────

#[test]
fn sync_prewarms_cache_and_never_writes_runai_skills() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[]);
    server.plant("bar", "bar", &[]);
    let (home, key) = install_client(&server, &format!("syn-{}", std::process::id()));
    let (ok, _stdout, stderr) = run_client(home.path(), &server, &key, &["sync", "foo", "bar"]);
    assert!(ok, "sync should succeed: stderr=\n{stderr}");
    assert!(
        cache_dir(home.path(), &server, "foo")
            .join("SKILL.md")
            .exists()
    );
    assert!(
        cache_dir(home.path(), &server, "bar")
            .join("SKILL.md")
            .exists()
    );
    assert!(
        !home.path().join(".runai/skills").exists(),
        "sync must NOT write ~/.runai/skills/"
    );
}

#[test]
fn sync_unknown_skill_skips_silently() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("syn404-{}", std::process::id()));
    let (ok, _stdout, stderr) = run_client(home.path(), &server, &key, &["sync", "missing"]);
    assert!(ok, "sync with unknown skill should exit 0 (skip silently)");
    assert!(
        stderr.contains("跳过") || stderr.contains("skip"),
        "should warn: {stderr}"
    );
    assert!(
        !cache_dir(home.path(), &server, "missing")
            .join("SKILL.md")
            .exists()
    );
}

#[test]
fn sync_then_activate_offline_works_pure_cache() {
    let mut server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("synoff-{}", std::process::id()));
    let (ok, _, _) = run_client(home.path(), &server, &key, &["sync", "foo"]);
    assert!(ok);
    // kill server, then activate — pure cache + outbox
    let _ = server.child.kill();
    let _ = server.child.wait();
    let (ok2, stdout, _) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(ok2, "offline activate after sync should succeed via cache");
    assert!(stdout.contains("# foo"));
    let outbox = cache_dir(home.path(), &server, "foo").join(".outbox");
    let entries: Vec<_> = std::fs::read_dir(&outbox)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(
        !entries.is_empty(),
        "offline activate should queue usage event"
    );
}

// ─── flush ───────────────────────────────────────────────────────────────

#[test]
fn flush_drains_outbox_after_server_returns() {
    let mut server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("fl-{}", std::process::id()));
    // warm the cache first so an offline activate queues + prints OK
    let (ok_warm, _, _) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(ok_warm, "warm activate should succeed");
    // kill server, queue an event via offline activate (warm cache)
    let _ = server.child.kill();
    let _ = server.child.wait();
    let (ok, _, _) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(ok, "offline activate with warm cache should queue + print");
    let outbox = cache_dir(home.path(), &server, "foo").join(".outbox");
    let before: Vec<_> = std::fs::read_dir(&outbox)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(!before.is_empty(), "outbox should have the queued event");
    // The client's RUNAI_SERVER still points at the dead port. Spawn a
    // fresh server and flush against it. The queued event_id is unknown
    // to the new server, so /skills/use returns 200 First → entry deleted.
    drop(server);
    let server2 = Server::spawn();
    server2.plant("foo", "foo", &[]);
    let (ok2, stdout, stderr) = run_client(home.path(), &server2, &key, &["flush"]);
    assert!(ok2, "flush should exit 0: stderr=\n{stderr}");
    assert!(
        stdout.contains("已重放") || stdout.contains("replayed"),
        "flush should report replay: {stdout}"
    );
    let after: Vec<_> = std::fs::read_dir(&outbox)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(
        after.is_empty(),
        "outbox should be drained after flush: {after:?}"
    );
}

// ─── cache layout invariants ─────────────────────────────────────────────

#[test]
fn cache_dir_permissions_no_wider_than_0700() {
    let server = Server::spawn();
    server.plant("foo", "foo", &[]);
    let (home, key) = install_client(&server, &format!("perm-{}", std::process::id()));
    let (ok, _, stderr) = run_client(home.path(), &server, &key, &["activate", "foo"]);
    assert!(ok, "stderr=\n{stderr}");
    let root = home.path().join(".runai/client-cache");
    let server_root = root.join("servers").join(cache_key(&server.base()));
    let skills_root = server_root.join("skills");
    let skill_dir = cache_dir(home.path(), &server, "foo");
    for p in [
        root.as_path(),
        server_root.as_path(),
        skills_root.as_path(),
        skill_dir.as_path(),
    ] {
        let md = std::fs::metadata(p).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = md.permissions().mode();
        assert_eq!(mode & 0o077, 0, "{:?} perms too wide: {:o}", p, mode);
    }
}

#[test]
fn cache_skill_md_content_matches_server_byte_for_byte() {
    let server = Server::spawn();
    let body = "---\nname: byte\ndescription: exact\n---\n\n# byte\n\nline1\nline2\n";
    server.plant("byte", "exact", &[]);
    // overwrite with a known-exact body
    std::fs::write(server.home().join(".runai/skills/byte/SKILL.md"), body).unwrap();
    let out = runai_cmd()
        .arg("scan")
        .env("HOME", server.home())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", server.home().join(".runai"))
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let (home, key) = install_client(&server, &format!("byte-{}", std::process::id()));
    let (ok, _, stderr) = run_client(
        home.path(),
        &server,
        &key,
        &["activate", "byte", "--refresh"],
    );
    assert!(ok, "stderr=\n{stderr}");
    let cached =
        std::fs::read_to_string(cache_dir(home.path(), &server, "byte").join("SKILL.md")).unwrap();
    assert_eq!(cached, body, "cached SKILL.md must be byte-equal to server");
}
