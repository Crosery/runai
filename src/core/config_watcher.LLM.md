# config_watcher

## Purpose

Filesystem-event watcher that fires reload signals to the TUI whenever any of the 9 watched paths change. Replaces the v0.10.0 mtime-polling implementation in `tui/app.rs::poll_config_changes`, which used 4 hardcoded paths — two of which were wrong (`~/.codex/settings.json` instead of `~/.codex/config.toml`, `~/.opencode/settings.json` instead of `~/.config/opencode/opencode.json`), so Codex / OpenCode changes never refreshed the TUI.

## Watched paths

| Source | Path | Why |
|---|---|---|
| Claude MCP config | `~/.claude.json` | New / removed / disabled MCP entries → list refresh |
| Codex MCP config | `~/.codex/config.toml` | Same |
| Gemini MCP config | `~/.gemini/settings.json` | Same |
| OpenCode MCP config | `~/.config/opencode/opencode.json` | Same |
| Claude skills dir | `~/.claude/skills/` | New `<name>/SKILL.md` symlink → list refresh |
| Codex skills dir | `~/.codex/skills/` | Same |
| Gemini skills dir | `~/.gemini/skills/` | Same |
| OpenCode skills dir | `~/.opencode/skills/` | Same |
| runai backup dir | `~/.runai/mcps/` | Cross-shell disable/enable from another terminal → list refresh |

All paths come from `CliTarget::mcp_config_path()` / `CliTarget::skills_dir()` — single source of truth, no hand-coded duplicates. Missing paths are silently skipped (a CLI that isn't installed doesn't break the watcher).

## Architecture

`notify-debouncer-mini` wraps `RecommendedWatcher` (auto-selects FSEvents on macOS / inotify on Linux / ReadDirectoryChangesW on Windows). 200 ms debounce window collapses bursts (text editors firing 3-5 events per save). On any event in the window, the watcher's callback sends `()` on the user-supplied `mpsc::Sender`.

TUI's `run_tui` main loop holds the receiver and drains all pending signals before each redraw, collapsing N events into a single `App::reload()` call. The `ConfigWatcher` is held for the lifetime of the TUI; dropping it at function return tears down the watcher.

## Public API

| Item | Signature | Notes |
|---|---|---|
| `ConfigWatcher::start` | `fn(Sender<()>) -> Result<Self>` | Spawn watcher. Caller must hold the returned value. |
| `ConfigWatcher::watched` | `pub Vec<PathBuf>` | Paths actually registered (missing paths excluded). Useful for diagnostics. |
| `watch_targets` | `fn() -> Vec<PathBuf>` | The full intent list, regardless of which paths exist. |
| `is_watched` | `fn(&Path) -> bool` | Test helper. |

## Invariants

- Watcher is **read-only**: it never mutates filesystem state. Its only side effect is `Sender::send`.
- All paths use `NonRecursive` mode. Skills / mcps directories don't need recursion because the events fire on direct children (the `<name>/` directory creation is itself a child event).
- Receiver-side coalescing: TUI must drain the channel before reloading, never call `reload()` per event — N rapid changes collapse to 1 reload, not N.
- Watcher does NOT observe `~/.runai/runai.db` or skill content files (`SKILL.md` body changes). Adding those would re-render the TUI on every keystroke during scan / edit.

## Test coverage

`config_watcher::tests` (3 tests, ~50 ms total):
- `watch_targets_includes_four_cli_configs` — schema sanity, ensures all 4 `mcp_config_path()` values are listed.
- `watch_targets_includes_four_skill_dirs` — same for skills.
- `watcher_fires_on_file_modify` — physical e2e: writes a temp file, modifies it, asserts an event arrives within 1 s.

The TUI integration (drain channel → reload) is exercised by manual smoke testing — running `runai` in TUI, then editing `~/.claude.json` from another shell, observing the MCP list refresh within 200-300 ms.
