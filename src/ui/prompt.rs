//! The single-field prompt strip used by `/`, `l`, and `p`.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::Prompt;
use crate::ui::{form::Caret, theme};

/// Draw the prompt and return where the caret should sit.
pub fn render(frame: &mut Frame, area: Rect, prompt: &Prompt) -> Option<Caret> {
    let [_, inner, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Percentage(70),
        Constraint::Fill(1),
    ])
    .areas(area);

    let box_area = Rect {
        height: area.height.min(3),
        ..inner
    };

    frame.render_widget(Clear, box_area);
    let block = Block::bordered().border_style(Style::default().fg(theme::MODE_FILTER));
    let content = block.inner(box_area);
    frame.render_widget(
        block.title(Line::from(Span::styled(
            format!(" {} ", prompt.kind.title()),
            Style::default()
                .fg(theme::TITLE)
                .add_modifier(Modifier::BOLD),
        ))),
        box_area,
    );

    let text = if prompt.buf.is_empty() {
        prompt.kind.hint().to_string()
    } else {
        prompt.buf.text().to_string()
    };
    let style = if prompt.buf.is_empty() {
        Style::default()
            .fg(theme::HEADER)
            .add_modifier(Modifier::ITALIC)
    } else {
        Style::default().fg(theme::MODE_INSERT)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, style))),
        content,
    );

    let width = usize::from(content.width);
    let (row, col) = prompt.buf.caret_position(width, prompt.scroll);
    Some(Caret {
        x: content.x + u16::try_from(col).unwrap_or(0),
        y: content.y + u16::try_from(row).unwrap_or(0),
    })
}
