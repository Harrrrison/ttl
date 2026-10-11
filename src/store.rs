//! Persistence layer: the JSON document, the append-only event log, and the
//! cross-process lock that keeps the TUI and `ttm` CLI subcommands from
//! clobbering each other.
//!
//! Layout of `$TTM_HOME` (default `~/.local/share/ttm`):
//!
//! ```text
//! tasks.json     source of truth, replaced atomically
//! events.jsonl   append-only audit log, one JSON object per line
//! store.lock     advisory exclusive lock, held across read-modify-write
//! ```
//!
//! `tasks.json` is the contract an AI agent reads. `events.jsonl` exists so an
//! agent can tail *what happened* without diffing two snapshots, and so
//! completion history survives even after a task is deleted.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{TaskId, TaskSet, ValidationError};

pub const TASKS_FILE: &str = "tasks.json";
pub const EVENTS_FILE: &str = "events.jsonl";
pub const LOCK_FILE: &str = "store.lock";

/// How long `lock()` waits before giving up, so a stuck peer produces an error
/// instead of a hang.
const LOCK_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const LOCK_POLL: StdDuration = StdDuration::from_millis(25);

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("store schema version {found} is newer than supported version {supported}")]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("validation failed: {0}")]
    Validation(#[from] ValidationError),

    #[error("task {0} not found")]
    NotFound(TaskId),

    #[error("store at {path} is locked by another process")]
    Locked { path: PathBuf },

    #[error("could not resolve {query:?}: {reason}")]
    Resolve {
        query: String,
        #[source]
        reason: crate::model::ResolveError,
    },
}

impl StoreError {
    fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    fn parse(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        Self::Parse {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

// ---------------------------------------------------------------------------
// Event log
// ---------------------------------------------------------------------------

/// The kind of change recorded in `events.jsonl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    TaskCreated,
    TaskUpdated,
    TaskDeleted,
    StatusChanged,
    AttachmentAdded,
    AttachmentRemoved,
    /// Emitted when a store is first created, and by `--summary` sweeps.
    Note,
}

impl EventKind {
    /// Wire name, matching the serde representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::TaskCreated => "task_created",
            EventKind::TaskUpdated => "task_updated",
            EventKind::TaskDeleted => "task_deleted",
            EventKind::StatusChanged => "status_changed",
            EventKind::AttachmentAdded => "attachment_added",
            EventKind::AttachmentRemoved => "attachment_removed",
            EventKind::Note => "note",
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One line of `events.jsonl`.
///
/// `patch` holds the *new* values for changed fields only, which keeps lines
/// small and lets an agent apply them without knowing the full task shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    #[serde(default = "Utc::now")]
    pub ts: DateTime<Utc>,
    pub event: EventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<serde_json::Value>,

    /// Previous values of the fields `patch` touched, keyed identically.
    ///
    /// `patch` answers "what is it now" (an audit concern); `undo` answers
    /// "what was it before" (a revert concern). They cannot be derived from
    /// each other, so both are recorded at write time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<serde_json::Value>,
}

impl Event {
    #[must_use]
    pub fn new(kind: EventKind, task_id: Option<TaskId>) -> Self {
        Self {
            ts: Utc::now(),
            event: kind,
            task_id,
            title: None,
            patch: None,
            undo: None,
        }
    }

    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    #[must_use]
    pub fn with_patch(mut self, patch: serde_json::Value) -> Self {
        self.patch = Some(patch);
        self
    }

    /// Record the values this change replaces, enabling `ttm undo`.
    #[must_use]
    pub fn with_undo(mut self, undo: serde_json::Value) -> Self {
        self.undo = Some(undo);
        self
    }

    /// Serialize as one compact line, newline-terminated.
    #[must_use]
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Handle to a task store on disk. Cheap to clone-by-reference; all I/O is
/// explicit so callers decide when to hit the filesystem.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open (creating if needed) a store rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| StoreError::io(&root, e))?;
        Ok(Self { root })
    }

    /// Resolve the store from `TTM_HOME`, falling back to `~/.local/share/ttm`.
    pub fn from_env() -> Result<Self> {
        Self::open(default_root())
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn tasks_path(&self) -> PathBuf {
        self.root.join(TASKS_FILE)
    }

    #[must_use]
    pub fn events_path(&self) -> PathBuf {
        self.root.join(EVENTS_FILE)
    }

    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// True once the store directory exists.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.tasks_path().exists()
    }

    // -- reading ------------------------------------------------------------

    /// Read `tasks.json`. A missing file yields an empty set; an unparsable one
    /// is an error (we never silently discard the user's tasks).
    pub fn load(&self) -> Result<TaskSet> {
        let path = self.tasks_path();
        match fs::read_to_string(&path) {
            Ok(text) => self.parse_tasks(&path, &text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(TaskSet::new()),
            Err(e) => Err(StoreError::io(&path, e)),
        }
    }

    fn parse_tasks(&self, path: &Path, text: &str) -> Result<TaskSet> {
        if text.trim().is_empty() {
            return Ok(TaskSet::new());
        }
        let set: TaskSet = serde_json::from_str(text).map_err(|e| StoreError::parse(path, e))?;
        if set.schema_version > crate::model::SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: set.schema_version,
                supported: crate::model::SCHEMA_VERSION,
            });
        }
        Ok(set)
    }

    /// Read the tail of the event log, newest last.
    pub fn read_events(&self, limit: Option<usize>) -> Result<Vec<Event>> {
        let path = self.events_path();
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StoreError::io(&path, e)),
        };
        let mut out = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|e| StoreError::io(&path, e))?;
            if line.trim().is_empty() {
                continue;
            }
            // A truncated final line (writer killed mid-append) must not break reads.
            match serde_json::from_str::<Event>(&line) {
                Ok(ev) => out.push(ev),
                Err(_) => continue,
            }
        }
        if let Some(n) = limit {
            let start = out.len().saturating_sub(n);
            out = out.split_off(start);
        }
        Ok(out)
    }

    /// History for one task, oldest first.
    pub fn events_for(&self, id: &TaskId) -> Result<Vec<Event>> {
        Ok(self
            .read_events(None)?
            .into_iter()
            .filter(|e| e.task_id.as_ref() == Some(id))
            .collect())
    }

    // -- writing ------------------------------------------------------------

    /// Atomically replace `tasks.json`: write a sibling temp file, fsync it, then
    /// `rename` over the target. Readers therefore only ever see a complete file.
    pub fn save(&self, set: &TaskSet) -> Result<()> {
        let errs = crate::model::validate_graph(set);
        if let Some(first) = errs.first() {
            return Err(StoreError::Validation(first.clone()));
        }

        let path = self.tasks_path();
        let mut set = set.clone();
        set.schema_version = crate::model::SCHEMA_VERSION;
        set.updated_at = Utc::now();

        let mut body = serde_json::to_vec_pretty(&set).map_err(|e| StoreError::parse(&path, e))?;
        body.push(b'\n');

        let tmp = self.temp_path("tasks");
        {
            let mut f = File::create(&tmp).map_err(|e| StoreError::io(&tmp, e))?;
            f.write_all(&body).map_err(|e| StoreError::io(&tmp, e))?;
            f.sync_all().map_err(|e| StoreError::io(&tmp, e))?;
        }
        fs::rename(&tmp, &path).map_err(|e| StoreError::io(&path, e))?;
        self.sync_dir();
        Ok(())
    }

    /// Append one event. Each line is written and flushed so a `tail -f` by an
    /// agent sees it immediately.
    pub fn append_event(&self, event: &Event) -> Result<()> {
        let path = self.events_path();
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| StoreError::io(&path, e))?;
        let mut line = event.to_line();
        line.push('\n');
        f.write_all(line.as_bytes())
            .map_err(|e| StoreError::io(&path, e))?;
        f.flush().map_err(|e| StoreError::io(&path, e))?;
        Ok(())
    }

    pub fn append_events(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let path = self.events_path();
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| StoreError::io(&path, e))?;
        let mut buf = String::new();
        for ev in events {
            buf.push_str(&ev.to_line());
            buf.push('\n');
        }
        f.write_all(buf.as_bytes())
            .map_err(|e| StoreError::io(&path, e))?;
        f.flush().map_err(|e| StoreError::io(&path, e))?;
        Ok(())
    }

    // -- locking ------------------------------------------------------------

    /// Acquire the exclusive store lock, waiting up to [`LOCK_TIMEOUT`].
    pub fn lock(&self) -> Result<StoreLock> {
        let path = self.lock_path();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| StoreError::io(&path, e))?;

        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(StoreLock { file, path }),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(StoreError::Locked { path });
                    }
                    std::thread::sleep(LOCK_POLL);
                }
                Err(e) => return Err(StoreError::io(&path, e)),
            }
        }
    }

    /// Lock, then read. The returned guard must be held for the whole
    /// read-modify-write cycle.
    pub fn lock_and_load(&self) -> Result<(StoreLock, TaskSet)> {
        let guard = self.lock()?;
        match self.load() {
            Ok(set) => Ok((guard, set)),
            Err(e) => {
                drop(guard);
                Err(e)
            }
        }
    }

    /// Read-modify-write under the lock, with automatic event logging.
    ///
    /// The closure receives the current task set and a sink for events. If it
    /// returns `Err`, nothing is written — no partial saves.
    ///
    /// ```no_run
    /// # use ttm::store::{Event, EventKind, Store};
    /// # use ttm::model::Task;
    /// let store = Store::open("/tmp/ttm-demo")?;
    /// let created: Task = store.mutate(|set, events| {
    ///     let t = Task::new("write docs", chrono::Utc::now());
    ///     set.insert(t.clone())?;
    ///     events.push(Event::new(EventKind::TaskCreated, Some(t.id)).with_title(&t.title));
    ///     Ok(t)
    /// })?;
    /// assert_eq!(store.load()?.tasks.len(), 1);
    /// # Ok::<(), ttm::store::StoreError>(())
    /// ```
    pub fn mutate<T>(
        &self,
        f: impl FnOnce(&mut TaskSet, &mut Vec<Event>) -> Result<T>,
    ) -> Result<T> {
        let (guard, mut set) = self.lock_and_load()?;
        let mut events: Vec<Event> = Vec::new();

        let out = f(&mut set, &mut events);

        match out {
            Ok(value) => {
                self.save(&set)?;
                self.append_events(&events)?;
                drop(guard);
                Ok(value)
            }
            Err(e) => {
                drop(guard);
                Err(e)
            }
        }
    }

    /// Convenience wrapper for read-only inspection: lock, load, drop lock.
    pub fn snapshot(&self) -> Result<TaskSet> {
        let (guard, set) = self.lock_and_load()?;
        drop(guard);
        Ok(set)
    }

    // -- helpers ------------------------------------------------------------

    fn temp_path(&self, tag: &str) -> PathBuf {
        self.root
            .join(format!(".{tag}.{}.tmp", Uuid::now_v7().simple()))
    }

    /// Durability for the rename itself. Best-effort: some filesystems refuse
    /// to open a directory for sync.
    fn sync_dir(&self) {
        if let Ok(dir) = File::open(&self.root) {
            let _ = dir.sync_all();
        }
    }

    /// Modification time of `tasks.json`, for detecting external edits.
    #[must_use]
    pub fn mtime(&self) -> Option<std::time::SystemTime> {
        fs::metadata(self.tasks_path()).ok()?.modified().ok()
    }

    /// Best-effort copy of a corrupt `tasks.json` aside, so a user (or agent)
    /// can inspect it. Called before any destructive write.
    pub fn backup(&self) -> Result<Option<PathBuf>> {
        let path = self.tasks_path();
        if !path.exists() {
            return Ok(None);
        }
        let dest = self
            .root
            .join(format!("tasks.broken.{}.json", Utc::now().timestamp()));
        fs::copy(&path, &dest).map_err(|e| StoreError::io(&dest, e))?;
        Ok(Some(dest))
    }
}

/// RAII guard that releases the advisory lock on drop.
#[derive(Debug)]
pub struct StoreLock {
    file: File,
    path: PathBuf,
}

impl StoreLock {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        if let Err(e) = FileExt::unlock(&self.file) {
            // Nothing useful to do; the OS releases the lock when the fd closes.
            if e.kind() != io::ErrorKind::NotConnected {
                eprintln!(
                    "ttm: warning: could not unlock {}: {e}",
                    self.path.display()
                );
            }
        }
    }
}

/// `$TTM_HOME`, or `~/.local/share/ttm`.
#[must_use]
pub fn default_root() -> PathBuf {
    if let Some(p) = std::env::var_os("TTM_HOME")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/share/ttm");
    }
    PathBuf::from(".ttm")
}
