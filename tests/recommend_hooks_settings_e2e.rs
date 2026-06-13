//! Physical e2e regression for `runai recommend install-hook` and `runai
//! recommend uninstall-hook` (PLANNING §1.36 / §1.37, P0).
//!
//! Each test spawns the real `runai` binary in an isolated `HOME=$(mktemp -d)`
//! with `RUNE_DATA_DIR` pointed inside the same tempdir, then asserts on the
//! on-disk shape of `<home>/.claude/settings.json` (and its `.runai-bak`
//! sidecar) after running the subcommand.
//!
//! Safety contract (`AGENTS.md` 5 条铁律): no test ever reads or writes the
//! real `~/.claude/` or `~/.runai/`. `HOME` and `RUNE_DATA_DIR` are scoped to
//! a fresh `TempDir` per test, and `SKILL_MANAGER_DATA_DIR` /
//! `XDG_CONFIG_HOME` are cleared so the binary's path resolver stays inside
//! the sandbox. `RUNAI_NO_AUTOSPAWN=1` keeps the TUI auto-spawn off.
//!
//! Skipped on Windows: `HOME` env mocking doesn't work for `dirs::home_dir()`
//! on Windows (same gate as the rest of the integration suite).
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Isolated env: HOME=tempdir, RUNE_DATA_DIR=<home>/.runai, the four CLI
/// skill dirs pre-created so any incidental path probe finds something
/// reasonable.
struct HookEnv {
    home: TempDir,
}

impl HookEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        for cli in ["claude", "codex", "gemini", "opencode"] {
            std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
                .expect("pre-create CLI skills dir");
        }
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn settings_path(&self) -> PathBuf {
        self.home().join(".claude/settings.json")
    }

    fn settings_bak_path(&self) -> PathBuf {
        self.home().join(".claude/settings.json.runai-bak")
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNE_DATA_DIR", self.home().join(".runai"))
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("XDG_CONFIG_HOME");
        cmd.output().expect("runai binary spawn")
    }
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn read_settings(path: &Path) -> serde_json::Value {
    let txt = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read settings.json at {}: {e}", path.display()));
    serde_json::from_str(&txt)
        .unwrap_or_else(|e| panic!("parse settings.json at {}: {e}\n{txt}", path.display()))
}

fn ups_commands(value: &serde_json::Value) -> Vec<String> {
    let arr = match value
        .get("hooks")
        .and_then(|h| h.get("UserPromptSubmit"))
        .and_then(|a| a.as_array())
    {
        Some(a) => a,
        None => return vec![],
    };
    let mut out = Vec::new();
    for group in arr {
        if let Some(hooks) = group.get("hooks").and_then(|h| h.as_array()) {
            for h in hooks {
                if let Some(c) = h.get("command").and_then(|c| c.as_str()) {
                    out.push(c.to_string());
                }
            }
        }
    }
    out
}

fn session_start_commands(value: &serde_json::Value) -> Vec<String> {
    let arr = match value
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .and_then(|a| a.as_array())
    {
        Some(a) => a,
        None => return vec![],
    };
    let mut out = Vec::new();
    for group in arr {
        if let Some(hooks) = group.get("hooks").and_then(|h| h.as_array()) {
            for h in hooks {
                if let Some(c) = h.get("command").and_then(|c| c.as_str()) {
                    out.push(c.to_string());
                }
            }
        }
    }
    out
}

fn post_tool_commands(value: &serde_json::Value) -> Vec<String> {
    let arr = match value
        .get("hooks")
        .and_then(|h| h.get("PostToolUse"))
        .and_then(|a| a.as_array())
    {
        Some(a) => a,
        None => return vec![],
    };
    let mut out = Vec::new();
    for group in arr {
        if let Some(hooks) = group.get("hooks").and_then(|h| h.as_array()) {
            for h in hooks {
                if let Some(c) = h.get("command").and_then(|c| c.as_str()) {
                    out.push(c.to_string());
                }
            }
        }
    }
    out
}

// ─── recommend install-hook ────────────────────────────────────────────────

#[test]
fn install_hook_idempotent_with_backup() {
    let env = HookEnv::new();
    let claude_dir = env.home().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let pre = serde_json::json!({
        "theme": "dark",
        "hooks": {
            "PostToolUse": [
                {"hooks": [{"type": "command", "command": "my-formatter"}]}
            ],
            "UserPromptSubmit": [
                {"hooks": [{"type": "command", "command": "user-existing-hook"}]}
            ]
        }
    });
    let pre_text = serde_json::to_string_pretty(&pre).unwrap();
    std::fs::write(env.settings_path(), &pre_text).unwrap();

    // First run installs the hook.
    let out1 = env.run(&["recommend", "install-hook"]);
    dump(&out1, "install-hook run 1");
    assert!(
        out1.status.success(),
        "install-hook run 1 must exit 0; stderr={}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let stdout1 = String::from_utf8_lossy(&out1.stdout).to_string();
    assert!(
        stdout1.contains("hook installed"),
        "first run should announce 'hook installed': stdout={stdout1}"
    );

    let after1 = read_settings(&env.settings_path());
    let cmds1 = ups_commands(&after1);
    assert!(
        cmds1.iter().any(|c| c == "runai recommend"),
        "UserPromptSubmit must contain our hook after run 1; cmds={cmds1:?}"
    );
    assert!(
        cmds1.iter().any(|c| c == "user-existing-hook"),
        "pre-existing UserPromptSubmit hook must be preserved; cmds={cmds1:?}"
    );
    let post1 = post_tool_commands(&after1);
    assert!(
        post1.iter().any(|c| c == "my-formatter"),
        "unrelated PostToolUse hook must be preserved; cmds={post1:?}"
    );
    assert_eq!(after1["theme"], "dark", "unrelated top-level keys preserved");

    // Backup of original content was written.
    assert!(
        env.settings_bak_path().exists(),
        ".runai-bak backup must be created on first install"
    );
    let bak_text = std::fs::read_to_string(env.settings_bak_path()).unwrap();
    let bak_json: serde_json::Value = serde_json::from_str(&bak_text).unwrap();
    assert!(
        !ups_commands(&bak_json)
            .iter()
            .any(|c| c == "runai recommend"),
        "backup must capture the pre-install content (no runai hook in it)"
    );

    // Snapshot for idempotency comparison.
    let snapshot = std::fs::read_to_string(env.settings_path()).unwrap();

    // Second run reports already-present and writes no changes.
    let out2 = env.run(&["recommend", "install-hook"]);
    dump(&out2, "install-hook run 2");
    assert!(out2.status.success(), "install-hook run 2 must exit 0");
    let stdout2 = String::from_utf8_lossy(&out2.stdout).to_string();
    assert!(
        stdout2.contains("hook already present"),
        "second run should announce 'hook already present': stdout={stdout2}"
    );

    let snapshot2 = std::fs::read_to_string(env.settings_path()).unwrap();
    assert_eq!(
        snapshot, snapshot2,
        "settings.json bytes must not change between idempotent runs"
    );
    let cmds2 = ups_commands(&read_settings(&env.settings_path()));
    let runai_count = cmds2.iter().filter(|c| *c == "runai recommend").count();
    assert_eq!(
        runai_count, 1,
        "second install-hook must not duplicate the runai hook; cmds={cmds2:?}"
    );
}

#[test]
fn install_hook_adds_session_start() {
    let env = HookEnv::new();
    // No prior settings.json; binary must create it.
    let out = env.run(&["recommend", "install-hook"]);
    dump(&out, "install-hook fresh");
    assert!(out.status.success(), "install-hook must exit 0 on fresh home");

    assert!(
        env.settings_path().exists(),
        "settings.json must be created on a fresh home"
    );
    let after = read_settings(&env.settings_path());
    let ups = ups_commands(&after);
    assert!(
        ups.iter().any(|c| c == "runai recommend"),
        "UserPromptSubmit must contain 'runai recommend' on first install; ups={ups:?}"
    );

    let session_start = session_start_commands(&after);
    assert!(
        session_start
            .iter()
            .any(|c| c == "runai recommend enrich --missing-only"),
        "SessionStart must contain 'runai recommend enrich --missing-only'; ss={session_start:?}"
    );

    // Both hooks installed in the same single run.
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("SessionStart enrich auto-trigger"),
        "stdout should mention SessionStart enrich auto-trigger: stdout={stdout}"
    );
}

#[test]
fn install_hook_removes_legacy_post_tool() {
    let env = HookEnv::new();
    let claude_dir = env.home().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let pre = serde_json::json!({
        "hooks": {
            "PostToolUse": [
                // Legacy runai PostToolUse entry — should be stripped.
                {"hooks": [{"type": "command", "command": "runai recommend post-tool"}]},
                // Unrelated user hook — must be preserved.
                {"hooks": [{"type": "command", "command": "my-formatter"}]}
            ]
        }
    });
    std::fs::write(
        env.settings_path(),
        serde_json::to_string_pretty(&pre).unwrap(),
    )
    .unwrap();

    let out = env.run(&["recommend", "install-hook"]);
    dump(&out, "install-hook legacy-post-tool");
    assert!(out.status.success(), "install-hook must exit 0");

    let after = read_settings(&env.settings_path());

    // Legacy PostToolUse entry gone.
    let post = post_tool_commands(&after);
    assert!(
        !post.iter().any(|c| c == "runai recommend post-tool"),
        "legacy 'runai recommend post-tool' PostToolUse entry must be removed; cmds={post:?}"
    );
    // Other PostToolUse hooks preserved.
    assert!(
        post.iter().any(|c| c == "my-formatter"),
        "unrelated PostToolUse hook must be preserved; cmds={post:?}"
    );
    // New UserPromptSubmit hook present.
    let ups = ups_commands(&after);
    assert!(
        ups.iter().any(|c| c == "runai recommend"),
        "new UserPromptSubmit hook must be installed; cmds={ups:?}"
    );
}

#[test]
fn install_hook_handles_missing_or_corrupt() {
    // Case A: ~/.claude/settings.json does not exist at all. install-hook
    // must create the file with our hook inside. (HookEnv pre-creates
    // .claude/skills/ for symmetry with other suites, so the directory may
    // exist, but settings.json does not.)
    let env_missing = HookEnv::new();
    assert!(
        !env_missing.settings_path().exists(),
        "precondition: settings.json must not exist before install-hook"
    );

    let out_missing = env_missing.run(&["recommend", "install-hook"]);
    dump(&out_missing, "install-hook missing-claude");
    assert!(
        out_missing.status.success(),
        "install-hook on missing ~/.claude must exit 0"
    );
    assert!(
        env_missing.settings_path().exists(),
        "install-hook must create ~/.claude/settings.json when missing"
    );
    let fresh = read_settings(&env_missing.settings_path());
    let ups = ups_commands(&fresh);
    assert!(
        ups.iter().any(|c| c == "runai recommend"),
        "fresh settings.json must contain our hook; cmds={ups:?}"
    );

    // Case B: corrupt JSON in settings.json — must error, no .runai-bak.
    let env_corrupt = HookEnv::new();
    let claude_dir_c = env_corrupt.home().join(".claude");
    std::fs::create_dir_all(&claude_dir_c).unwrap();
    std::fs::write(env_corrupt.settings_path(), "{not valid json,,,").unwrap();

    let out_corrupt = env_corrupt.run(&["recommend", "install-hook"]);
    dump(&out_corrupt, "install-hook corrupt-json");
    assert!(
        !out_corrupt.status.success(),
        "install-hook must NOT exit 0 when settings.json is invalid JSON"
    );
    // No backup written for a corrupt file (we never reached the write path).
    assert!(
        !env_corrupt.settings_bak_path().exists(),
        ".runai-bak must NOT be written when input JSON is corrupt"
    );
    // Original corrupt content is unchanged.
    let still = std::fs::read_to_string(env_corrupt.settings_path()).unwrap();
    assert_eq!(
        still, "{not valid json,,,",
        "corrupt settings.json content must not be rewritten when parse fails"
    );
}

#[test]
fn install_hook_canonical_path() {
    // install-hook must only touch ~/.claude/settings.json. The other three
    // CLI homes (codex / gemini / opencode) must remain unmodified, since the
    // recommend router hook is wired exclusively into Claude Code.
    let env = HookEnv::new();

    // Plant sentinel settings files in the other CLI homes.
    let codex_dir = env.home().join(".codex");
    let gemini_dir = env.home().join(".gemini");
    let opencode_dir = env.home().join(".config/opencode");
    for d in [&codex_dir, &gemini_dir, &opencode_dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    let codex_settings = codex_dir.join("config.toml");
    let gemini_settings = gemini_dir.join("settings.json");
    let opencode_settings = opencode_dir.join("opencode.json");
    std::fs::write(&codex_settings, "# sentinel codex\n").unwrap();
    std::fs::write(&gemini_settings, "{\"sentinel\":\"gemini\"}").unwrap();
    std::fs::write(&opencode_settings, "{\"sentinel\":\"opencode\"}").unwrap();

    let codex_before = std::fs::read_to_string(&codex_settings).unwrap();
    let gemini_before = std::fs::read_to_string(&gemini_settings).unwrap();
    let opencode_before = std::fs::read_to_string(&opencode_settings).unwrap();

    let out = env.run(&["recommend", "install-hook"]);
    dump(&out, "install-hook canonical-path");
    assert!(out.status.success(), "install-hook must exit 0");

    // Claude settings.json now has the hook.
    let after = read_settings(&env.settings_path());
    let ups = ups_commands(&after);
    assert!(
        ups.iter().any(|c| c == "runai recommend"),
        "claude settings.json must contain our hook; cmds={ups:?}"
    );

    // Other CLI config files are byte-identical to what we planted.
    assert_eq!(
        std::fs::read_to_string(&codex_settings).unwrap(),
        codex_before,
        "~/.codex/config.toml must not be touched by install-hook"
    );
    assert_eq!(
        std::fs::read_to_string(&gemini_settings).unwrap(),
        gemini_before,
        "~/.gemini/settings.json must not be touched by install-hook"
    );
    assert_eq!(
        std::fs::read_to_string(&opencode_settings).unwrap(),
        opencode_before,
        "~/.config/opencode/opencode.json must not be touched by install-hook"
    );
}
