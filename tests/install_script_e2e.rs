//! Phase P2 e2e: client install-script templating + non-interactive auth.
//!
//! Covers PLANNING.md §1.2:
//!
//! 1. Owner mode `/install` and `/install.ps1` return 404 (single-user
//!    self-serve never exposes a remote client script).
//! 2. Team mode `/install` returns 200 + a bash script that does NOT contain
//!    any runai-binary management commands (`runai scan` / `runai discover`
//!    / `runai doctor`). These are server-box-only operations and must
//!    never leak into a client-facing surface.
//! 3. The generated script can register a user fully non-interactively
//!    when `RUNAI_USERNAME` + `RUNAI_PASSWORD` are supplied via env, with
//!    `HOME` redirected to a temporary directory so the host's real
//!    `~/.runai-identity` / `~/.claude/settings.json` are never touched.
//!
//! Every test spawns the real `runai server` binary inside an isolated
//! HOME — same pattern as `server_mode_e2e.rs`. The bash phase that
//! exercises the rendered script also runs inside its own tempdir
//! HOME, so a failure / partial run cannot pollute the developer's box.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers (mirror of server_mode_e2e.rs — kept duplicated so each
// integration test file stays independently runnable) ──────────────────────

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

fn spawn_server(home: TempDir, port: u16, mode: &str) -> ServerGuard {
    std::fs::create_dir_all(home.path().join(".runai/skills")).expect("pre-create .runai/skills");

    let mut cmd = runai_cmd();
    cmd.arg("server")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--mode")
        .arg(mode)
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
        "runai server did not bind 127.0.0.1:{port} (mode={mode}) within 8s"
    );
    guard
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client")
}

fn make_home() -> TempDir {
    tempfile::tempdir().expect("create tmp HOME")
}

/// Replace the line `SERVER_URL="<whatever>"` in a rendered install
/// script with `SERVER_URL="<new_url>"`. Used by the e2e runner because
/// `guess_server_url` deliberately rewrites loopback Host headers to the
/// box's LAN IPv4 (so teammates get a reachable URL), but our test server
/// only binds 127.0.0.1 — without this rewrite the script can't connect.
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

fn write_test_skill(home_path: &Path, name: &str) {
    let dir = home_path.join(".runai/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test skill\n---\n\n# {name}\n"),
    )
    .unwrap();
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// PLANNING §1.2 (a): owner mode hides the remote client surface entirely.
/// `/install` and `/install.ps1` BOTH 404; same for `/uninstall*` so a
/// teammate cannot ping the sibling endpoint either.
#[test]
fn owner_mode_install_endpoints_return_404() {
    let home = make_home();
    let port = free_port();
    let server = spawn_server(home, port, "owner");

    let client = http_client();
    for path in &["/install", "/install.ps1", "/uninstall", "/uninstall.ps1"] {
        let resp = client
            .get(format!("{}{}", server.base_url(), path))
            .send()
            .unwrap_or_else(|e| panic!("GET {path}: {e}"));
        assert_eq!(
            resp.status().as_u16(),
            404,
            "owner mode {path} must 404; got {}",
            resp.status()
        );
        let body = resp.text().unwrap_or_default();
        assert!(
            body.is_empty(),
            "owner mode {path} must return empty body; got {body:?}"
        );
    }
}

/// PLANNING §1.2 (b): team mode `/install` returns 200 and the script body
/// does NOT contain any runai-binary management commands. `scan` /
/// `discover` / `doctor` are server-box-only operations and would 404 on
/// a remote client — but more importantly, even mentioning them risks the
/// remote teammate copy-pasting commands they can't run.
#[test]
fn team_mode_install_script_excludes_binary_management_commands() {
    let home = make_home();
    let port = free_port();
    let server = spawn_server(home, port, "team");

    let client = http_client();
    let resp = client
        .get(format!("{}/install", server.base_url()))
        .send()
        .expect("GET /install");
    assert_eq!(resp.status().as_u16(), 200, "team mode /install must 200");
    let body = resp.text().expect("install body");
    assert!(
        !body.is_empty(),
        "team mode /install body must be non-empty"
    );

    // Server URL must be substituted away from the literal placeholder.
    assert!(
        !body.contains("{SERVER_URL}"),
        "{{SERVER_URL}} placeholder must be replaced before serving"
    );
    // Section markers themselves must be stripped — the served file is
    // the clean assembled script, not the raw template. We check for the
    // marker FORM (`START ===` / `END ===` suffixes) rather than the
    // bare `RUNAI_SECTION` token, because the script's documentation
    // comment block legitimately references `RUNAI_SECTION:<mode>-only`
    // to explain to a human reading the source what the grammar is.
    for marker_form in &[
        "RUNAI_SECTION:owner-only START ===",
        "RUNAI_SECTION:owner-only END ===",
        "RUNAI_SECTION:team-only START ===",
        "RUNAI_SECTION:team-only END ===",
    ] {
        assert!(
            !body.contains(marker_form),
            "marker line {marker_form:?} must be stripped from served body"
        );
    }
    // Binary subcommands are the load-bearing assertion of §1.2.
    for forbidden in &["runai scan", "runai discover", "runai doctor"] {
        assert!(
            !body.contains(forbidden),
            "team mode /install leaked {forbidden:?} to remote client; body=\n{body}"
        );
    }
    // Sanity-check the script is the expected team-mode flow: it must
    // POST to /auth/login and /users/register (the only auth endpoints
    // a remote client should know about).
    assert!(
        body.contains("/auth/login"),
        "script should call /auth/login"
    );
    assert!(
        body.contains("/users/register"),
        "script should call /users/register"
    );
    // PLANNING §1.3: the team-mode install body must ship the
    // runai-client activation/feedback/sync/flush subcommands + the
    // client-cache dir (NEVER ~/.runai/skills/ as cache). Activation
    // is now client-mediated, not a bare curl against /skills/get.
    for needle in &[
        "runai-client activate",
        "runai-client feedback",
        "client-cache",
    ] {
        assert!(
            body.contains(needle),
            "team mode /install body must contain {needle:?}"
        );
    }
}

/// PLANNING §1.2 (c): non-interactive flow — env vars supplant TTY prompts.
/// Run the rendered script in a SEPARATE tempdir HOME so the host's real
/// `~/.runai-identity` / `~/.claude/settings.json` are NEVER touched.
#[test]
fn non_interactive_install_registers_user_via_env_vars() {
    let server_home = make_home();
    write_test_skill(server_home.path(), "demo-skill");
    let port = free_port();
    let server = spawn_server(server_home, port, "team");

    // Fetch the rendered script.
    let client = http_client();
    let body = client
        .get(format!("{}/install", server.base_url()))
        .send()
        .expect("GET /install")
        .text()
        .expect("install body");
    assert!(body.contains("/auth/login"));

    // Install script substitution switched from `guess_server_url`
    // (loopback → LAN IPv4) to `request_origin` (verbatim Host header),
    // so when the test curls 127.0.0.1:port the rendered SERVER_URL is
    // already the loopback URL — no rewrite needed. We assert the new
    // contract directly here AND keep `rewrite_server_url` available for
    // sites that may still need fixups (it's a no-op when the URL already
    // matches).
    let loopback_url = server.base_url();
    assert!(
        body.contains(&format!("SERVER_URL=\"{loopback_url}\"")),
        "install script must echo request origin (loopback) into SERVER_URL; got body excerpt:\n{}",
        body.lines().take(30).collect::<Vec<_>>().join("\n")
    );
    let script_body = rewrite_server_url(&body, &loopback_url);
    assert!(
        script_body.contains(&format!("SERVER_URL=\"{loopback_url}\"")),
        "post-rewrite SERVER_URL guard",
    );

    // Persist to a temp file and run under a fully isolated HOME so this
    // test cannot mutate the developer's real `~/.runai-identity` or
    // `~/.claude/settings.json`.
    let script_dir = tempfile::tempdir().expect("script tempdir");
    let script_path = script_dir.path().join("install.sh");
    std::fs::write(&script_path, script_body).expect("write rendered install.sh");

    let client_home = tempfile::tempdir().expect("client HOME tempdir");
    let username = format!("e2e-{}", std::process::id());
    let password = "correct-horse-battery-staple";

    let output = Command::new("bash")
        .arg(script_path.as_os_str())
        .env("HOME", client_home.path())
        .env("RUNAI_USERNAME", &username)
        .env("RUNAI_PASSWORD", password)
        // Block any stray attempt to spawn another runai dashboard from a
        // hook invocation — the script itself doesn't spawn anything, but
        // belt-and-braces against any subprocess that might.
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run rendered install.sh");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "non-interactive install.sh failed:\nexit={}\nstdout=\n{}\nstderr=\n{}",
        output.status,
        stdout,
        stderr,
    );

    // ~/.runai-identity must exist inside the client tempdir, mode 600,
    // with the api_key the server minted.
    let identity_path = client_home.path().join(".runai-identity");
    assert!(
        identity_path.exists(),
        "install.sh did not write {}; stdout=\n{stdout}\nstderr=\n{stderr}",
        identity_path.display(),
    );
    let identity_raw =
        std::fs::read_to_string(&identity_path).expect("read .runai-identity from client home");
    let identity_json: serde_json::Value =
        serde_json::from_str(&identity_raw).expect("identity is JSON");
    let api_key = identity_json
        .get("api_key")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        api_key.starts_with("rnai_"),
        "expected runai api_key in identity; got {identity_raw}"
    );
    assert_eq!(
        identity_json.get("username").and_then(|v| v.as_str()),
        Some(username.as_str()),
        "identity username should match registered name"
    );
    // First user registered in this fresh server → admin.
    assert_eq!(
        identity_json.get("is_admin").and_then(|v| v.as_bool()),
        Some(true),
    );

    // Hook + settings should have been wired up too (this proves the
    // default phase ran all three steps).
    let hook_path = client_home.path().join(".runai-hook.sh");
    assert!(
        hook_path.exists(),
        "install.sh did not write {}; stdout=\n{stdout}",
        hook_path.display(),
    );
    let settings_path = client_home.path().join(".claude/settings.json");
    assert!(
        settings_path.exists(),
        "install.sh did not write {}; stdout=\n{stdout}",
        settings_path.display(),
    );
    let settings_raw = std::fs::read_to_string(&settings_path).expect("read settings.json");
    assert!(
        settings_raw.contains(".runai-hook.sh"),
        "settings.json should reference .runai-hook.sh; got {settings_raw}"
    );

    // Cross-verify with the server: the api_key in the identity file
    // really authenticates against /api/me.
    let me = client
        .get(format!("{}/api/me", server.base_url()))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .send()
        .expect("GET /api/me");
    assert_eq!(
        me.status().as_u16(),
        200,
        "freshly-registered api_key did not authenticate against /api/me"
    );
    let me_json: serde_json::Value = me.json().expect("/api/me body");
    assert_eq!(
        me_json.get("username").and_then(|v| v.as_str()),
        Some(username.as_str())
    );
}

/// Smoke test the rendered script's `--help` flag. Agents discovering the
/// installer via `bash install.sh --help` should get a non-empty help
/// page that names the env vars and phase flags from PLANNING §1.2.
#[test]
fn rendered_script_supports_help_flag() {
    let server_home = make_home();
    let port = free_port();
    let server = spawn_server(server_home, port, "team");

    let client = http_client();
    let body = client
        .get(format!("{}/install", server.base_url()))
        .send()
        .expect("GET /install")
        .text()
        .expect("install body");

    let script_dir = tempfile::tempdir().expect("script tempdir");
    let script_path = script_dir.path().join("install.sh");
    std::fs::write(&script_path, body).expect("write install.sh");

    let client_home = tempfile::tempdir().expect("client HOME tempdir");
    let output = Command::new("bash")
        .arg(script_path.as_os_str())
        .arg("--help")
        .env("HOME", client_home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("bash install.sh --help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "install.sh --help failed: exit={}\nstdout={}\nstderr={}",
        output.status,
        stdout,
        String::from_utf8_lossy(&output.stderr),
    );
    for needle in &[
        "RUNAI_USERNAME",
        "RUNAI_PASSWORD",
        "--register-only",
        "--login-only",
        "--hook-only",
    ] {
        assert!(
            stdout.contains(needle),
            "help output missing {needle:?}; got:\n{stdout}"
        );
    }

    // --help must NOT touch HOME — verify no identity / hook / settings
    // file was created as a side effect.
    assert!(!client_home.path().join(".runai-identity").exists());
    assert!(!client_home.path().join(".runai-hook.sh").exists());
    assert!(!client_home.path().join(".claude/settings.json").exists());
}
