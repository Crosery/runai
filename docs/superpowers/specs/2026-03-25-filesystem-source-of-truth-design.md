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
- `enable_resource` / `disable_resource` for MCPs writes CLI config file (already works in `set_mcp_disabled`). No DB write.
- `resources` table no longer stores MCP entries. Existing MCP rows are ignored/cleaned up.
- `status()` counts MCPs from config files (already does this).

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
- `enable_resource` creates symlink only. `disable_resource` removes symlink only.
- `resource_targets` table is deleted from schema. `enabled_count` / `enabled_skill_count` scan the filesystem.

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

When listing group members:
- For `resource_id` starting with `"mcp:"`: build Resource from CLI config files
- For other IDs: look up in `resources` table as before

When enabling/disabling a group:
- Iterate members, dispatch to skill or MCP enable/disable based on ID prefix

### 5. Change Detection

**TUI mode** (already has `poll_config_changes`):
- Extend to also check mtime of CLI skills directories (`~/.claude/skills/`, etc.)
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
| `core/db.rs` | Remove `resource_targets` table, remove MCP from `resources`, drop FK on `group_members` |
| `core/manager.rs` | `list_resources` reads MCP from config files; skill enabled from symlinks; remove `resource_targets` writes |
| `core/scanner.rs` | Remove MCP registration to DB; simplify to only handle skills |
| `core/mcp_discovery.rs` | No change (already reads config files correctly) |
| `core/resource.rs` | `Resource.enabled` populated by filesystem checks, not DB |
| `mcp/tools.rs` | No structural change; `sm_market` returns better error messages |
| `core/market.rs` | `Market::fetch` detects `.claude-plugin` format and returns hint |
| `tui/app.rs` | `poll_config_changes` also watches skills directories; `reload` uses new filesystem-based list |
| `CLAUDE.md` | Fix "downloads only SKILL.md" description |

## Test Plan

### Core tests to add (TDD)

1. **MCP list from config files** — write a temp config with MCPs, call `list_resources(Mcp)`, verify all MCPs returned with correct enabled state
2. **MCP enable/disable roundtrip** — disable an MCP, re-read config, verify `disabled: true`; enable it, verify field removed
3. **Skill enabled from symlink** — register skill in DB, create symlink, verify `is_enabled_for` returns true; remove symlink, verify returns false
4. **Group with MCP members** — create group, add `mcp:foo`, list members, verify MCP resource built from config file
5. **Group enable/disable with mixed members** — group has skill + MCP, enable group, verify symlink created + config written
6. **Market plugin detection** — mock a repo tree with `.claude-plugin/plugin.json` and no SKILL.md, verify helpful message returned
7. **Change detection** — modify config file mtime, verify `poll_config_changes` triggers reload

### Existing tests to update

- `manager::tests::set_mcp_disabled_*` — keep as-is, still valid
- `mcp_discovery::tests::*` — keep as-is, still valid
- `mcp::tools::tests::tool_router_has_22_tools` — update count if tools change
- Remove any tests that assert on `resource_targets` behavior

## Migration

On startup, `Database::init_schema`:
1. Check if `resource_targets` table exists → drop it
2. Delete from `resources` where `kind = 'mcp'`
3. Remove FK constraint from `group_members` (recreate table without FK)

This is a one-way migration. The old schema is not needed after upgrade.

## Risks

- **Group members referencing deleted MCP rows**: mitigated by keeping `group_members` entries with `mcp:` prefix and resolving them dynamically.
- **Config file write races**: if user edits config while SM writes → mitigated by read-modify-write with pretty-print (same as current approach). Not atomic, but acceptable for this use case.
- **Performance of filesystem reads**: negligible. Stat ~20 symlinks + parse 4 small JSON files per query.
