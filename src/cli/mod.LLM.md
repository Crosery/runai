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
- `enum Commands` — all subcommands: `Scan`, `Discover`, `List`, `Enable`, `Disable`, `Install`, `MarketInstall`, `Uninstall`, `Trash(TrashCommands)`, `Restore`, `Backup`, `Group(GroupCommands)`, `Status`, `McpServe`, `Register`, `Unregister`, `Usage`, `Update`, `Doctor`.
- `enum GroupCommands` — `Create`, `Add`, `Remove`, `List`.
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
