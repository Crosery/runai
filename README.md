# Skill Manager

**English** | [中文](README_zh.md)

A terminal-based resource manager for AI CLI skills, MCP servers, and groups. Works across **Claude Code**, **Codex**, **Gemini CLI**, and **OpenCode**.

![TUI Groups View](docs/images/tui-groups.png)

## Features

- **TUI Interface** — Browse, enable/disable, search skills and MCPs with a terminal UI
- **Multi-CLI Support** — Manage resources across 4 AI CLIs, switch targets with `1234`
- **Groups** — Organize skills/MCPs into groups, batch enable/disable
- **One-Step Install** — `sm_install(repo="owner/repo")` downloads, registers, groups, and enables
- **Market** — Browse 2000+ skills from 5 built-in sources, add custom GitHub sources
- **MCP Server** — 24 tools exposed via MCP protocol, auto-registered to all CLIs
- **Filesystem as Source of Truth** — Skill enabled = symlink exists; MCP enabled = config entry exists
- **Backup & Restore** — Timestamped full backups of skill directories, MCP configs, and CLI configs
- **CLI** — Subcommands for scripting and automation

## Install

```bash
git clone https://github.com/Crosery/skill-manager.git
cd skill-manager
cargo install --path .
```

## Quick Start

```bash
# Launch TUI (first run will scan and register MCP automatically)
skill-manager

# Or use CLI directly
skill-manager list                    # List all skills and MCPs
skill-manager status                  # Show enabled counts
skill-manager enable brainstorming    # Enable a skill
skill-manager scan                    # Scan for new skills
skill-manager backup                  # Create a backup
```

## Architecture

```
Filesystem is the single source of truth:
  Skill enabled  = symlink exists in ~/.claude/skills/<name>
  MCP enabled    = entry exists in ~/.claude.json mcpServers
  MCP disabled   = entry removed, config backed up to ~/.skill-manager/mcps/

DB stores only:
  Skill metadata (name, description, source, directory)
  Group membership (supports both skill and MCP members)
```

## TUI Keybindings

| Key | Action |
|-----|--------|
| `H/L` or `Tab` | Switch tabs (Skills / MCPs / Groups / Market) |
| `j/k` | Navigate up/down |
| `Space` | Toggle enable/disable |
| `1234` | Switch CLI target (Claude/Codex/Gemini/OpenCode) |
| `/` | Search |
| `Enter` | Open group detail / Install from market |
| `d` | Delete selected item |
| `c` | Create new group |
| `a` | Add to group (Skills/MCPs tab) |
| `s` | Sources manager (Market tab) / Scan (other tabs) |
| `[ ]` | Switch market source |
| `q` | Quit |

## MCP Tools (24)

When running as MCP server (`skill-manager mcp-serve`), 24 tools are available:

**Skills & MCPs**

| Tool | Description |
|------|-------------|
| `sm_list` | List skills/MCPs with filters (kind, group) |
| `sm_status` | Enabled/total counts per CLI target |
| `sm_enable` / `sm_disable` | Toggle skill/MCP for a CLI |
| `sm_delete` | Remove a skill/MCP (files + symlinks + DB) |
| `sm_scan` | Scan CLI directories for new skills (with error details) |
| `sm_batch_enable` / `sm_batch_disable` | Batch toggle multiple by name list |

**Install**

| Tool | Description |
|------|-------------|
| `sm_install` | Returns CLI command for fast install from GitHub (agent runs via Bash) |
| `sm_market` | Browse cached market skills (filter by source/search) |
| `sm_market_install` | Install single skill from market |
| `sm_sources` | List/add/remove/enable/disable market sources |

**Groups**

| Tool | Description |
|------|-------------|
| `sm_groups` | List all groups with member counts |
| `sm_create_group` / `sm_delete_group` | Create or delete a group |
| `sm_group_add` / `sm_group_remove` | Add/remove members |
| `sm_batch_group_add` | Add multiple members to a group at once |
| `sm_group_enable` / `sm_group_disable` | Batch toggle all members in a group |

**Backup & Utility**

| Tool | Description |
|------|-------------|
| `sm_backup` | Create timestamped backup |
| `sm_restore` | Restore from backup (latest or by timestamp) |
| `sm_backups` | List all available backups |
| `sm_register` | Register MCP to all CLI configs |

## MCP Behavior

- **Disable** = remove entry from CLI config, save full config to `~/.skill-manager/mcps/{name}.json`
- **Enable** = restore saved config back into CLI config
- **skill-manager refuses to disable itself** (self-protection)
- Disabled MCPs still visible in TUI/list (shown as disabled, can toggle back)
- Auto-registers to all CLIs on first launch

## Skill Discovery

- Scanner checks `~/.claude/skills/` (user-managed) and `~/.claude/.agents/skills/` (plugin-managed, read-only)
- SKILL.md frontmatter `description` field is parsed for display
- Stale descriptions are refreshed on re-scan
- Plugin-format repos (`.claude-plugin`) are detected with install guidance

## Market Sources

Built-in sources (enable/disable via `s` on Market tab):

| Source | Skills | Default |
|--------|--------|---------|
| Anthropic Official | 23 | Enabled |
| Everything Claude Code | 125 | Enabled |
| Terminal Skills | 900+ | Disabled |
| Antigravity Skills | 1300+ | Disabled |
| OK Skills | 55 | Disabled |

Add custom sources with `a` (format: `owner/repo` or `owner/repo@branch`).

## Backup & Restore

Backups stored at `~/.skill-manager/backups/{timestamp}/`:

```
backups/20260325_120000/
├── managed-skills/     # Full copy of ~/.skill-manager/skills/
├── managed-mcps/       # Disabled MCP config backups
├── claude-skills/      # Symlinks in ~/.claude/skills/
├── claude.json         # Copy of ~/.claude.json
├── gemini-settings.json
├── codex-settings.json
├── opencode-settings.json
└── timestamp
```

First scan automatically creates a backup before making any changes.

## Data

All data stored in `~/.skill-manager/`:
- `skills/` — Managed skill directories (each with SKILL.md)
- `mcps/` — Disabled MCP config backups (JSON)
- `groups/` — Group definitions (TOML files)
- `backups/` — Timestamped full backups
- `market-cache/` — Cached market skill lists (JSON, 1hr TTL)
- `market-sources.json` — Custom market sources
- `skill-manager.db` — SQLite database (skill metadata + group members only)

## Migration

When upgrading from v0.1.x:
- Old DB tables (`resource_targets`, MCP rows in `resources`) are preserved but ignored
- New code reads state from filesystem instead of DB
- Downgrade to old version is safe (old data still intact)

## License

MIT
