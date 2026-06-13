//! P1 e2e regression for `runai recommend status` — section 1.34 of the test plan.
//!
//! These tests guard the contract that `recommend status` prints router
//! configuration WITHOUT echoing any secret material (api_key value, env-var
//! value, password, …). The status output is routinely piped into logs and
//! Claude Code transcripts, so leaking a key there leaks it everywhere.
//!
//! Each test runs the real `runai` binary at /Users/crosery/.cargo/bin/runai
//! (reinstalled from cloud HEAD) inside an isolated HOME/RUNE_DATA_DIR sandbox
//! per the safety contract in AGENTS.md — no test ever touches the real
//! `~/.runai/` or `~/.{claude,codex,gemini,opencode}/` paths.
//!
//! Skipped on Windows because the project's other physical-e2e suites are
//! all `#[cfg(not(target_os = "windows"))]` for HOME-mocking reasons.
#![cfg(not(target_os = "windows"))]

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

/// Spawn `runai recommend status` against an isolated HOME / RUNE_DATA_DIR.
/// `extra_env` lets a caller layer `RUNAI_RECOMMEND_API_KEY` on top.
fn run_status(home: &Path, rune_data: &Path, extra_env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(RUNAI_BIN);
    cmd.args(["recommend", "status"])
        .env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", rune_data)
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .env_remove("RUNAI_RECOMMEND_API_KEY");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn runai binary")
}

fn dump(out: &std::process::Output, label: &str) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{stdout}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stderr),
    );
    stdout
}

/// Plan 1.34 test 1: `recommend status` displays the config under `RUNE_DATA_DIR`
/// correctly and **never echoes the api_key value itself**. Status output is
/// piped to logs / transcripts; leaking the raw key there leaks it everywhere.
#[test]
fn recommend_status_no_key_echo() {
    let home = tempfile::tempdir().expect("create tmp HOME");
    let rune_data = home.path().join(".runai");
    std::fs::create_dir_all(&rune_data).unwrap();

    // Write a config.toml at the canonical location (`<RUNE_DATA_DIR>/config.toml`)
    // with a sentinel api_key the test will later assert is NOT echoed.
    let secret_key = "test-key-12345";
    std::fs::write(
        rune_data.join("config.toml"),
        format!(
            r#"[recommend]
enabled = true
provider = "anthropic"
base_url = "https://api.anthropic.com/v1"
model = "claude-3-5-haiku"
api_key = "{secret_key}"
top_k = 8
min_prompt_len = 0
summary_lang = "zh"
session_mode = "oneshot"
session_history_limit = 20
"#
        ),
    )
    .unwrap();

    let out = run_status(home.path(), &rune_data, &[]);
    let stdout = dump(&out, "recommend_status_no_key_echo");
    assert!(
        out.status.success(),
        "recommend status should succeed even with full config"
    );

    // Plan-mandated assertions.
    assert!(
        stdout.contains("enabled:") && stdout.contains("true"),
        "stdout should show 'enabled: true'\n{stdout}"
    );
    assert!(
        stdout.contains("provider:") && stdout.contains("anthropic"),
        "stdout should show 'provider: anthropic' (lowercase, matches toml enum)\n{stdout}"
    );
    assert!(
        stdout.contains("set in config"),
        "stdout should show 'api_key: set in config'\n{stdout}"
    );
    assert!(
        stdout.contains("config file:"),
        "stdout should show config-file line\n{stdout}"
    );
    // The config-file path printed must point at the active RUNE_DATA_DIR's
    // config.toml — that path is the one we wrote to in setup. We compare on
    // the canonicalized form because macOS symlinks /tmp → /private/tmp and
    // the binary may print either flavor.
    let cfg_path_canon = std::fs::canonicalize(rune_data.join("config.toml")).unwrap();
    let cfg_path_raw = rune_data.join("config.toml");
    assert!(
        stdout.contains(cfg_path_canon.to_str().unwrap())
            || stdout.contains(cfg_path_raw.to_str().unwrap()),
        "config-file line should reference the active RUNE_DATA_DIR's config.toml\n\
         (canonical {cfg_path_canon:?} or raw {cfg_path_raw:?})\n{stdout}"
    );

    // Anti-leak: the secret api_key value itself must NEVER appear in stdout
    // OR stderr. This is the load-bearing assertion of the test.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains(secret_key),
        "api_key value '{secret_key}' must NOT appear in stdout\n{stdout}"
    );
    assert!(
        !stderr.contains(secret_key),
        "api_key value '{secret_key}' must NOT appear in stderr\n{stderr}"
    );
}

/// Plan 1.34 test 2: when the api_key comes via `RUNAI_RECOMMEND_API_KEY`
/// env (and the config-file api_key is empty), status reports the env-var
/// source by name — without echoing the value itself. env vars often appear
/// in debug logs; only the *fact* of being set should be shown.
#[test]
fn recommend_status_env_api_key() {
    let home = tempfile::tempdir().expect("create tmp HOME");
    let rune_data = home.path().join(".runai");
    std::fs::create_dir_all(&rune_data).unwrap();

    // Config has empty api_key — forces fall-through to env var.
    std::fs::write(
        rune_data.join("config.toml"),
        r#"[recommend]
enabled = true
provider = "openai-compat"
base_url = "https://api.deepseek.com/v1"
model = "deepseek-v4-flash"
api_key = ""
top_k = 8
min_prompt_len = 0
summary_lang = "zh"
session_mode = "oneshot"
session_history_limit = 20
"#,
    )
    .unwrap();

    let env_secret = "env-key-abcdef-9876";
    let out = run_status(
        home.path(),
        &rune_data,
        &[("RUNAI_RECOMMEND_API_KEY", env_secret)],
    );
    let stdout = dump(&out, "recommend_status_env_api_key");
    assert!(out.status.success(), "recommend status should succeed");

    // Source label must be the env-var name, not "set in config" or "missing".
    assert!(
        stdout.contains("set via RUNAI_RECOMMEND_API_KEY"),
        "stdout should show 'api_key: set via RUNAI_RECOMMEND_API_KEY'\n{stdout}"
    );
    assert!(
        !stdout.contains("set in config"),
        "stdout must not say 'set in config' when config api_key is empty\n{stdout}"
    );

    // The env-var **value** must not be echoed.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains(env_secret),
        "env-var value '{env_secret}' must NOT appear in stdout\n{stdout}"
    );
    assert!(
        !stderr.contains(env_secret),
        "env-var value '{env_secret}' must NOT appear in stderr\n{stderr}"
    );
}

/// Plan 1.34 test 3: with no config file written (= disabled, all defaults),
/// `recommend status` should still print every field without error and show
/// `enabled: false`. Users need to see what defaults would apply BEFORE
/// running `recommend setup`.
#[test]
fn recommend_status_disabled_defaults() {
    let home = tempfile::tempdir().expect("create tmp HOME");
    let rune_data = home.path().join(".runai");
    std::fs::create_dir_all(&rune_data).unwrap();
    // Deliberately no config.toml — should fall back to RecommendConfig::default().

    let out = run_status(home.path(), &rune_data, &[]);
    let stdout = dump(&out, "recommend_status_disabled_defaults");
    assert!(out.status.success(), "status must succeed with no config");

    // `enabled: false` is the default. The current handler prints
    // `enabled:        false` (variable whitespace) — match the keyword + value
    // rather than the exact column.
    assert!(
        stdout.contains("enabled:") && stdout.contains("false"),
        "stdout should show 'enabled: false' on a fresh / unconfigured install\n{stdout}"
    );

    // Default provider per `RecommendConfig::default()` is `Provider::OpenaiCompat`,
    // which the handler renders as the lowercase, hyphenated `openai-compat`.
    assert!(
        stdout.contains("openai-compat"),
        "default provider should render as 'openai-compat'\n{stdout}"
    );

    // All advertised fields must be present, with no panic / error output.
    for field in [
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
            stdout.contains(field),
            "field '{field}' missing from default status output\n{stdout}"
        );
    }

    // No api_key set in config and no env var → label must be 'missing'.
    assert!(
        stdout.contains("missing"),
        "with no api_key configured, status should show 'api_key: missing'\n{stdout}"
    );
}

// Borrowed-but-unused suppression: silences `dead_code` if any helper above is
// not invoked by the public test set (e.g. when the file is appended to later
// and a helper temporarily falls out of use).
#[allow(dead_code)]
fn _keep_tempdir_alive(_t: &TempDir) {}
