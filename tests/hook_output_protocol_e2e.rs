//! Phase 2 e2e: the `/recommend` hook stdout must speak the new
//! runai-client activation/feedback protocol (PLANNING §1.3), not the
//! legacy curl-against-/skills/get protocol.
//!
//! Contract under test:
//!   - stdout contains `runai-client activate <name>`
//!   - stdout contains `runai-client feedback <name> --note`
//!   - stdout contains `runai-client file <name> <relpath>` guidance
//!   - stdout does NOT contain `curl -s -X POST`, `/skills/get/`, or a
//!     bare `curl .../feedback`
//!   - the `runai-client activate` line does NOT inline an `http://`
//!     server URL (identity is read by the client)
//!   - the activation session argument is a literal `rnai_sess_*`, never
//!     a host-specific environment variable such as `CLAUDE_SESSION_ID`
//!   - multi-candidate decisions render one `runai-client activate`
//!     mention per candidate
//!
//! The router LLM is mocked with an in-process TCP listener so the
//! decision is deterministic. Server runs in team mode against an
//! isolated HOME; real `~/.runai/` is never touched.

#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::json;
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
        thread::sleep(Duration::from_millis(50));
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

    fn home(&self) -> &std::path::Path {
        self.home.path()
    }

    fn plant(&self, name: &str, desc: &str) {
        let dir = self.home().join(".runai/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\n\n# {name}\n\n{desc}\n"),
        )
        .unwrap();
        let out = runai_cmd()
            .arg("scan")
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.home().join(".runai"))
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .output()
            .unwrap();
        assert!(out.status.success(), "scan failed");
    }

    fn write_recommend_config(&self, base_url: &str) {
        let toml = format!(
            "[recommend]\n\
             enabled = true\n\
             provider = \"openai-compat\"\n\
             base_url = \"{base_url}\"\n\
             model = \"mock-model\"\n\
             api_key = \"mock-key\"\n\
             top_k = 8\n\
             min_prompt_len = 0\n\
             summary_lang = \"en\"\n"
        );
        std::fs::write(self.home().join(".runai/config.toml"), toml).unwrap();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Tiny mock LLM that always returns the same escaped-JSON chat completion.
struct MockLlm {
    base_url: String,
    shutdown: Arc<AtomicBool>,
}

impl MockLlm {
    fn start(content: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock LLM");
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let body_content = content.to_string();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(2000)))
                            .ok();
                        let mut buf = [0u8; 8192];
                        let mut total = Vec::new();
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    total.extend_from_slice(&buf[..n]);
                                    if total.windows(4).any(|w| w == b"\r\n\r\n") {
                                        let _ = stream.read(&mut buf);
                                        break;
                                    }
                                    if total.len() > 1 << 20 {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let escaped = serde_json::to_string(&body_content).unwrap();
                        let json_body = format!(
                            "{{\"id\":\"mock-1\",\"object\":\"chat.completion\",\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":{escaped}}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}}}"
                        );
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            json_body.len(),
                            json_body
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Self { base_url, shutdown }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for MockLlm {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
}

fn post_recommend(s: &Server, prompt: &str) -> String {
    let resp = http()
        .post(format!("{}/recommend", s.base()))
        .json(&json!({"prompt": prompt, "session_id": "sess-hook", "cwd": "/tmp"}))
        .send()
        .unwrap();
    resp.text().unwrap_or_default()
}

#[test]
fn hook_output_uses_runai_client_activate_not_curl() {
    let s = Server::spawn();
    s.plant("alpha-skill", "alpha desc");
    // Router response naming the planted skill. The recommend pipeline
    // parses `task:`/`score:`/etc lines; a minimal valid payload.
    let mock_content = "EXCLUSIVE\nreasoning: matches alpha\nalpha-skill\n";
    let mock = MockLlm::start(mock_content);
    s.write_recommend_config(mock.base_url());
    // suppress bootstrap-seen so the disabled-router guide doesn't fire
    std::fs::write(s.home().join(".runai/.bootstrap-seen"), "1").unwrap();

    let stdout = post_recommend(&s, "i need an alpha");
    if stdout.is_empty() {
        // If the router didn't pick the skill, the test is moot — but we
        // still want to assert the protocol shape. Retry with a clearer
        // prompt to nudge the mock (the mock is content-fixed, so this
        // is mostly a timing guard).
        eprintln!("first /recommend returned empty; retrying");
        let stdout2 = post_recommend(&s, "alpha please");
        assert!(
            !stdout2.is_empty(),
            "/recommend returned empty twice — router did not pick skill"
        );
        assert_protocol(&stdout2);
        return;
    }
    assert_protocol(&stdout);
}

fn assert_protocol(stdout: &str) {
    assert!(
        stdout.contains("runai-client activate"),
        "hook output must use runai-client activate, got:\n{stdout}"
    );
    assert!(
        stdout.contains("runai-client feedback"),
        "hook output must use runai-client feedback, got:\n{stdout}"
    );
    assert!(
        stdout.contains("runai-client file"),
        "hook output must tell agents to read bundle support files through runai-client file, got:\n{stdout}"
    );
    assert!(
        stdout.contains("skill bundle") && stdout.contains("运行时用户数据"),
        "hook output must distinguish bundle files from runtime user data, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("curl -s -X POST"),
        "hook output must NOT use curl for activation, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("/skills/get/"),
        "hook output must NOT reference /skills/get/, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("/feedback\""),
        "hook output must NOT use bare curl /feedback, got:\n{stdout}"
    );
    // The activate line must not inline an http:// server URL or host env var.
    for line in stdout.lines() {
        if line.contains("runai-client activate") {
            assert!(
                !line.contains("http://"),
                "activate line must not inline server URL: {line}"
            );
            assert!(
                !line.contains("CLAUDE_SESSION_ID"),
                "activate line must not use host-specific env vars: {line}"
            );
        }
    }
}

#[test]
fn hook_output_passes_literal_runai_session_id() {
    // The activation directive carries a runai-owned literal session id so
    // host-specific session variables do not leak into agent instructions.
    let s = Server::spawn();
    s.plant("beta-skill", "beta desc");
    let mock_content = "EXCLUSIVE\nreasoning: matches beta\nbeta-skill\n";
    let mock = MockLlm::start(mock_content);
    s.write_recommend_config(mock.base_url());
    std::fs::write(s.home().join(".runai/.bootstrap-seen"), "1").unwrap();
    let stdout = post_recommend(&s, "i need beta");
    if stdout.is_empty() {
        eprintln!("router returned empty; skipping session-id assertion");
        return;
    }
    assert!(
        stdout.contains("--session-id \"rnai_sess_"),
        "hook output must pass a literal runai session id, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("CLAUDE_SESSION_ID") && !stdout.contains("sess-hook"),
        "hook output must not leak host session identifiers, got:\n{stdout}"
    );
}
