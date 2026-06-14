//! §1.4 e2e: the bash `runai-client` companion command installed by the
//! team-mode install script. Validates the three subcommands
//! (upload / list / install) and the fzf-missing fallback path.
//!
//! Strategy: run the rendered `/install` script into a tempdir HOME (same
//! pattern as `install_script_e2e.rs`), which writes
//! `<HOME>/.local/bin/runai-client`. Then drive that script directly with
//! `RUNAI_SERVER` + `RUNAI_API_KEY` env vars so we don't have to depend on
//! `~/.runai-identity` parsing (those tests are covered elsewhere).
//!
//! The fzf-missing path is exercised by pointing PATH at an empty
//! tempdir — that mirrors what a teammate gets on a fresh box without
//! fzf installed, and confirms the script prints an actionable install
//! hint instead of crashing.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers (mirror of install_script_e2e.rs) ───────────────────────────

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

fn spawn_server(home: TempDir, port: u16) -> ServerGuard {
    std::fs::create_dir_all(home.path().join(".runai/skills")).expect("pre-create skills");
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
        port,
    };
    assert!(
        wait_for_port(port, Duration::from_secs(8)),
        "runai server did not bind 127.0.0.1:{port} within 8s"
    );
    guard
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client")
}

fn rewrite_server_url(body: &str, new_url: &str) -> String {
    body.lines()
        .map(|line| {
            if let Some(rest) = line.trim_start().strip_prefix("SERVER_URL=\"") {
                if rest.ends_with('"') {
                    return format!("SERVER_URL=\"{new_url}\"");
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// Run the rendered install script into a fresh tempdir HOME, registering
/// `<username>` non-interactively. Returns (home, identity_path, api_key).
fn install_client_into(server: &ServerGuard, username: &str) -> (TempDir, PathBuf, String) {
    let client = http_client();
    let body = client
        .get(format!("{}/install", server.base_url()))
        .send()
        .expect("GET /install")
        .text()
        .expect("install body");
    let loopback_url = server.base_url();
    let script_body = rewrite_server_url(&body, &loopback_url);
    let script_dir = tempfile::tempdir().expect("script tempdir");
    let script_path = script_dir.path().join("install.sh");
    std::fs::write(&script_path, script_body).expect("write rendered install.sh");

    let client_home = tempfile::tempdir().expect("client HOME");
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
        .expect("run install.sh");
    assert!(
        output.status.success(),
        "install.sh failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let identity_path = client_home.path().join(".runai-identity");
    let identity_raw = std::fs::read_to_string(&identity_path).expect("read identity");
    let identity_json: serde_json::Value =
        serde_json::from_str(&identity_raw).expect("identity json");
    let api_key = identity_json
        .get("api_key")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(api_key.starts_with("rnai_"), "expected runai api_key");
    (client_home, identity_path, api_key)
}

/// Locate the installed runai-client script under <home>/.local/bin/.
fn runai_client_path(home: &Path) -> PathBuf {
    let p = home.join(".local/bin/runai-client");
    assert!(
        p.exists(),
        "install.sh did not write {} — check that the team-only section installs runai-client",
        p.display()
    );
    p
}

/// Build a tiny throw-away SKILL.md directory under `parent` and return
/// its path. Used as the source for `runai-client upload --path`.
fn make_test_skill_dir(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: e2e test skill\n---\n\n# {name}\n"),
    )
    .unwrap();
    dir
}

// ─── tests ──────────────────────────────────────────────────────────────

/// `runai-client --help` (and bare `runai-client`) must print a non-empty
/// help page that names the three subcommands.
#[test]
fn runai_client_help_lists_subcommands() {
    let server_home = tempfile::tempdir().expect("server HOME");
    let port = free_port();
    let server = spawn_server(server_home, port);
    let (home, _id, _key) = install_client_into(&server, &format!("help-{}", std::process::id()));
    let bin = runai_client_path(home.path());
    let out = Command::new("bash")
        .arg(&bin)
        .arg("--help")
        .env("HOME", home.path())
        .output()
        .expect("run runai-client --help");
    assert!(out.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for needle in &["upload", "list", "install", "--help"] {
        assert!(
            stdout.contains(needle),
            "--help output missing {needle}: {stdout}"
        );
    }
}

/// `runai-client upload --path <dir> --name <name>` runs non-interactively
/// (no fzf needed) and uploads the skill to the caller's PRIVATE pool
/// (PLANNING §1.4 rewrite — default is no longer community/upload).
/// After upload the skill must be visible to `runai-client list-mine`.
/// The community-list endpoint is not exercised here because that flow
/// now requires admin approval (covered by admin_publish_approve_e2e).
#[test]
fn runai_client_upload_then_list_roundtrip() {
    let server_home = tempfile::tempdir().expect("server HOME");
    let port = free_port();
    let server = spawn_server(server_home, port);
    let username = format!("up-{}", std::process::id());
    let (home, _id, api_key) = install_client_into(&server, &username);
    let bin = runai_client_path(home.path());

    let skill_parent = tempfile::tempdir().expect("skill parent tempdir");
    let skill_dir = make_test_skill_dir(skill_parent.path(), "upload-test-skill");

    // Drive runai-client directly with env-supplied creds so we don't
    // depend on identity-file parsing (covered by install_script_e2e).
    let upload_out = Command::new("bash")
        .arg(&bin)
        .arg("upload")
        .arg("--path")
        .arg(&skill_dir)
        .arg("--name")
        .arg("upload-test-skill")
        .env("HOME", home.path())
        .env("RUNAI_SERVER", server.base_url())
        .env("RUNAI_API_KEY", &api_key)
        .output()
        .expect("run runai-client upload");
    assert!(
        upload_out.status.success(),
        "upload should succeed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&upload_out.stdout),
        String::from_utf8_lossy(&upload_out.stderr),
    );

    // list-mine should now show the freshly-uploaded skill (PLANNING
    // §1.4 rewrite — `list` is the community-pool view which now
    // requires admin approval; `list-mine` is the per-user view).
    let list_out = Command::new("bash")
        .arg(&bin)
        .arg("list-mine")
        .env("HOME", home.path())
        .env("RUNAI_SERVER", server.base_url())
        .env("RUNAI_API_KEY", &api_key)
        .output()
        .expect("run runai-client list-mine");
    assert!(
        list_out.status.success(),
        "list-mine should succeed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&list_out.stdout),
        String::from_utf8_lossy(&list_out.stderr),
    );
    let stdout = String::from_utf8_lossy(&list_out.stdout);
    assert!(
        stdout.contains("upload-test-skill"),
        "list-mine output should contain the uploaded skill name:\n{stdout}"
    );
    assert!(
        stdout.contains("NAME"),
        "list-mine output should contain a header row:\n{stdout}"
    );
    assert!(
        stdout.contains("draft"),
        "fresh upload must show publish_status='draft':\n{stdout}"
    );
}

/// TUI mode without fzf installed must fail loudly with an install hint
/// rather than silently no-op. The driver uses an empty PATH dir so any
/// host-installed fzf is hidden — this is the only reliable way to
/// simulate the missing-fzf state inside CI.
#[test]
fn runai_client_upload_without_fzf_explains_how_to_install() {
    let server_home = tempfile::tempdir().expect("server HOME");
    let port = free_port();
    let server = spawn_server(server_home, port);
    let username = format!("nofzf-{}", std::process::id());
    let (home, _id, api_key) = install_client_into(&server, &username);
    let bin = runai_client_path(home.path());

    // Empty PATH dir → no fzf, no bash, no tar, no curl — but we still
    // need bash to run the script. Trick: put a minimal PATH that has
    // /bin + /usr/bin (system tools) but does NOT include any directory
    // that might carry fzf. This works on macOS / Linux developer boxes
    // where fzf is typically under /opt/homebrew/bin or /usr/local/bin.
    let minimal_path = "/bin:/usr/bin:/usr/sbin:/sbin";

    let out = Command::new("bash")
        .arg(&bin)
        .arg("upload")
        // Intentionally NO --path; TUI mode is selected, which requires fzf.
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", minimal_path)
        .env("RUNAI_SERVER", server.base_url())
        .env("RUNAI_API_KEY", &api_key)
        .output()
        .expect("run runai-client upload without fzf");
    assert!(
        !out.status.success(),
        "upload without fzf + no --path must fail; stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The user MUST see actionable text:
    //   - the word "fzf" so they know what is missing,
    //   - at least one install command hint (brew / apt / dnf / pacman),
    //   - the non-interactive fallback `--path <dir>`.
    // Catching any one of the install hints is enough — different distros
    // get different suggestions and the test box only matches its own.
    assert!(
        stderr.contains("fzf"),
        "missing fzf hint should mention 'fzf':\n{stderr}"
    );
    assert!(
        stderr.contains("--path"),
        "missing fzf hint should point at --path fallback:\n{stderr}"
    );
    assert!(
        stderr.contains("brew install fzf")
            || stderr.contains("apt install fzf")
            || stderr.contains("dnf install fzf")
            || stderr.contains("pacman -S fzf"),
        "missing fzf hint should suggest at least one package-manager command:\n{stderr}"
    );
}
