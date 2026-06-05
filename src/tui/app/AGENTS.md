---
module: tui::app
file: src/tui/app/
role: tui-state-machine
---

# tui::app

> This folder (`src/tui/app/`). One-liner: the TUI single-threaded state machine — owns `SkillManager`, dispatches keys, drives reloads.

## Purpose
The TUI state machine. Owns the `SkillManager`, the currently-selected tab / row / filter, pending modal dialogs, the event-loop dispatch, and the hooks that call into `manager` when the user presses a key. Paired with `tui::ui` (pure rendering) through a shared `App` struct.

## Public surface (the API contract — external code depends on these exact paths)
All re-exported from `mod.rs`; consumers (`tui::mod`, `tui::ui`) keep using `tui::app::X`.
- `crate::tui::app::App` — all TUI state + the impl methods (`new`, `reload`, `prefetch_market`, `poll_market`, `handle_key`, `do_first_launch_scan`, `is_blocking_quit`, the `visible_*`/`enabled_sources`/`current_source*` query helpers, `t()`).
- `crate::tui::app::Tab` — the 7 tabs (`Tab::ALL` is the source of truth) + `label()`. Order: Skills → MCPs → Groups → Market → Community → Hooks → Trash.
- `crate::tui::app::FilterMode` — All/Enabled/Disabled + `next()`/`label()`.
- `crate::tui::app::InputMode` — the modal/dialog state enum (Normal, Search, ConfirmDelete, …).
- `crate::tui::app::PendingDelete` — staged destructive action (Resource/Group/GroupMember/Source).
- `crate::tui::app::FirstLaunchInfo` — first-launch scan summary counts.
- `crate::tui::app::HookSlot` — per-CLI hook install row used by the Hooks tab (`supported` / `installed`); `HookSlot::supported(bool)` / `HookSlot::unsupported()` constructors.
- `crate::tui::app::CommunitySkill` — one row from `GET /api/community/list`, used by the Community tab. Fields match the server JSON shape (`name`, `uploader_uid`, `uploader_username`, `version`, `installs_total`, `created_at`).

## Submodule map
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports only, no logic | `pub use model::{App, CommunitySkill, FilterMode, FirstLaunchInfo, HookSlot, InputMode, PendingDelete, Tab}` |
| `model.rs` | state types + enums + `App` struct + `FirstLaunchInfo` | `Tab`, `FilterMode`, `InputMode`, `PendingDelete` (+`return_mode`), `App`, `FirstLaunchInfo`, `HookSlot`, `CommunitySkill` |
| `init.rs` | `App::new` + query/visibility helpers + `t()` | `new`, `t`, `is_blocking_quit`, `visible_items/groups/trash/market/pick_items/community`, `visible_count`, `enabled_sources`, `current_source`, `is_market_loading`, `current_source_loading` |
| `data.rs` | reload + async market fetch/poll | `reload` (now tab-aware: lazy-loads `reload_hook_status` / `reload_community` on Hooks / Community), `prefetch_market`, `poll_market` |
| `keybindings.rs` | `handle_key` dispatch + per-input-mode handlers | `handle_key`, `handle_search_key`, `handle_create_group_key`, `handle_add_to_group_key`, `handle_install_key`, `handle_first_launch_key`, `handle_rename_group_key`, `handle_confirm_delete_key`, `handle_source_manager_key`, `handle_add_source_key` |
| `normal_actions.rs` | Normal-mode key dispatch + enable/disable toggle | `handle_normal_key` (`pub(super)`; routes to `handle_hooks_key` first when on Hooks tab), `toggle_selected` |
| `resource_ops.rs` | single-item delete/restore/purge (trash-first) | `confirm_delete_selected_resource`, `delete_pending_resource`, `restore_selected_trash`, `purge_selected_trash`, `confirm_delete_selected_group`, `delete_pending_group` (all `pub(super)`) |
| `market_ops.rs` | install selected skill from market | `install_market_selected`, `install_from_market` (both `pub(super)`) |
| `group_detail.rs` | group-detail overlay + pick-items flow | `open_group_detail`, `reload_group_detail`, `handle_group_detail_key`, `handle_pick_skill_key` (`pub(super)`), `load_pick_items` |
| `first_launch.rs` | one-shot first-launch scan + MCP discovery/registration | `do_first_launch_scan` |
| `hook_panel.rs` | Hooks tab state + keys (PLANNING §1.5) | `reload_hook_status`, `handle_hooks_key` (`pub(super)`); shells out to `recommend::install_claude_hook` / `uninstall_claude_hook` so the TUI writes the same `~/.claude/settings.json` shape as `runai recommend install-hook` |
| `market_tab.rs` | Community tab data fetch (PLANNING §1.5 + §1.4) | `reload_community` (blocking `GET /api/community/list` to local dashboard, 3s timeout); `COMMUNITY_PORT = 17888` matches the auto-spawned dashboard port |
| `tests.rs` | unix-gated key/handler tests | `delete_key_*`, `enter_confirms_*`, `source_delete_requires_confirmation` |

## Invariants (load-bearing — do not break silently)
- **`mod.rs` is thin**: declarations + the single `pub use` line only. No logic.
- **Public path frozen**: `App`/`Tab`/`FilterMode`/`InputMode`/`PendingDelete`/`FirstLaunchInfo` stay reachable as `tui::app::X` — the split is invisible to `tui::mod` and `tui::ui`.
- **`impl App` spans submodules**: cross-submodule private methods are `pub(super)` (normal_actions/resource_ops/market_ops/group_detail handlers called from `keybindings`/`normal_actions`; `PendingDelete::return_mode`). Methods only called within their own file stay private.
- **Rendering is pure**: `tui::ui::draw(&App, frame)` must not mutate state. All mutation goes through `App` methods.
- **Blocking ops are off the UI thread**: market refresh spawns a `std::thread` with its own tokio `Runtime` (`prefetch_market`); results arrive via `mpsc` channels and are drained in `poll_market`. Synchronous installs (`install_*`, `handle_install_key`) build a throwaway `Runtime` per call.
- **Tabs**: Skills / MCPs / Groups / Market / Community / Hooks / Trash — `Tab::ALL` is the source of truth. Hooks (PLANNING §1.5) is a 4-row matrix of CLI targets where only Claude is currently `HookSlot::supported`; the other three rows render as `unsupported` placeholders. Community (PLANNING §1.5 + §1.4) reads `GET /api/community/list` from the local dashboard (owner mode returns 404, team mode returns rows) — same datasource as the dashboard's community pane.
- Trash is a **global** view; it ignores `active_target` for listing but restores with the targets captured at delete time.
- Target switching via digit keys: `1`=Claude, `2`=Codex, `3`=Gemini, `4`=OpenCode — matches `CliTarget::ALL` ordering.
- **Delete = trash-first, confirm-first**: destructive shortcuts populate `pending_delete` and switch to `ConfirmDelete`; Esc cancels, Enter performs the stored action via `handle_confirm_delete_key`. They never mutate disk/DB directly.

## Cross-module dependencies
- `crate::core::manager::SkillManager` — owned by `App`; every business op (enable/disable/scan/trash/groups/install) routes through it.
- `crate::core::market::{Market, MarketSkill, SourceEntry, load_sources/save_sources/load_cache/save_cache/find_skill_in_sources}` — market tab + source manager.
- `crate::core::installer::Installer` — GitHub `i`-key install path.
- `crate::core::resource::{Resource, TrashEntry, ResourceKind}` — list/filter state.
- `crate::core::transcript_stats::scan_default` — usage-count overlay in `reload`.
- `crate::core::mcp_discovery` / `crate::core::mcp_register` — first-launch scan.
- `crate::tui::{theme, i18n}` — `ThemeMode` (`super::super::theme` from submodules) + `T`/`Lang`.

## Gotchas / where bodies are buried
- `App.theme_mode` is `super::super::theme::ThemeMode` from inside the submodules (the extra `super` is the dir-level shift; in the old single file it was `super::theme`).
- Don't store `&SkillManager` — own it (`mgr: SkillManager`). The TUI is the process's last stop.
- `App.groups` is a 5-tuple `(id, name, total, enabled, description)`; `description` is read in `reload()` so `render_groups` shows the preview without re-querying. Every destructuring must use the 5-tuple shape.
- `prefetch_market`'s spawned worker opens its own tokio `Runtime` and writes the disk cache; `poll_market` is the only place that moves results into `market_cache` + clears `market_fetching`/`market_rxs`.
- After terminal teardown, `main.rs` prints the update notice via `eprintln`; never print from inside the TUI loop after `disable_raw_mode`.

## Tests
- `tests.rs` (`#[cfg(all(test, not(target_os = "windows")))]`) — confirm-delete staging for resources and sources; HOME mocking via `HOME_LOCK` + `with_home` (unix-only). Windows skips this module (HOME mocking unsupported — see AGENTS.md Key constraints).
