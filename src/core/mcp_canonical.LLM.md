# mcp_canonical

## What this module is for

Single source of truth for **MCP entry shape conversion** across the four CLI configs. `manager.rs` only writes/reads canonical-shaped JSON to `~/.runai/mcps/<name>.json`; this module is the only place that knows how each CLI's native format deviates and how to translate.

Created in response to the 2026-04-28 incident where an OpenCode-format entry (`command: ["bin", "args..."]` + `enabled: true` + `type: "local"`) was written verbatim into `~/.claude.json`, breaking Claude Code's MCP config parser. Root cause: `manager::remove_mcp_entry_from_target` did not normalize on backup-write, and `manager::write_mcp_entry_to_target` had no reverse converter for non-OpenCode targets.

## The canonical shape

The Claude / Gemini standard:

```json
{
  "command": "/abs/path/to/binary",
  "args": ["arg1", "arg2"],
  "type": "stdio" | "http" (optional, default stdio),
  "url": "https://..." (only when type=http),
  "env": { "KEY": "VALUE" } (optional),
  "headers": { ... } (only for http, optional),
  "timeout": 60000 (optional, ms),
  "description": "..." (optional),
  "disabled": true (optional, omitted when enabled),
  "tools": { "<tool-name>": { ... } } (Codex-only, optional)
}
```

Everything in `~/.runai/mcps/*.json` MUST conform to this. Migration on startup (`SkillManager::migrate_mcp_backups`) rewrites legacy OpenCode-shaped backups in place.

## Public API

| Function | Purpose |
|---|---|
| `is_opencode_shape(entry)` | Detect OpenCode native shape (command is array). |
| `is_corrupt(entry)` | Detect unusable entry (empty command). Used by migration to quarantine. |
| `to_canonical(entry)` | Normalize any-shape JSON → canonical. OpenCode shape gets split + flag-flipped; standard passes through. |
| `from_canonical_for_json_target(entry, target)` | Emit canonical → target's JSON shape. Identity for Claude/Gemini, calls `canonical_to_opencode` for OpenCode. Caller handles Codex (TOML) separately. |
| `canonical_to_opencode(entry)` | Merge command+args into single array, flip `disabled` → `enabled`, add `type:"local"`. |
| `codex_toml_to_canonical(toml::Value)` | Recursive TOML→JSON. Preserves `tools.*` and `env.*` subtables. |
| `canonical_to_codex_toml(entry)` | Recursive JSON→TOML. Adds `type:"stdio"` if missing AND no `url`. |

## Per-CLI shape map

| CLI | Container key | Stdio command | Args | Disabled | Identity field |
|---|---|---|---|---|---|
| Claude | `mcpServers` | `command: string` | `args: array` | `disabled: bool` | (canonical) |
| Gemini | `mcpServers` | `command: string` | `args: array` | `disabled: bool` | (canonical) |
| Codex | `mcp_servers` (TOML) | `command: string` | `args: array` | (key removed) | `type: "stdio"` |
| OpenCode | `mcp` | `command: array[0]` | `command: array[1..]` | `enabled: bool` (inverted) | `type: "local"` |

## Invariants

- `to_canonical(canonical)` is identity. Round-trip safe.
- `to_canonical` and `canonical_to_opencode` never panic — best-effort on malformed input.
- `is_corrupt` is the gate at the write layer (`manager::write_mcp_entry_to_target` refuses corrupt entries) AND the gate at migration startup.
- Codex `tools.*` and `env.*` subtables are preserved across disable/enable round-trip — verified by `e2e_codex_disable_enable_preserves_tools_and_env_subtables`.
- HTTP-type entries (no `command`, has `url`) are NOT corrupt and pass through unchanged.

## What lives in `manager.rs` vs here

`manager.rs` owns the **filesystem** side: read/write CLI configs, read/write `~/.runai/mcps/<name>.json` backups, dispatch through `CliTarget::uses_toml()` and `uses_opencode_format()`. This module owns only **value transformation** — pure functions over `serde_json::Value` / `toml::Value`. No I/O.

If you find yourself opening a file from this module, you're in the wrong place.

## Tests

14 unit tests in `mcp_canonical::tests` cover all conversion directions + corrupt detection + Codex tools subtable round-trip. 5 physical e2e in `tests/mcp_canonical_e2e.rs` drive the real binary against an isolated HOME to pin the binary-level wiring.
