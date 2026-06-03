---
module: mcp::tools
file: src/mcp/tools/
role: mcp-server
---

# mcp::tools

> Sibling to `src/mcp/tools/`. One-liner: rmcp-exposed `sm_*` tool surface delegating to `SkillManager`.

## Purpose
The rmcp-exposed tool surface. 21 `sm_*` tools that MCP clients (Claude Code / Codex / Gemini / OpenCode as consumers) can call. Each tool is thin — it delegates to `SkillManager` or other core modules and serializes the result.

## Public surface (stable — external code depends on these paths)
- `crate::mcp::tools::SmServer` — the rmcp server type; `mcp::serve()` calls `SmServer::new()`.
- `crate::mcp::tools::TextResult` — the uniform `{ result: String }` tool return shape.
- `crate::mcp::tools::{ListResourcesParams, NameTargetParams, UnifiedEnableParams, NameParams, UnifiedDeleteParams, TrashQueryParams, StatusParams, CreateGroupParams, GroupMembersActionParams, MarketListParams, UnifiedMarketInstallParams, InstallGitHubParams, UsageStatsParams, RecommendStatsParams, RestoreParams}` — the 15 `#[tool]` argument structs (rmcp `JsonSchema` derived).

## Submodule map
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports only, no logic | `pub use server::SmServer;` + `pub use params::*` |
| `params.rs` | the 15 `*Params` argument structs + `TextResult` | `ListResourcesParams`, …, `TextResult` |
| `helpers.rs` | shared free fns used by tool bodies (`pub(super)`) | `collect_names`, `resolve_group`, `is_safe_shell_arg`, `parse_target`, `sync_claude_mcp`, `maybe_sync_claude` |
| `server.rs` | `SmServer` struct + `new()` + the **whole** `#[tool_router] impl SmServer` block (all 22 tools) + `#[tool_handler] impl ServerHandler` + tests | `SmServer`, every `sm_*` method, `get_info()` |

> **Why `server.rs` stays large (~1.2k lines):** rmcp's `#[tool_router]` / `#[tool]` / `#[tool_handler]` macros require every `#[tool]` method to live in ONE `impl SmServer` block so the generated `tool_router()` can enumerate them. The block cannot be scattered across files without rewriting the macro setup (per-block `router = name` + `ToolRouter::merge`/`Add`), which would be a behavior change, not a move. So the split extracts only the param structs and helper fns; the macro impl is kept intact. This is the documented exception to the ≤700-line ceiling.

## Tool families (see README "MCP Tools" table for full list)

**Skills & MCPs** (7): `sm_list`, `sm_status`, `sm_enable`, `sm_disable`, `sm_delete`, `sm_scan`, `sm_search`.

**Install** (3): `sm_install`, `sm_market`, `sm_market_install`.

**Groups** (4): `sm_groups`, `sm_create_group`, `sm_delete_group`, `sm_group_members`. `sm_groups` returns one line per group `{id} ({display-name}) — {N} members` plus a second indented line carrying the first 200 chars of `description` (with `…` if truncated). For the full description tell the user to run `runai group show <id>` — there is no MCP equivalent.

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
- Adding a new tool: add the method **inside the single `#[tool_router] impl SmServer` block in `server.rs`** with the `#[tool]` macro, put its arg struct in `params.rs` and re-export it from `mod.rs`, then update `README.md` feature list + tool count (currently 21). Do NOT move tool methods out of `server.rs` — the macro requires them all in one impl block.
- Arg names must match the rmcp schema exactly — snake_case, no Rust keyword collisions.
- Helper fns used by tool bodies live in `helpers.rs` as `pub(super)` and are glob-imported into `server.rs` via `use super::helpers::*` — keep them non-`pub` so they don't widen the public surface.

## Tests
- `server.rs::tests` (unix `HOME_LOCK`-gated via `test_support`) — `tool_router_has_expected_tools` enumerates all 22 registered tools (the R4 macro-coupling regression gate), plus `sm_status` JSON shape, `sm_backups`, `sm_groups` description preview, and `sm_search` no-results fallback.
