---
module: tui::app
file: src/tui/app.rs
role: tui-state-machine
---

# tui::app

## Purpose
The TUI state machine. Owns the `SkillManager`, the currently-selected tab / row / filter, pending modal dialogs, the event-loop dispatch, and the hooks that call into `manager` when the user presses a key. Paired with `tui::ui` (pure rendering) through a shared `App` struct.

## Public API (internal, non-`pub` outside the crate)
- `struct App` — all TUI state: active tab, list indices, search filter, theme, pending modals, async task handles, and a reference to `SkillManager`.
- Event dispatch entry: `App::on_key(key)` (or similar) — called from `tui::mod::run_tui` on each `crossterm::Event::Key`.
- Action handlers: `on_toggle_enable`, `on_install_from_market`, `on_switch_target`, `on_theme_toggle`, `on_search`, etc.
- `PendingDelete` + `InputMode::ConfirmDelete` — destructive `d` actions are staged first and only execute after Enter in the confirmation dialog.

## Key invariants
- **Rendering is pure**: `tui::ui::draw(&App, frame)` must not mutate state. All mutation goes through `App` methods.
- **Blocking ops are off the UI thread**: long-running I/O (market refresh, install download) is spawned with `std::thread` or `tokio::task::spawn_blocking`, results come back via channels and are applied in the next tick.
- **Tabs**: Skills / MCPs / Groups / Market (4 tabs; `Tab::ALL` is the source of truth).
- Target switching via digit keys: `1`=Claude, `2`=Codex, `3`=Gemini, `4`=OpenCode. Matches `CliTarget::ALL` ordering.
- Delete/remove shortcuts must not mutate disk or DB directly. They populate `pending_delete` and switch to `ConfirmDelete`; Esc cancels, Enter performs the stored action.

## Touch points
- **Upstream**: `tui::run_tui` (entry from `cli::mod`).
- **Downstream**: `SkillManager` (all business ops), `tui::ui` (render), `tui::theme` (style lookup), `tui::i18n` (labels).

## Gotchas
- Don't store `&SkillManager` — own it (`manager: SkillManager`). TUI is the process's last stop; there's nothing else holding the manager.
- After terminal teardown (alternate-screen exit), `main.rs` prints update-available notification via `eprintln`. Don't print from inside the TUI loop after `disable_raw_mode` or you'll corrupt the next prompt.
