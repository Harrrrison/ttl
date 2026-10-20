//! `ttm` — a terminal task manager with a gantt timeline and an
//! agent-readable JSON store.
//!
//! Layering, deliberately strict so the domain is testable without a terminal:
//!
//! * [`model`] — types, validation, human-input parsing. Pure.
//! * [`store`] — persistence, the event log, cross-process locking.
//! * [`cli`] — the agent-facing command-line surface.
//! * [`input`] — key bindings as data, plus the vim-style dispatcher.
//! * [`app`] — state and behaviour; no I/O beyond the store.
//! * [`ui`] — drawing, as pure functions over [`app::App`].
//! * [`tui`] — the only module that touches a real terminal.

pub mod app;
pub mod cli;
pub mod detail;
pub mod editor;
pub mod form;
pub mod import;
pub mod input;
pub mod model;
pub mod store;
pub mod timeline;
pub mod tui;
pub mod ui;

pub use app::{App, Overlay};
pub use cli::{Cli, Exit};
pub use detail::{Detail, OpenOutcome};
pub use form::{Field, FieldKind, Form, FormMode};
pub use input::{Action, Context, Dispatcher};
pub use model::{
    Attachment, AttachmentKind, DueWindow, Filter, Priority, Rollup, SortKey, Status, Summary,
    Task, TaskId, TaskSet, ValidationError,
};
pub use store::{Event, EventKind, Store, StoreError, StoreLock};
pub use timeline::Scale;
