//! Rendering the task form: a centred panel of labelled fields, focused one at
//! a time, with inline errors.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::form::{Form, FormMode};
use crate::ui::theme;

/// Width of the label gutter, including its gap.
const LABEL_WIDTH: usize = 14;

/// Where the caret sits, so the caller can place the real terminal cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    pub x: u16,
    pub y: u16,
}

/// Total rows the form wants, given its content.
#[must_use]
pub fn desired_height(form: &Form) -> usize {
    let fields: usize = form.fields.iter().map(|f| f.kind.rows()).sum();
    // header + fields + per-field gaps + banner + footer + borders
    fields + form.fields.len() + 5
}

/// Draw the form. Returns the caret position in terminal coordinates.
pub fn render(frame: &mut Frame, area: Rect, form: &Form) -> Option<Caret> {
    let title = match form.mode {
        FormMode::Add => "new task",
        FormMode::Edit(_) => "edit task",
    };

    // Fit the panel to the terminal, but never exceed what it can show.
    let max_h = usize::from(area.height.saturating_sub(2));
    let height = desired_height(form).min(max_h).max(8) as u16;
    let width = area.width.saturating_sub(8).clamp(40, 100);

    let [_, centred, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width),
        Constraint::Fill(1),
    ])
    .areas(area);
    let panel = Rect {
        y: area.y + (area.height.saturating_sub(height)) / 2,
        height,
        ..centred
    };

    frame.render_widget(Clear, panel);
    let block = Block::bordered().border_style(Style::default().fg(theme::TITLE));
    let inner = block.inner(panel);
    frame.render_widget(
        block.title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(theme::TITLE),
        ))),
        panel,
    );

    let mut y = inner.y;
    let content_width = usize::from(inner.width).saturating_sub(LABEL_WIDTH);

    // Banner for a failed save.
    if let Some(banner) = &form.banner {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                banner.clone(),
                Style::default()
                    .fg(theme::ERROR)
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect { height: 1, ..inner },
        );
        y += 1;
    }

    let mut caret = None;

    for (i, field) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        let rows = field.kind.rows();

        // Render the value, wrapped to the content width.
        let wrapped = field.buf.wrap(content_width, field.scroll);
        let label_style = if focused {
            Style::default()
                .fg(theme::TITLE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::HEADER)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{:width$}", field.kind.label(), width = LABEL_WIDTH),
                label_style,
            ))),
            Rect {
                x: inner.x,
                y,
                width: u16::try_from(LABEL_WIDTH).unwrap_or(14),
                height: 1,
            },
        );

        let show_hint = field.buf.is_empty() && !focused;
        let value_lines: Vec<Line> = if show_hint {
            vec![Line::from(Span::styled(
                field.kind.hint(),
                Style::default()
                    .fg(theme::HEADER)
                    .add_modifier(Modifier::ITALIC),
            ))]
        } else {
            wrapped
                .lines
                .iter()
                .map(|l| {
                    // Continuation rows are indented so wrapped text reads as a
                    // continuation of the same field.
                    let prefix = if l.continuation { "  " } else { "" };
                    Line::from(Span::styled(
                        format!("{prefix}{}", l.text),
                        if focused {
                            Style::default()
                        } else {
                            Style::default().fg(theme::NORMAL)
                        },
                    ))
                })
                .collect()
        };

        let value_area = Rect {
            x: inner.x + u16::try_from(LABEL_WIDTH).unwrap_or(14),
            y,
            width: inner
                .width
                .saturating_sub(u16::try_from(LABEL_WIDTH).unwrap_or(14)),
            height: u16::try_from(rows).unwrap_or(1),
        };
        frame.render_widget(Paragraph::new(value_lines), value_area);

        if focused {
            // The editor owns wrap geometry; ask it where the caret lands.
            let (caret_row, caret_col) = field.buf.caret_position(content_width, field.scroll);
            caret = Some(Caret {
                x: value_area.x + u16::try_from(caret_col).unwrap_or(0),
                y: y + u16::try_from(caret_row).unwrap_or(0),
            });
        }

        y += u16::try_from(rows).unwrap_or(1);

        // Inline error, taking a row only when there is one.
        if let Some(err) = &field.error
            && y < inner.y + inner.height
        {
            let label = u16::try_from(LABEL_WIDTH).unwrap_or(14);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    err.clone(),
                    Style::default().fg(theme::ERROR),
                ))),
                Rect {
                    x: inner.x + label,
                    y,
                    width: inner.width.saturating_sub(label),
                    height: 1,
                },
            );
            y += 1;
        }
    }

    // Footer hint, pinned to the bottom.
    if inner.height > 2 {
        let hint_y = inner.y + inner.height - 1;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Tab next field · Ctrl-s save · Esc cancel",
                Style::default().fg(theme::HEADER),
            ))),
            Rect {
                y: hint_y,
                height: 1,
                ..inner
            },
        );
    }

    caret
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form::FieldKind;

    #[test]
    fn every_field_kind_has_a_label_and_a_hint() {
        for kind in FieldKind::ALL {
            assert!(!kind.label().is_empty(), "{kind:?} needs a label");
            assert!(!kind.hint().is_empty(), "{kind:?} needs a hint");
        }
    }

    #[test]
    fn the_form_is_tall_enough_for_its_fields() {
        let form = Form::add();
        let h = desired_height(&form);
        assert!(h >= 12, "expected room for 8 fields, got {h}");
    }
}
