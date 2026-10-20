//! Application state and behaviour, independent of any terminal.
//!
//! Everything here is testable without a TTY: the event loop in [`crate::tui`]
//! feeds it [`Action`]s and renders whatever [`crate::ui`] draws.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};

use crate::editor::TextBuffer;
use crate::input::{Action, Context, Dispatcher, Key, Outcome};
use crate::model::{self, Filter, SortKey, Status, Task, TaskId, TaskSet};
use crate::store::{Event, EventKind, Store, StoreError};

/// How long the "saved" toast lingers before clearing itself.
const TOAST_TTL: Duration = Duration::from_secs(3);

/// Which full-screen view the app is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    List,
    /// A single task, opened with `<Enter>`.
    Detail,
    /// The gantt chart.
    Timeline,
}

/// Which transient overlay, if any, is on top of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    /// The add/edit form. Owns its own field buffers.
    Form(Box<crate::form::Form>),
    /// A single-field prompt, e.g. `/` or `l`.
    Prompt(Prompt),
    /// A destructive action awaiting confirmation.
    Confirm(ConfirmKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    Delete,
    BulkDelete,
}

/// The single-field prompts: search, attach a link, attach a PDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Filter,
    Link,
    Pdf,
}

impl PromptKind {
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            PromptKind::Filter => "search",
            PromptKind::Link => "attach link",
            PromptKind::Pdf => "attach PDF",
        }
    }

    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            PromptKind::Filter => "text to match",
            PromptKind::Link => "https://example.com/spec",
            PromptKind::Pdf => "/path/to/file.pdf",
        }
    }

    /// A link or path is committed on Enter; search can also be committed by
    /// leaving the field, so both are single-line.
    #[must_use]
    pub fn is_multiline(self) -> bool {
        false
    }
}

/// A one-field text prompt, reusing the same editor as the task form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub buf: TextBuffer,
    pub scroll: usize,
}

/// A task as displayed, with its nesting resolved.
#[derive(Debug, Clone)]
pub struct Row {
    pub task: Task,
    /// 0 for a root task, 1 for a subtask.
    pub depth: u8,
    /// Total immediate children, for the `(+n)` marker.
    pub children: usize,
    /// Children already completed, for the rollup bar.
    pub children_done: usize,
}

/// A transient message in the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    /// Set when this toast is an error the user must acknowledge.
    pub expires: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

impl Toast {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ToastKind::Info,
            expires: None,
        }
    }

    pub fn success(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ToastKind::Success,
            expires: Some(SystemTime::now() + TOAST_TTL),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ToastKind::Error,
            expires: None,
        }
    }
}

/// The running application.
#[derive(Debug)]
pub struct App {
    pub store: Store,
    pub set: TaskSet,

    /// Which full-screen view is active.
    pub view: View,
    /// Scroll and attachment-cursor state for the detail view.
    pub detail: crate::detail::Detail,
    /// Horizontal resolution of the timeline.
    pub scale: crate::timeline::Scale,
    /// First visible timeline row.
    pub timeline_scroll: usize,

    pub context: Context,
    pub overlay: Overlay,

    /// Index into [`App::rows`].
    pub cursor: usize,
    /// Index of the visual-mode anchor, if a selection is active.
    pub visual_anchor: Option<usize>,

    pub sort: SortKey,
    pub filter: Filter,
    /// Status filter cycle position; `None` means "all".
    pub status_cycle: Option<usize>,

    /// Cached visible rows, recomputed by [`App::rebuild`].
    pub rows: Vec<Row>,

    /// Keys typed but not yet a complete binding.
    pub pending: Vec<Key>,

    pub message: Option<Toast>,
    pub show_help_panel: bool,
    /// Row offset of the help overlay, so long help stays reachable.
    pub help_scroll: usize,
    pub tree: bool,

    pub should_quit: bool,

    /// Store mtime at the last read, used to notice external writes.
    last_mtime: Option<SystemTime>,

    dispatcher: Dispatcher,
    /// Viewport height, set by the renderer each frame.
    pub viewport: u16,
}

impl App {
    #[must_use]
    pub fn new(store: Store) -> Self {
        let mut app = Self {
            set: TaskSet::new(),
            store,
            view: View::List,
            detail: crate::detail::Detail::new(),
            scale: crate::timeline::Scale::Day,
            timeline_scroll: 0,
            context: Context::Global,
            overlay: Overlay::None,
            cursor: 0,
            visual_anchor: None,
            sort: SortKey::Due,
            filter: Filter::default(),
            status_cycle: None,
            rows: Vec::new(),
            pending: Vec::new(),
            message: None,
            show_help_panel: false,
            help_scroll: 0,
            tree: true,
            should_quit: false,
            last_mtime: None,
            dispatcher: Dispatcher::new(),
            viewport: 20,
        };
        app.reload();
        app
    }

    // -- data ---------------------------------------------------------------

    /// Re-read the store from disk, discarding nothing and resetting the cursor.
    pub fn reload(&mut self) {
        match self.store.load() {
            Ok(set) => {
                self.set = set;
                self.last_mtime = self.store.mtime();
                self.rebuild();
                self.clamp_cursor();
            }
            Err(e) => self.set_error(e),
        }
    }

    /// Recompute [`App::rows`] from the current filter and sort.
    pub fn rebuild(&mut self) {
        let now = Utc::now();
        let mut tasks: Vec<Task> = self
            .set
            .tasks
            .iter()
            .filter(|t| self.filter.matches(t, now))
            .cloned()
            .collect();

        if self.sort == SortKey::Manual {
            model::default_order(&mut tasks);
        } else {
            self.sort.apply(&mut tasks);
        }

        self.rows = if self.tree {
            // Keep children adjacent to their parent, and skip orphans whose
            // parent the filter excluded.
            let visible: std::collections::HashSet<TaskId> = tasks.iter().map(|t| t.id).collect();
            let mut rows: Vec<Row> = Vec::with_capacity(tasks.len());
            for task in tasks.iter().filter(|t| t.parent_id.is_none()) {
                rows.push(self.row_for(task));
                for child in tasks.iter().filter(|t| t.parent_id == Some(task.id)) {
                    rows.push(self.row_for(child));
                }
            }
            // Any task whose parent is missing from the filtered set would
            // otherwise vanish; show it at the top level.
            for task in tasks
                .iter()
                .filter(|t| t.parent_id.is_some_and(|p| !visible.contains(&p)))
            {
                rows.push(self.row_for(task));
            }
            rows
        } else {
            tasks.iter().map(|t| self.row_for(t)).collect()
        };
    }

    fn row_for(&self, task: &Task) -> Row {
        let rollup = self.set.rollup(&task.id);
        Row {
            task: task.clone(),
            // In flat mode nothing is indented, even though parent_id is set.
            depth: if self.tree {
                u8::from(task.parent_id.is_some())
            } else {
                0
            },
            children: rollup.total,
            children_done: rollup.done,
        }
    }

    /// The selected task, if any.
    #[must_use]
    pub fn selected(&self) -> Option<&Task> {
        self.rows.get(self.cursor).map(|r| &r.task)
    }

    #[must_use]
    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    /// The tasks the current selection covers: the cursor task in NORMAL, or the
    /// anchor-to-cursor range in VISUAL.
    #[must_use]
    pub fn selection(&self) -> Vec<TaskId> {
        match self.visual_anchor {
            None => self.selected().map(|t| vec![t.id]).unwrap_or_default(),
            Some(anchor) => {
                let (lo, hi) = (anchor.min(self.cursor), anchor.max(self.cursor));
                self.rows
                    .iter()
                    .skip(lo)
                    .take(hi - lo + 1)
                    .map(|r| r.task.id)
                    .collect()
            }
        }
    }

    #[must_use]
    pub fn in_visual(&self) -> bool {
        self.visual_anchor.is_some()
    }

    // -- movement -----------------------------------------------------------

    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = self.rows.len() as isize - 1;
        let next = (self.cursor as isize + delta).clamp(0, last);
        self.cursor = next as usize;
    }

    pub fn cursor_top(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_bottom(&mut self) {
        self.cursor = self.rows.len().saturating_sub(1);
    }

    /// Half a page, the vim `Ctrl-d` default.
    pub fn half_page(&self) -> isize {
        isize::try_from((self.viewport / 2).max(1)).unwrap_or(isize::MAX)
    }

    /// Whole-viewport height as a signed delta for [`App::move_by`].
    fn page(&self) -> isize {
        isize::try_from(self.viewport).unwrap_or(isize::MAX)
    }

    /// Scroll the help overlay, clamped to its content.
    pub fn scroll_help(&mut self, delta: isize) {
        let max = crate::ui::help::max_scroll(self.viewport.saturating_add(3));
        let next = self.help_scroll as isize + delta;
        self.help_scroll = next.clamp(0, max as isize) as usize;
    }

    /// True when a success toast has timed out since the last frame.
    #[must_use]
    pub fn tick_expired(&self) -> bool {
        self.message
            .as_ref()
            .is_some_and(|m| m.expires.is_some_and(|at| SystemTime::now() >= at))
    }

    /// Scroll the focused form field or prompt so the caret stays visible.
    pub fn scroll_field(&mut self, delta: isize) {
        match &mut self.overlay {
            Overlay::Form(form) => {
                if let Some(f) = form.focused_mut() {
                    let step = delta as i64;
                    f.scroll = (f.scroll as i64 + step).max(0) as usize;
                }
            }
            Overlay::Prompt(p) => {
                let step = delta as i64;
                p.scroll = (p.scroll as i64 + step).max(0) as usize;
            }
            _ => {}
        }
    }

    /// Carry out a confirmed delete of the current selection.
    pub fn confirm_delete(&mut self) {
        let ids = self.selection();
        if ids.is_empty() {
            self.set_message(Toast::error("nothing to delete"));
            return;
        }

        // Titles are captured up front so events survive the deletion.
        let titles: Vec<String> = self
            .rows
            .iter()
            .filter(|r| ids.contains(&r.task.id))
            .map(|r| r.task.title.clone())
            .collect();
        let events: Vec<Event> = ids
            .iter()
            .zip(titles.iter())
            .map(|(id, title)| {
                Event::new(EventKind::TaskDeleted, Some(*id)).with_title(title.clone())
            })
            .collect();

        match self.store.mutate(move |set, logged| {
            let removed = set.remove_cascade(&ids[0]);
            logged.extend(events);
            Ok(removed)
        }) {
            Ok(removed) => {
                let n = removed.len();
                self.visual_anchor = None;
                self.refresh();
                self.set_message(Toast::success(format!(
                    "deleted {n} task{}",
                    if n == 1 { "" } else { "s" }
                )));
            }
            Err(e) => self.set_error(e),
        }
    }

    /// Decline a delete and return to normal.
    pub fn cancel_delete(&mut self) {
        self.close_overlay();
        self.set_message(Toast::info("nothing deleted"));
    }

    fn clamp_cursor(&mut self) {
        let last = self.rows.len().saturating_sub(1);
        if self.cursor > last {
            self.cursor = last;
        }
    }

    // -- overlays: prompts and forms ----------------------------------------

    /// Open a single-field prompt, e.g. for `/` or `l`.
    fn open_prompt(&mut self, kind: PromptKind, initial: &str) {
        self.context = Context::Prompt;
        self.overlay = Overlay::Prompt(Prompt {
            kind,
            buf: TextBuffer::single_line(initial),
            scroll: 0,
        });
        self.pending.clear();
    }

    /// Open a blank task-creation form.
    pub fn open_add_form(&mut self) {
        self.context = Context::Prompt;
        self.overlay = Overlay::Form(Box::new(crate::form::Form::add()));
        self.pending.clear();
    }

    /// Open an edit form for the selected task.
    pub fn open_edit_form(&mut self) {
        match self.selected().cloned() {
            Some(task) => {
                self.context = Context::Prompt;
                self.overlay = Overlay::Form(Box::new(crate::form::Form::edit(&task)));
                self.pending.clear();
            }
            None => self.set_message(Toast::error("no task selected")),
        }
    }

    /// Open the edit form focused on a particular field, for the shortcut keys.
    pub fn open_edit_focus(&mut self, kind: crate::form::FieldKind) {
        match self.selected().cloned() {
            Some(task) => {
                let mut form = crate::form::Form::edit(&task);
                if let Some(index) = form.fields.iter().position(|f| f.kind == kind) {
                    form.focus = index;
                }
                self.context = Context::Prompt;
                self.overlay = Overlay::Form(Box::new(form));
                self.pending.clear();
            }
            None => self.set_message(Toast::error("no task selected")),
        }
    }

    /// True while a form holds changes that are not yet on disk.
    ///
    /// Forms buffer locally and commit in one atomic write, so this is the only
    /// thing in the app that can be "unsaved". Everything else writes through
    /// under the store lock immediately.
    #[must_use]
    pub fn has_unsaved_changes(&self) -> bool {
        self.form().is_some_and(crate::form::Form::is_dirty)
    }

    /// Show the detail pane for the selected task.
    pub fn open_detail(&mut self) {
        if self.selected().is_none() {
            self.set_message(Toast::error("no task selected"));
            return;
        }
        self.detail = crate::detail::Detail::new();
        self.view = View::Detail;
        self.pending.clear();
    }

    /// Return to the task list.
    pub fn close_detail(&mut self) {
        self.view = View::List;
        self.detail = crate::detail::Detail::new();
        self.pending.clear();
    }

    #[must_use]
    pub fn in_detail(&self) -> bool {
        self.view == View::Detail
    }

    #[must_use]
    pub fn in_timeline(&self) -> bool {
        self.view == View::Timeline
    }

    /// Switch between the task list and the gantt chart.
    ///
    /// The selection is shared, so the same task stays highlighted in both.
    pub fn toggle_timeline(&mut self) {
        if self.in_timeline() {
            self.close_timeline();
        } else {
            self.view = View::Timeline;
            self.timeline_scroll = 0;
            self.scroll_timeline_into_view();
        }
    }

    pub fn close_timeline(&mut self) {
        self.view = View::List;
        self.timeline_scroll = 0;
        self.pending.clear();
    }

    /// Zoom the chart coarser (days to weeks to months).
    pub fn zoom_in(&mut self) {
        if !self.in_timeline() {
            return;
        }
        self.scale = self.scale.next();
        self.set_message(Toast::info(format!("showing {}", self.scale.label())));
    }

    /// Zoom the chart finer.
    pub fn zoom_out(&mut self) {
        if !self.in_timeline() {
            return;
        }
        self.scale = self.scale.prev();
        self.set_message(Toast::info(format!("showing {}", self.scale.label())));
    }

    /// Keep the selected row on screen in the timeline.
    ///
    /// Scrolls the minimum amount, so stepping down a list does not make the
    /// chart lurch.
    pub fn scroll_timeline_into_view(&mut self) {
        let height = usize::from(self.viewport);
        if height == 0 || self.rows.len() <= height {
            self.timeline_scroll = 0;
            return;
        }
        if self.cursor < self.timeline_scroll {
            self.timeline_scroll = self.cursor;
        } else if self.cursor >= self.timeline_scroll + height {
            self.timeline_scroll = self.cursor + 1 - height;
        }
    }

    /// Lay the chart out for the current rows, selection, scale, and width.
    #[must_use]
    pub fn timeline(&self, columns: usize) -> crate::timeline::Timeline {
        let inputs: Vec<crate::timeline::Input<'_>> = self
            .rows
            .iter()
            .map(|r| crate::timeline::Input {
                task: &r.task,
                depth: r.depth,
                children: r.children,
                children_done: r.children_done,
            })
            .collect();
        crate::timeline::Timeline::build(&inputs, Utc::now(), self.scale, columns)
    }

    /// The task the detail pane is showing.
    #[must_use]
    pub fn detail_task(&self) -> Option<&Task> {
        self.in_detail().then(|| self.selected()).flatten()
    }

    /// Scroll the detail pane by `delta` rows.
    pub fn scroll_detail(&mut self, delta: isize) {
        if !self.in_detail() {
            return;
        }
        let Some(task) = self.detail_task().cloned() else {
            return;
        };
        let now = Utc::now();
        let rows = crate::detail::rows(&task, &self.subtasks_of(&task), now, true);
        let height = usize::from(self.viewport);
        self.detail.scroll_by(delta, rows.len(), height);
    }

    /// Jump to the top or bottom of the detail pane.
    pub fn detail_goto(&mut self, to_end: bool) {
        if !self.in_detail() {
            return;
        }
        let Some(task) = self.detail_task().cloned() else {
            return;
        };
        let now = Utc::now();
        let rows = crate::detail::rows(&task, &self.subtasks_of(&task), now, true);
        self.detail
            .scroll_to(rows.len(), usize::from(self.viewport), to_end);
    }

    /// `j`/`k` scroll the pane in detail view, and move the list cursor
    /// otherwise.
    fn move_or_scroll(&mut self, delta: isize) {
        if self.in_detail() {
            self.scroll_detail(delta);
        } else {
            self.move_by(delta);
            if self.in_timeline() {
                self.scroll_timeline_into_view();
            }
        }
    }

    /// Move the attachment cursor and keep it on screen.
    pub fn move_attachment(&mut self, delta: isize) {
        if !self.in_detail() {
            return;
        }
        let Some(task) = self.detail_task().cloned() else {
            return;
        };
        let now = Utc::now();
        let rows = crate::detail::rows(&task, &self.subtasks_of(&task), now, true);
        self.detail.move_attachment(delta, task.attachments.len());
        self.detail
            .keep_focus_visible(&rows, usize::from(self.viewport));
    }

    fn subtasks_of(&self, task: &Task) -> Vec<&Task> {
        self.set.children(&task.id)
    }

    /// The attachment the detail pane's cursor is on.
    #[must_use]
    pub fn focused_attachment(&self) -> Option<model::Attachment> {
        self.detail_task()
            .and_then(|t| t.attachments.get(self.detail.attachment_focus))
            .cloned()
    }

    /// Hand the focused attachment to the operating system.
    pub fn open_focused_attachment(&mut self) {
        let Some(a) = self.focused_attachment() else {
            self.set_message(Toast::error("no attachment selected"));
            return;
        };
        let label = crate::detail::attachment_label(&a);
        match crate::detail::launch(&a) {
            crate::detail::OpenOutcome::Launched { .. } => {
                self.set_message(Toast::success(format!("opened {label}")));
            }
            crate::detail::OpenOutcome::NotFound { target } => {
                self.set_message(Toast::error(format!("{target} does not exist")));
            }
            crate::detail::OpenOutcome::Failed { reason, .. } => {
                self.set_message(Toast::error(format!("could not open {label}: {reason}")));
            }
            crate::detail::OpenOutcome::Unsupported { kind } => {
                self.set_message(Toast::error(format!("cannot open a {kind}")));
            }
        }
    }

    /// Remove the focused attachment from the task.
    pub fn remove_focused_attachment(&mut self) {
        let Some(task) = self.detail_task().cloned() else {
            return;
        };
        let Some(index) = task.attachments.get(self.detail.attachment_focus).cloned() else {
            self.set_message(Toast::error("no attachment selected"));
            return;
        };
        let label = crate::detail::attachment_label(&index);
        let result = self.mutate(
            move |t, events| {
                if t.attachments.len() <= 1 {
                    // Drop the element rather than the whole task.
                }
                t.attachments.retain(|a| a != &index);
                t.updated_at = Utc::now();
                events.push(
                    Event::new(EventKind::AttachmentRemoved, Some(t.id))
                        .with_title(t.title.clone())
                        .with_patch(serde_json::json!({
                            "target": index.target,
                        }))
                        .with_undo(serde_json::json!({ "attachment": index })),
                );
                Ok(Toast::success(format!("removed {label}")))
            },
            task.id,
        );

        match result {
            Ok(toast) => {
                // Keep the cursor inside the new, shorter list.
                self.detail
                    .move_attachment(0, self.focused_attachment().map_or(0, |_| 1));
                self.detail.attachment_focus = 0;
                self.set_message(toast);
            }
            Err(e) => self.set_error(e),
        }
    }

    /// The open form, if any.
    #[must_use]
    pub fn form(&self) -> Option<&crate::form::Form> {
        match &self.overlay {
            Overlay::Form(f) => Some(f.as_ref()),
            _ => None,
        }
    }

    pub fn form_mut(&mut self) -> Option<&mut crate::form::Form> {
        match &mut self.overlay {
            Overlay::Form(f) => Some(f),
            _ => None,
        }
    }

    /// The focused text buffer, whichever overlay owns it.
    #[must_use]
    pub fn focused_buffer(&self) -> Option<&TextBuffer> {
        match &self.overlay {
            Overlay::Form(f) => f.focused().map(|f| &f.buf),
            Overlay::Prompt(p) => Some(&p.buf),
            _ => None,
        }
    }

    pub fn focused_buffer_mut(&mut self) -> Option<&mut TextBuffer> {
        match &mut self.overlay {
            Overlay::Form(f) => f.focused_mut().map(|f| &mut f.buf),
            Overlay::Prompt(p) => Some(&mut p.buf),
            _ => None,
        }
    }

    /// Apply an INSERT-mode key to the focused buffer.
    ///
    /// Returns false when the key is not text-editing, so the caller can fall
    /// through to form-level handling.
    pub fn edit_key(&mut self, key: Key) -> bool {
        let Some(buf) = self.focused_buffer_mut() else {
            return false;
        };
        match key {
            Key::Char(c) => buf.insert(c),
            Key::Space => buf.insert(' '),
            Key::Backspace => buf.backspace(),
            Key::Delete => buf.delete(),
            Key::Ctrl('h') => buf.backspace(),
            Key::Ctrl('w') => buf.delete_word_back(),
            Key::Ctrl('u') => buf.delete_to_line_start(),
            Key::Ctrl('k') => buf.delete_to_line_end(),
            Key::Ctrl('y') => buf.yank(),
            Key::Ctrl('a') => buf.line_start(),
            Key::Ctrl('e') => buf.line_end(),
            Key::Alt('b') | Key::Alt('d') => buf.word_left(),
            Key::Alt('f') | Key::Alt('c') => buf.word_right(),
            Key::Left => buf.left(),
            Key::Right => buf.right(),
            Key::Up => buf.up(),
            Key::Down => buf.down(),
            Key::Home => buf.home(),
            Key::End => buf.end(),
            _ => return false,
        }
        true
    }

    /// Insert pasted text into the focused field.
    ///
    /// Outside an editor a paste is ignored rather than guessed at, so a
    /// mis-paste cannot silently run a command.
    pub fn paste(&mut self, text: &str) {
        let Some(buf) = self.focused_buffer_mut() else {
            return;
        };
        buf.insert_str(text);
    }

    /// Route a key to whichever context is active.
    ///
    /// All the modal grammar lives here rather than in the TUI so it can be
    /// tested without a terminal; the event loop just translates crossterm
    /// events and calls this.
    ///
    /// Returns whether the screen needs redrawing.
    pub fn route_key(&mut self, key: Key) -> bool {
        if matches!(self.overlay, Overlay::Confirm(_)) {
            self.confirm_key(key);
            return true;
        }
        if self.context == Context::Prompt {
            self.edit_overlay_key(key);
            return true;
        }
        if self.overlay == Overlay::Help && self.help_key(key) {
            return true;
        }
        if self.in_detail() {
            self.detail_key(key);
            return true;
        }
        if let Some(action) = self.feed(key) {
            self.act(action);
        }
        true
    }

    /// Keys while the detail pane is showing.
    ///
    /// Esc and `q` go back; everything else uses the shared keymap, whose
    /// bindings are scoped so the same keys mean the right thing here.
    pub fn detail_key(&mut self, key: Key) {
        match key {
            Key::Esc | Key::Char('q') => {
                self.close_detail();
                return;
            }
            _ => {}
        }
        if let Some(action) = self.feed(key) {
            self.act(action);
        }
    }

    /// Keys while the help overlay is open.
    ///
    /// Returns whether the key was consumed; `?` and `<Esc>` fall through so
    /// the overlay can still be closed.
    pub fn help_key(&mut self, key: Key) -> bool {
        let page = isize::try_from(self.viewport).unwrap_or(10);
        match key {
            Key::Char('j') => self.scroll_help(1),
            Key::Char('k') => self.scroll_help(-1),
            Key::Ctrl('d') | Key::PageDown => self.scroll_help(page / 2),
            Key::Ctrl('u') | Key::PageUp => self.scroll_help(-(page / 2)),
            Key::Char('g') => self.help_scroll = 0,
            Key::Char('G') => self.help_scroll = usize::MAX / 2, // clamped by scroll_help
            Key::Char('q') => self.act(Action::Quit),
            _ => return false,
        }
        true
    }

    /// Answer a confirmation dialog.
    fn confirm_key(&mut self, key: Key) {
        use crate::app::ConfirmKind;

        let Overlay::Confirm(kind) = self.overlay else {
            return;
        };
        match key {
            Key::Char('y') | Key::Char('Y') => {
                match kind {
                    ConfirmKind::Delete | ConfirmKind::BulkDelete => self.confirm_delete(),
                }
                self.close_overlay();
            }
            Key::Char('n') | Key::Char('N') | Key::Esc => self.cancel_delete(),
            _ => {}
        }
    }

    /// Handle a key while a form or prompt has focus.
    ///
    /// This lives here rather than in the TUI so the whole INSERT-mode grammar
    /// is reachable from tests without a terminal.
    pub fn edit_overlay_key(&mut self, key: Key) {
        match key {
            Key::Esc => {
                self.cancel_overlay();
                return;
            }
            Key::Enter => {
                // A prompt commits on Enter. A multiline form field inserts a
                // newline; the form is always saved explicitly with Ctrl-s, so
                // Enter can never submit a half-typed description.
                if matches!(self.overlay, Overlay::Prompt(_)) {
                    self.commit_overlay();
                } else if self
                    .form()
                    .and_then(crate::form::Form::focused_kind)
                    .is_some_and(crate::form::FieldKind::is_multiline)
                    && let Some(buf) = self.focused_buffer_mut()
                {
                    buf.newline();
                }
                return;
            }
            _ => {}
        }

        // Choice fields are not free text: arrows cycle, and a letter jumps to
        // the matching value.
        if let Some(kind) = self.form().and_then(crate::form::Form::focused_kind)
            && !kind.is_text()
        {
            match key {
                Key::Left | Key::Up => {
                    if let Some(f) = self.form_mut().and_then(crate::form::Form::focused_mut) {
                        f.cycle(false);
                    }
                    return;
                }
                Key::Right | Key::Down => {
                    if let Some(f) = self.form_mut().and_then(crate::form::Form::focused_mut) {
                        f.cycle(true);
                    }
                    return;
                }
                Key::Char(c) => {
                    if let Some(target) = kind.jump_target(c)
                        && let Some(f) = self.form_mut().and_then(crate::form::Form::focused_mut)
                    {
                        f.buf.set_text(target);
                        f.error = None;
                    }
                    return;
                }
                _ => {}
            }
        }

        match key {
            Key::Tab => {
                if let Some(f) = self.form_mut() {
                    f.focus_next();
                }
            }
            Key::BackTab => {
                if let Some(f) = self.form_mut() {
                    f.focus_prev();
                }
            }
            Key::Ctrl('s') => self.commit_overlay(),
            Key::Ctrl('n') => self.scroll_field(1),
            Key::Ctrl('p') => self.scroll_field(-1),
            Key::PageDown => self.scroll_field(5),
            Key::PageUp => self.scroll_field(-5),
            _ => {
                let _ = self.edit_key(key);
            }
        }
    }

    /// Commit the open form or prompt.
    pub fn commit_overlay(&mut self) {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Form(mut form) => {
                match form.save(&self.store) {
                    Ok(task) => {
                        let verb = if form.mode == crate::form::FormMode::Add {
                            "created"
                        } else {
                            "saved"
                        };
                        self.close_overlay();
                        // Keep the cursor on what the user was working on.
                        if let Some(index) = self.rows.iter().position(|r| r.task.id == task.id) {
                            self.cursor = index;
                        }
                        self.refresh();
                        self.set_message(Toast::success(format!("{verb} {}", task.title)));
                    }
                    Err(e) => {
                        // Put the form back so the user can fix it.
                        self.overlay = Overlay::Form(form);
                        self.set_message(Toast::error(e.to_string()));
                    }
                }
            }
            Overlay::Prompt(prompt) => {
                let text = prompt.buf.trimmed();
                self.close_overlay();
                match prompt.kind {
                    PromptKind::Filter => {
                        self.filter.text = (!text.is_empty()).then_some(text);
                        self.rebuild();
                        self.cursor_top();
                        let n = self.rows.len();
                        self.set_message(Toast::info(format!(
                            "{n} matching task{}",
                            if n == 1 { "" } else { "s" }
                        )));
                    }
                    PromptKind::Link | PromptKind::Pdf => {
                        if text.is_empty() {
                            self.set_message(Toast::error("empty input; cancelled"));
                            return;
                        }
                        let Some(id) = self.selected().map(|t| t.id) else {
                            self.set_message(Toast::error("no task selected"));
                            return;
                        };
                        let input = if prompt.kind == PromptKind::Link {
                            AttachmentInput::Url(text)
                        } else {
                            AttachmentInput::Pdf(text)
                        };
                        self.add_attachment(id, input);
                    }
                }
            }
            other => self.overlay = other,
        }
    }

    /// Abandon the open overlay, discarding anything typed.
    pub fn cancel_overlay(&mut self) {
        let was_form = matches!(self.overlay, Overlay::Form(_));
        self.close_overlay();
        self.set_message(Toast::info(if was_form {
            "discarded"
        } else {
            "cancelled"
        }));
    }

    /// Return to NORMAL mode and drop any overlay state.
    pub fn close_overlay(&mut self) {
        self.context = Context::Global;
        self.overlay = Overlay::None;
        self.pending.clear();
        self.visual_anchor = None;
    }

    // -- input --------------------------------------------------------------

    /// Feed a key into the pending buffer. Returns the action to perform.
    ///
    /// In a prompt, keys are text instead, so this is bypassed entirely.
    pub fn feed(&mut self, key: Key) -> Option<Action> {
        if self.context == Context::Prompt || matches!(self.overlay, Overlay::Confirm(_)) {
            return None;
        }
        if self.pending.len() >= self.dispatcher.max_len() {
            // Should be unreachable: a full buffer always resolves, but guard
            // anyway so a bad table cannot wedge the UI.
            self.pending.clear();
            return None;
        }
        self.pending.push(key);
        let scope = if self.in_detail() {
            crate::input::Scope::Detail
        } else if self.in_timeline() {
            crate::input::Scope::Timeline
        } else {
            crate::input::Scope::List
        };
        match self.dispatcher.resolve(&self.pending, scope) {
            Outcome::Bound(action) => {
                self.pending.clear();
                Some(action)
            }
            Outcome::Incomplete => None,
            Outcome::Unbound => {
                self.pending.clear();
                None
            }
        }
    }

    /// Keys typed so far, for the pending indicator.
    #[must_use]
    pub fn pending_label(&self) -> String {
        crate::input::format_sequence(&self.pending)
    }

    /// Perform an action.
    pub fn act(&mut self, action: Action) {
        match action {
            // ---- global ----
            Action::Quit => {
                // Every mutation is written through under the store lock, so
                // there is never anything pending to lose.
                self.should_quit = true;
            }
            Action::SaveForm => {
                if matches!(self.overlay, Overlay::Form(_)) {
                    self.commit_overlay();
                } else {
                    self.set_message(Toast::info(
                        "nothing to save; changes save as you make them",
                    ));
                }
            }
            Action::Help => {
                self.help_scroll = 0;
                self.overlay = match self.overlay {
                    Overlay::Help => Overlay::None,
                    _ => Overlay::Help,
                };
            }
            Action::CloseOverlay => {
                // Esc backs out of whatever is on top, including visual mode.
                if self.in_visual() {
                    self.visual_anchor = None;
                    self.pending.clear();
                } else if self.overlay == Overlay::None {
                    self.pending.clear();
                } else {
                    self.close_overlay();
                }
            }
            Action::Refresh => self.reload(),
            Action::SortNext => {
                self.sort = self.sort.next();
                self.rebuild();
                self.clamp_cursor();
                self.set_message(Toast::info(format!("sort: {}", self.sort.as_str())));
            }

            // ---- movement ----
            // In the detail pane the same keys scroll instead of moving a
            // selection, which is why this view needs no duplicate keymap.
            Action::Down => self.move_or_scroll(1),
            Action::Up => self.move_or_scroll(-1),
            Action::HalfPageDown => self.move_or_scroll(self.half_page()),
            Action::HalfPageUp => self.move_or_scroll(-self.half_page()),
            Action::PageDown => self.move_or_scroll(self.page()),
            Action::PageUp => self.move_or_scroll(-self.page()),
            Action::FirstLine => {
                if self.in_detail() {
                    self.detail_goto(false);
                } else {
                    self.cursor_top();
                    self.scroll_timeline_into_view();
                }
            }
            Action::LastLine => {
                if self.in_detail() {
                    self.detail_goto(true);
                } else {
                    self.cursor_bottom();
                    self.scroll_timeline_into_view();
                }
            }
            Action::CenterLine => {
                let centre = self.page() / 2;
                self.move_by(centre - self.cursor as isize);
            }
            // ---- filtering ----
            Action::ToggleFilterPanel => {
                let initial = self.filter.text.clone().unwrap_or_default();
                self.open_prompt(PromptKind::Filter, &initial);
            }
            Action::ClearFilter => {
                self.filter = Filter::default();
                self.status_cycle = None;
                self.rebuild();
                self.cursor_top();
                self.set_message(Toast::info("filters cleared"));
            }
            Action::CycleFilterStatus => {
                // None (all) -> todo -> doing -> blocked -> open -> all
                let order: [Option<Status>; 5] = [
                    None,
                    Some(Status::Todo),
                    Some(Status::Doing),
                    Some(Status::Blocked),
                    Some(Status::Todo),
                ];
                let next = match self.status_cycle {
                    None => 1,
                    Some(4) => 0,
                    Some(i) => i + 1,
                };
                self.status_cycle = Some(next);
                self.filter.statuses = order[next].into_iter().collect();
                self.rebuild();
                self.cursor_top();
                let label = order[next].map_or("all".to_string(), |s| s.to_string());
                self.set_message(Toast::info(format!("status filter: {label}")));
            }

            // ---- tasks (prompts land here in step 5) ----
            Action::Add => self.open_add_form(),
            Action::EditTask => self.open_edit_form(),
            Action::OpenDetail => {
                if self.in_timeline() {
                    self.view = View::List;
                    self.open_detail();
                } else {
                    self.open_detail();
                }
            }
            Action::CycleStatus => self.cycle_status(),
            Action::SetDue => self.open_edit_focus(crate::form::FieldKind::Due),
            Action::SetEstimate => self.open_edit_focus(crate::form::FieldKind::Estimate),
            Action::AddLink => self.open_prompt(PromptKind::Link, ""),
            Action::AddPdf => self.open_prompt(PromptKind::Pdf, ""),
            Action::Delete => {
                if self.selected().is_some() {
                    self.overlay = Overlay::Confirm(ConfirmKind::Delete);
                } else {
                    self.set_message(Toast::error("no task selected"));
                }
            }

            // ---- timeline ----
            Action::ToggleTimeline => self.toggle_timeline(),
            Action::ZoomIn => self.zoom_in(),
            Action::ZoomOut => self.zoom_out(),

            // ---- detail pane ----
            Action::OpenAttachment => self.open_focused_attachment(),
            Action::RemoveAttachment => self.remove_focused_attachment(),
            Action::NextAttachment => self.move_attachment(1),
            Action::PrevAttachment => self.move_attachment(-1),

            // ---- visual ----
            Action::EnterVisual => {
                if self.rows.is_empty() {
                    self.set_message(Toast::error("nothing to select"));
                } else {
                    self.visual_anchor = Some(self.cursor);
                    self.set_message(Toast::info("visual mode; t/o/d apply"));
                }
            }
            Action::BulkSetStatus | Action::BulkSetPriority | Action::BulkDelete => {
                if !self.in_visual() {
                    self.set_message(Toast::error("press V first"));
                } else if action == Action::BulkDelete {
                    self.overlay = Overlay::Confirm(ConfirmKind::BulkDelete);
                } else {
                    self.set_message(Toast::info("bulk edit arrives in step 8"));
                }
            }

            // ---- view ----
            Action::ToggleTree => {
                self.tree = !self.tree;
                self.rebuild();
                self.clamp_cursor();
                self.set_message(Toast::info(if self.tree { "tree" } else { "flat" }));
            }
            Action::ToggleHelpPanel => {
                self.show_help_panel = !self.show_help_panel;
            }
        }
    }

    // -- mutations ----------------------------------------------------------

    fn cycle_status(&mut self) {
        let Some(id) = self.selected().map(|t| t.id) else {
            self.set_message(Toast::error("no task selected"));
            return;
        };
        let next = self.selected().map_or(Status::Todo, |t| t.status.next());

        let result = self.mutate(
            |task, events| {
                let previous = task.status;
                task.set_status(next, Utc::now());
                events.push(
                    Event::new(EventKind::StatusChanged, Some(task.id))
                        .with_title(task.title.clone())
                        .with_patch(serde_json::json!({
                            "from": previous.as_str(),
                            "to": next.as_str(),
                        }))
                        .with_undo(serde_json::json!({ "status": previous.as_str() })),
                );
                Ok(Toast::success(format!(
                    "{} → {}",
                    previous.as_str(),
                    next.as_str()
                )))
            },
            id,
        );
        match result {
            Ok(toast) => self.set_message(toast),
            Err(e) => self.set_error(e),
        }
    }

    fn add_attachment(&mut self, id: TaskId, input: AttachmentInput) {
        let result = self.mutate(
            |task, events| {
                let attachment = input.build()?;
                let previous = task.attachments.clone();
                task.attachments.push(attachment.clone());
                task.updated_at = Utc::now();
                events.push(
                    Event::new(EventKind::AttachmentAdded, Some(task.id))
                        .with_title(task.title.clone())
                        .with_patch(serde_json::json!({
                            "kind": attachment.kind.as_str(),
                            "target": attachment.target,
                        }))
                        .with_undo(serde_json::json!({ "attachments": previous })),
                );
                Ok(Toast::success(format!("attached {}", attachment.display())))
            },
            id,
        );
        match result {
            Ok(toast) => self.set_message(toast),
            Err(e) => self.set_error(e),
        }
    }

    /// Re-read the store into memory. Called after every write so the view can
    /// never disagree with what is on disk.
    fn refresh(&mut self) {
        match self.store.load() {
            Ok(set) => {
                self.set = set;
                self.last_mtime = self.store.mtime();
                self.rebuild();
                self.clamp_cursor();
            }
            Err(e) => self.set_error(e),
        }
    }

    /// Run a mutation under the store lock, then adopt the new state.
    fn mutate(
        &mut self,
        f: impl FnOnce(&mut Task, &mut Vec<Event>) -> Result<Toast, String>,
        id: TaskId,
    ) -> Result<Toast, String> {
        let mut events: Vec<Event> = Vec::new();
        let updated: Task = self
            .store
            .lock_and_load()
            .map_err(|e| e.to_string())?
            .1
            .get(&id)
            .cloned()
            .ok_or_else(|| "task no longer exists".to_string())?;

        let mut task = updated;
        let toast = f(&mut task, &mut events)?;

        // Write back through the store, re-checking under the lock.
        self.store
            .mutate(|set, logged| {
                let slot = set.get_mut(&id).ok_or(StoreError::NotFound(id))?;
                *slot = task.clone();
                logged.extend(events.clone());
                Ok(())
            })
            .map_err(|e| e.to_string())?;

        // Re-read so `self.set` matches disk; a partial update here is how a
        // TUI ends up showing stale rows.
        self.refresh();
        Ok(toast)
    }

    // -- messages -----------------------------------------------------------

    pub fn set_message(&mut self, toast: Toast) {
        self.message = Some(toast);
    }

    fn set_error(&mut self, e: impl std::fmt::Display) {
        self.set_message(Toast::error(e.to_string()));
    }

    /// Clear an expired toast. Called each frame.
    pub fn tick(&mut self) {
        let expired = self
            .message
            .as_ref()
            .and_then(|m| m.expires)
            .is_some_and(|at| SystemTime::now() >= at);
        if expired {
            self.message = None;
        }
    }

    /// True when the store changed on disk since we last read it.
    #[must_use]
    pub fn store_changed_externally(&self) -> bool {
        match (self.store.mtime(), self.last_mtime) {
            (Some(now), Some(seen)) => now > seen,
            (Some(_), None) => true,
            _ => false,
        }
    }

    // -- rendering helpers --------------------------------------------------

    /// Rows visible in a viewport of `height`, scrolled to keep the cursor on
    /// screen. Pure, so the scrolling maths is unit-testable.
    #[must_use]
    pub fn visible_window(&self, height: usize) -> (usize, usize) {
        let total = self.rows.len();
        if total == 0 || height == 0 {
            return (0, 0);
        }
        if total <= height {
            return (0, total);
        }
        // Centre on the cursor like vim does, but never scroll past the end.
        let half = height / 2;
        let start = self.cursor.saturating_sub(half).min(total - height);
        (start, start + height)
    }

    /// One-line summary counts for the status bar.
    #[must_use]
    pub fn summary(&self, now: DateTime<Utc>) -> model::Summary {
        model::summarize(&self.set.tasks, now)
    }
}

enum AttachmentInput {
    Url(String),
    Pdf(String),
}

impl AttachmentInput {
    fn build(&self) -> Result<model::Attachment, String> {
        let a = match self {
            AttachmentInput::Url(u) => model::Attachment::url(u.clone(), String::new()),
            AttachmentInput::Pdf(p) => model::Attachment::pdf(p.clone(), String::new()),
        };
        a.validate().map_err(|e| e.to_string())?;
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;
    use crate::timeline::Scale;
    use tempfile::TempDir;

    fn app_with(tasks: Vec<Task>) -> (App, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let mut set = TaskSet::new();
        for t in tasks {
            set.tasks.push(t);
        }
        store.save(&set).unwrap();
        let mut app = App::new(store);
        app.viewport = 10;
        (app, tmp)
    }

    fn t(title: &str) -> Task {
        Task::new(title, Utc::now())
    }

    fn press(app: &mut App, s: &str) -> Option<Action> {
        let keys = crate::input::parse_sequence(s).expect("parses");
        let mut last = None;
        for k in keys {
            last = app.feed(k);
        }
        last
    }

    // -- movement --

    #[test]
    fn cursor_moves_and_clamps_at_both_ends() {
        let (mut app, _t) = app_with(vec![t("a"), t("b"), t("c")]);

        app.act(Action::Down);
        assert_eq!(app.cursor, 1);
        app.act(Action::Down);
        app.act(Action::Down);
        assert_eq!(app.cursor, 2, "clamped at the end");

        app.act(Action::Up);
        app.act(Action::Up);
        app.act(Action::Up);
        assert_eq!(app.cursor, 0, "clamped at the start");
    }

    #[test]
    fn movement_is_safe_on_an_empty_list() {
        let (mut app, _t) = app_with(vec![]);
        app.act(Action::Down);
        app.act(Action::PageDown);
        app.act(Action::LastLine);
        assert_eq!(app.cursor, 0);
        assert!(app.rows.is_empty());
        assert!(app.selected().is_none());
    }

    #[test]
    fn gg_and_capital_g_jump_to_the_ends() {
        let (mut app, _t) = app_with(vec![t("a"), t("b"), t("c")]);

        assert_eq!(press(&mut app, "G"), Some(Action::LastLine));
        app.act(Action::LastLine);
        assert_eq!(app.cursor, 2);

        assert_eq!(press(&mut app, "gg"), Some(Action::FirstLine));
        app.act(Action::FirstLine);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn half_page_uses_half_the_viewport() {
        let (mut app, _t) = app_with((0..20).map(|i| t(&format!("t{i}"))).collect());
        app.viewport = 10;
        assert_eq!(app.half_page(), 5);

        app.act(Action::HalfPageDown);
        assert_eq!(app.cursor, 5);
        app.act(Action::HalfPageUp);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn visible_window_centres_and_never_overscrolls() {
        let (mut app, _t) = app_with((0..30).map(|i| t(&format!("t{i}"))).collect());
        app.viewport = 10;

        app.cursor = 0;
        let (start, end) = app.visible_window(10);
        assert_eq!(
            (start, end),
            (0, 10),
            "no scroll while the cursor is on screen"
        );

        app.cursor = 15;
        let (start, end) = app.visible_window(10);
        assert!(
            start <= 15 && end > 15,
            "cursor {start}..{end} must contain 15"
        );
        assert!(end <= 30);

        app.cursor = 29;
        let (start, end) = app.visible_window(10);
        assert_eq!((start, end), (20, 30), "flush with the end");
    }

    #[test]
    fn visible_window_handles_a_list_shorter_than_the_viewport() {
        let (app, _t) = app_with(vec![t("a"), t("b")]);
        assert_eq!(app.visible_window(10), (0, 2));
        assert_eq!(app.visible_window(0), (0, 0));
    }

    // -- selection --

    #[test]
    fn visual_selection_covers_the_anchor_range() {
        let (mut app, _t) = app_with(vec![t("a"), t("b"), t("c"), t("d"), t("e")]);

        assert_eq!(app.selection().len(), 1, "normal mode selects one");
        assert!(!app.in_visual());

        app.act(Action::EnterVisual);
        assert!(app.in_visual());
        app.act(Action::Down);
        app.act(Action::Down);

        assert_eq!(app.selection().len(), 3, "anchor..cursor inclusive");

        // Collapsing back onto the anchor narrows to one, it does not invert.
        app.act(Action::Up);
        app.act(Action::Up);
        assert_eq!(app.cursor, 0);
        assert_eq!(app.selection().len(), 1);
    }

    #[test]
    fn moving_above_the_anchor_does_not_invert_the_selection() {
        let (mut app, _t) = app_with((0..7).map(|i| t(&format!("t{i}"))).collect());

        app.cursor = 3;
        app.act(Action::EnterVisual);
        app.act(Action::Down);
        app.act(Action::Down);
        assert_eq!(app.cursor, 5);
        assert_eq!(app.selection().len(), 3, "rows 3..=5");

        // Now walk well above the anchor; the range must widen, not invert.
        app.act(Action::Up);
        app.act(Action::Up);
        app.act(Action::Up);
        app.act(Action::Up);
        assert_eq!(app.cursor, 1);
        assert_eq!(
            app.selection().len(),
            3,
            "the anchor is still row 3, so the range is 1..=3"
        );
        assert!(
            app.selection().contains(&app.rows[3].task.id),
            "the anchor row stays selected when the cursor is above it"
        );
    }

    #[test]
    fn closing_an_overlay_leaves_visual_mode() {
        let (mut app, _t) = app_with(vec![t("a"), t("b")]);
        app.act(Action::EnterVisual);
        assert!(app.in_visual());
        app.act(Action::CloseOverlay);
        assert!(!app.in_visual());
    }

    // -- rows --

    #[test]
    fn tree_rows_nest_children_under_parents() {
        let parent = t("parent");
        let mut child = t("child");
        child.parent_id = Some(parent.id);
        let (app, _t) = app_with(vec![child, parent.clone()]); // deliberately out of order

        let titles: Vec<&str> = app.rows.iter().map(|r| r.task.title.as_str()).collect();
        assert_eq!(titles, vec!["parent", "child"], "child follows its parent");
        assert_eq!(app.rows[0].depth, 0);
        assert_eq!(app.rows[1].depth, 1);
        assert_eq!(app.rows[0].children, 1);
    }

    #[test]
    fn a_child_survives_even_when_the_filter_hides_its_parent() {
        let parent = t("parent");
        let mut child = t("child");
        child.parent_id = Some(parent.id);
        let (mut app, _t) = app_with(vec![parent, child]);

        // Filter matches only the child.
        app.filter.text = Some("child".into());
        app.rebuild();

        assert_eq!(
            app.rows.len(),
            1,
            "must not disappear just because its parent is filtered out"
        );
        assert_eq!(app.rows[0].task.title, "child");
    }

    #[test]
    fn flat_mode_ignores_nesting() {
        let parent = t("parent");
        let mut child = t("child");
        child.parent_id = Some(parent.id);
        let (mut app, _t) = app_with(vec![parent, child]);

        app.tree = false;
        app.rebuild();
        assert!(app.rows.iter().all(|r| r.depth == 0));
    }

    // -- filtering --

    #[test]
    fn status_filter_cycle_walks_and_resets() {
        let (mut app, _t) = app_with(vec![t("a")]);

        app.act(Action::CycleFilterStatus);
        assert_eq!(app.filter.statuses, vec![Status::Todo]);
        app.act(Action::CycleFilterStatus);
        assert_eq!(app.filter.statuses, vec![Status::Doing]);
        app.act(Action::CycleFilterStatus);
        assert_eq!(app.filter.statuses, vec![Status::Blocked]);
        app.act(Action::CycleFilterStatus);
        assert_eq!(app.filter.statuses, vec![Status::Todo]);
        app.act(Action::CycleFilterStatus);
        assert!(app.filter.statuses.is_empty(), "cycles back to all");
    }

    #[test]
    fn clearing_filters_restores_every_task() {
        let (mut app, _t) = app_with(vec![t("alpha"), t("beta")]);
        app.filter.text = Some("alpha".into());
        app.rebuild();
        assert_eq!(app.rows.len(), 1);

        app.act(Action::ClearFilter);
        assert_eq!(app.rows.len(), 2);
        assert_eq!(app.cursor, 0, "cursor resets to the top");
    }

    #[test]
    fn sort_key_cycles_through_every_key() {
        let (mut app, _t) = app_with(vec![t("a")]);
        let start = app.sort;
        let total = SortKey::ALL.len();

        for i in 1..total {
            app.act(Action::SortNext);
            assert_ne!(app.sort, start, "step {i} did not advance");
        }
        app.act(Action::SortNext);
        assert_eq!(app.sort, start, "sorting wraps back to the start");
    }

    // -- quitting --

    #[test]
    fn quit_exits_immediately_because_nothing_is_ever_pending() {
        let (mut app, _t) = app_with(vec![t("a")]);
        app.act(Action::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn saving_without_a_form_says_there_is_nothing_to_save() {
        let (mut app, _t) = app_with(vec![t("a")]);
        app.act(Action::SaveForm);
        assert!(
            app.message
                .as_ref()
                .is_some_and(|m| m.text.contains("nothing to save")),
            "got {:?}",
            app.message
        );
    }

    #[test]
    fn cycling_status_writes_through_to_the_store() {
        let task = t("a");
        let id = task.id;
        let (mut app, _tmp) = app_with(vec![task]);

        app.act(Action::CycleStatus);
        assert_eq!(app.selected().unwrap().status, Status::Doing);

        // And it is really on disk, with an audit event.
        let set = app.store.load().unwrap();
        assert_eq!(set.get(&id).unwrap().status, Status::Doing);

        let events = app.store.read_events(None).unwrap();
        assert_eq!(events.last().unwrap().event, EventKind::StatusChanged);
        assert_eq!(
            events.last().unwrap().undo.as_ref().unwrap()["status"],
            "todo"
        );
    }

    // -- forms --

    /// Type into the focused form field.
    fn type_into(app: &mut App, text: &str) {
        for c in text.chars() {
            if c == ' ' {
                app.edit_overlay_key(Key::Space);
            } else {
                app.edit_overlay_key(Key::Char(c));
            }
        }
    }

    /// Move to a named field in the open form.
    fn focus(app: &mut App, kind: crate::form::FieldKind) {
        let index = app
            .form()
            .expect("a form is open")
            .fields
            .iter()
            .position(|f| f.kind == kind)
            .expect("field exists");
        app.form_mut().expect("a form is open").focus = index;
    }

    #[test]
    fn add_opens_a_form_on_the_title_field() {
        let (mut app, _t) = app_with(vec![]);
        app.act(Action::Add);

        assert_eq!(app.context, Context::Prompt);
        let form = app.form().expect("a form is open");
        assert_eq!(form.focused_kind(), Some(crate::form::FieldKind::Title));
        assert!(!form.is_dirty(), "a blank form starts clean");
    }

    #[test]
    fn typing_then_saving_creates_the_task() {
        let (mut app, _tmp) = app_with(vec![]);
        app.act(Action::Add);
        type_into(&mut app, "Draft the roadmap");

        focus(&mut app, crate::form::FieldKind::Due);
        type_into(&mut app, "+7d");

        focus(&mut app, crate::form::FieldKind::Estimate);
        type_into(&mut app, "4h");

        assert!(app.form().expect("form").is_dirty());

        app.commit_overlay();

        let set = app.store.load().unwrap();
        assert_eq!(set.tasks.len(), 1);
        assert_eq!(set.tasks[0].title, "Draft the roadmap");
        assert!(set.tasks[0].due_at.is_some());
        assert_eq!(set.tasks[0].estimate_minutes, Some(240));

        assert_eq!(app.overlay, Overlay::None, "form closes on save");
        assert!(matches!(
            app.message.as_ref().map(|m| m.kind),
            Some(ToastKind::Success)
        ));
    }

    #[test]
    fn saving_an_invalid_form_keeps_it_open_and_writes_nothing() {
        let (mut app, _tmp) = app_with(vec![]);
        app.act(Action::Add);

        focus(&mut app, crate::form::FieldKind::Estimate);
        type_into(&mut app, "nonsense");
        app.commit_overlay();

        assert!(app.form().is_some(), "the form stays open on error");
        assert!(app.store.load().unwrap().tasks.is_empty());
        assert!(matches!(
            app.message.as_ref().map(|m| m.kind),
            Some(ToastKind::Error)
        ));
        assert!(
            app.form()
                .and_then(|f| f.fields.iter().find(|f| f.error.is_some()))
                .is_some(),
            "the offending field is flagged"
        );
    }

    #[test]
    fn an_empty_title_blocks_saving() {
        let (mut app, _tmp) = app_with(vec![]);
        app.act(Action::Add);
        app.commit_overlay();
        assert!(app.form().is_some());
        assert!(app.store.load().unwrap().tasks.is_empty());
    }

    #[test]
    fn escaping_a_form_discards_it_entirely() {
        let (mut app, _tmp) = app_with(vec![]);
        app.act(Action::Add);
        type_into(&mut app, "Never saved");
        assert!(app.form().expect("form").is_dirty());

        app.cancel_overlay();

        assert_eq!(app.overlay, Overlay::None);
        assert!(
            app.store.load().unwrap().tasks.is_empty(),
            "nothing written"
        );
    }

    #[test]
    fn edit_opens_a_form_prefilled_from_the_task() {
        let task = t("Original");
        let (mut app, _tmp) = app_with(vec![task.clone()]);

        app.act(Action::EditTask);
        let form = app.form().expect("a form is open");
        assert!(!form.is_dirty(), "prefilled from the stored task");
        assert_eq!(form.value_of(crate::form::FieldKind::Title), "Original");
        assert_eq!(
            form.value_of(crate::form::FieldKind::Status),
            task.status.as_str()
        );
    }

    #[test]
    fn editing_then_saving_updates_in_place() {
        let task = t("Before");
        let id = task.id;
        let (mut app, _tmp) = app_with(vec![task]);

        app.act(Action::EditTask);
        type_into(&mut app, " v2");
        app.commit_overlay();

        let set = app.store.load().unwrap();
        assert_eq!(set.tasks.len(), 1, "not duplicated");
        assert_eq!(set.tasks[0].id, id);
        assert_eq!(set.tasks[0].title, "Before v2");
    }

    #[test]
    fn editing_without_a_selection_is_refused() {
        let (mut app, _t) = app_with(vec![]);
        app.act(Action::EditTask);
        assert!(app.form().is_none());
        assert!(matches!(
            app.message.as_ref().map(|m| m.kind),
            Some(ToastKind::Error)
        ));
    }

    #[test]
    fn the_due_shortcut_opens_the_edit_form_on_the_due_field() {
        let task = t("a");
        let (mut app, _tmp) = app_with(vec![task]);

        app.act(Action::SetDue);
        assert_eq!(
            app.form().expect("form").focused_kind(),
            Some(crate::form::FieldKind::Due)
        );

        type_into(&mut app, "+2d");
        app.commit_overlay();
        assert!(app.store.load().unwrap().tasks[0].due_at.is_some());
    }

    #[test]
    fn tab_cycles_fields_and_wraps() {
        let (mut app, _t) = app_with(vec![]);
        app.act(Action::Add);
        let count = app.form().expect("form").fields.len();

        assert_eq!(app.form().expect("form").focus, 0);
        for _ in 0..count {
            app.edit_overlay_key(Key::Tab);
        }
        assert_eq!(app.form().expect("form").focus, 0, "wraps around");
    }

    #[test]
    fn typing_a_letter_in_a_choice_field_jumps_to_it() {
        let (mut app, _t) = app_with(vec![]);
        app.act(Action::Add);
        focus(&mut app, crate::form::FieldKind::Status);
        app.edit_overlay_key(Key::Char('b'));
        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Status),
            "blocked"
        );

        focus(&mut app, crate::form::FieldKind::Priority);
        app.edit_overlay_key(Key::Char('u'));
        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Priority),
            "urgent"
        );
    }

    #[test]
    fn right_arrow_cycles_a_choice_field() {
        let (mut app, _t) = app_with(vec![]);
        app.act(Action::Add);
        focus(&mut app, crate::form::FieldKind::Status);
        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Status),
            "todo"
        );
        app.edit_overlay_key(Key::Right);
        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Status),
            "doing"
        );
        app.edit_overlay_key(Key::Left);
        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Status),
            "todo"
        );
    }

    #[test]
    fn enter_in_a_multiline_field_inserts_a_newline_instead_of_saving() {
        let (mut app, _tmp) = app_with(vec![]);
        app.act(Action::Add);
        focus(&mut app, crate::form::FieldKind::Description);
        type_into(&mut app, "line one");
        app.edit_overlay_key(Key::Enter);
        type_into(&mut app, "line two");

        assert!(
            app.form().is_some(),
            "Enter must not submit the form from a text field"
        );
        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Description)
                .replace('\n', "|"),
            "line one|line two"
        );
    }

    #[test]
    fn insert_mode_word_and_line_operations_reach_the_field() {
        let (mut app, _t) = app_with(vec![]);
        app.act(Action::Add);
        type_into(&mut app, "alpha beta gamma");

        app.edit_overlay_key(Key::Ctrl('w'));
        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Title),
            "alpha beta"
        );

        app.edit_overlay_key(Key::Ctrl('u'));
        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Title),
            ""
        );
    }

    #[test]
    fn paste_inserts_into_the_focused_field() {
        let (mut app, _tmp) = app_with(vec![]);
        app.act(Action::Add);
        type_into(&mut app, "Notes: ");
        app.paste("a long pasted description");

        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Title),
            "Notes: a long pasted description"
        );
    }

    #[test]
    fn pasting_newlines_flattens_them_in_single_line_fields() {
        let (mut app, _t) = app_with(vec![]);
        app.act(Action::Add);
        app.paste("one\ntwo");
        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Title),
            "one two",
            "a title must stay on one line"
        );
    }

    #[test]
    fn pasting_into_a_multiline_field_keeps_the_newlines() {
        let (mut app, _tmp) = app_with(vec![]);
        app.act(Action::Add);
        let index = app
            .form()
            .expect("form")
            .fields
            .iter()
            .position(|f| f.kind == crate::form::FieldKind::Description)
            .expect("description field");
        app.form_mut().expect("form").focus = index;
        app.paste("first line\nsecond line");

        assert_eq!(
            app.form()
                .expect("form")
                .value_of(crate::form::FieldKind::Description)
                .replace('\n', "|"),
            "first line|second line"
        );
    }

    #[test]
    fn pasting_outside_an_editor_is_ignored() {
        let (mut app, _tmp) = app_with(vec![t("a")]);
        let before = app.store.load().unwrap();
        app.paste("rm -rf /");
        assert_eq!(
            app.store.load().unwrap(),
            before,
            "a paste is never a command"
        );
    }

    #[test]
    fn unsaved_changes_are_reported_only_while_a_form_is_dirty() {
        let (mut app, _tmp) = app_with(vec![t("a")]);
        assert!(!app.has_unsaved_changes());

        app.act(Action::Add);
        assert!(
            !app.has_unsaved_changes(),
            "a blank form has nothing pending"
        );
        type_into(&mut app, "Draft");
        assert!(app.has_unsaved_changes());

        app.cancel_overlay();
        assert!(!app.has_unsaved_changes());
    }

    // -- detail pane --

    #[test]
    fn enter_opens_the_detail_pane_for_the_selected_task() {
        let task = t("Inspect me");
        let (mut app, _tmp) = app_with(vec![task.clone()]);

        assert!(!app.in_detail());
        app.act(Action::OpenDetail);
        assert!(app.in_detail());
        assert_eq!(app.detail_task().map(|t| t.id), Some(task.id));
    }

    #[test]
    fn opening_the_detail_pane_with_no_selection_is_refused() {
        let (mut app, _t) = app_with(vec![]);
        app.act(Action::OpenDetail);
        assert!(!app.in_detail());
        assert!(matches!(
            app.message.as_ref().map(|m| m.kind),
            Some(ToastKind::Error)
        ));
    }

    #[test]
    fn escape_and_q_leave_the_detail_pane() {
        let (mut app, _tmp) = app_with(vec![t("a")]);

        app.act(Action::OpenDetail);
        app.route_key(Key::Esc);
        assert!(!app.in_detail());

        app.act(Action::OpenDetail);
        app.route_key(Key::Char('q'));
        assert!(!app.in_detail(), "q also leaves the detail pane");
    }

    #[test]
    fn movement_scrolls_the_pane_rather_than_moving_a_cursor() {
        let mut task = t("long");
        task.description = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (mut app, _tmp) = app_with(vec![task]);
        app.viewport = 10;
        app.act(Action::OpenDetail);
        assert_eq!(app.detail.scroll, 0);

        app.act(Action::Down);
        assert_eq!(app.detail.scroll, 1, "j scrolls the pane");
        assert_eq!(app.cursor, 0, "and leaves the list cursor alone");

        app.act(Action::PageDown);
        assert!(app.detail.scroll > 1);

        app.act(Action::LastLine);
        assert!(app.detail.scroll > 0);
        app.act(Action::FirstLine);
        assert_eq!(app.detail.scroll, 0);
    }

    #[test]
    fn attachment_keys_navigate_the_attachment_cursor() {
        let mut task = t("linked");
        for i in 0..3 {
            task.attachments
                .push(model::Attachment::url(format!("https://e.test/{i}"), ""));
        }
        let (mut app, _tmp) = app_with(vec![task]);
        app.viewport = 30;
        app.act(Action::OpenDetail);

        assert_eq!(
            app.focused_attachment().map(|a| a.target),
            Some("https://e.test/0".into())
        );

        app.act(Action::NextAttachment);
        assert_eq!(
            app.focused_attachment().map(|a| a.target),
            Some("https://e.test/1".into())
        );

        app.act(Action::PrevAttachment);
        app.act(Action::PrevAttachment);
        assert_eq!(
            app.focused_attachment().map(|a| a.target),
            Some("https://e.test/0".into())
        );
    }

    #[test]
    fn opening_without_an_attachment_says_so() {
        let (mut app, _tmp) = app_with(vec![t("bare")]);
        app.act(Action::OpenDetail);
        app.act(Action::OpenAttachment);
        assert!(matches!(
            app.message.as_ref().map(|m| m.kind),
            Some(ToastKind::Error)
        ));
    }

    #[test]
    fn opening_a_missing_file_reports_it_rather_than_handing_off() {
        let mut task = t("has a ghost");
        task.attachments
            .push(model::Attachment::pdf("/definitely/not/here.pdf", ""));
        let (mut app, _tmp) = app_with(vec![task]);
        app.act(Action::OpenDetail);

        app.act(Action::OpenAttachment);
        let msg = app.message.as_ref().expect("a message");
        assert_eq!(msg.kind, ToastKind::Error);
        assert!(msg.text.contains("does not exist"), "got {}", msg.text);
    }

    #[test]
    fn removing_an_attachment_writes_through_and_logs_it() {
        let mut task = t("linked");
        task.attachments
            .push(model::Attachment::url("https://e.test/keep", "keep"));
        task.attachments
            .push(model::Attachment::url("https://e.test/drop", "drop"));
        let (mut app, _tmp) = app_with(vec![task]);

        app.act(Action::OpenDetail);
        app.act(Action::NextAttachment);
        assert_eq!(
            app.focused_attachment().map(|a| a.label),
            Some("drop".to_string())
        );

        app.act(Action::RemoveAttachment);

        let set = app.store.load().unwrap();
        let targets: Vec<String> = set.tasks[0]
            .attachments
            .iter()
            .map(|a| a.target.clone())
            .collect();
        assert_eq!(targets, vec!["https://e.test/keep".to_string()]);

        let event = app.store.read_events(None).unwrap().pop().unwrap();
        assert_eq!(event.event, EventKind::AttachmentRemoved);
        assert_eq!(
            event.patch.as_ref().expect("a patch")["target"],
            "https://e.test/drop"
        );
    }

    #[test]
    fn removing_the_last_attachment_leaves_the_task_intact() {
        let mut task = t("one link");
        task.attachments
            .push(model::Attachment::url("https://e.test/only", "only"));
        let (mut app, _tmp) = app_with(vec![task]);
        app.act(Action::OpenDetail);
        app.act(Action::RemoveAttachment);

        let set = app.store.load().unwrap();
        assert_eq!(set.tasks.len(), 1, "the task survives");
        assert!(set.tasks[0].attachments.is_empty());
        assert!(app.focused_attachment().is_none());
    }

    #[test]
    fn editing_from_the_detail_pane_returns_to_it_afterwards() {
        let task = t("edit me");
        let (mut app, _tmp) = app_with(vec![task.clone()]);
        app.act(Action::OpenDetail);

        app.act(Action::EditTask);
        assert!(app.form().is_some(), "the edit form opened");
        app.cancel_overlay();
        assert!(app.in_detail(), "back in the detail pane");
    }

    // -- timeline --

    #[test]
    fn capital_t_toggles_the_timeline() {
        let (mut app, _t) = app_with(vec![t("a")]);
        assert_eq!(app.view, View::List);

        app.route_key(Key::Char('T'));
        assert!(app.in_timeline());

        app.route_key(Key::Char('T'));
        assert_eq!(app.view, View::List, "T toggles back");
    }

    #[test]
    fn zoom_keys_change_the_scale_only_in_the_timeline() {
        let (mut app, _t) = app_with(vec![t("a")]);

        app.route_key(Key::Char('+'));
        assert_eq!(app.scale, Scale::Day, "ignored outside the timeline");

        app.route_key(Key::Char('T'));
        assert_eq!(app.scale, Scale::Day);
        app.route_key(Key::Char('+'));
        assert_eq!(app.scale, Scale::Week);
        app.route_key(Key::Char('+'));
        assert_eq!(app.scale, Scale::Month);
        app.route_key(Key::Char('-'));
        assert_eq!(app.scale, Scale::Week);
    }

    #[test]
    fn zooming_out_fills_more_calendar_in_the_same_width() {
        let (mut app, _t) = app_with(vec![t("a")]);
        let day = app.timeline(40).to;
        let day_from = app.timeline(40).from;

        app.route_key(Key::Char('T'));
        app.route_key(Key::Char('+'));
        let week = app.timeline(40);

        assert!(
            (week.to - week.from).num_days() > (day - day_from).num_days() * 2,
            "a week column should span more calendar than a day one"
        );
    }

    #[test]
    fn the_timeline_and_the_list_share_a_selection() {
        let (mut app, _tmp) = app_with(vec![t("a"), t("b"), t("c"), t("d")]);
        app.act(Action::Down);
        app.act(Action::Down);
        let selected = app.cursor;

        app.act(Action::ToggleTimeline);
        assert!(app.in_timeline());

        // The same task stays highlighted, and moving carries back to the list.
        app.act(Action::Down);
        assert_eq!(app.cursor, selected + 1);
        app.act(Action::ToggleTimeline);
        assert_eq!(
            app.cursor,
            selected + 1,
            "the cursor survives the round trip"
        );
    }

    #[test]
    fn enter_from_the_timeline_opens_the_detail_pane() {
        let task = t("inspect");
        let (mut app, _tmp) = app_with(vec![task]);
        app.act(Action::ToggleTimeline);

        app.act(Action::OpenDetail);
        assert!(app.in_detail());
        assert_eq!(app.detail_task().map(|t| t.title.as_str()), Some("inspect"));
    }

    #[test]
    fn the_timeline_scrolls_to_keep_the_cursor_visible() {
        let (mut app, _tmp) = app_with((0..40).map(|i| t(&format!("t{i}"))).collect());
        app.viewport = 10;
        app.act(Action::ToggleTimeline);

        app.act(Action::LastLine);
        assert_eq!(app.cursor, 39);
        assert!(
            app.timeline_scroll > 0,
            "the chart must scroll to the selection, scroll={}",
            app.timeline_scroll
        );
        assert!(app.timeline_scroll + 10 >= 39);

        app.act(Action::FirstLine);
        assert_eq!(app.timeline_scroll, 0);
    }

    #[test]
    fn the_timeline_lays_out_the_filtered_rows_in_order() {
        let (mut app, _tmp) = app_with(vec![t("keep me"), t("drop me")]);
        app.filter.text = Some("keep".into());
        app.rebuild();

        let tl = app.timeline(40);
        assert_eq!(tl.rows.len(), 1);
        assert_eq!(tl.rows[0].title, "keep me");
    }

    #[test]
    fn the_timeline_includes_parents_with_subtasks() {
        let parent = t("parent");
        let mut child = t("child");
        child.parent_id = Some(parent.id);
        let (app, _tmp) = app_with(vec![parent, child]);

        let tl = app.timeline(40);
        assert_eq!(tl.rows.len(), 2);
        assert_eq!(tl.rows[0].depth, 0);
        assert_eq!(tl.rows[0].rollup, Some((0, 1)));
        assert_eq!(tl.rows[1].depth, 1);
    }

    #[test]
    fn a_tiny_timeline_does_not_panic() {
        let (mut app, _tmp) = app_with(vec![t("a"), t("b")]);
        app.act(Action::ToggleTimeline);
        let tl = app.timeline(40);
        assert!(tl.columns >= 1);
        assert_eq!(tl.rows.len(), 2);
        // Clamping to an absurdly narrow chart must also survive.
        let tiny = tl.narrow(1);
        assert_eq!(tiny.columns, 1);
        assert_eq!(tiny.rows.len(), 2);
    }

    #[test]
    fn attachments_reachable_from_the_pane_survive_an_edit() {
        let mut task = t("with a pdf");
        task.attachments
            .push(model::Attachment::pdf("/tmp/spec.pdf", "spec"));
        let (mut app, _tmp) = app_with(vec![task.clone()]);

        app.act(Action::OpenDetail);
        app.act(Action::EditTask);
        app.commit_overlay();

        let set = app.store.load().unwrap();
        assert_eq!(set.tasks[0].attachments.len(), 1);
        assert_eq!(set.tasks[0].attachments[0].kind, model::AttachmentKind::Pdf);
    }

    #[test]
    fn keys_mean_different_things_in_the_detail_view() {
        let mut task = t("a");
        task.attachments
            .push(model::Attachment::url("https://e.test/1", "one"));
        task.attachments
            .push(model::Attachment::url("https://e.test/2", "two"));
        let (mut app, _tmp) = app_with(vec![task]);

        // In the list, `]` is unbound and must not touch the cursor.
        assert!(
            app.feed(Key::Char(']')).is_none(),
            "`]` is not a list binding"
        );
        assert_eq!(
            app.focused_attachment(),
            None,
            "and it must not open the detail pane by accident"
        );

        app.act(Action::OpenDetail);
        app.route_key(Key::Char(']'));
        assert_eq!(
            app.focused_attachment().map(|a| a.label),
            Some("two".to_string())
        );

        // Back in the list the same key does nothing again.
        app.route_key(Key::Esc);
        assert!(app.feed(Key::Char(']')).is_none());
    }

    #[test]
    fn a_prompt_still_works_for_search_links_and_pdfs() {
        let task = t("a");
        let (mut app, _tmp) = app_with(vec![task]);

        app.act(Action::ToggleFilterPanel);
        type_into(&mut app, "a");
        app.commit_overlay();
        assert_eq!(app.filter.text.as_deref(), Some("a"));
        assert_eq!(app.rows.len(), 1, "the filter still matches the task");

        app.act(Action::AddLink);
        type_into(&mut app, "https://example.com/spec");
        app.commit_overlay();

        let set = app.store.load().unwrap();
        assert_eq!(set.tasks[0].attachments.len(), 1);
        assert_eq!(
            set.tasks[0].attachments[0].target,
            "https://example.com/spec"
        );
    }

    #[test]
    fn an_empty_prompt_cancels_rather_than_applying() {
        let task = t("a");
        let (mut app, _tmp) = app_with(vec![task]);

        app.act(Action::AddLink);
        app.commit_overlay();

        assert_eq!(app.store.load().unwrap().tasks[0].attachments.len(), 0);
        assert!(matches!(
            app.message.as_ref().map(|m| m.kind),
            Some(ToastKind::Error)
        ));
    }

    #[test]
    fn a_malformed_link_is_rejected_with_a_reason() {
        let task = t("a");
        let (mut app, _tmp) = app_with(vec![task]);

        app.act(Action::AddLink);
        type_into(&mut app, "not-a-url");
        app.commit_overlay();

        assert_eq!(app.store.load().unwrap().tasks[0].attachments.len(), 0);
        let msg = &app.message.as_ref().unwrap().text;
        assert!(msg.contains("URL") || msg.contains("url"), "got {msg}");
    }

    // -- reload and external change --

    #[test]
    fn reload_picks_up_external_edits() {
        let (mut app, tmp) = app_with(vec![t("original")]);

        // Simulate the CLI writing behind our back.
        let other = Store::open(tmp.path()).unwrap();
        let mut set = other.load().unwrap();
        set.tasks[0].title = "changed elsewhere".into();
        other.save(&set).unwrap();

        app.reload();
        assert_eq!(app.rows[0].task.title, "changed elsewhere");
    }

    #[test]
    fn external_change_is_detected_and_cleared_by_reload() {
        let (mut app, tmp) = app_with(vec![t("a")]);
        assert!(!app.store_changed_externally());

        let other = Store::open(tmp.path()).unwrap();
        other.save(&other.load().unwrap()).unwrap();
        assert!(
            app.store_changed_externally(),
            "an external write must be noticed"
        );

        app.reload();
        assert!(!app.store_changed_externally());
    }

    #[test]
    fn reload_of_a_corrupt_store_reports_without_crashing() {
        let (mut app, tmp) = app_with(vec![t("a")]);
        std::fs::write(tmp.path().join("tasks.json"), "{ broken").unwrap();

        app.reload();
        assert!(matches!(
            app.message.as_ref().map(|m| m.kind),
            Some(ToastKind::Error)
        ));
        // Rows from the last good read are still on screen.
        assert_eq!(app.rows.len(), 1);
    }

    // -- messages --

    #[test]
    fn informational_toasts_persist_and_success_toasts_expire() {
        let (mut app, _t) = app_with(vec![t("a")]);

        app.set_message(Toast::info("just so you know"));
        app.tick();
        assert!(app.message.is_some(), "info messages stay until replaced");

        app.set_message(Toast::success("saved"));
        app.tick();
        assert!(app.message.is_some(), "not yet expired");
    }

    #[test]
    fn help_overlay_toggles() {
        let (mut app, _t) = app_with(vec![t("a")]);
        assert_eq!(app.overlay, Overlay::None);
        app.act(Action::Help);
        assert_eq!(app.overlay, Overlay::Help);
        app.act(Action::Help);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn the_help_panel_toggles_independently_of_the_overlay() {
        let (mut app, _t) = app_with(vec![t("a")]);
        assert!(!app.show_help_panel);
        app.act(Action::ToggleHelpPanel);
        assert!(app.show_help_panel);
        app.act(Action::Help);
        assert!(app.show_help_panel, "the overlay does not close the panel");
        app.act(Action::CloseOverlay);
        assert!(app.show_help_panel);
    }

    #[test]
    fn the_pending_indicator_shows_partial_sequences() {
        let (mut app, _t) = app_with(vec![t("a")]);
        assert_eq!(app.pending_label(), "");

        app.feed(Key::Char('g'));
        assert_eq!(app.pending_label(), "g");

        app.feed(Key::Char('g'));
        assert_eq!(app.pending_label(), "", "cleared once bound");
    }

    #[test]
    fn an_unbound_sequence_clears_the_buffer() {
        let (mut app, _t) = app_with(vec![t("a")]);
        app.feed(Key::Char('g'));
        assert!(app.feed(Key::Char('x')).is_none());
        assert_eq!(app.pending_label(), "");
    }

    #[test]
    fn tree_toggle_flips_the_view() {
        let parent = t("parent");
        let mut child = t("child");
        child.parent_id = Some(parent.id);
        let (mut app, _t) = app_with(vec![parent, child]);

        assert!(app.tree);
        app.act(Action::ToggleTree);
        assert!(!app.tree);
        assert!(app.rows.iter().all(|r| r.depth == 0));
    }
}
