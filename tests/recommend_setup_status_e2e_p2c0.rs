//! P2 e2e regression for `runai recommend setup` and `runai recommend status`.
//!
//! These spawn the installed `/Users/crosery/.cargo/bin/runai` binary inside a
//! tempdir HOME with an explicit `RUNE_DATA_DIR` so they NEVER touch the real
//! `~/.runai/`. Each test pipes interactive answers to stdin where the wizard
//! demands them, then asserts on the resulting `config.toml` and stdout.
//!
//! Plan target: `tests/recommend_setup_status_e2e.rs`.
//! Chunked filename to avoid concurrent-write conflicts across agents.
#![cfg(not(target_os = "windows"))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

/// Spawn the installed runai binary in an isolated HOME with RUNE_DATA_DIR
/// pointed at a freshly-created `<home>/.runai` so the wizard cannot reach
/// the real user data dir even if something in the wizard ignored HOME.
fn isolated_env() -> (TempDir, PathBuf) {
    let home = tempfile::tempdir().expect("create tmp HOME");
    let rune_data = home.path().join(".runai");
    std::fs::create_dir_all(&rune_data).expect("create RUNE_DATA_DIR");
    (home, rune_data)
}

fn runai_cmd(home: &Path, rune_data: &Path) -> Command {
    let mut cmd = Command::new(RUNAI_BIN);
    cmd.env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", rune_data)
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .env_remove("RUNAI_RECOMMEND_API_KEY");
    cmd
}

/// Drive `recommend setup` interactively by piping `answers` to stdin and
/// capturing the wizard's stdout. Each line of `answers` is one answer the
/// `ask()` prompt reads via `BufRead::read_line`.
fn run_setup_with_answers(home: &Path, rune_data: &Path, answers: &str) -> (bool, String, String) {
    let mut child = runai_cmd(home, rune_data)
        .args(["recommend", "setup"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn runai recommend setup");
    {
        let stdin = child.stdin.as_mut().expect("stdin pipe");
        stdin
            .write_all(answers.as_bytes())
            .expect("write wizard answers to stdin");
    }
    let output = child
        .wait_with_output()
        .expect("collect runai recommend setup output");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (output.status.success(), stdout, stderr)
}

// ───── Feature: recommend setup ────────────────────────────────────────────

#[test]
fn recommend_setup_writes_config_0o600() {
    let (home, rune_data) = isolated_env();

    // Wizard reads 5 lines (in order): provider, base_url, model, api_key,
    // summary_lang. Anthropic provider with a real key + zh summary.
    let answers = "anthropic\nhttps://api.anthropic.com\nclaude-opus\nmy-key\nzh\n";
    let (success, stdout, stderr) =
        run_setup_with_answers(home.path(), rune_data.as_path(), answers);
    assert!(
        success,
        "setup must succeed; stdout={stdout}\nstderr={stderr}"
    );

    // 1) config.toml created at RUNE_DATA_DIR/config.toml.
    let cfg_path = rune_data.join("config.toml");
    assert!(
        cfg_path.exists(),
        "config.toml must be written to RUNE_DATA_DIR ({})\nstdout={stdout}",
        cfg_path.display()
    );

    // 2) Wizard prints the saved path so the user knows where it went.
    assert!(
        stdout.contains("Saved to") && stdout.contains(cfg_path.to_string_lossy().as_ref()),
        "setup output must announce the saved path; stdout={stdout}"
    );

    // 3) [recommend] section has enabled=true + the anthropic answers.
    let contents = std::fs::read_to_string(&cfg_path).expect("read written config.toml");
    assert!(
        contents.contains("[recommend]"),
        "config.toml needs [recommend] section; contents={contents}"
    );
    assert!(
        contents.contains("enabled = true"),
        "setup must enable router; contents={contents}"
    );
    assert!(
        contents.contains("provider = \"anthropic\""),
        "provider answer must persist; contents={contents}"
    );
    assert!(
        contents.contains("base_url = \"https://api.anthropic.com\""),
        "base_url must persist; contents={contents}"
    );
    assert!(
        contents.contains("model = \"claude-opus\""),
        "model must persist; contents={contents}"
    );
    assert!(
        contents.contains("api_key = \"my-key\""),
        "api_key must persist; contents={contents}"
    );
    assert!(
        contents.contains("summary_lang = \"zh\""),
        "summary_lang must persist; contents={contents}"
    );

    // 4) On unix the file MUST be 0o600 — api_key lives on disk and other
    //    users on the box cannot be allowed to read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cfg_path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "config.toml must be owner-only on unix (api_key on disk); got {mode:o}"
        );
    }
}

#[test]
fn recommend_setup_idempotent_overwrites() {
    let (home, rune_data) = isolated_env();

    // First run — openai-compat / deepseek defaults + a first key.
    let first = "openai-compat\nhttps://api.deepseek.com/v1\ndeepseek-v4-flash\nfirst-key\nzh\n";
    let (ok1, out1, err1) = run_setup_with_answers(home.path(), rune_data.as_path(), first);
    assert!(ok1, "first setup must succeed; stdout={out1}\nstderr={err1}");
    let cfg_path = rune_data.join("config.toml");
    let first_contents = std::fs::read_to_string(&cfg_path).expect("read first config");
    assert!(
        first_contents.contains("provider = \"openai-compat\""),
        "first run wrote openai-compat; contents={first_contents}"
    );
    assert!(
        first_contents.contains("api_key = \"first-key\""),
        "first key persisted; contents={first_contents}"
    );

    // Second run — switch provider to anthropic with a different key/model.
    // The wizard is interactive so a fresh full set of answers overrides.
    let second = "anthropic\nhttps://api.anthropic.com\nclaude-opus\nsecond-key\nzh\n";
    let (ok2, out2, err2) = run_setup_with_answers(home.path(), rune_data.as_path(), second);
    assert!(
        ok2,
        "second setup must succeed; stdout={out2}\nstderr={err2}"
    );

    let second_contents = std::fs::read_to_string(&cfg_path).expect("read second config");
    assert!(
        second_contents.contains("provider = \"anthropic\""),
        "second run replaced provider; contents={second_contents}"
    );
    assert!(
        second_contents.contains("api_key = \"second-key\""),
        "second key replaced first; contents={second_contents}"
    );
    assert!(
        second_contents.contains("model = \"claude-opus\""),
        "model replaced; contents={second_contents}"
    );
    // Note: the binary may keep prior provider answers in a saved_providers
    // sub-table for quick switching — what we strictly require is that the
    // *active* fields (top-level api_key / provider / model) reflect the
    // second run's answers, which is asserted above. We do NOT require the
    // first key to vanish from the file entirely.

    // On unix the rewrite must still be 0o600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cfg_path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "rewritten config.toml must remain owner-only; got {mode:o}"
        );
    }
}

#[test]
fn recommend_setup_respects_rune_data_dir() {
    // HOME and RUNE_DATA_DIR are two different tempdirs. Setup must write
    // config.toml into RUNE_DATA_DIR, NOT HOME/.runai. This is the bug
    // multiplier from the 2026-04-20 / 04-27 incidents — anything that
    // resolves a managed path must consult the active data dir.
    let home = tempfile::tempdir().expect("home tmp");
    let rune_data_holder = tempfile::tempdir().expect("rune_data tmp");
    let rune_data = rune_data_holder.path().to_path_buf();
    std::fs::create_dir_all(&rune_data).ok();

    // Pre-create HOME/.runai so we can verify the wizard does NOT use it.
    let home_runai = home.path().join(".runai");
    std::fs::create_dir_all(&home_runai).ok();

    let answers = "anthropic\nhttps://api.anthropic.com\nclaude-opus\noverride-key\nzh\n";
    let (success, stdout, stderr) = run_setup_with_answers(home.path(), &rune_data, answers);
    assert!(
        success,
        "setup must succeed under custom RUNE_DATA_DIR; stdout={stdout}\nstderr={stderr}"
    );

    // config.toml lands at RUNE_DATA_DIR/config.toml.
    let custom_cfg = rune_data.join("config.toml");
    assert!(
        custom_cfg.exists(),
        "config.toml must live in RUNE_DATA_DIR ({})",
        custom_cfg.display()
    );

    // And NOT at HOME/.runai/config.toml.
    let home_cfg = home_runai.join("config.toml");
    assert!(
        !home_cfg.exists(),
        "config.toml must NOT land in HOME/.runai when RUNE_DATA_DIR is overridden ({})",
        home_cfg.display()
    );

    // The wizard's "Saved to" message must point at the custom path.
    assert!(
        stdout.contains(custom_cfg.to_string_lossy().as_ref()),
        "Saved-to line must echo the custom RUNE_DATA_DIR path; stdout={stdout}"
    );

    // Follow-up `recommend status` MUST read from RUNE_DATA_DIR too — the
    // multi-machine deployment scenario relies on a single env var pinning
    // everything together.
    let out = runai_cmd(home.path(), &rune_data)
        .args(["recommend", "status"])
        .output()
        .expect("spawn recommend status");
    assert!(out.status.success(), "recommend status must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("enabled:        true"),
        "status under same RUNE_DATA_DIR reads our saved config; stdout={stdout}"
    );
    assert!(
        stdout.contains("provider:       anthropic"),
        "status reflects our provider; stdout={stdout}"
    );
    assert!(
        stdout.contains(custom_cfg.to_string_lossy().as_ref()),
        "status prints the custom config path, not HOME default; stdout={stdout}"
    );
}
