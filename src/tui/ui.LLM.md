---
module: tui::ui
file: src/tui/ui.rs
role: tui-render
---

# tui::ui

## Purpose
Pure rendering. Takes `&App` + `&mut Frame`, draws the current tab, modal dialogs, footer, search bar. No state mutation, no I/O.

## Public API (internal)
- `draw(frame, app)` — top-level entry; dispatches to per-tab draw functions.
- Per-tab: `render_resources` (Skills + MCPs), `render_groups`, `render_market`, `render_trash`. `render_groups` renders each group as a 1- or 2-line `ListItem`: header line (marker + display-name + enabled/total + id), plus a dim 120-char description preview line when `description` is non-empty. Empty descriptions stay 1-line to keep dense groups scannable.
- Widgets: `draw_footer`, `draw_help_overlay`, `draw_search_bar`, `draw_install_modal`, `draw_confirm_delete`, etc.

## Key invariants
- **No mutation**: must not change `app` state. Any condition that feels like "I want to store this in App for next frame" belongs in `tui::app`, computed before render.
- Each tab fills `frame.size()` minus a shared header/footer — layout uses `ratatui::layout::{Layout, Constraint}` consistently.
- Trash is rendered as a dedicated global table, and the header swaps the per-target status summary for a global trash count when that tab is active.
- Colors are always looked up via `tui::theme`, never hardcoded — dark/light theme switch is a one-call operation.

## Touch points
- **Upstream**: `tui::mod::run_tui` calls `terminal.draw(|f| ui::draw(f, &app))` each tick.
- **Downstream**: `tui::theme` for styles, `tui::i18n` for every user-visible string, `ratatui` widget library.

## Gotchas
- Every string displayed to the user should go through `i18n` — hardcoded English strings break Chinese users and vice versa.
- Long-list rendering uses `ratatui::widgets::List` with `state`; the state (scroll offset, selection) lives in `App`, not here.
- Help overlay is a modal drawn **last** so it occludes the tab — preserve that draw-order if refactoring.
- Delete confirmation renders from `app.pending_delete` only; it describes the exact impact for resource, group, group member, and market-source removal without performing any mutation.
