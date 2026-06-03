---
module: core::db
file: src/core/db/
role: storage
---

# db

> This folder (`src/core/db/`). One-liner: SQLite schema + migrations + the typed query layer for all managed metadata.

## Purpose
SQLite wrapper (via rusqlite bundled). Stores resource metadata, trash metadata, group membership, AI summaries / scores, router telemetry, users, and the per-user skill library. **Not** runtime enabled state — that lives on the filesystem.

As of the v0.11 decoupling, the former 2507-line `db.rs` is a directory: a thin `mod.rs` re-exports the public surface, the `Database` struct lives in `core.rs`, schema + every migration stays monolithic in `schema.rs`, and the CRUD is split into `impl Database` blocks per domain.

## Public surface (the API contract — external code depends on these exact paths)
- `crate::core::db::Database` — the connection wrapper + all CRUD methods.
- `crate::core::db::RouterEvent` — one router-telemetry row.
- `crate::core::db::User` — one account row (schema v15+).
- `crate::core::db::RouterModelStat` / `RouterStatsSummary` / `TimelineBucket` — aggregate stats value types.

### `Database` method surface (unchanged — moved, not renamed)
- `Database::open(path) -> Result<Self>` — opens or creates; runs schema migration idempotently. (`core.rs`)
- `conn_ref() -> &Connection`, `schema_version() -> i64`. (`core.rs`)
- `insert_resource` / `get_resource` / `delete_resource` / `list_resources(kind?)` / `update_description` / `record_usage` / `get_usage_stats` / `resource_count` / `skill_count` / `dedupe_skills_by_name`. (`resources.rs`; `delete_resource` in `trash.rs`)
- `list_resources_for_user(kind?, owner)` — owner-aware listing. `None` → public-pool only; `Some(uid)` → public ∪ uid's private; `Some("*")` → everything (admin). (`resources.rs`)
- `find_resource_by_name_for_user(kind, name, owner)` — single-row lookup, same owner semantics; private rows shadow public ones of the same name. (`resources.rs`)
- `insert_trash_entry` / `get_trash_entry` / `list_trash_entries` / `delete_trash_entry`. (`trash.rs`)
- `add_group_member` / `remove_group_member` / `get_group_members` / `get_group_member_ids` / `get_groups_for_resource` / `groups_for_all_resources` / `take_groups_for_resource`. (`groups.rs`)
- AI summaries / scores: `skill_llm_score[s_all]` / `set_skill_ai_summary[_scored]` / `delete_skill_scoring` / `reset_summaries` / `skill_ai_summary[_all,_timestamps,_stats]`. (`ai_summary.rs`)
- Router events + session memory: `insert_router_event` / `router_events_for_skill` / `router_recent_events` / `router_events_paged[_filtered]` / `router_events_count[_filtered]` / `router_events_since_ordered` / `router_event_by_id` / `router_session_routed_skills` / `router_session_recommended_skills` / `router_session_turn_history` / `record_session_adoption`. (`router.rs`)
- Router stats: `router_stats_summary[_filtered]` / `router_timeline[_filtered]`. (`router_stats.rs`)
- Users (v15+): `create_user` / `find_user_by_username` / `find_user_by_api_key_hash` / `find_user_by_id` / `list_users` / `set_user_admin` / `set_user_disabled` / `update_user_prefs` / `rotate_api_key`. (`users.rs`)
- Library (v15+): `library_add` / `library_remove` / `library_remove_for_all` / `cleanup_orphan_library_entries` / `library_list` / `library_contains` / `library_clear` / `library_count` / `top_public_skills`. (`library.rs`)

## Submodule map
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports only, no logic | `pub use core::Database;` `pub use types::{RouterEvent, RouterModelStat, RouterStatsSummary, TimelineBucket, User};` |
| `types.rs` | plain value structs | `RouterEvent`, `User`, `RouterModelStat`, `TimelineBucket`, `RouterStatsSummary` |
| `core.rs` | struct + connection lifecycle | `Database { conn }`, `open`, `conn_ref`, `schema_version` |
| `schema.rs` | schema + ALL migrations (monolithic) | `init_schema` (v1–v15) |
| `router.rs` | `router_events` + session memory | `row_to_router_event` (positional), insert/page/count/by-id, session adoption/recommend/turn-history |
| `router_stats.rs` | aggregates over router_events | `router_stats_summary[_filtered]`, `router_timeline[_filtered]` |
| `ai_summary.rs` | `resource_ai_summary` CRUD | summaries + `llm_score` getters/setters |
| `resources.rs` | `resources` CRUD + owner queries + usage + dedupe | `collect_resources` (positional, `pub(super)`), `list_resources_for_user`, `find_resource_by_name_for_user` |
| `groups.rs` | `group_members` associations | reuses `collect_resources` for `get_group_members` |
| `trash.rs` | `trash_entries` CRUD + `delete_resource` | JSON `TrashEntry` payloads |
| `users.rs` | `users` CRUD + auth lookups | `row_to_user` (positional) |
| `library.rs` | `user_skill_library` CRUD | `top_public_skills` |
| `tests.rs` | migration / users / library / resources fixtures | unit suite (18 tests) |

## Invariants (load-bearing — do not break silently)
- **`schema.rs` keeps `init_schema` + every v1–v15 migration MONOLITHIC.** Migrations run on every `open()` with no version lock; splitting them across files risks a half-applied schema. Do not factor per-version files.
- **Row converters read columns POSITIONALLY.** `router.rs::row_to_router_event`, `users.rs::row_to_user`, and `resources.rs::collect_resources` index by `r.get(N)`. Each lives in the SAME file as the SELECTs whose column order it depends on. Never reorder a SELECT's columns without updating its converter, and never separate a query from its converter.
- **Schema migrations are idempotent** — `Database::open` must be safe to call repeatedly on an existing DB.
- Schema version `4` adds `trash_entries`; version `15` adds `users`, `user_skill_library`, `resources.owner_user_id`, `router_events.user_id`. Owner-aware separation: `owner_user_id IS NULL` = public pool, `Some(uid)` = uid's private.
- Legacy table names (from the `skill-manager` era) are **kept alive** for rollback safety; new code writes only to the renamed tables.
- `insert_resource` round-trips `Source` via `to_meta_json` / `from_meta_json` and preserves usage columns on conflict — re-scan/update paths must not zero usage. PK `id` already encodes the owner (`u:<uid>:` prefix from `Resource::generate_id`) so same `(source, name)` across users do not collide.
- Trash payloads are stored as JSON `TrashEntry` blobs; new `TrashEntry` fields must stay serde backward-compatible. `owner_user_id` was added with `#[serde(default)]` so pre-v15 payloads still decode (owner surfaces as `None`).
- `Database::conn` is `pub(super)` — sibling submodules' `impl Database` blocks access `self.conn`; it is NOT part of the public API. Use `conn_ref()` for the (rare) crate-internal raw-SQL escape hatch.

## Touch points
- **Upstream**: `SkillManager`, `scanner` (insert on adopt), `server`, `recommend`, MCP tools (list/search).
- **Downstream**: rusqlite, `resource::{Resource, TrashEntry, UsageStat}`, `cli_target::CliTarget`.

## Gotchas
- Tests must serialize DB access with `--test-threads=1` — rusqlite bundled SQLite gets upset under parallel I/O on the same file. CI already does this.
- No connection pool — one `Database` == one connection, pass `&Database` through call stacks. Don't clone.
- `mod.rs` has a child `mod core;` while `crate::core` also exists — `use core::Database;` in `mod.rs` resolves to the local child module (it's an in-scope `mod`), not `crate::core`. Don't "fix" it to `crate::`.
- `schema_version()` is the contract between the app and the DB — bump it whenever you change the schema.

## Tests
- `tests.rs` (`#[cfg(test)] mod tests`, no platform gate) — 18 tests: migration to v15, group-member migration, resource usage/conflict round-trips, trash round-trips (incl. pre-v15 payload decode), user CRUD, library CRUD, owner-aware list/find. Physical owner-pool contract lives in the workspace integration suite `tests/multiuser_owner_e2e.rs`.
