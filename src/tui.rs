//! Terminal lifecycle and the event loop.
//!
//! This is the only module that talks to a real terminal. It initialises the
//! screen, pumps events into [`App`], and guarantees the terminal is restored on
//! every exit path, including panics.

use std::io;
use std::time::Duration;

use chrono::Utc;
use ratatui::Frame;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;

use crate::app::{App, Overlay};
use crate::input::key_of;
use crate::store::Store;
use crate::ui;

/// How long to wait for input before redrawing for time-based changes.
const TICK: Duration = Duration::from_millis(250);

/// Rows the single-field prompt box occupies.
const PROMPT_HEIGHT: u16 = 3;

/// Run the TUI against `store`, returning when the user quits.
pub fn run(store: Store) -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    // Without this, a large paste arrives as a stream of individual key
    // events and is indistinguishable from typing.
    let enable = execute!(io::stdout(), EnableBracketedPaste);

    let result = event_loop(&mut terminal, App::new(store));

    // Restore explicitly rather than relying on the panic hook alone, so a
    // clean exit also leaves the scrollback in a sane state.
    ratatui::try_restore()?;
    let _ = execute!(io::stdout(), DisableBracketedPaste);
    set_cursor(None);
    enable.and(result)
}

/// The loop, separated from setup so tests can drive it with synthetic events.
fn event_loop(terminal: &mut ratatui::DefaultTerminal, mut app: App) -> io::Result<()> {
    let mut needs_redraw = true;

    while !app.should_quit {
        if needs_redraw {
            let now = Utc::now();
            let mut caret = None;
            terminal.draw(|frame| draw(frame, &mut app, now, &mut caret))?;
            set_cursor(caret);
            needs_redraw = false;
        }

        if !event::poll(TICK)? {
            // Timed out: only worth redrawing if a toast expired.
            if app.tick_expired() {
                needs_redraw = true;
            }
            continue;
        }

        match event::read()? {
            TermEvent::Key(k) if k.kind == KeyEventKind::Press => {
                if let Some(key) = key_of(&k) {
                    needs_redraw = app.route_key(key) || needs_redraw;
                }
            }
            TermEvent::Paste(text) => {
                app.paste(&text);
                needs_redraw = true;
            }
            TermEvent::Resize(_, _) => needs_redraw = true,
            // Mouse and focus events are not bound yet; ignore them.
            _ => {}
        }
    }

    Ok(())
}

/// Draw one frame.
pub fn draw(
    frame: &mut Frame,
    app: &mut App,
    now: chrono::DateTime<Utc>,
    cursor_out: &mut Option<ui::form::Caret>,
) {
    let area = frame.area();
    if area.width < 20 || area.height < 4 {
        // Too small to lay out; say so rather than drawing garbage.
        // Short enough to fit the smallest terminal we still accept.
        frame.render_widget(ratatui::widgets::Paragraph::new("too small"), area);
        return;
    }

    // The renderer is the only thing that knows the viewport height, and
    // movement keys depend on it, so measure first.
    let chrome_rows = 3u16; // block border top/bottom + footer + status
    app.viewport = area.height.saturating_sub(chrome_rows).max(1);

    let [main, footer, status] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    if app.in_detail() {
        if let Some(task) = app.detail_task().cloned() {
            let subtasks = app.set.children(&task.id);
            ui::detail::render(frame, main, &task, &subtasks, &app.detail, now);
        }
    } else if app.in_timeline() {
        // The chart must be laid out before it is drawn, and that needs the
        // column count, which depends on the title and gutter widths.
        let title = main.width.saturating_sub(12).min(26);
        let columns = usize::from(main.width.saturating_sub(title).saturating_sub(12));
        let tl = app.timeline(columns);
        ui::timeline::render(frame, main, &tl, app.cursor, app.timeline_scroll);
    } else {
        ui::list::render(frame, app, main);
        ui::statusbar::render_footer(frame, app, footer);
    }
    ui::statusbar::render(frame, app, status);

    if app.show_help_panel && app.overlay == Overlay::None {
        ui::help::render_panel(frame, main, app);
    }

    // The task form is a full panel; a prompt is a single-line strip above the
    // status bar.
    match &app.overlay {
        Overlay::Form(form) => {
            let caret = ui::form::render(frame, area, form);
            *cursor_out = caret;
        }
        Overlay::Prompt(prompt) => {
            // Sit just above the status line rather than on top of it.
            let strip = Rect {
                x: area.x,
                y: status.y.saturating_sub(PROMPT_HEIGHT),
                width: area.width,
                height: PROMPT_HEIGHT.min(status.y),
            };
            *cursor_out = ui::prompt::render(frame, strip, prompt);
        }
        _ => {}
    }

    if let Some(kind) = match app.overlay {
        Overlay::Confirm(k) => Some(k),
        _ => None,
    } {
        let text = match kind {
            crate::app::ConfirmKind::Delete => "Delete this task and its subtasks? y/n",
            crate::app::ConfirmKind::BulkDelete => "Delete the selection? y/n",
        };
        let width = text.chars().count() as u16 + 4;
        let [_, box_area, _] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(width),
            Constraint::Fill(1),
        ])
        .areas(main);
        let rows = 3u16;
        let box_area = Rect {
            x: box_area.x,
            y: box_area.y + box_area.height / 2 - 1,
            width: box_area.width,
            height: rows.min(box_area.height),
        };
        frame.render_widget(ratatui::widgets::Clear, box_area);
        frame.render_widget(
            ratatui::widgets::Paragraph::new(text).block(ui::block("confirm", true)),
            box_area,
        );
    }

    if app.overlay == Overlay::Help {
        ui::help::render(frame, area, app, app.help_scroll);
    }

    // A trailing newline style keeps the frame tidy when the overlay closes.
    let _ = Style::default();
}

/// Show or hide the hardware cursor, and place it where the form's caret is.
///
/// Called after every draw so the real terminal caret tracks the focused field,
/// which a buffer-only renderer cannot do.
fn set_cursor(caret: Option<crate::ui::form::Caret>) {
    use ratatui::crossterm::cursor::{Hide, MoveTo, Show};
    use ratatui::crossterm::execute;

    let mut out = io::stdout();
    match caret {
        Some(c) => {
            let _ = execute!(out, MoveTo(c.x, c.y), Show);
        }
        None => {
            let _ = execute!(out, Hide);
        }
    }
}
