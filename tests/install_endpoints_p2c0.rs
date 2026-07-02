//! P2 e2e regression tests for `GET /install`
//! (PLANNING.md §1.2 — client install script, mode-gated).
//!
//! Spawns the workspace-built `runai` binary (`env!("CARGO_BIN_EXE_runai")`)
//! as a server in an isolated HOME + RUNE_DATA_DIR sandbox, then probes the
//! install endpoint with a minimal blocking HTTP client. Asserts:
//!   - owner mode: 404 with empty body (anti-explore)
//!   - team mode: 200, text/x-shellscript, `{SERVER_URL}` substituted,
//!     no binary-only management commands leaked
//!   - loopback Host header gets rewritten to a LAN IPv4 (best-effort:
//!     skipped when the test host has no LAN IPv4)
//!   - non-loopback Host header preserved verbatim

#![cfg(not(target_os = "windows"))]

use std::io::{BufRead, BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_runai");

/// Pick a free TCP port by binding to :0 and immediately dropping the
/// listener. Race-prone in principle (another process could grab it),
/// but fine for an isolated dev box.
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    l.local_addr().expect("local_addr").port()
}

/// Block until the given (host, port) accepts a TCP connection or the
/// timeout elapses. Returns true on success.
fn wait_for_port(host: &str, port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(
            &format!("{host}:{port}").parse().unwrap(),
            Duration::from_millis(100),
        )
        .is_ok()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

struct ServerHandle {
    child: Child,
    _home: tempfile::TempDir,
    port: u16,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server(mode: &str) -> ServerHandle {
    let home = tempfile::tempdir().expect("mktemp HOME");
    let port = free_port();
    let data_dir: PathBuf = home.path().join(".runai");

    let child = Command::new(BINARY)
        .args([
            "server",
            "--mode",
            mode,
            "--port",
            &port.to_string(),
            "--host",
            "127.0.0.1",
        ])
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", &data_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn runai server");

    if !wait_for_port("127.0.0.1", port, Duration::from_secs(15)) {
        panic!("runai server (mode={mode}) did not bind 127.0.0.1:{port}");
    }

    ServerHandle {
        child,
        _home: home,
        port,
    }
}

/// Minimal blocking HTTP GET so we control the Host header verbatim.
/// Returns (status_code, content_type, content_length_from_header,
/// body_bytes). reqwest::blocking would also work but normalizes Host
/// and follows redirects; we want a raw, predictable client.
fn http_get(port: u16, path: &str, host_header: Option<&str>) -> (u16, String, usize, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect server");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let host_line = match host_header {
        Some(h) => format!("Host: {h}\r\n"),
        None => format!("Host: 127.0.0.1:{port}\r\n"),
    };
    let req = format!("GET {path} HTTP/1.1\r\n{host_line}Connection: close\r\n\r\n");
    use std::io::Write;
    stream.write_all(req.as_bytes()).expect("write request");

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).expect("read status");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut content_type = String::new();
    let mut content_length: usize = 0;
    let mut transfer_chunked = false;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read header");
        if n == 0 {
            break;
        }
        let trim = line.trim_end_matches(['\r', '\n']);
        if trim.is_empty() {
            break;
        }
        let lower = trim.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-type:") {
            content_type = v.trim().to_string();
        } else if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = lower.strip_prefix("transfer-encoding:")
            && v.trim().eq_ignore_ascii_case("chunked")
        {
            transfer_chunked = true;
        }
    }

    let mut body = Vec::new();
    if transfer_chunked {
        loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line).unwrap_or(0) == 0 {
                break;
            }
            let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
            if size == 0 {
                break;
            }
            let mut chunk = vec![0u8; size];
            reader.read_exact(&mut chunk).expect("read chunk");
            body.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            let _ = reader.read_exact(&mut crlf);
        }
    } else if content_length > 0 {
        body.resize(content_length, 0);
        reader.read_exact(&mut body).expect("read body");
    } else {
        let _ = reader.read_to_end(&mut body);
    }

    (status_code, content_type, content_length, body)
}

// ----- 4.15 GET /install ------------------------------------------------

#[test]
fn install_owner_mode_returns_empty_404() {
    let server = spawn_server("owner");
    let (status, _ct, content_len, body) = http_get(server.port, "/install", None);
    assert_eq!(status, 404, "owner mode must 404 /install");
    assert!(
        body.is_empty(),
        "owner-mode /install body must be empty (PLANNING §1.2: anti-explore), got {} bytes",
        body.len()
    );
    assert_eq!(
        content_len, 0,
        "owner-mode /install must advertise Content-Length: 0"
    );
}

#[test]
fn install_team_mode_returns_script_with_url_substituted() {
    let server = spawn_server("team");
    let (status, ct, _len, body) = http_get(server.port, "/install", None);
    assert_eq!(status, 200, "team mode /install must 200");
    assert!(
        ct.contains("text/x-shellscript"),
        "team mode /install must return shellscript content-type, got: {ct}"
    );
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.starts_with("#!"),
        "install script must start with shebang, got: {:?}",
        body_str.lines().next()
    );
    assert!(
        !body_str.contains("{SERVER_URL}"),
        "install script must fully substitute {{SERVER_URL}} placeholder"
    );
    // Binary-only management commands must NOT leak into a remote client.
    for forbidden in ["runai scan", "runai discover", "runai doctor"] {
        assert!(
            !body_str.contains(forbidden),
            "install script must not contain binary-only command `{forbidden}` (PLANNING §1.2)"
        );
    }
}

#[test]
fn install_loopback_substituted_with_lan_ipv4() {
    let server = spawn_server("team");
    let (status, _ct, _len, body) = http_get(
        server.port,
        "/install",
        Some(&format!("localhost:{}", server.port)),
    );
    assert_eq!(status, 200);
    let body_str = String::from_utf8_lossy(&body);
    let url_line = body_str
        .lines()
        .find(|l| l.trim_start().starts_with("SERVER_URL="))
        .unwrap_or_else(|| panic!("no SERVER_URL= line in install script"));
    // Best-effort: machines without a LAN IPv4 fall back to the Host
    // header — degrade to "did not worsen things".
    let host_localhost = format!("localhost:{}", server.port);
    if url_line.contains("127.0.0.1") || url_line.contains(&host_localhost) {
        eprintln!(
            "install_loopback_substituted: no LAN IPv4 detected, server fell back to Host. line={url_line}"
        );
    } else {
        assert!(
            url_line.contains(&format!(":{}", server.port)),
            "rewritten URL must preserve port {} — got {url_line}",
            server.port
        );
        assert!(
            !url_line.contains("localhost"),
            "rewritten URL must not contain `localhost` — got {url_line}"
        );
    }
}

#[test]
fn install_preserves_non_loopback_hostname() {
    let server = spawn_server("team");
    let host = "example.com:9999";
    let (status, _ct, _len, body) = http_get(server.port, "/install", Some(host));
    assert_eq!(status, 200);
    let body_str = String::from_utf8_lossy(&body);
    let url_line = body_str
        .lines()
        .find(|l| l.trim_start().starts_with("SERVER_URL="))
        .unwrap_or_else(|| panic!("no SERVER_URL= line in install script"));
    assert!(
        url_line.contains("example.com:9999"),
        "non-loopback Host (example.com:9999) must be preserved verbatim, got: {url_line}"
    );
    assert!(
        !url_line.contains("127.0.0.1"),
        "non-loopback Host must not be rewritten to loopback, got: {url_line}"
    );
}

// ----- 4.16 GET /uninstall ----------------------------------------------

#[test]
fn uninstall_owner_mode_returns_empty_404() {
    let server = spawn_server("owner");
    let (status, _ct, content_len, body) = http_get(server.port, "/uninstall", None);
    assert_eq!(status, 404, "owner mode must 404 /uninstall");
    assert!(
        body.is_empty(),
        "owner-mode /uninstall body must be empty (PLANNING §1.2: anti-explore), got {} bytes",
        body.len()
    );
    assert_eq!(
        content_len, 0,
        "owner-mode /uninstall must advertise Content-Length: 0"
    );
}

#[test]
fn uninstall_team_mode_returns_removal_script() {
    let server = spawn_server("team");
    let (status, ct, _len, body) = http_get(server.port, "/uninstall", None);
    assert_eq!(status, 200, "team mode /uninstall must 200");
    assert!(
        ct.contains("text/x-shellscript"),
        "team mode /uninstall must return shellscript content-type, got: {ct}"
    );
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.starts_with("#!"),
        "uninstall script must start with shebang"
    );
    // The uninstall script must reference the hook it removes.
    let mentions_hook = body_str.contains(".runai-hook.sh")
        || body_str.contains("settings.json")
        || body_str.contains("UserPromptSubmit");
    assert!(
        mentions_hook,
        "uninstall script must reference .runai-hook.sh / settings.json / UserPromptSubmit"
    );
    // Smoke-check: a syntactically broken remote-shipped script is a
    // release blocker; `bash -n` parses without executing.
    let tmp = tempfile::tempdir().expect("mktemp tmpdir");
    let path = tmp.path().join("uninstall.sh");
    std::fs::write(&path, &body).expect("write uninstall.sh");
    let status = Command::new("bash")
        .arg("-n")
        .arg(&path)
        .status()
        .expect("spawn bash -n");
    assert!(
        status.success(),
        "uninstall script failed `bash -n` syntax check"
    );
}
