# Runai

AI CLI skill/MCP resource manager — TUI + MCP server.

## Architecture

- **Rust** — single binary, no runtime dependencies
- **Core** — `src/core/` (paths, db, linker, scanner, classifier, manager, installer, market, dazi)
- **TUI** — `src/tui/` (ratatui + crossterm, 5 tabs: Skills/MCPs/Groups/Market/搭子)
- **CLI** — `src/cli/` (15 subcommands)
- **MCP server** — `src/mcp/` (rmcp, 42 tools, stdio transport)

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
- **Data directory** — `~/.runai/` (skills/ groups/ market-cache/ runai.db). Auto-migrates from `~/.skill-manager/` on first launch.
- **Symlink direction** — `~/.claude/skills/<name>` -> `~/.runai/skills/<name>`
- **MCP auto-registers** on first launch to all CLIs (claude/codex/gemini/opencode)
- **Market lists are disk-cached** — loads instantly, refreshes in background (1hr TTL)
- **Market install downloads full skill directory** — all files, not just SKILL.md
- **Filesystem is source of truth** — skill enabled = symlink exists; MCP enabled = CLI config file `disabled` field absent
- **DB stores only metadata and groups** — not runtime state; old tables preserved for rollback safety
- **Environment variables** — `RUNE_DATA_DIR` (preferred) or `SKILL_MANAGER_DATA_DIR` (legacy) to override data directory

## Dazi Marketplace (搭子)

- **Module** — `src/core/dazi.rs` — HTTP API client for `dazi.ktvsky.com`
- **Three resource types** — Skills (ZIP download), Agents (JSON → SKILL.md), Bundles (batch install)
- **Cache** — `~/.runai/dazi-cache/` (skills.json, agents.json, bundles.json), 1hr TTL
- **MCP token** — `~/.runai/dazi-token.json`, auto-refresh every 10min in TUI, registers `dazi-marketplace` as remote MCP in CLI configs
- **Session** — `~/.runai/dazi-session.json`, for team API operations (bundle publish), 7-day validity with auto-renewal
- **ZIP prefix stripping** — `extract_zip()` auto-strips common top-level directory prefix (e.g. `docx/SKILL.md` → `SKILL.md`)
- **MCP tools (12)**:
  - `sm_dazi_search` / `sm_dazi_list` / `sm_dazi_stats` — browse & search
  - `sm_dazi_install` / `sm_dazi_install_bundle` — install skills/agents/bundles
  - `sm_dazi_publish` / `sm_dazi_publish_agent` — publish to marketplace
  - `sm_dazi_login` / `sm_dazi_logout` — session management (local HTTP server + browser auth)
  - `sm_dazi_publishable` / `sm_dazi_publish_bundle` — team bundle operations (requires login)
  - `sm_dazi_refresh` — refresh cache + MCP token
- **Environment variables** — `DAZI_BASE_URL` to override marketplace server

## Migration Note (v0.5.0)

The binary was renamed from `skill-manager` to `runai`. On first launch:
1. Data directory auto-migrates: `~/.skill-manager/` → `~/.runai/`
2. DB file auto-renames: `skill-manager.db` → `runai.db`
3. MCP entries auto-migrate: `skill-manager` → `runai` in all CLI configs
4. Both `RUNE_DATA_DIR` and `SKILL_MANAGER_DATA_DIR` env vars accepted

## Tests

```bash
cargo test                      # 123 tests
cargo test -- --test-threads=1  # if HOME env race conditions occur
```
