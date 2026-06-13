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
