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
- `crate::core::db::RouterIntentMemoryItem` — one current-session short-memory row for the recommend router.
- `crate::core::db::User` — one account row (schema v15+).
- `crate::core::db::CommunitySkill` / `CommunitySort` — community-market row + sort enum (schema v16+).
- `crate::core::db::RouterModelStat` / `RouterStatsSummary` / `TimelineBucket` — aggregate stats value types.

### `Database` method surface (unchanged — moved, not renamed)
- `Database::open(path) -> Result<Self>` — opens or creates; runs schema migration idempotently. (`core.rs`)
- `conn_ref() -> &Connection`, `schema_version() -> i64`. (`core.rs`)
- `insert_resource` / `get_resource` / `delete_resource` / `list_resources(kind?)` / `update_description` / `record_usage` / `get_usage_stats` / `resource_count` / `skill_count` / `dedupe_skills_by_name`. (`resources.rs`; `delete_resource` in `trash.rs`)
- Activation/feedback idempotency: `record_usage_event` (generic event row) and `record_activation_usage_event` (event row + usage_count + session adoption in one SQLite transaction). (`usage_events.rs`)
- `list_resources_for_user(kind?, owner)` — owner-aware listing. `None` → public-pool only; `Some(uid)` → public ∪ uid's private; `Some("*")` → everything (admin). (`resources.rs`)
- `find_resource_by_name_for_user(kind, name, owner)` — single-row lookup, same owner semantics; private rows shadow public ones of the same name. (`resources.rs`)
- `insert_trash_entry` / `get_trash_entry` / `list_trash_entries` / `delete_trash_entry`. (`trash.rs`)
- `add_group_member` / `remove_group_member` / `get_group_members` / `get_group_member_ids` / `get_groups_for_resource` / `groups_for_all_resources` / `take_groups_for_resource`. (`groups.rs`)
- AI summaries / scores: `skill_ai_index[_all]` / `skill_ai_index_*_resource*` / `skill_llm_score[s_all]` / `set_skill_ai_index[_scoped]` / `set_skill_ai_summary[_scored]` / `delete_skill_scoring[_for_resource]` / `reset_summaries` / `skill_ai_summary[_all,_timestamps,_stats]`. (`ai_summary.rs`)
- Router events + session memory: `insert_router_event` / `router_events_for_skill` / `router_recent_events` / `router_events_paged[_filtered]` / `router_events_count[_filtered]` / `router_events_since_ordered` / `router_event_by_id` / `router_session_routed_skills` / `router_session_recommended_skills` / `router_session_turn_history` / `append_router_intent_memory` / `router_intent_memory` / `record_session_adoption`. (`router.rs`)
- Router stats: `router_stats_summary[_filtered]` / `router_timeline[_filtered]`. (`router_stats.rs`)
- Users (v15+): `create_user` / `find_user_by_username` / `find_user_by_api_key_hash` / `find_user_by_session_key_hash` (v22, cookie auth lane only — a session token must never authenticate as a Bearer) / `find_user_by_id` / `list_users` / `set_user_admin` / `set_user_disabled` / `update_user_prefs` / `rotate_api_key` / `set_session_key_hash` (v22: Some = new browser session, None = revoke; single slot per user) / `set_user_credentials` (atomic `password_hash` + `api_key_hash` rewrite + `session_key_hash = NULL` — backs admin password reset) / `delete_user` (auth row only — owned resources/dirs go through `SkillManager::delete_user_cascade`) / `anonymize_router_events_for_user` (null out `router_events.user_id`). (`users.rs`)
- Library (v15+): `library_add` / `library_remove` / `library_remove_for_all` / `cleanup_orphan_library_entries` / `library_list` / `library_contains` / `library_clear` / `library_count` / `top_public_skills`. (`library.rs`)
- Community (v16+): `insert_community_skill` / `upsert_community_skill` / `get_community_skill` / `list_community_skills` / `community_skills_by_uploader` (cascade reap) / `count_community_skills` / `increment_community_installs` / `delete_community_skill`. (`community.rs`)

## Submodule map
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports only, no logic | `pub use core::Database;` `pub use types::{RouterEvent, RouterIntentMemoryItem, RouterModelStat, RouterStatsSummary, TimelineBucket, User};` |
| `types.rs` | plain value structs | `RouterEvent`, `RouterIntentMemoryItem`, `User`, `RouterModelStat`, `TimelineBucket`, `RouterStatsSummary` |
| `core.rs` | struct + connection lifecycle | `Database { conn }`, `open`, `conn_ref`, `schema_version`; sets a short SQLite busy timeout for concurrent request writes |
| `schema.rs` | schema + ALL migrations (monolithic) | `init_schema` (v1–v25) |
| `router.rs` | `router_events` + session memory | `row_to_router_event` (positional), insert/page/count/by-id, session adoption/recommend/turn-history, scoped intent-memory append/read/trim |
| `router_stats.rs` | aggregates over router_events | `router_stats_summary[_filtered]`, `router_timeline[_filtered]` |
| `ai_summary.rs` | `resource_ai_summary` CRUD | structured AI index + `llm_score` getters/setters |
| `resources.rs` | `resources` CRUD + owner queries + usage + dedupe | `collect_resources` (positional, `pub(super)`), `list_resources_for_user`, `find_resource_by_name_for_user` |
| `usage_events.rs` | activation/feedback idempotency | `UsageOutcome`, `record_usage_event`, `record_activation_usage_event`, `usage_event_count` |
| `groups.rs` | `group_members` associations | reuses `collect_resources` for `get_group_members` |
| `trash.rs` | `trash_entries` CRUD + `delete_resource` | JSON `TrashEntry` payloads |
| `users.rs` | `users` CRUD + auth lookups | `row_to_user` (positional) |
| `library.rs` | `user_skill_library` CRUD | `top_public_skills` |
| `community.rs` | `community_skills` CRUD (v16+) | `row_to_community_skill` (positional), `CommunitySort` query enum |
| `tests.rs` | migration / users / library / resources / router fixtures | unit suite (45 tests, all run) |

## Invariants (load-bearing — do not break silently)
- **`schema.rs` keeps `init_schema` + every migration MONOLITHIC.** Migrations run on every `open()` with no version lock; splitting them across files risks a half-applied schema. Do not factor per-migration files.
- **Row converters read columns POSITIONALLY.** `router.rs::row_to_router_event`, `users.rs::row_to_user`, and `resources.rs::collect_resources` index by `r.get(N)`. Each lives in the SAME file as the SELECTs whose column order it depends on. Never reorder a SELECT's columns without updating its converter, and never separate a query from its converter.
- **Schema migrations are idempotent** — `Database::open` must be safe to call repeatedly on an existing DB. `Database::open` also sets a short busy timeout; do not remove it unless every concurrent request write path has its own retry.
- Schema migration notes: `trash_entries`; `users`, `user_skill_library`, `resources.owner_user_id`, `router_events.user_id`; `community_skills` (PK `(uploader_uid, name)`) for the team-mode community market; structured `resource_ai_summary` with PK `(owner_user_id, name)` plus `search_doc`, `router_card`, `source_hash`, `prompt_hash`, and `format_key`; `users.session_key_hash` (issue #35) for browser sessions independent from hook api_keys; `usage_events` for activation/feedback idempotency; `router_intent_memory` (v24) for bounded recommend short memory scoped by `(session_id, user_id, client_kind)`; and router_events v25 first-wave fields (`intent_llm_input`, `intent_llm_output`, `intent_status`, `intent_error_msg`, `bm25_candidates_json`) for two-stage recommend observability. Owner-aware separation: `owner_user_id IS NULL` = public pool, `Some(uid)` = uid's private. Empty AI-summary `owner_user_id` means the public-pool summary; private rows use the uid so same-named public/private skills never share an index row.
- Legacy table names (from the `skill-manager` era) are **kept alive** for rollback safety; new code writes only to the renamed tables.
- `insert_resource` round-trips `Source` via `to_meta_json` / `from_meta_json` and preserves usage columns on conflict — re-scan/update paths must not zero usage. PK `id` already encodes the owner (`u:<uid>:` prefix from `Resource::generate_id`) so same `(source, name)` across users do not collide.
- **`dedupe_skills_by_name` groups by `(name, owner_user_id)`, never `name` alone.** Name-only grouping would collapse a public skill and a different user's same-named private skill (or two users' privates) into one row and delete the loser's directory reference — cross-owner data loss against the owner-pool invariant. Uses SQLite's null-safe `owner_user_id IS ?` so a bound NULL matches public rows and a bound uid matches that user exactly. Public same-name rows still collapse together (the legitimate local-install + later-adopt case).
- **`library_remove_for_all` / `cleanup_orphan_library_entries` are public-pool-aware (C4, scan_findings.md).** `user_skill_library` only ever tracks public-pool subscriptions (private skills the owner installs are never rows here). Because the owner-pool design lets a private skill share a name with an unrelated public one, both sweeps must scope to `owner_user_id IS NULL`: `library_remove_for_all(name)` deletes only when NO public row of that name remains (`AND NOT EXISTS (... owner_user_id IS NULL)`), so trashing a PRIVATE skill that shares a name with a public one does NOT wipe every other user's subscription to the public skill; `cleanup_orphan_library_entries` counts a name as "still exists" only via a public row, so a subscriber's public `foo` being trashed while a different user's private `foo` survives still sweeps the now-orphan row. Callers (`trash_resource`, `delete_user_cascade`, `doctor --fix`) `delete_resource` first, so for a genuine public trash the guard passes and subscribers are swept. Do NOT drop the `owner_user_id IS NULL` filter back to name-only. Pinned by `library_remove_for_all_spares_public_when_private_same_name_gone` + `cleanup_orphan_library_entries_is_public_pool_aware` in `tests.rs`.
- Trash payloads are stored as JSON `TrashEntry` blobs; new `TrashEntry` fields must stay serde backward-compatible. `owner_user_id` was added with `#[serde(default)]` so pre-v15 payloads still decode (owner surfaces as `None`).
- **Activation ACK is atomic.** `record_activation_usage_event` is the only correct way to ACK `/skills/use/{name}`. It wraps `usage_events` insert/dedupe, `resources.usage_count` bump, and `router_session_adoptions` write in one `BEGIN IMMEDIATE` transaction. A server ACK must never leave behind only the idempotency row without the usage side effect.
- **Recommend intent memory is bounded by the caller's limit and stores first-wave intent artifacts.** `append_router_intent_memory(session_id, user_id, client_kind, memory, limit)` stores at most one capped short-memory row per call, then trims that exact `(session_id, user_id, client_kind)` scope by dropping oldest rows beyond `limit`. The caller must pass the compact Stage-1 `intent_llm_output`, not raw user prompt text. `limit == 0`, empty `session_id`, or empty memory is a no-op. Do not merge scopes: Pi/Codex/Claude/OpenCode memory must not cross-contaminate even if they share a native session string.
- `Database::conn` is `pub(super)` — sibling submodules' `impl Database` blocks access `self.conn`; it is NOT part of the public API. Use `conn_ref()` for the (rare) crate-internal raw-SQL escape hatch.
- **Fixed regression (github.com/Crosery/runai/issues/33)**: `router.rs::router_event_by_id` and `router.rs::router_events_since_ordered` used to omit the trailing `user_id` column from their SELECTs while `row_to_router_event` still read it positionally — both silently returned `user_id: None` regardless of the stored value. Same failure class as the historical "`router_event_by_id` 漏 user_id 列" incident, recurring in the same function. Both SELECTs now include `user_id`; pinned by `router_event_by_id_preserves_user_id` and `router_events_since_ordered_preserves_user_id` in `tests.rs`. If either query is touched again, keep its column list in sync with `row_to_router_event`'s positional reads (see the INVARIANT comment at the top of `router.rs`).

## Touch points
- **Upstream**: `SkillManager`, `scanner` (insert on adopt), `server`, `recommend`, MCP tools (list/search).
- **Downstream**: rusqlite, `resource::{Resource, TrashEntry, UsageStat}`, `cli_target::CliTarget`.

## Gotchas
- Tests must serialize DB access with `--test-threads=1` — rusqlite bundled SQLite gets upset under parallel I/O on the same file. CI already does this.
- No connection pool — one `Database` == one connection, pass `&Database` through call stacks. Don't clone.
- `mod.rs` has a child `mod core;` while `crate::core` also exists — `use core::Database;` in `mod.rs` resolves to the local child module (it's an in-scope `mod`), not `crate::core`. Don't "fix" it to `crate::`.
- `schema_version()` is the contract between the app and the DB — bump it whenever you change the schema.

## Tests
- `tests.rs` (`#[cfg(test)] mod tests`, no platform gate) — migration to v25, group-member migration, resource usage/conflict round-trips, trash round-trips (incl. pre-v15 payload decode), user CRUD, library CRUD, owner-aware list/find, owner-aware dedupe (no cross-owner merge + same-owner collapse), and the full `router.rs` + `router_stats.rs` surface (issue #27) — insert/roundtrip incl. `user_id`, two-stage intent fields, and field-length caps, `router_event_by_id` hit/miss, exact-name `json_each` matching, session adoption/routed/recommended/turn-history memory, bounded intent-memory append/read/drop-oldest/scoping, paged/count/user-scoped queries, `router_events_since_ordered` grouping, and `router_stats_summary`/`router_timeline` aggregation incl. per-user scoping. `router_event_by_id_preserves_user_id` and `router_events_since_ordered_preserves_user_id` pin the fix for the `user_id`-dropping regression tracked in issue #33. Physical owner-pool contract lives in the workspace integration suite `tests/multiuser_owner_e2e.rs`.
