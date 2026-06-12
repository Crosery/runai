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

use std::net::TcpListener;
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

static SERVER_START_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

fn wait_for_dashboard(port: u16, timeout: Duration) -> bool {
    let url = format!("http://127.0.0.1:{port}/");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(300))
        .build()
        .expect("reqwest readiness client");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if client
            .get(&url)
            .send()
            .map(|resp| resp.status().is_success())
            .unwrap_or(false)
        {
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

    let _start_guard = SERVER_START_LOCK
        .lock()
        .expect("install_script_e2e server start lock poisoned");
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
    let mut child = cmd.spawn().expect("spawn runai server");

    if !wait_for_dashboard(port, Duration::from_secs(15)) {
        let _ = child.kill();
        let output = child
            .wait_with_output()
            .expect("collect failed runai server output");
        panic!(
            "runai server did not answer HTTP readiness at 127.0.0.1:{port} (mode={mode}) within 15s\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    ServerGuard {
        child,
        _home: home,
        port,
    }
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

fn rendered_install_script(server: &ServerGuard) -> String {
    let client = http_client();
    let script_body = client
        .get(format!("{}/install", server.base_url()))
        .send()
        .expect("GET /install")
        .text()
        .expect("install body");
    rewrite_server_url(&script_body, &server.base_url())
}

fn run_install_script(
    script_body: &str,
    client_home: &Path,
    username: Option<&str>,
    password: Option<&str>,
) -> std::process::Output {
    run_install_script_with_args(script_body, client_home, username, password, &[])
}

fn run_install_script_with_args(
    script_body: &str,
    client_home: &Path,
    username: Option<&str>,
    password: Option<&str>,
    args: &[&str],
) -> std::process::Output {
    let script_dir = tempfile::tempdir().expect("script tempdir");
    let script_path = script_dir.path().join("install.sh");
    std::fs::write(&script_path, script_body).expect("write rendered install.sh");

    let mut cmd = Command::new("bash");
    cmd.arg(script_path.as_os_str())
        .args(args)
        .env("HOME", client_home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(username) = username {
        cmd.env("RUNAI_USERNAME", username);
    }
    if let Some(password) = password {
        cmd.env("RUNAI_PASSWORD", password);
    }
    cmd.output().expect("run rendered install.sh")
}

fn fetch_uninstall_script(server: &ServerGuard) -> String {
    http_client()
        .get(format!("{}/uninstall", server.base_url()))
        .send()
        .expect("GET /uninstall")
        .text()
        .expect("uninstall body")
}

fn run_uninstall_script(script_body: &str, client_home: &Path) -> std::process::Output {
    let script_dir = tempfile::tempdir().expect("uninstall script tempdir");
    let script_path = script_dir.path().join("uninstall.sh");
    std::fs::write(&script_path, script_body).expect("write uninstall.sh");

    Command::new("bash")
        .arg(script_path.as_os_str())
        .env("HOME", client_home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run uninstall.sh")
}

fn settings_hook_commands(path: &Path) -> Vec<String> {
    let raw = std::fs::read_to_string(path).expect("read settings.json");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("settings.json is valid JSON");
    json.pointer("/hooks/UserPromptSubmit")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .flat_map(|group| {
            group
                .get("hooks")
                .and_then(|hooks| hooks.as_array())
                .into_iter()
                .flatten()
        })
        .filter_map(|hook| {
            hook.get("command")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect()
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
}

/// Regression for the documented user command:
/// `curl -fsSL http://<server>/install | bash`.
///
/// In this shape bash's stdin is the installer pipe, not an interactive
/// terminal. A new device with no env credentials must fail with actionable
/// text and must drain the remaining script bytes so curl does not surface
/// `(23) Failure writing output to destination`.
#[test]
fn curl_pipe_new_device_without_credentials_fails_without_broken_pipe_noise() {
    let home = make_home();
    let port = free_port();
    let server = spawn_server(home, port, "team");
    let client_home = tempfile::tempdir().expect("client HOME tempdir");

    let output = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "curl --limit-rate 1024 -fsSL {}/install | bash",
            server.base_url()
        ))
        .env("HOME", client_home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNAI_USERNAME")
        .env_remove("RUNAI_PASSWORD")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run curl | bash installer");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "new-device curl pipe without credentials should fail, not silently install:\nstdout=\n{stdout}\nstderr=\n{stderr}"
    );
    assert!(
        stderr.contains("stdin is the installer pipe") || stderr.contains("run non-interactively"),
        "failure should explain how to run non-interactively:\nstdout=\n{stdout}\nstderr=\n{stderr}"
    );
    assert!(
        !stderr.contains("curl: (23)") && !stdout.contains("curl: (23)"),
        "installer must drain stdin before exiting so curl does not report broken pipe:\nstdout=\n{stdout}\nstderr=\n{stderr}"
    );
    assert!(
        !client_home.path().join(".runai-identity").exists(),
        "failed curl-pipe install must not create identity"
    );
    assert!(
        !client_home.path().join(".runai-hook.sh").exists(),
        "failed curl-pipe install must not create hook"
    );
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

    // `guess_server_url` rewrites a loopback Host header to the box's
    // LAN IPv4 so teammate-facing scripts get a URL their machine can
    // reach. In the test runner the server is only bound to 127.0.0.1,
    // so we have to point the script back at the actual loopback URL
    // before executing — without this, `curl http://<LAN-IP>:<port>`
    // hangs/fails. The substitution stays on a stable anchor (the
    // `SERVER_URL=` assignment near the top of the template) so a
    // future docstring rewrite doesn't accidentally match.
    let loopback_url = server.base_url();
    let script_body = rewrite_server_url(&body, &loopback_url);
    assert!(
        script_body.contains(&format!("SERVER_URL=\"{loopback_url}\"")),
        "test rewrite_server_url failed: script body did not contain expected loopback URL",
    );

    let client_home = tempfile::tempdir().expect("client HOME tempdir");
    let username = format!("e2e-{}", std::process::id());
    let password = "correct-horse-battery-staple";

    let output = run_install_script(
        &script_body,
        client_home.path(),
        Some(&username),
        Some(password),
    );

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

    assert!(
        stdout.contains("install complete"),
        "installer should print an install-complete summary; stdout=\n{stdout}"
    );
    assert_eq!(
        stdout.lines().last(),
        Some("install complete"),
        "final installer line must be machine-parseable; stdout=\n{stdout}"
    );
    for field in [
        "account", "password", "api_key", "server", "identity", "hook", "config", "client",
    ] {
        assert!(
            stdout.contains(field),
            "installer summary missing {field}; stdout=\n{stdout}"
        );
    }
    assert!(
        !stdout.contains(&format!("api_key   {api_key}")),
        "installer leaked raw api_key in stdout"
    );
    assert!(
        !stdout.contains(password),
        "installer leaked plaintext password in stdout"
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

#[test]
fn existing_identity_must_verify_with_server_before_prompt_skip() {
    let server_home = make_home();
    let port = free_port();
    let server = spawn_server(server_home, port, "team");
    let script_body = rendered_install_script(&server);

    let client_home = tempfile::tempdir().expect("client HOME tempdir");
    let username = format!("reuse-{}", std::process::id());
    let password = "reuse-password";

    let first = run_install_script(
        &script_body,
        client_home.path(),
        Some(&username),
        Some(password),
    );
    assert!(
        first.status.success(),
        "initial install failed:\nstdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );

    let reused = run_install_script(&script_body, client_home.path(), None, None);
    let reused_stdout = String::from_utf8_lossy(&reused.stdout);
    let reused_stderr = String::from_utf8_lossy(&reused.stderr);
    assert!(
        reused.status.success(),
        "valid identity should skip prompts after /api/me verification:\nstdout=\n{reused_stdout}\nstderr=\n{reused_stderr}"
    );
    assert!(
        reused_stdout.contains("server accepted stored api_key"),
        "second install should report server-side identity verification; stdout=\n{reused_stdout}"
    );

    let identity_path = client_home.path().join(".runai-identity");
    let mut identity_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&identity_path).expect("read identity"))
            .expect("identity json");
    identity_json["api_key"] = serde_json::Value::String("rnai_live_stale_key".into());
    std::fs::write(
        &identity_path,
        serde_json::to_string_pretty(&identity_json).unwrap(),
    )
    .expect("write stale identity");

    let stale = run_install_script(&script_body, client_home.path(), None, None);
    let stale_stdout = String::from_utf8_lossy(&stale.stdout);
    let stale_stderr = String::from_utf8_lossy(&stale.stderr);
    assert!(
        !stale.status.success(),
        "stale identity must fail instead of silently skipping auth; stdout=\n{stale_stdout}\nstderr=\n{stale_stderr}"
    );
    assert!(
        stale_stderr.contains("existing identity was rejected"),
        "stale identity should explain the server rejection; stderr=\n{stale_stderr}"
    );
}

#[test]
fn hook_only_refreshes_server_pin_before_writing_hook_surface() {
    let server_home = make_home();
    let port = free_port();
    let server = spawn_server(server_home, port, "team");
    let script_body = rendered_install_script(&server);

    let client_home = tempfile::tempdir().expect("client HOME tempdir");
    let username = format!("hook-only-{}", std::process::id());
    let password = "hook-only-password";

    let first = run_install_script(
        &script_body,
        client_home.path(),
        Some(&username),
        Some(password),
    );
    assert!(
        first.status.success(),
        "initial install failed:\nstdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );

    let pin_path = client_home.path().join(".runai-server.json");
    std::fs::remove_file(&pin_path).expect("remove pin before hook-only reinstall");
    let hook_path = client_home.path().join(".runai-hook.sh");
    std::fs::remove_file(&hook_path).expect("remove hook before hook-only reinstall");

    let hook_only = run_install_script_with_args(
        &script_body,
        client_home.path(),
        None,
        None,
        &["--hook-only"],
    );
    let stdout = String::from_utf8_lossy(&hook_only.stdout);
    let stderr = String::from_utf8_lossy(&hook_only.stderr);
    assert!(
        hook_only.status.success(),
        "--hook-only should refresh pin and reinstall hook when identity exists:\nstdout=\n{stdout}\nstderr=\n{stderr}"
    );
    assert!(
        pin_path.exists(),
        "--hook-only must recreate .runai-server.json before writing HTTPS-gated clients"
    );
    let pin_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&pin_path).expect("read refreshed server pin"),
    )
    .expect("server pin json");
    assert_eq!(
        pin_json.get("scheme").and_then(|v| v.as_str()),
        Some("http"),
        "HTTP install should record explicit no-pin scheme"
    );
    assert!(
        hook_path.exists(),
        "--hook-only should still write the hook after refreshing pin"
    );
}

/// Full bash client lifecycle: first install creates the hook, settings entry,
/// companion CLI, server pin, identity, and remote MCP; second install reuses
/// the verified identity without prompting; uninstall removes only runai-owned
/// surfaces and keeps unrelated hooks/MCPs plus the identity.
#[test]
fn install_second_run_and_uninstall_round_trip_preserves_unrelated_config() {
    let server_home = make_home();
    let port = free_port();
    let server = spawn_server(server_home, port, "team");
    let loopback_url = server.base_url();

    // Render the install script and point it back at the loopback bind.
    let script_body = rendered_install_script(&server);

    let client_home = tempfile::tempdir().expect("client HOME tempdir");

    let settings_path = client_home.path().join(".claude/settings.json");
    std::fs::create_dir_all(settings_path.parent().unwrap()).expect("create .claude dir");
    std::fs::write(
        &settings_path,
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo unrelated"}]}]},"theme":"dark"}"#,
    )
    .expect("seed settings.json");

    // Pre-seed ~/.claude.json with an unrelated MCP server so we can prove
    // the install/uninstall touch ONLY the runai-client entry.
    let claude_json_path = client_home.path().join(".claude.json");
    std::fs::write(
        &claude_json_path,
        r#"{"mcpServers":{"other-mcp":{"command":"other","args":["x"]}},"someTopLevel":42}"#,
    )
    .expect("seed .claude.json");

    let username = format!("mcp-{}", std::process::id());
    let password = "mcp-leg-password";
    let output = run_install_script(
        &script_body,
        client_home.path(),
        Some(&username),
        Some(password),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "install.sh failed:\nstdout=\n{stdout}\nstderr=\n{stderr}"
    );

    // The api_key the server minted (used to assert the Bearer header value).
    let identity_raw =
        std::fs::read_to_string(client_home.path().join(".runai-identity")).expect("read identity");
    let api_key = serde_json::from_str::<serde_json::Value>(&identity_raw)
        .ok()
        .and_then(|v| v.get("api_key").and_then(|k| k.as_str()).map(String::from))
        .unwrap_or_default();
    assert!(api_key.starts_with("rnai_"), "expected minted api_key");
    assert!(
        client_home.path().join(".runai-server.json").is_file(),
        "install should write the server pin file that uninstall later cleans"
    );
    assert!(
        client_home.path().join(".runai-hook.sh").is_file(),
        "install should write hook script"
    );
    assert!(
        client_home.path().join(".local/bin/runai-client").is_file(),
        "install should write companion runai-client script"
    );
    let hook_commands = settings_hook_commands(&settings_path);
    assert!(
        hook_commands.iter().any(|cmd| cmd == "echo unrelated"),
        "install must preserve unrelated UserPromptSubmit hook; commands={hook_commands:?}"
    );
    assert!(
        hook_commands
            .iter()
            .any(|cmd| cmd.ends_with(".runai-hook.sh")),
        "install must add runai hook to settings; commands={hook_commands:?}"
    );

    // ── after install: runai-client MCP present + correct shape ──
    let claude_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&claude_json_path).expect("read .claude.json after install"),
    )
    .expect(".claude.json must remain valid JSON after install");

    // Pre-existing entry + top-level key preserved.
    assert!(
        claude_json["mcpServers"]["other-mcp"].is_object(),
        "install must not clobber pre-existing mcpServers entries: {claude_json}"
    );
    assert_eq!(
        claude_json["someTopLevel"], 42,
        "install must not drop unrelated top-level keys"
    );

    let entry = &claude_json["mcpServers"]["runai-client"];
    assert!(
        entry.is_object(),
        "install must add mcpServers.runai-client; got {claude_json}"
    );
    assert_eq!(
        entry["type"], "http",
        "remote MCP must be declared as an http transport: {entry}"
    );
    let url = entry["url"].as_str().unwrap_or_default();
    assert_eq!(
        url,
        format!("{loopback_url}/mcp"),
        "remote MCP url must point at <SERVER_URL>/mcp"
    );
    assert_eq!(
        entry["headers"]["Authorization"],
        serde_json::Value::String(format!("Bearer {api_key}")),
        "remote MCP must carry Authorization: Bearer <api_key>"
    );

    let second = run_install_script(&script_body, client_home.path(), None, None);
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    let second_stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        second.status.success(),
        "second install should reuse the verified identity without credentials:\nstdout=\n{second_stdout}\nstderr=\n{second_stderr}"
    );
    assert!(
        second_stdout.contains("server accepted stored api_key"),
        "second install should report verified identity reuse; stdout=\n{second_stdout}"
    );
    let hook_commands_after_second = settings_hook_commands(&settings_path);
    let runai_hook_count = hook_commands_after_second
        .iter()
        .filter(|cmd| cmd.ends_with(".runai-hook.sh"))
        .count();
    assert_eq!(
        runai_hook_count, 1,
        "second install must not duplicate hook entries; commands={hook_commands_after_second:?}"
    );

    // ── uninstall: remove runai-client, keep other-mcp ──
    let uninstall_body = fetch_uninstall_script(&server);
    let un_out = run_uninstall_script(&uninstall_body, client_home.path());
    assert!(
        un_out.status.success(),
        "uninstall.sh failed:\nstdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&un_out.stdout),
        String::from_utf8_lossy(&un_out.stderr),
    );

    let claude_after: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&claude_json_path).expect("read .claude.json after uninstall"),
    )
    .expect(".claude.json must remain valid JSON after uninstall");
    assert!(
        claude_after["mcpServers"]["runai-client"].is_null(),
        "uninstall must remove mcpServers.runai-client; got {claude_after}"
    );
    assert!(
        claude_after["mcpServers"]["other-mcp"].is_object(),
        "uninstall must keep unrelated mcpServers entries; got {claude_after}"
    );
    assert_eq!(
        claude_after["someTopLevel"], 42,
        "uninstall must not drop unrelated top-level keys"
    );
    assert!(
        !client_home.path().join(".runai-server.json").exists(),
        "uninstall must remove installer-generated server fingerprint pin"
    );
    assert!(
        !client_home.path().join(".runai-hook.sh").exists(),
        "uninstall must remove hook script"
    );
    assert!(
        !client_home.path().join(".local/bin/runai-client").exists(),
        "uninstall must remove companion runai-client script"
    );
    let hook_commands_after_uninstall = settings_hook_commands(&settings_path);
    assert_eq!(
        hook_commands_after_uninstall,
        vec!["echo unrelated".to_string()],
        "uninstall must remove only runai hook and keep unrelated hook"
    );
    assert!(
        client_home.path().join(".runai-identity").is_file(),
        "uninstall keeps identity for explicit account lifecycle, not hook cleanup"
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
