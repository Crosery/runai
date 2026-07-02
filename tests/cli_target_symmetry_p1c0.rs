//! P1 cargo_integration tests for `runai recommend status`.
//!
//! Plan: runai-158-test-plan.md §1.34 (status feature chunk).
//!
//! These tests spawn the workspace-built `runai` binary
//! (`env!("CARGO_BIN_EXE_runai")`) against a fully sandboxed HOME /
//! RUNE_DATA_DIR. They verify that `recommend status` prints the loaded
//! config without ever echoing the api_key value (per the rationale:
//! status output may be piped to logs).
//!
//! Skipped on Windows because HOME mocking + the unix binary path don't apply.
#![cfg(not(target_os = "windows"))]

use std::path::Path;
use std::process::Command;

const RUNAI_BIN: &str = env!("CARGO_BIN_EXE_runai");

/// Spawn the installed binary with a fully isolated HOME + RUNE_DATA_DIR and
/// every env var that could leak the host machine's real recommend config
/// scrubbed.
fn run_status(home: &Path, extra_env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(RUNAI_BIN);
    cmd.args(["recommend", "status"])
        .env("HOME", home)
        .env("RUNE_DATA_DIR", home.join(".runai"))
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .env_remove("RUNAI_RECOMMEND_API_KEY");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn runai recommend status")
}

fn write_config(home: &Path, toml_body: &str) {
    let dir = home.join(".runai");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), toml_body).unwrap();
}

/// Test 1 (plan §1.34.1): `recommend_status_no_key_echo`
///
/// Config file with provider=anthropic + non-empty api_key. status must print
/// `api_key: set in config` and must NEVER echo the literal key value.
#[test]
fn recommend_status_no_key_echo() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let secret_key = "test-key-12345";
    write_config(
        home,
        &format!(
            r#"[recommend]
enabled = true
provider = "anthropic"
base_url = "https://api.anthropic.com/v1"
model = "claude-sonnet-4"
api_key = "{secret_key}"
top_k = 5
min_prompt_len = 0
summary_lang = "en"
session_mode = "oneshot"
session_history_limit = 20
"#
        ),
    );

    let out = run_status(home, &[]);
    assert!(
        out.status.success(),
        "recommend status exited non-zero: status={:?}\nstdout={}\nstderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    // Per-line assertions on the documented status output shape.
    assert!(
        stdout.contains("enabled:        true"),
        "expected enabled:true line, got:\n{stdout}"
    );
    assert!(
        stdout.contains("provider:       anthropic"),
        "expected provider:anthropic (lowercase, matches toml enum), got:\n{stdout}"
    );
    assert!(
        stdout.contains("api_key:        set in config"),
        "expected api_key 'set in config' label, got:\n{stdout}"
    );
    // Config file path must mention the sandboxed RUNE_DATA_DIR.
    let cfg_path = home.join(".runai").join("config.toml");
    assert!(
        stdout.contains(&cfg_path.display().to_string()),
        "expected config file path {} in stdout:\n{stdout}",
        cfg_path.display()
    );

    // Security invariant: api_key value must never appear anywhere in output.
    assert!(
        !stdout.contains(secret_key),
        "SECURITY: api_key value leaked to stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains(secret_key),
        "SECURITY: api_key value leaked to stderr:\n{stderr}"
    );
}

/// Test 2 (plan §1.34.2): `recommend_status_env_api_key`
///
/// config.toml has empty api_key, but `RUNAI_RECOMMEND_API_KEY` is set in the
/// env. status must report `set via RUNAI_RECOMMEND_API_KEY` and must NOT
/// echo the env var value.
#[test]
fn recommend_status_env_api_key() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let env_secret = "env-key-supersecret-987";
    write_config(
        home,
        r#"[recommend]
enabled = true
provider = "openai-compat"
base_url = "https://example.com/v1"
model = "test-model"
api_key = ""
top_k = 8
min_prompt_len = 0
summary_lang = "zh"
session_mode = "oneshot"
session_history_limit = 20
"#,
    );

    let out = run_status(home, &[("RUNAI_RECOMMEND_API_KEY", env_secret)]);
    assert!(
        out.status.success(),
        "recommend status (env api_key) exited non-zero: status={:?}\nstdout={}\nstderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert!(
        stdout.contains("api_key:        set via RUNAI_RECOMMEND_API_KEY"),
        "expected env-var api_key marker, got:\n{stdout}"
    );

    // The env var name being mentioned is OK; the value must never be.
    assert!(
        !stdout.contains(env_secret),
        "SECURITY: env RUNAI_RECOMMEND_API_KEY value leaked to stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains(env_secret),
        "SECURITY: env RUNAI_RECOMMEND_API_KEY value leaked to stderr:\n{stderr}"
    );

    // Sanity: env precedence holds even though config api_key is "".
    assert!(
        !stdout.contains("api_key:        missing"),
        "env var should take precedence over empty config api_key, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("api_key:        set in config"),
        "empty config api_key must not report 'set in config', got:\n{stdout}"
    );
}

/// Test 3 (plan §1.34.3): `recommend_status_disabled_defaults`
///
/// No config file written → `RecommendConfig::load` returns defaults. status
/// must print `enabled: false`, `provider: openai-compat`, and every other
/// field must render without error or panic.
#[test]
fn recommend_status_disabled_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // Pre-create the .runai dir so paths::data_dir() doesn't trip on a
    // missing parent — but DO NOT write config.toml.
    std::fs::create_dir_all(home.join(".runai")).unwrap();

    let out = run_status(home, &[]);
    assert!(
        out.status.success(),
        "recommend status (defaults) exited non-zero: status={:?}\nstdout={}\nstderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8(out.stdout).unwrap();

    // The two anchors the plan calls out by name.
    assert!(
        stdout.contains("enabled:        false"),
        "expected enabled:false on default config, got:\n{stdout}"
    );
    assert!(
        stdout.contains("provider:       openai-compat"),
        "expected default provider 'openai-compat', got:\n{stdout}"
    );

    // Every documented field must render without erroring out.
    for label in [
        "enabled:",
        "provider:",
        "base_url:",
        "model:",
        "api_key:",
        "top_k:",
        "summary_lang:",
        "min_prompt_len:",
        "config file:",
    ] {
        assert!(
            stdout.contains(label),
            "missing field label '{label}' in default status output:\n{stdout}"
        );
    }

    // No key was set anywhere → must show 'missing'.
    assert!(
        stdout.contains("api_key:        missing"),
        "expected api_key 'missing' marker on default config, got:\n{stdout}"
    );
}
