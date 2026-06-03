---
module: tui::ui
file: src/tui/ui/
role: tui-render
---

# tui::ui

> Sibling to `src/tui/ui/`. One-liner: pure rendering of every TUI tab, dialog, and chrome from `&App`.

## Purpose
Pure rendering. Takes `&App` + `&mut Frame`, draws the current tab, modal dialogs, footer, search bar. No state mutation, no I/O.

## Public surface (stable — external code depends on these paths)
- `crate::tui::ui::render(frame, app)` — the only public item; top-level entry, re-exported from `mod.rs` (lives in `layout.rs`). `tui::mod::run_tui` calls it as `ui::render(f, &app)` each tick. Everything else in the module is `pub(super)` / private and not part of the API.

## Submodule map
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports only, no logic | `pub use layout::render;` |
| `layout.rs` | frame entry + chrome + tab dispatch | `render` (pub entry), `render_header`, `render_body`, `render_footer` |
| `tabs.rs` | per-tab body renders | `render_resources` (Skills + MCPs), `render_groups`, `render_trash`, `render_market` |
| `dialogs.rs` | modal/overlay dialogs | `render_create_dialog`, `render_group_picker`, `render_install_dialog`, `render_group_detail`, `render_pick_skill`, `render_source_manager`, `render_add_source_dialog`, `render_rename_dialog`, `render_help` |
| `first_launch.rs` | first-launch welcome / scanning / scan-done steps | `render_first_launch` |
| `confirm.rs` | delete-confirmation modal | `render_confirm_delete` |
| `helpers.rs` | shared private render helpers | `heat_bar_line`, `styled_help`, `centered_rect` |

`render_groups` renders each group as a 1- or 2-line `ListItem`: header line (marker + display-name + enabled/total + id), plus a dim 120-char description preview line when `description` is non-empty. Empty descriptions stay 1-line to keep dense groups scannable.

## Key invariants
- **No mutation**: must not change `app` state. Any condition that feels like "I want to store this in App for next frame" belongs in `tui::app`, computed before render.
- Each tab fills `frame.area()` minus a shared header/footer — layout uses `ratatui::layout::{Layout, Constraint}` consistently.
- Trash is rendered as a dedicated global table, and the header swaps the per-target status summary for a global trash count when that tab is active.
- Colors are always looked up via `tui::theme`, never hardcoded — dark/light theme switch is a one-call operation.
- **Public path frozen**: only `render` is re-exported; the per-tab/dialog fns stay `pub(super)` so the split is invisible to `tui::mod`.

## Cross-module dependencies
- `crate::tui::app::{App, InputMode, PendingDelete, Tab, FilterMode}` — read-only state the renders consume (imported in submodules as `super::super::app::*`).
- `crate::tui::i18n::T` — every user-visible string.
- `crate::tui::theme::Theme` — all colors/styles.
- `crate::core::resource::format_time_ago` (trash rows), `crate::core::updater::pending_update_version` (footer update hint).
- `ratatui` widget library.

## Gotchas / where bodies are buried
- Shared private helpers (`heat_bar_line`, `styled_help`, `centered_rect`) live only in `helpers.rs`; siblings import them via `use super::helpers::{...}`. Never copy one into another file.
- Every string displayed to the user should go through `i18n` — hardcoded English strings break Chinese users and vice versa.
- Long-list rendering uses `ratatui::widgets::List` with `state`; the state (scroll offset, selection) lives in `App`, not here.
- Help overlay (and all dialogs) is drawn **last** in `layout::render`'s `match app.mode` so it occludes the tab — preserve that draw-order if refactoring.
- Delete confirmation (`confirm.rs`) renders from `app.pending_delete` only; it describes the exact impact for resource, group, group member, and market-source removal without performing any mutation.
- `dialogs.rs::render_rename_dialog` carries a pre-existing stray `///` doc comment ("Turn key1 desc1...") that actually describes `styled_help`; left in place to keep the move byte-faithful.

## Tests
- No `#[cfg(test)]` in this module — rendering is exercised indirectly. The TUI key/handler tests live in `tui::app::tests` (unix-gated).
