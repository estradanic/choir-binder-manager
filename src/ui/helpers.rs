use anyhow::Error;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::models::Binder;

/// Repeat a short ASCII motif until it fills the requested width.
pub(crate) fn repeat_pattern_row(row: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if row.is_empty() {
        return " ".repeat(width);
    }
    let repeat_count = width / row.len() + 2;
    let mut repeated = row.repeat(repeat_count);
    repeated.truncate(width);
    repeated
}

/// Render the binder label centered inside square brackets.
pub(crate) fn binder_label_line(label: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return " ".repeat(width);
    }
    let mut decorated = format!("[ {} ]", trimmed);
    if decorated.len() > width {
        decorated.truncate(width);
    }
    let padding = width.saturating_sub(decorated.len());
    let left = padding / 2;
    let right = padding - left;
    let mut line = String::with_capacity(width);
    line.push_str(&" ".repeat(left));
    line.push_str(&decorated);
    line.push_str(&" ".repeat(right));
    if line.len() < width {
        line.push_str(&" ".repeat(width - line.len()));
    } else if line.len() > width {
        line.truncate(width);
    }
    line
}

/// Build the textual payload for a binder card, mixing the repeating pattern
/// with an optional bold highlight when the card is selected.
pub(crate) fn build_binder_cover_lines(
    binder: &Binder,
    pattern: &[&str],
    inner_width: u16,
    inner_height: u16,
    selected: bool,
) -> Vec<Line<'static>> {
    let width = inner_width as usize;
    let height = inner_height as usize;
    if width == 0 || height == 0 {
        return vec![Line::from("")];
    }

    let mut lines = Vec::with_capacity(height);
    let pattern_rows = pattern.len();
    let label_lines = if height >= 2 { 2 } else { 1 };
    let pattern_height = height.saturating_sub(label_lines);
    let pattern_style = if selected {
        Style::default().fg(Color::Gray)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    if pattern_rows == 0 {
        for _ in 0..pattern_height {
            lines.push(Line::from(vec![Span::styled(
                " ".repeat(width),
                pattern_style,
            )]));
        }
    } else {
        for row_idx in 0..pattern_height {
            let base = pattern[row_idx % pattern_rows];
            let row = repeat_pattern_row(base, width);
            lines.push(Line::from(vec![Span::styled(row, pattern_style)]));
        }
    }

    if height >= 2 {
        lines.push(Line::from(vec![Span::styled(
            " ".repeat(width),
            pattern_style,
        )]));
    }

    let label_content = binder_label_line(&binder.label, width);
    if selected {
        lines.push(Line::from(vec![Span::styled(
            label_content,
            Style::default().add_modifier(Modifier::BOLD),
        )]));
    } else {
        lines.push(Line::from(label_content));
    }

    while lines.len() < height {
        lines.push(Line::from(vec![Span::styled(
            " ".repeat(width),
            pattern_style,
        )]));
    }

    lines
}

/// Produce a rectangle centered within `area` that spans the requested percent
/// of the width and height. Used for modal dialogs.
pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(area);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(horizontal[1]);

    vertical[1]
}

/// Extract the most relevant error message from a chained error.
pub(crate) fn surface_error(err: &Error) -> String {
    err.chain()
        .last()
        .map(|cause| cause.to_string())
        .unwrap_or_else(|| err.to_string())
}
