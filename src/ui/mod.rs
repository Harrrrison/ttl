//! Rendering. Every function here is pure with respect to the app: it reads
//! state and draws, mutating nothing.
//!
//! Layout, colour, and truncation are kept in small helpers so they can be
//! unit-tested without a terminal buffer, and so the golden-buffer tests in
//! `tests/render.rs` have stable output to assert against.

pub mod detail;
pub mod form;
pub mod help;
pub mod list;
pub mod prompt;
pub mod statusbar;
pub mod timeline;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, Overlay};

/// The app's palette, kept in one place so it is easy to retune.
pub mod theme {
    use ratatui::style::Color;

    pub const TITLE: Color = Color::Cyan;
    pub const BORDER: Color = Color::DarkGray;
    pub const BORDER_FOCUSED: Color = Color::Cyan;

    pub const HEADER: Color = Color::DarkGray;

    pub const NORMAL: Color = Color::Reset;
    pub const DONE: Color = Color::DarkGray;
    pub const OVERDUE: Color = Color::Red;
    pub const DUE_SOON: Color = Color::Yellow;
    pub const SELECTED_BG: Color = Color::Rgb(38, 48, 66);

    pub const MODE_NORMAL: Color = Color::Blue;
    pub const MODE_INSERT: Color = Color::Green;
    pub const MODE_FILTER: Color = Color::Magenta;

    pub const ERROR: Color = Color::Red;
    pub const SUCCESS: Color = Color::Green;
    pub const INFO: Color = Color::Gray;
}

/// Style for the mode indicator.
#[must_use]
pub fn mode_style(app: &App) -> Style {
    let color = match app.context {
        crate::input::Context::Global => theme::MODE_NORMAL,
        crate::input::Context::Insert => theme::MODE_INSERT,
        crate::input::Context::Prompt => theme::MODE_FILTER,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// Vertical split for the list area: optional header row, rows, optional footer.
///
/// Returns the row area and whether the header had room to be drawn. Both are
/// derived from the same geometry, so the caller cannot ask for a header it has
/// no space for.
#[must_use]
pub fn list_area(area: Rect, has_header: bool, has_footer: bool) -> (Rect, bool) {
    let constraints = [
        if has_header {
            Constraint::Length(1)
        } else {
            Constraint::Length(0)
        },
        Constraint::Min(1),
        if has_footer {
            Constraint::Length(1)
        } else {
            Constraint::Length(0)
        },
    ];
    let [_, rows, _] = Layout::vertical(constraints).areas(area);
    (rows, has_header && area.height > 2)
}

/// Truncate to `width` columns, appending `…` when text is cut.
///
/// Width is measured in terminal columns, not bytes or `char`s, so CJK and emoji
/// do not overflow their column.
#[must_use]
pub fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if unicode_width::UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    let target = width - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > target {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Pad to exactly `width` columns, truncating when too long.
#[must_use]
pub fn cell(text: &str, width: usize) -> String {
    let t = truncate(text, width);
    let pad = width.saturating_sub(unicode_width::UnicodeWidthStr::width(t.as_str()));
    format!("{t}{}", " ".repeat(pad))
}

/// A titled block with the standard border styling.
#[must_use]
pub fn block(title: &str, focused: bool) -> ratatui::widgets::Block<'_> {
    let border_color = if focused {
        theme::BORDER_FOCUSED
    } else {
        theme::BORDER
    };
    let style = Style::default().fg(border_color);

    let b = ratatui::widgets::Block::bordered().border_style(style);
    if title.is_empty() {
        b
    } else {
        // ratatui 0.30 takes `Into<Line>` directly; there is no `Title` wrapper.
        b.title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(theme::TITLE),
        )))
    }
}

/// Build the bottom status line: mode, position, message, pending keys.
#[must_use]
pub fn status_line(app: &App, width: u16) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();

    spans.push(Span::styled(
        format!(" {} ", app.context.label()),
        mode_style(app).bg(theme::MODE_NORMAL),
    ));
    spans.push(Span::raw(" "));

    let total = app.rows.len();
    if total == 0 {
        spans.push(Span::styled("no tasks", Style::default().fg(theme::HEADER)));
    } else {
        spans.push(Span::styled(
            format!("{}/{}", app.cursor + 1, total),
            Style::default().fg(theme::NORMAL),
        ));
        if app.in_detail() {
            let total = app.detail_task().map_or(0, |t| t.attachments.len());
            spans.push(Span::raw("  "));
            if total == 0 {
                spans.push(Span::styled(
                    "no attachments",
                    Style::default().fg(theme::HEADER),
                ));
            } else {
                spans.push(Span::styled(
                    format!(
                        "att {}/{}  (o open, d remove)",
                        app.detail.attachment_focus + 1,
                        total
                    ),
                    Style::default().fg(theme::MODE_FILTER),
                ));
            }
        }
        if app.in_visual() {
            spans.push(Span::styled(
                format!(" ({})", app.selection().len()),
                Style::default()
                    .fg(theme::MODE_FILTER)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("sort:{}", app.sort.as_str()),
            Style::default().fg(theme::HEADER),
        ));
        if !app.filter.statuses.is_empty() {
            spans.push(Span::styled(
                format!(
                    "  filter:{}",
                    app.filter
                        .statuses
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                Style::default().fg(theme::MODE_FILTER),
            ));
        }
        if let Some(text) = &app.filter.text {
            spans.push(Span::styled(
                format!("  /{text}"),
                Style::default().fg(theme::MODE_FILTER),
            ));
        }
    }

    // Prompt contents, when a single-field prompt is open. The task form has
    // its own panel and does not echo here.
    if let Overlay::Prompt(p) = &app.overlay {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("{}: ", p.kind.title()),
            Style::default().fg(theme::TITLE),
        ));
        spans.push(Span::styled(
            p.buf.text().to_string(),
            Style::default().fg(theme::MODE_INSERT),
        ));
        spans.push(Span::styled("▏", Style::default().fg(theme::MODE_INSERT)));
    }

    // The message sits on the right, and pending keys take precedence visually.
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if let Some(msg) = &app.message {
        let color = match msg.kind {
            crate::app::ToastKind::Error => theme::ERROR,
            crate::app::ToastKind::Success => theme::SUCCESS,
            crate::app::ToastKind::Info => theme::INFO,
        };
        let room = (width as usize).saturating_sub(used + msg.text.chars().count() + 4);
        if room > 0 {
            spans.push(Span::raw(" ".repeat(room)));
            spans.push(Span::styled(
                crate::ui::truncate(&msg.text, room),
                Style::default().fg(color),
            ));
        }
    }

    if !app.pending.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            app.pending_label(),
            Style::default()
                .fg(theme::MODE_INSERT)
                .add_modifier(Modifier::BOLD),
        ));
    }

    Line::from(spans)
}
