# runai — AI Agent Guide

> **Single source of truth for any AI assistant** (Claude Code / Codex / Gemini CLI / OpenCode / Cursor / …).
> Human-readable docs live in [README.md](README.md) and [README_zh.md](README_zh.md) — do not duplicate that content here. This file is for agents.

---

## Maintenance invariants (read first, enforce always)

**Every code change must ship its documentation update in the same commit.** Missing docs = half-finished work, treat the PR as not ready to merge.

| If you changed … | You MUST update … |
|---|---|
| A public API, behavior, invariant, or gotcha of a module | That module's sibling `*.LLM.md` (e.g. `src/core/updater.rs` → `src/core/updater.LLM.md`) |
| User-visible CLI flags, install steps, or features | Both `README.md` AND `README_zh.md` (keep in sync) |
| Cross-cutting architecture, a new module, or an invariant that spans modules | This file's "Architecture" / "Key constraints" sections + the Module index table |
| Release-worthy fix or feature | Bump `Cargo.toml` version, tag `vX.Y.Z`, let `.github/workflows/release.yml` build artifacts |
| CI / build / release workflows | Add a note under "Build & CI" below |

**Version cadence**: patch for bug fixes, minor for features, no major bumps until v1.0. Commit messages follow conventional commits (see `git log --oneline`).

---

## Architecture

- **Language/runtime**: Rust 2024 edition, single static binary, no runtime dependencies.
- **Top-level crates/modules** under `src/`:
  - `cli/` — clap subcommand dispatch. Every user-facing subcommand lives here.
  - `core/` — business logic. 19 files, see Module index. `manager.rs` is the orchestration hub.
  - `mcp/` — rmcp-based MCP server exposing tool calls to host CLIs (stdio transport).
  - `tui/` — ratatui + crossterm full-screen UI. `app.rs` is the state machine; `ui.rs` renders.
- **Data layout**: `~/.runai/` holds `skills/`, `mcps/`, `groups/`, `backups/`, `market-cache/`, `runai.db` (SQLite via rusqlite bundled). On Windows: `%APPDATA%\runai\` (via `dirs::data_dir`).
- **Source of truth**:
  - Skill **enabled** = symlink exists at `<cli-home>/<target>/skills/<name>` pointing at `~/.runai/skills/<name>`.
  - MCP **enabled** = entry present in target CLI's config file (no `"disabled": true`).
  - DB carries metadata, groups, usage counts — **never runtime enabled state**.
- **Config targets** (all config paths are `dirs::home_dir()`-rooted on every OS, including Windows):
  - Claude Code: `~/.claude.json`
  - Codex: `~/.codex/config.toml`
  - Gemini CLI: `~/.gemini/settings.json`
  - OpenCode: `~/.config/opencode/opencode.json`

---

## Module index

File-level LLM docs follow the convention `<name>.LLM.md` as a sibling to the source file. Find the doc for any file by appending `.LLM.md`.

| Module | Source | Doc | One-liner |
|---|---|---|---|
| cli | [src/cli/mod.rs](src/cli/mod.rs) | [src/cli/mod.LLM.md](src/cli/mod.LLM.md) | clap subcommand dispatcher + TUI launcher |
| core::auto_group | [src/core/auto_group.rs](src/core/auto_group.rs) | [src/core/auto_group.LLM.md](src/core/auto_group.LLM.md) | Heuristic grouping of freshly-installed resources |
| core::backup | [src/core/backup.rs](src/core/backup.rs) | [src/core/backup.LLM.md](src/core/backup.LLM.md) | Timestamped backup/restore of managed data and CLI configs |
| core::channel | [src/core/channel.rs](src/core/channel.rs) | [src/core/channel.LLM.md](src/core/channel.LLM.md) | Release channel (stable / beta) selection |
| core::classifier | [src/core/classifier.rs](src/core/classifier.rs) | [src/core/classifier.LLM.md](src/core/classifier.LLM.md) | Classifies installable artifacts into Skill vs MCP vs Agent |
| core::cli_target | [src/core/cli_target.rs](src/core/cli_target.rs) | [src/core/cli_target.LLM.md](src/core/cli_target.LLM.md) | CliTarget enum + per-target dir/config resolvers |
| core::dazi | [src/core/dazi.rs](src/core/dazi.rs) | [src/core/dazi.LLM.md](src/core/dazi.LLM.md) | Dazi marketplace HTTP client (feature-gated) |
| core::db | [src/core/db.rs](src/core/db.rs) | [src/core/db.LLM.md](src/core/db.LLM.md) | SQLite schema + migrations + query layer |
| core::doctor | [src/core/doctor.rs](src/core/doctor.rs) | [src/core/doctor.LLM.md](src/core/doctor.LLM.md) | `runai doctor` health checks |
| core::group | [src/core/group.rs](src/core/group.rs) | [src/core/group.LLM.md](src/core/group.LLM.md) | Group definition (TOML on disk) + member type |
| core::installer | [src/core/installer.rs](src/core/installer.rs) | [src/core/installer.LLM.md](src/core/installer.LLM.md) | GitHub / market install pipeline |
| core::linker | [src/core/linker.rs](src/core/linker.rs) | [src/core/linker.LLM.md](src/core/linker.LLM.md) | Cross-platform symlink create/remove/detect |
| core::manager | [src/core/manager.rs](src/core/manager.rs) | [src/core/manager.LLM.md](src/core/manager.LLM.md) | `SkillManager` — orchestrates everything |
| core::market | [src/core/market.rs](src/core/market.rs) | [src/core/market.LLM.md](src/core/market.LLM.md) | Market source list + skill index cache (1h TTL) |
| core::mcp_discovery | [src/core/mcp_discovery.rs](src/core/mcp_discovery.rs) | [src/core/mcp_discovery.LLM.md](src/core/mcp_discovery.LLM.md) | Discover MCP entries from existing CLI configs |
| core::mcp_register | [src/core/mcp_register.rs](src/core/mcp_register.rs) | [src/core/mcp_register.LLM.md](src/core/mcp_register.LLM.md) | Self-register runai as an MCP across all four CLIs |
| core::paths | [src/core/paths.rs](src/core/paths.rs) | [src/core/paths.LLM.md](src/core/paths.LLM.md) | `AppPaths` resolver + legacy-dir migration |
| core::resource | [src/core/resource.rs](src/core/resource.rs) | [src/core/resource.LLM.md](src/core/resource.LLM.md) | `Resource` / `ResourceKind` domain types |
| core::scanner | [src/core/scanner.rs](src/core/scanner.rs) | [src/core/scanner.LLM.md](src/core/scanner.LLM.md) | Filesystem discovery + adoption of unmanaged skills |
| core::updater | [src/core/updater.rs](src/core/updater.rs) | [src/core/updater.LLM.md](src/core/updater.LLM.md) | Self-update: check, download, verify, replace binary |
| mcp::tools | [src/mcp/tools.rs](src/mcp/tools.rs) | [src/mcp/tools.LLM.md](src/mcp/tools.LLM.md) | 30 `sm_*` tools exposed to MCP clients |
| mcp::dazi_tools | [src/mcp/dazi_tools.rs](src/mcp/dazi_tools.rs) | [src/mcp/dazi_tools.LLM.md](src/mcp/dazi_tools.LLM.md) | 12 additional Dazi tools (feature=dazi) |
| tui::app | [src/tui/app.rs](src/tui/app.rs) | [src/tui/app.LLM.md](src/tui/app.LLM.md) | TUI state machine and event loop |
| tui::ui | [src/tui/ui.rs](src/tui/ui.rs) | [src/tui/ui.LLM.md](src/tui/ui.LLM.md) | Rendering for all TUI tabs/panels |
| tui::theme | [src/tui/theme.rs](src/tui/theme.rs) | [src/tui/theme.LLM.md](src/tui/theme.LLM.md) | Dark/light color themes |
| tui::i18n | [src/tui/i18n.rs](src/tui/i18n.rs) | [src/tui/i18n.LLM.md](src/tui/i18n.LLM.md) | English/Chinese UI strings |

Small `mod.rs` wiring files without substance are not separately documented; their contents are obvious `pub mod` declarations.

---

## Key constraints (load-bearing, do not break silently)

- **Scanner never auto-runs at startup.** It's explicit (`runai scan` / `runai discover`) — auto-running risks clobbering user symlinks.
- **Scanner is defensive.** It skips missing source dirs and missing `SKILL.md` rather than erroring; orphan symlinks are left alone, only matching-name broken symlinks are healed.
- **Data directory auto-migrates** from `~/.skill-manager/` → `~/.runai/` on first launch (v0.5.0 transition). DB file, symlinks, and CLI MCP entries all get renamed. `RUNE_DATA_DIR` and `SKILL_MANAGER_DATA_DIR` env vars both honored.
- **MCP self-registration** runs on first launch if not already present in a CLI's config. Idempotent — re-running does nothing if the entry already matches.
- **Market lists are disk-cached** under `~/.runai/market-cache/`; refresh is background, 1-hour TTL. UI loads instantly from cache.
- **Market install fetches the full skill dir**, not just `SKILL.md` — skills often have assets.
- **DB only carries metadata**, never runtime enabled state (that's filesystem). Old tables are preserved for rollback safety.
- **Symlinks in Windows** require Developer Mode or Administrator — `linker.rs` uses `symlink_dir`; failures surface as permission errors.
- **`dirs::home_dir()` on Windows** (dirs 6.x) uses the Win32 `SHGetKnownFolderPath` API and **ignores HOME / USERPROFILE env vars**, so tests cannot mock home via env. The `manager::tests` module is consequently gated with `#[cfg(not(target_os = "windows"))]`.

---

## Dazi marketplace (`feature = "dazi"`)

Optional, compiled out by default. Ships as `v*-dazi` tagged releases (`release.yml` reads tag suffix).

- HTTP client for `dazi.ktvsky.com`, three resource types (Skills ZIP, Agents JSON → SKILL.md, Bundles batch install).
- Session/token stored at `~/.runai/dazi-{session,token}.json`; token auto-refreshes every 10 min in TUI.
- Registers `dazi-marketplace` as a remote MCP in CLI configs.
- `DAZI_BASE_URL` env var overrides the server.
- 12 extra MCP tools (`sm_dazi_*`) — see [src/mcp/dazi_tools.LLM.md](src/mcp/dazi_tools.LLM.md).

---

## Build & run

```bash
cargo build
./target/debug/runai            # TUI mode (default)
./target/debug/runai list       # CLI mode
./target/debug/runai mcp-serve  # MCP server over stdio
```

## Build & CI

- **CI** (`.github/workflows/ci.yml`): `cargo fmt --check` → `cargo clippy -W all` → `cargo test -- --test-threads=1`, matrix = `[ubuntu-latest, macos-latest, windows-latest]`, `fail-fast: false`.
- **Release** (`.github/workflows/release.yml`): triggered by `v*` tags; matrix produces `runai-{linux,darwin,windows}-{amd64,arm64}.{tar.gz,zip}` + `checksums.txt`. Windows target skipped for arm64 (no MSVC cross from runner host); all others present.
- **HOME mocking** in `manager::tests` uses `HOME` env var — unix only. Do not assume it works on Windows (see Key constraints).

---

## Tests

```bash
cargo test -- --test-threads=1   # default in CI; SQLite dislikes parallel I/O here
cargo test --lib <module>        # scope to a module
```

**Test count varies by platform**: unix runs 147, Windows skips `manager::tests` (~30 tests) so count drops to ~115. That's intentional — see Key constraints.

---

## Getting oriented as a new agent

1. Start at the Module index above. Click through to the `*.LLM.md` for whichever module the current task touches.
2. If you're editing a module's code, the sibling `*.LLM.md` is your first read — it tells you the public API surface, invariants, and gotchas without making you reverse-engineer from code.
3. When unsure about cross-module behavior, re-read "Key constraints" — most non-obvious invariants live there.
4. When you change anything under an invariant, update both the code and the `*.LLM.md` in the same commit. The invariant at the top of this file is non-negotiable.
