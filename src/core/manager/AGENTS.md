---
module: core::manager
file: src/core/manager/
role: runtime
---

# manager — business orchestration (the hub)

## Purpose
`SkillManager` is **the** orchestration layer. Every CLI command, TUI action, and MCP tool goes through it. Owns an `AppPaths` and a `Database`, coordinates `scanner`/`linker`/`installer`/`market` to execute operations. If unsure where an operation lives, start here.

## Public API (≈30 methods — pick the relevant family)

**Construction**: `new()` / `with_base(base)` / `paths()` / `db()`

**Resource lifecycle**:
- `scan()` — delegate to scanner.
- `register_local_skill(name)` — add a public-pool skill that's already under `skills/` to the DB. Wrapper over `register_local_skill_for(name, None)`.
- `register_local_skill_for(name, owner_user_id)` — Phase C: owner-aware adopt. `Some(uid)` adopts `<data>/users/<uid>/skills/<name>/` into uid's private pool; `None` is the public-pool path. Returns `Err` when the candidate dir doesn't exist or the uid fails `paths::is_safe_user_id`.
- `enable_resource(id, target, group?)` / `disable_resource(...)` — for skill: create/remove symlink; for MCP: edit target's config file. Private skills do NOT get symlinked (remote clients pull them via `/skills/get` + `/skills/file`, not via the local Claude Code symlink farm).
- `delete_user_cascade(uid) -> UserCascadeReport` — **destructive, high-risk.** Cascade-delete everything a user owns then the user row: trashes each owned private skill to the PUBLIC (admin-recoverable) trash, de-owned, with restore target = public pool (the owner is gone); reaps `community_skills` rows + `<data>/community/<uid>/`; `remove_dir_all`s `<data>/users/<uid>/`; anonymizes `router_events.user_id`; clears the library; deletes the user. FS-before-DB so a crash leaves recoverable state. Every `remove_dir_all` goes through `guarded_remove_dir_all` (canonicalize + `starts_with` containment) and the uid is validated by `AppPaths::user_root` first. Wired into `server::api_admin_users_delete`. Covered by `tests/user_delete_cascade_e2e.rs`. Do NOT shortcut back to a bare `DELETE FROM users` — that orphaned the live data this fixed.
- `trash_resource(id)` / `uninstall(id)` — move a skill/MCP into trash. `uninstall` is now a compatibility wrapper over `trash_resource`. Trash payload is owner-aware: public rows land in `<data>/trash/`, private rows land in `<data>/users/<uid>/trash/`. Restoration uses `entry.directory` (saved at trash time) so private skills come back to their per-user dir, never spilling into the public pool.
- `list_trash()` / `find_trash_id(query)` / `restore_from_trash(id)` / `purge_trash(id)` / `empty_trash()`.
- `list_resources(kind?, target?)` — unified listing (Skills from DB + MCPs by reading each CLI's config live via `mcp_discovery`). NOT owner-filtered — use `db.list_resources_for_user` directly for per-user views (the server's `/api/skills` handler does this).
- `find_resource_id(name)` / `find_group_id(query)` — fuzzy lookup. **Public-pool only, and enforced (C5):** the name-fallback goes through `db.list_resources_for_user(None, None)` (`owner_user_id IS NULL`), NEVER `db.list_resources(None, None)` (all owners) — otherwise a name that only exists as another user's private skill would resolve to that private id and feed destructive local ops (`uninstall` / `sm_delete` / `batch_delete` / group member resolution), letting an operator trash another user's private skill via a name collision. The server's owner-aware lookup goes through `db.find_resource_by_name_for_user`. Pinned by `tests/multiuser_owner_e2e.rs::find_resource_id_is_public_pool_only`.
- `record_usage(name)` / `usage_stats()` — usage tracking (DB-backed).

**Groups**:
- `create_group(id, group)` / `list_groups()` / `rename_group` / `update_group` / `get_group_members(id)` / `enable_group` / `disable_group`.

**Install**:
- `install_github_repo(owner, repo, branch, target)` — public-pool wrapper.
- `install_github_repo_filtered(... only?)` — public-pool, with skill-name filter (for the dashboard's "parse → pick → install" flow).
- `install_github_repo_filtered_for(... only?, owner_user_id)` — Phase C: owner-aware install. `Some(uid)` downloads into `<data>/users/<uid>/skills/`, stamps `owner_user_id` on every row, skips symlink registration and group auto-creation. The server's `/api/install/github` and `/api/market/install` call this with the authenticated user.
- `register_and_group_skills(...)` — called after market install (public-pool only).
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
