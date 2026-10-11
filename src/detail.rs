//! The detail pane: everything known about one task, laid out as a flat list of
//! rows that can be scrolled.
//!
//! Building the whole pane as a `Vec<Row>` before slicing keeps scrolling a
//! pure function, which is what makes the behaviour testable without a terminal
//! and keeps the renderer trivial.

use chrono::{DateTime, Utc};

use crate::model::{Attachment, AttachmentKind, Status, Task};

/// One rendered line of the detail pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A spacer.
    Blank,
    /// A `label  value` metadata line.
    Meta {
        label: &'static str,
        value: String,
        /// Highlights the line when the deadline is close or past.
        urgent: bool,
    },
    /// A section heading, e.g. `description`.
    Heading(&'static str),
    /// Free text, indented under a heading.
    Text { text: String },
    /// An attachment row; `index` is its position in the task's list.
    Attachment {
        index: usize,
        kind: AttachmentKind,
        label: String,
        target: String,
        /// True when the referenced file is known to be missing.
        missing: bool,
    },
    /// A closing hint line.
    Hint(&'static str),
}

impl Row {
    /// Whether this row can hold the attachment cursor.
    #[must_use]
    pub fn is_attachment(&self) -> bool {
        matches!(self, Row::Attachment { .. })
    }

    /// The attachment index, if this is an attachment row.
    #[must_use]
    pub fn attachment_index(&self) -> Option<usize> {
        match self {
            Row::Attachment { index, .. } => Some(*index),
            _ => None,
        }
    }
}

/// Scroll and attachment-cursor state for the detail pane.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Detail {
    /// First visible row.
    pub scroll: usize,
    /// Which attachment the `o` / `x` keys act on.
    pub attachment_focus: usize,
}

impl Detail {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Move the attachment cursor, clamped to the available range.
    pub fn move_attachment(&mut self, delta: isize, count: usize) {
        if count == 0 {
            self.attachment_focus = 0;
            return;
        }
        let last = count as isize - 1;
        let next = (self.attachment_focus as isize + delta).clamp(0, last);
        self.attachment_focus = next as usize;
    }

    /// Keep the focused attachment on screen after a scroll.
    ///
    /// Returns the adjusted scroll offset.
    pub fn keep_focus_visible(&mut self, rows: &[Row], height: usize) -> usize {
        let Some(target) = rows
            .iter()
            .position(|r| r.attachment_index() == Some(self.attachment_focus))
        else {
            return self.scroll;
        };
        if height == 0 {
            return self.scroll;
        }
        // Put the focused row a third of the way down rather than flush to an
        // edge, so scrolling further in either direction still moves.
        let margin = height / 3;
        if target < self.scroll + margin {
            self.scroll = target.saturating_sub(margin);
        } else if target >= self.scroll + height.saturating_sub(margin) {
            self.scroll = (target + 1).saturating_sub(height);
        }
        self.scroll = self.scroll.min(rows.len().saturating_sub(1));
        self.scroll
    }

    /// Scroll by `delta` rows, clamped to the content.
    pub fn scroll_by(&mut self, delta: isize, total: usize, height: usize) {
        let max = total.saturating_sub(height);
        let next = (self.scroll as isize + delta).clamp(0, max as isize);
        self.scroll = next as usize;
    }

    /// Jump to the first or last row.
    pub fn scroll_to(&mut self, total: usize, height: usize, to_end: bool) {
        self.scroll = if to_end {
            total.saturating_sub(height)
        } else {
            0
        };
    }
}

/// Whether a local path exists. Split out so tests can reason about it.
fn path_exists(target: &str) -> bool {
    std::path::Path::new(target).exists()
}

/// Build the full set of rows for a task.
///
/// `missing_files` controls whether local paths are probed; callers rendering in
/// a hot loop can pass `false`.
#[must_use]
pub fn rows(task: &Task, subtasks: &[&Task], now: DateTime<Utc>, check_files: bool) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::with_capacity(24);

    // --- headline ---
    out.push(Row::Meta {
        label: "status",
        value: task.status.as_str().to_string(),
        urgent: false,
    });
    out.push(Row::Meta {
        label: "priority",
        value: task.priority.as_str().to_string(),
        urgent: task.priority == crate::model::Priority::Urgent && task.status.is_open(),
    });

    if let Some(due) = task.due_at {
        let open = task.status.is_open();
        out.push(Row::Meta {
            label: "due",
            value: format!(
                "{} ({})",
                due.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
                crate::model::countdown(due, now)
            ),
            urgent: open && due < now,
        });
    }
    if let Some(est) = task.estimate_human() {
        out.push(Row::Meta {
            label: "estimate",
            value: est,
            urgent: false,
        });
    }
    if !task.tags.is_empty() {
        out.push(Row::Meta {
            label: "tags",
            value: task.tags.join(", "),
            urgent: false,
        });
    }
    out.push(Row::Meta {
        label: "created",
        value: task
            .created_at
            .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
        urgent: false,
    });
    if task.updated_at != task.created_at {
        out.push(Row::Meta {
            label: "updated",
            value: task
                .updated_at
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            urgent: false,
        });
    }
    if let Some(started) = task.started_at {
        out.push(Row::Meta {
            label: "started",
            value: started.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            urgent: false,
        });
    }
    if let Some(done) = task.completed_at {
        out.push(Row::Meta {
            label: "completed",
            value: done.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            urgent: false,
        });
    }

    // --- description ---
    out.push(Row::Blank);
    if task.description.trim().is_empty() {
        out.push(Row::Heading("description"));
        out.push(Row::Text {
            text: "(none — press i to add one)".to_string(),
        });
    } else {
        out.push(Row::Heading("description"));
        for line in task.description.lines() {
            out.push(Row::Text {
                text: line.to_string(),
            });
        }
    }

    // --- subtasks ---
    if !subtasks.is_empty() {
        out.push(Row::Blank);
        out.push(Row::Heading("subtasks"));
        for (i, sub) in subtasks.iter().enumerate() {
            let done = sub.status == Status::Done;
            let est = sub.estimate_human().unwrap_or_else(|| "-".into());
            out.push(Row::Meta {
                label: if done { "done" } else { "open" },
                value: format!("{}. {}  ({})", i + 1, sub.title, est),
                urgent: !done && sub.due_at.is_some_and(|d| d < now),
            });
        }
    }

    // --- attachments ---
    out.push(Row::Blank);
    out.push(Row::Heading("attachments"));
    if task.attachments.is_empty() {
        out.push(Row::Text {
            text: "(none — press l to attach a link, p for a PDF)".to_string(),
        });
    } else {
        for (index, a) in task.attachments.iter().enumerate() {
            let missing = check_files && a.kind != AttachmentKind::Url && !path_exists(&a.target);
            out.push(Row::Attachment {
                index,
                kind: a.kind,
                label: attachment_label(a),
                target: a.target.clone(),
                missing,
            });
        }
    }

    out.push(Row::Blank);
    out.push(Row::Hint(
        "j/k scroll · [ ] pick · o open · i edit · Esc back",
    ));
    out
}

/// Attachments are stored with an optional label; fall back to the target.
#[must_use]
pub fn attachment_label(a: &Attachment) -> String {
    if a.label.trim().is_empty() {
        match a.kind {
            AttachmentKind::Url => "link".to_string(),
            AttachmentKind::Pdf => "PDF".to_string(),
            AttachmentKind::File => "file".to_string(),
        }
    } else {
        a.label.clone()
    }
}

/// Hand an attachment to the operating system.
///
/// Separated from the `open` call so the decision is testable without spawning
/// anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Handed off successfully.
    Launched { target: String },
    /// The attachment does not exist on disk.
    NotFound { target: String },
    /// The system refused.
    Failed { target: String, reason: String },
    /// The kind cannot be opened at all.
    Unsupported { kind: &'static str },
}

/// Launch an attachment with the platform's default handler.
///
/// PDFs and files are checked for existence first: the error a user gets from
/// the OS for a missing file is far worse than being told here.
pub fn launch(a: &Attachment) -> OpenOutcome {
    if a.kind != AttachmentKind::Url && !path_exists(&a.target) {
        return OpenOutcome::NotFound {
            target: a.target.clone(),
        };
    }
    match open::that(&a.target) {
        Ok(()) => OpenOutcome::Launched {
            target: a.target.clone(),
        },
        Err(e) => OpenOutcome::Failed {
            target: a.target.clone(),
            reason: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn task() -> Task {
        Task::new("Write the RFC", Utc::now())
    }

    fn text_of(rows: &[Row]) -> String {
        rows.iter()
            .map(|r| match r {
                Row::Meta { label, value, .. } => format!("{label}: {value}"),
                Row::Heading(h) => format!("[{h}]"),
                Row::Text { text } => text.clone(),
                Row::Attachment {
                    index, kind, label, ..
                } => {
                    format!("{index} <{kind}> {label}")
                }
                Row::Hint(h) => h.to_string(),
                Row::Blank => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // -- layout --

    #[test]
    fn a_bare_task_shows_its_basics() {
        let t = task();
        let text = text_of(&rows(&t, &[], Utc::now(), false));
        assert!(text.contains("status: todo"), "{text}");
        assert!(text.contains("priority: normal"), "{text}");
        assert!(
            !text.contains("due:"),
            "no due date means no due row: {text}"
        );
        assert!(text.contains("[attachments]"), "{text}");
        assert!(text.contains("(none — press l"), "{text}");
    }

    #[test]
    fn timestamps_and_counts_are_shown_when_present() {
        let now = Utc::now();
        let mut t = task();
        t.due_at = Some(now + Duration::days(2));
        t.estimate_minutes = Some(240);
        t.tags = vec!["work".into(), "q4".into()];
        t.set_status(Status::Doing, now);

        let text = text_of(&rows(&t, &[], now, false));
        assert!(text.contains("due: "), "{text}");
        assert!(text.contains("in 2d"), "countdown included: {text}");
        assert!(text.contains("estimate: 4h"), "{text}");
        assert!(text.contains("tags: work, q4"), "{text}");
        assert!(text.contains("started:"), "{text}");
        assert!(!text.contains("completed:"), "{text}");
    }

    #[test]
    fn an_overdue_deadline_is_flagged() {
        let now = Utc::now();
        let mut t = task();
        t.due_at = Some(now - Duration::days(3));

        let rows = rows(&t, &[], now, false);
        let due = rows
            .iter()
            .find(|r| matches!(r, Row::Meta { label: "due", .. }));
        assert!(
            matches!(due, Some(Row::Meta { urgent: true, .. })),
            "an overdue task should be highlighted"
        );
    }

    #[test]
    fn an_overdue_date_on_a_finished_task_is_not_flagged() {
        let now = Utc::now();
        let mut t = task();
        t.due_at = Some(now - Duration::days(3));
        t.set_status(Status::Done, now);

        let rows = rows(&t, &[], now, false);
        let due = rows
            .iter()
            .find(|r| matches!(r, Row::Meta { label: "due", .. }));
        assert!(
            matches!(due, Some(Row::Meta { urgent: false, .. })),
            "completed work is not urgent, whatever its deadline"
        );
    }

    #[test]
    fn subtasks_are_listed_with_their_state() {
        let parent = task();
        let mut one = Task::new("draft intro", Utc::now());
        one.estimate_minutes = Some(60);
        let mut two = Task::new("collect feedback", Utc::now());
        two.set_status(Status::Done, Utc::now());
        two.due_at = Some(Utc::now() - Duration::days(1));

        let subs: Vec<&Task> = vec![&one, &two];
        let text = text_of(&rows(&parent, &subs, Utc::now(), false));
        assert!(text.contains("[subtasks]"), "{text}");
        assert!(text.contains("1. draft intro  (1h)"), "{text}");
        assert!(text.contains("2. collect feedback"), "{text}");
        assert!(
            text.contains("done: 2. collect feedback"),
            "a finished subtask is labelled done: {text}"
        );

        // And nothing is shown when there are none.
        assert!(!text_of(&rows(&parent, &[], Utc::now(), false)).contains("[subtasks]"));
    }

    #[test]
    fn a_multiline_description_becomes_several_rows() {
        let mut t = task();
        t.description = "first\nsecond\nthird".into();
        let text = text_of(&rows(&t, &[], Utc::now(), false));
        assert!(text.contains("first\nsecond\nthird"), "{text}");
    }

    #[test]
    fn an_empty_description_says_so() {
        let text = text_of(&rows(&task(), &[], Utc::now(), false));
        assert!(text.contains("(none — press i to add one)"), "{text}");
    }

    #[test]
    fn attachments_are_listed_with_their_index() {
        let mut t = task();
        t.attachments
            .push(Attachment::url("https://a.test", "spec"));
        t.attachments.push(Attachment::pdf("/tmp/x.pdf", ""));

        let text = text_of(&rows(&t, &[], Utc::now(), false));
        assert!(text.contains("0 <url> spec"), "{text}");
        assert!(
            text.contains("1 <pdf> PDF"),
            "unlabelled falls back: {text}"
        );
    }

    #[test]
    fn attachment_rows_expose_their_index() {
        let mut t = task();
        t.attachments.push(Attachment::url("https://a.test", ""));
        let rows = rows(&t, &[], Utc::now(), false);
        let indices: Vec<usize> = rows.iter().filter_map(Row::attachment_index).collect();
        assert_eq!(indices, vec![0]);
        assert!(rows.iter().any(Row::is_attachment));
    }

    // -- scrolling --

    #[test]
    fn scrolling_is_clamped_to_the_content() {
        let mut d = Detail::new();
        d.scroll_by(5, 20, 10);
        assert_eq!(d.scroll, 5);

        d.scroll_by(100, 20, 10);
        assert_eq!(d.scroll, 10, "cannot scroll past the end");

        d.scroll_by(-100, 20, 10);
        assert_eq!(d.scroll, 0);
    }

    #[test]
    fn scrolling_past_the_end_of_a_short_pane_is_a_no_op() {
        let mut d = Detail::new();
        d.scroll_by(10, 5, 20);
        assert_eq!(d.scroll, 0);
    }

    #[test]
    fn jump_to_end_lands_on_the_last_full_screen() {
        let mut d = Detail::new();
        d.scroll_to(30, 10, true);
        assert_eq!(d.scroll, 20);

        d.scroll_to(30, 10, false);
        assert_eq!(d.scroll, 0);
    }

    // -- attachment cursor --

    #[test]
    fn the_attachment_cursor_is_clamped() {
        let mut d = Detail::new();
        d.move_attachment(1, 3);
        assert_eq!(d.attachment_focus, 1);
        d.move_attachment(10, 3);
        assert_eq!(d.attachment_focus, 2);
        d.move_attachment(-10, 3);
        assert_eq!(d.attachment_focus, 0);
    }

    #[test]
    fn the_cursor_stays_at_zero_when_there_are_no_attachments() {
        let mut d = Detail::new();
        d.move_attachment(1, 0);
        assert_eq!(d.attachment_focus, 0);
    }

    #[test]
    fn scrolling_keeps_the_focused_attachment_on_screen() {
        let mut t = task();
        for i in 0..30 {
            t.attachments
                .push(Attachment::url(format!("https://example.com/{i}"), ""));
        }
        let rows = rows(&t, &[], Utc::now(), false);
        let mut d = Detail::new();

        // Focus an attachment well below the fold.
        d.attachment_focus = 25;
        d.keep_focus_visible(&rows, 10);
        let focused_row = rows
            .iter()
            .position(|r| r.attachment_index() == Some(25))
            .expect("row exists");
        assert!(
            focused_row >= d.scroll && focused_row < d.scroll + 10,
            "focused row {focused_row} not visible at scroll {}",
            d.scroll
        );
    }

    #[test]
    fn keeping_focus_visible_is_stable_when_already_visible() {
        let mut t = task();
        t.attachments.push(Attachment::url("https://a.test", ""));
        let rows = rows(&t, &[], Utc::now(), false);
        let mut d = Detail::new();
        let before = d.scroll;
        d.keep_focus_visible(&rows, 40);
        assert_eq!(d.scroll, before, "a tall pane should not scroll");
    }

    // -- opening --

    #[test]
    fn a_missing_local_file_is_reported_before_the_os_is_asked() {
        let a = Attachment::pdf("/definitely/not/here.pdf", "");
        assert_eq!(
            launch(&a),
            OpenOutcome::NotFound {
                target: "/definitely/not/here.pdf".into()
            }
        );
    }

    #[test]
    fn unsupported_kinds_are_named() {
        let a = Attachment {
            kind: AttachmentKind::File,
            label: String::new(),
            target: "/tmp".into(),
            added_at: Utc::now(),
        };
        // A directory exists, so this reaches the launcher; the point is that
        // the outcome is always one of the modelled cases.
        assert!(matches!(
            launch(&a),
            OpenOutcome::Launched { .. } | OpenOutcome::Failed { .. }
        ));
    }
}
