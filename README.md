# Runai

**English** | [中文](README_zh.md)

A terminal-based resource manager for AI CLI skills, MCP servers, and groups. Works across **Claude Code**, **Codex**, **Gemini CLI**, and **OpenCode**.

![TUI Groups View](docs/images/tui-groups.png)

## Features

- **TUI Interface** — Browse, enable/disable, search skills and MCPs with a terminal UI
- **Multi-CLI Support** — Manage resources across 4 AI CLIs, switch targets with `1234`
- **Groups** — Organize skills/MCPs into groups, batch enable/disable, rename
- **One-Step Install** — `runai install owner/repo` downloads, registers, groups, and enables
- **Skill Discovery** — Built-in recursive scanner finds all SKILL.md on disk in seconds
- **Market** — Browse 2000+ skills from 5 built-in sources, add custom GitHub sources
- **MCP Server** — 25 tools exposed via MCP protocol, auto-registered to all CLIs on first launch
- **Dark/Light Theme** — Press `t` to toggle, optimized for both terminal backgrounds
- **Filesystem as Source of Truth** — Skill enabled = symlink exists; MCP enabled = config entry exists
- **Backup & Restore** — Timestamped full backups of skill directories, MCP configs, and CLI configs
- **CLI** — Subcommands for scripting and automation

## Install

```bash
git clone https://github.com/Crosery/runai.git
cd runai
cargo install --path .
```

## Quick Start

```bash
# Launch TUI (first run will scan and register MCP automatically)
runai

# Install skills from GitHub (auto-download, register, group, enable)
runai install pbakaus/impeccable
runai install MiniMax-AI/skills

# Install from market
runai market-install github

# Discover all skills on disk
runai discover
runai discover --root /    # Full disk scan

# CLI management
runai list                    # List all skills and MCPs
runai status                  # Show enabled counts
runai enable brainstorming    # Enable a skill
runai scan                    # Scan known directories
runai backup                  # Create a backup
```

## TUI Keybindings

Footer shows essential keys. Press `?` for full help panel.

| Key | Action |
|-----|--------|
| `j/k` | Navigate up/down |
| `H/L` or `Tab` | Switch tabs (Skills / MCPs / Groups / Market) |
| `Space` | Toggle enable/disable |
| `/` | Search filter |
| `t` | Toggle dark/light theme |
| `?` | Help panel (all keybindings) |
| `q` | Quit |

## MCP Tools (25)

When running as MCP server (`runai mcp-serve`), 25 tools are available:

**Skills & MCPs**

| Tool | Description |
|------|-------------|
| `sm_list` | List skills/MCPs (compact format, supports kind/group/target filters) |
| `sm_status` | Enabled/total counts per CLI target |
| `sm_enable` / `sm_disable` | Toggle skill/MCP for a CLI (supports fuzzy group name) |
| `sm_delete` | Remove a skill/MCP (files + symlinks + DB) |
| `sm_scan` | Scan known directories for new skills |
| `sm_discover` | Find all SKILL.md on disk, returns unmanaged skills |
| `sm_batch_enable` / `sm_batch_disable` | Batch toggle multiple by name list |

**Install**

| Tool | Description |
|------|-------------|
| `sm_install` | Returns CLI command for fast GitHub install (agent runs via Bash) |
| `sm_market` | Browse cached market skills (filter by source/search) |
| `sm_market_install` | Returns CLI command for market install |
| `sm_sources` | List/add/remove/enable/disable market sources |

**Groups**

| Tool | Description |
|------|-------------|
| `sm_groups` | List all groups with member counts |
| `sm_create_group` / `sm_delete_group` | Create or delete a group |
| `sm_group_add` / `sm_group_remove` | Add/remove members (single `name` or batch `names`) |
| `sm_update_group` | Update group name and/or description |
| `sm_group_enable` / `sm_group_disable` | Batch toggle all members (fuzzy group match) |

**Backup & Utility**

| Tool | Description |
|------|-------------|
| `sm_backup` | Create timestamped backup |
| `sm_restore` | Restore from backup (latest or by timestamp) |
| `sm_backups` | List all available backups |
| `sm_register` | Register MCP to all CLI configs |

## Key Behaviors

- **Fuzzy group matching** — `sm_group_enable(name="superpower")` matches `superpowers`
- **Install delegates to CLI** — MCP tools return Bash commands instead of downloading in-process (avoids proxy timeouts)
- **Compact output** — `sm_list` uses one-line-per-resource format to stay within token limits
- **Auto-discovery** — MCP instructions guide AI to search GitHub when market has no results
- **Self-protection** — runai refuses to disable itself
- **Scans `~/skills/`** — SkillHub installs are automatically discovered

## Skill Discovery

```bash
runai discover               # Scan home directory
runai discover --root /      # Full disk scan
```

Built-in recursive scanner with smart filtering:
- **Finds**: `~/.skill-manager/skills/`, `~/.claude/skills/`, `~/skills/`, project dirs
- **Skips**: plugins/marketplaces, IDE extensions, backups, node_modules, .git
- **Classifies**: `●` Managed / `◆` CLI dir / `○` Unmanaged (can import)

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

## Data

All data stored in `~/.skill-manager/`:
- `skills/` — Managed skill directories (each with SKILL.md)
- `mcps/` — Disabled MCP config backups (JSON)
- `groups/` — Group definitions (TOML files)
- `backups/` — Timestamped full backups
- `market-cache/` — Cached market skill lists (JSON, 1hr TTL)
- `market-sources.json` — Custom market sources
- `skill-manager.db` — SQLite database (skill metadata + group members only)

> **Note**: The data directory `~/.skill-manager/` is kept for backward compatibility with versions prior to v0.5.0.

## License

MIT
