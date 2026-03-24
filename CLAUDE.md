# Skill Manager

AI CLI skill/MCP resource manager — TUI + MCP server.

## Architecture

- **Rust** — single binary, no runtime dependencies
- **Core** — `src/core/` (paths, db, linker, scanner, classifier, manager, installer, market)
- **TUI** — `src/tui/` (ratatui + crossterm, 4 tabs: Skills/MCPs/Groups/Market)
- **CLI** — `src/cli/` (15 subcommands)
- **MCP server** — `src/mcp/` (rmcp, 17 tools, stdio transport)

## Build & Run

```bash
cargo build
./target/debug/skill-manager          # TUI mode
./target/debug/skill-manager list     # CLI mode
./target/debug/skill-manager mcp-serve # MCP server (stdio)
```

## Key Constraints

- **Scanner does not auto-run** on startup — avoids breaking existing skill links
- **Scanner has broken-link protection** — skips missing source dirs or missing SKILL.md
- **Data directory** — `~/.skill-manager/` (skills/ groups/ market-cache/ skill-manager.db)
- **Symlink direction** — `~/.claude/skills/<name>` -> `~/.skill-manager/skills/<name>`
- **MCP auto-registers** on first launch to all CLIs (claude/codex/gemini/opencode)
- **Market lists are disk-cached** — loads instantly, refreshes in background (1hr TTL)
- **Market install downloads only SKILL.md** — not the entire repo

## Tests

```bash
cargo test    # 38 tests
```
