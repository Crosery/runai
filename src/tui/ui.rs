use super::app::{App, InputMode, Tab};
use super::theme::Theme;
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
        InputMode::Help => render_help(f, &t),
        InputMode::RenameGroup => render_rename_dialog(f, app, &t),
        _ => {}
    }
}

fn render_header(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let border = Style::default().fg(t.border);

    let chunks = Layout::horizontal([Constraint::Min(0), Constraint::Length(32)]).split(area);

    // Left: tabs only, flush left
    let mut tab_spans = Vec::new();
    tab_spans.push(Span::raw(" "));
    for tab in Tab::ALL {
        if *tab == app.tab {
            tab_spans.push(Span::styled(
                format!("● {}", tab.label()),
                Style::default().fg(t.tab_active).bold(),
            ));
        } else {
            tab_spans.push(Span::styled(
                format!("  {}", tab.label()),
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
    let (es, ts, em, tm) = app.status;
    let target_name = app.active_target.name();
    let status = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("[{target_name}] "),
            Style::default().fg(t.brand).bold(),
        ),
        Span::styled(format!("{es}"), Style::default().fg(t.status_skills).bold()),
        Span::styled(format!("/{ts} skills  "), Style::default().fg(t.status_dim)),
        Span::styled(format!("{em}"), Style::default().fg(t.status_mcps).bold()),
        Span::styled(format!("/{tm} mcp "), Style::default().fg(t.status_dim)),
    ]))
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
        Tab::Dazi => render_dazi(f, app, t, area),
        _ => render_resources(f, app, t, area),
    }
}

fn render_resources(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let visible = app.visible_items();
    let items: Vec<ListItem> = visible
        .iter()
        .enumerate()
        .map(|(_, r)| {
            let enabled = r.is_enabled_for(app.active_target);
            let marker = if enabled { "●" } else { "○" };
            let marker_color = if enabled {
                t.item_enabled
            } else {
                t.item_disabled
            };

            let kind_color = match r.kind.as_str() {
                "skill" => t.item_kind,
                _ => t.item_kind_mcp,
            };

            let desc: String = r.description.chars().take(40).collect();

            let usage_str = if r.usage_count > 0 {
                format!(" {}x", r.usage_count)
            } else {
                String::new()
            };

            let mut spans = vec![
                Span::raw("  "),
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::raw("  "),
                Span::styled(
                    format!("{:6}", r.kind.as_str()),
                    Style::default().fg(kind_color),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<28}", r.name),
                    Style::default().fg(t.item_name).bold(),
                ),
                Span::styled(desc, Style::default().fg(t.item_desc)),
            ];
            if !usage_str.is_empty() {
                spans.push(Span::styled(usage_str, Style::default().fg(t.text_dim)));
            }

            let line = Line::from(spans);

            ListItem::new(line)
        })
        .collect();

    let title = format!(" {} ({}) ", app.tab.label(), visible.len());
    let list = List::new(items)
        .block(
            Block::default()
                .title(Span::styled(title, Style::default().fg(t.text).bold()))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
        )
        .highlight_style(Style::default().bg(t.item_selected_bg));

    let mut state = ListState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_groups(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let visible = app.visible_groups();
    let items: Vec<ListItem> = visible
        .iter()
        .enumerate()
        .map(|(_, (id, name, total, enabled))| {
            let all_on = *total > 0 && *enabled == *total;
            let partial = *enabled > 0 && *enabled < *total;
            let marker = if all_on {
                "●"
            } else if partial {
                "◐"
            } else {
                "○"
            };
            let marker_color = if all_on {
                t.tag_enabled
            } else if partial {
                t.tag_warning
            } else {
                t.item_disabled
            };

            let line = Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::raw("  "),
                Span::styled(
                    format!("{:<25}", name),
                    Style::default().fg(t.item_name).bold(),
                ),
                Span::styled(
                    format!("{enabled}/{total} enabled"),
                    Style::default().fg(if all_on { t.tag_enabled } else { t.text_dim }),
                ),
                Span::raw("    "),
                Span::styled(id, Style::default().fg(t.text_highlight)),
            ]);

            ListItem::new(line)
        })
        .collect();

    let title = format!(" Groups ({}) ", visible.len());
    let list = List::new(items)
        .block(
            Block::default()
                .title(Span::styled(title, Style::default().fg(t.text).bold()))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
        )
        .highlight_style(Style::default().bg(t.item_selected_bg));

    let mut state = ListState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_market(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let visible = app.visible_market();
    let enabled = app.enabled_sources();
    let total_enabled = enabled.len();
    let source = app.current_source();

    let items: Vec<ListItem> = visible
        .iter()
        .map(|s| {
            let marker = if s.installed { "✓" } else { " " };
            let marker_color = if s.installed {
                t.item_enabled
            } else {
                t.item_disabled
            };
            let name_color = if s.installed { t.text_dim } else { t.item_name };

            let line = Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::raw("  "),
                Span::styled(
                    format!("{:<35}", s.name),
                    Style::default().fg(name_color).bold(),
                ),
                Span::styled(&s.source_label, Style::default().fg(t.text_highlight)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title_text = if app.current_source_loading() {
        let label = source.map(|s| s.label.as_str()).unwrap_or("...");
        format!(" Market — Loading {label}... ")
    } else if let Some(src) = source {
        let custom_tag = if src.builtin { "" } else { " ★" };
        format!(
            " Market — {}{} ({}) [{}/{}] ◀ {} ▶ ",
            src.label,
            custom_tag,
            visible.len(),
            app.market_source_idx + 1,
            total_enabled,
            src.description
        )
    } else {
        " Market — No sources enabled (press 's') ".into()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(Span::styled(title_text, Style::default().fg(t.text).bold()))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
        )
        .highlight_style(Style::default().bg(t.item_selected_bg));

    let mut state = ListState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_dazi(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use crate::core::dazi::DaziKind;

    let kind_label = app.dazi_kind.label();
    let loading_indicator = if app.dazi_loading { " ⟳" } else { "" };

    let items: Vec<ListItem> = match app.dazi_kind {
        DaziKind::Skills => {
            let visible = app.visible_dazi_skills();
            visible
                .iter()
                .map(|s| {
                    let marker = if s.installed { "✓" } else { " " };
                    let marker_color = if s.installed {
                        t.item_enabled
                    } else {
                        t.item_disabled
                    };
                    let name_color = if s.installed { t.text_dim } else { t.item_name };
                    let official = if s.is_official { " ★" } else { "" };
                    let dl = if s.download_count > 0 {
                        format!(" ↓{}", s.download_count)
                    } else {
                        String::new()
                    };
                    let desc: String = if s.description.chars().count() > 40 {
                        let truncated: String = s.description.chars().take(40).collect();
                        format!("{truncated}…")
                    } else {
                        s.description.clone()
                    };

                    let line = Line::from(vec![
                        Span::raw("  "),
                        Span::styled(marker, Style::default().fg(marker_color)),
                        Span::raw("  "),
                        Span::styled(
                            format!("{:<28}", s.name),
                            Style::default().fg(name_color).bold(),
                        ),
                        Span::styled(
                            format!("{desc}{official}{dl}"),
                            Style::default().fg(t.text_dim),
                        ),
                    ]);
                    ListItem::new(line)
                })
                .collect()
        }
        DaziKind::Agents => {
            let visible = app.visible_dazi_agents();
            visible
                .iter()
                .map(|a| {
                    let marker = if a.installed { "✓" } else { " " };
                    let marker_color = if a.installed {
                        t.item_enabled
                    } else {
                        t.item_disabled
                    };
                    let name_color = if a.installed { t.text_dim } else { t.item_name };
                    let official = if a.is_official { " ★" } else { "" };
                    let dl = if a.download_count > 0 {
                        format!(" ↓{}", a.download_count)
                    } else {
                        String::new()
                    };
                    let display_name = if a.title.is_empty() {
                        &a.name
                    } else {
                        &a.title
                    };

                    let line = Line::from(vec![
                        Span::raw("  "),
                        Span::styled(marker, Style::default().fg(marker_color)),
                        Span::raw("  "),
                        Span::styled(
                            format!("{:<20}", a.name),
                            Style::default().fg(name_color).bold(),
                        ),
                        Span::styled(
                            format!("{display_name}{official}{dl}"),
                            Style::default().fg(t.text_highlight),
                        ),
                    ]);
                    ListItem::new(line)
                })
                .collect()
        }
        DaziKind::Bundles => {
            let visible = app.visible_dazi_bundles();
            visible
                .iter()
                .map(|b| {
                    let display_name = if b.source_team_name.is_empty() {
                        &b.name
                    } else {
                        &b.source_team_name
                    };
                    let official = if b.is_official { " ★" } else { "" };
                    let agents = b.agent_refs.len();
                    let skills = b.skill_refs.len();
                    let counts = format!(" [{agents}A+{skills}S]");

                    let line = Line::from(vec![
                        Span::raw("  📦  "),
                        Span::styled(
                            format!("{:<25}", display_name),
                            Style::default().fg(t.item_name).bold(),
                        ),
                        Span::styled(
                            format!("v{}{official}{counts}", b.version),
                            Style::default().fg(t.text_dim),
                        ),
                    ]);
                    ListItem::new(line)
                })
                .collect()
        }
    };

    let count = items.len();
    let title_text = format!(
        " 搭子 — {kind_label} ({count}){loading_indicator} ◀ [ ] ▶ "
    );

    let list = List::new(items)
        .block(
            Block::default()
                .title(Span::styled(title_text, Style::default().fg(t.text).bold()))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
        )
        .highlight_style(Style::default().bg(t.item_selected_bg));

    let mut state = ListState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_footer(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
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
                Tab::Groups => "j/k ↕  H/L tab  SPACE toggle  / search  t theme  ? help  q quit",
                Tab::Market => "j/k ↕  H/L tab  ENTER install  [ ] source  t theme  ? help  q quit",
                Tab::Dazi => "j/k ↕  H/L tab  ENTER install  [ ] 切换类型  / search  ? help  q quit",
                _ => "j/k ↕  H/L tab  SPACE toggle  / search  t theme  ? help  q quit",
            };
            (
                format!(
                    "{}{}",
                    search_info,
                    if msg.is_empty() {
                        String::new()
                    } else {
                        format!(" {} ", msg)
                    }
                ),
                help.to_string(),
            )
        }
        _ => (String::new(), String::new()),
    };

    let border = Style::default().fg(t.border);
    let version = env!("CARGO_PKG_VERSION");

    let brand_text = format!(" Runai v{version} ");
    let brand_len = brand_text.len() as u16 + 1;

    let footer_chunks =
        Layout::horizontal([Constraint::Length(brand_len), Constraint::Min(0)]).split(area);

    // Left: brand + version
    let brand = Paragraph::new(Line::from(vec![
        Span::styled(" Runai ", Style::default().fg(t.brand).bold()),
        Span::styled(
            format!("v{version} "),
            Style::default().fg(t.version).italic(),
        ),
    ]))
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

fn render_create_dialog(f: &mut Frame, app: &App, t: &Theme, step: u8) {
    let area = centered_rect(50, 30, f.area());
    f.render_widget(Clear, area);

    let prompt = if step == 0 {
        "Group Name:"
    } else {
        "Description (Enter to skip):"
    };
    let title = if step == 0 {
        " Create Group (1/2) "
    } else {
        " Create Group (2/2) "
    };

    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(t.brand).bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.text_highlight));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {prompt}"),
            Style::default().fg(t.item_desc),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(t.text).bold()),
            Span::styled("█", Style::default().fg(t.text_highlight)),
        ]),
        Line::from(""),
        styled_help("  ESC cancel  ENTER confirm", t),
    ];
    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

fn render_group_picker(f: &mut Frame, app: &App, t: &Theme) {
    let area = centered_rect(40, 50, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            " Add to Group ",
            Style::default().fg(t.brand).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.text_highlight));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let items: Vec<ListItem> = app
        .groups
        .iter()
        .enumerate()
        .map(|(i, (_, name, total, _))| {
            let is_sel = i == app.group_pick_idx;
            let line = Line::from(vec![
                Span::raw(if is_sel { " ▸ " } else { "   " }),
                Span::styled(name, Style::default().fg(t.item_name).bold()),
                Span::styled(
                    format!("  ({total} items)"),
                    Style::default().fg(t.text_dim),
                ),
            ]);
            let style = if is_sel {
                Style::default().bg(t.item_selected_bg)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();

    let help = Line::from(Span::styled(
        " j/k navigate  ENTER select  ESC cancel",
        Style::default().fg(t.text_dim),
    ));

    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(inner);

    let list = List::new(items);
    f.render_widget(list, chunks[0]);
    f.render_widget(Paragraph::new(help), chunks[1]);
}

fn render_install_dialog(f: &mut Frame, app: &App, t: &Theme) {
    let area = centered_rect(55, 25, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            " Install from GitHub ",
            Style::default().fg(t.brand).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.text_highlight));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Enter GitHub source (owner/repo or owner/repo@branch):",
            Style::default().fg(t.item_desc),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(t.text).bold()),
            Span::styled("█", Style::default().fg(t.text_highlight)),
        ]),
        Line::from(""),
        styled_help("  ESC cancel  ENTER install", t),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_group_detail(f: &mut Frame, app: &App, t: &Theme) {
    let area = centered_rect(65, 70, f.area());
    f.render_widget(Clear, area);

    let title = format!(
        " {} ({} skills) ",
        app.detail_group_name,
        app.detail_members.len()
    );
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(t.brand).bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.text_highlight));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(inner);

    if app.detail_members.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "  (empty) — press 'a' to add skills",
            Style::default().fg(t.text_dim),
        )));
        f.render_widget(empty, chunks[0]);
    } else {
        let items: Vec<ListItem> = app
            .detail_members
            .iter()
            .map(|r| {
                let enabled = r.is_enabled_for(app.active_target);
                let marker = if enabled { "●" } else { "○" };
                let marker_color = if enabled {
                    t.item_enabled
                } else {
                    t.item_disabled
                };

                let line = Line::from(vec![
                    Span::raw("  "),
                    Span::styled(marker, Style::default().fg(marker_color)),
                    Span::raw("  "),
                    Span::styled(&r.name, Style::default().fg(t.item_name).bold()),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items)
            .highlight_style(Style::default().bg(t.item_selected_bg))
            .highlight_symbol(" ▸");

        let mut state = ListState::default();
        state.select(Some(app.detail_idx));
        f.render_stateful_widget(list, chunks[0], &mut state);
    }

    let target_name = app.active_target.name();
    let mut help_spans = vec![Span::styled(
        format!(" [{target_name}] "),
        Style::default().fg(t.text_highlight).bold(),
    )];
    help_spans.extend(
        styled_help(
            "j/k navigate  SPACE toggle  a add  d remove  1234 CLI  ESC close",
            t,
        )
        .spans,
    );
    f.render_widget(Paragraph::new(Line::from(help_spans)), chunks[1]);
}

fn render_pick_skill(f: &mut Frame, app: &App, t: &Theme) {
    let area = centered_rect(60, 70, f.area());
    f.render_widget(Clear, area);

    let visible = app.visible_pick_items();
    let kind_label = if app.pick_show_mcp { "MCPs" } else { "Skills" };
    let title = format!(
        " Add {kind_label} to {} — {} available ",
        app.detail_group_name,
        visible.len()
    );
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(t.brand).bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.text_highlight));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(inner);

    // Search bar
    let search_line = if app.pick_search.is_empty() {
        Line::from(Span::styled(
            "  Type to filter...",
            Style::default().fg(t.text_dim),
        ))
    } else {
        Line::from(vec![
            Span::styled("  /", Style::default().fg(t.text_highlight)),
            Span::styled(&app.pick_search, Style::default().fg(t.text).bold()),
        ])
    };
    f.render_widget(Paragraph::new(search_line), chunks[0]);

    // Skill list with scroll
    let items: Vec<ListItem> = visible
        .iter()
        .map(|r| {
            let line = Line::from(vec![
                Span::raw("  "),
                Span::styled(&r.name, Style::default().fg(t.item_name).bold()),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(t.item_selected_bg))
        .highlight_symbol(" ▸ ");

    let mut state = ListState::default();
    state.select(Some(app.pick_idx));
    f.render_stateful_widget(list, chunks[1], &mut state);

    f.render_widget(
        Paragraph::new(styled_help(
            " j/k navigate  ENTER add  TAB skill/mcp  type search  BS clear  ESC back",
            t,
        )),
        chunks[2],
    );
}

fn render_source_manager(f: &mut Frame, app: &App, t: &Theme) {
    let area = centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            " Market Sources ",
            Style::default().fg(t.brand).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.text_highlight));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(inner);

    let items: Vec<ListItem> = app
        .sources
        .iter()
        .map(|src| {
            let marker = if src.enabled { "●" } else { "○" };
            let marker_color = if src.enabled {
                t.item_enabled
            } else {
                t.item_disabled
            };
            let tag = if src.builtin { "" } else { " ★" };

            let line = Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::raw("  "),
                Span::styled(
                    format!("{}{}", src.label, tag),
                    Style::default().fg(t.item_name).bold(),
                ),
                Span::raw("  "),
                Span::styled(&src.description, Style::default().fg(t.text_dim)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(t.item_selected_bg))
        .highlight_symbol(" ▸");

    let mut state = ListState::default();
    state.select(Some(app.source_pick_idx));
    f.render_stateful_widget(list, chunks[0], &mut state);

    f.render_widget(
        Paragraph::new(styled_help(
            " j/k navigate  SPACE toggle  a add  d delete  ESC close",
            t,
        )),
        chunks[1],
    );
}

fn render_add_source_dialog(f: &mut Frame, app: &App, t: &Theme) {
    let area = centered_rect(55, 25, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            " Add Market Source ",
            Style::default().fg(t.brand).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.text_highlight));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Enter GitHub repo (owner/repo or full URL):",
            Style::default().fg(t.item_desc),
        )),
        Line::from(Span::styled(
            "  e.g. anthropics/claude-code  or  owner/repo@branch",
            Style::default().fg(t.text_dim),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(t.text).bold()),
            Span::styled("█", Style::default().fg(t.text_highlight)),
        ]),
        Line::from(""),
        styled_help("  ESC cancel  ENTER add", t),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_first_launch(f: &mut Frame, app: &App, t: &Theme, step: u8) {
    let area = centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);

    match step {
        0 => {
            // Welcome
            let block = Block::default()
                .title(Span::styled(
                    " Welcome to Runai ",
                    Style::default().fg(t.brand).bold(),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.text_highlight));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  First time setup detected!",
                    Style::default().fg(t.text).bold(),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  This will:",
                    Style::default().fg(t.item_desc),
                )),
                Line::from(Span::styled(
                    "    • Scan all CLI directories for skills",
                    Style::default().fg(t.item_desc),
                )),
                Line::from(Span::styled(
                    "      (Claude, Codex, Gemini, OpenCode)",
                    Style::default().fg(t.text_dim),
                )),
                Line::from(Span::styled(
                    "    • Discover MCP servers from config files",
                    Style::default().fg(t.item_desc),
                )),
                Line::from(Span::styled(
                    "    • Offer smart auto-grouping",
                    Style::default().fg(t.item_desc),
                )),
                Line::from(""),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  ENTER ", Style::default().fg(t.tag_enabled).bold()),
                    Span::styled("start scan    ", Style::default().fg(t.item_desc)),
                    Span::styled("ESC ", Style::default().fg(t.tag_warning).bold()),
                    Span::styled("skip", Style::default().fg(t.item_desc)),
                ]),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
        1 => {
            // Scanning in progress
            let block = Block::default()
                .title(Span::styled(
                    " Scanning... ",
                    Style::default().fg(t.tag_warning).bold(),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.text_highlight));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let mut lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Scanning all skill directories and MCP configs...",
                    Style::default().fg(t.text).bold(),
                )),
                Line::from(""),
            ];
            for log_line in &app.scan_log {
                lines.push(Line::from(Span::styled(
                    format!("  {log_line}"),
                    Style::default().fg(t.item_desc),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  Please wait...",
                Style::default().fg(t.text_dim),
            )));
            f.render_widget(Paragraph::new(lines), inner);
        }
        2 => {
            // Scan done, show results + log
            let block = Block::default()
                .title(Span::styled(
                    " Scan Complete ",
                    Style::default().fg(t.tag_enabled).bold(),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.text_highlight));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let mut lines: Vec<Line> = vec![Line::from("")];

            // Show scan log
            for log_line in &app.scan_log {
                let color = if log_line.starts_with("  ✓") {
                    t.tag_enabled
                } else if log_line.starts_with("  ⚠") {
                    t.tag_warning
                } else if log_line.starts_with("Done") {
                    t.tag_enabled
                } else {
                    t.item_desc
                };
                lines.push(Line::from(Span::styled(
                    format!("  {log_line}"),
                    Style::default().fg(color),
                )));
            }
            lines.push(Line::from(""));

            if let Some(info) = &app.first_launch_info {
                lines.push(Line::from(vec![
                    Span::styled("  Skills found: ", Style::default().fg(t.item_desc)),
                    Span::styled(
                        format!("{}", info.skills_found),
                        Style::default().fg(t.status_skills).bold(),
                    ),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("  MCPs found:   ", Style::default().fg(t.item_desc)),
                    Span::styled(
                        format!("{}", info.mcps_found),
                        Style::default().fg(t.status_mcps).bold(),
                    ),
                ]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  Press any key to continue.",
                    Style::default().fg(t.text_dim),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    "  Scanning...",
                    Style::default().fg(t.item_desc),
                )));
            }

            f.render_widget(Paragraph::new(lines), inner);
        }
        _ => {}
    }
}

/// Turn "key1 desc1  key2 desc2" into styled spans: keys bold+colored, descs dim.
fn render_rename_dialog(f: &mut Frame, app: &App, t: &Theme) {
    let area = centered_rect(50, 25, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            " Rename Group ",
            Style::default().fg(t.brand).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.text_highlight));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  New name:",
            Style::default().fg(t.item_desc),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(t.text).bold()),
            Span::styled("█", Style::default().fg(t.text_highlight)),
        ]),
        Line::from(""),
        styled_help("  ESC cancel  ENTER confirm", t),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_help(f: &mut Frame, t: &Theme) {
    let area = centered_rect(55, 70, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            " Keybindings ",
            Style::default().fg(t.brand).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.text_highlight));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let ks = Style::default().fg(t.help_key).bold();
    let ds = Style::default().fg(t.item_desc);
    let ss = Style::default().fg(t.text_highlight).bold();

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled("  Navigation", ss)),
        Line::from(vec![
            Span::styled(" g/G     ", ks),
            Span::styled("Jump to top/bottom", ds),
        ]),
        Line::from(vec![
            Span::styled(" 1234    ", ks),
            Span::styled("Switch CLI target (Claude/Codex/Gemini/OpenCode)", ds),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Skills & MCPs", ss)),
        Line::from(vec![
            Span::styled(" Enter   ", ks),
            Span::styled("Open group detail / Install from market", ds),
        ]),
        Line::from(vec![
            Span::styled(" s       ", ks),
            Span::styled("Scan for new skills", ds),
        ]),
        Line::from(vec![
            Span::styled(" i       ", ks),
            Span::styled("Install from GitHub (owner/repo)", ds),
        ]),
        Line::from(vec![
            Span::styled(" d       ", ks),
            Span::styled("Delete selected skill or MCP", ds),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Groups", ss)),
        Line::from(vec![
            Span::styled(" c       ", ks),
            Span::styled("Create new group", ds),
        ]),
        Line::from(vec![
            Span::styled(" r       ", ks),
            Span::styled("Rename group (Groups tab)", ds),
        ]),
        Line::from(vec![
            Span::styled(" a       ", ks),
            Span::styled("Add selected to a group", ds),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Market", ss)),
        Line::from(vec![
            Span::styled(" [ ]     ", ks),
            Span::styled("Switch market source", ds),
        ]),
        Line::from(vec![
            Span::styled(" s       ", ks),
            Span::styled("Source manager (Market tab)", ds),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Press any key to close",
            Style::default().fg(t.text_dim),
        )),
    ];

    f.render_widget(Paragraph::new(lines), inner);
}

fn styled_help<'a>(text: &'a str, t: &Theme) -> Line<'a> {
    let key_style = Style::default().fg(t.help_key).bold();
    let desc_style = Style::default().fg(t.help_text);
    let mut spans = Vec::new();
    // Split by double-space to get "key desc" pairs
    for part in text.split("  ") {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
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
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}
