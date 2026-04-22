---
module: core::resource
file: src/core/resource.rs
role: domain-types
---

# resource

## Purpose
Domain types for "something runai can enable/disable": `Resource`, `ResourceKind` (Skill / Mcp), `Source` (where it came from), `TrashEntry`, and `UsageStat`.

## Public API
- `enum ResourceKind { Skill, Mcp }` with `as_str()` and `FromStr`.
- `enum Source` — `Local`, `GitHub { owner, repo, branch }`, `Adopted { original_cli }`. Used by DB to round-trip provenance.
  - `to_meta_json(&self) -> String` + `from_meta_json(type, meta)` — DB serialization.
- `struct Resource { id, name, kind, description, directory, source, installed_at, enabled, usage_count, last_used_at }`.
  - `Resource::generate_id(source, name) -> String` — deterministic ID based on source kind (`local:name`, `github:owner/repo:name`, `adopted:name`).
  - `resource.is_enabled_for(target) -> bool`.
- `struct TrashEntry { id, resource_id, name, kind, directory, source, deleted_at, payload_path, enabled_targets, group_ids, ... }` — serialized into DB for global trash management and restore/purge flows.
- `struct UsageStat { id, name, count, last_used_at }`.
- `fn format_time_ago(ts: Option<i64>) -> String` — `"3h ago"` / `"just now"` etc. for CLI output.

## Key invariants
- `Resource::enabled_for` is cosmetic/runtime-derived — **persistence of enable state is filesystem** (symlink / config entry), not this field. Only fill it before presenting.
- `generate_id` must be stable across versions — identity depends on it for DB rows and group membership.
- `TrashEntry.resource_id` must keep the original ID so restore can reattach group memberships and re-enable targets without fuzzy lookup.

## Touch points
- **Upstream**: `Database` for rows, `SkillManager`, `scanner`, `mcp_discovery` for construction, MCP `sm_list` / `sm_trash` for output.
- **Downstream**: `CliTarget`.

## Gotchas
- `format_time_ago` takes `Option<i64>`; `None` → `"never"`. Don't pass `0` as "never".
- When adding a new `Source` variant: update `source_type()`, both `to_meta_json` and `from_meta_json`, DB migrations, and group-suggestion classifier.
