---
module: core::cli_target
file: src/core/cli_target.rs
role: domain-types
---

# cli_target

## Purpose
Enumerate the four supported CLIs and resolve each one's per-CLI paths + config format peculiarities.

## Public API
- `enum CliTarget { Claude, Codex, Gemini, OpenCode }`, `CliTarget::ALL` constant.
- `target.name() -> &'static str` — `"claude"` / `"codex"` / `"gemini"` / `"opencode"`.
- `target.skills_dir() -> PathBuf` — where the CLI looks for skills (`~/.claude/skills`, etc.).
- `target.agents_skills_dir() -> PathBuf` — legacy `~/.claude-code/...` path for migration.
- `target.settings_path() -> PathBuf` — user-scoped settings file the CLI reads.
- `target.mcp_config_path() -> PathBuf` — where MCP entries live for this CLI.
- `s.parse::<CliTarget>() -> Result<CliTarget, ()>` — accepts `claude`, `codex`, `gemini`, `opencode`.
- `target.uses_toml() -> bool` — Codex only.
- `target.uses_opencode_format() -> bool` — OpenCode's `command: [..]` array form.

## Key invariants
- All path resolvers call `dirs::home_dir()` internally — **Windows Win32 API** returns the real user home; env-var mocking does not work. For tests that need a sandbox, call `with_base` on `AppPaths` or pass `home: &Path` explicitly (see `mcp_register`).
- Config paths verified against the 4 CLIs' source on Windows (all use `%USERPROFILE%\.xxx`, including OpenCode's XDG-style `.config/opencode/`).

## Touch points
- **Upstream**: Everywhere — `scanner`, `manager`, `mcp_register`, `mcp_discovery`, TUI (tab header uses `name()`).
- **Downstream**: `dirs::home_dir`.

## Gotchas
- The four `ALL` entries are ordered Claude → Codex → Gemini → OpenCode — TUI tab numbering `1/2/3/4` and serialization depend on this order, don't reshuffle.
- Adding a 5th target: plumb it through `ALL`, `name()/FromStr`, all four path resolvers, `uses_toml`, `uses_opencode_format`, and every file in `mcp_register.rs` (each CLI has its own format-specific writer).
