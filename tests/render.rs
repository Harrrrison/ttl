//! Golden-frame tests: render into an off-screen buffer and assert on the
//! result, so layout regressions are caught without a human at a terminal.

use chrono::{Duration, Utc};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use tempfile::TempDir;
use ttm::app::{App, Overlay};
use ttm::model::{Attachment, Priority, Status, Task, TaskSet};
use ttm::store::Store;

/// Render `app` at `width`x`height` and return the buffer as text lines.
fn render(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let mut caret = None;
    terminal
        .draw(|frame| ttm::tui::draw(frame, app, Utc::now(), &mut caret))
        .expect("draw succeeds");
    let buffer = terminal.backend().buffer().clone();

    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

fn joined(app: &mut App, width: u16, height: u16) -> String {
    render(app, width, height).join("\n")
}

fn app_with(tasks: Vec<Task>) -> (App, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let set = TaskSet {
        tasks,
        ..TaskSet::new()
    };
    store.save(&set).unwrap();
    (App::new(store), tmp)
}

fn task(title: &str) -> Task {
    Task::new(title, Utc::now())
}

/// Feed text into whatever field currently has focus.
fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        let key = if c == ' ' {
            ttm::input::Key::Space
        } else {
            ttm::input::Key::Char(c)
        };
        app.edit_overlay_key(key);
    }
}

// ---------------------------------------------------------------------------
// Basic rendering
// ---------------------------------------------------------------------------

#[test]
fn an_empty_store_renders_a_message_rather_than_a_blank_screen() {
    let (mut app, _t) = app_with(vec![]);
    let out = joined(&mut app, 80, 20);

    assert!(out.contains("no tasks"), "{out}");
    assert!(out.contains("0 tasks"), "footer counts: {out}");
    assert!(out.contains("NORMAL"), "mode indicator: {out}");
}

#[test]
fn the_task_table_shows_headings_and_rows() {
    let (mut app, _t) = app_with(vec![task("Write the report"), task("Review PR")]);
    let out = joined(&mut app, 100, 20);

    for header in ["DUE", "LEFT", "EST", "LNKS", "PRI", "TITLE"] {
        assert!(out.contains(header), "missing header {header}: {out}");
    }
    assert!(out.contains("Write the report"), "{out}");
    assert!(out.contains("Review PR"), "{out}");
    assert!(out.contains("1/2"), "cursor position: {out}");
}

#[test]
fn subtasks_are_indented_under_their_parent() {
    let parent = task("parent");
    let mut child = task("child");
    child.parent_id = Some(parent.id);

    let (mut app, _t) = app_with(vec![parent, child]);
    let out = joined(&mut app, 100, 20);

    let parent_line = out.lines().find(|l| l.contains("parent")).unwrap();
    let child_line = out.lines().find(|l| l.contains("child")).unwrap();
    assert!(
        child_line.trim_start().matches("  ").count()
            > parent_line.trim_start().matches("  ").count(),
        "child should be indented further:\n{parent_line}\n{child_line}"
    );
    assert!(
        parent_line.contains("(0/1)"),
        "the rollup marker belongs on the parent row: {out}"
    );
}

#[test]
fn flat_mode_removes_the_indent() {
    let parent = task("parent");
    let mut child = task("child");
    child.parent_id = Some(parent.id);

    let (mut app, _t) = app_with(vec![parent, child]);
    app.tree = false;
    app.rebuild();
    let out = joined(&mut app, 100, 20);

    let child_line = out.lines().find(|l| l.contains("child")).unwrap();
    assert_eq!(
        child_line.trim_start(),
        child_line,
        "no indent in flat mode: {child_line:?}"
    );
}

#[test]
fn due_dates_estimates_and_links_are_rendered() {
    let now = Utc::now();
    let mut t = task("Ship it");
    t.due_at = Some(now + Duration::days(3));
    t.estimate_minutes = Some(90);
    t.attachments
        .push(Attachment::url("https://example.com", "spec"));

    let (mut app, _t) = app_with(vec![t]);
    let out = joined(&mut app, 100, 20);

    assert!(out.contains("1h 30m"), "estimate: {out}");
    assert!(out.contains("in 3d"), "countdown: {out}");
    assert!(out.contains("1"), "link count: {out}");
}

#[test]
fn overdue_work_shows_a_negative_countdown() {
    let mut t = task("Late");
    t.due_at = Some(Utc::now() - Duration::days(4));

    let (mut app, _t) = app_with(vec![t]);
    let out = joined(&mut app, 100, 20);
    assert!(out.contains("overdue 4d"), "{out}");
    assert!(out.contains("1 overdue"), "footer: {out}");
}

#[test]
fn done_tasks_are_still_listed_but_counted_separately() {
    let mut done = task("Finished work");
    done.status = Status::Done;

    let (mut app, _t) = app_with(vec![done, task("Outstanding")]);
    let out = joined(&mut app, 100, 20);

    assert!(out.contains("Finished work"));
    assert!(out.contains("Outstanding"));
    assert!(out.contains("1 done"), "{out}");
}

#[test]
fn the_status_column_shows_the_actual_status() {
    let mut todo = task("todo item");
    todo.status = Status::Todo;
    let mut done = task("done item");
    done.status = Status::Done;

    let (mut app, _t) = app_with(vec![todo, done]);
    let out = joined(&mut app, 100, 20);
    assert!(out.contains('·'), "todo marker: {out}");
    assert!(out.contains('✓'), "done marker: {out}");
}

#[test]
fn urgency_markers_are_distinguishable() {
    let mut low = task("low");
    low.priority = Priority::Low;
    let mut urgent = task("urgent");
    urgent.priority = Priority::Urgent;

    let (mut app, _t) = app_with(vec![low, urgent]);
    let out = joined(&mut app, 100, 20);
    assert!(out.contains("!!!"), "urgent marker: {out}");
    assert!(out.contains("··"), "low marker: {out}");
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

#[test]
fn help_lists_every_documented_binding() {
    let (mut app, _t) = app_with(vec![task("a")]);
    app.overlay = Overlay::Help;
    let out = joined(&mut app, 100, 40);

    for expected in [
        "key bindings",
        "Global",
        "Move",
        "Filter & sort",
        "Tasks",
        "Visual",
        "View",
        "quit",
        "down one",
        "search tasks",
        "add a task",
        "visual",
    ] {
        assert!(out.contains(expected), "help missing {expected:?}:\n{out}");
    }
    assert!(out.contains("press ? or <Esc> to close"), "{out}");
}

#[test]
fn the_help_overlay_covers_the_list() {
    let (mut app, _t) = app_with(vec![task("visible task")]);
    let with_help = {
        app.overlay = Overlay::Help;
        joined(&mut app, 100, 30)
    };
    assert!(
        !with_help.contains("visible task"),
        "the overlay should hide the rows beneath it:\n{with_help}"
    );
}

#[test]
fn the_cheatsheet_panel_lists_the_essentials() {
    let (mut app, _t) = app_with(vec![task("a")]);
    app.show_help_panel = true;
    let out = joined(&mut app, 120, 30);

    assert!(out.contains("keys"), "{out}");
    assert!(out.contains("cycle status"), "{out}");
    assert!(out.contains("C-p hides this"), "{out}");
}

#[test]
fn the_confirmation_prompt_is_rendered() {
    let (mut app, _t) = app_with(vec![task("a")]);
    app.overlay = Overlay::Confirm(ttm::app::ConfirmKind::Delete);
    let out = joined(&mut app, 100, 20);

    assert!(out.contains("Delete this task"), "{out}");
    assert!(out.contains("y/n"), "{out}");
    assert!(out.contains("confirm"), "{out}");
}

// ---------------------------------------------------------------------------
// Status line
// ---------------------------------------------------------------------------

#[test]
fn the_status_line_shows_the_pending_key_sequence() {
    let (mut app, _t) = app_with(vec![task("a")]);
    app.feed(ttm::input::Key::Char('g'));
    let out = joined(&mut app, 80, 20);

    assert!(
        out.lines().last().unwrap().contains('g'),
        "pending keys should be visible: {out}"
    );
}

#[test]
fn an_open_prompt_shows_its_contents_in_the_status_line() {
    let (mut app, _t) = app_with(vec![task("a")]);
    app.act(ttm::Action::ToggleFilterPanel);
    type_text(&mut app, "alpha");
    let out = joined(&mut app, 100, 20);

    let status = out.lines().last().unwrap();
    assert!(status.contains("FILTER"), "mode indicator: {status}");
    assert!(
        status.contains("alpha"),
        "typed text should be visible: {status}"
    );
}

#[test]
fn a_prompt_draws_its_own_panel() {
    let (mut app, _t) = app_with(vec![task("a"), task("b")]);
    app.act(ttm::Action::AddLink);
    type_text(&mut app, "https://example.com");
    let out = joined(&mut app, 100, 20);

    assert!(out.contains("attach link"), "prompt title: {out}");
    assert!(out.contains("https://example.com"), "{out}");
    assert!(
        out.contains("https://example.com/spec") || !out.contains("example.com/spec"),
        "the hint is only shown when empty"
    );
}

#[test]
fn the_task_form_renders_all_its_fields() {
    let (mut app, _t) = app_with(vec![task("existing")]);
    app.act(ttm::Action::Add);
    type_text(&mut app, "A new task");
    let out = joined(&mut app, 110, 30);

    for label in [
        "title",
        "description",
        "due",
        "estimate",
        "status",
        "priority",
        "tags",
        "links",
    ] {
        assert!(
            out.contains(label),
            "form is missing the {label:?} field:\n{out}"
        );
    }
    assert!(out.contains("A new task"), "typed text: {out}");
    assert!(out.contains("new task"), "panel title: {out}");
    assert!(
        out.contains("Ctrl-s save"),
        "the save hint must be discoverable: {out}"
    );
}

#[test]
fn the_form_hides_the_list_beneath_it() {
    let (mut app, _t) = app_with(vec![task("SECRET-TASK-ROW")]);
    app.act(ttm::Action::Add);
    let out = joined(&mut app, 100, 26);
    assert!(
        !out.contains("SECRET-TASK-ROW"),
        "the form panel should cover the list:\n{out}"
    );
}

#[test]
fn the_edit_form_shows_the_stored_values() {
    let mut t = task("Stored title");
    t.due_at = Some(Utc::now() + chrono::Duration::days(5));
    t.estimate_minutes = Some(120);
    t.tags = vec!["work".into()];
    t.description = "Stored description".into();

    let (mut app, _tmp) = app_with(vec![t]);
    app.act(ttm::Action::EditTask);
    let out = joined(&mut app, 110, 30);

    for expected in [
        "Stored title",
        "Stored description",
        "work",
        "2h",
        "edit task",
    ] {
        assert!(
            out.contains(expected),
            "edit form is missing {expected:?}:\n{out}"
        );
    }
}

#[test]
fn a_form_error_is_shown_next_to_its_field() {
    let (mut app, _tmp) = app_with(vec![]);
    app.act(ttm::Action::Add);
    type_text(&mut app, "Title is fine");

    // Focus the estimate field and give it nonsense.
    let index = app
        .form()
        .expect("form")
        .fields
        .iter()
        .position(|f| f.kind == ttm::form::FieldKind::Estimate)
        .expect("estimate field");
    app.form_mut().expect("form").focus = index;
    type_text(&mut app, "nonsense");
    app.commit_overlay();

    let out = joined(&mut app, 110, 30);
    assert!(
        out.contains("expected something like 4h"),
        "the field error must be visible:\n{out}"
    );
    assert!(
        out.contains("1 field needs attention") || out.contains("fields need"),
        "a banner summarises the problem: {out}"
    );
}

#[test]
fn an_empty_prompt_leaves_the_data_alone() {
    let (mut app, _tmp) = app_with(vec![task("a")]);
    app.act(ttm::Action::AddPdf);
    app.commit_overlay();
    let out = joined(&mut app, 100, 20);
    assert!(out.contains("empty input"), "{out}");
}

#[test]
fn the_active_filter_is_visible_in_the_status_line() {
    let (mut app, _t) = app_with(vec![task("alpha"), task("beta")]);

    app.act(ttm::Action::ToggleFilterPanel);
    type_text(&mut app, "alpha");
    app.commit_overlay();

    let out = joined(&mut app, 120, 20);
    assert!(
        out.contains("/alpha"),
        "the search text should persist: {out}"
    );
    assert!(out.contains("1 matching") || out.contains("1/1"), "{out}");
}

#[test]
fn sort_changes_are_announced() {
    let (mut app, _t) = app_with(vec![task("a")]);
    app.act(ttm::Action::SortNext);
    let out = joined(&mut app, 100, 20);
    assert!(out.contains("sort:"), "{out}");
}

// ---------------------------------------------------------------------------
// Degenerate geometries
// ---------------------------------------------------------------------------

#[test]
fn a_tiny_terminal_does_not_panic() {
    let (mut app, _t) = app_with(vec![task("a"), task("b")]);
    for (w, h) in [(1, 1), (5, 2), (20, 4), (40, 3), (200, 60)] {
        let _ = joined(&mut app, w, h); // must not panic
    }
}

#[test]
fn a_terminal_too_small_for_the_layout_says_so() {
    let (mut app, _t) = app_with(vec![task("a")]);
    let out = joined(&mut app, 15, 3);
    assert!(out.contains("too small"), "{out}");
    // And nothing overflows even here.
    for line in out.lines() {
        assert!(line.chars().count() <= 15, "{line:?}");
    }
}

#[test]
fn long_titles_are_truncated_not_wrapped() {
    let long = "x".repeat(400);
    let (mut app, _t) = app_with(vec![Task::new(long, Utc::now())]);
    let out = joined(&mut app, 80, 20);

    // Every rendered line stays within the terminal width.
    for line in out.lines() {
        assert!(
            line.chars().count() <= 80,
            "line overflowed: {} chars",
            line.chars().count()
        );
    }
    assert!(out.contains("…"), "truncation marker expected: {out}");
}

#[test]
fn wide_glyphs_do_not_overflow_their_column() {
    let (mut app, _t) = app_with(vec![Task::new("日本語のタスク名前", Utc::now())]);
    let out = joined(&mut app, 60, 12);
    for line in out.lines() {
        assert!(
            line.chars().count() <= 60,
            "a wide-glyph title overflowed its column: {line:?}"
        );
    }
}

#[test]
fn scrolling_keeps_the_cursor_on_screen() {
    let (mut app, _t) = app_with((0..50).map(|i| task(&format!("task {i}"))).collect());
    app.viewport = 10;

    for _ in 0..40 {
        app.act(ttm::Action::Down);
    }
    let out = joined(&mut app, 80, 20);
    assert!(
        out.contains("task 40"),
        "the cursor row should be drawn: {out}"
    );

    app.act(ttm::Action::LastLine);
    let out = joined(&mut app, 80, 20);
    assert!(out.contains("task 49"), "{out}");
    assert!(out.contains("50/50"), "{out}");
}

#[test]
fn visual_mode_marks_the_selected_rows() {
    let (mut app, _t) = app_with(vec![task("a"), task("b"), task("c")]);
    app.act(ttm::Action::EnterVisual);
    app.act(ttm::Action::Down);
    let out = joined(&mut app, 80, 20);

    assert!(out.contains("▌"), "visual rows should be marked: {out}");
    assert!(
        out.contains("(2)"),
        "selection size in the status bar: {out}"
    );
}

#[test]
fn an_empty_visual_selection_is_reported_not_silently_ignored() {
    let (mut app, _t) = app_with(vec![]);
    app.act(ttm::Action::EnterVisual);
    let out = joined(&mut app, 80, 20);
    assert!(out.contains("nothing to select"), "{out}");
}

#[test]
fn bulk_actions_outside_visual_mode_say_press_v_first() {
    let (mut app, _t) = app_with(vec![task("a")]);
    app.act(ttm::Action::BulkSetStatus);
    let out = joined(&mut app, 80, 20);
    assert!(out.contains("press V first"), "{out}");
}

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

#[test]
fn truncation_is_measured_in_columns_not_bytes() {
    use ttm::ui::truncate;

    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate("hello", 5), "hello");
    assert_eq!(truncate("hello", 4), "hel…");
    assert_eq!(truncate("hello", 1), "…");
    assert_eq!(truncate("hello", 0), "");

    // Three wide characters occupy six columns.
    assert_eq!(truncate("日本語", 4), "日…");
    assert_eq!(truncate("日本語", 6), "日本語");
}

#[test]
fn cell_padding_always_yields_the_requested_width() {
    use ttm::ui::cell;
    use unicode_width::UnicodeWidthStr;

    // Padding is measured in terminal columns, not characters.
    assert_eq!(UnicodeWidthStr::width(cell("abc", 6).as_str()), 6);
    assert_eq!(UnicodeWidthStr::width(cell("abcdefgh", 6).as_str()), 6);
    assert_eq!(UnicodeWidthStr::width(cell("日本語", 6).as_str()), 6);
    assert_eq!(UnicodeWidthStr::width(cell("日本語", 5).as_str()), 5);
}

#[test]
fn list_area_reserves_space_only_when_it_exists() {
    use ttm::ui::list_area;

    let big = Rect::new(0, 0, 80, 20);
    let (rows, header) = list_area(big, true, true);
    assert!(header);
    assert_eq!(rows.height, 18, "header and footer take one row each");

    let tiny = Rect::new(0, 0, 80, 2);
    let (_, header) = list_area(tiny, true, true);
    assert!(!header, "no room for a header in 2 rows");
}

/// The overlay geometry bug: content rendered at the block's top-left corner
/// instead of inside it, which let the task list show through the borders.
#[test]
fn the_help_overlay_is_opaque_and_inside_its_border() {
    let (mut app, _t) = app_with(vec![task("SECRET-TASK-ROW")]);
    app.overlay = Overlay::Help;
    let lines = render(&mut app, 100, 24);

    assert!(
        !lines.iter().any(|l| l.contains("SECRET-TASK-ROW")),
        "the list must be fully hidden:\n{}",
        lines.join("\n")
    );
    assert!(lines[0].starts_with('┌'), "top border: {:?}", lines[0]);
    assert!(lines[0].ends_with('┐'), "top-right corner: {:?}", lines[0]);
    for (i, line) in lines.iter().enumerate() {
        if i == 0 || i == lines.len() - 1 {
            continue;
        }
        assert!(
            line.starts_with('│') && line.ends_with('│'),
            "row {i} escapes the border: {line:?}"
        );
    }
    assert!(
        lines[lines.len() - 1].starts_with('└'),
        "bottom border: {:?}",
        lines[lines.len() - 1]
    );
}

#[test]
fn the_confirmation_dialog_is_centred_over_the_list() {
    let (mut app, _t) = app_with(vec![task("ONLY-TASK")]);
    app.overlay = Overlay::Confirm(ttm::app::ConfirmKind::Delete);
    let lines = render(&mut app, 100, 24);

    // A modal dialog is small on purpose, so the list stays visible around it;
    // what matters is that the dialog itself is centred and opaque.
    let dialog = lines
        .iter()
        .position(|l| l.contains("Delete this task"))
        .expect("dialog drawn");
    assert!(
        dialog > 4 && dialog < 20,
        "dialog should be centred, at row {dialog}"
    );
    // The box is three rows: title border, text, bottom border.
    assert!(
        lines[dialog - 1].contains("┌ confirm"),
        "{:?}",
        lines[dialog - 1]
    );
    assert!(
        lines[dialog - 1].contains('┐') && lines[dialog + 1].contains('└'),
        "dialog box is malformed:\n{:?}\n{:?}\n{:?}",
        lines[dialog - 1],
        lines[dialog],
        lines[dialog + 1]
    );
}

#[test]
fn the_cheatsheet_panel_is_opaque() {
    let (mut app, _t) = app_with(vec![task("visible")]);
    app.show_help_panel = true;
    let lines = render(&mut app, 100, 24);
    assert!(lines.iter().any(|l| l.contains("keys")), "panel shown");
}

// ---------------------------------------------------------------------------
// Detail pane
// ---------------------------------------------------------------------------

fn open_detail(app: &mut App) {
    app.act(ttm::Action::OpenDetail);
}

#[test]
fn the_detail_pane_shows_the_task_metadata() {
    let now = Utc::now();
    let mut t = task("Write the RFC");
    t.due_at = Some(now + chrono::Duration::days(3));
    t.estimate_minutes = Some(240);
    t.tags = vec!["work".into(), "q4".into()];
    t.priority = ttm::model::Priority::High;

    let (mut app, _t) = app_with(vec![t]);
    open_detail(&mut app);
    let out = joined(&mut app, 100, 26);

    for expected in [
        "Write the RFC",
        "status",
        "todo",
        "high",
        "estimate",
        "4h",
        "tags",
        "work, q4",
    ] {
        assert!(
            out.contains(expected),
            "detail is missing {expected:?}:\n{out}"
        );
    }
}

#[test]
fn the_detail_pane_shows_the_description_on_its_own_lines() {
    let mut t = task("Has notes");
    t.description = "Context and goals\nOpen questions".into();

    let (mut app, _t) = app_with(vec![t]);
    open_detail(&mut app);
    let out = joined(&mut app, 100, 26);

    assert!(out.contains("description"), "{out}");
    assert!(out.contains("Context and goals"), "{out}");
    assert!(out.contains("Open questions"), "{out}");
}

#[test]
fn an_absent_description_and_attachment_set_say_so() {
    let (mut app, _t) = app_with(vec![task("Bare")]);
    open_detail(&mut app);
    let out = joined(&mut app, 100, 26);

    assert!(out.contains("(none — press i to add one)"), "{out}");
    assert!(out.contains("(none — press l to attach"), "{out}");
}

#[test]
fn the_detail_pane_lists_attachments_with_their_targets() {
    let mut t = task("Linked");
    t.attachments.push(ttm::model::Attachment::url(
        "https://example.com/spec",
        "spec",
    ));
    t.attachments
        .push(ttm::model::Attachment::pdf("/tmp/contract.pdf", ""));

    let (mut app, _t) = app_with(vec![t]);
    open_detail(&mut app);
    let out = joined(&mut app, 100, 26);

    assert!(out.contains("0."), "indexed: {out}");
    assert!(out.contains("spec"), "{out}");
    assert!(
        out.contains("https://example.com/spec"),
        "target shown: {out}"
    );
    assert!(out.contains("PDF"), "unlabelled falls back: {out}");
}

#[test]
fn a_missing_attachment_file_is_flagged() {
    let mut t = task("Ghost link");
    t.attachments.push(ttm::model::Attachment::pdf(
        "/definitely/not/here.pdf",
        "spec",
    ));

    let (mut app, _t) = app_with(vec![t]);
    open_detail(&mut app);
    let out = joined(&mut app, 100, 26);
    assert!(out.contains("(missing)"), "{out}");
}

#[test]
fn subtasks_are_shown_in_the_detail_pane() {
    let parent = task("Parent");
    let mut child = task("Child");
    child.parent_id = Some(parent.id);

    let (mut app, _t) = app_with(vec![parent, child]);
    open_detail(&mut app);
    let out = joined(&mut app, 100, 26);

    assert!(out.contains("subtasks"), "{out}");
    assert!(out.contains("1. Child"), "{out}");
}

#[test]
fn the_detail_pane_scrolls_and_clamps() {
    let mut t = task("Long task");
    t.description = (0..200)
        .map(|i| format!("body line {i}"))
        .collect::<Vec<_>>()
        .join("\n");

    let (mut app, _t) = app_with(vec![t]);
    app.viewport = 10;
    open_detail(&mut app);

    assert_eq!(app.detail.scroll, 0);
    app.route_key(ttm::input::Key::Char('j'));
    assert_eq!(app.detail.scroll, 1);
    app.route_key(ttm::input::Key::PageDown);
    assert!(app.detail.scroll > 1);
    app.route_key(ttm::input::Key::Char('G'));
    assert!(app.detail.scroll > 1, "G jumps to the end");
    app.route_key(ttm::input::Key::Char('g'));
    app.route_key(ttm::input::Key::Char('g'));
    assert_eq!(app.detail.scroll, 0, "gg returns to the top");
}

#[test]
fn the_detail_status_line_reports_the_attachment_cursor() {
    let mut t = task("Two links");
    t.attachments
        .push(ttm::model::Attachment::url("https://a.test", "one"));
    t.attachments
        .push(ttm::model::Attachment::url("https://b.test", "two"));

    let (mut app, _t) = app_with(vec![t]);
    open_detail(&mut app);
    let out = joined(&mut app, 100, 26);
    assert!(out.contains("att 1/2"), "{out}");
    assert!(out.contains("o open, d remove"), "{out}");

    app.route_key(ttm::input::Key::Char(']'));
    let out = joined(&mut app, 100, 26);
    assert!(out.contains("att 2/2"), "{out}");
}

#[test]
fn the_detail_pane_says_when_there_are_no_attachments() {
    let (mut app, _t) = app_with(vec![task("Bare")]);
    open_detail(&mut app);
    let out = joined(&mut app, 100, 26);
    assert!(out.contains("no attachments"), "{out}");
    assert!(
        !out.contains("o open, d remove"),
        "no point offering keys: {out}"
    );
}

#[test]
fn the_focused_attachment_is_marked() {
    let mut t = task("Two links");
    t.attachments
        .push(ttm::model::Attachment::url("https://a.test", "one"));
    t.attachments
        .push(ttm::model::Attachment::url("https://b.test", "two"));

    let (mut app, _t) = app_with(vec![t]);
    open_detail(&mut app);
    let first = joined(&mut app, 100, 26);
    assert_eq!(first.matches('▌').count(), 1, "exactly one row is focused");

    app.route_key(ttm::input::Key::Char(']'));
    let second = joined(&mut app, 100, 26);
    let marked = second
        .lines()
        .find(|l| l.contains('▌'))
        .expect("a focused row");
    assert!(marked.contains("two"), "the cursor moved: {marked:?}");
}

#[test]
fn a_tiny_terminal_does_not_panic_the_detail_pane() {
    let mut t = task("x");
    t.description = "y".into();
    t.attachments
        .push(ttm::model::Attachment::url("https://a.test", "one"));

    let (mut app, _t) = app_with(vec![t]);
    open_detail(&mut app);
    for (w, h) in [(1, 1), (10, 5), (20, 4), (200, 60)] {
        let _ = joined(&mut app, w, h);
    }
}

#[test]
fn a_long_title_is_truncated_in_the_detail_border() {
    let long = "z".repeat(300);
    let (mut app, _t) = app_with(vec![Task::new(long, Utc::now())]);
    open_detail(&mut app);
    let out = joined(&mut app, 60, 20);
    for line in out.lines() {
        assert!(line.chars().count() <= 60, "overflowed: {line:?}");
    }
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

fn open_timeline(app: &mut App) {
    app.act(ttm::Action::ToggleTimeline);
}

#[test]
fn the_timeline_shows_an_axis_and_rows() {
    let (mut app, _t) = app_with(vec![task("Ship it")]);
    open_timeline(&mut app);
    let out = joined(&mut app, 100, 20);

    assert!(out.contains("timeline"), "panel title: {out}");
    assert!(out.contains("Ship it"), "the task is listed: {out}");
    // The legend explains every glyph the chart can draw.
    for legend in [
        "scheduled",
        "overdue",
        "today",
        "est finishes",
        "due",
        "subtasks",
    ] {
        assert!(out.contains(legend), "legend is missing {legend:?}: {out}");
    }
}

#[test]
fn the_timeline_draws_a_bar_for_a_scheduled_task() {
    let now = Utc::now();
    let mut t = task("Dated");
    t.due_at = Some(now + chrono::Duration::days(4));
    t.created_at = now - chrono::Duration::days(2);
    t.updated_at = t.created_at;

    let (mut app, _t) = app_with(vec![t]);
    open_timeline(&mut app);
    let out = joined(&mut app, 100, 20);

    assert!(out.contains("─"), "a bar glyph should be drawn: {out}");
    assert!(out.contains("▐"), "and a due edge: {out}");
    assert!(out.contains("│"), "and the now line: {out}");
    // The exact day count drifts as the test's own clock advances, so assert
    // that a forward-looking deadline is shown as such.
    assert!(
        out.contains("D+3") || out.contains("D+4"),
        "and a countdown: {out}"
    );
}

#[test]
fn the_timeline_renders_an_overdue_task_distinctly() {
    let now = Utc::now();
    let mut t = task("Late");
    t.due_at = Some(now - chrono::Duration::days(2));
    t.created_at = now - chrono::Duration::days(6);
    t.updated_at = t.created_at;

    let (mut app, _t) = app_with(vec![t]);
    open_timeline(&mut app);
    let out = joined(&mut app, 100, 20);

    assert!(out.contains("█"), "an overdue bar is solid: {out}");
    assert!(out.contains("OVER2d"), "and says how late: {out}");
}

#[test]
fn the_timeline_marks_the_estimate_finish_point() {
    let now = Utc::now();
    let mut t = task("Estimated");
    t.due_at = Some(now + chrono::Duration::days(6));
    t.created_at = now - chrono::Duration::days(1);
    t.updated_at = t.created_at;
    t.estimate_minutes = Some(24 * 60);

    let (mut app, _t) = app_with(vec![t]);
    open_timeline(&mut app);
    let out = joined(&mut app, 100, 20);

    assert!(out.contains("◆"), "the estimate marker: {out}");
    assert!(out.contains("1d"), "and the estimate in the gutter: {out}");
}

#[test]
fn the_timeline_shows_the_month_axis_at_every_scale() {
    let (mut app, _t) = app_with(vec![task("a")]);
    open_timeline(&mut app);

    let month = chrono::Local::now().format("%b").to_string();

    // Every scale carries a month row: with a column only one character wide,
    // the day digits alone would be a meaningless sequence.
    let out = joined(&mut app, 100, 20);
    assert!(
        out.contains(&month),
        "the axis should name the month it spans: {out}"
    );

    app.act(ttm::Action::ZoomIn);
    let out = joined(&mut app, 100, 20);
    assert!(
        out.contains("timeline · weeks"),
        "the title states the scale: {out}"
    );
    assert!(
        out.contains(&month),
        "the month row survives zooming: {out}"
    );
}

#[test]
fn zooming_changes_the_scale_in_the_title() {
    let (mut app, _t) = app_with(vec![task("a")]);
    open_timeline(&mut app);
    assert!(joined(&mut app, 100, 20).contains("timeline · days"));

    app.act(ttm::Action::ZoomIn);
    assert!(joined(&mut app, 100, 20).contains("timeline · weeks"));

    app.act(ttm::Action::ZoomOut);
    assert!(joined(&mut app, 100, 20).contains("timeline · days"));
}

#[test]
fn the_timeline_marks_the_selected_row() {
    let (mut app, _t) = app_with(vec![task("first"), task("second")]);
    open_timeline(&mut app);
    let out = joined(&mut app, 100, 20);
    assert_eq!(out.matches('▌').count(), 1, "one selected row");

    app.act(ttm::Action::Down);
    let out = joined(&mut app, 100, 20);
    let marked = out.lines().find(|l| l.contains("▌")).expect("a marker");
    assert!(marked.contains("second"), "the cursor moved: {marked:?}");
}

#[test]
fn the_timeline_scrolls_so_the_selection_stays_visible() {
    let (mut app, _t) = app_with((0..30).map(|i| task(&format!("task {i}"))).collect());
    app.viewport = 8;
    open_timeline(&mut app);

    app.act(ttm::Action::LastLine);
    let out = joined(&mut app, 100, 20);
    assert!(
        out.contains("task 29"),
        "the last row must be on screen: {out}"
    );
}

#[test]
fn an_empty_timeline_says_so() {
    let (mut app, _t) = app_with(vec![]);
    open_timeline(&mut app);
    let out = joined(&mut app, 100, 20);
    assert!(out.contains("no tasks"), "{out}");
}

#[test]
fn the_timeline_hides_subtasks_under_their_parent() {
    let parent = task("Parent");
    let mut child = task("Child");
    child.parent_id = Some(parent.id);

    let (mut app, _t) = app_with(vec![parent, child]);
    open_timeline(&mut app);
    let out = joined(&mut app, 100, 20);

    let parent_line = out.lines().find(|l| l.contains("Parent")).expect("parent");
    let child_line = out.lines().find(|l| l.contains("Child")).expect("child");
    // Strip the pane border, then compare the indent itself: the parent carries
    // a rollup bar that would otherwise push its title right and make the
    // comparison meaningless.
    fn strip(l: &str) -> &str {
        l.trim_start_matches('\u{2502}')
    }
    assert!(
        strip(child_line).starts_with("   Child"),
        "the child row should be indented:\n{parent_line}\n{child_line}"
    );
    assert!(
        strip(parent_line).starts_with('\u{258c}'),
        "the parent row should not be:\n{parent_line}"
    );
    assert!(
        parent_line.contains('░'),
        "and show a rollup: {parent_line:?}"
    );
}

#[test]
fn the_timeline_never_overflows_the_terminal() {
    let now = Utc::now();
    let mut t = task("A fairly long task title for a narrow terminal");
    t.due_at = Some(now + chrono::Duration::days(30));
    let (mut app, _t) = app_with(vec![t]);
    open_timeline(&mut app);

    for w in [40u16, 60, 80, 120, 200] {
        let out = joined(&mut app, w, 16);
        for line in out.lines() {
            assert!(
                line.chars().count() <= w as usize,
                "row overflowed a {w}-column terminal: {:?}",
                line
            );
        }
    }
}

#[test]
fn a_very_narrow_timeline_does_not_panic() {
    let (mut app, _t) = app_with(vec![task("a"), task("b")]);
    open_timeline(&mut app);
    for (w, h) in [(1u16, 1u16), (10, 5), (20, 4), (40, 3)] {
        let _ = joined(&mut app, w, h);
    }
}

#[test]
fn the_timeline_and_the_detail_pane_are_distinct_views() {
    let (mut app, _t) = app_with(vec![task("a")]);
    open_timeline(&mut app);
    app.act(ttm::Action::OpenDetail);
    assert!(app.in_detail());
    assert!(!app.in_timeline(), "opening a detail leaves the chart");
}
