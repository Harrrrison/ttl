//! Rendering the gantt chart.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::timeline::{self, Cell, Timeline};
use crate::ui::{self, theme};

/// Title column width, capped so the chart always gets room.
const TITLE_MAX: u16 = 26;

/// Right gutter: countdown plus estimate.
const GUTTER: u16 = 12;

/// Colour for one chart cell.
#[must_use]
pub fn cell_style(cell: Cell, row: &timeline::Row, selected: bool) -> Style {
    let mut base = Style::default();
    match cell {
        Cell::Blank => base = base.fg(theme::BORDER),
        Cell::Weekend => base = base.fg(theme::BORDER),
        Cell::Bar => base = base.fg(theme::MODE_NORMAL),
        Cell::Overdue => base = base.fg(theme::ERROR).add_modifier(Modifier::BOLD),
        Cell::Done => base = base.fg(theme::DONE),
        Cell::DueEdge => base = base.fg(theme::DUE_SOON),
        Cell::Now => base = base.fg(theme::TITLE).add_modifier(Modifier::BOLD),
        Cell::Estimate => base = base.fg(theme::MODE_INSERT).add_modifier(Modifier::BOLD),
        Cell::TodayBare => base = base.fg(theme::TITLE),
    }
    if row.completed && cell != Cell::Estimate {
        base = base.add_modifier(Modifier::DIM);
    }
    if selected {
        base = base.bg(theme::SELECTED_BG);
    }
    base
}

/// The axis: a month row above, and bucket labels with today marked.
///
/// A month row is always drawn, because a single-character day column cannot
/// carry the information on its own — the digits alone would read as a
/// meaningless sequence.
#[must_use]
pub fn header_lines(tl: &Timeline, offset: usize) -> Vec<Line<'static>> {
    let cw = tl.scale.column_width();
    let style = Style::default()
        .fg(theme::TITLE)
        .add_modifier(Modifier::BOLD);

    // Month names are wider than one column, so each is written at its own
    // starting character offset. A later month that would overlap an earlier
    // one is skipped rather than drawn on top of it.
    let chart_width = tl.columns * cw;
    let mut month_row = vec![' '; chart_width];
    for (col, label) in tl.months.iter().enumerate().take(tl.columns) {
        let Some(label) = label else { continue };
        let start = col * cw;
        let end = start + label.chars().count();
        if end > chart_width || month_row[start..end].iter().any(|c| *c != ' ') {
            continue;
        }
        for (i, c) in label.chars().enumerate() {
            month_row[start + i] = c;
        }
    }
    let months: Vec<Span> = vec![
        Span::raw(" ".repeat(offset)),
        Span::styled(month_row.into_iter().collect::<String>(), style),
    ];

    let mut days: Vec<Span> = vec![Span::raw(" ".repeat(offset))];
    for (col, label) in tl.header.iter().enumerate().take(tl.columns) {
        let is_now = col == tl.now_col;
        let cell_style = if is_now {
            Style::default()
                .fg(theme::TITLE)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else if tl.is_weekend(col) {
            Style::default().fg(theme::HEADER)
        } else {
            Style::default()
                .fg(theme::HEADER)
                .add_modifier(Modifier::DIM)
        };
        // Each label is padded to exactly the column width, which is what keeps
        // the axis aligned with the chart drawn beneath it.
        let mut cell = String::new();
        for _ in label.chars().count()..cw {
            cell.push(' ');
        }
        cell.push_str(label);
        while cell.chars().count() < cw {
            cell.push(' ');
        }
        days.push(Span::styled(cell, cell_style));
    }

    if tl.months.iter().all(Option::is_none) {
        // At month scale the day row already names every column, so a month row
        // on top of it would be redundant.
        vec![Line::from(days)]
    } else {
        vec![Line::from(months), Line::from(days)]
    }
}

/// A rollup bar for a parent, drawn inside the title column.
#[must_use]
pub fn rollup(done: usize, total: usize, width: usize) -> String {
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

/// Build one rendered line for a timeline row.
#[must_use]
pub fn row_line(
    tl: &Timeline,
    row: &timeline::Row,
    selected: bool,
    title_width: usize,
    gutter: u16,
) -> Line<'static> {
    let mut title = "  ".repeat(usize::from(row.depth));
    if let Some((done, total)) = row.rollup {
        // Reserve room for the rollup plus a separating space, so the title
        // always gets whatever is left.
        let used = 5usize.min(title_width.saturating_sub(title.chars().count() + 8));
        if used > 1 {
            title.push_str(&rollup(done, total, used));
            title.push(' ');
        }
    }
    title.push_str(&row.title);
    let marker = if selected { "▌" } else { " " };
    let mut spans = vec![Span::styled(
        format!("{marker}{title}"),
        if selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    )];
    // The title is truncated to the column so the chart geometry never shifts.
    let used = spans[0].content.chars().count();
    if title_width > used {
        spans[0] = Span::styled(
            format!("{:<width$}", spans[0].content, width = title_width),
            spans[0].style,
        );
    } else if used > title_width {
        spans[0] = Span::styled(ui::truncate(&spans[0].content, title_width), spans[0].style);
    }

    // Chart cells.
    let cells = timeline::row_cells(row, tl);
    let width = tl.scale.column_width();
    for cell in cells.iter().take(tl.columns) {
        spans.push(Span::styled(
            cell.render(width),
            cell_style(*cell, row, selected),
        ));
    }

    // Right gutter: countdown, then estimate.
    spans.push(Span::styled(
        format!("{:>width$}", row.countdown, width = usize::from(gutter) - 4),
        if row.overdue {
            Style::default()
                .fg(theme::ERROR)
                .add_modifier(Modifier::BOLD)
        } else if row.due_today {
            Style::default().fg(theme::DUE_SOON)
        } else {
            Style::default().fg(theme::HEADER)
        },
    ));
    spans.push(Span::styled(
        format!("{:>4}", row.estimate.as_deref().unwrap_or("")),
        Style::default().fg(theme::INFO),
    ));

    Line::from(spans)
}

/// A legend explaining the glyphs, shown under the chart.
///
/// Entries are dropped from the end when the chart is narrow, so the legend
/// never wraps onto a second line and pushes the rows up.
#[must_use]
pub fn legend(width: usize) -> Line<'static> {
    // `const` cannot hold `Color::Rgb`, so the table is built at runtime.
    let entries: [(char, &str, Color); 6] = [
        ('\u{2500}', "scheduled ", theme::MODE_NORMAL),
        ('\u{2588}', "overdue ", theme::ERROR),
        ('\u{2502}', "today ", theme::TITLE),
        ('\u{25c6}', "est finishes ", theme::MODE_INSERT),
        ('\u{2590}', "due ", theme::DUE_SOON),
        ('\u{2591}', "subtasks ", theme::HEADER),
    ];

    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0usize;
    for (glyph, label, colour) in entries {
        let w = 2 + label.chars().count();
        if used + w > width {
            break;
        }
        used += w;
        spans.push(Span::styled(
            format!("{glyph} "),
            Style::default().fg(colour),
        ));
        spans.push(Span::styled(label, Style::default().fg(theme::HEADER)));
    }
    Line::from(spans)
}

/// Draw the whole chart.
pub fn render(frame: &mut Frame, area: Rect, tl: &Timeline, cursor: usize, scroll: usize) {
    let title = format!("timeline · {}", tl.scale.label());
    let block = ui::block(&title, true);
    // Measure against the block's *inner* width. Measuring against `area`
    // includes the two border columns, so every row overflows by exactly the
    // amount it wraps.
    let inner_width = usize::from(block.inner(area).width);

    let title_width = inner_width
        .saturating_sub(usize::from(GUTTER))
        .min(usize::from(TITLE_MAX))
        .max(1);
    // A column is not one character: days are three wide, so the character
    // budget has to be divided by the column width to get a column count.
    // Without this the chart is over-wide by a factor of three and every row
    // wraps onto a second line.
    let chart_columns = inner_width
        .saturating_sub(title_width)
        .saturating_sub(usize::from(GUTTER))
        / tl.scale.column_width();

    // The chart may have been laid out for a different terminal; re-narrow it so
    // the geometry and the drawing agree.
    let tl = tl.narrow(chart_columns);

    let body_height = usize::from(area.height).saturating_sub(4);
    let end = (scroll + body_height).min(tl.rows.len());

    let mut lines: Vec<Line> = Vec::with_capacity(body_height + 1);
    if scroll < tl.rows.len() {
        lines.extend(header_lines(&tl, title_width));
        for (offset, row) in tl.rows.iter().take(end).skip(scroll).enumerate() {
            let selected = scroll + offset == cursor;
            lines.push(row_line(&tl, row, selected, title_width, GUTTER));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "no tasks",
            Style::default().fg(theme::HEADER),
        )));
    }
    lines.push(legend(inner_width.saturating_sub(title_width)));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}
