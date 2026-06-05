use super::super::app::App;
use super::super::i18n::T;
use super::super::theme::Theme;
use super::helpers::{centered_rect, styled_help};
use ratatui::prelude::*;
use ratatui::widgets::*;

pub(super) fn render_create_dialog(f: &mut Frame, app: &App, t: &Theme, step: u8) {
    let i = T::new(app.lang);
    let area = centered_rect(50, 30, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            i.title_create_group(step),
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
            format!("  {}", i.create_group_prompt(step)),
            Style::default().fg(t.item_desc),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(t.text).bold()),
            Span::styled("█", Style::default().fg(t.text_highlight)),
        ]),
        Line::from(""),
        styled_help(i.help_dialog(), t),
    ];
    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

pub(super) fn render_group_picker(f: &mut Frame, app: &App, t: &Theme) {
    let i = T::new(app.lang);
    let area = centered_rect(40, 50, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            i.title_add_to_group(),
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
        .map(|(i, (_, name, total, _, _))| {
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
        i.help_group_picker(),
        Style::default().fg(t.text_dim),
    ));

    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(inner);

    let list = List::new(items);
    f.render_widget(list, chunks[0]);
    f.render_widget(Paragraph::new(help), chunks[1]);
}

pub(super) fn render_install_dialog(f: &mut Frame, app: &App, t: &Theme) {
    let i = T::new(app.lang);
    let area = centered_rect(55, 25, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            i.title_install(),
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
            i.install_prompt(),
            Style::default().fg(t.item_desc),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(t.text).bold()),
            Span::styled("█", Style::default().fg(t.text_highlight)),
        ]),
        Line::from(""),
        styled_help(i.help_install(), t),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

pub(super) fn render_group_detail(f: &mut Frame, app: &App, t: &Theme) {
    let i = T::new(app.lang);
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
            i.group_empty(),
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
    help_spans.extend(styled_help(i.help_group_detail(), t).spans);
    f.render_widget(Paragraph::new(Line::from(help_spans)), chunks[1]);
}

pub(super) fn render_pick_skill(f: &mut Frame, app: &App, t: &Theme) {
    let i = T::new(app.lang);
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
            i.pick_filter_hint(),
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
        Paragraph::new(styled_help(i.help_pick_skill(), t)),
        chunks[2],
    );
}

/// Community upload picker overlay (PLANNING §1.5).
///
/// Lists local skill candidates scanned by `App::scan_upload_candidates`
/// (`~/.claude/skills/` + cwd `.claude/skills/`). User uses j/k or arrows
/// to move, Enter to upload, Esc/q to cancel, r to rescan. Footer shows
/// the last upload message (success or error). When `upload_busy` is
/// true the picker shows a "uploading …" line and all keys are ignored
/// (see `handle_community_upload_picker_key`).
pub(super) fn render_community_upload_picker(f: &mut Frame, app: &App, t: &Theme) {
    let area = centered_rect(60, 70, f.area());
    f.render_widget(Clear, area);

    let title = format!(
        " Upload skill to community — {} candidate(s) ",
        app.upload_candidates.len()
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
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .split(inner);

    let header = Line::from(Span::styled(
        "  Scan: ~/.claude/skills + cwd/.claude/skills",
        Style::default().fg(t.text_dim),
    ));
    f.render_widget(Paragraph::new(header), chunks[0]);

    if app.upload_candidates.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "  no candidates found — create a skill dir with a SKILL.md first",
            Style::default().fg(t.text_dim),
        )));
        f.render_widget(empty, chunks[1]);
    } else {
        let items: Vec<ListItem> = app
            .upload_candidates
            .iter()
            .map(|c| {
                let line = Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("[{:>7}]", c.source.short_label()),
                        Style::default().fg(t.text_dim),
                    ),
                    Span::raw(" "),
                    Span::styled(&c.name, Style::default().fg(t.item_name).bold()),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items)
            .highlight_style(Style::default().bg(t.item_selected_bg))
            .highlight_symbol(" ▸ ");

        let mut state = ListState::default();
        state.select(Some(app.upload_idx));
        f.render_stateful_widget(list, chunks[1], &mut state);
    }

    let msg_style = if app.upload_busy {
        Style::default().fg(t.text_highlight).bold()
    } else if app.upload_message.starts_with("upload failed") {
        Style::default().fg(t.tag_warning)
    } else {
        Style::default().fg(t.text_dim)
    };
    let msg = Paragraph::new(Line::from(Span::styled(
        format!("  {}", app.upload_message),
        msg_style,
    )));
    f.render_widget(msg, chunks[2]);

    let help = Line::from(vec![
        Span::styled("  j/k", Style::default().fg(t.text_highlight)),
        Span::styled(" move  ", Style::default().fg(t.text_dim)),
        Span::styled("Enter", Style::default().fg(t.text_highlight)),
        Span::styled(" upload  ", Style::default().fg(t.text_dim)),
        Span::styled("r", Style::default().fg(t.text_highlight)),
        Span::styled(" rescan  ", Style::default().fg(t.text_dim)),
        Span::styled("Esc/q", Style::default().fg(t.text_highlight)),
        Span::styled(" close", Style::default().fg(t.text_dim)),
    ]);
    f.render_widget(Paragraph::new(help), chunks[3]);
}

pub(super) fn render_source_manager(f: &mut Frame, app: &App, t: &Theme) {
    let i = T::new(app.lang);
    let area = centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            i.title_sources(),
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

    f.render_widget(Paragraph::new(styled_help(i.help_sources(), t)), chunks[1]);
}

pub(super) fn render_add_source_dialog(f: &mut Frame, app: &App, t: &Theme) {
    let i = T::new(app.lang);
    let area = centered_rect(55, 25, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            i.title_add_source(),
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
            i.add_source_prompt(),
            Style::default().fg(t.item_desc),
        )),
        Line::from(Span::styled(
            i.add_source_example(),
            Style::default().fg(t.text_dim),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(t.text).bold()),
            Span::styled("█", Style::default().fg(t.text_highlight)),
        ]),
        Line::from(""),
        styled_help(i.help_add_source(), t),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

/// Turn "key1 desc1  key2 desc2" into styled spans: keys bold+colored, descs dim.
pub(super) fn render_rename_dialog(f: &mut Frame, app: &App, t: &Theme) {
    let i = T::new(app.lang);
    let area = centered_rect(50, 25, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            i.title_rename_group(),
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
            i.rename_prompt(),
            Style::default().fg(t.item_desc),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  > "),
            Span::styled(&app.input_buf, Style::default().fg(t.text).bold()),
            Span::styled("█", Style::default().fg(t.text_highlight)),
        ]),
        Line::from(""),
        styled_help(i.help_dialog(), t),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

pub(super) fn render_help(f: &mut Frame, app: &App, t: &Theme) {
    let i = T::new(app.lang);
    let area = centered_rect(55, 70, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            i.title_keybindings(),
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
        Line::from(Span::styled(i.help_section_nav(), ss)),
        Line::from(vec![
            Span::styled(" g/G     ", ks),
            Span::styled(i.help_g(), ds),
        ]),
        Line::from(vec![
            Span::styled(" 1234    ", ks),
            Span::styled(i.help_1234(), ds),
        ]),
        Line::from(vec![
            Span::styled(" f       ", ks),
            Span::styled(i.help_f(), ds),
        ]),
        Line::from(""),
        Line::from(Span::styled(i.help_section_skills(), ss)),
        Line::from(vec![
            Span::styled(" Enter   ", ks),
            Span::styled(i.help_enter(), ds),
        ]),
        Line::from(vec![
            Span::styled(" s       ", ks),
            Span::styled(i.help_s(), ds),
        ]),
        Line::from(vec![
            Span::styled(" i       ", ks),
            Span::styled(i.help_i(), ds),
        ]),
        Line::from(vec![
            Span::styled(" d       ", ks),
            Span::styled(i.help_d(), ds),
        ]),
        Line::from(""),
        Line::from(Span::styled(i.help_section_groups(), ss)),
        Line::from(vec![
            Span::styled(" c       ", ks),
            Span::styled(i.help_c(), ds),
        ]),
        Line::from(vec![
            Span::styled(" r       ", ks),
            Span::styled(i.help_r(), ds),
        ]),
        Line::from(vec![
            Span::styled(" a       ", ks),
            Span::styled(i.help_a(), ds),
        ]),
        Line::from(""),
        Line::from(Span::styled(i.help_section_market(), ss)),
        Line::from(vec![
            Span::styled(" [ ]     ", ks),
            Span::styled(i.help_brackets(), ds),
        ]),
        Line::from(vec![
            Span::styled(" s       ", ks),
            Span::styled(i.help_s_market(), ds),
        ]),
        Line::from(""),
        Line::from(Span::styled(i.help_section_trash(), ss)),
        Line::from(vec![
            Span::styled(" r       ", ks),
            Span::styled(i.help_r_trash(), ds),
        ]),
        Line::from(vec![
            Span::styled(" D       ", ks),
            Span::styled(i.help_d_trash(), ds),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            i.help_close(),
            Style::default().fg(t.text_dim),
        )),
    ];

    f.render_widget(Paragraph::new(lines), inner);
}
