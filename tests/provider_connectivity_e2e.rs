//! Provider connectivity e2e for the dashboard Admin pane.
//!
//! The test runs the real `runai server` binary inside an isolated HOME, then
//! points a saved OpenAI-compatible provider at a local fake model endpoint.
//! Calling `/api/providers/{id}/test` must send a real chat-completions request
//! carrying the configured model, not just ping the base URL.

#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
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

fn register_admin(server: &ServerGuard) -> String {
    let client = http_client();
    let resp = client
        .post(format!("{}/users/register", server.base_url()))
        .json(&serde_json::json!({
            "username": "admin",
            "password": "correct horse battery staple",
        }))
        .send()
        .expect("POST /users/register");
    assert_eq!(resp.status().as_u16(), 201, "admin register should succeed");
    let body: serde_json::Value = resp.json().expect("register JSON");
    body["api_key"].as_str().unwrap_or_default().to_string()
}

fn read_http_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 1024];
    let mut expected_len: Option<usize> = None;
    loop {
        let n = stream.read(&mut tmp).expect("read fake provider request");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if expected_len.is_none()
            && let Some(headers_end) = find_headers_end(&buf)
        {
            let headers = String::from_utf8_lossy(&buf[..headers_end]);
            expected_len = headers.lines().find_map(|line| {
                let (k, v) = line.split_once(':')?;
                if k.eq_ignore_ascii_case("content-length") {
                    v.trim().parse::<usize>().ok()
                } else {
                    None
                }
            });
        }
        if let (Some(headers_end), Some(len)) = (find_headers_end(&buf), expected_len) {
            let body_start = headers_end + 4;
            if buf.len() >= body_start + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn spawn_fake_openai_provider() -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
    let port = listener.local_addr().expect("fake provider addr").port();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept fake provider request");
        let req = read_http_request(&mut stream);
        tx.send(req).expect("send captured request");
        let body = r#"{"choices":[{"message":{"content":"OK"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(resp.as_bytes())
            .expect("write fake provider response");
    });
    (format!("http://127.0.0.1:{port}"), rx, handle)
}

#[test]
fn admin_provider_test_sends_real_model_request() {
    let server = spawn_team_server();
    let admin_key = register_admin(&server);
    let client = http_client();
    let (provider_url, captured_rx, provider_thread) = spawn_fake_openai_provider();

    let save = client
        .post(format!("{}/api/providers", server.base_url()))
        .bearer_auth(&admin_key)
        .json(&serde_json::json!({
            "id": "fake-openai",
            "label": "Fake OpenAI",
            "kind": "openai-compat",
            "base_url": provider_url,
            "model": "fake-model-for-connectivity-test",
            "api_key": "test-key",
        }))
        .send()
        .expect("POST /api/providers");
    assert_eq!(save.status().as_u16(), 200, "provider save should succeed");

    let test = client
        .post(format!(
            "{}/api/providers/fake-openai/test",
            server.base_url()
        ))
        .bearer_auth(&admin_key)
        .send()
        .expect("POST /api/providers/{id}/test");
    assert_eq!(
        test.status().as_u16(),
        200,
        "provider test endpoint should return JSON"
    );
    let body: serde_json::Value = test.json().expect("provider test JSON");
    assert_eq!(body["ok"], true, "provider test body={body}");
    assert_eq!(body["model"], "fake-model-for-connectivity-test");
    assert_eq!(body["reply"], "OK");

    let captured = captured_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("fake provider should receive a model request");
    provider_thread.join().expect("fake provider thread");

    assert!(
        captured.starts_with("POST /chat/completions "),
        "provider test must hit chat completions, got: {captured}"
    );
    assert!(
        captured.contains("Authorization: Bearer test-key")
            || captured.contains("authorization: Bearer test-key"),
        "provider test must send the stored api key, got: {captured}"
    );
    assert!(
        captured.contains(r#""model":"fake-model-for-connectivity-test""#),
        "provider test must send configured model, got: {captured}"
    );
    assert!(
        captured.contains("Reply with exactly OK."),
        "provider test must send a real LLM prompt, got: {captured}"
    );
}
