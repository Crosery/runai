//! Canonical MCP entry format and per-CLI conversion.
//!
//! Canonical = the standard mcpServers JSON shape used by Claude / Gemini.
//! OpenCode and Codex deviate; this module is the only place that knows how
//! they deviate, so manager can stash a single canonical entry in
//! `~/.runai/mcps/<name>.json` and re-emit per target on enable.
//!
//! See `mcp_canonical.LLM.md` for the full schema.

use crate::core::cli_target::CliTarget;
use serde_json::{Map, Value};

/// Whether `entry` is in OpenCode-native shape (command is an array).
pub fn is_opencode_shape(entry: &Value) -> bool {
    entry.get("command").map(|v| v.is_array()).unwrap_or(false)
}

/// True when the entry is unusable: command is missing, empty string,
/// or an array whose first non-meta element is empty / missing.
/// Migration uses this to quarantine into `mcps/.corrupt/`.
pub fn is_corrupt(entry: &Value) -> bool {
    let Some(cmd) = entry.get("command") else {
        return entry
            .get("type")
            .and_then(|v| v.as_str())
            .map(|t| t != "http")
            .unwrap_or(true)
            && entry.get("url").and_then(|v| v.as_str()).is_none();
    };
    if let Some(s) = cmd.as_str() {
        return s.trim().is_empty();
    }
    if let Some(arr) = cmd.as_array() {
        let first = arr.iter().find_map(|v| v.as_str()).unwrap_or("");
        return first.trim().is_empty();
    }
    true
}

/// Normalize any incoming entry shape into canonical (Claude/Gemini-style).
///
/// - OpenCode shape: split `command:[bin, ...args]` into `command:bin` + `args:[...]`,
///   flip `enabled` → `disabled`, drop `type:"local"`.
/// - Codex/Standard shape: identity (Codex `type:"stdio"` preserved as harmless extra).
/// - Any other shape: best-effort, never panic.
pub fn to_canonical(entry: &Value) -> Value {
    let Some(obj) = entry.as_object() else {
        return entry.clone();
    };

    if !is_opencode_shape(entry) {
        // Already canonical-ish (Claude/Gemini/Codex-json). Pass through.
        return entry.clone();
    }

    // OpenCode: command is array — split out
    let mut out = Map::new();
    let arr = obj
        .get("command")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let cmd = arr
        .first()
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let args: Vec<Value> = arr.into_iter().skip(1).collect();

    out.insert("command".into(), Value::String(cmd));
    out.insert("args".into(), Value::Array(args));

    // OpenCode `enabled: bool` (default true) → `disabled: bool`
    if let Some(enabled) = obj.get("enabled").and_then(|v| v.as_bool())
        && !enabled
    {
        out.insert("disabled".into(), Value::Bool(true));
    }

    // Carry over remaining fields except OpenCode-specific ones we've consumed.
    for (k, v) in obj.iter() {
        if matches!(k.as_str(), "command" | "args" | "enabled" | "type") {
            continue;
        }
        out.insert(k.clone(), v.clone());
    }

    Value::Object(out)
}

/// Emit a canonical entry into the JSON shape that `target` expects.
/// Caller still handles serialization (JSON vs TOML); this just shapes the data.
///
/// For Codex (TOML), call `canonical_to_codex_toml` instead — it returns `toml::Value`.
pub fn from_canonical_for_json_target(entry: &Value, target: CliTarget) -> Value {
    match target {
        CliTarget::Claude | CliTarget::Gemini => entry.clone(),
        CliTarget::OpenCode => canonical_to_opencode(entry),
        CliTarget::Codex => entry.clone(), // caller should use TOML helper
    }
}

/// Convert canonical → OpenCode shape: merge command+args into array,
/// flip disabled→enabled, add `type:"local"`.
pub fn canonical_to_opencode(entry: &Value) -> Value {
    let Some(obj) = entry.as_object() else {
        return entry.clone();
    };

    let cmd = obj
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut command_arr: Vec<Value> = vec![Value::String(cmd)];
    if let Some(args) = obj.get("args").and_then(|v| v.as_array()) {
        for a in args {
            if let Some(s) = a.as_str() {
                command_arr.push(Value::String(s.to_string()));
            }
        }
    }

    let mut out = Map::new();
    out.insert("command".into(), Value::Array(command_arr));
    out.insert("type".into(), Value::String("local".into()));
    let enabled = !obj
        .get("disabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    out.insert("enabled".into(), Value::Bool(enabled));

    // Carry over env / timeout / description / headers / url / tools etc.
    for (k, v) in obj.iter() {
        if matches!(k.as_str(), "command" | "args" | "disabled") {
            continue;
        }
        out.insert(k.clone(), v.clone());
    }

    Value::Object(out)
}

/// Convert a TOML entry (as read from `~/.codex/config.toml`) into canonical JSON.
pub fn codex_toml_to_canonical(val: &toml::Value) -> Value {
    fn walk(v: &toml::Value) -> Value {
        match v {
            toml::Value::String(s) => Value::String(s.clone()),
            toml::Value::Integer(i) => serde_json::json!(i),
            toml::Value::Float(f) => serde_json::json!(f),
            toml::Value::Boolean(b) => Value::Bool(*b),
            toml::Value::Datetime(d) => Value::String(d.to_string()),
            toml::Value::Array(a) => Value::Array(a.iter().map(walk).collect()),
            toml::Value::Table(t) => {
                let mut obj = Map::new();
                for (k, v) in t {
                    obj.insert(k.clone(), walk(v));
                }
                Value::Object(obj)
            }
        }
    }
    walk(val)
}

/// Convert canonical JSON → TOML for writing to `~/.codex/config.toml`.
/// Codex uses `type = "stdio"` explicitly; we ensure it's set.
pub fn canonical_to_codex_toml(entry: &Value) -> toml::Value {
    fn walk(v: &Value) -> toml::Value {
        match v {
            Value::String(s) => toml::Value::String(s.clone()),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    toml::Value::Integer(i)
                } else if let Some(f) = n.as_f64() {
                    toml::Value::Float(f)
                } else {
                    toml::Value::String(n.to_string())
                }
            }
            Value::Bool(b) => toml::Value::Boolean(*b),
            Value::Array(a) => toml::Value::Array(a.iter().map(walk).collect()),
            Value::Object(obj) => {
                let mut t = toml::Table::new();
                for (k, v) in obj {
                    // toml crate cannot represent JSON null; skip
                    if matches!(v, Value::Null) {
                        continue;
                    }
                    t.insert(k.clone(), walk(v));
                }
                toml::Value::Table(t)
            }
            Value::Null => toml::Value::String(String::new()),
        }
    }

    let mut t = match walk(entry) {
        toml::Value::Table(t) => t,
        other => {
            let mut wrapper = toml::Table::new();
            wrapper.insert("value".into(), other);
            wrapper
        }
    };

    // Codex requires explicit `type` for stdio. Only set if missing AND no url field.
    if !t.contains_key("type") && !t.contains_key("url") {
        t.insert("type".into(), toml::Value::String("stdio".into()));
    }
    toml::Value::Table(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn standard_entry_is_canonical_identity() {
        let e = json!({ "command": "/bin/foo", "args": ["a", "b"] });
        let c = to_canonical(&e);
        assert_eq!(c, e);
    }

    #[test]
    fn opencode_entry_normalized_to_canonical() {
        let oc = json!({
            "command": ["/bin/foo", "a", "b"],
            "enabled": true,
            "type": "local"
        });
        let c = to_canonical(&oc);
        assert_eq!(c["command"], json!("/bin/foo"));
        assert_eq!(c["args"], json!(["a", "b"]));
        assert!(c.get("enabled").is_none(), "enabled stripped");
        assert!(c.get("type").is_none(), "OpenCode type stripped");
    }

    #[test]
    fn opencode_disabled_becomes_canonical_disabled() {
        let oc = json!({
            "command": ["/bin/foo"],
            "enabled": false,
            "type": "local"
        });
        let c = to_canonical(&oc);
        assert_eq!(c["disabled"], json!(true));
    }

    #[test]
    fn canonical_to_opencode_round_trip_preserves_command_and_args() {
        let canonical = json!({ "command": "/bin/foo", "args": ["a", "b"] });
        let oc = canonical_to_opencode(&canonical);
        assert_eq!(oc["command"], json!(["/bin/foo", "a", "b"]));
        assert_eq!(oc["enabled"], json!(true));
        assert_eq!(oc["type"], json!("local"));
    }

    #[test]
    fn canonical_to_opencode_handles_disabled_flag() {
        let canonical = json!({ "command": "/bin/foo", "args": [], "disabled": true });
        let oc = canonical_to_opencode(&canonical);
        assert_eq!(oc["enabled"], json!(false));
    }

    #[test]
    fn round_trip_opencode_canonical_opencode_is_stable() {
        let oc = json!({
            "command": ["/bin/foo", "x", "y"],
            "enabled": true,
            "type": "local"
        });
        let back = canonical_to_opencode(&to_canonical(&oc));
        assert_eq!(back["command"], json!(["/bin/foo", "x", "y"]));
        assert_eq!(back["enabled"], json!(true));
        assert_eq!(back["type"], json!("local"));
    }

    #[test]
    fn corrupt_detects_empty_command_string() {
        assert!(is_corrupt(&json!({ "command": "" })));
        assert!(is_corrupt(&json!({ "command": "   " })));
    }

    #[test]
    fn corrupt_detects_empty_command_array() {
        assert!(is_corrupt(&json!({ "command": [""] })));
        assert!(is_corrupt(&json!({ "command": [], "type": "local" })));
    }

    #[test]
    fn corrupt_does_not_flag_valid_stdio_entries() {
        assert!(!is_corrupt(&json!({ "command": "/bin/foo" })));
        assert!(!is_corrupt(&json!({ "command": ["/bin/foo", "args"] })));
    }

    #[test]
    fn corrupt_allows_http_entries_without_command() {
        assert!(!is_corrupt(
            &json!({ "type": "http", "url": "https://x.com" })
        ));
    }

    #[test]
    fn codex_toml_round_trip_preserves_tools_subtable() {
        let canonical = json!({
            "command": "/bin/foo",
            "args": ["x"],
            "type": "stdio",
            "tools": {
                "approval_mode": "approve",
                "nested": { "key": "val" }
            }
        });
        let tv = canonical_to_codex_toml(&canonical);
        let back = codex_toml_to_canonical(&tv);
        assert_eq!(back["command"], json!("/bin/foo"));
        assert_eq!(back["args"], json!(["x"]));
        assert_eq!(back["tools"]["approval_mode"], json!("approve"));
        assert_eq!(back["tools"]["nested"]["key"], json!("val"));
    }

    #[test]
    fn codex_toml_adds_type_stdio_when_missing() {
        let canonical = json!({ "command": "/bin/foo", "args": [] });
        let tv = canonical_to_codex_toml(&canonical);
        if let toml::Value::Table(t) = tv {
            assert_eq!(t.get("type").and_then(|v| v.as_str()), Some("stdio"));
        } else {
            panic!("expected table");
        }
    }

    #[test]
    fn codex_toml_does_not_overwrite_http_url_with_stdio_type() {
        let canonical = json!({ "url": "https://x.com", "type": "http" });
        let tv = canonical_to_codex_toml(&canonical);
        if let toml::Value::Table(t) = tv {
            assert_eq!(t.get("type").and_then(|v| v.as_str()), Some("http"));
        } else {
            panic!("expected table");
        }
    }

    #[test]
    fn from_canonical_for_target_dispatches_correctly() {
        let canonical = json!({ "command": "/bin/foo", "args": ["a"] });
        let claude = from_canonical_for_json_target(&canonical, CliTarget::Claude);
        assert_eq!(claude["command"], json!("/bin/foo"));
        let oc = from_canonical_for_json_target(&canonical, CliTarget::OpenCode);
        assert_eq!(oc["command"], json!(["/bin/foo", "a"]));
        assert_eq!(oc["type"], json!("local"));
    }
}
