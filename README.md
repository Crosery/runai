# Skill Manager

A terminal-based resource manager for AI CLI skills, MCP servers, and groups. Works across **Claude Code**, **Codex**, **Gemini CLI**, and **OpenCode**.

## Features

- **TUI Interface** — Browse, enable/disable, search skills and MCPs with a terminal UI
- **Multi-CLI Support** — Manage resources across 4 AI CLIs, switch targets with `1234`
- **Groups** — Organize skills/MCPs into groups, batch enable/disable
- **Market** — Browse 2000+ skills from 5 built-in sources, add custom GitHub sources
- **MCP Server** — 17 tools exposed via MCP protocol, auto-registered to all CLIs
- **CLI** — 15 subcommands for scripting and automation

## Install

```bash
git clone https://github.com/Crosery/skill-manager.git
cd skill-manager
cargo build --release
```

Optionally add to PATH:
```bash
cp target/release/skill-manager ~/.local/bin/
```

## Quick Start

```bash
# Launch TUI (first run will scan and register MCP automatically)
skill-manager

# Or use CLI directly
skill-manager list                    # List all skills
skill-manager status                  # Show enabled counts
skill-manager enable brainstorming    # Enable a skill
skill-manager scan                    # Scan for new skills
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
| `s` | Sources manager (Market tab) / Scan (other tabs) |
| `[ ]` | Switch market source |
| `q` | Quit |

## MCP Tools

When running as MCP server (`skill-manager mcp-serve`), 17 tools are available:

| Tool | Description |
|------|-------------|
| `sm_list` | List skills/MCPs with filters |
| `sm_groups` | List all groups |
| `sm_status` | Enabled/total counts per target |
| `sm_enable` / `sm_disable` | Toggle skill/MCP for a CLI |
| `sm_scan` | Scan directories for new skills |
| `sm_delete` | Remove a skill/MCP |
| `sm_create_group` / `sm_delete_group` | Group CRUD |
| `sm_group_add` / `sm_group_remove` | Manage group members |
| `sm_group_enable` / `sm_group_disable` | Batch toggle group |
| `sm_market` | Browse market skills |
| `sm_market_install` | Install single skill from market |
| `sm_sources` | Manage market sources |
| `sm_register` | Register MCP to all CLIs |

The MCP server auto-registers to `~/.claude.json`, `~/.codex/settings.json`, `~/.gemini/settings.json`, and `~/.opencode/settings.json` on first launch.

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
- `groups/` — Group definitions (TOML files)
- `market-cache/` — Cached market skill lists (JSON, auto-refreshed)
- `market-sources.json` — Custom market sources
- `skill-manager.db` — SQLite database (resources, targets, group members)

## License

MIT
