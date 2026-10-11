//! The `ttm` command-line interface: the surface an AI agent uses to read and
//! write the task store.
//!
//! Design rules, in priority order:
//!
//! 1. **Machine-readable by default where it matters.** Every query command
//!    accepts `--json`, and mutations echo the affected task as JSON so an agent
//!    can capture the new id without re-reading.
//! 2. **Never corrupt.** All writes go through [`Store::mutate`], which holds an
//!    exclusive lock and re-reads the store before applying a change, so a TUI
//!    session and a concurrent `ttm add` cannot clobber each other.
//! 3. **Idempotence is the caller's job, but reported honestly.** Mutating
//!    commands support `--dry-run`, which validates and prints the resulting
//!    task without touching disk.
//!
//! Human-facing prose output is deliberately terse; `--summary` exists for when
//! an agent wants context without full JSON.

use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use clap::{Args, Parser, Subcommand};
use serde_json::json;

use crate::model::{
    self, Attachment, DueWindow, Filter, Priority, SortKey, Status, Task, TaskId, TaskSet,
    ValidationError,
};
use crate::store::{Event, EventKind, Store, StoreError};

// ---------------------------------------------------------------------------
// Argument definitions
// ---------------------------------------------------------------------------

/// Task manager for the terminal, with a gantt timeline and an agent-readable
/// JSON store.
///
/// Run with no arguments to open the interactive TUI.
#[derive(Debug, Parser)]
#[command(
    name = "ttm",
    version,
    about,
    long_about = None,
    propagate_version = true
)]
pub struct Cli {
    /// Task store directory. Defaults to $TTM_HOME, then ~/.local/share/ttm.
    #[arg(long, short = 'H', global = true, env = "TTM_HOME", value_name = "DIR")]
    pub home: Option<PathBuf>,

    /// Emit JSON instead of human-readable text.
    #[arg(long, short, global = true, conflicts_with = "text")]
    pub json: bool,

    /// Force plain-text output even when stdout is not a terminal.
    #[arg(long, global = true, conflicts_with = "json")]
    pub text: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List tasks, filtered and sorted.
    #[command(visible_alias = "ls")]
    List(ListArgs),

    /// Show one task in full, including its event history.
    Show(ShowArgs),

    /// Aggregate counts and totals across the store.
    Summary(SummaryArgs),

    /// Tasks laid out against the calendar, as the TUI's timeline renders them.
    Timeline(TimelineArgs),

    /// Create a task.
    Add(AddArgs),

    /// Change fields on an existing task.
    #[command(visible_alias = "set")]
    Edit(EditArgs),

    /// Mark a task done.
    Done(RefArgs),

    /// Mark a task as in progress.
    Start(RefArgs),

    /// Delete a task (and its subtasks).
    #[command(visible_alias = "rm")]
    Remove(RemoveArgs),

    /// Attach a link or a PDF to a task.
    Link(LinkArgs),

    /// Undo the most recent change to a task by restoring its previous field values.
    Undo(UndoArgs),

    /// Print where the store lives and what is in it.
    Info(InfoArgs),
}

/// Filters and sorting shared by the read commands.
#[derive(Debug, Args, Clone)]
pub struct QueryArgs {
    /// Only tasks in this status. Repeatable.
    #[arg(
        long = "status",
        short = 's',
        value_name = "STATUS",
        allow_hyphen_values = true
    )]
    pub statuses: Vec<String>,

    /// Only tasks due in this window: any, overdue, today, week, nodue.
    #[arg(
        long = "due",
        short = 'd',
        value_name = "WINDOW",
        allow_hyphen_values = true
    )]
    pub due: Option<String>,

    /// Only tasks carrying this tag. Repeatable.
    #[arg(
        long = "tag",
        short = 't',
        value_name = "TAG",
        allow_hyphen_values = true
    )]
    pub tags: Vec<String>,

    /// Only tasks whose title, description, tag, or attachment contains this text.
    #[arg(
        long = "search",
        short = 'q',
        value_name = "TEXT",
        allow_hyphen_values = true
    )]
    pub search: Option<String>,

    /// Only top-level tasks, excluding subtasks.
    #[arg(long)]
    pub roots: bool,

    /// Only subtasks of this task (id prefix or exact title).
    #[arg(long = "parent", short = 'p', value_name = "REF")]
    pub parent: Option<String>,

    /// Sort by: manual, due, priority, status, created, title, estimate.
    #[arg(
        long = "sort",
        short = 'S',
        value_name = "KEY",
        default_value = "due",
        allow_hyphen_values = true
    )]
    pub sort: String,

    /// Reverse the sort order.
    #[arg(long = "reverse", short = 'r')]
    pub reverse: bool,

    /// Hide completed and cancelled tasks.
    #[arg(long = "open", short = 'o', conflicts_with_all = ["statuses", "all"])]
    pub open_only: bool,

    /// Include completed and cancelled tasks.
    #[arg(long, conflicts_with = "open_only")]
    pub all: bool,

    /// Show subtasks indented under their parents.
    #[arg(long, short = 'T')]
    pub tree: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub query: QueryArgs,

    /// Print only ids, one per line, for shell piping.
    #[arg(long, conflicts_with_all = ["json", "summary"])]
    pub ids: bool,

    /// Print one compact line per task: status, countdown, estimate, links.
    #[arg(long, conflicts_with_all = ["json", "ids"])]
    pub summary: bool,

    /// Maximum number of rows to print. 0 means no limit.
    #[arg(long, short = 'n', default_value_t = 0)]
    pub limit: usize,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Task id prefix or exact title.
    pub reference: String,

    /// Include the task's event-log history.
    #[arg(long)]
    pub history: bool,

    /// Include subtask rollups (counts and rolled-up estimates).
    #[arg(long)]
    pub rollup: bool,
}

#[derive(Debug, Args)]
pub struct SummaryArgs {
    #[command(flatten)]
    pub query: QueryArgs,
}

#[derive(Debug, Args)]
pub struct TimelineArgs {
    /// Days of calendar to include after today.
    #[arg(long, default_value_t = 30)]
    pub days: i64,

    /// Days of calendar to include before today.
    #[arg(long, default_value_t = 7)]
    pub before: i64,

    /// Only top-level tasks.
    #[arg(long)]
    pub roots: bool,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Task title. Required.
    #[arg(long, short = 't')]
    pub title: String,

    /// Task description. Supports newlines.
    #[arg(long, short = 'd', visible_alias = "desc")]
    pub description: Option<String>,

    /// Deadline. Accepts 2026-11-01, 2026-11-01T14:30, +3d, -1d, today, eod.
    #[arg(long, allow_hyphen_values = true)]
    pub due: Option<String>,

    /// Effort estimate: 4h, 90m, 2d, 1d 4h, or bare minutes.
    #[arg(long, short = 'e', visible_alias = "est", allow_hyphen_values = true)]
    pub estimate: Option<String>,

    /// Initial status: todo, doing, blocked. Defaults to todo.
    #[arg(long, short = 's', value_name = "STATUS", allow_hyphen_values = true)]
    pub status: Option<String>,

    /// Priority: low, normal, high, urgent.
    #[arg(long, short = 'p')]
    pub priority: Option<String>,

    /// Tag to attach. Repeatable.
    #[arg(
        long = "tag",
        short = 'T',
        value_name = "TAG",
        allow_hyphen_values = true
    )]
    pub tags: Vec<String>,

    /// Make this a subtask of the referenced task.
    #[arg(long = "parent", value_name = "REF")]
    pub parent: Option<String>,

    /// Attach a URL. Repeatable.
    #[arg(
        long = "url",
        short = 'u',
        value_name = "URL",
        allow_hyphen_values = true
    )]
    pub urls: Vec<String>,

    /// Attach a local PDF by path. Repeatable.
    #[arg(long = "pdf", value_name = "PATH")]
    pub pdfs: Vec<String>,

    /// Label for the next --url/--pdf. Pairs positionally; reuse to label several.
    #[arg(long = "label", value_name = "TEXT")]
    pub labels: Vec<String>,

    /// Validate and print the result without writing to disk.
    #[arg(long)]
    pub dry_run: bool,

    /// Emit the created task as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    /// Task id prefix or exact title.
    pub reference: String,

    #[arg(long, short = 't')]
    pub title: Option<String>,

    #[arg(long, short = 'd', visible_alias = "desc")]
    pub description: Option<String>,

    /// Set the deadline. Accepts the same forms as `add --due`.
    #[arg(long, allow_hyphen_values = true)]
    pub due: Option<String>,

    /// Remove the deadline.
    #[arg(long, conflicts_with = "due")]
    pub no_due: bool,

    /// Set the estimate. Accepts the same forms as `add --estimate`.
    #[arg(long, short = 'e', visible_alias = "est", allow_hyphen_values = true)]
    pub estimate: Option<String>,

    /// Remove the estimate.
    #[arg(long, conflicts_with = "estimate")]
    pub no_estimate: bool,

    #[arg(long, short = 's', value_name = "STATUS")]
    pub status: Option<String>,

    #[arg(long, short = 'p')]
    pub priority: Option<String>,

    /// Add a tag. Repeatable.
    #[arg(
        long = "tag",
        short = 'T',
        value_name = "TAG",
        allow_hyphen_values = true
    )]
    pub tags: Vec<String>,

    /// Remove a tag. Repeatable.
    #[arg(long = "untag", value_name = "TAG")]
    pub untags: Vec<String>,

    /// Re-parent onto the referenced task.
    #[arg(long = "parent", value_name = "REF")]
    pub parent: Option<String>,

    /// Detach from the current parent.
    #[arg(long, conflicts_with = "parent")]
    pub no_parent: bool,

    /// Apply and print the result without writing to disk.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct RefArgs {
    /// Task id prefix or exact title.
    pub reference: String,

    /// Print what would change without writing to disk.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Task id prefix or exact title.
    pub reference: String,

    /// Delete subtasks too. This is the default; the flag exists for scripts.
    #[arg(long)]
    pub cascade: bool,

    /// Print what would be deleted without writing to disk.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct LinkArgs {
    /// Task id prefix or exact title.
    pub reference: String,

    /// URL to attach.
    #[arg(
        long,
        short = 'u',
        required_unless_present = "pdf",
        conflicts_with = "pdf"
    )]
    pub url: Option<String>,

    /// Path to a PDF to attach.
    #[arg(long, required_unless_present = "url")]
    pub pdf: Option<String>,

    /// Display name for the attachment.
    #[arg(long, short = 'l', allow_hyphen_values = true)]
    pub label: Option<String>,

    /// Print what would change without writing to disk.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct UndoArgs {
    /// Task id prefix or exact title. Omit to undo the most recent change.
    pub reference: Option<String>,

    /// Number of events to walk back. Defaults to 1.
    #[arg(long, short = 'n', default_value_t = 1)]
    pub steps: usize,

    /// Print what would change without writing to disk.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Print store paths as JSON.
    #[arg(long)]
    pub paths: bool,
}

// ---------------------------------------------------------------------------
// Output modes
// ---------------------------------------------------------------------------

/// The two global output flags, lifted out of [`Cli`] before the subcommand is
/// moved out of it.
#[derive(Debug, Clone, Copy, Default)]
pub struct GlobalFlags {
    pub json: bool,
    pub text: bool,
}

/// How a command should render its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Pretty JSON.
    Json,
    /// Aligned plain text (default when stdout is a TTY).
    Text,
    /// Machine-stable one-line-per-item format, for agents and pipes.
    Summary,
    /// Bare ids, one per line.
    Ids,
}

/// Tri-state for the `--due` / `--no-due` pair, which clap cannot express as a
/// single flag pair without leaving the caller to infer intent.
#[derive(Debug, Clone, Copy)]
enum DueChange {
    Keep,
    Set(DateTime<Utc>),
    Clear,
}

impl Output {
    /// Pick a mode: an explicit per-command choice wins, then the global flags,
    /// then a non-TTY default of `Summary` so piping never gets box drawing.
    fn resolve(flags: GlobalFlags, explicit: Option<Output>) -> Self {
        if let Some(o) = explicit {
            return o;
        }
        if flags.json {
            return Output::Json;
        }
        if flags.text || !is_terminal() {
            return Output::Summary;
        }
        Output::Text
    }
}

fn is_terminal() -> bool {
    // Without a dependency on `is-terminal`, std has no stable check. Use the
    // conventional TERM-based heuristic plus an explicit opt-out.
    std::env::var_os("TTM_PLAIN").is_none() && std::env::var_os("NO_COLOR").is_none()
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Exit code returned to the shell. Distinct codes let an agent branch without
/// parsing stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Exit {
    Ok = 0,
    /// Bad input: unparseable filter, unknown status, malformed date.
    Usage = 2,
    /// The referenced task does not exist.
    NotFound = 3,
    /// Store-level problem: locked, unparsable, validation failed.
    Store = 4,
}

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub code: Exit,
}

impl Failure {
    fn usage(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            code: Exit::Usage,
        }
    }

    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            code: Exit::NotFound,
        }
    }

    fn store(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            code: Exit::Store,
        }
    }
}

impl From<StoreError> for Failure {
    fn from(e: StoreError) -> Self {
        let code = match &e {
            StoreError::NotFound(_) => Exit::NotFound,
            _ => Exit::Store,
        };
        Self {
            message: e.to_string(),
            code,
        }
    }
}

pub type Result<T> = std::result::Result<T, Failure>;

/// Run the CLI. Returns the process exit code.
pub fn run(cli: Cli) -> Result<()> {
    // Lift the globals before the subcommand is moved out of `cli`.
    let Cli {
        home,
        json,
        text,
        command,
    } = cli;
    let flags = GlobalFlags { json, text };

    let store =
        Store::open(home.unwrap_or_else(crate::store::default_root)).map_err(Failure::from)?;

    match command {
        None => {
            // The TUI lands in step 4. Until then exit non-zero rather than
            // pretending to have done something, so scripts notice.
            Err(Failure {
                message: "interactive TUI is not implemented yet (arriving in step 4). \
                          Use a subcommand: `ttm list --summary`, or `ttm add --help`."
                    .to_string(),
                code: Exit::Usage,
            })
        }
        Some(Command::List(a)) => cmd_list(&store, flags, a),
        Some(Command::Show(a)) => cmd_show(&store, flags, a),
        Some(Command::Summary(a)) => cmd_summary(&store, flags, a),
        Some(Command::Timeline(a)) => cmd_timeline(&store, flags, a),
        Some(Command::Add(a)) => cmd_add(&store, flags, a),
        Some(Command::Edit(a)) => cmd_edit(&store, flags, a),
        Some(Command::Done(a)) => cmd_set_status(&store, flags, a, Status::Done),
        Some(Command::Start(a)) => cmd_set_status(&store, flags, a, Status::Doing),
        Some(Command::Remove(a)) => cmd_remove(&store, flags, a),
        Some(Command::Link(a)) => cmd_link(&store, flags, a),
        Some(Command::Undo(a)) => cmd_undo(&store, flags, a),
        Some(Command::Info(a)) => cmd_info(&store, flags, a),
    }
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

/// Convert a textual status into the enum, with a helpful error.
fn parse_status(s: &str) -> Result<Status> {
    Status::parse(s).ok_or_else(|| {
        Failure::usage(format!(
            "unknown status {s:?}; expected one of: todo, doing, blocked, done, cancelled"
        ))
    })
}

fn parse_priority(s: &str) -> Result<Priority> {
    Priority::parse(s).ok_or_else(|| {
        Failure::usage(format!(
            "unknown priority {s:?}; expected one of: low, normal, high, urgent"
        ))
    })
}

fn parse_due_window(s: &str) -> Result<DueWindow> {
    DueWindow::parse(s).ok_or_else(|| {
        Failure::usage(format!(
            "unknown due window {s:?}; expected one of: any, overdue, today, week, nodue"
        ))
    })
}

fn parse_due(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    model::parse_datetime(s, now).ok_or_else(|| {
        Failure::usage(format!(
            "could not parse due date {s:?}; try 2026-11-01, 2026-11-01T14:30, +3d, today, or eod"
        ))
    })
}

fn parse_estimate(s: &str) -> Result<u32> {
    model::parse_estimate_minutes(s).ok_or_else(|| {
        Failure::usage(format!(
            "could not parse estimate {s:?}; try 4h, 90m, 2d, 1d 4h, or bare minutes"
        ))
    })
}

fn parse_sort(s: &str) -> Result<SortKey> {
    SortKey::parse(s).ok_or_else(|| {
        Failure::usage(format!(
            "unknown sort key {s:?}; expected one of: {}",
            SortKey::ALL.map(|k| k.as_str()).join(", ")
        ))
    })
}

/// Resolve a task reference, mapping resolution failures onto exit codes.
fn resolve<'a>(set: &'a TaskSet, reference: &str) -> Result<&'a Task> {
    match set.resolve(reference) {
        Ok(id) => set
            .get(&id)
            .ok_or_else(|| Failure::not_found(format!("no task matches {reference:?}"))),
        Err(model::ResolveError::Ambiguous { candidates, .. }) => Err(Failure::not_found(format!(
            "{reference:?} is ambiguous; candidates: {}",
            candidates
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
        Err(e) => Err(Failure::not_found(e.to_string())),
    }
}

/// Build a [`Filter`] from the shared query arguments.
fn build_filter(query: &QueryArgs, set: &TaskSet) -> Result<Filter> {
    let mut statuses: Vec<Status> = query
        .statuses
        .iter()
        .map(|s| parse_status(s))
        .collect::<Result<Vec<_>>>()?;

    // `--open` and `--all` are shorthands that layer on top of `--status`.
    if query.open_only {
        statuses.extend([Status::Todo, Status::Doing, Status::Blocked]);
    }

    let due = match &query.due {
        Some(d) => parse_due_window(d)?,
        None => DueWindow::Any,
    };

    let parent = match &query.parent {
        Some(p) => Some(
            set.resolve(p)
                .map_err(|e| Failure::not_found(format!("parent {p:?}: {e}")))?,
        ),
        None => None,
    };

    Ok(Filter {
        statuses,
        tags: query.tags.clone(),
        text: query.search.clone(),
        due,
        parent,
        roots_only: query.roots || parent.is_some(),
    })
}

/// Apply filters, then sort. Returns owned clones so callers can reorder freely.
fn select(set: &TaskSet, filter: &Filter, sort: SortKey, now: DateTime<Utc>) -> Vec<Task> {
    let mut out: Vec<Task> = set
        .tasks
        .iter()
        .filter(|t| filter.matches(t, now))
        .cloned()
        .collect();

    if sort == SortKey::Manual {
        model::default_order(&mut out);
    } else {
        sort.apply(&mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    let body = serde_json::to_string_pretty(value)
        .map_err(|e| Failure::store(format!("could not serialise output: {e}")))?;
    writeln!(stdout, "{body}").map_err(|e| Failure::store(format!("could not write output: {e}")))
}

/// The stable one-line-per-task format for agents.
///
/// Shape: `<status>|<priority>|<due>|<countdown>|<estimate>|<links>|<id>|<title>`
///
/// Timestamps are RFC-3339 UTC or `-`. The title is last and is sanitised so
/// that splitting the line on `|` always yields exactly eight fields.
fn summary_line(t: &Task, set: &TaskSet, now: DateTime<Utc>) -> String {
    let fmt_ts = |t: Option<DateTime<Utc>>| {
        t.map_or_else(
            || "-".to_string(),
            |d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
    };
    let links = t.attachments.len();
    let est = t.estimate_human().unwrap_or_else(|| "-".to_string());
    let due_str = match t.due_at {
        Some(d) if t.status.is_open() && d < now => countdown(d, now),
        Some(d) if t.status.is_open() => countdown(d, now),
        Some(_) => "-".to_string(),
        None => "-".to_string(),
    };

    let mut children = 0usize;
    if set.children(&t.id).len() > 1 {
        children = set.children(&t.id).len();
    }

    // The title is last, but a naive `split('|')` must still yield exactly
    // eight fields, so the delimiter is *removed* rather than backslash-escaped.
    let title: String = t
        .title
        .chars()
        .map(|c| match c {
            '|' => '/',
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect();
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}{}",
        t.status.as_str(),
        t.priority.as_str(),
        fmt_ts(t.due_at),
        due_str,
        est,
        links,
        t.id,
        title,
        if children > 0 {
            format!(" (+{children} subtasks)")
        } else {
            String::new()
        }
    )
}

fn print_summary_rows(tasks: &[Task], set: &TaskSet, now: DateTime<Utc>) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    if tasks.is_empty() {
        writeln!(stdout, "no matching tasks").ok();
        return Ok(());
    }
    for t in tasks {
        writeln!(stdout, "{}", summary_line(t, set, now))
            .map_err(|e| Failure::store(format!("could not write output: {e}")))?;
    }
    Ok(())
}

/// Aligned columns for human reading. Only used when stdout is a TTY.
fn print_text_table(tasks: &[Task], set: &TaskSet, now: DateTime<Utc>) -> Result<()> {
    let mut stdout = std::io::stdout().lock();

    if tasks.is_empty() {
        writeln!(stdout, "no matching tasks").ok();
        return Ok(());
    }

    for (label, width) in [
        ("STAT", 4),
        ("PRI", 6),
        ("DUE", 12),
        ("LEFT", 10),
        ("EST", 8),
        ("LINKS", 7),
    ] {
        write!(stdout, "{label:<width$} ").ok();
    }
    writeln!(stdout, "TITLE").ok();

    for t in tasks {
        let left = t
            .due_at
            .filter(|_| t.status.is_open())
            .map_or_else(|| "-".to_string(), |d| countdown(d, now));
        writeln!(
            stdout,
            "{:<4} {:<6} {:<12} {:<10} {:<8} {:<7} {}{}",
            t.status.as_str(),
            t.priority.as_str(),
            t.due_at
                .map_or_else(|| "-".to_string(), |d| d.format("%Y-%m-%d").to_string()),
            left,
            t.estimate_human().unwrap_or_else(|| "-".into()),
            t.attachments.len(),
            t.title,
            {
                let kids = set.children(&t.id).len();
                if kids > 0 {
                    format!("  (+{kids})")
                } else {
                    String::new()
                }
            }
        )
        .ok();
    }

    let s = model::summarize(tasks, now);
    writeln!(stdout).ok();
    writeln!(
        stdout,
        "{} total · {} todo · {} doing · {} blocked · {} done · {} overdue · {} est",
        s.total,
        s.todo,
        s.doing,
        s.blocked,
        s.done,
        s.overdue,
        model::format_duration(Duration::minutes(i64::from(s.total_estimate_minutes)))
    )
    .ok();

    Ok(())
}

/// Serialize a task for `--json`, optionally with rollups and history attached.
fn task_json(
    t: &Task,
    set: &TaskSet,
    include_rollup: bool,
    history: &[Event],
    include_history: bool,
) -> serde_json::Value {
    let mut v = serde_json::to_value(t).unwrap_or_else(|_| json!({}));

    if include_rollup {
        let rollup = set.rollup(&t.id);
        let obj = v.as_object_mut().expect("task serialises to an object");
        obj.insert("subtask_count".into(), json!(rollup.total));
        obj.insert("subtasks_done".into(), json!(rollup.done));
        obj.insert(
            "rolled_estimate_minutes".into(),
            json!(set.rolled_estimate_minutes(&t.id)),
        );
        if let Some(due) = set.rolled_due_at(&t.id) {
            obj.insert("rolled_due_at".into(), json!(due.to_rfc3339()));
        }
        let kids: Vec<&Task> = set.children(&t.id);
        obj.insert(
            "subtasks".into(),
            json!(
                kids.iter()
                    .map(|k| json!({
                        "id": k.id.to_string(),
                        "title": k.title,
                        "status": k.status.as_str(),
                        "estimate_minutes": k.estimate_minutes,
                        "due_at": k.due_at,
                    }))
                    .collect::<Vec<_>>()
            ),
        );
    }

    if include_history {
        v.as_object_mut()
            .expect("task serialises to an object")
            .insert(
                "history".into(),
                serde_json::to_value(history).unwrap_or(json!([])),
            );
    }

    v
}

// ---------------------------------------------------------------------------
// Query commands
// ---------------------------------------------------------------------------

fn cmd_list(store: &Store, flags: GlobalFlags, args: ListArgs) -> Result<()> {
    let set = store.snapshot()?;
    let now = Utc::now();

    let explicit = if args.ids {
        Some(Output::Ids)
    } else if args.summary {
        Some(Output::Summary)
    } else {
        None
    };
    let output = Output::resolve(flags, explicit);

    // Filter, sort, reverse, and truncate exactly once, so every output mode
    // (`--json`, `--ids`, `--summary`, text) sees the same rows in the same order.
    let filter = build_filter(&args.query, &set)?;
    let sort = parse_sort(&args.query.sort)?;
    let mut tasks = select(&set, &filter, sort, now);
    if args.query.reverse {
        tasks.reverse();
    }
    let limit = limit_of(args.limit);
    if limit < tasks.len() {
        tasks.truncate(limit);
    }

    if output == Output::Ids {
        let mut stdout = std::io::stdout().lock();
        for t in &tasks {
            writeln!(stdout, "{}", t.id).ok();
        }
        return Ok(());
    }

    match output {
        Output::Json => {
            let payload = if args.query.tree {
                json!({
                    "count": tasks.len(),
                    "tree": tasks.iter().map(|t| json!({
                        "task": t,
                        "subtasks": set.children(&t.id),
                    })).collect::<Vec<_>>(),
                })
            } else {
                json!({
                    "count": tasks.len(),
                    "summary": model::summarize(&tasks, now),
                    "tasks": tasks,
                })
            };
            print_json(&payload)
        }
        Output::Text => print_text_table(&tasks, &set, now),
        _ => print_summary_rows(&tasks, &set, now),
    }
}

fn limit_of(limit: usize) -> usize {
    if limit == 0 { usize::MAX } else { limit }
}

fn cmd_show(store: &Store, flags: GlobalFlags, args: ShowArgs) -> Result<()> {
    let set = store.snapshot()?;
    let now = Utc::now();
    let output = Output::resolve(flags, None);

    let id = set
        .resolve(&args.reference)
        .map_err(|e| Failure::not_found(e.to_string()))?;
    let task = set
        .get(&id)
        .ok_or_else(|| Failure::not_found(format!("no task matches {:?}", args.reference)))?;

    let history = if args.history {
        store.events_for(&id).unwrap_or_default()
    } else {
        Vec::new()
    };

    match output {
        Output::Json => print_json(&task_json(task, &set, args.rollup, &history, args.history)),
        _ => {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{}", task.title).ok();
            writeln!(stdout).ok();
            writeln!(stdout, "  id       {}", task.id).ok();
            writeln!(stdout, "  status   {}", task.status).ok();
            writeln!(stdout, "  priority {}", task.priority).ok();
            writeln!(stdout, "  created  {}", task.created_at.to_rfc3339()).ok();
            writeln!(
                stdout,
                "  due      {}",
                task.due_at.map_or_else(
                    || "-".into(),
                    |d| format!("{} ({})", d.to_rfc3339(), countdown(d, now))
                )
            )
            .ok();
            writeln!(
                stdout,
                "  estimate {}",
                task.estimate_human().unwrap_or_else(|| "-".into())
            )
            .ok();
            if let Some(c) = task.completed_at {
                writeln!(stdout, "  done     {}", c.to_rfc3339()).ok();
            }
            if !task.tags.is_empty() {
                writeln!(stdout, "  tags     {}", task.tags.join(", ")).ok();
            }

            if !task.description.is_empty() {
                writeln!(stdout).ok();
                for line in task.description.lines() {
                    writeln!(stdout, "  {line}").ok();
                }
            }

            if !task.attachments.is_empty() {
                writeln!(stdout).ok();
                writeln!(stdout, "  attachments").ok();
                for a in &task.attachments {
                    writeln!(stdout, "    [{}] {}", a.kind.as_str(), a.display()).ok();
                    if a.display() != a.target {
                        writeln!(stdout, "          {}", a.target).ok();
                    }
                }
            }

            if args.rollup {
                let r = set.rollup(&task.id);
                writeln!(stdout).ok();
                writeln!(
                    stdout,
                    "  subtasks {}/{} done · rolled estimate {}",
                    r.done,
                    r.total,
                    model::format_duration(Duration::minutes(i64::from(
                        set.rolled_estimate_minutes(&task.id)
                    )))
                )
                .ok();
                for k in set.children(&task.id) {
                    writeln!(
                        stdout,
                        "    {}  {}  {}",
                        k.status.as_str(),
                        k.title,
                        k.estimate_human().unwrap_or_else(|| "-".into())
                    )
                    .ok();
                }
            }

            if args.history && !history.is_empty() {
                writeln!(stdout).ok();
                writeln!(stdout, "  history").ok();
                for ev in &history {
                    writeln!(
                        stdout,
                        "    {}  {}  {}",
                        ev.ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        ev.event.as_str(),
                        ev.title.as_deref().unwrap_or("")
                    )
                    .ok();
                }
            }

            Ok(())
        }
    }
}

fn cmd_summary(store: &Store, flags: GlobalFlags, args: SummaryArgs) -> Result<()> {
    let set = store.snapshot()?;
    let now = Utc::now();
    // No explicit override: a global --json must still win here.
    let output = Output::resolve(flags, None);

    let filter = build_filter(&args.query, &set)?;
    let tasks = select(&set, &filter, parse_sort(&args.query.sort)?, now);
    let summary = model::summarize(&tasks, now);

    match output {
        Output::Json => print_json(&json!({
            "summary": summary,
            "root_count": set.roots().len(),
            "total_in_store": set.tasks.len(),
            "with_attachments": set.tasks.iter().filter(|t| !t.attachments.is_empty()).count(),
            "estimate_total_human": model::format_duration(Duration::minutes(
                i64::from(summary.total_estimate_minutes))),
        })),
        _ => {
            let mut stdout = std::io::stdout().lock();
            for line in [
                format!("total{:>13}", summary.total),
                format!("todo{:>14}", summary.todo),
                format!("doing{:>13}", summary.doing),
                format!("blocked{:>12}", summary.blocked),
                format!("done{:>14}", summary.done),
                format!("cancelled{:>11}", summary.cancelled),
                format!("overdue{:>12}", summary.overdue),
                format!("due today{:>11}", summary.due_today),
                format!(
                    "estimate{:>11}",
                    model::format_duration(Duration::minutes(i64::from(
                        summary.total_estimate_minutes
                    )))
                ),
            ] {
                writeln!(stdout, "{line}").ok();
            }
            Ok(())
        }
    }
}

/// Machine-readable "what is due when", for an agent planning its own work.
fn cmd_timeline(store: &Store, flags: GlobalFlags, args: TimelineArgs) -> Result<()> {
    let set = store.snapshot()?;
    let now = Utc::now();
    // No explicit override: a global --json must still win here.
    let output = Output::resolve(flags, None);

    let from = now - Duration::days(args.before);
    let to = now + Duration::days(args.days);

    let mut rows: Vec<serde_json::Value> = set
        .tasks
        .iter()
        .filter(|t| !args.roots || t.parent_id.is_none())
        .filter(|t| t.status.is_open())
        .filter(|t| {
            // Keep anything with a due date inside the window, plus undated
            // tasks when their bar would still be on screen.
            match t.due_at {
                Some(d) => d >= from && d <= to,
                None => true,
            }
        })
        .map(|t| {
            json!({
                "id": t.id.to_string(),
                "title": t.title,
                "status": t.status.as_str(),
                "priority": t.priority.as_str(),
                "parent_id": t.parent_id,
                "due_at": t.due_at,
                // Fractional, so "due in 23h" is not silently reported as 0 days.
                "days_remaining": t.due_at.map(|d| (d - now).num_seconds() as f64 / 86_400.0),
                "hours_remaining": t.due_at.map(|d| (d - now).num_hours()),
                "overdue": t.due_at.is_some_and(|d| d < now),
                "created_at": t.created_at,
                "estimate_minutes": t.estimate_minutes,
                "rolled_estimate_minutes": set.rolled_estimate_minutes(&t.id),
                "attachments": t.attachments.iter().map(|a| json!({
                    "kind": a.kind.as_str(),
                    "label": a.label,
                    "target": a.target,
                })).collect::<Vec<_>>(),
                "subtask_count": set.rollup(&t.id).total,
                "subtasks_done": set.rollup(&t.id).done,
            })
        })
        .collect();

    // Soonest deadline first; undated tasks last.
    rows.sort_by_key(|v| {
        let due = v.get("due_at").and_then(serde_json::Value::as_str);
        match due {
            Some(d) => (0u8, d.to_string()),
            None => (1u8, String::new()),
        }
    });

    match output {
        Output::Json => print_json(&json!({
            "now": now.to_rfc3339(),
            "from": from.to_rfc3339(),
            "to": to.to_rfc3339(),
            "count": rows.len(),
            "items": rows,
        })),
        _ => {
            let mut stdout = std::io::stdout().lock();
            if rows.is_empty() {
                writeln!(stdout, "nothing due between {from} and {to}").ok();
                return Ok(());
            }
            for v in &rows {
                let title = v["title"].as_str().unwrap_or("");
                let due = v["due_at"].as_str().unwrap_or("-");
                let left = v["hours_remaining"].as_i64().unwrap_or(0);
                let est = v["estimate_minutes"].as_u64().unwrap_or(0);
                let left = if left < 0 {
                    "overdue".to_string()
                } else {
                    format!("{left}d")
                };
                let marker = if v["overdue"].as_bool().unwrap_or(false) {
                    "OVERDUE"
                } else if v["priority"].as_str() == Some("urgent") {
                    "urgent"
                } else {
                    "-"
                };
                writeln!(
                    stdout,
                    "{:<12} {:<9} {:>7}  {:<8} {}",
                    due.chars().take(10).collect::<String>(),
                    left,
                    if est > 0 {
                        format!("{est}m")
                    } else {
                        "-".to_string()
                    },
                    marker,
                    title
                )
                .ok();
            }
            Ok(())
        }
    }
}

fn cmd_info(store: &Store, flags: GlobalFlags, args: InfoArgs) -> Result<()> {
    let set = store.snapshot()?;
    let now = Utc::now();
    let summary = model::summarize(&set.tasks, now);

    if args.paths || flags.json {
        return print_json(&json!({
            "root": store.root(),
            "tasks_path": store.tasks_path(),
            "events_path": store.events_path(),
            "lock_path": store.lock_path(),
            "exists": store.exists(),
            "schema_version": set.schema_version,
            "task_count": set.tasks.len(),
            "updated_at": set.updated_at.to_rfc3339(),
        }));
    }

    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "store    {}", store.root().display()).ok();
    writeln!(stdout, "tasks    {}", set.tasks.len()).ok();
    writeln!(stdout, "schema   {}", set.schema_version).ok();
    writeln!(
        stdout,
        "open     {} · {} overdue · {} due today",
        summary.total - summary.done - summary.cancelled,
        summary.overdue,
        summary.due_today
    )
    .ok();
    writeln!(stdout, "updated  {}", set.updated_at.to_rfc3339()).ok();
    Ok(())
}

// ---------------------------------------------------------------------------
// Mutation commands
// ---------------------------------------------------------------------------

/// Attach URL/PDF arguments to a task, pairing `--label` positionally.
fn build_attachments(
    urls: &[String],
    pdfs: &[String],
    labels: &[String],
) -> Result<Vec<Attachment>> {
    let mut out = Vec::new();
    let mut label_iter = labels.iter();

    for u in urls {
        let label = label_iter.next().cloned().unwrap_or_default();
        let a = Attachment::url(u.clone(), label);
        a.validate().map_err(|e| Failure::usage(e.to_string()))?;
        out.push(a);
    }
    for p in pdfs {
        let label = label_iter.next().cloned().unwrap_or_default();
        let a = Attachment::pdf(p.clone(), label);
        a.validate().map_err(|e| Failure::usage(e.to_string()))?;
        out.push(a);
    }
    Ok(out)
}

fn cmd_add(store: &Store, flags: GlobalFlags, args: AddArgs) -> Result<()> {
    let now = Utc::now();
    let output = Output::resolve(flags, Some(Output::Json));

    let due_at = match &args.due {
        Some(d) => Some(parse_due(d, now)?),
        None => None,
    };
    let estimate_minutes = match &args.estimate {
        Some(e) => Some(parse_estimate(e)?),
        None => None,
    };
    let status = match &args.status {
        Some(s) => parse_status(s)?,
        None => Status::Todo,
    };
    let priority = match &args.priority {
        Some(p) => parse_priority(p)?,
        None => Priority::Normal,
    };
    let attachments = build_attachments(&args.urls, &args.pdfs, &args.labels)?;

    let mut task = Task::new(args.title.clone(), now);
    task.due_at = due_at;
    task.estimate_minutes = estimate_minutes;
    task.status = status;
    task.priority = priority;
    task.attachments = attachments;
    task.tags = args.tags.clone();
    if let Some(d) = args.description.clone() {
        task.description = d;
    }
    if status == Status::Doing {
        task.started_at = Some(now);
    }

    // Resolve the parent once, outside the mutation, so a bad reference is
    // reported as a normal CLI failure rather than a store error.
    let parent_id = match &args.parent {
        Some(p) => {
            let set = store.snapshot()?;
            Some(
                set.resolve(p)
                    .map_err(|e| Failure::not_found(format!("parent {p:?}: {e}")))?,
            )
        }
        None => None,
    };
    task.parent_id = parent_id;

    if args.dry_run {
        task.validate()
            .map_err(|e| Failure::store(format!("invalid task: {e}")))?;
        let errs = {
            let mut probe = TaskSet::new();
            probe.tasks.push(task.clone());
            model::validate_graph(&probe)
        };
        if let Some(e) = errs.first() {
            return Err(Failure::store(format!("invalid task: {e}")));
        }
        return print_json(&json!({
            "dry_run": true,
            "would_create": task,
        }));
    }

    let created = store.mutate(|set, events| {
        // Re-assert the parent still exists; the store may have changed between
        // our read and acquiring the lock.
        if let Some(pid) = parent_id
            && set.get(&pid).is_none()
        {
            return Err(StoreError::Validation(ValidationError::ParentNotFound(pid)));
        }
        set.insert(task.clone())?;
        events
            .push(Event::new(EventKind::TaskCreated, Some(task.id)).with_title(task.title.clone()));
        Ok(task.clone())
    })?;

    match output {
        Output::Json => print_json(&json!({ "created": created })),
        _ => {
            println!("{}  {}", created.id, created.title);
            Ok(())
        }
    }
}

/// Parsed `--edit` intent, resolved before the store lock is taken so that a
/// bad argument fails fast without touching the file.
#[derive(Debug, Clone)]
struct EditPlan {
    title: Option<String>,
    description: Option<String>,
    due: DueChange,
    estimate: Option<u32>,
    clear_estimate: bool,
    status: Option<Status>,
    priority: Option<Priority>,
    add_tags: Vec<String>,
    remove_tags: Vec<String>,
    /// `Some(None)` clears the parent, `Some(Some(id))` sets it, `None` leaves it.
    parent: Option<Option<TaskId>>,
}

/// The before/after of one `--edit` invocation.
struct EditResult {
    /// Post-change values, for the audit trail.
    patch: serde_json::Map<String, serde_json::Value>,
    /// Pre-change values, so `ttm undo` can put them back.
    undo: serde_json::Map<String, serde_json::Value>,
}

/// Apply an [`EditPlan`] to a task, recording both directions. Pure, so the
/// dry-run path and the real mutation share exactly one implementation.
fn apply_edit(t: &mut Task, plan: &EditPlan) -> EditResult {
    let mut patch = serde_json::Map::new();
    let mut undo = serde_json::Map::new();

    // Each field records its own previous value before being overwritten, so
    // `patch` and `undo` stay keyed identically.
    fn record(
        patch: &mut serde_json::Map<String, serde_json::Value>,
        undo: &mut serde_json::Map<String, serde_json::Value>,
        key: &str,
        before: serde_json::Value,
        after: &serde_json::Value,
    ) {
        undo.insert(key.to_string(), before);
        patch.insert(key.to_string(), after.clone());
    }

    if let Some(v) = &plan.title {
        let before = json!(t.title);
        t.title = v.clone();
        record(&mut patch, &mut undo, "title", before, &json!(t.title));
    }
    if let Some(v) = &plan.description {
        let before = json!(t.description);
        t.description = v.clone();
        record(
            &mut patch,
            &mut undo,
            "description",
            before,
            &json!(t.description),
        );
    }
    match plan.due {
        DueChange::Set(v) => {
            let before = json!(t.due_at);
            t.due_at = Some(v);
            record(&mut patch, &mut undo, "due_at", before, &json!(t.due_at));
        }
        DueChange::Clear => {
            let before = json!(t.due_at);
            t.due_at = None;
            record(&mut patch, &mut undo, "due_at", before, &json!(t.due_at));
        }
        DueChange::Keep => {}
    }
    if plan.estimate.is_some() || plan.clear_estimate {
        let before = json!(t.estimate_minutes);
        t.estimate_minutes = if plan.clear_estimate {
            None
        } else {
            plan.estimate
        };
        record(
            &mut patch,
            &mut undo,
            "estimate_minutes",
            before,
            &json!(t.estimate_minutes),
        );
    }
    if let Some(p) = plan.priority {
        let before = json!(t.priority.as_str());
        t.priority = p;
        record(
            &mut patch,
            &mut undo,
            "priority",
            before,
            &json!(t.priority.as_str()),
        );
    }

    let before_tags = t.tags.clone();
    for tag in &plan.add_tags {
        if !t.tags.iter().any(|x| x.eq_ignore_ascii_case(tag)) {
            t.tags.push(tag.clone());
        }
    }
    for tag in &plan.remove_tags {
        t.tags.retain(|x| !x.eq_ignore_ascii_case(tag));
    }
    if t.tags != before_tags {
        undo.insert("tags".into(), json!(before_tags));
        patch.insert("tags".into(), json!(t.tags));
    }

    if let Some(parent) = plan.parent {
        undo.insert("parent_id".into(), json!(t.parent_id));
        t.parent_id = parent;
        patch.insert(
            "parent_id".into(),
            parent.map_or(json!(null), |id| json!(id.to_string())),
        );
    }

    // Status is applied by the caller so its timestamps are stamped once, but
    // the previous status is recorded here so undo can restore it.
    if let Some(s) = plan.status {
        undo.insert("status".into(), json!(t.status.as_str()));
        patch.insert("status".into(), json!(s.as_str()));
    }

    EditResult { patch, undo }
}

/// Restore the field values recorded in an event's `undo` payload.
///
/// Returns the names of the fields actually reverted.
fn revert_task(t: &mut Task, undo: &serde_json::Value, now: DateTime<Utc>) -> Vec<String> {
    let Some(fields) = undo.as_object() else {
        return Vec::new();
    };
    let mut reverted = Vec::new();

    for (key, prev) in fields {
        match key.as_str() {
            "title" => t.title = prev.as_str().unwrap_or_default().to_string(),
            "description" => t.description = prev.as_str().unwrap_or_default().to_string(),
            "due_at" => {
                t.due_at = prev
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc));
            }
            "estimate_minutes" => {
                t.estimate_minutes = prev.as_u64().and_then(|n| u32::try_from(n).ok());
            }
            "priority" => {
                if let Some(p) = prev.as_str().and_then(Priority::parse) {
                    t.priority = p;
                }
            }
            "tags" => {
                t.tags = prev
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
            }
            "parent_id" => {
                t.parent_id = prev.as_str().and_then(|s| uuid::Uuid::parse_str(s).ok());
            }
            "attachments" => {
                t.attachments = prev
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| serde_json::from_value(v.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default();
            }
            "status" => {
                if let Some(s) = prev.as_str().and_then(Status::parse) {
                    t.set_status(s, now);
                    reverted.push(key.clone());
                    continue; // set_status already stamps updated_at
                }
            }
            _ => continue,
        }
        reverted.push(key.clone());
    }

    reverted
}

/// Build an [`EditPlan`] from parsed arguments. Fails on unknown enum values.
fn build_edit_plan(
    args: &EditArgs,
    now: DateTime<Utc>,
    parent: Option<TaskId>,
) -> Result<EditPlan> {
    let due = match (&args.due, args.no_due) {
        (Some(d), _) => DueChange::Set(parse_due(d, now)?),
        (None, true) => DueChange::Clear,
        (None, false) => DueChange::Keep,
    };

    let parent: Option<Option<TaskId>> = if let Some(p) = parent {
        Some(Some(p))
    } else if args.no_parent {
        Some(None)
    } else {
        None
    };

    Ok(EditPlan {
        title: args.title.clone(),
        description: args.description.clone(),
        due,
        estimate: match &args.estimate {
            Some(e) => Some(parse_estimate(e)?),
            None => None,
        },
        clear_estimate: args.no_estimate,
        status: match &args.status {
            Some(s) => Some(parse_status(s)?),
            None => None,
        },
        priority: match &args.priority {
            Some(p) => Some(parse_priority(p)?),
            None => None,
        },
        add_tags: args.tags.clone(),
        remove_tags: args.untags.clone(),
        parent,
    })
}

fn cmd_edit(store: &Store, flags: GlobalFlags, args: EditArgs) -> Result<()> {
    let now = Utc::now();
    let output = Output::resolve(flags, Some(Output::Json));

    let parent_id = match &args.parent {
        Some(p) => {
            let set = store.snapshot()?;
            Some(
                set.resolve(p)
                    .map_err(|e| Failure::not_found(format!("parent {p:?}: {e}")))?,
            )
        }
        None => None,
    };
    let plan = build_edit_plan(&args, now, parent_id)?;

    if plan.title.is_none()
        && plan.description.is_none()
        && matches!(plan.due, DueChange::Keep)
        && plan.estimate.is_none()
        && !plan.clear_estimate
        && plan.status.is_none()
        && plan.priority.is_none()
        && plan.add_tags.is_empty()
        && plan.remove_tags.is_empty()
        && plan.parent.is_none()
    {
        return Err(Failure::usage(
            "nothing to change; pass at least one of --title, --description, --due, \
             --no-due, --estimate, --no-estimate, --status, --priority, --tag, \
             --untag, --parent, --no-parent",
        ));
    }

    if args.dry_run {
        let set = store.snapshot()?;
        let task = resolve(&set, &args.reference)?.clone();
        if let Some(pid) = parent_id
            && set.get(&pid).is_none()
        {
            return Err(Failure::not_found(format!("parent {pid} no longer exists")));
        }

        let mut updated = task.clone();
        let result = apply_edit(&mut updated, &plan);
        if let Some(s) = plan.status {
            updated.set_status(s, now);
        }
        updated.updated_at = now;
        updated
            .validate()
            .map_err(|e| Failure::store(format!("invalid task: {e}")))?;

        return print_json(&json!({
            "dry_run": true,
            "current": task,
            "would_update": updated,
            "patch": result.patch,
            "would_restore_on_undo": result.undo,
        }));
    }

    let updated = store.mutate(|set, events| {
        let id = set
            .resolve(&args.reference)
            .map_err(|_| StoreError::NotFound(uuid::Uuid::nil()))?;

        let mut task = set.get(&id).cloned().ok_or(StoreError::NotFound(id))?;

        if let Some(pid) = parent_id
            && set.get(&pid).is_none()
        {
            return Err(StoreError::Validation(ValidationError::ParentNotFound(pid)));
        }

        let result = apply_edit(&mut task, &plan);
        if let Some(s) = plan.status {
            task.set_status(s, now);
        }
        task.updated_at = now;
        task.validate()?;

        if let Some(existing) = set.get_mut(&id) {
            *existing = task.clone();
        }

        events.push(
            Event::new(EventKind::TaskUpdated, Some(id))
                .with_title(task.title.clone())
                .with_patch(serde_json::Value::Object(result.patch))
                .with_undo(serde_json::Value::Object(result.undo)),
        );
        Ok(task)
    })?;

    match output {
        Output::Json => print_json(&json!({ "updated": updated })),
        _ => {
            println!("{}  {}", updated.id, updated.title);
            Ok(())
        }
    }
}

fn cmd_set_status(store: &Store, flags: GlobalFlags, args: RefArgs, status: Status) -> Result<()> {
    let now = Utc::now();
    let output = Output::resolve(flags, Some(Output::Json));

    if args.dry_run {
        let set = store.snapshot()?;
        let task = resolve(&set, &args.reference)?;
        let mut updated = task.clone();
        updated.set_status(status, now);
        return print_json(&json!({
            "dry_run": true,
            "current": task,
            "would_update": updated,
        }));
    }

    let updated = store.mutate(|set, events| {
        let id = set
            .resolve(&args.reference)
            .map_err(|_| StoreError::NotFound(uuid::Uuid::nil()))?;
        let task = set.get_mut(&id).ok_or(StoreError::NotFound(id))?;
        let previous = task.status;
        task.set_status(status, now);
        let title = task.title.clone();
        events.push(
            Event::new(EventKind::StatusChanged, Some(id))
                .with_title(title)
                .with_patch(json!({
                    "from": previous.as_str(),
                    "to": status.as_str(),
                }))
                .with_undo(json!({ "status": previous.as_str() })),
        );
        Ok(task.clone())
    })?;

    match output {
        Output::Json => print_json(&json!({ "updated": updated })),
        _ => {
            println!("{}  {}  {}", updated.id, updated.status, updated.title);
            Ok(())
        }
    }
}

fn cmd_remove(store: &Store, flags: GlobalFlags, args: RemoveArgs) -> Result<()> {
    let output = Output::resolve(flags, Some(Output::Json));
    let _ = args.cascade; // cascading is the only supported behaviour

    if args.dry_run {
        let set = store.snapshot()?;
        let task = resolve(&set, &args.reference)?;
        let doomed: Vec<Task> = set
            .tasks
            .iter()
            .filter(|t| {
                Some(&t.id)
                    == resolve(&set, &args.reference)
                        .ok()
                        .map(|_| &t.id)
                        .filter(|_| t.id == task.id || t.parent_id.as_ref() == Some(&task.id))
            })
            .cloned()
            .collect();
        return print_json(&json!({
            "dry_run": true,
            "would_delete": doomed.iter().map(|t| json!({
                "id": t.id.to_string(),
                "title": t.title,
            })).collect::<Vec<_>>(),
        }));
    }

    let removed = store.mutate(|set, events| {
        let id = set
            .resolve(&args.reference)
            .map_err(|_| StoreError::NotFound(uuid::Uuid::nil()))?;
        let titles: Vec<String> = set
            .tasks
            .iter()
            .filter(|t| t.id == id || t.parent_id.as_ref() == Some(&id))
            .map(|t| t.title.clone())
            .collect();
        let ids = set.remove_cascade(&id);
        for (rid, title) in ids.iter().zip(titles.iter()) {
            events.push(Event::new(EventKind::TaskDeleted, Some(*rid)).with_title(title.clone()));
        }
        Ok(json!({
            "removed": ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "titles": titles,
        }))
    })?;

    match output {
        Output::Json => print_json(&removed),
        _ => {
            if let Some(titles) = removed.get("titles").and_then(serde_json::Value::as_array) {
                for t in titles {
                    println!("removed  {}", t.as_str().unwrap_or(""));
                }
            }
            Ok(())
        }
    }
}

fn cmd_link(store: &Store, flags: GlobalFlags, args: LinkArgs) -> Result<()> {
    let output = Output::resolve(flags, Some(Output::Json));

    let attachment = match (&args.url, &args.pdf) {
        (Some(u), _) => Attachment::url(u.clone(), args.label.clone().unwrap_or_default()),
        (_, Some(p)) => Attachment::pdf(p.clone(), args.label.clone().unwrap_or_default()),
        _ => return Err(Failure::usage("provide --url or --pdf")),
    };
    attachment
        .validate()
        .map_err(|e| Failure::usage(e.to_string()))?;

    if args.dry_run {
        let set = store.snapshot()?;
        let task = resolve(&set, &args.reference)?;
        return print_json(&json!({
            "dry_run": true,
            "task": task.title,
            "would_add": attachment,
        }));
    }

    let a = attachment.clone();
    let updated = store.mutate(|set, events| {
        let id = set
            .resolve(&args.reference)
            .map_err(|_| StoreError::NotFound(uuid::Uuid::nil()))?;
        let task = set.get_mut(&id).ok_or(StoreError::NotFound(id))?;
        let previous = task.attachments.clone();
        task.attachments.push(a.clone());
        task.updated_at = Utc::now();
        events.push(
            Event::new(EventKind::AttachmentAdded, Some(id))
                .with_title(task.title.clone())
                .with_patch(json!({
                    "kind": a.kind.as_str(),
                    "label": a.label,
                    "target": a.target,
                }))
                .with_undo(json!({ "attachments": previous })),
        );
        Ok(task.clone())
    })?;

    match output {
        Output::Json => print_json(&json!({ "updated": updated })),
        _ => {
            println!(
                "{}  +{}  {}",
                updated.id,
                attachment.kind.as_str(),
                attachment.display()
            );
            Ok(())
        }
    }
}

/// Revert the most recent recorded change to a task.
///
/// This works from the `undo` payload written alongside every mutation, not from
/// a snapshot diff, so it restores exactly the fields that change touched. It
/// does not remove events from the log: the audit trail records the undo as a
/// new change rather than rewriting history.
fn cmd_undo(store: &Store, flags: GlobalFlags, args: UndoArgs) -> Result<()> {
    let output = Output::resolve(flags, Some(Output::Json));
    let now = Utc::now();

    let events = store.read_events(None)?;
    let set = store.snapshot()?;

    // Narrow to the referenced task when one was given.
    let target_id = match &args.reference {
        Some(r) => Some(
            set.resolve(r)
                .map_err(|e| Failure::not_found(e.to_string()))?,
        ),
        None => None,
    };

    let revertible = [
        EventKind::TaskUpdated,
        EventKind::StatusChanged,
        EventKind::AttachmentAdded,
    ];

    let candidates: Vec<&Event> = events
        .iter()
        .filter(|e| revertible.contains(&e.event))
        .filter(|e| e.undo.is_some())
        .filter(|e| match (target_id, e.task_id) {
            (Some(want), Some(have)) => want == have,
            (Some(_), None) => false,
            (None, _) => true,
        })
        .collect();

    let target = candidates
        .iter()
        .rev()
        .nth(args.steps.saturating_sub(1))
        .ok_or_else(|| {
            Failure::not_found(match &args.reference {
                Some(r) => format!("no undoable change recorded for {r:?}"),
                None => "no undoable change found in the event log".to_string(),
            })
        })?;

    let id = target
        .task_id
        .ok_or_else(|| Failure::not_found("that change has no task id"))?;
    let undo = target
        .undo
        .clone()
        .ok_or_else(|| Failure::not_found("that change recorded nothing to undo"))?;

    if args.dry_run {
        let task = set
            .get(&id)
            .ok_or_else(|| Failure::not_found("that task no longer exists"))?;
        let mut preview = task.clone();
        let fields = revert_task(&mut preview, &undo, now);
        return print_json(&json!({
            "dry_run": true,
            "reverting_event": target,
            "fields": fields,
            "current": task,
            "would_update": preview,
        }));
    }

    let updated = store.mutate(|set, events| {
        let task = set.get_mut(&id).ok_or(StoreError::NotFound(id))?;
        let fields = revert_task(task, &undo, now);
        task.updated_at = now;

        let title = task.title.clone();
        let reverted = task.clone();
        events.push(
            Event::new(EventKind::TaskUpdated, Some(id))
                .with_title(title)
                .with_patch(json!({
                    "undo_of": target.ts.to_rfc3339(),
                    "reverted_fields": fields,
                })),
        );
        Ok(reverted)
    })?;

    match output {
        Output::Json => print_json(&json!({ "reverted": updated })),
        _ => {
            println!("reverted  {}  {}", updated.id, updated.title);
            Ok(())
        }
    }
}

// Re-exported for callers that format values the way `--summary` does.
fn countdown(due: DateTime<Utc>, now: DateTime<Utc>) -> String {
    model::countdown(due, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap validates flag uniqueness, defaults, and conflicts lazily. Without
    /// this, a collision like two args claiming `-S` only panics at runtime.
    #[test]
    fn clap_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_command_has_a_help_page() {
        let cmd = Cli::command();
        let subs: Vec<_> = cmd.get_subcommands().collect();
        assert!(subs.len() >= 12, "expected the full command surface");
        for sub in subs {
            assert!(
                sub.get_about().is_some(),
                "`ttm {}` is missing its about text",
                sub.get_name()
            );
        }
    }

    #[test]
    fn summary_line_has_stable_field_order() {
        let now = Utc::now();
        let mut t = Task::new("Ship it", now);
        t.estimate_minutes = Some(90);
        t.due_at = Some(now + Duration::days(2));

        let line = summary_line(&t, &TaskSet::new(), now);
        let fields: Vec<&str> = line.split('|').collect();

        // 8 fixed fields, with the id in position 7 and the title last.
        assert_eq!(fields.len(), 8);
        assert_eq!(fields[0], "todo");
        assert_eq!(fields[1], "normal");
        assert_eq!(fields[4], "1h 30m");
        assert_eq!(fields[6], t.id.to_string());
        assert_eq!(fields[7], "Ship it");
    }

    #[test]
    fn summary_line_uses_dash_for_missing_optionals() {
        let now = Utc::now();
        let t = Task::new("bare", now);
        let line = summary_line(&t, &TaskSet::new(), now);
        let fields: Vec<&str> = line.split('|').collect();

        assert_eq!(fields[2], "-");
        assert_eq!(fields[4], "-");
        assert_eq!(fields[5], "0");
    }

    #[test]
    fn summary_line_stays_parseable_for_awkward_titles() {
        let now = Utc::now();
        let t = Task::new("a|b\nc\td", now);
        let line = summary_line(&t, &TaskSet::new(), now);
        assert_eq!(
            line.split('|').count(),
            8,
            "a naive split must still yield eight fields: {line}"
        );
        assert!(line.ends_with("a/b c d"));
    }

    #[test]
    fn labels_pair_positionally_with_attachments() {
        let urls = vec!["https://a.test".to_string()];
        let pdfs = vec!["/tmp/b.pdf".to_string()];
        let labels = vec!["first".to_string(), "second".to_string()];

        let out = build_attachments(&urls, &pdfs, &labels).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].label, "first");
        assert_eq!(out[1].label, "second");
    }

    #[test]
    fn build_attachments_rejects_bad_targets() {
        let urls = vec!["not-a-url".to_string()];
        assert!(build_attachments(&urls, &[], &[]).is_err());

        let pdfs = vec!["/tmp/notes.md".to_string()];
        assert!(build_attachments(&[], &pdfs, &[]).is_err());
    }

    #[test]
    fn status_and_priority_parse_errors_are_usage_level() {
        let e = parse_status("nope").unwrap_err();
        assert_eq!(e.code, Exit::Usage);

        let e = parse_priority("nope").unwrap_err();
        assert_eq!(e.code, Exit::Usage);

        let e = parse_due_window("soon").unwrap_err();
        assert_eq!(e.code, Exit::Usage);

        let e = parse_sort("nope").unwrap_err();
        assert_eq!(e.code, Exit::Usage);

        let e = parse_due("never", Utc::now()).unwrap_err();
        assert_eq!(e.code, Exit::Usage);

        let e = parse_estimate("lots").unwrap_err();
        assert_eq!(e.code, Exit::Usage);
    }

    #[test]
    fn missing_task_is_not_found_level() {
        let set = TaskSet::new();
        let e = resolve(&set, "ghost").unwrap_err();
        assert_eq!(e.code, Exit::NotFound);
    }

    #[test]
    fn limit_of_zero_means_unlimited() {
        assert_eq!(limit_of(0), usize::MAX);
        assert_eq!(limit_of(5), 5);
    }

    #[test]
    fn store_errors_map_to_store_exit_code() {
        let e: Failure = StoreError::Locked {
            path: PathBuf::from("/tmp/x"),
        }
        .into();
        assert_eq!(e.code, Exit::Store);
    }
}
