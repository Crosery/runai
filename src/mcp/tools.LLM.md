---
module: mcp::tools
file: src/mcp/tools.rs
role: mcp-server
---

# mcp::tools

## Purpose
The rmcp-exposed tool surface. 18 `sm_*` tools that MCP clients (Claude Code / Codex / Gemini / OpenCode as consumers) can call. Each tool is thin — it delegates to `SkillManager` or other core modules and serializes the result.

## Tool families (see README "MCP Tools" table for full list)

**Skills & MCPs** (9): `sm_list`, `sm_status`, `sm_enable`, `sm_disable`, `sm_delete`, `sm_scan`, `sm_discover`, `sm_search`, `sm_batch_{enable,disable}`.

**Install** (5): `sm_install`, `sm_market`, `sm_market_install`, `sm_batch_install`, `sm_sources`.

**Groups** (7): `sm_groups`, `sm_create_group`, `sm_delete_group`, `sm_group_add`, `sm_group_remove`, `sm_update_group`, `sm_group_{enable,disable}`.

**Usage** (2): `sm_record_usage`, `sm_usage_stats`.

**Backup/utility** (5): `sm_backup`, `sm_restore`, `sm_backups`, `sm_register`, `sm_batch_delete`.

## Key invariants
- **Tools never mutate without confirming the target exists** — `sm_enable("nonexistent", ...)` returns a structured error, never silently no-ops.
- `sm_install` / `sm_market_install` / `sm_batch_install` return **a shell command** for the host agent to run via Bash — they do not directly fork processes. This keeps MCP clean of long-running downloads.
- Every tool returns JSON with `{ ok: true, ... }` or `{ ok: false, error: ... }` so callers can branch without exception handling.
- `sm_search` is **unified** — returns installed resources and market hits in one call.

## Touch points
- **Upstream**: MCP clients via stdio JSON-RPC (rmcp `tool_router`).
- **Downstream**: `SkillManager` (almost everything), `market`, `Database`.

## Gotchas
- stdout must carry only JSON-RPC frames — `tracing::subscriber::fmt()` in `main.rs` writes to stderr for this reason. Any `println!` / `print!` in a tool path will break Codex CLI silently.
- Adding a new tool: register in `tool_router`, add schema via `#[tool]` / `#[args]` macros, update `README.md` feature list + tool count (currently 18).
- Arg names must match the rmcp schema exactly — snake_case, no Rust keyword collisions.
