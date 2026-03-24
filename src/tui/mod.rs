pub mod app;
pub mod ui;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    execute,
};
use ratatui::prelude::*;
use std::io;
use std::time::Duration;

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
        app.prefetch_market();
    }

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

        // Poll async market loading
        app.poll_market();

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }
                match key.code {
                    KeyCode::Char('q') if !app.is_blocking_quit() => break,
                    _ => app.handle_key(key),
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
