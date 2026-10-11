//! The bottom status line.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::ui;

/// Draw the status line into `area`, one row tall.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let row = Rect { height: 1, ..area };
    frame.render_widget(
        Paragraph::new(ui::status_line(app, area.width)).style(Style::default().bg(Color::Reset)),
        row,
    );
}

/// The line under the table: counts and total estimate.
pub fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(crate::ui::list::footer(app)).style(Style::default()),
        Rect { height: 1, ..area },
    );
}
