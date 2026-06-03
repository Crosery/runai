use super::super::app::{App, Tab};
use super::super::i18n::T;
use super::super::theme::Theme;
use super::helpers::heat_bar_line;
use ratatui::prelude::*;
use ratatui::widgets::*;

pub(super) fn render_resources(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let i = T::new(app.lang);
    let visible = app.visible_items();
    let max_usage = app.max_usage_count;

    // Three-column layout:
    //   Col 0 — marker + kind + name (fixed 40 cols)
    //   Col 1 — description (fills remaining width; truncated by Table)
    //   Col 2 — 7-cell heat bar (fixed)
    // Using a Table (not List) because we need the bar right-aligned and
    // the description to expand into the remaining width; a List can only
    // flow left-to-right with space padding that breaks on CJK width.
    let rows: Vec<Row> = visible
        .iter()
        .map(|r| {
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

            let prefix = Line::from(vec![
                Span::raw(" "),
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::raw("  "),
                Span::styled(
                    format!("{:6}", r.kind.as_str()),
                    Style::default().fg(kind_color),
                ),
                Span::raw(" "),
                Span::styled(r.name.clone(), Style::default().fg(t.item_name).bold()),
            ]);

            let desc = Span::styled(r.description.clone(), Style::default().fg(t.item_desc));

            let bar = heat_bar_line(r.usage_count, max_usage, t);

            Row::new(vec![
                Cell::from(prefix),
                Cell::from(Line::from(desc)),
                Cell::from(bar),
            ])
        })
        .collect();

    let tab_label = match app.tab {
        Tab::Skills => i.tab_skills(),
        Tab::Mcps => i.tab_mcps(),
        _ => app.tab.label(),
    };
    let filter_label = if app.filter_mode != super::super::app::FilterMode::All {
        let fl = match app.filter_mode {
            super::super::app::FilterMode::Enabled => i.filter_enabled(),
            super::super::app::FilterMode::Disabled => i.filter_disabled(),
            super::super::app::FilterMode::All => "",
        };
        format!(" [{}]", fl)
    } else {
        String::new()
    };
    let title = format!(" {}{} ({}) ", tab_label, filter_label, visible.len());
    let widths = [
        Constraint::Length(40),
        Constraint::Min(10),
        Constraint::Length(7),
    ];
    let table = Table::new(rows, widths)
        .block(
            Block::default()
                .title(Span::styled(title, Style::default().fg(t.text).bold()))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
        )
        .row_highlight_style(Style::default().bg(t.item_selected_bg));

    let mut state = TableState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(table, area, &mut state);
}

pub(super) fn render_groups(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let i = T::new(app.lang);
    let visible = app.visible_groups();
    let items: Vec<ListItem> = visible
        .iter()
        .map(|(id, name, total, enabled, description)| {
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

            let header = Line::from(vec![
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

            if description.is_empty() {
                ListItem::new(header)
            } else {
                let desc_text: String = description.chars().take(120).collect();
                let desc_line = Line::from(vec![
                    Span::raw("       "),
                    Span::styled(desc_text, Style::default().fg(t.text_dim)),
                ]);
                ListItem::new(vec![header, desc_line])
            }
        })
        .collect();

    let title = format!(" {} ({}) ", i.title_groups(), visible.len());
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

pub(super) fn render_trash(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let i = T::new(app.lang);
    let visible = app.visible_trash();

    let rows: Vec<Row> = visible
        .iter()
        .map(|entry| {
            let kind_color = match entry.kind.as_str() {
                "skill" => t.item_kind,
                _ => t.item_kind_mcp,
            };
            let deleted_ago = crate::core::resource::format_time_ago(Some(entry.deleted_at));
            let mut scope_parts: Vec<String> = entry
                .enabled_targets
                .iter()
                .map(|target| target.name().to_string())
                .collect();
            if entry.disabled_backup.is_some() {
                scope_parts.push(i.trash_scope_backup().into());
            }
            if !entry.group_ids.is_empty() {
                scope_parts.push(i.trash_groups_suffix(entry.group_ids.len()));
            }
            if scope_parts.is_empty() {
                scope_parts.push(i.trash_scope_global().into());
            }

            let name = Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!("{:6}", entry.kind.as_str()),
                    Style::default().fg(kind_color),
                ),
                Span::raw(" "),
                Span::styled(entry.name.clone(), Style::default().fg(t.item_name).bold()),
            ]);

            Row::new(vec![
                Cell::from(name),
                Cell::from(Line::from(Span::styled(
                    deleted_ago,
                    Style::default().fg(t.text_highlight),
                ))),
                Cell::from(Line::from(Span::styled(
                    scope_parts.join(", "),
                    Style::default().fg(t.item_desc),
                ))),
            ])
        })
        .collect();

    let title = format!(" {} ({}) ", i.title_trash(), visible.len());
    let widths = [
        Constraint::Length(40),
        Constraint::Length(12),
        Constraint::Min(10),
    ];
    let table = Table::new(rows, widths)
        .block(
            Block::default()
                .title(Span::styled(title, Style::default().fg(t.text).bold()))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
        )
        .row_highlight_style(Style::default().bg(t.item_selected_bg));

    let mut state = TableState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(table, area, &mut state);
}

pub(super) fn render_market(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let i = T::new(app.lang);
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
        format!(
            " {} — {} {label}... ",
            i.tab_market(),
            i.title_market_loading()
        )
    } else if let Some(src) = source {
        let custom_tag = if src.builtin { "" } else { " ★" };
        format!(
            " {} — {}{} ({}) [{}/{}] ◀ {} ▶ ",
            i.tab_market(),
            src.label,
            custom_tag,
            visible.len(),
            app.market_source_idx + 1,
            total_enabled,
            src.description
        )
    } else {
        i.title_market_no_source().to_string()
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
