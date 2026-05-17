# Runai

**English** | [中文](README_zh.md)

<p align="center">
  <img src="docs/images/runai-logo.png" alt="runai logo" width="180">
</p>

**runai** — a Swiss Army knife for managing AI CLI skills and MCP servers.

Skills and MCP servers live scattered across Claude Code, Codex, Gemini CLI, and OpenCode — each with its own config file and quirks. runai gives you one TUI (plus a scriptable CLI and an MCP server) to browse, enable, install, search, and back up all of it at once, on **macOS / Linux / Windows**.

- Install any skill from GitHub into all four CLIs with one command
- 2,000+ skills one-click installable from the built-in market
- Filesystem is the source of truth: symlink exists = enabled, gone = disabled
- Native config formats for Claude JSON, Codex TOML, Gemini JSON, OpenCode JSON
- Native binaries for three OSes; Developer Mode recommended on Windows for symlinks

## Features

- **TUI Interface** — Browse, enable/disable, search skills and MCPs with a terminal UI
- **Multi-CLI Support** — Manage resources across 4 AI CLIs, switch targets with `1234`
- **Groups** — Organize skills/MCPs into groups, batch enable/disable, rename
- **One-Step Install** — `runai install owner/repo` downloads, registers, groups, and enables
- **Market Install** — Browse 2000+ skills, Enter to install directly from TUI Market tab
- **Trash & Restore** — Deletes go into a global trash tab first, with restore and permanent purge
- **Skill Discovery** — Built-in recursive scanner finds all SKILL.md on disk in seconds
- **Unified Search** — `sm_search` searches installed resources and market at once
- **Usage Tracking** — Track skill usage count and last-used time, identify unused skills
- **MCP Server** — 21 tools exposed via MCP protocol, auto-registered to all CLIs on first launch
- **Batch Operations** — Batch enable/disable/delete/install multiple resources in one call
- **Multi-CLI Config** — Native format support: Claude JSON, Codex TOML, OpenCode custom JSON, Gemini JSON
- **Dark/Light Theme** — Press `t` to toggle, optimized for both terminal backgrounds
- **Filesystem as Source of Truth** — Skill enabled = symlink exists; MCP enabled = config entry exists
- **Backup & Restore** — Timestamped full backups of skill directories, MCP configs, and CLI configs
- **Auto Migration** — Seamless upgrade from `skill-manager` to `runai` (data dir, DB, symlinks, MCP entries)
- **CLI** — Subcommands for scripting and automation

## Install

```bash
git clone https://github.com/Crosery/runai.git
cd runai
cargo install --path .
```

### Windows

Pre-built binary: download `runai-windows-amd64.zip` from [releases](https://github.com/Crosery/runai/releases) and put `runai.exe` on your PATH. Or build from source with `cargo install --path .`.

**Symlink prerequisite**: runai uses filesystem symlinks as the source of truth for "skill enabled". Windows creating symlinks requires one of:

- **Developer Mode** enabled (Settings → Privacy & security → For developers → Developer Mode), or
- Running the shell as **Administrator**

Without either, `enable`/`install` will fail when creating the symlink. Developer Mode is the recommended option (no elevation per-invocation).

CLI config files are read from the same user-home paths as unix (`%USERPROFILE%\.claude.json`, `.codex\config.toml`, `.gemini\settings.json`, `.config\opencode\opencode.json`) — verified against each CLI's source.

## Quick Start

```bash
# Launch TUI (first run will scan, register MCP, and migrate from skill-manager if needed)
runai

# Install skills from GitHub (auto-download, register, group, enable)
runai install pbakaus/impeccable
runai install MiniMax-AI/skills

# Install from market
runai market-install github

# Show usage statistics
runai usage --top 10

# Discover all skills on disk
runai discover

# CLI management
runai list                    # List all skills and MCPs
runai status                  # Show enabled counts
runai enable brainstorming    # Enable a skill
runai uninstall brainstorming # Move a resource into trash
runai trash list              # List trash entries
runai trash restore brainstorming
runai scan                    # Scan known directories
runai backup                  # Create a backup
runai backups                 # List existing backups
runai search figma            # Search installed resources and market
runai market --search figma   # Browse market skills, optional --source filter
runai group delete my-group   # Remove a group definition (members untouched)
runai group update my-group --name "New Name" --description "..."
```

## Skill auto-routing (opt-in)

Tell Claude Code which installed skills are relevant for each prompt — without typing skill names yourself. A small LLM picks top-K skills from your installed set and runai emits their full `SKILL.md` content into the Claude Code conversation via a `UserPromptSubmit` hook.

Disabled by default. To enable:

```bash
runai recommend setup          # interactive: pick provider, paste API key, choose model
runai recommend hook-snippet   # prints the JSON to drop into ~/.claude/settings.json
runai recommend status         # shows current config (API key redacted)
```

Default provider is OpenAI-compatible with DeepSeek (`deepseek-v4-flash`, ~1s per route, very cheap). Anthropic Messages API also supported (set `provider = "anthropic"`, `model = "claude-haiku-4-5-20251001"`). Any OpenAI-compatible backend works — Moonshot, Groq, vLLM, etc.

API key can also come from env `RUNAI_RECOMMEND_API_KEY`. Config lives at `~/.runai/config.toml` with `0o600` permission.

## TUI Keybindings

Footer shows essential keys. Press `?` for full help panel.

| Key | Action |
|-----|--------|
| `j/k` | Navigate up/down |
| `H/L` or `Tab` | Switch tabs (Skills / MCPs / Groups / Market / Trash) |
| `Space` | Toggle enable/disable |
| `Enter` | Open group detail / Install from market |
| `d` | Move selected skill/MCP into trash |
| `r` | Restore selected trash entry (Trash tab) |
| `Shift+D` | Permanently delete selected trash entry (Trash tab) |
| `/` | Search filter |
| `1234` | Switch CLI target (Claude/Codex/Gemini/OpenCode) |
| `i` | Install from GitHub |
| `t` | Toggle dark/light theme |
| `?` | Help panel (all keybindings) |
| `q` | Quit |

## MCP Tools (21)

When running as MCP server (`runai mcp-serve`), 21 tools are available:

**Skills & MCPs**

| Tool | Description |
|------|-------------|
| `sm_list` | List skills/MCPs with usage count (supports kind/group/target filters) |
| `sm_status` | Enabled/total counts per CLI target |
| `sm_enable` / `sm_disable` | Toggle skill/MCP for a CLI (supports fuzzy group name) |
| `sm_delete` | Move a skill/MCP into global trash |
| `sm_scan` | Scan known directories for new skills |
| `sm_search` | Unified search across installed resources + market |

**Install**

| Tool | Description |
|------|-------------|
| `sm_install` | Returns CLI command for fast GitHub install (agent runs via Bash) |
| `sm_market` | Browse cached market skills (filter by source/search/repo path) |
| `sm_market_install` | Returns CLI command for market install |

**Groups**

| Tool | Description |
|------|-------------|
| `sm_groups` | List all groups with member counts + 200-char description preview (full description via `runai group show <id>` CLI) |
| `sm_create_group` / `sm_delete_group` | Create or delete a group |
| `sm_group_members` | Add/remove/update group members and metadata |

**Trash**

| Tool | Description |
|------|-------------|
| `sm_trash` | List global trash entries |
| `sm_trash_restore` | Restore a trash entry by trash ID or resource name |
| `sm_trash_purge` | Permanently delete a trash entry by trash ID or resource name |

**Usage Tracking**

| Tool | Description |
|------|-------------|
| `sm_usage_stats` | Show usage statistics sorted by most used |

**Backup & Utility**

| Tool | Description |
|------|-------------|
| `sm_backup` | Create timestamped backup |
| `sm_restore` | Restore from backup (latest or by timestamp) |
| `sm_backups` | List all available backups |

## Multi-CLI Config Formats

| CLI | Config File | Format |
|-----|-------------|--------|
| Claude | `~/.claude.json` | JSON (`mcpServers`) |
| Codex | `~/.codex/config.toml` | TOML (`[mcp_servers.*]`) |
| Gemini | `~/.gemini/settings.json` | JSON (`mcpServers`) |
| OpenCode | `~/.config/opencode/opencode.json` | JSON (`mcp`, command=array) |

## Data

All data stored in `~/.runai/`:
- `skills/` — Managed skill directories (each with SKILL.md)
- `mcps/` — Disabled MCP config backups (JSON)
- `groups/` — Group definitions (TOML files)
- `trash/` — Deleted resource payloads kept for restore/purge
- `backups/` — Timestamped full backups
- `market-cache/` — Cached market skill lists (JSON, 1hr TTL)
- `market-sources.json` — Custom market sources
- `runai.db` — SQLite database (skill metadata, usage stats, group members)

## Migration from skill-manager

Runai v0.5.0 auto-migrates on first launch:
1. Data directory: `~/.skill-manager/` → `~/.runai/`
2. Database: `skill-manager.db` → `runai.db`
3. Symlinks: all CLI skill symlinks repointed automatically
4. MCP entries: `skill-manager` → `runai` in all CLI configs
5. Environment variables: both `RUNE_DATA_DIR` and `SKILL_MANAGER_DATA_DIR` accepted

No manual steps needed. All data is preserved.

## License

MIT
