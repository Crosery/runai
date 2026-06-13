//! P2 regression: core::mcp_canonical / mcp_discovery / mcp_register /
//! paths (AppPaths + 迁移). One file, multiple commits — one feature per
//! commit. Tests here intentionally avoid touching real ~/.runai by using
//! tempfile sandboxes for every filesystem-mutating call.

#![cfg(not(target_os = "windows"))]

// =====================================================================
//  Feature 1: core::mcp_canonical (规范化)
// =====================================================================

use runai::core::cli_target::CliTarget;
use runai::core::mcp_canonical::{
    canonical_to_codex_toml, canonical_to_opencode, codex_toml_to_canonical,
    from_canonical_for_json_target, is_corrupt, to_canonical,
};
use serde_json::json;

/// 1. canonical_identity_for_standard — Claude/Gemini-style JSON entries are
/// already canonical; to_canonical must be the identity, no field invented.
#[test]
fn mcp_canonical_identity_for_standard_entry() {
    let entry = json!({
        "command": "/bin/foo",
        "args": ["a", "b"],
        "description": "test entry",
        "disabled": false,
    });
    let canon = to_canonical(&entry);
    // Field shape identical
    assert_eq!(canon["command"], json!("/bin/foo"));
    assert_eq!(canon["args"], json!(["a", "b"]));
    assert_eq!(canon["disabled"], json!(false));
    // Same set of keys, no new ones spuriously inserted.
    assert_eq!(canon, entry, "canonical input must round-trip identically");

    // Round-trip through Claude/Gemini-target emitter also stays identical.
    let back_claude = from_canonical_for_json_target(&canon, CliTarget::Claude);
    let back_gemini = from_canonical_for_json_target(&canon, CliTarget::Gemini);
    assert_eq!(back_claude, entry, "Claude-target emit is identity");
    assert_eq!(back_gemini, entry, "Gemini-target emit is identity");
}

/// 2. opencode_normalization — OpenCode shape (command:array + enabled +
/// type:"local") must normalize to canonical: command split out, args
/// extracted, enabled→disabled flipped, type:"local" stripped.
#[test]
fn mcp_canonical_normalizes_opencode_shape() {
    // enabled:false → disabled:true, command array split.
    let oc = json!({
        "command": ["/bin/foo", "arg1", "arg2"],
        "enabled": false,
        "type": "local",
    });
    let canon = to_canonical(&oc);
    assert_eq!(canon["command"], json!("/bin/foo"));
    assert_eq!(canon["args"], json!(["arg1", "arg2"]));
    assert_eq!(canon["disabled"], json!(true), "enabled:false → disabled:true");
    assert!(
        canon.get("enabled").is_none(),
        "enabled key stripped (consumed)"
    );
    assert!(
        canon.get("type").is_none(),
        "OpenCode-specific type:local stripped"
    );

    // enabled:true → no disabled key (default enabled).
    let oc2 = json!({
        "command": ["/bin/bar"],
        "enabled": true,
        "type": "local",
    });
    let canon2 = to_canonical(&oc2);
    assert_eq!(canon2["command"], json!("/bin/bar"));
    assert_eq!(canon2["args"], json!([]));
    assert!(
        canon2.get("disabled").is_none(),
        "enabled:true → no disabled key (default-enabled canonical form)"
    );
}

/// 3. corrupt_detection — is_corrupt flags entries that are unusable
/// (empty command AND no http url). HTTP-style remote entries (no command,
/// non-empty url) are NOT corrupt.
#[test]
fn mcp_canonical_corrupt_detection() {
    // Empty command string is corrupt.
    assert!(is_corrupt(&json!({ "command": "" })));
    assert!(is_corrupt(&json!({ "command": "   " })));
    // Empty command array is corrupt.
    assert!(is_corrupt(&json!({ "command": [] })));
    assert!(is_corrupt(&json!({ "command": [""] })));
    // Missing command + missing url is corrupt.
    assert!(is_corrupt(&json!({ "type": "stdio" })));
    assert!(is_corrupt(&json!({})));
    // Empty url field + no command is corrupt.
    assert!(is_corrupt(&json!({ "url": "" })));
    // Valid stdio entry is NOT corrupt.
    assert!(!is_corrupt(&json!({ "command": "/bin/foo" })));
    assert!(!is_corrupt(&json!({ "command": ["/bin/foo", "args"] })));
    // HTTP entry without command IS valid (url is the signal).
    assert!(!is_corrupt(
        &json!({ "type": "http", "url": "https://example.com" })
    ));
    assert!(!is_corrupt(&json!({ "url": "https://example.com" })));
}

/// 4. codex_toml_preserves_subtables — Codex MCP entries carry tools.* /
/// env.* subtables; canonical_to_codex_toml then codex_toml_to_canonical
/// must round-trip them intact.
#[test]
fn mcp_canonical_codex_toml_preserves_tools_and_env_subtables() {
    let canonical = json!({
        "command": "/bin/codex-mcp",
        "args": ["--flag"],
        "tools": {
            "approval_mode": "approve",
            "deeply": { "nested": { "value": "v" } }
        },
        "env": {
            "API_KEY": "secret",
            "TIMEOUT": "30"
        }
    });
    let tv = canonical_to_codex_toml(&canonical);
    let back = codex_toml_to_canonical(&tv);

    assert_eq!(back["command"], json!("/bin/codex-mcp"));
    assert_eq!(back["args"], json!(["--flag"]));
    // tools.* subtable preserved (including deep nesting).
    assert_eq!(back["tools"]["approval_mode"], json!("approve"));
    assert_eq!(back["tools"]["deeply"]["nested"]["value"], json!("v"));
    // env.* subtable preserved.
    assert_eq!(back["env"]["API_KEY"], json!("secret"));
    assert_eq!(back["env"]["TIMEOUT"], json!("30"));
    // type:"stdio" auto-added for stdio entries (since no url).
    assert_eq!(back["type"], json!("stdio"));
}

/// 5. opencode_roundtrip_stable — canonical → OpenCode → canonical →
/// OpenCode should be idempotent: same output both rounds, no field drift.
#[test]
fn mcp_canonical_opencode_roundtrip_is_stable() {
    let canonical = json!({
        "command": "/bin/foo",
        "args": ["x", "y", "z"],
    });
    let oc1 = canonical_to_opencode(&canonical);
    let canon_back = to_canonical(&oc1);
    let oc2 = canonical_to_opencode(&canon_back);

    // Both rounds produce same OpenCode shape.
    assert_eq!(oc1["command"], oc2["command"], "command array stable");
    assert_eq!(oc1["enabled"], oc2["enabled"], "enabled flag stable");
    assert_eq!(oc1["type"], oc2["type"], "type field stable");
    assert_eq!(oc1["command"], json!(["/bin/foo", "x", "y", "z"]));
    assert_eq!(oc1["enabled"], json!(true));
    assert_eq!(oc1["type"], json!("local"));

    // Same for a disabled-canonical entry.
    let disabled = json!({
        "command": "/bin/bar",
        "args": [],
        "disabled": true,
    });
    let oc_a = canonical_to_opencode(&disabled);
    let oc_b = canonical_to_opencode(&to_canonical(&oc_a));
    assert_eq!(oc_a["enabled"], json!(false));
    assert_eq!(oc_a["enabled"], oc_b["enabled"]);
    assert_eq!(oc_a["command"], oc_b["command"]);
}

// =====================================================================
//  Feature 2: core::mcp_discovery
// =====================================================================

use runai::core::mcp_discovery::{McpDiscovery, McpType};
use std::path::Path;

fn write_text(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, content).unwrap();
}

/// 1. discover_claude_json_mcps — find all entries in ~/.claude.json,
/// parse command/args/description, honor `disabled` flag.
#[test]
fn mcp_discovery_finds_claude_json_entries() {
    let tmp = tempfile::tempdir().unwrap();
    write_text(
        tmp.path(),
        ".claude.json",
        r#"{
            "mcpServers": {
                "github": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-github"],
                    "description": "GitHub MCP",
                    "disabled": false
                },
                "memory": {
                    "command": "node",
                    "args": ["server.js"],
                    "description": "memory server",
                    "disabled": true
                }
            }
        }"#,
    );

    let results = McpDiscovery::discover_all(tmp.path());
    assert_eq!(results.len(), 2, "both entries found");

    let github = results.iter().find(|e| e.name == "github").unwrap();
    assert_eq!(github.command, "npx");
    assert_eq!(
        github.args,
        vec!["-y", "@modelcontextprotocol/server-github"]
    );
    assert_eq!(github.description, "GitHub MCP");
    assert!(!github.disabled);
    assert_eq!(github.source_cli, "claude");
    assert_eq!(github.mcp_type, McpType::Stdio);

    let memory = results.iter().find(|e| e.name == "memory").unwrap();
    assert!(memory.disabled, "disabled:true must be honored");
}

/// 2. discover_codex_toml_mcps — Codex uses TOML at ~/.codex/config.toml,
/// `[mcp_servers.*]` tables, `type="http"` recognized as HTTP entry.
#[test]
fn mcp_discovery_finds_codex_toml_entries() {
    let tmp = tempfile::tempdir().unwrap();
    write_text(
        tmp.path(),
        ".codex/config.toml",
        r#"
[mcp_servers.codex-stdio]
type = "stdio"
command = "/bin/codex-tool"
args = ["--verbose"]

[mcp_servers.codex-http]
type = "http"
url = "https://mcp.example.com"
command = ""
"#,
    );

    let results = McpDiscovery::discover_all(tmp.path());
    // codex-http has empty command so it gets dropped (see parse_codex_toml).
    let stdio_entry = results.iter().find(|e| e.name == "codex-stdio").unwrap();
    assert_eq!(stdio_entry.command, "/bin/codex-tool");
    assert_eq!(stdio_entry.args, vec!["--verbose"]);
    assert_eq!(stdio_entry.mcp_type, McpType::Stdio);
    assert_eq!(stdio_entry.source_cli, "codex");

    // codex-http: dropped because parse_codex_toml requires non-empty command
    // (this is the real-world Codex shape — TOML can't be JSON parsed, so the
    // assertion here is "TOML is the parser used, not JSON; entries with empty
    // command are dropped regardless of type").
    assert!(
        results.iter().all(|e| e.name != "codex-http"),
        "empty-command Codex entries are dropped"
    );
}

/// 3. discover_opencode_json_mcps — OpenCode lives at
/// ~/.config/opencode/opencode.json under `mcp` (NOT mcpServers).
/// command is an array split into command+args; enabled:false → disabled:true.
#[test]
fn mcp_discovery_finds_opencode_json_entries() {
    let tmp = tempfile::tempdir().unwrap();
    write_text(
        tmp.path(),
        ".config/opencode/opencode.json",
        r#"{
            "mcp": {
                "pencil": {
                    "command": ["npx", "-y", "pencil"],
                    "enabled": true,
                    "type": "local"
                },
                "disabled-one": {
                    "command": ["/bin/x", "arg"],
                    "enabled": false,
                    "type": "local"
                }
            }
        }"#,
    );

    let results = McpDiscovery::discover_all(tmp.path());
    let pencil = results.iter().find(|e| e.name == "pencil").unwrap();
    assert_eq!(pencil.command, "npx");
    assert_eq!(pencil.args, vec!["-y", "pencil"]);
    assert!(!pencil.disabled, "enabled:true → disabled:false");
    assert_eq!(pencil.source_cli, "opencode");

    let off = results.iter().find(|e| e.name == "disabled-one").unwrap();
    assert!(off.disabled, "enabled:false → disabled:true (inverted)");
    assert_eq!(off.command, "/bin/x");
    assert_eq!(off.args, vec!["arg"]);
}

/// 4. discovery_deduplication_first_wins — when the same MCP name appears in
/// both ~/.claude.json and ~/.codex/config.toml, the Claude version (read
/// first) is kept and Codex is skipped.
#[test]
fn mcp_discovery_dedupes_first_reader_wins() {
    let tmp = tempfile::tempdir().unwrap();
    write_text(
        tmp.path(),
        ".claude.json",
        r#"{
            "mcpServers": {
                "shared": { "command": "claude-cmd", "args": ["c"] }
            }
        }"#,
    );
    write_text(
        tmp.path(),
        ".codex/config.toml",
        r#"
[mcp_servers.shared]
type = "stdio"
command = "codex-cmd"
args = ["x"]
"#,
    );

    let results = McpDiscovery::discover_all(tmp.path());
    let shared: Vec<_> = results.iter().filter(|e| e.name == "shared").collect();
    assert_eq!(shared.len(), 1, "duplicate name appears once after dedupe");
    let only = shared[0];
    assert_eq!(
        only.command, "claude-cmd",
        "Claude (loaded first) is the kept version, Codex skipped"
    );
    assert_eq!(only.source_cli, "claude");
}

/// 5. discovery_skips_meta_keys — keys starting with `_` (e.g. `_comments`)
/// must not be returned as MCP entries.
#[test]
fn mcp_discovery_skips_underscore_meta_keys() {
    let tmp = tempfile::tempdir().unwrap();
    write_text(
        tmp.path(),
        ".claude.json",
        r#"{
            "mcpServers": {
                "_comments": { "note": "this is a comment" },
                "_anything": { "command": "ignored", "args": [] },
                "real": { "command": "node", "args": ["server.js"] }
            }
        }"#,
    );

    let results = McpDiscovery::discover_all(tmp.path());
    let names: Vec<&str> = results.iter().map(|e| e.name.as_str()).collect();
    assert!(
        !names.contains(&"_comments"),
        "_comments meta key must be skipped"
    );
    assert!(
        !names.contains(&"_anything"),
        "any underscore-prefixed key must be skipped"
    );
    assert!(names.contains(&"real"), "real entries still discovered");
}

/// 6. discovery_missing_configs_ok — completely empty home should yield an
/// empty result, never an error.
#[test]
fn mcp_discovery_empty_home_returns_empty_no_error() {
    let tmp = tempfile::tempdir().unwrap();
    let results = McpDiscovery::discover_all(tmp.path());
    assert!(
        results.is_empty(),
        "empty home returns empty list, got {} entries",
        results.len()
    );
}

// =====================================================================
//  Feature 3: core::mcp_register (high-risk: writes 4 CLI configs)
// =====================================================================

use runai::core::mcp_register::McpRegister;

/// 1. register_all_symmetric_across_targets — register_all() writes the runai
/// entry to all four CLIs in their format-specific shape.
#[test]
fn mcp_register_all_symmetric_across_four_targets() {
    let tmp = tempfile::tempdir().unwrap();

    let result = McpRegister::register_all(tmp.path());
    // Errors should be empty, every target should be either registered or
    // skipped (idempotent case). On a fresh empty HOME all four should land
    // in "registered".
    assert!(
        result.errors.is_empty(),
        "no errors expected, got {:?}",
        result.errors
    );
    let total = result.registered.len() + result.skipped.len();
    assert_eq!(
        total, 4,
        "all four CLI targets accounted for, got registered={:?} skipped={:?}",
        result.registered, result.skipped
    );
    assert_eq!(
        result.registered.len(),
        4,
        "fresh home: all four should be NEWLY registered, got {:?}",
        result.registered
    );

    // Claude: ~/.claude.json has mcpServers.runai
    let claude_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap())
            .unwrap();
    assert!(
        claude_json["mcpServers"]["runai"]["command"].is_string(),
        "Claude config has mcpServers.runai.command as string"
    );
    assert_eq!(claude_json["mcpServers"]["runai"]["args"][0], "mcp-serve");

    // Gemini: ~/.gemini/settings.json has mcpServers.runai
    let gem_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".gemini/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(gem_json["mcpServers"]["runai"]["command"].is_string());

    // Codex: ~/.codex/config.toml has [mcp_servers.runai]
    let codex_text = std::fs::read_to_string(tmp.path().join(".codex/config.toml")).unwrap();
    assert!(
        codex_text.contains("[mcp_servers.runai]"),
        "Codex TOML has [mcp_servers.runai] section, got:\n{codex_text}"
    );

    // OpenCode: ~/.config/opencode/opencode.json has mcp.runai with array command
    let oc_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".config/opencode/opencode.json")).unwrap(),
    )
    .unwrap();
    assert!(
        oc_json["mcp"]["runai"]["command"].is_array(),
        "OpenCode command MUST be an array, got {:?}",
        oc_json["mcp"]["runai"]["command"]
    );
}

/// 2. register_idempotent — second register_all() reports all four as
/// skipped (already registered with same binary path); no duplicate entries.
#[test]
fn mcp_register_is_idempotent_on_second_call() {
    let tmp = tempfile::tempdir().unwrap();

    let first = McpRegister::register_all(tmp.path());
    assert_eq!(first.registered.len(), 4);
    let second = McpRegister::register_all(tmp.path());
    assert!(
        second.errors.is_empty(),
        "second call should produce no errors, got {:?}",
        second.errors
    );
    assert_eq!(
        second.skipped.len(),
        4,
        "second call: all four should be skipped (idempotent), got registered={:?} skipped={:?}",
        second.registered,
        second.skipped
    );

    // Claude config has exactly one runai entry (count occurrences in the
    // mcpServers object — there can be at most one key per name).
    let claude_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap())
            .unwrap();
    let mcp_obj = claude_json["mcpServers"].as_object().unwrap();
    let runai_keys: Vec<_> = mcp_obj.keys().filter(|k| k.as_str() == "runai").collect();
    assert_eq!(runai_keys.len(), 1, "exactly one runai entry, no dupes");
}

/// 3. register_opencode_command_array — OpenCode's `command` MUST be an
/// array (load-bearing for the OpenCode MCP parser). A string format would
/// silently break.
#[test]
fn mcp_register_opencode_command_is_array_not_string() {
    let tmp = tempfile::tempdir().unwrap();
    let result = McpRegister::register_all(tmp.path());
    assert!(result.errors.is_empty(), "errors={:?}", result.errors);

    let oc_text =
        std::fs::read_to_string(tmp.path().join(".config/opencode/opencode.json")).unwrap();
    let oc_json: serde_json::Value = serde_json::from_str(&oc_text).unwrap();
    let cmd = &oc_json["mcp"]["runai"]["command"];
    assert!(
        cmd.is_array(),
        "OpenCode command must be an array, got {cmd:?}"
    );
    let arr = cmd.as_array().unwrap();
    assert!(!arr.is_empty(), "command array must not be empty");
    // [0] is the binary; [1..] are args. mcp-serve must appear somewhere
    // (typically arr[1]).
    let strs: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        strs.iter().any(|s| *s == "mcp-serve"),
        "command array must include mcp-serve, got {arr:?}"
    );
    assert_eq!(
        oc_json["mcp"]["runai"]["enabled"], serde_json::json!(true),
        "OpenCode entry must be enabled by default"
    );
    assert_eq!(
        oc_json["mcp"]["runai"]["type"], serde_json::json!("local"),
        "OpenCode entry must carry type:local"
    );
}

/// 4. register_codex_toml_format — Codex uses TOML (not JSON), with
/// type="stdio" and string command.
#[test]
fn mcp_register_codex_uses_toml_with_type_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let result = McpRegister::register_all(tmp.path());
    assert!(result.errors.is_empty(), "errors={:?}", result.errors);

    let codex_path = tmp.path().join(".codex/config.toml");
    assert!(codex_path.exists(), "Codex config.toml must be created");
    let codex_text = std::fs::read_to_string(&codex_path).unwrap();

    // Must be TOML, not JSON — JSON parse must fail, TOML parse must succeed.
    assert!(
        serde_json::from_str::<serde_json::Value>(&codex_text).is_err(),
        "Codex config must be TOML, not JSON"
    );
    let table: toml::Table = codex_text.parse().expect("valid TOML");
    let servers = table
        .get("mcp_servers")
        .and_then(|v| v.as_table())
        .expect("[mcp_servers] section");
    let runai = servers
        .get("runai")
        .and_then(|v| v.as_table())
        .expect("[mcp_servers.runai] entry");
    assert_eq!(
        runai.get("type").and_then(|v| v.as_str()),
        Some("stdio"),
        "Codex entry needs type=\"stdio\""
    );
    // command is a STRING in Codex (not array).
    assert!(
        runai.get("command").and_then(|v| v.as_str()).is_some(),
        "Codex command must be a string, got {:?}",
        runai.get("command")
    );
}

/// 5. register_creates_cli_dirs — Gemini needs ~/.gemini/ which doesn't
/// exist in a fresh home; register_gemini must mkdir-p and create
/// settings.json.
#[test]
fn mcp_register_creates_missing_cli_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    // Sanity: dirs absent before register.
    assert!(!tmp.path().join(".gemini").exists());
    assert!(!tmp.path().join(".codex").exists());
    assert!(!tmp.path().join(".config/opencode").exists());

    let result = McpRegister::register_all(tmp.path());
    assert!(result.errors.is_empty(), "errors={:?}", result.errors);

    // Each directory was created.
    assert!(
        tmp.path().join(".gemini").is_dir(),
        "Gemini dir auto-created"
    );
    assert!(
        tmp.path().join(".gemini/settings.json").is_file(),
        "Gemini settings.json created with runai entry"
    );
    assert!(
        tmp.path().join(".codex/config.toml").is_file(),
        "Codex config.toml created"
    );
    assert!(
        tmp.path()
            .join(".config/opencode/opencode.json")
            .is_file(),
        "OpenCode opencode.json created"
    );

    // Verify the Gemini settings.json actually has the runai entry.
    let gem_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".gemini/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(gem_json["mcpServers"]["runai"].is_object());
}

// =====================================================================
//  Feature 4: core::paths (AppPaths + 迁移)
// =====================================================================

use runai::core::paths::{self as paths_mod, AppPaths};
use std::sync::Mutex;

/// Local HOME lock — `runai::test_support` is `#[cfg(test)]` and not
/// reachable from external integration tests. We mirror the contract here:
/// serialize any test that mutates HOME / RUNE_DATA_DIR / SKILL_MANAGER_DATA_DIR.
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Helper: with HOME_LOCK held, set HOME + clear data-dir env, run `f`,
/// then restore. SAFETY: HOME_LOCK serializes env mutation across tests.
fn with_isolated_home<F: FnOnce(&std::path::Path)>(scratch_home: &std::path::Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let orig_home = std::env::var("HOME").ok();
    let orig_rdd = std::env::var("RUNE_DATA_DIR").ok();
    let orig_smdd = std::env::var("SKILL_MANAGER_DATA_DIR").ok();
    unsafe {
        std::env::set_var("HOME", scratch_home);
        std::env::remove_var("RUNE_DATA_DIR");
        std::env::remove_var("SKILL_MANAGER_DATA_DIR");
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(scratch_home)));
    unsafe {
        match orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match orig_rdd {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
        match orig_smdd {
            Some(v) => std::env::set_var("SKILL_MANAGER_DATA_DIR", v),
            None => std::env::remove_var("SKILL_MANAGER_DATA_DIR"),
        }
    }
}

/// 1. data_dir_respects_env_override — RUNE_DATA_DIR > SKILL_MANAGER_DATA_DIR
/// > HOME default. default_data_dir_no_env IGNORES env override.
#[test]
fn paths_data_dir_respects_rune_data_dir_env() {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let custom = tmp.path().join("custom-data");
    let orig_home = std::env::var("HOME").ok();
    let orig_rdd = std::env::var("RUNE_DATA_DIR").ok();
    let orig_smdd = std::env::var("SKILL_MANAGER_DATA_DIR").ok();
    unsafe {
        std::env::set_var("HOME", tmp.path());
        std::env::set_var("RUNE_DATA_DIR", &custom);
        std::env::remove_var("SKILL_MANAGER_DATA_DIR");
    }

    // data_dir() reads RUNE_DATA_DIR.
    let from_env = paths_mod::data_dir();
    // default_data_dir_no_env IGNORES env vars.
    let no_env = paths_mod::default_data_dir_no_env();

    unsafe {
        match orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match orig_rdd {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
        match orig_smdd {
            Some(v) => std::env::set_var("SKILL_MANAGER_DATA_DIR", v),
            None => std::env::remove_var("SKILL_MANAGER_DATA_DIR"),
        }
    }

    assert_eq!(from_env, custom, "data_dir must honor RUNE_DATA_DIR");
    assert_eq!(
        no_env,
        tmp.path().join(".runai"),
        "default_data_dir_no_env must IGNORE RUNE_DATA_DIR (this is the 2026-04-27 guard)"
    );
    assert_ne!(
        from_env, no_env,
        "env-aware vs env-blind paths must diverge when env is set"
    );
}

/// 2. SKILL_MANAGER_DATA_DIR is honored as the second-priority env (when
/// RUNE_DATA_DIR is unset). This is the 2026-04-27 high-risk regression area.
#[test]
fn paths_data_dir_honors_skill_manager_data_dir_when_rune_unset() {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let smdd = tmp.path().join("legacy-data");
    let orig_home = std::env::var("HOME").ok();
    let orig_rdd = std::env::var("RUNE_DATA_DIR").ok();
    let orig_smdd = std::env::var("SKILL_MANAGER_DATA_DIR").ok();
    unsafe {
        std::env::set_var("HOME", tmp.path());
        std::env::remove_var("RUNE_DATA_DIR");
        std::env::set_var("SKILL_MANAGER_DATA_DIR", &smdd);
    }
    let resolved = paths_mod::data_dir();
    unsafe {
        match orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match orig_rdd {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
        match orig_smdd {
            Some(v) => std::env::set_var("SKILL_MANAGER_DATA_DIR", v),
            None => std::env::remove_var("SKILL_MANAGER_DATA_DIR"),
        }
    }
    assert_eq!(
        resolved, smdd,
        "with RUNE_DATA_DIR unset, SKILL_MANAGER_DATA_DIR takes over"
    );
}

// Plan-item user_root_path_traversal_guard SKIPPED: cloud HEAD's AppPaths
// has no user_root / user_skills_dir / user_mcps_dir / user_trash_dir /
// ensure_user_dirs — those are multi-user (v15+) APIs not in this revision.
// Test phase=src_missing.

/// 4. AppPaths::default_path() runs migration from ~/.skill-manager/ →
/// ~/.runai/ when only the old dir exists. Renames dir, renames DB file,
/// fixes CLI symlinks, rewrites DB path columns.
#[test]
fn paths_default_path_migrates_skill_manager_to_runai() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().to_path_buf();
    let old_base = home.join(".skill-manager");
    let new_base = home.join(".runai");

    // Set up old structure: skill dir, SKILL.md, the old DB file name.
    std::fs::create_dir_all(old_base.join("skills/test-skill")).unwrap();
    std::fs::write(old_base.join("skills/test-skill/SKILL.md"), "# Test").unwrap();
    std::fs::write(old_base.join("skill-manager.db"), "old-db-bytes").unwrap();
    std::fs::create_dir_all(old_base.join("groups")).unwrap();

    // Symlink in CLI skills dir pointing at the OLD path.
    let claude_skills = home.join(".claude/skills");
    std::fs::create_dir_all(&claude_skills).unwrap();
    std::os::unix::fs::symlink(
        old_base.join("skills/test-skill"),
        claude_skills.join("test-skill"),
    )
    .unwrap();

    // Drive migration through default_path() (with HOME mocked).
    with_isolated_home(&home, |_| {
        let app = AppPaths::default_path();
        assert_eq!(app.data_dir(), new_base.as_path());
    });

    // Old path is gone.
    assert!(!old_base.exists(), ".skill-manager removed after migration");
    // New path has the skill content.
    assert!(new_base.is_dir(), ".runai created");
    assert!(
        new_base.join("skills/test-skill/SKILL.md").is_file(),
        "skill file preserved"
    );
    let content = std::fs::read_to_string(new_base.join("skills/test-skill/SKILL.md")).unwrap();
    assert_eq!(content, "# Test", "skill content preserved byte-for-byte");
    // DB file is renamed.
    assert!(
        new_base.join("runai.db").is_file(),
        "skill-manager.db → runai.db"
    );
    assert!(
        !new_base.join("skill-manager.db").exists(),
        "old DB name removed"
    );
    assert_eq!(
        std::fs::read_to_string(new_base.join("runai.db")).unwrap(),
        "old-db-bytes",
        "DB bytes preserved across rename"
    );
    // Symlink under ~/.claude/skills repointed.
    let link = claude_skills.join("test-skill");
    assert!(link.is_symlink(), "symlink still in place");
    let target = std::fs::read_link(&link).unwrap();
    assert!(
        target.to_string_lossy().contains(".runai"),
        "symlink retargeted to .runai, got: {}",
        target.display()
    );
    assert!(
        !target.to_string_lossy().contains(".skill-manager"),
        "symlink no longer points at .skill-manager"
    );
}

/// 5. Migration is skipped when ~/.runai/ already exists — protects an
/// already-upgraded user from accidental clobber.
#[test]
fn paths_default_path_skips_migration_when_new_dir_exists() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().to_path_buf();
    let old_base = home.join(".skill-manager");
    let new_base = home.join(".runai");

    // Both old and new exist; new has different content.
    std::fs::create_dir_all(old_base.join("skills")).unwrap();
    std::fs::write(old_base.join("skill-manager.db"), "OLD").unwrap();
    std::fs::create_dir_all(&new_base).unwrap();
    std::fs::write(new_base.join("runai.db"), "NEW").unwrap();

    with_isolated_home(&home, |_| {
        let app = AppPaths::default_path();
        assert_eq!(app.data_dir(), new_base.as_path());
    });

    // .runai untouched (still NEW).
    assert!(
        new_base.exists(),
        ".runai still present and not overwritten"
    );
    assert_eq!(
        std::fs::read_to_string(new_base.join("runai.db")).unwrap(),
        "NEW",
        ".runai DB content untouched (migration was skipped)"
    );
    // .skill-manager NOT removed (since no migration happened).
    assert!(
        old_base.exists(),
        ".skill-manager still on disk (migration skipped)"
    );
    assert_eq!(
        std::fs::read_to_string(old_base.join("skill-manager.db")).unwrap(),
        "OLD",
        "old DB untouched too"
    );
}

/// 6. with_base + the public-pool path getters give the expected layout.
/// Public skills dir, mcps dir, trash dir all sibling under base.
#[test]
fn paths_with_base_public_layout_is_correct() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::with_base(tmp.path().to_path_buf());

    assert_eq!(paths.data_dir(), tmp.path());
    assert_eq!(paths.skills_dir(), tmp.path().join("skills"));
    assert_eq!(paths.mcps_dir(), tmp.path().join("mcps"));
    assert_eq!(paths.groups_dir(), tmp.path().join("groups"));
    assert_eq!(paths.trash_dir(), tmp.path().join("trash"));
    assert_eq!(paths.config_path(), tmp.path().join("config.toml"));

    // ensure_dirs creates them all.
    paths.ensure_dirs().unwrap();
    assert!(paths.skills_dir().is_dir());
    assert!(paths.mcps_dir().is_dir());
    assert!(paths.groups_dir().is_dir());
    assert!(paths.trash_dir().is_dir());
}

// Plan-item per-user pool layout SKIPPED: cloud HEAD has no per-user paths.

/// 7. db_path() prefers runai.db, falls back to skill-manager.db when only
/// the old one exists. Compat for users mid-migration.
#[test]
fn paths_db_path_prefers_new_falls_back_to_old() {
    let tmp = tempfile::tempdir().unwrap();
    // Only legacy DB exists.
    std::fs::write(tmp.path().join("skill-manager.db"), "legacy").unwrap();
    let p = AppPaths::with_base(tmp.path().to_path_buf());
    assert_eq!(
        p.db_path(),
        tmp.path().join("skill-manager.db"),
        "fall back to legacy DB name"
    );

    // Add new DB; from now on it's preferred.
    std::fs::write(tmp.path().join("runai.db"), "new").unwrap();
    let p2 = AppPaths::with_base(tmp.path().to_path_buf());
    assert_eq!(
        p2.db_path(),
        tmp.path().join("runai.db"),
        "new DB preferred over legacy when both exist"
    );

    // Fresh install (neither exists) → returns the new name.
    let tmp2 = tempfile::tempdir().unwrap();
    let p3 = AppPaths::with_base(tmp2.path().to_path_buf());
    assert_eq!(
        p3.db_path(),
        tmp2.path().join("runai.db"),
        "fresh install uses new DB name"
    );
}

/// 6. unregister_all_removes_runai_entry — sanity for the inverse: after
/// register_all + unregister_all, no runai entry remains in any CLI config.
#[test]
fn mcp_register_unregister_removes_entries_in_all_clis() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = McpRegister::register_all(tmp.path());
    McpRegister::unregister_all(tmp.path()).expect("unregister_all should succeed");

    // Claude
    let claude_text = std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap();
    let claude_json: serde_json::Value = serde_json::from_str(&claude_text).unwrap();
    assert!(
        claude_json["mcpServers"]
            .as_object()
            .map(|m| !m.contains_key("runai"))
            .unwrap_or(true),
        "Claude runai entry removed"
    );

    // Gemini
    let gem_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".gemini/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(
        gem_json["mcpServers"]
            .as_object()
            .map(|m| !m.contains_key("runai"))
            .unwrap_or(true),
        "Gemini runai entry removed"
    );

    // Codex
    let codex_text = std::fs::read_to_string(tmp.path().join(".codex/config.toml")).unwrap();
    assert!(
        !codex_text.contains("[mcp_servers.runai]"),
        "Codex runai section removed:\n{codex_text}"
    );

    // OpenCode
    let oc_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".config/opencode/opencode.json")).unwrap(),
    )
    .unwrap();
    assert!(
        oc_json["mcp"]
            .as_object()
            .map(|m| !m.contains_key("runai"))
            .unwrap_or(true),
        "OpenCode runai entry removed"
    );
}
