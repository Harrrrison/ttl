//! Task creation and editing forms.
//!
//! A form buffers field values in memory and commits them to the store in one
//! atomic write. Nothing is written until the form is saved, so abandoning one
//! with `<Esc>` can never leave a half-created task behind.

use chrono::{DateTime, Utc};

use crate::editor::TextBuffer;
use crate::model::{self, Attachment, Priority, Status, Task, TaskId};
use crate::store::{Event, EventKind, Store, StoreError};

/// What a form will do when saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormMode {
    /// Create a new task.
    Add,
    /// Modify an existing task.
    Edit(TaskId),
}

/// The semantic type of a field, which decides how it validates and how it
/// presents a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Title,
    Description,
    Due,
    Estimate,
    Status,
    Priority,
    Tags,
    Links,
}

impl FieldKind {
    /// All fields, in tab order.
    pub const ALL: [FieldKind; 8] = [
        FieldKind::Title,
        FieldKind::Description,
        FieldKind::Due,
        FieldKind::Estimate,
        FieldKind::Status,
        FieldKind::Priority,
        FieldKind::Tags,
        FieldKind::Links,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            FieldKind::Title => "title",
            FieldKind::Description => "description",
            FieldKind::Due => "due",
            FieldKind::Estimate => "estimate",
            FieldKind::Status => "status",
            FieldKind::Priority => "priority",
            FieldKind::Tags => "tags",
            FieldKind::Links => "links",
        }
    }

    /// Placeholder shown when the field is empty.
    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            FieldKind::Title => "what needs doing",
            FieldKind::Description => "details, notes, context",
            FieldKind::Due => "2026-11-01, +3d, tomorrow",
            FieldKind::Estimate => "4h, 90m, 2d, 1d 4h",
            FieldKind::Status => "todo / doing / blocked",
            FieldKind::Priority => "low / normal / high / urgent",
            FieldKind::Tags => "comma separated",
            FieldKind::Links => "one URL per line",
        }
    }

    /// Fields edited as free text; the rest cycle with Left/Right.
    #[must_use]
    pub fn is_text(self) -> bool {
        !matches!(self, FieldKind::Status | FieldKind::Priority)
    }

    #[must_use]
    pub fn is_multiline(self) -> bool {
        matches!(self, FieldKind::Description | FieldKind::Links)
    }

    #[must_use]
    pub fn is_required(self) -> bool {
        matches!(self, FieldKind::Title)
    }

    /// The value a typed first letter should jump to, if any.
    ///
    /// Lets `d` select "doing" in the status field instead of inserting a
    /// character into what is really a dropdown.
    #[must_use]
    pub fn jump_target(self, typed: char) -> Option<&'static str> {
        let t = typed.to_ascii_lowercase();
        match self {
            FieldKind::Status => Status::ALL
                .into_iter()
                .map(|s| s.as_str())
                .find(|s| s.starts_with(t)),
            FieldKind::Priority => Priority::ALL
                .into_iter()
                .map(|p| p.as_str())
                .find(|p| p.starts_with(t)),
            _ => None,
        }
    }

    /// How many display rows the field gets when focused.
    #[must_use]
    pub fn rows(self) -> usize {
        match self {
            FieldKind::Description => 5,
            FieldKind::Links => 3,
            _ => 1,
        }
    }
}

/// One editable field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub kind: FieldKind,
    pub buf: TextBuffer,
    /// Set when the value failed to parse; shown under the field.
    pub error: Option<String>,
    /// Display scroll offset, in wrapped rows.
    pub scroll: usize,
}

impl Field {
    #[must_use]
    pub fn new(kind: FieldKind, text: &str) -> Self {
        let buf = if kind.is_multiline() {
            TextBuffer::multiline(text)
        } else {
            TextBuffer::single_line(text)
        };
        Self {
            kind,
            buf,
            error: None,
            scroll: 0,
        }
    }

    /// The stored value, trimmed and normalised.
    #[must_use]
    pub fn value(&self) -> String {
        self.buf.trimmed()
    }

    /// Validate this field in isolation, clearing any stale error.
    pub fn validate(&mut self, now: DateTime<Utc>) -> Result<(), String> {
        let raw = self.value();
        let result = match self.kind {
            FieldKind::Title => {
                if raw.is_empty() {
                    Err("a title is required".to_string())
                } else if raw.chars().count() > 500 {
                    Err("title is limited to 500 characters".to_string())
                } else {
                    Ok(())
                }
            }
            FieldKind::Description => {
                if raw.chars().count() > 100_000 {
                    Err("description is limited to 100000 characters".to_string())
                } else {
                    Ok(())
                }
            }
            FieldKind::Due => {
                if raw.is_empty() {
                    Ok(())
                } else {
                    model::parse_datetime(&raw, now)
                        .map(|_| ())
                        .ok_or_else(|| "expected a date like 2026-11-01, +3d, or tomorrow".into())
                }
            }
            FieldKind::Estimate => {
                if raw.is_empty() {
                    Ok(())
                } else {
                    model::parse_estimate_minutes(&raw)
                        .map(|_| ())
                        .ok_or_else(|| "expected something like 4h, 90m, or 1d 4h".into())
                }
            }
            FieldKind::Status => Status::parse(&raw)
                .map(|_| ())
                .ok_or_else(|| "expected todo, doing, blocked, done, or cancelled".into()),
            FieldKind::Priority => Priority::parse(&raw)
                .map(|_| ())
                .ok_or_else(|| "expected low, normal, high, or urgent".into()),
            FieldKind::Tags => {
                if raw.split(',').filter(|t| !t.trim().is_empty()).count() > 32 {
                    Err("at most 32 tags".to_string())
                } else {
                    Ok(())
                }
            }
            FieldKind::Links => {
                for line in raw.lines().filter(|l| !l.trim().is_empty()) {
                    let candidate = Attachment::url(line.trim(), "");
                    candidate.validate().map_err(|e| e.to_string())?;
                }
                Ok(())
            }
        };
        self.error = result.as_ref().err().cloned();
        result
    }

    /// Cycle a choice field to its next value. Returns true if it changed.
    pub fn cycle(&mut self, forward: bool) -> bool {
        let current = self.value();
        let changed = match self.kind {
            FieldKind::Status => {
                let all = Status::ALL;
                let idx = all.iter().position(|s| s.as_str() == current).unwrap_or(0);
                let next = if forward {
                    (idx + 1) % all.len()
                } else {
                    (idx + all.len() - 1) % all.len()
                };
                self.buf.set_text(all[next].as_str());
                true
            }
            FieldKind::Priority => {
                let all = Priority::ALL;
                let idx = all.iter().position(|p| p.as_str() == current).unwrap_or(1);
                let next = if forward {
                    (idx + 1) % all.len()
                } else {
                    (idx + all.len() - 1) % all.len()
                };
                self.buf.set_text(all[next].as_str());
                true
            }
            _ => false,
        };
        if changed {
            self.error = None;
        }
        changed
    }
}

/// A whole form, focused on one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub mode: FormMode,
    pub fields: Vec<Field>,
    pub focus: usize,
    /// Set when saving failed, shown at the top of the form.
    pub banner: Option<String>,
    /// Field values when the form opened, for change detection.
    original: Vec<String>,
}

impl Form {
    /// A blank form for creating a task.
    #[must_use]
    pub fn add() -> Self {
        let fields = vec![
            Field::new(FieldKind::Title, ""),
            Field::new(FieldKind::Description, ""),
            Field::new(FieldKind::Due, ""),
            Field::new(FieldKind::Estimate, ""),
            Field::new(FieldKind::Status, Status::Todo.as_str()),
            Field::new(FieldKind::Priority, Priority::Normal.as_str()),
            Field::new(FieldKind::Tags, ""),
            Field::new(FieldKind::Links, ""),
        ];
        let original = fields.iter().map(|f| f.value()).collect();
        Self {
            mode: FormMode::Add,
            fields,
            focus: 0,
            banner: None,
            original,
        }
    }

    /// A form prefilled from an existing task.
    #[must_use]
    pub fn edit(task: &Task) -> Self {
        let links = task
            .attachments
            .iter()
            .filter(|a| a.kind == model::AttachmentKind::Url)
            .map(|a| a.target.clone())
            .collect::<Vec<_>>()
            .join("\n");

        let fields = vec![
            Field::new(FieldKind::Title, &task.title),
            Field::new(FieldKind::Description, &task.description),
            Field::new(FieldKind::Due, &fmt_opt(task.due_at)),
            Field::new(
                FieldKind::Estimate,
                &task.estimate_human().unwrap_or_default(),
            ),
            Field::new(FieldKind::Status, task.status.as_str()),
            Field::new(FieldKind::Priority, task.priority.as_str()),
            Field::new(FieldKind::Tags, &task.tags.join(", ")),
            Field::new(FieldKind::Links, &links),
        ];
        let original = fields.iter().map(|f| f.value()).collect();
        Self {
            mode: FormMode::Edit(task.id),
            fields,
            focus: 0,
            banner: None,
            original,
        }
    }

    #[must_use]
    pub fn focused(&self) -> Option<&Field> {
        self.fields.get(self.focus)
    }

    pub fn focused_mut(&mut self) -> Option<&mut Field> {
        self.fields.get_mut(self.focus)
    }

    #[must_use]
    pub fn focused_kind(&self) -> Option<FieldKind> {
        self.fields.get(self.focus).map(|f| f.kind)
    }

    /// Move focus by `delta` fields, wrapping around.
    pub fn move_focus(&mut self, delta: isize) {
        if self.fields.is_empty() {
            return;
        }
        let len = self.fields.len() as isize;
        let next = (self.focus as isize + delta).rem_euclid(len);
        self.focus = next as usize;
    }

    pub fn focus_next(&mut self) {
        self.move_focus(1);
    }

    pub fn focus_prev(&mut self) {
        self.move_focus(-1);
    }

    /// Jump to the first field with an error.
    pub fn focus_first_error(&mut self) -> bool {
        match self.fields.iter().position(|f| f.error.is_some()) {
            Some(i) => {
                self.focus = i;
                true
            }
            None => false,
        }
    }

    /// Any field value differs from how the form was opened.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.fields
            .iter()
            .map(|f| f.value())
            .ne(self.original.iter().cloned())
    }

    /// Validate every field. Returns the number of invalid fields.
    pub fn validate(&mut self, now: DateTime<Utc>) -> usize {
        let mut errors = 0;
        for field in &mut self.fields {
            if field.validate(now).is_err() {
                errors += 1;
            }
        }
        self.banner = if errors == 0 {
            None
        } else {
            Some(format!(
                "{} field{} need{} attention",
                errors,
                if errors == 1 { "" } else { "s" },
                if errors == 1 { "s" } else { "" }
            ))
        };
        errors
    }

    /// Build the task this form describes. Only valid after [`Form::validate`].
    fn to_task(&self, now: DateTime<Utc>) -> Result<Task, String> {
        let mut task = Task::new(self.value_of(FieldKind::Title), now);
        task.description = self.value_of(FieldKind::Description);

        let due = self.value_of(FieldKind::Due);
        if !due.is_empty() {
            task.due_at = Some(
                model::parse_datetime(&due, now).ok_or_else(|| format!("bad due date {due:?}"))?,
            );
        }
        let est = self.value_of(FieldKind::Estimate);
        if !est.is_empty() {
            task.estimate_minutes = Some(
                model::parse_estimate_minutes(&est)
                    .ok_or_else(|| format!("bad estimate {est:?}"))?,
            );
        }
        task.status = Status::parse(&self.value_of(FieldKind::Status)).unwrap_or(Status::Todo);
        task.priority =
            Priority::parse(&self.value_of(FieldKind::Priority)).unwrap_or(Priority::Normal);
        if task.status == Status::Doing {
            task.started_at = Some(now);
        }
        task.tags = self
            .value_of(FieldKind::Tags)
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();

        for line in self
            .value_of(FieldKind::Links)
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            task.attachments
                .push(Attachment::url(line.trim().to_string(), ""));
        }

        Ok(task)
    }

    /// The stored value of one field, trimmed.
    #[must_use]
    pub fn value_of(&self, kind: FieldKind) -> String {
        self.fields
            .iter()
            .find(|f| f.kind == kind)
            .map(Field::value)
            .unwrap_or_default()
    }

    /// Commit to the store.
    ///
    /// Validates first and reports which fields are wrong, so nothing is written
    /// unless the whole form is sound.
    pub fn save(&mut self, store: &Store) -> Result<Task, SaveError> {
        let now = Utc::now();
        if self.validate(now) > 0 {
            self.focus_first_error();
            return Err(SaveError::Invalid(self.fields[0].error.clone()));
        }
        let parsed = self.to_task(now).map_err(|e| SaveError::Invalid(Some(e)))?;

        match self.mode.clone() {
            FormMode::Add => {
                let created = store
                    .mutate(|set, events| {
                        set.insert(parsed.clone())?;
                        events.push(
                            Event::new(EventKind::TaskCreated, Some(parsed.id))
                                .with_title(parsed.title.clone()),
                        );
                        Ok(parsed.clone())
                    })
                    .map_err(|e| SaveError::Store(e.to_string()))?;
                Ok(created)
            }
            FormMode::Edit(id) => {
                let saved = store
                    .mutate(|set, events| {
                        let existing = set.get(&id).cloned().ok_or(StoreError::NotFound(id))?;

                        let mut updated = parsed.clone();
                        updated.id = id;
                        updated.created_at = existing.created_at;
                        // The form manages URLs; anything else the task carries
                        // (currently PDFs) is invisible to it and must survive.
                        updated.attachments.extend(
                            existing
                                .attachments
                                .iter()
                                .filter(|a| a.kind != model::AttachmentKind::Url)
                                .cloned(),
                        );

                        // Reuse the stored object for any link whose target is
                        // unchanged, so `added_at` survives an edit and an
                        // untouched link set does not register as a change.
                        for slot in &mut updated.attachments {
                            if let Some(previous) = existing
                                .attachments
                                .iter()
                                .find(|a| a.kind == slot.kind && a.target == slot.target)
                            {
                                *slot = previous.clone();
                            }
                        }
                        updated.updated_at = now;
                        // Status transitions stamp the lifecycle fields.
                        if updated.status == existing.status {
                            updated.started_at = existing.started_at;
                            updated.completed_at = existing.completed_at;
                        } else if updated.status == Status::Done {
                            updated.started_at = existing.started_at.or(Some(now));
                            updated.completed_at = Some(now);
                        } else if updated.status.is_terminal() {
                            updated.completed_at = Some(now);
                        } else {
                            updated.completed_at = None;
                            if updated.status == Status::Doing {
                                updated.started_at = existing.started_at.or(Some(now));
                            }
                        }

                        updated.validate()?;

                        let patch = diff(&existing, &updated);
                        let undo = inverse(&existing, &updated);

                        if let Some(slot) = set.get_mut(&id) {
                            *slot = updated.clone();
                        }
                        events.push(
                            Event::new(EventKind::TaskUpdated, Some(id))
                                .with_title(updated.title.clone())
                                .with_patch(patch)
                                .with_undo(undo),
                        );
                        Ok(updated)
                    })
                    .map_err(|e| SaveError::Store(e.to_string()))?;
                Ok(saved)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SaveError {
    #[error("invalid input: {}", .0.as_deref().unwrap_or("see the highlighted fields"))]
    Invalid(Option<String>),
    #[error("could not write to the store: {0}")]
    Store(String),
}

/// Fields that differ between two tasks, as a JSON patch.
fn diff(before: &Task, after: &Task) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if before.title != after.title {
        map.insert("title".into(), serde_json::json!(after.title));
    }
    if before.description != after.description {
        map.insert("description".into(), serde_json::json!(after.description));
    }
    if before.due_at != after.due_at {
        map.insert("due_at".into(), serde_json::json!(after.due_at));
    }
    if before.estimate_minutes != after.estimate_minutes {
        map.insert(
            "estimate_minutes".into(),
            serde_json::json!(after.estimate_minutes),
        );
    }
    if before.status != after.status {
        map.insert("status".into(), serde_json::json!(after.status.as_str()));
    }
    if before.priority != after.priority {
        map.insert(
            "priority".into(),
            serde_json::json!(after.priority.as_str()),
        );
    }
    if before.tags != after.tags {
        map.insert("tags".into(), serde_json::json!(after.tags));
    }
    if before.attachments != after.attachments {
        map.insert("attachments".into(), serde_json::json!(after.attachments));
    }
    serde_json::Value::Object(map)
}

/// The `before` values of everything [`diff`] recorded, so the change can be
/// reverted.
fn inverse(before: &Task, after: &Task) -> serde_json::Value {
    let patch = diff(before, after);
    let mut map = serde_json::Map::new();
    let Some(fields) = patch.as_object() else {
        return serde_json::Value::Object(map);
    };
    for key in fields.keys() {
        let value = match key.as_str() {
            "title" => serde_json::json!(before.title),
            "description" => serde_json::json!(before.description),
            "due_at" => serde_json::json!(before.due_at),
            "estimate_minutes" => serde_json::json!(before.estimate_minutes),
            "status" => serde_json::json!(before.status.as_str()),
            "priority" => serde_json::json!(before.priority.as_str()),
            "tags" => serde_json::json!(before.tags),
            "attachments" => serde_json::json!(before.attachments),
            _ => continue,
        };
        map.insert(key.clone(), value);
    }
    serde_json::Value::Object(map)
}

/// RFC-3339 for an optional timestamp, or an empty string.
fn fmt_opt(t: Option<DateTime<Utc>>) -> String {
    t.map_or_else(String::new, |d| {
        // Precision is preserved exactly: rounding to whole seconds would
        // silently rewrite the stored deadline on every edit.
        d.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;
    use tempfile::TempDir;

    fn store() -> (Store, TempDir) {
        let tmp = TempDir::new().unwrap();
        (Store::open(tmp.path()).unwrap(), tmp)
    }

    /// Put a task into the store so an edit form has something to edit.
    fn insert(store: &Store, mut task: Task) -> Task {
        store
            .mutate(|set, _| {
                set.insert(task.clone())?;
                Ok(())
            })
            .unwrap();
        task.updated_at = Utc::now();
        task
    }

    fn set(form: &mut Form, kind: FieldKind, value: &str) {
        let f = form.fields.iter_mut().find(|f| f.kind == kind).unwrap();
        f.buf.set_text(value);
    }

    // -- field validation --

    #[test]
    fn an_empty_title_is_invalid() {
        let (s, _t) = store();
        let mut form = Form::add();
        assert!(matches!(form.save(&s), Err(SaveError::Invalid(_))));
        assert_eq!(form.fields[0].error.as_deref(), Some("a title is required"));
        assert_eq!(form.focus, 0, "focus jumps to the bad field");
    }

    #[test]
    fn bad_dates_and_estimates_are_reported_per_field() {
        let now = Utc::now();
        let mut title = Field::new(FieldKind::Title, "ok");
        let mut due = Field::new(FieldKind::Due, "next tuesday");
        let mut est = Field::new(FieldKind::Estimate, "a while");

        assert!(title.validate(now).is_ok());
        assert!(due.validate(now).is_err());
        assert!(est.validate(now).is_err());
        assert!(due.error.is_some() && est.error.is_some());
    }

    #[test]
    fn a_blank_due_or_estimate_is_fine() {
        let now = Utc::now();
        assert!(Field::new(FieldKind::Due, "").validate(now).is_ok());
        assert!(Field::new(FieldKind::Estimate, "").validate(now).is_ok());
    }

    #[test]
    fn links_field_validates_every_line() {
        let now = Utc::now();
        let mut ok = Field::new(FieldKind::Links, "https://a.test\nhttps://b.test");
        assert!(ok.validate(now).is_ok());

        let mut bad = Field::new(FieldKind::Links, "https://a.test\nnope");
        assert!(bad.validate(now).is_err());

        let mut blank = Field::new(FieldKind::Links, "\n  \n");
        assert!(blank.validate(now).is_ok(), "blank lines are ignored");
    }

    #[test]
    fn status_and_priority_validate_their_vocabulary() {
        let now = Utc::now();
        assert!(Field::new(FieldKind::Status, "doing").validate(now).is_ok());
        assert!(
            Field::new(FieldKind::Status, "finished")
                .validate(now)
                .is_err()
        );
        assert!(
            Field::new(FieldKind::Priority, "urgent")
                .validate(now)
                .is_ok()
        );
        assert!(
            Field::new(FieldKind::Priority, "asap")
                .validate(now)
                .is_err()
        );
    }

    #[test]
    fn cycling_choice_fields_wraps_in_both_directions() {
        let mut f = Field::new(FieldKind::Status, "todo");
        f.cycle(true);
        assert_eq!(f.value(), "doing");
        f.cycle(false);
        assert_eq!(f.value(), "todo");
        f.cycle(false);
        assert_eq!(f.value(), "cancelled", "wraps backwards");

        let mut p = Field::new(FieldKind::Priority, "low");
        p.cycle(false);
        assert_eq!(p.value(), "urgent");
    }

    #[test]
    fn cycling_does_nothing_for_text_fields() {
        let mut f = Field::new(FieldKind::Title, "hello");
        assert!(!f.cycle(true));
        assert_eq!(f.value(), "hello");
    }

    // -- focus --

    #[test]
    fn focus_wraps_in_both_directions() {
        let mut form = Form::add();
        let n = form.fields.len() as isize;

        form.focus_prev();
        assert_eq!(
            form.focus as isize,
            n - 1,
            "backwards from the top wraps to the end"
        );

        form.focus_next();
        assert_eq!(form.focus, 0);
    }

    #[test]
    fn focus_first_error_targets_the_problem() {
        let now = Utc::now();
        let mut form = Form::add();
        set(&mut form, FieldKind::Title, "fine");
        set(&mut form, FieldKind::Due, "whenever");
        set(&mut form, FieldKind::Estimate, "some");

        assert_eq!(form.validate(now), 2);
        assert!(form.focus_first_error());
        assert_eq!(form.focused_kind(), Some(FieldKind::Due));
    }

    #[test]
    fn a_valid_form_reports_no_errors() {
        let now = Utc::now();
        let mut form = Form::add();
        set(&mut form, FieldKind::Title, "Ship it");
        set(&mut form, FieldKind::Due, "+3d");
        assert_eq!(form.validate(now), 0);
        assert!(form.banner.is_none());
    }

    // -- change detection --

    #[test]
    fn dirtiness_is_detected_and_resets_to_the_opened_state() {
        let mut form = Form::add();
        assert!(!form.is_dirty());

        set(&mut form, FieldKind::Title, "New task");
        assert!(form.is_dirty());

        set(&mut form, FieldKind::Title, "");
        assert!(!form.is_dirty(), "returning to the original value is clean");
    }

    #[test]
    fn an_edit_form_is_clean_until_something_changes() {
        let task = Task::new("Original", Utc::now());
        let form = Form::edit(&task);
        assert!(!form.is_dirty());
    }

    // -- saving new tasks --

    #[test]
    fn adding_a_task_writes_it_to_the_store() {
        let (s, _t) = store();
        let mut form = Form::add();
        set(&mut form, FieldKind::Title, "  Draft roadmap  ");
        set(&mut form, FieldKind::Description, "three themes");
        set(&mut form, FieldKind::Due, "+7d");
        set(&mut form, FieldKind::Estimate, "4h");
        set(&mut form, FieldKind::Priority, "high");
        set(&mut form, FieldKind::Tags, " work , q4 ,");
        set(
            &mut form,
            FieldKind::Links,
            "https://a.test\nhttps://b.test",
        );

        let task = form.save(&s).expect("saves");

        assert_eq!(task.title, "Draft roadmap", "trimmed on the way in");
        assert_eq!(task.description, "three themes");
        assert!(task.due_at.is_some());
        assert_eq!(task.estimate_minutes, Some(240));
        assert_eq!(task.priority, Priority::High);
        assert_eq!(task.tags, vec!["work", "q4"], "blanks dropped");
        assert_eq!(task.attachments.len(), 2);

        let set_on_disk = s.load().unwrap();
        assert_eq!(set_on_disk.tasks.len(), 1);
        assert_eq!(set_on_disk.tasks[0], task);
    }

    #[test]
    fn adding_records_an_event() {
        let (s, _t) = store();
        let mut form = Form::add();
        set(&mut form, FieldKind::Title, "Logged");
        form.save(&s).unwrap();

        let events = s.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, EventKind::TaskCreated);
        assert_eq!(events[0].title.as_deref(), Some("Logged"));
    }

    #[test]
    fn an_invalid_add_writes_nothing() {
        let (s, _t) = store();
        let mut form = Form::add();
        set(&mut form, FieldKind::Title, "ok");
        set(&mut form, FieldKind::Estimate, "nope");

        assert!(matches!(form.save(&s), Err(SaveError::Invalid(_))));
        assert!(s.load().unwrap().tasks.is_empty(), "nothing written");
        assert!(!s.events_path().exists(), "no events on a failed save");
    }

    #[test]
    fn starting_a_task_stamps_the_start_time() {
        let (s, _t) = store();
        let mut form = Form::add();
        set(&mut form, FieldKind::Title, "WIP");
        set(&mut form, FieldKind::Status, "doing");
        let task = form.save(&s).unwrap();
        assert!(task.started_at.is_some());
        assert!(task.completed_at.is_none());
    }

    // -- editing --

    #[test]
    fn editing_updates_the_task_and_keeps_its_identity() {
        let (s, _t) = store();
        let created = {
            let mut form = Form::add();
            set(&mut form, FieldKind::Title, "Before");
            set(&mut form, FieldKind::Description, "keep me");
            set(&mut form, FieldKind::Tags, "keep");
            form.save(&s).unwrap()
        };

        let mut form = Form::edit(&created);
        set(&mut form, FieldKind::Title, "After");
        set(&mut form, FieldKind::Estimate, "2h");
        let updated = form.save(&s).unwrap();

        assert_eq!(updated.id, created.id, "identity is preserved");
        assert_eq!(updated.created_at, created.created_at);
        assert_eq!(updated.title, "After");
        assert_eq!(updated.description, "keep me", "unrelated fields survive");
        assert_eq!(updated.tags, vec!["keep"]);
        assert_eq!(updated.estimate_minutes, Some(120));

        assert_eq!(s.load().unwrap().tasks.len(), 1, "not duplicated");
    }

    #[test]
    fn editing_preserves_pdf_attachments() {
        let (s, _t) = store();
        let mut task = Task::new("Docs", Utc::now());
        task.attachments
            .push(Attachment::pdf("/tmp/spec.pdf", "spec"));
        s.mutate(|set, _| {
            set.insert(task.clone())?;
            Ok(())
        })
        .unwrap();

        let mut form = Form::edit(&task);
        set(&mut form, FieldKind::Title, "Docs v2");
        let updated = form.save(&s).unwrap();

        assert!(
            updated
                .attachments
                .iter()
                .any(|a| a.kind == model::AttachmentKind::Pdf),
            "a PDF the form does not show must not be dropped: {:?}",
            updated.attachments
        );
    }

    #[test]
    fn editing_records_a_patch_and_its_inverse() {
        let (s, _t) = store();
        let mut form = Form::add();
        set(&mut form, FieldKind::Title, "Original");
        set(&mut form, FieldKind::Estimate, "1h");
        let task = form.save(&s).unwrap();

        let mut form = Form::edit(&task);
        set(&mut form, FieldKind::Title, "Changed");
        set(&mut form, FieldKind::Estimate, "9h");
        form.save(&s).unwrap();

        let event = s.read_events(None).unwrap().pop().unwrap();
        assert_eq!(event.event, EventKind::TaskUpdated);
        let patch = event.patch.as_ref().expect("a patch was recorded");
        assert_eq!(patch["title"], "Changed");
        assert_eq!(patch["estimate_minutes"], 540);
        // The inverse must describe the values to restore.
        assert_eq!(event.undo.as_ref().unwrap()["title"], "Original");
        assert_eq!(event.undo.as_ref().unwrap()["estimate_minutes"], 60);
    }

    #[test]
    fn editing_a_deleted_task_reports_not_found() {
        let (s, _t) = store();
        let task = Task::new("Gone", Utc::now());
        let mut form = Form::edit(&task);
        set(&mut form, FieldKind::Title, "Anything");

        assert!(matches!(form.save(&s), Err(SaveError::Store(_))));
    }

    #[test]
    fn marking_done_through_the_form_stamps_completion() {
        let (s, _t) = store();
        let mut form = Form::add();
        set(&mut form, FieldKind::Title, "Finish me");
        let task = form.save(&s).unwrap();
        assert!(task.completed_at.is_none());

        let mut form = Form::edit(&task);
        set(&mut form, FieldKind::Status, "done");
        let done = form.save(&s).unwrap();
        assert!(done.completed_at.is_some());
        assert!(done.started_at.is_some());

        // Reopening clears the completion.
        let mut form = Form::edit(&done);
        set(&mut form, FieldKind::Status, "todo");
        let reopened = form.save(&s).unwrap();
        assert!(reopened.completed_at.is_none());
    }

    #[test]
    fn an_edit_that_changes_nothing_records_an_empty_patch() {
        let (s, _t) = store();
        let mut form = Form::add();
        set(&mut form, FieldKind::Title, "Same");
        let task = form.save(&s).unwrap();

        let mut form = Form::edit(&task);
        // Re-typing the same value counts as no change.
        set(&mut form, FieldKind::Title, "Same");
        form.save(&s).unwrap();

        let event = s.read_events(None).unwrap().pop().unwrap();
        assert_eq!(
            event.patch.as_ref().expect("an event was recorded"),
            &serde_json::json!({})
        );
    }

    #[test]
    fn editing_preserves_due_date_precision_exactly() {
        let (s, _t) = store();
        let mut task = Task::new("Precise", Utc::now());
        // Sub-second precision, as a real due date usually carries.
        task.due_at = Some(
            Utc::now()
                .with_nanosecond(123_456_789)
                .expect("valid nanoseconds"),
        );
        let task = insert(&s, task);
        let original = task.due_at.expect("set");

        let mut form = Form::edit(&task);
        set(&mut form, FieldKind::Title, "Precise v2");
        let updated = form.save(&s).unwrap();

        assert_eq!(
            updated.due_at,
            Some(original),
            "a form round-trip must not round the stored deadline"
        );

        // And it should not show up as a change in the audit trail.
        let event = s.read_events(None).unwrap().pop().unwrap();
        let patch = event.patch.as_ref().expect("patch");
        assert!(
            !patch.as_object().is_some_and(|o| o.contains_key("due_at")),
            "an unchanged deadline must not be recorded as edited: {patch}"
        );
    }

    #[test]
    fn editing_preserves_attachment_timestamps_for_untouched_links() {
        let (s, _t) = store();
        let mut task = Task::new("Linked", Utc::now());
        task.attachments
            .push(Attachment::url("https://a.test", "spec"));
        let task = insert(&s, task);
        let original_added_at = task.attachments[0].added_at;

        let mut form = Form::edit(&task);
        set(&mut form, FieldKind::Title, "Linked v2");
        let updated = form.save(&s).unwrap();

        assert_eq!(
            updated.attachments[0].added_at, original_added_at,
            "an untouched link keeps its original timestamp"
        );

        let event = s.read_events(None).unwrap().pop().unwrap();
        let patch = event.patch.as_ref().expect("patch");
        assert!(
            !patch
                .as_object()
                .is_some_and(|o| o.contains_key("attachments")),
            "an unchanged link set must not be recorded as edited: {patch}"
        );
    }

    #[test]
    fn links_can_be_removed_by_blanking_the_field() {
        let (s, _t) = store();
        let mut form = Form::add();
        set(&mut form, FieldKind::Title, "Has links");
        set(&mut form, FieldKind::Links, "https://a.test");
        let task = form.save(&s).unwrap();
        assert_eq!(task.attachments.len(), 1);

        let mut form = Form::edit(&task);
        set(&mut form, FieldKind::Links, "");
        let updated = form.save(&s).unwrap();
        assert!(
            updated.attachments.is_empty(),
            "blanking the field should remove the link"
        );
    }
}
