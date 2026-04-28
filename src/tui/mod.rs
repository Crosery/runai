pub mod app;
pub mod i18n;
pub mod theme;
pub mod ui;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;
use std::io;
use std::sync::mpsc;
use std::time::Duration;

use crate::core::config_watcher::ConfigWatcher;
use crate::core::manager::SkillManager;
use app::{App, InputMode};

pub fn run_tui(mgr: SkillManager) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(mgr);
    app.reload();
    if app.mode == InputMode::Normal {
        // Auto-scan if no skills found (e.g. MCP registered but skills not scanned yet)
        if app.items.is_empty() {
            let _ = app.mgr.scan();
            app.reload();
        }
        app.prefetch_market();
    }

    // Live filesystem watcher: events from the 4 CLI MCP configs / skills dirs /
    // runai mcps backup dir trigger a reload before the next redraw. Held for the
    // lifetime of the TUI; dropping at function return stops the watcher.
    let (watch_tx, watch_rx) = mpsc::channel::<()>();
    let _watcher = ConfigWatcher::start(watch_tx).ok();

    loop {
        terminal.draw(|f| ui::render(f, &app))?;

        // If in scanning state, auto-trigger scan after rendering the loading screen
        if app.mode == InputMode::FirstLaunch(1) {
            // Brief pause so the "Scanning..." frame is visible
            std::thread::sleep(Duration::from_millis(50));
            app.do_first_launch_scan();
            app.mode = InputMode::FirstLaunch(2);
            continue; // re-render immediately with results
        }

        // Drain watcher events: any pending fs change collapses to a single reload.
        let mut should_reload = false;
        while watch_rx.try_recv().is_ok() {
            should_reload = true;
        }
        if should_reload {
            app.reload();
        }

        // Poll async market loading
        app.poll_market();

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            // Windows crossterm delivers both Press and Release (and Repeat)
            // events; macOS/Linux only deliver Press by default. Without
            // this filter, every Windows keystroke fires actions twice —
            // Tab / H / L navigation "jumping" is the most visible symptom.
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                break;
            }
            match key.code {
                KeyCode::Char('q') if !app.is_blocking_quit() => break,
                _ => app.handle_key(key),
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
