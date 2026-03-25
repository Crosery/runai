use ratatui::prelude::*;
use ratatui::widgets::*;
use super::app::{App, Tab, InputMode};

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();

    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ]).split(area);

    render_header(f, app, chunks[0]);
    render_body(f, app, chunks[1]);
    render_footer(f, app, chunks[2]);

    // Overlay dialogs
    match app.mode {
        InputMode::CreateGroup(step) => render_create_dialog(f, app, step),
        InputMode::AddToGroup => render_group_picker(f, app),
        InputMode::FirstLaunch(step) => render_first_launch(f, app, step),
        InputMode::Install => render_install_dialog(f, app),
        InputMode::AddSource => render_add_source_dialog(f, app),
        InputMode::SourceManager => render_source_manager(f, app),
        InputMode::GroupDetail => render_group_detail(f, app),
        InputMode::PickSkillForGroup => render_pick_skill(f, app),
        _ => {}
    }
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let border = Style::default().fg(Color::Rgb(40, 40, 50));

    let chunks = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(32),
    ]).split(area);

    // Left: tabs only, flush left
    let mut tab_spans = Vec::new();
    tab_spans.push(Span::raw(" "));
    for t in Tab::ALL {
        if *t == app.tab {
            tab_spans.push(Span::styled(format!("● {}", t.label()), Style::default().fg(Color::Rgb(56, 164, 252)).bold()));
        } else {
            tab_spans.push(Span::styled(format!("  {}", t.label()), Style::default().fg(Color::Gray)));
        }
        tab_spans.push(Span::raw("   "));
    }
    let tabs_widget = Paragraph::new(Line::from(tab_spans))
        .block(Block::default().borders(Borders::BOTTOM).border_style(border));
    f.render_widget(tabs_widget, chunks[0]);

    // Right: target + counts
    let (es, ts, em, tm) = app.status;
    let target_name = app.active_target.name();
    let status = Paragraph::new(Line::from(vec![
        Span::styled(format!("[{target_name}] "), Style::default().fg(Color::Rgb(232, 149, 74)).bold()),
        Span::styled(format!("{es}"), Style::default().fg(Color::Rgb(52, 211, 153)).bold()),
        Span::styled(format!("/{ts} skills  "), Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{em}"), Style::default().fg(Color::Rgb(129, 140, 248)).bold()),
        Span::styled(format!("/{tm} mcp "), Style::default().fg(Color::DarkGray)),
    ])).alignment(Alignment::Right)
       .block(Block::default().borders(Borders::BOTTOM).border_style(border));
    f.render_widget(status, chunks[1]);
}

fn render_body(f: &mut Frame, app: &App, area: Rect) {
    match app.tab {
        Tab::Groups => render_groups(f, app, area),
        Tab::Market => render_market(f, app, area),
        _ => render_resources(f, app, area),
    }
}

fn render_resources(f: &mut Frame, app: &App, area: Rect) {
    let visible = app.visible_items();
    let items: Vec<ListItem> = visible.iter().enumerate().map(|(_, r)| {
        let enabled = r.is_enabled_for(app.active_target);
        let marker = if enabled { "●" } else { "○" };
        let marker_color = if enabled { Color::Rgb(52, 211, 153) } else { Color::DarkGray };

        let kind_color = match r.kind.as_str() {
            "skill" => Color::Rgb(232, 149, 74),
            _ => Color::Rgb(129, 140, 248),
        };

        let desc: String = r.description.chars().take(50).collect();

        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(marker, Style::default().fg(marker_color)),
            Span::raw("  "),
            Span::styled(format!("{:6}", r.kind.as_str()), Style::default().fg(kind_color)),
            Span::raw(" "),
            Span::styled(format!("{:<30}", r.name), Style::default().fg(Color::White).bold()),
            Span::styled(desc, Style::default().fg(Color::DarkGray)),
        ]);

        ListItem::new(line)
    }).collect();

    let title = format!(" {} ({}) ", app.tab.label(), visible.len());
    let list = List::new(items)
        .block(Block::default()
            .title(Span::styled(title, Style::default().fg(Color::White).bold()))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(40, 40, 50))))
        .highlight_style(Style::default().bg(Color::Rgb(25, 25, 35)));

    let mut state = ListState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_groups(f: &mut Frame, app: &App, area: Rect) {
    let visible = app.visible_groups();
    let items: Vec<ListItem> = visible.iter().enumerate().map(|(_, (id, name, total, enabled))| {
        let all_on = *total > 0 && *enabled == *total;
        let partial = *enabled > 0 && *enabled < *total;
        let marker = if all_on { "●" } else if partial { "◐" } else { "○" };
        let marker_color = if all_on {
            Color::Rgb(52, 211, 153)
        } else if partial {
            Color::Rgb(251, 191, 36)
        } else {
            Color::DarkGray
        };

        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(marker, Style::default().fg(marker_color)),
            Span::raw("  "),
            Span::styled(format!("{:<25}", name), Style::default().fg(Color::White).bold()),
            Span::styled(
                format!("{enabled}/{total} enabled"),
                Style::default().fg(if all_on { Color::Rgb(52, 211, 153) } else { Color::DarkGray }),
            ),
            Span::raw("    "),
            Span::styled(id, Style::default().fg(Color::Rgb(56, 164, 252))),
        ]);

        ListItem::new(line)
    }).collect();

    let title = format!(" Groups ({}) ", visible.len());
    let list = List::new(items)
        .block(Block::default()
            .title(Span::styled(title, Style::default().fg(Color::White).bold()))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(40, 40, 50))))
        .highlight_style(Style::default().bg(Color::Rgb(25, 25, 35)));

    let mut state = ListState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_market(f: &mut Frame, app: &App, area: Rect) {
    let visible = app.visible_market();
    let enabled = app.enabled_sources();
    let total_enabled = enabled.len();
    let source = app.current_source();

    let items: Vec<ListItem> = visible.iter().map(|s| {
        let marker = if s.installed { "✓" } else { " " };
        let marker_color = if s.installed { Color::Rgb(52, 211, 153) } else { Color::DarkGray };
        let name_color = if s.installed { Color::DarkGray } else { Color::White };

        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(marker, Style::default().fg(marker_color)),
            Span::raw("  "),
            Span::styled(format!("{:<35}", s.name), Style::default().fg(name_color).bold()),
            Span::styled(&s.source_label, Style::default().fg(Color::Rgb(56, 164, 252))),
        ]);
        ListItem::new(line)
    }).collect();

    let title_text = if app.current_source_loading() {
        let label = source.map(|s| s.label.as_str()).unwrap_or("...");
        format!(" Market — Loading {label}... ")
    } else if let Some(src) = source {
        let custom_tag = if src.builtin { "" } else { " ★" };
        format!(" Market — {}{} ({}) [{}/{}] ◀ {} ▶ ",
            src.label, custom_tag, visible.len(),
            app.market_source_idx + 1, total_enabled,
            src.description)
    } else {
        " Market — No sources enabled (press 's') ".into()
    };

    let list = List::new(items)
        .block(Block::default()
            .title(Span::styled(title_text, Style::default().fg(Color::White).bold()))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(40, 40, 50))))
        .highlight_style(Style::default().bg(Color::Rgb(25, 25, 35)));

    let mut state = ListState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let (left, right) = match app.mode {
        InputMode::Search => (
            format!(" /{} ", app.search),
            "ESC cancel  ENTER confirm".to_string(),
        ),
        InputMode::Normal => {
            let msg = app.message.as_deref().unwrap_or("");
            let search_info = if !app.search.is_empty() {
                format!(" filter: {} ", app.search)
            } else {
                String::new()
            };
            let help = match app.tab {
                Tab::Groups => "j/k ↕  H/L tab  SPACE toggle  1234 CLI  c create  d delete  i install  / search  s scan  q quit",
                Tab::Market => "j/k ↕  H/L tab  ENTER install  [ ] source  s sources  / search  q quit",
                _ => "j/k ↕  H/L tab  SPACE toggle  1234 CLI  a group  d delete  i install  / search  s scan  q quit",
            };
            (
                format!("{}{}", search_info, if msg.is_empty() { String::new() } else { format!(" {} ", msg) }),
                help.to_string(),
            )
        }
        _ => (String::new(), String::new()),
    };

    let border = Style::default().fg(Color::Rgb(40, 40, 50));
    let version = env!("CARGO_PKG_VERSION");

    let brand_text = format!(" Skill Manager v{version} ");
    let brand_len = brand_text.len() as u16 + 1;

    let footer_chunks = Layout::horizontal([
        Constraint::Length(brand_len),
        Constraint::Min(0),
    ]).split(area);

    // Left: brand + version
    let brand = Paragraph::new(Line::from(vec![
        Span::styled(" Skill Manager ", Style::default().fg(Color::Rgb(232, 149, 74)).bold()),
        Span::styled(format!("v{version} "), Style::default().fg(Color::Rgb(120, 120, 140)).italic()),
    ])).block(Block::default().borders(Borders::TOP).border_style(border));
    f.render_widget(brand, footer_chunks[0]);

    // Right: keybindings + messages
    let mut spans = vec![];
    if !left.is_empty() {
        spans.push(Span::styled(left, Style::default().fg(Color::Rgb(56, 164, 252))));
        spans.push(Span::raw("  "));
    }
    spans.extend(styled_help(&right).spans);

    let help_bar = Paragraph::new(Line::from(spans))
        .alignment(Alignment::Right)
        .block(Block::default().borders(Borders::TOP).border_style(border));
    f.render_widget(help_bar, footer_chunks[1]);
}

fn render_create_dialog(f: &mut Frame, app: &App, step: u8) {
    let area = centered_rect(50, 30, f.area());
    f.render_widget(Clear, area);

    let prompt = if step == 0 { "Group Name:" } else { "Description (Enter to skip):" };
    let title = if step == 0 { " Create Group (1/2) " } else { " Create Group (2/2) " };

    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(Color::Rgb(232, 149, 74)).bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(56, 164, 252)));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!("  {prompt}"), Style::default().fg(Color::Gray))),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(Color::White).bold()),
            Span::styled("█", Style::default().fg(Color::Rgb(56, 164, 252))),
        ]),
        Line::from(""),
        styled_help("  ESC cancel  ENTER confirm"),
    ];
    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

fn render_group_picker(f: &mut Frame, app: &App) {
    let area = centered_rect(40, 50, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(" Add to Group ", Style::default().fg(Color::Rgb(232, 149, 74)).bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(56, 164, 252)));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let items: Vec<ListItem> = app.groups.iter().enumerate().map(|(i, (_, name, total, _))| {
        let is_sel = i == app.group_pick_idx;
        let line = Line::from(vec![
            Span::raw(if is_sel { " ▸ " } else { "   " }),
            Span::styled(name, Style::default().fg(Color::White).bold()),
            Span::styled(format!("  ({total} items)"), Style::default().fg(Color::DarkGray)),
        ]);
        let style = if is_sel { Style::default().bg(Color::Rgb(25, 25, 35)) } else { Style::default() };
        ListItem::new(line).style(style)
    }).collect();

    let help = Line::from(Span::styled(
        " j/k navigate  ENTER select  ESC cancel",
        Style::default().fg(Color::DarkGray),
    ));

    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(2),
    ]).split(inner);

    let list = List::new(items);
    f.render_widget(list, chunks[0]);
    f.render_widget(Paragraph::new(help), chunks[1]);
}

fn render_install_dialog(f: &mut Frame, app: &App) {
    let area = centered_rect(55, 25, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(" Install from GitHub ", Style::default().fg(Color::Rgb(232, 149, 74)).bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(56, 164, 252)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled("  Enter GitHub source (owner/repo or owner/repo@branch):", Style::default().fg(Color::Gray))),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(Color::White).bold()),
            Span::styled("█", Style::default().fg(Color::Rgb(56, 164, 252))),
        ]),
        Line::from(""),
        styled_help("  ESC cancel  ENTER install"),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_group_detail(f: &mut Frame, app: &App) {
    let area = centered_rect(65, 70, f.area());
    f.render_widget(Clear, area);

    let title = format!(" {} ({} skills) ", app.detail_group_name, app.detail_members.len());
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(Color::Rgb(232, 149, 74)).bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(56, 164, 252)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(2),
    ]).split(inner);

    if app.detail_members.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "  (empty) — press 'a' to add skills",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(empty, chunks[0]);
    } else {
        let items: Vec<ListItem> = app.detail_members.iter().map(|r| {
            let enabled = r.is_enabled_for(app.active_target);
            let marker = if enabled { "●" } else { "○" };
            let marker_color = if enabled { Color::Rgb(52, 211, 153) } else { Color::DarkGray };

            let line = Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::raw("  "),
                Span::styled(&r.name, Style::default().fg(Color::White).bold()),
            ]);
            ListItem::new(line)
        }).collect();

        let list = List::new(items)
            .highlight_style(Style::default().bg(Color::Rgb(25, 25, 35)))
            .highlight_symbol(" ▸");

        let mut state = ListState::default();
        state.select(Some(app.detail_idx));
        f.render_stateful_widget(list, chunks[0], &mut state);
    }

    let target_name = app.active_target.name();
    let mut help_spans = vec![
        Span::styled(format!(" [{target_name}] "), Style::default().fg(Color::Rgb(56, 164, 252)).bold()),
    ];
    help_spans.extend(styled_help("j/k navigate  SPACE toggle  a add  d remove  1234 CLI  ESC close").spans);
    f.render_widget(Paragraph::new(Line::from(help_spans)), chunks[1]);
}

fn render_pick_skill(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 70, f.area());
    f.render_widget(Clear, area);

    let visible = app.visible_pick_items();
    let kind_label = if app.pick_show_mcp { "MCPs" } else { "Skills" };
    let title = format!(" Add {kind_label} to {} — {} available ", app.detail_group_name, visible.len());
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(Color::Rgb(232, 149, 74)).bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(56, 164, 252)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ]).split(inner);

    // Search bar
    let search_line = if app.pick_search.is_empty() {
        Line::from(Span::styled("  Type to filter...", Style::default().fg(Color::DarkGray)))
    } else {
        Line::from(vec![
            Span::styled("  /", Style::default().fg(Color::Rgb(56, 164, 252))),
            Span::styled(&app.pick_search, Style::default().fg(Color::White).bold()),
        ])
    };
    f.render_widget(Paragraph::new(search_line), chunks[0]);

    // Skill list with scroll
    let items: Vec<ListItem> = visible.iter().map(|r| {
        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(&r.name, Style::default().fg(Color::White).bold()),
        ]);
        ListItem::new(line)
    }).collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Rgb(25, 25, 35)))
        .highlight_symbol(" ▸ ");

    let mut state = ListState::default();
    state.select(Some(app.pick_idx));
    f.render_stateful_widget(list, chunks[1], &mut state);

    f.render_widget(Paragraph::new(styled_help(" j/k navigate  ENTER add  TAB skill/mcp  type search  BS clear  ESC back")), chunks[2]);
}

fn render_source_manager(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(" Market Sources ", Style::default().fg(Color::Rgb(232, 149, 74)).bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(56, 164, 252)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(2),
    ]).split(inner);

    let items: Vec<ListItem> = app.sources.iter().map(|src| {
        let marker = if src.enabled { "●" } else { "○" };
        let marker_color = if src.enabled { Color::Rgb(52, 211, 153) } else { Color::DarkGray };
        let tag = if src.builtin { "" } else { " ★" };

        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(marker, Style::default().fg(marker_color)),
            Span::raw("  "),
            Span::styled(format!("{}{}", src.label, tag), Style::default().fg(Color::White).bold()),
            Span::raw("  "),
            Span::styled(&src.description, Style::default().fg(Color::DarkGray)),
        ]);
        ListItem::new(line)
    }).collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Rgb(25, 25, 35)))
        .highlight_symbol(" ▸");

    let mut state = ListState::default();
    state.select(Some(app.source_pick_idx));
    f.render_stateful_widget(list, chunks[0], &mut state);

    f.render_widget(Paragraph::new(styled_help(" j/k navigate  SPACE toggle  a add  d delete  ESC close")), chunks[1]);
}

fn render_add_source_dialog(f: &mut Frame, app: &App) {
    let area = centered_rect(55, 25, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(" Add Market Source ", Style::default().fg(Color::Rgb(232, 149, 74)).bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(56, 164, 252)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled("  Enter GitHub repo (owner/repo or full URL):", Style::default().fg(Color::Gray))),
        Line::from(Span::styled("  e.g. anthropics/claude-code  or  owner/repo@branch", Style::default().fg(Color::DarkGray))),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(Color::White).bold()),
            Span::styled("█", Style::default().fg(Color::Rgb(56, 164, 252))),
        ]),
        Line::from(""),
        styled_help("  ESC cancel  ENTER add"),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_first_launch(f: &mut Frame, app: &App, step: u8) {
    let area = centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);

    match step {
        0 => {
            // Welcome
            let block = Block::default()
                .title(Span::styled(" Welcome to Skill Manager ", Style::default().fg(Color::Rgb(232, 149, 74)).bold()))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(56, 164, 252)));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let lines = vec![
                Line::from(""),
                Line::from(Span::styled("  First time setup detected!", Style::default().fg(Color::White).bold())),
                Line::from(""),
                Line::from(Span::styled("  This will:", Style::default().fg(Color::Gray))),
                Line::from(Span::styled("    • Scan all CLI directories for skills", Style::default().fg(Color::Gray))),
                Line::from(Span::styled("      (Claude, Codex, Gemini, OpenCode)", Style::default().fg(Color::DarkGray))),
                Line::from(Span::styled("    • Discover MCP servers from config files", Style::default().fg(Color::Gray))),
                Line::from(Span::styled("    • Offer smart auto-grouping", Style::default().fg(Color::Gray))),
                Line::from(""),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  ENTER ", Style::default().fg(Color::Rgb(52, 211, 153)).bold()),
                    Span::styled("start scan    ", Style::default().fg(Color::Gray)),
                    Span::styled("ESC ", Style::default().fg(Color::Rgb(251, 191, 36)).bold()),
                    Span::styled("skip", Style::default().fg(Color::Gray)),
                ]),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
        1 => {
            // Scanning in progress
            let block = Block::default()
                .title(Span::styled(" Scanning... ", Style::default().fg(Color::Rgb(251, 191, 36)).bold()))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(56, 164, 252)));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let mut lines = vec![
                Line::from(""),
                Line::from(Span::styled("  Scanning all skill directories and MCP configs...", Style::default().fg(Color::White).bold())),
                Line::from(""),
            ];
            for log_line in &app.scan_log {
                lines.push(Line::from(Span::styled(format!("  {log_line}"), Style::default().fg(Color::Gray))));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("  Please wait...", Style::default().fg(Color::DarkGray))));
            f.render_widget(Paragraph::new(lines), inner);
        }
        2 => {
            // Scan done, show results + log
            let block = Block::default()
                .title(Span::styled(" Scan Complete ", Style::default().fg(Color::Rgb(52, 211, 153)).bold()))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(56, 164, 252)));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let mut lines: Vec<Line> = vec![Line::from("")];

            // Show scan log
            for log_line in &app.scan_log {
                let color = if log_line.starts_with("  ✓") {
                    Color::Rgb(52, 211, 153)
                } else if log_line.starts_with("  ⚠") {
                    Color::Rgb(251, 191, 36)
                } else if log_line.starts_with("Done") {
                    Color::Rgb(52, 211, 153)
                } else {
                    Color::Gray
                };
                lines.push(Line::from(Span::styled(format!("  {log_line}"), Style::default().fg(color))));
            }
            lines.push(Line::from(""));

            if let Some(info) = &app.first_launch_info {
                lines.push(Line::from(vec![
                    Span::styled("  Skills found: ", Style::default().fg(Color::Gray)),
                    Span::styled(format!("{}", info.skills_found), Style::default().fg(Color::Rgb(52, 211, 153)).bold()),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("  MCPs found:   ", Style::default().fg(Color::Gray)),
                    Span::styled(format!("{}", info.mcps_found), Style::default().fg(Color::Rgb(129, 140, 248)).bold()),
                ]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("  Press any key to continue.", Style::default().fg(Color::DarkGray))));
            } else {
                lines.push(Line::from(Span::styled("  Scanning...", Style::default().fg(Color::Gray))));
            }

            f.render_widget(Paragraph::new(lines), inner);
        }
        _ => {}
    }
}

/// Turn "key1 desc1  key2 desc2" into styled spans: keys bold+colored, descs dim.
fn styled_help(text: &str) -> Line<'_> {
    let key_style = Style::default().fg(Color::Rgb(232, 149, 74)).bold();
    let desc_style = Style::default().fg(Color::DarkGray);
    let mut spans = Vec::new();
    // Split by double-space to get "key desc" pairs
    for part in text.split("  ") {
        let part = part.trim();
        if part.is_empty() { continue; }
        if !spans.is_empty() {
            spans.push(Span::styled("  ", desc_style));
        }
        // First word is key, rest is description
        if let Some(idx) = part.find(' ') {
            spans.push(Span::styled(&part[..idx], key_style));
            spans.push(Span::styled(&part[idx..], desc_style));
        } else {
            spans.push(Span::styled(part, key_style));
        }
    }
    Line::from(spans)
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ]).split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ]).split(popup_layout[1])[1]
}
