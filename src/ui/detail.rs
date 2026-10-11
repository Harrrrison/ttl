//! Rendering the detail pane.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::detail::{self, Row};
use crate::model::Task;
use crate::ui::{self, theme};

/// Gutter width for the `label` column.
const LABEL: usize = 12;

/// Turn one detail row into styled display lines.
fn render_row(row: &Row, focused_attachment: usize) -> Vec<Line<'static>> {
    match row {
        Row::Blank => vec![Line::from("")],
        Row::Heading(h) => vec![Line::from(Span::styled(
            format!(" {h}"),
            Style::default()
                .fg(theme::TITLE)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ))],
        Row::Meta {
            label,
            value,
            urgent,
        } => {
            let style = if *urgent {
                Style::default()
                    .fg(theme::OVERDUE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::NORMAL)
            };
            vec![Line::from(vec![
                Span::styled(
                    format!(" {label:width$}", width = LABEL),
                    Style::default().fg(theme::HEADER),
                ),
                Span::styled(value.clone(), style),
            ])]
        }
        Row::Text { text } => vec![Line::from(Span::styled(
            format!("  {text}"),
            Style::default().fg(theme::NORMAL),
        ))],
        Row::Attachment {
            index,
            kind,
            label,
            target,
            missing,
        } => {
            let focused = *index == focused_attachment;
            let marker = if focused { "▌" } else { " " };
            let kind_style = Style::default().fg(theme::MODE_FILTER);
            let title_style = if *missing {
                Style::default().fg(theme::ERROR)
            } else if focused {
                Style::default()
                    .fg(theme::NORMAL)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::NORMAL)
            };

            let mut lines = vec![Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(theme::MODE_NORMAL)),
                Span::styled(format!("{index}. "), Style::default().fg(theme::HEADER)),
                Span::styled(format!("{kind}"), kind_style),
                Span::raw("  "),
                Span::styled(label.clone(), title_style),
                Span::styled(
                    if *missing { "  (missing)" } else { "" }.to_string(),
                    Style::default().fg(theme::ERROR),
                ),
            ])];
            // Show the target only when it differs from the label, so a labelled
            // attachment is not printed twice.
            if label != target {
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(target.clone(), Style::default().fg(theme::HEADER)),
                ]));
            }
            lines
        }
        Row::Hint(h) => vec![Line::from(Span::styled(
            format!(" {h}"),
            Style::default().fg(theme::HEADER),
        ))],
    }
}

/// Flatten a task into display lines, honouring the scroll offset.
#[must_use]
pub fn lines(
    task: &Task,
    subtasks: &[&Task],
    now: chrono::DateTime<chrono::Utc>,
    scroll: usize,
    height: usize,
    focused_attachment: usize,
) -> Vec<Line<'static>> {
    let rows = detail::rows(task, subtasks, now, true);
    let end = scroll.saturating_add(height).min(rows.len());
    if scroll >= rows.len() {
        return Vec::new();
    }
    rows[scroll..end]
        .iter()
        .flat_map(|r| render_row(r, focused_attachment))
        .collect()
}

/// Draw the pane. Returns the total row count, for scroll clamping.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    task: &Task,
    subtasks: &[&Task],
    state: &crate::detail::Detail,
    now: chrono::DateTime<chrono::Utc>,
) -> usize {
    let rows = detail::rows(task, subtasks, now, true);
    let inner_height = usize::from(area.height).saturating_sub(2);

    let body: Vec<Line> = {
        let end = state.scroll.saturating_add(inner_height).min(rows.len());
        if state.scroll >= rows.len() {
            Vec::new()
        } else {
            rows[state.scroll..end]
                .iter()
                .flat_map(|r| render_row(r, state.attachment_focus))
                .collect()
        }
    };

    let title = if task.status.is_terminal() {
        format!("{} {}", task.title, task.status.as_str())
    } else {
        task.title.clone()
    };

    frame.render_widget(
        Paragraph::new(body)
            .block(ui::block(
                &ui::truncate(&title, usize::from(area.width) - 6),
                true,
            ))
            .wrap(Wrap { trim: false }),
        area,
    );

    rows.len()
}

/// Render the pane as plain text, for tests and for `--text` output.
#[must_use]
pub fn text(task: &Task, subtasks: &[&Task], now: chrono::DateTime<chrono::Utc>) -> String {
    detail::rows(task, subtasks, now, false)
        .iter()
        .map(|r| match r {
            Row::Blank => String::new(),
            Row::Meta {
                label,
                value,
                urgent,
            } => {
                format!("{label:width$}{value}", width = LABEL + 1)
                    + if *urgent { "  !" } else { "" }
            }
            Row::Heading(h) => format!("-- {h} --"),
            Row::Text { text } => text.clone(),
            Row::Attachment {
                index,
                kind,
                label,
                target,
                missing,
            } => {
                let mut s = format!("{index}. [{kind}] {label}");
                if label != target {
                    s.push_str(&format!("\n   {target}"));
                }
                if *missing {
                    s.push_str("   (missing)");
                }
                s
            }
            Row::Hint(h) => h.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
