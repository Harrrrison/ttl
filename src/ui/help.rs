//! The full-screen help overlay, rendered from the binding tables.
//!
//! Because the tables in [`crate::input::BINDINGS`] are the single source of
//! truth, this can never document a key the dispatcher does not handle.
//!
//! Sections are laid out in two columns. A single column needed 46 rows, which
//! does not fit an ordinary 24-row terminal, so the bottom third of the help
//! was silently unreachable.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::app::App;
use crate::input::BINDINGS;
use crate::ui::{self, theme};

/// Key column width, including padding.
const KEY_WIDTH: usize = 13;

/// Rows the overlay needs beyond the binding list: title, store path, blanks,
/// and the closing hint.
const CHROME_ROWS: u16 = 5;

/// Flatten the grouped tables into renderable lines.
#[must_use]
fn section_lines() -> Vec<(Vec<Line<'static>>, usize)> {
    BINDINGS
        .iter()
        .map(|(section, bindings)| {
            let mut lines = vec![Line::from(Span::styled(
                section.to_string(),
                Style::default()
                    .fg(theme::MODE_NORMAL)
                    .add_modifier(Modifier::BOLD),
            ))];
            for b in bindings.iter() {
                lines.push(Line::from(vec![
                    Span::styled(
                        ui::cell(b.keys, KEY_WIDTH),
                        Style::default().fg(theme::MODE_INSERT),
                    ),
                    Span::styled(b.desc.to_string(), Style::default().fg(theme::NORMAL)),
                ]));
            }
            lines.push(Line::from(""));
            let height = lines.len();
            (lines, height)
        })
        .collect()
}

/// Total rows the full help needs in one column.
#[must_use]
pub fn full_height() -> u16 {
    let sections: usize = section_lines().iter().map(|(_, n)| n).sum();
    u16::try_from(sections + usize::from(CHROME_ROWS)).unwrap_or(u16::MAX)
}

/// Rows of bindings that fit in `height`, after the border and chrome.
#[must_use]
pub fn visible_rows(height: u16) -> usize {
    usize::from(height.saturating_sub(CHROME_ROWS + 2)).max(1)
}

/// Split sections into two balanced columns, preserving declaration order.
///
/// Returns the row count of the taller column.
fn split_columns(
    sections: &[(Vec<Line<'static>>, usize)],
    per_column: usize,
) -> (Vec<Line<'static>>, Vec<Line<'static>>, u16) {
    let total: usize = sections.iter().map(|(_, n)| n).sum();
    // Two columns means roughly half the rows each.
    let per_column = per_column.max(total / 2).max(1);

    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut left_rows, mut right_rows) = (0usize, 0usize);

    for (lines, len) in sections {
        // Always place the section in whichever column has more room, so a tall
        // section cannot spill off the end.
        if left_rows <= right_rows && left_rows + len <= per_column {
            left_rows += len;
            left.extend(lines.iter().cloned());
        } else {
            right_rows += len;
            right.extend(lines.iter().cloned());
        }
    }

    (
        left,
        right,
        u16::try_from(left_rows.max(right_rows)).unwrap_or(u16::MAX),
    )
}

/// Last two components of a path, e.g. `.../.local/share/ttm`.
#[must_use]
pub fn short_path(path: &std::path::Path) -> String {
    // `Path::components` yields the root as a separate component, so joining
    // everything would produce `//ttm`.
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .filter(|s| s != "/")
        .collect();

    if parts.is_empty() {
        return "/".to_string();
    }
    match parts.len() {
        0 | 1 => format!("/{}", parts.join("/")),
        n => format!(".../{}", parts[n - 2..].join("/")),
    }
}

/// Build the overlay body as two columns of lines.
#[must_use]
pub fn lines(app: &App) -> Vec<Vec<Line<'static>>> {
    let header = vec![
        Line::from(Span::styled(
            "ttm — key bindings",
            Style::default()
                .fg(theme::TITLE)
                .add_modifier(Modifier::BOLD),
        )),
        // Just the tail of the path: the full one wraps and pushes the
        // bindings off screen.
        Line::from(Span::styled(
            format!("store  {}", short_path(app.store.root())),
            Style::default().fg(theme::HEADER),
        )),
        Line::from(""),
    ];
    let footer = vec![
        Line::from(""),
        Line::from(Span::styled(
            "press ? or <Esc> to close",
            Style::default().fg(theme::HEADER),
        )),
    ];
    vec![header, footer]
}

/// Draw the help overlay centred over the list.
///
/// `scroll` drops that many rows from the top of both columns, so every binding
/// stays reachable on a terminal too short to show them all at once.
pub fn render(frame: &mut Frame, area: Rect, app: &App, scroll: usize) {
    let sections = section_lines();

    // Fill the terminal: the overlay is a transient view, so use the height.
    let height = area.height;
    let per_column = visible_rows(height);

    let (left, right, _) = split_columns(&sections, per_column);
    let head = lines(app);

    // Clamp every line to one column so a long store path cannot spill into the
    // neighbouring column. The exact width is known only after the block's
    // inner area is computed, so this is applied below.
    let col_width = usize::from(area.width / 2).max(8);
    let clamp = |lines: Vec<Line<'static>>| -> Vec<Line<'static>> {
        lines
            .into_iter()
            .map(|line| {
                let spans: Vec<Span> = line
                    .spans
                    .iter()
                    .map(|s| Span::styled(ui::truncate(&s.content, col_width), s.style))
                    .collect();
                Line::from(spans)
            })
            .collect()
    };

    let mut left_lines: Vec<Line<'static>> = clamp(head[0].clone());
    left_lines.extend(left.into_iter().skip(scroll));
    let right_lines: Vec<Line<'static>> = clamp(right.into_iter().skip(scroll).collect());
    left_lines.extend(head[1].clone());

    // The overlay fills the terminal: leaving margins would let the task list
    // show through around it, which reads as a rendering bug.
    let inner = Rect { height, ..area };

    frame.render_widget(Clear, inner);
    let block = Block::bordered().border_style(Style::default().fg(theme::TITLE));
    // `Block::inner` keeps the text off the borders. Deriving the rect by hand
    // (or via `..inner`) starts at the block's top-left corner and overwrites
    // the top and left edges, which lets the task list show through.
    let content = block.inner(inner);
    frame.render_widget(block, inner);

    let [left_area, right_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(content);

    if !right_lines.is_empty() {
        frame.render_widget(
            Paragraph::new(right_lines).wrap(Wrap { trim: false }),
            right_area,
        );
    }
    frame.render_widget(
        Paragraph::new(left_lines).wrap(Wrap { trim: false }),
        left_area,
    );
}

/// Largest valid scroll offset for a given terminal height.
#[must_use]
pub fn max_scroll(height: u16) -> usize {
    let sections = section_lines();
    let per_column = visible_rows(height);
    let (left, right, _) = split_columns(&sections, per_column);
    left.len()
        .saturating_sub(per_column)
        .max(right.len().saturating_sub(per_column))
}

/// The small always-available cheat sheet, toggled with `C-p`.
pub fn render_panel(frame: &mut Frame, area: Rect, _app: &App) {
    // A compact digest: the most-used actions only.
    const ESSENTIAL: &[(&str, &str)] = &[
        ("j k", "move"),
        ("g g / G", "top / bottom"),
        ("/", "search"),
        ("f", "status filter"),
        ("S", "sort"),
        ("a", "add"),
        ("<Space>", "cycle status"),
        ("<Enter>", "detail"),
        ("V", "visual"),
        ("?", "help"),
        ("q", "quit"),
    ];

    let mut body: Vec<Line> = ESSENTIAL
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(
                    ui::cell(k, KEY_WIDTH),
                    Style::default().fg(theme::MODE_INSERT),
                ),
                Span::raw(*d),
            ])
        })
        .collect();
    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "C-p hides this",
        Style::default().fg(theme::HEADER),
    )));

    let panel_width = 34u16;
    let [_, right, _] = Layout::horizontal([
        Constraint::Min(20),
        Constraint::Length(panel_width),
        Constraint::Length(0),
    ])
    .areas(area);

    // The block's top and bottom borders occupy two of the rows.
    let border_rows = 2u16;
    let height = (u16::try_from(body.len()).unwrap_or(u16::MAX) + border_rows).min(right.height);
    if height <= border_rows {
        return;
    }
    let rect = Rect { height, ..right };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(body)
            .block(ui::block("keys", false))
            .wrap(Wrap { trim: false }),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_80x24_terminal_can_show_part_of_the_help() {
        // The full list is longer than any terminal, so the overlay scrolls;
        // what matters is that a useful amount is visible at once.
        let rows = visible_rows(24);
        assert!(
            rows >= 10,
            "only {rows} binding rows visible on a 24-row terminal"
        );
    }

    #[test]
    fn short_paths_stay_on_one_line() {
        use std::path::Path;
        assert_eq!(short_path(Path::new("/a/b/c/d/ttm")), ".../d/ttm");
        assert_eq!(short_path(Path::new("/ttm")), "/ttm");
        assert_eq!(short_path(Path::new("/")), "/");
    }

    #[test]
    fn scrolling_reaches_every_row() {
        let height = 24;
        let per_column = visible_rows(height);
        let sections = section_lines();
        let (left, right, _) = split_columns(&sections, per_column);
        let max = max_scroll(height);

        assert!(max > 0, "the help should overflow and need scrolling");
        assert_eq!(
            max,
            left.len()
                .saturating_sub(per_column)
                .max(right.len().saturating_sub(per_column))
        );

        // At the last offset the tail of the longer column is visible.
        let tail: Vec<&Line> = left.iter().skip(max).collect();
        assert!(!tail.is_empty(), "nothing left to show at max scroll");
    }

    #[test]
    fn the_two_columns_are_balanced() {
        let sections = section_lines();
        let (left, right, _) = split_columns(&sections, 22);
        assert!(
            !left.is_empty() && !right.is_empty(),
            "both columns should be used"
        );
        let (l, r) = (left.len(), right.len());
        assert!(l.abs_diff(r) <= 6, "columns are unbalanced: {l} vs {r}");
    }

    #[test]
    fn every_section_appears_in_one_of_the_two_columns() {
        let sections = section_lines();
        let (left, right, _) = split_columns(&sections, 22);
        let mut all = left;
        all.extend(right);
        let text: String = all
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();

        for (name, _) in BINDINGS {
            assert!(text.contains(name), "section {name:?} went missing");
        }
    }
}
