---
module: cli
file: src/cli/mod.rs
role: entry
---

# cli::mod — subcommand dispatcher

## Purpose
clap-based CLI entry point. Parses subcommands, constructs a `SkillManager`, dispatches. When no subcommand given, hands off to `tui::run_tui(mgr)`.

## Public API
- `struct Cli` (clap `Parser`) — top-level arg parser.
- `enum Commands` — all subcommands: `Scan`, `Discover`, `List`, `Enable`, `Disable`, `Install`, `MarketInstall`, `Uninstall`, `Trash(TrashCommands)`, `Restore`, `Backup`, `Backups`, `Search`, `Market`, `Group(GroupCommands)`, `Status`, `McpServe`, `Register`, `Unregister`, `Usage`, `Update`, `Doctor`, `Recommend(RecommendCommands)`.
- `enum GroupCommands` — `Create`, `Add`, `Remove`, `List`, `Delete`, `Update`, `Show { id }`. `List` prints one line per group plus a 120-char description preview (indented). `Show` dumps the full description (preserving newlines) + member list with per-member kind badge and 70-char description snippet; errors with `group not found: <id>` when missing.
- `enum TrashCommands` — `List`, `Restore`, `Purge`, `Empty`.
- `run(cli) -> Result<()>` — top dispatch.

## Key invariants
- Manager construction honors `RUNE_DATA_DIR` → `SKILL_MANAGER_DATA_DIR` → default, in that order.
- `Enable` / `Disable` first check if the name matches a group (via `list_groups` contains), otherwise treat as resource — group-name wins over resource-name with same id.
- `Install` supports `owner/repo`, `owner/repo@branch`, and bare GitHub URLs (strips prefix + trailing `/`).
- `Uninstall` is trash-first: it delegates to `SkillManager::uninstall`, which now moves the resource into global trash instead of purging it permanently.
- `TrashCommands::{Restore,Purge}` resolve either an exact trash entry ID or a resource name through `SkillManager::find_trash_id`.
- `McpServe` runs a Tokio runtime inline and blocks on `mcp::serve()`; it is the **only** subcommand that takes over the process for stdio I/O.

## Touch points
- **Upstream**: `main.rs` parses + invokes `run(cli)`.
- **Downstream**: `SkillManager` (most commands), `tui::run_tui` (no-subcommand path), `mcp::serve` (`McpServe`), `backup::{create_backup, restore_backup, list_backups}`, `updater::perform_update`, `doctor::run_doctor`, `mcp_register::{register_all, unregister_all}`.

## Gotchas
- When adding a new subcommand: update `Commands` enum, add match arm in `run`, document in `AGENTS.md` if user-facing.
- `find_resource_id_by_name` returns `"resource not found"` error — match the exact message if adding tests.
- The `--target` arg defaults to `claude`. Explicit target required for non-Claude CLIs.
- `Doctor { fix: bool }` — when `--fix`, calls `core::doctor::run_doctor_fix()`: prunes dangling symlinks under `~/.{claude,codex,gemini,opencode}/skills/` and reruns the skill-row dedupe. The same dedupe runs silently inside `SkillManager::new()/with_base()` so most of the time `--fix` reports zero — it's the explicit recovery surface for users whose state drifted mid-session.
- `Search`, `Market`, `Backups`, `GroupCommands::{Delete, Update}` mirror the MCP `sm_search` / `sm_market` / `sm_backups` / `sm_delete_group` / `sm_group_members(action="update")` tools so the CLI surface is functionally on par with MCP. `GroupCommands::Delete` removes only the `.toml` (members untouched, matching MCP semantics).
