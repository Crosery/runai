---
module: mcp::tools
file: src/mcp/tools.rs
role: mcp-server
---

# mcp::tools

## Purpose
The rmcp-exposed tool surface. 21 `sm_*` tools that MCP clients (Claude Code / Codex / Gemini / OpenCode as consumers) can call. Each tool is thin — it delegates to `SkillManager` or other core modules and serializes the result.

## Tool families (see README "MCP Tools" table for full list)

**Skills & MCPs** (7): `sm_list`, `sm_status`, `sm_enable`, `sm_disable`, `sm_delete`, `sm_scan`, `sm_search`.

**Install** (3): `sm_install`, `sm_market`, `sm_market_install`.

**Groups** (4): `sm_groups`, `sm_create_group`, `sm_delete_group`, `sm_group_members`.

**Trash** (3): `sm_trash`, `sm_trash_restore`, `sm_trash_purge`.

**Usage** (1): `sm_usage_stats`.

**Backup/utility** (3): `sm_backup`, `sm_restore`, `sm_backups`.

## Key invariants
- **Tools never mutate without confirming the target exists** — `sm_enable("nonexistent", ...)` returns a structured error, never silently no-ops.
- `sm_install` / `sm_market_install` return **a shell command** for the host agent to run via Bash — they do not directly fork processes. This keeps MCP clean of long-running downloads.
- `sm_delete` is trash-first. Permanent deletion is only exposed through `sm_trash_purge`.
- Every tool currently returns `TextResult { result: String }`; callers need to parse the string or the embedded JSON string for structured responses like `sm_status`.
- `sm_search` is **unified** — returns installed resources and market hits in one call.

## Touch points
- **Upstream**: MCP clients via stdio JSON-RPC (rmcp `tool_router`).
- **Downstream**: `SkillManager` (almost everything), `market`, `Database`.

## Gotchas
- stdout must carry only JSON-RPC frames — `tracing::subscriber::fmt()` in `main.rs` writes to stderr for this reason. Any `println!` / `print!` in a tool path will break Codex CLI silently.
- Adding a new tool: register in `tool_router`, add schema via `#[tool]` / `#[args]` macros, update `README.md` feature list + tool count (currently 21).
- Arg names must match the rmcp schema exactly — snake_case, no Rust keyword collisions.
