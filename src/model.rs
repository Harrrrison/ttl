//! Core domain types for `ttm`.
//!
//! Everything here is pure data plus validation: no I/O, no terminal, no clock
//! reads beyond the `now` arguments passed in. That keeps the whole domain layer
//! trivially unit-testable and safe for an agent to reason about, because the
//! serde representation is the contract.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Bumped whenever the on-disk layout changes in a non-backwards-compatible way.
pub const SCHEMA_VERSION: u32 = 1;

/// Minimum number of leading *hex digits* needed before a [`TaskId`]
/// abbreviation is treated as an id rather than a title.
///
/// Ids are UUIDv7, so the first 12 hex digits are the millisecond timestamp:
/// any shorter prefix carries no information beyond "created recently" and
/// matches every task in the same millisecond. Note this counts hex digits,
/// not characters, because the canonical form inserts dashes
/// (`0192f3a1-b2c3-…`) and those do not narrow the search.
pub const MIN_ID_PREFIX: usize = 12;

/// Sortable, collision-resistant task identifier (UUIDv7: lexicographic == chronological).
pub type TaskId = Uuid;

pub fn new_task_id() -> TaskId {
    Uuid::now_v7()
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Lifecycle state of a task.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Todo,
    Doing,
    Blocked,
    Done,
    Cancelled,
}

impl Status {
    /// Every status, in display order.
    pub const ALL: [Status; 5] = [
        Status::Todo,
        Status::Doing,
        Status::Blocked,
        Status::Done,
        Status::Cancelled,
    ];

    /// The next status in the `Space`-key cycle.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Status::Todo => Status::Doing,
            Status::Doing => Status::Blocked,
            Status::Blocked => Status::Done,
            Status::Done => Status::Todo,
            Status::Cancelled => Status::Todo,
        }
    }

    /// Terminal statuses never appear as "open work".
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Done | Status::Cancelled)
    }

    #[must_use]
    pub fn is_open(self) -> bool {
        !self.is_terminal()
    }

    /// Single-character glyph used by the list and timeline views.
    ///
    /// Every glyph is visible: an empty marker would make the status column
    /// unreadable for the most common state.
    #[must_use]
    pub fn glyph(self) -> char {
        match self {
            Status::Todo => '·',
            Status::Doing => '>',
            Status::Blocked => '!',
            Status::Done => '✓',
            Status::Cancelled => '-',
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Todo => "todo",
            Status::Doing => "doing",
            Status::Blocked => "blocked",
            Status::Done => "done",
            Status::Cancelled => "cancelled",
        }
    }

    /// Case-insensitive parse for CLI and keymap input.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "todo" | "open" | "pending" => Some(Status::Todo),
            "doing" | "in_progress" | "wip" | "started" => Some(Status::Doing),
            "blocked" => Some(Status::Blocked),
            "done" | "complete" | "completed" => Some(Status::Done),
            "cancelled" | "canceled" | "dropped" => Some(Status::Cancelled),
            _ => None,
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Priority
// ---------------------------------------------------------------------------

/// Relative urgency. `Ord` follows urgency, so `task_a > task_b` means "more urgent".
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Urgent,
}

impl Priority {
    pub const ALL: [Priority; 4] = [
        Priority::Low,
        Priority::Normal,
        Priority::High,
        Priority::Urgent,
    ];

    /// Numeric form for compact agent output (0..=3).
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            Priority::Low => 0,
            Priority::Normal => 1,
            Priority::High => 2,
            Priority::Urgent => 3,
        }
    }

    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Priority::Low),
            1 => Some(Priority::Normal),
            2 => Some(Priority::High),
            3 => Some(Priority::Urgent),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" | "p3" => Some(Priority::Low),
            "normal" | "med" | "medium" | "p2" => Some(Priority::Normal),
            "high" | "p1" => Some(Priority::High),
            "urgent" | "critical" | "p0" => Some(Priority::Urgent),
            _ => None,
        }
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Url,
    Pdf,
    /// Any other local file. Reserved for future use.
    File,
}

impl AttachmentKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AttachmentKind::Url => "url",
            AttachmentKind::Pdf => "pdf",
            AttachmentKind::File => "file",
        }
    }
}

impl std::fmt::Display for AttachmentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Supporting material: a link or a local PDF.
///
/// `target` is the URL for `Url` and the path for `Pdf`/`File`. `label` is the
/// human-facing name shown in the detail pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub kind: AttachmentKind,
    #[serde(default)]
    pub label: String,
    pub target: String,
    #[serde(default = "Utc::now")]
    pub added_at: DateTime<Utc>,
}

impl Attachment {
    #[must_use]
    pub fn url(target: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind: AttachmentKind::Url,
            label: label.into(),
            target: target.into(),
            added_at: Utc::now(),
        }
    }

    #[must_use]
    pub fn pdf(target: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind: AttachmentKind::Pdf,
            label: label.into(),
            target: target.into(),
            added_at: Utc::now(),
        }
    }

    /// What to display in a one-line list: label if set, else a trimmed target.
    #[must_use]
    pub fn display(&self) -> String {
        if self.label.is_empty() {
            shorten_target(&self.target, 60)
        } else {
            self.label.clone()
        }
    }

    /// Structural checks. Filesystem existence is deliberately *not* checked here;
    /// that is the caller's job since this type stays I/O-free.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.target.trim().is_empty() {
            return Err(ValidationError::EmptyAttachmentTarget);
        }
        match self.kind {
            AttachmentKind::Url => {
                if !has_scheme(&self.target) {
                    return Err(ValidationError::MalformedUrl(self.target.clone()));
                }
            }
            AttachmentKind::Pdf | AttachmentKind::File => {
                if !has_extension(&self.target, "pdf") && self.kind == AttachmentKind::Pdf {
                    return Err(ValidationError::NotAPdf(self.target.clone()));
                }
            }
        }
        Ok(())
    }
}

/// True if the string looks like `scheme://…` or a bare `mailto:`.
fn has_scheme(s: &str) -> bool {
    match s.split_once("://") {
        Some((scheme, rest)) => {
            !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
                && !rest.is_empty()
        }
        None => s.trim().starts_with("mailto:"),
    }
}

fn has_extension(s: &str, ext: &str) -> bool {
    std::path::Path::new(s)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

fn shorten_target(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(3)).collect();
    format!("{head}...")
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

/// A single unit of work.
///
/// Timestamps are full ISO-8601 instants in UTC. Date-only input from a human
/// (`--due 2026-11-01`) is resolved to 17:00 local by [`parse_datetime`], so the
/// stored value is always unambiguous.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,

    /// Optional parent task. Nesting is capped at one level (see [`validate_graph`]).
    #[serde(default)]
    pub parent_id: Option<TaskId>,

    pub title: String,

    #[serde(default)]
    pub description: String,

    #[serde(default)]
    pub status: Status,

    #[serde(default)]
    pub priority: Priority,

    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,

    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,

    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,

    #[serde(default)]
    pub due_at: Option<DateTime<Utc>>,

    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,

    /// Effort estimate in minutes. Integer minutes keep the JSON trivially
    /// machine-readable for an agent.
    #[serde(default)]
    pub estimate_minutes: Option<u32>,

    #[serde(default)]
    pub tags: Vec<String>,

    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

impl Task {
    /// Create a task with sensible defaults. `now` is injected so callers (and
    /// tests) control the clock.
    #[must_use]
    pub fn new(title: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            id: new_task_id(),
            parent_id: None,
            title: title.into(),
            description: String::new(),
            status: Status::Todo,
            priority: Priority::Normal,
            created_at: now,
            updated_at: now,
            started_at: None,
            due_at: None,
            completed_at: None,
            estimate_minutes: None,
            tags: Vec::new(),
            attachments: Vec::new(),
        }
    }

    /// Builder-style due date.
    #[must_use]
    pub fn due(mut self, due_at: DateTime<Utc>) -> Self {
        self.due_at = Some(due_at);
        self
    }

    /// Builder-style estimate, from a [`Duration`].
    #[must_use]
    pub fn estimate(mut self, d: Duration) -> Self {
        self.estimate_minutes = Some(u32::try_from(d.num_minutes().max(0)).unwrap_or(u32::MAX));
        self
    }

    /// Builder-style attachment.
    #[must_use]
    pub fn with_attachment(mut self, a: Attachment) -> Self {
        self.attachments.push(a);
        self
    }

    /// Builder-style tag.
    #[must_use]
    pub fn with_tag(mut self, t: impl Into<String>) -> Self {
        self.tags.push(t.into());
        self
    }

    #[must_use]
    pub fn estimate_duration(&self) -> Option<Duration> {
        self.estimate_minutes
            .map(|m| Duration::minutes(i64::from(m)))
    }

    /// Human-friendly estimate: `90m`, `4h`, `2d 3h`, `1w`.
    #[must_use]
    pub fn estimate_human(&self) -> Option<String> {
        self.estimate_duration().map(format_duration)
    }

    #[must_use]
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    }

    /// Update `updated_at`, stamping `started_at`/`completed_at` to match the new status.
    pub fn set_status(&mut self, status: Status, now: DateTime<Utc>) {
        self.status = status;
        match status {
            Status::Todo | Status::Blocked => self.completed_at = None,
            Status::Doing => {
                self.completed_at = None;
                self.started_at.get_or_insert(now);
            }
            Status::Done => {
                self.started_at.get_or_insert(now);
                self.completed_at = Some(now);
            }
            Status::Cancelled => self.completed_at = Some(now),
        }
        self.updated_at = now;
    }

    /// Per-task structural validation. Cross-task rules live in [`validate_graph`].
    pub fn validate(&self) -> Result<(), ValidationError> {
        let title_len = self.title.chars().count();
        if self.title.trim().is_empty() {
            return Err(ValidationError::EmptyTitle);
        }
        if title_len > 500 {
            return Err(ValidationError::TitleTooLong(title_len));
        }
        let desc_len = self.description.chars().count();
        if desc_len > 100_000 {
            return Err(ValidationError::DescriptionTooLong(desc_len));
        }
        // Note: a `due_at` earlier than `created_at` is deliberately allowed.
        // Back-filling an already-overdue task ("we missed this deadline") is a
        // normal thing to record, and rejecting it would make overdue work
        // unrepresentable.
        if let Some(done) = self.completed_at
            && done < self.created_at
        {
            return Err(ValidationError::CompletedBeforeCreated {
                completed: done,
                created: self.created_at,
            });
        }
        if self.status != Status::Done && self.completed_at.is_some() {
            return Err(ValidationError::CompletedWithoutDone);
        }
        for a in &self.attachments {
            a.validate()?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("title must not be empty")]
    EmptyTitle,
    #[error("title is limited to 500 characters (got {0})")]
    TitleTooLong(usize),
    #[error("description is limited to 100000 characters (got {0})")]
    DescriptionTooLong(usize),
    #[error("completion timestamp {completed} precedes creation date {created}")]
    CompletedBeforeCreated {
        completed: DateTime<Utc>,
        created: DateTime<Utc>,
    },
    #[error("completed_at is set but status is not `done`")]
    CompletedWithoutDone,
    #[error("attachment target must not be empty")]
    EmptyAttachmentTarget,
    #[error("not a valid URL (expected `scheme://…`): {0:?}")]
    MalformedUrl(String),
    #[error("attachment does not have a .pdf extension: {0:?}")]
    NotAPdf(String),
    #[error("parent task {0} does not exist")]
    ParentNotFound(TaskId),
    #[error("task cannot be its own parent")]
    SelfParent,
    #[error("task {child} is already a subtask; nesting is limited to one level (parent {parent})")]
    NestingTooDeep { child: TaskId, parent: TaskId },
    #[error("duplicate task id in store: {0}")]
    DuplicateId(TaskId),
    #[error("a task titled {0:?} already exists")]
    DuplicateTitle(String),
}

// ---------------------------------------------------------------------------
// Store document
// ---------------------------------------------------------------------------

/// The full on-disk document: `tasks.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSet {
    /// Unknown/absent versions are treated as the current one.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    #[serde(default)]
    pub tasks: Vec<Task>,

    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

impl Default for TaskSet {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            tasks: Vec::new(),
            updated_at: Utc::now(),
        }
    }
}

impl TaskSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn get(&self, id: &TaskId) -> Option<&Task> {
        self.tasks.iter().find(|t| &t.id == id)
    }

    pub fn get_mut(&mut self, id: &TaskId) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| &t.id == id)
    }

    /// Accepts a full UUID or a unique id prefix, which is what a human types.
    ///
    /// Short prefixes are rejected outright. Ids are UUIDv7, whose leading
    /// characters encode the creation timestamp, so tasks created together
    /// share a prefix and a 4-character guess is ambiguous almost always.
    /// Titles are matched separately, so anything shorter than
    /// [`MIN_ID_PREFIX`] is treated as a title.
    pub fn resolve(&self, ref_id: &str) -> Result<TaskId, ResolveError> {
        let trimmed = ref_id.trim();
        if trimmed.is_empty() {
            return Err(ResolveError::Empty);
        }

        let lower = trimmed.to_ascii_lowercase();
        let looks_like_uuid = lower.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        let hex_digits = lower.chars().filter(char::is_ascii_hexdigit).count();

        if looks_like_uuid && hex_digits >= MIN_ID_PREFIX {
            if let Ok(id) = Uuid::parse_str(&lower)
                && self.get(&id).is_some()
            {
                return Ok(id);
            }

            let mut hits: Vec<TaskId> = self
                .tasks
                .iter()
                .filter(|t| t.id.to_string().starts_with(&lower))
                .map(|t| t.id)
                .collect();

            match hits.len() {
                0 => return Err(ResolveError::NotFound(trimmed.to_string())),
                1 => return Ok(hits.remove(0)),
                _ => {
                    return Err(ResolveError::Ambiguous {
                        query: trimmed.to_string(),
                        candidates: hits,
                    });
                }
            }
        }

        // Otherwise treat the reference as a title.
        self.find_exact(&lower)
            .map(|t| t.id)
            .ok_or_else(|| ResolveError::NotFound(trimmed.to_string()))
    }

    /// Fallback for convenience refs: exact title match, case-insensitive.
    #[must_use]
    pub fn find_exact(&self, title_lower: &str) -> Option<&Task> {
        self.tasks
            .iter()
            .find(|t| t.title.trim().to_ascii_lowercase() == title_lower)
    }

    pub fn insert(&mut self, task: Task) -> Result<(), ValidationError> {
        task.validate()?;
        if self.get(&task.id).is_some() {
            return Err(ValidationError::DuplicateId(task.id));
        }
        self.tasks.push(task);
        Ok(())
    }

    /// Remove a task and cascade its subtasks. Returns the removed ids.
    pub fn remove_cascade(&mut self, id: &TaskId) -> Vec<TaskId> {
        let mut doomed: HashSet<TaskId> = HashSet::from([*id]);
        // One level of nesting means a single extra pass suffices, but loop to
        // be safe if a hand-edited file contains deeper nesting.
        loop {
            let before = doomed.len();
            for t in &self.tasks {
                if t.parent_id.is_some_and(|p| doomed.contains(&p)) {
                    doomed.insert(t.id);
                }
            }
            if doomed.len() == before {
                break;
            }
        }
        self.tasks.retain(|t| !doomed.contains(&t.id));
        let mut out: Vec<TaskId> = doomed.into_iter().collect();
        out.sort_unstable();
        out
    }

    /// Immediate children of `id`.
    #[must_use]
    pub fn children(&self, id: &TaskId) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.parent_id.as_ref() == Some(id))
            .collect()
    }

    #[must_use]
    pub fn roots(&self) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.parent_id.is_none())
            .collect()
    }

    /// Group children under their parents, preserving input order for roots.
    #[must_use]
    pub fn tree(&self) -> Vec<(&Task, Vec<&Task>)> {
        self.roots()
            .into_iter()
            .map(|r| (r, self.children(&r.id)))
            .collect()
    }

    /// Total open and done child counts, for parent rollups in the timeline.
    #[must_use]
    pub fn rollup(&self, id: &TaskId) -> Rollup {
        let kids = self.children(id);
        Rollup {
            total: kids.len(),
            done: kids.iter().filter(|k| k.status == Status::Done).count(),
        }
    }

    /// Aggregated estimate across a task and its children.
    #[must_use]
    pub fn rolled_estimate_minutes(&self, id: &TaskId) -> u32 {
        let own = self.get(id).and_then(|t| t.estimate_minutes).unwrap_or(0);
        let kids: u32 = self
            .children(id)
            .iter()
            .filter_map(|k| k.estimate_minutes)
            .sum();
        own.saturating_add(kids)
    }

    /// Oldest open due date among a task and its children — the real deadline.
    #[must_use]
    pub fn rolled_due_at(&self, id: &TaskId) -> Option<DateTime<Utc>> {
        self.get(id)
            .and_then(|t| t.due_at)
            .into_iter()
            .chain(self.children(id).into_iter().filter_map(|k| k.due_at))
            .min()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rollup {
    pub total: usize,
    pub done: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("no task reference given")]
    Empty,
    #[error("no task matches {0:?}")]
    NotFound(String),
    #[error("{query:?} is ambiguous: matches {}", .candidates.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))]
    Ambiguous {
        query: String,
        candidates: Vec<TaskId>,
    },
}

/// Validate every task and all cross-task invariants. Returns all problems found
/// rather than stopping at the first, which is more useful for a bulk import.
pub fn validate_graph(set: &TaskSet) -> Vec<ValidationError> {
    let mut errs: Vec<ValidationError> = Vec::new();

    let mut seen: HashSet<TaskId> = HashSet::new();
    for t in &set.tasks {
        if !seen.insert(t.id) {
            errs.push(ValidationError::DuplicateId(t.id));
        }
        if let Err(e) = t.validate() {
            errs.push(e);
        }
    }

    let by_id: HashMap<TaskId, &Task> = set.tasks.iter().map(|t| (t.id, t)).collect();
    for t in &set.tasks {
        let Some(parent_id) = t.parent_id else {
            continue;
        };

        if parent_id == t.id {
            errs.push(ValidationError::SelfParent);
            continue;
        }
        let Some(parent) = by_id.get(&parent_id) else {
            errs.push(ValidationError::ParentNotFound(parent_id));
            continue;
        };
        if parent.parent_id.is_some() {
            errs.push(ValidationError::NestingTooDeep {
                child: t.id,
                parent: parent_id,
            });
        }
    }

    errs
}

// ---------------------------------------------------------------------------
// Human input parsing (CLI `--due` / `--est`)
// ---------------------------------------------------------------------------

/// Default local hour for a date-only deadline: end of the working day.
const EOD_HOUR: u32 = 17;

/// Parse a due-date expression into a UTC instant.
///
/// Accepts:
/// * `2026-11-01` / `2026-11-01T14:30` / full RFC-3339
/// * `today`, `tomorrow`, `yesterday`
/// * `+3d`, `+2h`, `+90m` relative to `now`, and the same forms negated
///   (`-1d`) so an already-missed deadline can be recorded
/// * `eod`, `eow` (end of this week, i.e. coming Sunday 17:00)
///
/// A bare date resolves to 17:00 in the machine's local timezone, then to UTC, so
/// the value we persist is always a full instant.
#[must_use]
pub fn parse_datetime(input: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();

    match lower.as_str() {
        "today" | "eod" => return end_of_day(now),
        "tomorrow" | "eot" => return end_of_day(now + Duration::days(1)),
        "yesterday" => return end_of_day(now - Duration::days(1)),
        "eow" | "weekend" => {
            let days_until_sunday = 6i64 - now.weekday().num_days_from_monday() as i64;
            return end_of_day(now + Duration::days(days_until_sunday.max(0)));
        }
        _ => {}
    }

    if let Some(rest) = lower.strip_prefix('+') {
        return parse_relative(rest, now);
    }
    if let Some(rest) = lower.strip_prefix('-') {
        // `-1d` = one day ago, the natural way to record already-missed work.
        return parse_relative(rest, now).map(|d| now - (d - now));
    }

    // Bare date.
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return local_end_of_day(d);
    }
    // Local date + time, no offset.
    for fmt in ["%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            return local_to_utc(ndt);
        }
    }
    // Full RFC-3339 / with offset.
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // Date + time + numeric offset.
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%z") {
        return local_to_utc(ndt);
    }

    None
}

fn parse_relative(spec: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let (digits, unit) = spec.split_at(
        spec.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(spec.len()),
    );
    if digits.is_empty() {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    Some(match unit.trim() {
        "m" | "min" | "mins" | "minute" | "minutes" => now + Duration::minutes(n),
        "h" | "hr" | "hrs" | "hour" | "hours" => now + Duration::hours(n),
        "d" | "day" | "days" => now + Duration::days(n),
        "w" | "wk" | "wks" | "week" | "weeks" => now + Duration::weeks(n),
        _ => return None,
    })
}

fn end_of_day(now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    local_end_of_day(now.with_timezone(&chrono::Local).date_naive())
}

/// Resolve a local calendar date's end-of-day to UTC.
fn local_end_of_day(d: NaiveDate) -> Option<DateTime<Utc>> {
    let ndt = d.and_hms_opt(EOD_HOUR, 0, 0)?;
    local_to_utc(ndt)
}

fn local_to_utc(ndt: NaiveDateTime) -> Option<DateTime<Utc>> {
    // `Local` has no `from_local_datetime` that can't be ambiguous around DST
    // transitions; take the earliest valid interpretation, which is the
    // conventional choice for a deadline.
    chrono::Local
        .from_local_datetime(&ndt)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| Some(Utc.from_utc_datetime(&ndt)))
}

/// Parse an effort estimate into minutes.
///
/// Accepts `90m`, `4h`, `2d`, `1w`, `1.5h`, and combinations like `1d 4h`,
/// `1d4h`, or `4 h`. A bare number is minutes. Zero, negatives, and anything
/// with an unrecognised unit return `None`.
#[must_use]
pub fn parse_estimate_minutes(input: &str) -> Option<u32> {
    // Whitespace is never significant, so drop it and parse the compact form.
    let s: String = input
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();

    if s.is_empty() {
        return None;
    }

    let factor_for = |unit: &str| -> Option<f64> {
        Some(match unit {
            "" | "m" | "min" | "mins" | "minute" | "minutes" => 1.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 60.0,
            "d" | "day" | "days" => 60.0 * 24.0,
            "w" | "wk" | "wks" | "week" | "weeks" => 60.0 * 24.0 * 7.0,
            _ => return None,
        })
    };

    let mut total = 0.0f64;
    let mut chars = s.chars().peekable();

    while chars.peek().is_some() {
        let mut number = String::new();
        while chars
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || *c == '.')
        {
            number.push(chars.next().expect("peeked above"));
        }
        if number.is_empty() {
            return None;
        }

        let mut unit = String::new();
        while chars
            .peek()
            .is_some_and(|c| !c.is_ascii_digit() && *c != '.')
        {
            unit.push(chars.next().expect("peeked above"));
        }

        let n: f64 = number.parse().ok()?;
        if !n.is_finite() || n < 0.0 {
            return None;
        }
        total += n * factor_for(&unit)?;
    }

    let total = total.round();
    if total <= 0.0 {
        return None;
    }
    u32::try_from(total as i64).ok()
}

/// Render minutes as `90m` / `4h` / `2d 3h` / `1w`.
#[must_use]
pub fn format_duration(d: Duration) -> String {
    let mins = d.num_minutes().max(0);
    if mins == 0 {
        return "0m".to_string();
    }
    let weeks = mins / (60 * 24 * 7);
    let days = (mins % (60 * 24 * 7)) / (60 * 24);
    let hours = (mins % (60 * 24)) / 60;
    let rem = mins % 60;

    let mut parts: Vec<String> = Vec::new();
    if weeks > 0 {
        parts.push(format!("{weeks}w"));
    }
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if rem > 0 {
        parts.push(format!("{rem}m"));
    }
    parts.join(" ")
}

/// Compact relative countdown: `overdue 5d`, `in 3d`, `today`, `now`.
#[must_use]
pub fn countdown(due: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = due - now;

    if delta < Duration::zero() {
        // Round towards zero so "overdue 1d" covers up to a day and a half.
        let overdue = (-delta.num_minutes() as f64 / (60.0 * 24.0)).round() as i64;
        return match overdue {
            0 => "due now".to_string(),
            1 => "overdue 1d".to_string(),
            n => format!("overdue {n}d"),
        };
    }

    // Round rather than truncate: a deadline 71h59m away is "in 3d", not
    // "in 2d", because the clock advanced between when it was set and now.
    let days = (delta.num_minutes() as f64 / (60.0 * 24.0)).round() as i64;
    match days {
        0 => {
            let minutes = delta.num_minutes();
            match minutes {
                0 => "due now".to_string(),
                1 => "in 1m".to_string(),
                n if n < 60 => format!("in {n}m"),
                n => format!("in {}h", n / 60),
            }
        }
        1 => "in 1d".to_string(),
        _ => format!("in {days}d"),
    }
}

/// Grouping key used by the list view's sort and filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    #[default]
    Manual,
    Due,
    Priority,
    Status,
    Created,
    Title,
    Estimate,
}

impl SortKey {
    pub const ALL: [SortKey; 7] = [
        SortKey::Manual,
        SortKey::Due,
        SortKey::Priority,
        SortKey::Status,
        SortKey::Created,
        SortKey::Title,
        SortKey::Estimate,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SortKey::Manual => "manual",
            SortKey::Due => "due",
            SortKey::Priority => "priority",
            SortKey::Status => "status",
            SortKey::Created => "created",
            SortKey::Title => "title",
            SortKey::Estimate => "estimate",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s.trim().to_ascii_lowercase())
    }

    /// Sort a task list. Tasks with no due date sort last when sorting by due.
    pub fn apply(self, tasks: &mut [Task]) {
        match self {
            SortKey::Manual => {}
            // Reverse the None ordering so undated tasks sort last.
            SortKey::Due => tasks.sort_by_key(|t| (t.due_at.is_none(), t.due_at)),
            // Descending urgency: highest priority first.
            SortKey::Priority => tasks.sort_by_key(|t| Reverse(t.priority)),
            SortKey::Status => tasks.sort_by_key(|t| t.status),
            SortKey::Created => tasks.sort_by_key(|a| a.created_at),
            SortKey::Title => tasks.sort_by_key(|a| a.title.to_lowercase()),
            SortKey::Estimate => tasks.sort_by_key(|t| t.estimate_minutes),
        }
    }
}

/// A filter over the task set, shared by the TUI and the CLI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    #[serde(default)]
    pub statuses: Vec<Status>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub due: DueWindow,
    #[serde(default)]
    pub parent: Option<TaskId>,
    #[serde(default)]
    pub roots_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DueWindow {
    #[default]
    Any,
    Overdue,
    Today,
    Week,
    NoDueDate,
}

impl Filter {
    #[must_use]
    pub fn matches(&self, t: &Task, now: DateTime<Utc>) -> bool {
        if !self.statuses.is_empty() && !self.statuses.contains(&t.status) {
            return false;
        }
        if self.roots_only && t.parent_id.is_some() {
            return false;
        }
        if let Some(p) = self.parent
            && t.parent_id != Some(p)
        {
            return false;
        }
        if !self.tags.is_empty() && !self.tags.iter().all(|tag| t.has_tag(tag)) {
            return false;
        }
        if let Some(needle) = &self.text {
            let n = needle.to_ascii_lowercase();
            let hit = t.title.to_ascii_lowercase().contains(&n)
                || t.description.to_ascii_lowercase().contains(&n)
                || t.tags
                    .iter()
                    .any(|tag| tag.to_ascii_lowercase().contains(&n))
                || t.attachments
                    .iter()
                    .any(|a| a.target.to_ascii_lowercase().contains(&n));
            if !hit {
                return false;
            }
        }
        self.due.matches(t, now)
    }
}

impl DueWindow {
    #[must_use]
    pub fn matches(self, t: &Task, now: DateTime<Utc>) -> bool {
        match self {
            DueWindow::Any => true,
            DueWindow::NoDueDate => t.due_at.is_none(),
            DueWindow::Overdue => t.due_at.is_some_and(|d| d < now && t.status.is_open()),
            DueWindow::Today => t.due_at.is_some_and(|d| {
                d >= start_of_local_day(now) && d < start_of_local_day(now) + Duration::days(1)
            }),
            DueWindow::Week => t
                .due_at
                .is_some_and(|d| d >= now && d <= now + Duration::days(7)),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DueWindow::Any => "any",
            DueWindow::Overdue => "overdue",
            DueWindow::Today => "today",
            DueWindow::Week => "week",
            DueWindow::NoDueDate => "nodue",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "any" | "all" => Some(DueWindow::Any),
            "overdue" => Some(DueWindow::Overdue),
            "today" => Some(DueWindow::Today),
            "week" | "next7" => Some(DueWindow::Week),
            "nodue" | "no-due" | "none" => Some(DueWindow::NoDueDate),
            _ => None,
        }
    }
}

fn start_of_local_day(now: DateTime<Utc>) -> DateTime<Utc> {
    let local = now.with_timezone(&chrono::Local);
    local
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|ndt| {
            chrono::Local
                .from_local_datetime(&ndt)
                .earliest()
                .map(|dt| dt.with_timezone(&Utc))
        })
        .unwrap_or_else(|| {
            now.date_naive()
                .and_hms_opt(0, 0, 0)
                .map_or(now, |n| Utc.from_utc_datetime(&n))
        })
}

/// Reorder tasks into a stable, human-friendly default: open before closed,
/// then by due date, then by urgency. Used whenever `manual` order is not
/// explicitly tracked by the UI.
pub fn default_order(tasks: &mut [Task]) {
    tasks.sort_by(|a, b| {
        a.status
            .is_terminal()
            .cmp(&b.status.is_terminal())
            .then(a.due_at.cmp(&b.due_at))
            .then(b.priority.cmp(&a.priority))
            .then(a.created_at.cmp(&b.created_at))
    });
}

/// Summary counts for the status bar and for `--summary` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub total: usize,
    pub todo: usize,
    pub doing: usize,
    pub blocked: usize,
    pub done: usize,
    pub cancelled: usize,
    pub overdue: usize,
    pub due_today: usize,
    pub total_estimate_minutes: u32,
}

/// Compute [`Summary`] over a task list.
#[must_use]
pub fn summarize(tasks: &[Task], now: DateTime<Utc>) -> Summary {
    let mut s = Summary {
        total: tasks.len(),
        todo: 0,
        doing: 0,
        blocked: 0,
        done: 0,
        cancelled: 0,
        overdue: 0,
        due_today: 0,
        total_estimate_minutes: 0,
    };
    let day_end = start_of_local_day(now) + Duration::days(1);

    for t in tasks {
        match t.status {
            Status::Todo => s.todo += 1,
            Status::Doing => s.doing += 1,
            Status::Blocked => s.blocked += 1,
            Status::Done => s.done += 1,
            Status::Cancelled => s.cancelled += 1,
        }
        s.total_estimate_minutes = s
            .total_estimate_minutes
            .saturating_add(t.estimate_minutes.unwrap_or(0));
        if t.status.is_open()
            && let Some(due) = t.due_at
        {
            if due < now {
                s.overdue += 1;
            }
            if due >= now && due < day_end {
                s.due_today += 1;
            }
        }
    }
    s
}

/// Compact attachment counts by kind, for list rows and agent summaries.
#[must_use]
pub fn attachment_counts(t: &Task) -> BTreeMap<&'static str, usize> {
    let mut m = BTreeMap::new();
    for a in &t.attachments {
        *m.entry(a.kind.as_str()).or_insert(0) += 1;
    }
    m
}
