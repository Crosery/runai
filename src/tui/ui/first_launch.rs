use super::super::app::App;
use super::super::i18n::T;
use super::super::theme::Theme;
use super::helpers::centered_rect;
use ratatui::prelude::*;
use ratatui::widgets::*;

pub(super) fn render_first_launch(f: &mut Frame, app: &App, t: &Theme, step: u8) {
    let i = T::new(app.lang);
    let area = centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);

    match step {
        0 => {
            let block = Block::default()
                .title(Span::styled(
                    i.title_welcome(),
                    Style::default().fg(t.brand).bold(),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.text_highlight));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let (k1, d1, k2, d2) = i.welcome_keys();
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    i.welcome_detected(),
                    Style::default().fg(t.text).bold(),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    i.welcome_will(),
                    Style::default().fg(t.item_desc),
                )),
                Line::from(Span::styled(
                    i.welcome_scan_dirs(),
                    Style::default().fg(t.item_desc),
                )),
                Line::from(Span::styled(
                    i.welcome_scan_dirs2(),
                    Style::default().fg(t.text_dim),
                )),
                Line::from(Span::styled(
                    i.welcome_discover_mcp(),
                    Style::default().fg(t.item_desc),
                )),
                Line::from(Span::styled(
                    i.welcome_auto_group(),
                    Style::default().fg(t.item_desc),
                )),
                Line::from(""),
                Line::from(""),
                Line::from(vec![
                    Span::styled(k1, Style::default().fg(t.tag_enabled).bold()),
                    Span::styled(d1, Style::default().fg(t.item_desc)),
                    Span::styled(k2, Style::default().fg(t.tag_warning).bold()),
                    Span::styled(d2, Style::default().fg(t.item_desc)),
                ]),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
        1 => {
            let block = Block::default()
                .title(Span::styled(
                    i.title_scanning(),
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
                    i.scanning_msg(),
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
                i.scanning_wait(),
                Style::default().fg(t.text_dim),
            )));
            f.render_widget(Paragraph::new(lines), inner);
        }
        2 => {
            let block = Block::default()
                .title(Span::styled(
                    i.title_scan_done(),
                    Style::default().fg(t.tag_enabled).bold(),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.text_highlight));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let mut lines: Vec<Line> = vec![Line::from("")];
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
                    Span::styled(i.scan_skills_found(), Style::default().fg(t.item_desc)),
                    Span::styled(
                        format!("{}", info.skills_found),
                        Style::default().fg(t.status_skills).bold(),
                    ),
                ]));
                lines.push(Line::from(vec![
                    Span::styled(i.scan_mcps_found(), Style::default().fg(t.item_desc)),
                    Span::styled(
                        format!("{}", info.mcps_found),
                        Style::default().fg(t.status_mcps).bold(),
                    ),
                ]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    i.scan_continue(),
                    Style::default().fg(t.text_dim),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    i.scan_in_progress(),
                    Style::default().fg(t.item_desc),
                )));
            }

            f.render_widget(Paragraph::new(lines), inner);
        }
        _ => {}
    }
}
