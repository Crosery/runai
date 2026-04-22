---
module: core::db
file: src/core/db.rs
role: storage
---

# db

## Purpose
SQLite wrapper (via rusqlite bundled). Stores resource metadata, trash metadata, group membership, and usage stats. **Not** runtime enabled state — that lives on the filesystem.

## Public API
- `Database::open(path) -> Result<Self>` — opens or creates; runs schema migration idempotently.
- `insert_resource(res)` / `get_resource(id)` / `delete_resource(id)` / `list_resources(kind?)` / `update_description(id, desc)`.
- `insert_trash_entry(entry)` / `get_trash_entry(id)` / `list_trash_entries()` / `delete_trash_entry(id)`.
- `record_usage(id) -> count` / `get_usage_stats() -> Vec<UsageStat>`.
- `add_group_member(group_id, resource_id)` / `remove_group_member` / `get_group_members(group_id) -> Vec<Resource>` / `get_group_member_ids(group_id) -> Vec<String>` / `get_groups_for_resource(id) -> Vec<String>` / `take_groups_for_resource(id) -> Vec<String>`.
- `resource_count() -> (skills, mcps)`, `skill_count()`.
- `schema_version() -> i64` — used by startup sanity check.

## Key invariants
- **Schema migrations are idempotent** — `Database::open` must be safe to call repeatedly on an existing DB without breaking it.
- Schema version `4` adds `trash_entries`; upgrades must keep older installs readable and writeable.
- Legacy table names (from the `skill-manager` era) are **kept alive** for rollback safety; new code writes only to the renamed tables.
- `insert_resource` round-trips `Source` via `to_meta_json` / `from_meta_json` and preserves usage columns on conflict — re-scan/update paths must not zero usage.
- Trash payloads are stored as JSON `TrashEntry` blobs; adding fields to `TrashEntry` means keeping serde backward-compatible.

## Touch points
- **Upstream**: `SkillManager`, `scanner` (insert on adopt), MCP tools (list/search).
- **Downstream**: rusqlite, `resource::{Resource, TrashEntry, UsageStat}`.

## Gotchas
- Tests must serialize DB access with `--test-threads=1` — rusqlite bundled SQLite gets upset under parallel I/O on the same file. CI already does this.
- No connection pool — one `Database` == one connection, pass `&Database` through call stacks. Don't clone.
- `schema_version()` is the contract between the app and the DB — bump it whenever you change the schema.
