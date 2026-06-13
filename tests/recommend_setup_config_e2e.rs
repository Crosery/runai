//! Physical end-to-end tests for `runai recommend setup`.
//!
//! Each test runs the real `runai` binary in an isolated `HOME=mktemp -d`
//! sandbox (and an explicit `RUNE_DATA_DIR` where the plan requires it). The
//! setup command reads answers from stdin and writes a `config.toml` to the
//! managed data dir.
//!
//! Safety: NEVER points HOME / RUNE_DATA_DIR at the real `~/.runai/` —
//! every test gets its own tempdir.
//!
//! Skipped on Windows: unix-only `0o600` permission semantics and HOME
//! mocking limitations match the rest of the integration suite.
//!
//! Plan coverage:
//! - `recommend_setup_triggers_auto_enrich` (P0) — green here
//! - `recommend_setup_respects_rune_data_dir` (P0) — green here
//! - `recommend_setup_writes_config_0o600` (P0) — skipped: requires
//!   `summary_lang_confirmed` field on `RecommendConfig` plus a
//!   `SessionStart enrich auto-trigger` hint in the setup handler. Neither
//!   exists in this branch.
//! - `recommend_setup_idempotent_overwrites` (P0) — skipped: requires the
//!   setup handler to write `config.toml.runai-bak` before overwriting. The
//!   current `RecommendConfig::save` truncates without backing up.
#![cfg(not(target_os = "windows"))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Spawn `runai recommend setup`, pipe the provided answers in on stdin,
/// wait, return the captured output.
fn run_setup_with_answers(
    home: &Path,
    rune_data_dir: Option<&Path>,
    answers: &str,
) -> std::process::Output {
    let mut cmd = runai();
    cmd.arg("recommend")
        .arg("setup")
        .env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match rune_data_dir {
        Some(p) => {
            cmd.env("RUNE_DATA_DIR", p);
        }
        None => {
            cmd.env_remove("RUNE_DATA_DIR");
        }
    }
    let mut child = cmd.spawn().expect("spawn runai recommend setup");
    {
        let mut stdin = child.stdin.take().expect("get stdin");
        stdin
            .write_all(answers.as_bytes())
            .expect("write answers to stdin");
    }
    child
        .wait_with_output()
        .expect("wait runai recommend setup")
}

/// Make a tempdir + ensure `~/.runai/` and a CLI skills dir exist.
fn fresh_home() -> TempDir {
    let home = tempfile::tempdir().expect("create tmp HOME");
    std::fs::create_dir_all(home.path().join(".runai/skills"))
        .expect("pre-create managed skills dir");
    for cli in ["claude", "codex", "gemini", "opencode"] {
        std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
            .expect("pre-create CLI skills dir");
    }
    home
}

/// Plant a skill directory at `<skills>/<name>/SKILL.md` with no AI summary.
fn plant_skill(skills_dir: &Path, name: &str, description: &str) -> PathBuf {
    let dir = skills_dir.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let skill_md = dir.join("SKILL.md");
    std::fs::write(
        &skill_md,
        format!(
            "---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\n{description}\n"
        ),
    )
    .unwrap();
    dir
}

/// Dump the captured output on assertion failure to aid debugging.
fn label(out: &std::process::Output, tag: &str) -> String {
    format!(
        "--- {tag} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[test]
fn recommend_setup_triggers_auto_enrich() {
    let home = fresh_home();
    let data_dir = home.path().join(".runai");
    let skills_dir = data_dir.join("skills");

    // Plant 3 skills with SKILL.md but no AI summary in the DB.
    plant_skill(&skills_dir, "skill-alpha", "First test skill description");
    plant_skill(&skills_dir, "skill-beta", "Second test skill description");
    plant_skill(&skills_dir, "skill-gamma", "Third test skill description");

    // Adopt them into the DB so list_resources returns them.
    let scan_out = Command::cargo_bin("runai")
        .unwrap()
        .arg("scan")
        .env("HOME", home.path())
        .env("RUNE_DATA_DIR", &data_dir)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .expect("runai scan");
    assert!(
        scan_out.status.success(),
        "scan should succeed: {}",
        label(&scan_out, "scan")
    );

    // Run setup. The 3 planted skills have no AI summary yet, so setup should
    // announce a background enrich for 3 skills.
    let answers = "openai-compat\nhttps://api.deepseek.com/v1\ndeepseek-v4-flash\nfake-key\nzh\n";
    let out = run_setup_with_answers(home.path(), Some(&data_dir), answers);
    assert!(
        out.status.success(),
        "setup should succeed even with enrich spawn: {}",
        label(&out, "setup")
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("spawned background enrich for 3 skills missing AI summary"),
        "setup should announce a background enrich for 3 skills, got stdout:\n{stdout}"
    );
}

#[test]
fn recommend_setup_respects_rune_data_dir() {
    let home = fresh_home();
    // RUNE_DATA_DIR points somewhere completely different from HOME/.runai.
    let custom_data = tempfile::tempdir().expect("custom RUNE_DATA_DIR tempdir");
    std::fs::create_dir_all(custom_data.path().join("skills")).unwrap();

    let answers = "anthropic\nhttps://api.anthropic.com\nclaude-opus\nmy-key\nzh\n";
    let out = run_setup_with_answers(home.path(), Some(custom_data.path()), answers);
    assert!(
        out.status.success(),
        "setup should succeed under custom RUNE_DATA_DIR: {}",
        label(&out, "setup")
    );

    let custom_cfg = custom_data.path().join("config.toml");
    let home_cfg = home.path().join(".runai/config.toml");

    assert!(
        custom_cfg.exists(),
        "config.toml should land in RUNE_DATA_DIR ({})\n{}",
        custom_cfg.display(),
        label(&out, "setup")
    );
    assert!(
        !home_cfg.exists(),
        "config.toml MUST NOT leak into HOME/.runai/ when RUNE_DATA_DIR is set; found at {}\n{}",
        home_cfg.display(),
        label(&out, "setup")
    );

    // recommend status should read the same custom-location config back.
    let status_out = Command::cargo_bin("runai")
        .unwrap()
        .arg("recommend")
        .arg("status")
        .env("HOME", home.path())
        .env("RUNE_DATA_DIR", custom_data.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .expect("runai recommend status");
    let status_stdout = String::from_utf8_lossy(&status_out.stdout);
    assert!(
        status_out.status.success(),
        "recommend status should succeed: {}",
        label(&status_out, "status")
    );
    assert!(
        status_stdout.contains("anthropic"),
        "recommend status should reflect the custom config (provider=anthropic), got stdout:\n{status_stdout}"
    );
}
