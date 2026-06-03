use super::super::theme::Theme;
use ratatui::prelude::*;

/// A 7-cell heat bar as a `Line`, ready to drop into a `Table` cell.
/// - `count == 0` → empty bar (7 spaces, preserves column alignment).
/// - `count > 0`  → `filled = round(count/max * 7)` (min 1); filled cells are `▇`
///   in the heat palette bucket picked by the same ratio, unfilled cells are spaces.
pub(super) fn heat_bar_line(count: u64, max: u64, t: &Theme) -> Line<'static> {
    if count == 0 || max == 0 {
        return Line::from("       ");
    }
    let ratio = (count as f64 / max as f64).clamp(0.0, 1.0);
    let filled = (((ratio * 7.0).round()) as usize).clamp(1, 7);
    // Bucket 0..=4 — nudge so ratio=0.2 lands in bucket 1, not bucket 0.
    let bucket = ((ratio * 4.999) as usize).min(4);
    let color = t.heat[bucket];
    let bar: String = "▇".repeat(filled) + &" ".repeat(7 - filled);
    Line::from(Span::styled(bar, Style::default().fg(color)))
}

pub(super) fn styled_help<'a>(text: &'a str, t: &Theme) -> Line<'a> {
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

pub(super) fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
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
