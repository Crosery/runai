use super::super::app::{App, PendingDelete};
use super::super::i18n::T;
use super::super::theme::Theme;
use super::helpers::centered_rect;
use ratatui::prelude::*;
use ratatui::widgets::*;

pub(super) fn render_confirm_delete(f: &mut Frame, app: &App, t: &Theme) {
    let i = T::new(app.lang);
    let area = centered_rect(58, 26, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            i.title_confirm_delete(),
            Style::default().fg(t.tag_warning).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.tag_warning));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let (summary, impact, caution) = match app.pending_delete.as_ref() {
        Some(PendingDelete::Resource { name, kind, .. }) => (
            i.confirm_delete_resource(name, kind),
            i.confirm_delete_impact_resource(),
            i.confirm_trash_recoverable(),
        ),
        Some(PendingDelete::Group { name, .. }) => (
            i.confirm_delete_group(name),
            i.confirm_delete_impact_group(),
            i.confirm_delete_irreversible(),
        ),
        Some(PendingDelete::GroupMember {
            group_name,
            resource_name,
            ..
        }) => (
            i.confirm_remove_group_member(resource_name, group_name),
            i.confirm_delete_impact_group_member(),
            i.confirm_delete_irreversible(),
        ),
        Some(PendingDelete::Source { label, .. }) => (
            i.confirm_delete_source(label),
            i.confirm_delete_impact_source(),
            i.confirm_delete_irreversible(),
        ),
        None => (String::new(), "", ""),
    };

    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .split(inner);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(summary, Style::default().fg(t.text).bold())),
        Line::from(""),
        Line::from(Span::styled(impact, Style::default().fg(t.item_desc))),
        Line::from(Span::styled(caution, Style::default().fg(t.tag_warning))),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        chunks[0],
    );

    let button_chunks =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[2]);
    let no = format!(" {}  {} ", "ESC", i.confirm_no());
    let yes = format!(" {}  {} ", "ENTER", i.confirm_yes());
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            no,
            Style::default().fg(t.text_dim).bold(),
        )))
        .alignment(Alignment::Center),
        button_chunks[0],
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            yes,
            Style::default().fg(t.tag_warning).bold(),
        )))
        .alignment(Alignment::Center),
        button_chunks[1],
    );
}
