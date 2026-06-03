use super::super::app::{App, InputMode, Tab};
use super::super::i18n::T;
use super::super::theme::Theme;
use super::confirm::render_confirm_delete;
use super::dialogs::{
    render_add_source_dialog, render_create_dialog, render_group_detail, render_group_picker,
    render_help, render_install_dialog, render_pick_skill, render_rename_dialog,
    render_source_manager,
};
use super::first_launch::render_first_launch;
use super::helpers::styled_help;
use super::tabs::{render_groups, render_market, render_resources, render_trash};
use ratatui::prelude::*;
use ratatui::widgets::*;

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    let t = Theme::from_mode(app.theme_mode);

    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);

    render_header(f, app, &t, chunks[0]);
    render_body(f, app, &t, chunks[1]);
    render_footer(f, app, &t, chunks[2]);

    // Overlay dialogs
    match app.mode {
        InputMode::CreateGroup(step) => render_create_dialog(f, app, &t, step),
        InputMode::AddToGroup => render_group_picker(f, app, &t),
        InputMode::FirstLaunch(step) => render_first_launch(f, app, &t, step),
        InputMode::Install => render_install_dialog(f, app, &t),
        InputMode::AddSource => render_add_source_dialog(f, app, &t),
        InputMode::SourceManager => render_source_manager(f, app, &t),
        InputMode::GroupDetail => render_group_detail(f, app, &t),
        InputMode::PickSkillForGroup => render_pick_skill(f, app, &t),
        InputMode::Help => render_help(f, app, &t),
        InputMode::RenameGroup => render_rename_dialog(f, app, &t),
        InputMode::ConfirmDelete => render_confirm_delete(f, app, &t),
        _ => {}
    }
}

fn render_header(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let border = Style::default().fg(t.border);
    let i = T::new(app.lang);

    let chunks = Layout::horizontal([Constraint::Min(0), Constraint::Length(32)]).split(area);

    // Left: tabs only, flush left
    let tab_labels = [
        i.tab_skills(),
        i.tab_mcps(),
        i.tab_groups(),
        i.tab_market(),
        i.tab_trash(),
    ];
    let mut tab_spans = Vec::new();
    tab_spans.push(Span::raw(" "));
    for (tab, label) in Tab::ALL.iter().zip(tab_labels.iter()) {
        if *tab == app.tab {
            tab_spans.push(Span::styled(
                format!("● {}", label),
                Style::default().fg(t.tab_active).bold(),
            ));
        } else {
            tab_spans.push(Span::styled(
                format!("  {}", label),
                Style::default().fg(t.tab_inactive),
            ));
        }
        tab_spans.push(Span::raw("   "));
    }
    let tabs_widget = Paragraph::new(Line::from(tab_spans)).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(border),
    );
    f.render_widget(tabs_widget, chunks[0]);

    // Right: target + counts
    let status_line = if app.tab == Tab::Trash {
        Line::from(vec![
            Span::styled("[global] ", Style::default().fg(t.brand).bold()),
            Span::styled(
                format!("{}", app.trash_items.len()),
                Style::default().fg(t.tag_warning).bold(),
            ),
            Span::styled(
                format!(" {}", i.status_trash()),
                Style::default().fg(t.status_dim),
            ),
        ])
    } else {
        let (es, ts, em, tm) = app.status;
        let target_name = app.active_target.name();
        Line::from(vec![
            Span::styled(
                format!("[{target_name}] "),
                Style::default().fg(t.brand).bold(),
            ),
            Span::styled(format!("{es}"), Style::default().fg(t.status_skills).bold()),
            Span::styled(
                format!("/{ts} {}  ", i.status_skills()),
                Style::default().fg(t.status_dim),
            ),
            Span::styled(format!("{em}"), Style::default().fg(t.status_mcps).bold()),
            Span::styled(
                format!("/{tm} {}", i.status_mcp()),
                Style::default().fg(t.status_dim),
            ),
        ])
    };
    let status = Paragraph::new(status_line)
        .alignment(Alignment::Right)
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(border),
        );
    f.render_widget(status, chunks[1]);
}

fn render_body(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    match app.tab {
        Tab::Groups => render_groups(f, app, t, area),
        Tab::Market => render_market(f, app, t, area),
        Tab::Trash => render_trash(f, app, t, area),
        _ => render_resources(f, app, t, area),
    }
}

fn render_footer(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let i = T::new(app.lang);
    let (left, right) = match app.mode {
        InputMode::Search => (format!(" /{} ", app.search), i.help_search().to_string()),
        InputMode::Normal => {
            let search_info = if !app.search.is_empty() {
                format!(" filter: {} ", app.search)
            } else {
                String::new()
            };
            let help = match app.tab {
                Tab::Groups => i.help_normal_groups(),
                Tab::Market => i.help_normal_market(),
                Tab::Trash => i.help_normal_trash(),
                _ => i.help_normal_skills(),
            };
            (search_info, help.to_string())
        }
        _ => (String::new(), String::new()),
    };

    let border = Style::default().fg(t.border);
    let version = env!("CARGO_PKG_VERSION");

    // Check for pending update. Cache read is cheap; refreshes on every frame
    // so the background update check (written mid-TUI) surfaces without restart.
    let update_latest = crate::core::updater::pending_update_version(app.mgr.paths().data_dir());

    // Width estimate for the left chunk. `chars().count()` is a visual-width
    // approximation (good for ASCII, off by 1 col per CJK char — acceptable
    // since the only CJK here is the short i18n hint). The `● ` glyph counts
    // as 2 chars but renders 1–2 cols depending on the terminal; the +2
    // slack at the end prevents truncation of the last char.
    let hint = i.update_hint();
    let brand_text_len = if let Some(ref latest) = update_latest {
        format!(" Runai v{version}  ● v{latest} · {hint} ")
            .chars()
            .count()
    } else {
        format!(" Runai v{version} ").chars().count()
    };
    let brand_len = brand_text_len as u16 + 2;

    let footer_chunks =
        Layout::horizontal([Constraint::Length(brand_len), Constraint::Min(0)]).split(area);

    // Left: brand + version (+ update hint if any)
    let mut brand_spans = vec![
        Span::styled(" Runai ", Style::default().fg(t.brand).bold()),
        Span::styled(
            format!("v{version}"),
            Style::default().fg(t.version).italic(),
        ),
    ];
    if let Some(latest) = update_latest {
        // Design goal: look like a healthy "you can upgrade" hint, not a
        // shout. Amber signal-dot + bold version carries the alert; the
        // command hint is dim italic so the eye lands on the version first.
        //
        // Reference: gh CLI / cargo / starship all use a coloured marker +
        // bolded version number; the verbose "available" wording is dropped.
        brand_spans.push(Span::styled("  ● ", Style::default().fg(t.tag_warning)));
        brand_spans.push(Span::styled(
            format!("v{latest}"),
            Style::default().fg(t.tag_warning).bold(),
        ));
        brand_spans.push(Span::styled(
            format!(" · {hint}"),
            Style::default().fg(t.text_dim).italic(),
        ));
    } else {
        brand_spans.push(Span::raw(" "));
    }
    let brand = Paragraph::new(Line::from(brand_spans))
        .block(Block::default().borders(Borders::TOP).border_style(border));
    f.render_widget(brand, footer_chunks[0]);

    // Right: keybindings + messages
    let mut spans = vec![];
    if !left.is_empty() {
        spans.push(Span::styled(left, Style::default().fg(t.text_highlight)));
        spans.push(Span::raw("  "));
    }
    spans.extend(styled_help(&right, t).spans);

    let help_bar = Paragraph::new(Line::from(spans))
        .alignment(Alignment::Right)
        .block(Block::default().borders(Borders::TOP).border_style(border));
    f.render_widget(help_bar, footer_chunks[1]);
}
