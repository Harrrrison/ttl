//! The task table.

use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row, Table, TableState};

use crate::app::App;
use crate::model::Task;
use crate::ui::{self, theme};

/// Column headers, in order.
pub const HEADERS: [&str; 7] = ["", "DUE", "LEFT", "EST", "LNKS", "PRI", "TITLE"];

/// Fixed column widths, in order, plus the flexible title. Kept numeric as
/// well as constrained so the title's available width can be computed.
const COLUMN_WIDTHS: [u16; 6] = [1, 10, 11, 7, 5, 4];

/// Columns consumed by the fixed columns, the gaps between them, and the
/// block's two vertical borders.
fn fixed_columns() -> u16 {
    COLUMN_WIDTHS.iter().sum::<u16>() + u16::try_from(COLUMN_WIDTHS.len()).unwrap_or(7) + 2
}

const WIDTHS: [Constraint; 7] = [
    Constraint::Length(1),  // selection marker
    Constraint::Length(10), // due date
    Constraint::Length(11), // countdown: "overdue 365d" must fit
    Constraint::Length(7),  // estimate
    Constraint::Length(5),  // link count
    Constraint::Length(4),  // priority
    Constraint::Min(10),    // title
];

/// How many characters into the future count as "soon".
const SOON_DAYS: i64 = 2;

/// Style for a row's title, driven by status and deadline pressure.
#[must_use]
pub fn task_style(task: &Task, now: chrono::DateTime<Utc>) -> Style {
    let base = if task.status == crate::model::Status::Done {
        Style::default()
            .fg(theme::DONE)
            .add_modifier(Modifier::CROSSED_OUT)
    } else if task.status == crate::model::Status::Cancelled {
        Style::default().fg(theme::DONE)
    } else if task.due_at.is_some_and(|d| d < now) {
        Style::default()
            .fg(theme::OVERDUE)
            .add_modifier(Modifier::BOLD)
    } else if task
        .due_at
        .is_some_and(|d| d <= now + chrono::Duration::days(SOON_DAYS))
    {
        Style::default().fg(theme::DUE_SOON)
    } else {
        Style::default().fg(theme::NORMAL)
    };

    match task.priority {
        crate::model::Priority::Urgent if task.status.is_open() => {
            base.fg(theme::OVERDUE).add_modifier(Modifier::BOLD)
        }
        _ => base,
    }
}

/// Priority shown as a short marker: `!!!` style urgency reads at a glance.
#[must_use]
pub fn priority_label(p: crate::model::Priority) -> &'static str {
    match p {
        crate::model::Priority::Low => "··",
        crate::model::Priority::Normal => "·",
        crate::model::Priority::High => "!!",
        crate::model::Priority::Urgent => "!!!",
    }
}

/// A rollup bar like `▓▓▓░░` for a parent with subtasks.
#[must_use]
pub fn rollup_bar(done: usize, total: usize, width: usize) -> String {
    if total == 0 || width == 0 {
        return String::new();
    }
    let filled = width * done / total;
    let mut s = String::new();
    for i in 0..width {
        s.push(if i < filled { '▓' } else { '░' });
    }
    s
}

/// Title text, including indent and rollup markers.
///
/// `width` is the title column's width in terminal columns. ratatui clips an
/// over-long cell silently, so the ellipsis has to be added here for it to be
/// visible that the text was cut.
#[must_use]
pub fn title_of(app: &App, index: usize, width: usize) -> String {
    let Some(row) = app.rows.get(index) else {
        return String::new();
    };
    let indent = "  ".repeat(usize::from(row.depth));
    let mut s = indent;
    if row.children > 0 {
        s.push_str(&format!("({}/{}) ", row.children_done, row.children));
    }
    s.push_str(&row.task.title);
    ui::truncate(&s, width)
}

/// Draw the table into `area`.
pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let now = Utc::now();
    let (row_area, header_shown) = ui::list_area(area, true, false);
    let title_width = usize::from(area.width.saturating_sub(fixed_columns())).max(4);

    let header = Row::new(
        HEADERS.iter().map(|h| {
            Cell::from(Span::styled(
                *h,
                Style::default()
                    .fg(theme::HEADER)
                    .add_modifier(Modifier::BOLD),
            ))
        }),
        // Keep headers aligned with their columns.
    )
    .bottom_margin(0)
    .style(Style::default());

    let rows: Vec<Row> = app
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let t = &row.task;
            let style = task_style(t, now);
            // The marker doubles as the visual-mode indicator and the status
            // glyph, so the column is never blank.
            let marker = if app.in_visual() && app.selection().contains(&t.id) {
                '▌'.to_string()
            } else {
                t.status.glyph().to_string()
            };

            let due = t
                .due_at
                .map_or_else(|| "-".to_string(), |d| d.format("%Y-%m-%d").to_string());
            let left = t
                .due_at
                .filter(|_| t.status.is_open())
                .map_or_else(|| "-".to_string(), |d| crate::model::countdown(d, now));
            let est = t.estimate_human().unwrap_or_else(|| "-".into());
            let links = if t.attachments.is_empty() {
                "-".to_string()
            } else {
                t.attachments.len().to_string()
            };

            Row::new(vec![
                Cell::from(Span::styled(marker, style)),
                Cell::from(Span::styled(due, style)),
                Cell::from(Span::styled(left, style)),
                Cell::from(Span::styled(est, style)),
                Cell::from(Span::styled(links, style)),
                Cell::from(Span::styled(priority_label(t.priority), style)),
                Cell::from(Line::from(Span::styled(
                    title_of(app, i, title_width),
                    style,
                ))),
            ])
        })
        .collect();

    // ratatui 0.30 has no optional header, so fall back to an empty row when
    // there is no room for one.
    let table = Table::new(rows, WIDTHS)
        .header(if header_shown {
            header
        } else {
            Row::new(Vec::<Cell>::new())
        })
        .row_highlight_style(Style::default().bg(theme::SELECTED_BG))
        .column_spacing(1)
        .block(ui::block(
            if app.tree { "tasks" } else { "tasks (flat)" },
            true,
        ));

    // Scroll so the cursor is on screen; `visible_window` is pure and tested.
    let (start, _) = app.visible_window(usize::from(row_area.height));
    let mut state = TableState::default().with_offset(start);
    state.select(if app.rows.is_empty() {
        None
    } else {
        Some(app.cursor)
    });

    frame.render_stateful_widget(table, row_area, &mut state);
}

/// The one-line summary shown under the table.
#[must_use]
pub fn footer(app: &App) -> Line<'static> {
    let now = Utc::now();
    let s = app.summary(now);
    let total = s.total;

    let mut spans = vec![Span::styled(
        format!("{total} task{}", if total == 1 { "" } else { "s" }),
        Style::default().fg(theme::NORMAL),
    )];
    if s.done > 0 {
        spans.push(Span::styled(
            format!("  {} done", s.done),
            Style::default().fg(theme::SUCCESS),
        ));
    }
    if s.doing > 0 {
        spans.push(Span::styled(
            format!("  {} doing", s.doing),
            Style::default().fg(theme::DUE_SOON),
        ));
    }
    if s.blocked > 0 {
        spans.push(Span::styled(
            format!("  {} blocked", s.blocked),
            Style::default().fg(theme::OVERDUE),
        ));
    }
    if s.overdue > 0 {
        spans.push(Span::styled(
            format!("  {} overdue", s.overdue),
            Style::default()
                .fg(theme::ERROR)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if s.total_estimate_minutes > 0 {
        spans.push(Span::styled(
            format!(
                "  est {}",
                crate::model::format_duration(chrono::Duration::minutes(i64::from(
                    s.total_estimate_minutes
                )))
            ),
            Style::default().fg(theme::INFO),
        ));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_labels_are_readable() {
        assert_eq!(priority_label(crate::model::Priority::Low), "··");
        assert_eq!(priority_label(crate::model::Priority::Normal), "·");
        assert_eq!(priority_label(crate::model::Priority::Urgent), "!!!");
    }

    #[test]
    fn rollup_bar_reflects_completion() {
        assert_eq!(rollup_bar(0, 4, 4), "░░░░");
        assert_eq!(rollup_bar(4, 4, 4), "▓▓▓▓");
        assert_eq!(rollup_bar(1, 4, 4), "▓░░░");
        assert_eq!(rollup_bar(1, 3, 6), "▓▓░░░░");
        assert_eq!(rollup_bar(0, 0, 4), "", "no children, no bar");
        assert_eq!(rollup_bar(1, 4, 0), "");
    }

    #[test]
    fn a_done_task_is_struck_through() {
        let now = Utc::now();
        let mut t = Task::new("x", now);
        t.status = crate::model::Status::Done;
        assert!(
            task_style(&t, now)
                .add_modifier
                .contains(Modifier::CROSSED_OUT),
            "completed work should look finished"
        );
    }

    #[test]
    fn an_overdue_task_is_highlighted() {
        let now = Utc::now();
        let mut t = Task::new("x", now);
        t.due_at = Some(now - chrono::Duration::days(1));
        assert_eq!(task_style(&t, now).fg, Some(theme::OVERDUE));
    }
}
