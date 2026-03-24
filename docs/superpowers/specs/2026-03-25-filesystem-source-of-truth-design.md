# Design: Filesystem as Source of Truth

Date: 2026-03-25

## Problem

skill-manager has a split-brain problem: the DB stores resource metadata and enabled state, but the real state lives in the filesystem (symlinks for skills, CLI config files for MCPs). This causes:

1. **MCP management broken in TUI** — MCPs must be in DB to toggle, but only appear after manual scan. New MCPs added outside SM are invisible.
2. **Skill state out of sync** — DB says enabled, but symlink may be missing (or vice versa). External changes require manual scan.
3. **Market confuses AI assistants** — returns empty `[]` for incompatible formats with no explanation. AI spends tokens retrying instead of falling back to `/plugin install`.
4. **Incomplete skill downloads** — CLAUDE.md says "downloads only SKILL.md" but code downloads full directory; inconsistency and potential edge cases in recursive download.

## Design Principles

- **Filesystem is the single source of truth** for all runtime state.
- **DB stores only relationships and metadata** (groups, install provenance) — never authoritative state.
- **Non-destructive** — SM creates symlinks and sets `disabled` fields. User can uninstall SM and everything reverts.
- **Consistent across all CLIs** — same enable/disable logic for claude, codex, gemini, opencode.
- **No manual scan required** — state is read from filesystem on every query.

## Architecture Changes

### 1. MCP: Remove from DB, Read Directly from Config Files

**Current flow:**
```
scan → discover MCPs → write to DB resources table
list → read DB → overlay config file status → return
enable → find in DB → write config file
```

**New flow:**
```
list → read CLI config files directly → build Resource objects in memory → return
enable → write CLI config file disabled field → done
```

Changes:
- `list_resources(kind=Mcp)` reads `~/.claude.json`, `~/.gemini/settings.json`, etc. directly. Each `mcpServers` entry becomes a `Resource` with `id = "mcp:{name}"`.
- `resources` table no longer stores MCP entries. Existing MCP rows are cleaned up on migration.
- `status()` counts MCPs from config files (already does this). `resource_count()` returns MCP total from config files too.
- `is_first_launch()` checks skill count from DB + MCP count from config files (not DB).

**Write paths for MCP (enable/disable/find/group-add):**

- `enable_resource("mcp:{name}", target)`: detect `mcp:` prefix → extract name → call `set_mcp_disabled(name, target, false)`. No DB lookup.
- `disable_resource("mcp:{name}", target)`: detect `mcp:` prefix → extract name → call `set_mcp_disabled(name, target, true)`. No DB lookup.
- `find_resource_id(name)`: after checking DB prefixes, also check if `name` exists in `read_mcp_status_from_configs()` → return `"mcp:{name}"`.
- `sm_group_add(group, name)`: `find_resource_id` now finds MCPs too → inserts `"mcp:{name}"` into `group_members`.

### 2. Skill State: Read from Symlinks, Not DB

**Current flow:**
```
enable → create symlink + write DB resource_targets
list → read DB resource_targets for enabled state
```

**New flow:**
```
enable → create symlink (no DB write for enabled state)
list → read DB for metadata → check symlink existence for each CLI target → return
```

Changes:
- `Resource.enabled` is populated by checking `{cli_skills_dir}/{name}` symlink existence for each CLI target, not from `resource_targets` table.
- `enable_resource` creates symlink only. `disable_resource` removes symlink only. No `db.set_target_enabled()` calls anywhere (remove from `scanner.rs` too).
- `resource_targets` table is deleted from schema.
- `enabled_skill_count(target)` counts symlinks in `target.skills_dir()` that point into `~/.skill-manager/skills/`.

### 3. DB Schema Simplification

```sql
-- Keep: skill metadata and provenance
CREATE TABLE resources (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind = 'skill'),
    description TEXT,
    directory TEXT NOT NULL,
    source_type TEXT NOT NULL,
    source_meta TEXT,
    installed_at INTEGER NOT NULL
);

-- Keep: group membership (remove foreign key constraint)
CREATE TABLE group_members (
    group_id TEXT NOT NULL,
    resource_id TEXT NOT NULL,  -- "local:xxx", "adopted:xxx", "mcp:xxx"
    PRIMARY KEY (group_id, resource_id)
    -- No FOREIGN KEY: MCP members don't exist in resources table
);

-- Delete: resource_targets (enabled state from filesystem)
```

Migration: on startup, if `resource_targets` table exists, drop it. Delete rows from `resources` where `kind = 'mcp'`.

### 4. Group Members with MCPs

Groups can contain both skills and MCPs. Since MCPs are no longer in the `resources` table, `group_members.resource_id` uses `"mcp:{name}"` without a foreign key.

**Reading group members** — replace `db.get_group_members()` (which JOINs resources):
- New `manager.get_group_members(group_id)` queries `group_members` for IDs only (no JOIN).
- For each `resource_id`:
  - Starts with `"mcp:"` → build Resource from CLI config files using `read_mcp_status_from_configs()`
  - Otherwise → look up in `resources` table as before
- This replaces all call sites: `sm_groups`, `sm_group_enable/disable`, TUI `reload_group_detail`, TUI `enable_group/disable_group`.

**Adding MCP to group** — `sm_group_add` / TUI `pick_show_mcp`:
- `find_resource_id(name)` now checks config files for MCPs → returns `"mcp:{name}"`
- `db.add_group_member(group_id, "mcp:{name}")` works because no FK constraint

**Enabling/disabling a group:**
- Iterate members from new `get_group_members`, dispatch to `enable_resource`/`disable_resource` which handles `mcp:` prefix

### 5. Change Detection

**TUI mode** (already has `poll_config_changes`):
- Extend to also check mtime of CLI skills directories (`~/.claude/skills/`, etc.)
- Also check `~/.claude/mcp-configs/` directory mtime (MCP configs can live here too)
- On change detected: `reload()` which now reads filesystem anyway

**MCP server mode**:
- No polling needed. Each `sm_list` / `sm_status` call reads filesystem directly.
- Reads are cheap: stat a few symlinks + parse 1-4 JSON files < 1ms.

### 6. Market: Better Error Signals

When `sm_market` or `sm_market_install` encounters a repo:
- Has no `SKILL.md` files → check for `.claude-plugin/plugin.json`
  - If found: return message "This is a Claude Code plugin. Install with: /plugin install {name}@{marketplace}"
  - If not found: return "No skills found in this repository"
- Search returns empty with a search term → return "No skills matching '{query}' found. Available sources: {list}"

When `sm_sources` adds a new source and the background fetch returns zero skills, surface why (no SKILL.md found, API error, etc.) instead of silently caching an empty list.

### 7. Skill Install Completeness

- `Market::install_single` already downloads full directory recursively via Contents API — this is correct.
- `Installer::install_from_github` downloads tar.gz and extracts — this is also correct.
- Update CLAUDE.md to remove "downloads only SKILL.md" — it's inaccurate.
- Add validation after install: check that SKILL.md exists in the installed directory.

## Affected Files

| File | Change |
|------|--------|
| File | Change |
|------|--------|
| `core/db.rs` | Remove `resource_targets` table, remove MCP from `resources`, drop FK on `group_members`. Add `schema_version` table for migration. New `get_group_member_ids()` (no JOIN). Remove `set_target_enabled()`, `get_targets_for_resource()`, `enabled_count()`, `enabled_skill_count()`. |
| `core/manager.rs` | `list_resources(Mcp)` builds Resources from config files. `list_resources(Skill)` checks symlinks for enabled state. `enable/disable_resource` handles `mcp:` prefix without DB lookup. `find_resource_id` checks config files for MCPs. New `get_group_members()` replaces DB JOIN version. `is_first_launch()` checks config files too. `resource_count()` gets MCP total from config files. Remove all `set_target_enabled` calls. |
| `core/scanner.rs` | Remove MCP registration to DB. Remove `set_target_enabled` calls for skills. Simplify to only register new skill metadata to DB. |
| `core/mcp_discovery.rs` | No change (already reads config files correctly) |
| `core/resource.rs` | No structural change; `enabled` field still exists, populated by caller |
| `mcp/tools.rs` | `sm_market` returns better error messages. Fix tool description "downloads only SKILL.md" → "downloads full skill directory". |
| `core/market.rs` | `Market::fetch` detects `.claude-plugin` format and returns hint |
| `tui/app.rs` | `poll_config_changes` also watches skills directories + `mcp-configs/`. `reload` uses new filesystem-based list. |
| `CLAUDE.md` | Fix "downloads only SKILL.md" description |

## Test Plan

### Core tests to add (TDD)

1. **MCP list from config files** — write a temp config with MCPs, call `list_resources(Mcp)`, verify all MCPs returned with correct enabled state
2. **MCP enable/disable roundtrip** — disable an MCP, re-read config, verify `disabled: true`; enable it, verify field removed
3. **Skill enabled from symlink** — register skill in DB, create symlink, verify `is_enabled_for` returns true; remove symlink, verify returns false
4. **find_resource_id finds MCPs** — config file has MCP "foo", call `find_resource_id("foo")`, verify returns `"mcp:foo"`
5. **Group with MCP members** — create group, add `mcp:foo`, list members, verify MCP resource built from config file
6. **Group enable/disable with mixed members** — group has skill + MCP, enable group, verify symlink created + config written
7. **is_first_launch with MCPs only** — no skills in DB, but MCPs in config → returns false
8. **resource_count includes MCPs from config** — verify MCP total from config files
9. **Market plugin detection** — mock a repo tree with `.claude-plugin/plugin.json` and no SKILL.md, verify helpful message returned
10. **Change detection** — modify config file mtime, verify `poll_config_changes` triggers reload

### Existing tests to update

- `manager::tests::set_mcp_disabled_*` — keep as-is, still valid
- `mcp_discovery::tests::*` — keep as-is, still valid
- `mcp::tools::tests::tool_router_has_22_tools` — update count if tools change
- Remove any tests that assert on `resource_targets` behavior

## Migration

Use a `schema_version` table (standard SQLite migration pattern):

```sql
CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
```

On startup, check version. If < 2 (or table missing = version 0/1):

```sql
-- 1. Drop resource_targets
DROP TABLE IF EXISTS resource_targets;

-- 2. Remove MCP rows from resources
DELETE FROM resources WHERE kind = 'mcp';

-- 3. Recreate group_members without FK constraint
CREATE TABLE group_members_new (
    group_id TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    PRIMARY KEY (group_id, resource_id)
);
INSERT INTO group_members_new SELECT group_id, resource_id FROM group_members;
DROP TABLE group_members;
ALTER TABLE group_members_new RENAME TO group_members;

-- 4. Update version
DELETE FROM schema_version;
INSERT INTO schema_version VALUES (2);
```

This runs once on upgrade. The migration is idempotent (safe to re-run).

## Risks

- **Group members referencing deleted MCP rows**: mitigated by keeping `group_members` entries with `mcp:` prefix and resolving them dynamically.
- **Config file write races**: if user edits config while SM writes → mitigated by read-modify-write with pretty-print (same as current approach). Not atomic, but acceptable for this use case.
- **Performance of filesystem reads**: negligible. Stat ~20 symlinks + parse 4 small JSON files per query.
