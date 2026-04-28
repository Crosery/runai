---
module: core::manager
file: src/core/manager.rs
role: runtime
---

# manager — business orchestration (the hub)

## Purpose
`SkillManager` is **the** orchestration layer. Every CLI command, TUI action, and MCP tool goes through it. Owns an `AppPaths` and a `Database`, coordinates `scanner`/`linker`/`installer`/`market` to execute operations. If unsure where an operation lives, start here.

## Public API (≈30 methods — pick the relevant family)

**Construction**: `new()` / `with_base(base)` / `paths()` / `db()`

**Resource lifecycle**:
- `scan()` — delegate to scanner.
- `register_local_skill(name)` — add a skill that's already under `skills/` to the DB.
- `enable_resource(id, target, group?)` / `disable_resource(...)` — for skill: create/remove symlink; for MCP: edit target's config file.
- `trash_resource(id)` / `uninstall(id)` — move a skill/MCP into global trash. `uninstall` is now a compatibility wrapper over `trash_resource`.
- `list_trash()` / `find_trash_id(query)` / `restore_from_trash(id)` / `purge_trash(id)` / `empty_trash()`.
- `list_resources(kind?, target?)` — unified listing (Skills from DB + MCPs by reading each CLI's config live via `mcp_discovery`).
- `find_resource_id(name)` / `find_group_id(query)` — fuzzy lookup.
- `record_usage(name)` / `usage_stats()` — usage tracking (DB-backed).

**Groups**:
- `create_group(id, group)` / `list_groups()` / `rename_group` / `update_group` / `get_group_members(id)` / `enable_group` / `disable_group`.

**Install**:
- `install_github_repo(owner, repo, branch, target)` — fetch, classify, register, group.
- `register_and_group_skills(...)` — called after market install.
- `batch_delete(names) -> (count, failed)` — now batch-trash, not permanent delete.

**Status**: `status(target) -> (enabled_skills, enabled_mcps)`, `resource_count()`, `is_first_launch()`.

## Key invariants
- **MCP enabled state is never in DB.** Re-read every `list_resources` / `status` call from CLI config files (`mcp_discovery::discover_all`). Caching this would go stale.
- **Skill enabled state is never in DB.** It's the filesystem (symlink exists). DB only stores metadata and group membership.
- **MCP backups in `~/.runai/mcps/<name>.json` are always canonical shape** (Claude/Gemini-style: `command:string` + `args:array`). `remove_mcp_entry_from_target` runs `mcp_canonical::to_canonical` before persisting; `write_mcp_entry_to_target` runs `from_canonical_for_json_target` / `canonical_to_codex_toml` per target on the way out. This is the only contract that lets cross-CLI disable→enable (e.g. disable from OpenCode → enable for Claude) round-trip without corrupting Claude's `mcpServers` schema. Root-cause for the 2026-04-28 fix.
- **Corrupt MCP entries (empty command) are refused at write time.** `write_mcp_entry_to_target` calls `mcp_canonical::is_corrupt` and bails. Migration on startup quarantines pre-existing corrupt backups into `~/.runai/mcps/.corrupt/`.
- **`SkillManager::new()` and `with_base()` auto-run `migrate_mcp_backups`** to normalize legacy OpenCode-shaped backups into canonical and quarantine corrupt ones. Idempotent. Logs via stderr when changes occur.
- Delete is **trash-first** across CLI / TUI / MCP. Restoring a skill rebuilds its managed directory + enabled symlinks; restoring an MCP rebuilds live config entries + disabled backup JSON.
- Trash capture removes group memberships from the active DB and stores them in the trash entry so the normal Groups view does not show ghost resources.
- `disable_rune_self` — refuses to disable runai's own MCP entry across CLIs (guard rail).

## Touch points
- **Upstream**: `cli/mod.rs`, `mcp/tools.rs`, `tui/app.rs` — every high-level feature.
- **Downstream**: `scanner`, `linker`, `installer`, `market`, `db`, `mcp_register`, `mcp_discovery`, `paths`.

## Gotchas
- `list_resources` has non-trivial dedup logic: MCPs can live in multiple CLIs, show once with combined enable-state.
- `find_resource_id(name)` must check disabled MCP backup files in `mcps/` in addition to live configs, otherwise trashed/disabled MCPs become unaddressable from CLI/MCP entrypoints.
- `with_home` test helper uses `HOME` env var; the whole `tests` module is `#[cfg(not(target_os = "windows"))]` because `dirs 6.x` on Windows uses Win32 API and ignores env vars.
- Enable/disable takes a `target: CliTarget`. Group enable/disable delegates to per-resource with the same target — it is **not** an all-targets operation.
