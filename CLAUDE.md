# Runai

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
./target/debug/runai          # TUI mode
./target/debug/runai list     # CLI mode
./target/debug/runai mcp-serve # MCP server (stdio)
```

## Key Constraints

- **Scanner does not auto-run** on startup — avoids breaking existing skill links
- **Scanner has broken-link protection** — skips missing source dirs or missing SKILL.md
- **Data directory** — `~/.skill-manager/` (skills/ groups/ market-cache/ skill-manager.db) — kept for backward compatibility
- **Symlink direction** — `~/.claude/skills/<name>` -> `~/.skill-manager/skills/<name>`
- **MCP auto-registers** on first launch to all CLIs (claude/codex/gemini/opencode)
- **Market lists are disk-cached** — loads instantly, refreshes in background (1hr TTL)
- **Market install downloads full skill directory** — all files, not just SKILL.md
- **Filesystem is source of truth** — skill enabled = symlink exists; MCP enabled = CLI config file `disabled` field absent
- **DB stores only metadata and groups** — not runtime state; old tables preserved for rollback safety
- **Environment variables** — `RUNE_DATA_DIR` (preferred) or `SKILL_MANAGER_DATA_DIR` (legacy) to override data directory

## Migration Note

The binary was renamed from `skill-manager` to `runai` in v0.5.0. The data directory
`~/.skill-manager/` is intentionally preserved for backward compatibility. Both
`RUNE_DATA_DIR` and `SKILL_MANAGER_DATA_DIR` environment variables are accepted.

## Tests

```bash
cargo test                      # 60 tests
cargo test -- --test-threads=1  # if HOME env race conditions occur
```
