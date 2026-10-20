//! Bulk task creation from JSON.
//!
//! `ttm import` reads a JSON document describing one or more tasks and creates
//! them in a single atomic write. It exists because the flag-based `add` is a
//! poor fit for automation: an agent holding structured data has to flatten it
//! into argv, cannot express subtasks in one shot, and cannot tell whether a
//! partially-applied batch is acceptable.
//!
//! Two shapes are accepted:
//!
//! * A **batch** — `{"tasks": [ ... ]}`, optionally with a default `parent`.
//! * A **single task** — any bare task object, for symmetry with `add --json`.
//!
//! Within a task, `subtasks` nests one level, matching the model's one-level
//! nesting rule. Within a batch, `"id"` is an *input* label that later siblings
//! can reference as their `parent`, so a document can express a tree without
//! knowing any UUIDs in advance.
//!
//! Everything is validated before the store lock is taken, so a malformed
//! document leaves the store untouched and reports the offending path.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{
    Attachment, AttachmentKind, Priority, Status, Task, TaskId, TaskSet, ValidationError,
};

/// Top-level document accepted by [`parse`].
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Document {
    /// `{"tasks": [...]}`, with optional defaults applied to every task.
    Batch(Batch),
    /// A bare task object.
    Single(Box<TaskSpec>),
}

impl Document {
    /// Tasks in the document, with batch defaults folded in.
    ///
    /// A bare task object is normalised into a batch of one so that every
    /// reported path looks the same, whatever shape the caller wrote.
    #[must_use]
    pub fn into_specs(self) -> Vec<TaskSpec> {
        match self {
            Self::Batch(b) => b
                .tasks
                .into_iter()
                .map(|mut t| {
                    if t.parent.is_none() {
                        t.parent = b.parent.clone();
                    }
                    t
                })
                .collect(),
            Self::Single(t) => vec![*t],
        }
    }
}

/// A batch of tasks.
#[derive(Debug, Clone, Deserialize)]
pub struct Batch {
    /// Default parent for tasks that do not name one.
    #[serde(default)]
    pub parent: Option<String>,
    /// The tasks to create, parents before their children.
    pub tasks: Vec<TaskSpec>,
}

/// One task as written by a caller.
///
/// Every field beyond `title` is optional. Dates accept the same human forms as
/// `--due` (`+3d`, `today`, `2026-11-01T14:30`) rather than forcing the caller
/// to pre-compute UTC, because the whole point is to hand the tool structured
/// data without doing its job for it. Estimates accept `"4h"` or a plain number
/// of minutes.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TaskSpec {
    /// Required, non-empty.
    pub title: String,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<DateSpec>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<EstimateSpec>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentSpec>,

    /// Parent: an id prefix, an exact title, or an `"id"` label declared by a
    /// sibling earlier in the same document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,

    /// Nested children. One level deep, matching the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<TaskSpec>,

    /// A caller-chosen label other entries in this document may reference as
    /// their `parent`. Never persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Skipped silently, so re-running an unchanged document is idempotent
    /// rather than duplicating work.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip_if_exists: bool,
}

/// A deadline, in any form `parse_datetime` accepts.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DateSpec {
    /// A string: `+3d`, `today`, `eod`, `2026-11-01`, `2026-11-01T14:30`.
    Text(String),
    /// Minutes from now, for callers that prefer arithmetic to parsing.
    OffsetMinutes(i64),
}

/// An estimate, as a human string or a plain number of minutes.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum EstimateSpec {
    /// `4h`, `90m`, `2d`, `1d 4h`.
    Text(String),
    Minutes(u32),
}

/// One attachment.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AttachmentSpec {
    /// `url` or `pdf`. Inferred from `target` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The URL, or the path to a PDF.
    pub target: String,
    /// Display name.
    #[serde(default)]
    pub label: String,
}

/// A problem with the document, located well enough to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportError {
    /// Where in the document, e.g. `tasks[2].subtasks[0].due`.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for ImportError {}

/// Parse a document from JSON text.
pub fn parse(text: &str) -> Result<Document, String> {
    serde_json::from_str(text).map_err(|e| format!("could not parse JSON: {e}"))
}

/// A task that has been validated and given an id, ready to insert.
#[derive(Debug, Clone)]
pub struct Planned {
    pub task: Task,
    /// Id of the parent, resolved within the document or against the store.
    pub parent: Option<TaskId>,
}

/// Turn a document into concrete tasks.
///
/// `resolve` maps a parent reference to an id. It is called for references not
/// satisfied by a sibling `id` label in the same document, which is how a task
/// attaches to something already in the store. Returning `None` for a reference
/// the caller gave is an error, never a silently dropped parent.
///
/// Returns every task, parents before children, so the caller can insert them in
/// one pass. Validation happens here, before any lock is taken.
pub fn plan(
    doc: Document,
    now: DateTime<Utc>,
    mut resolve: impl FnMut(&str) -> Option<TaskId>,
) -> Result<Vec<Planned>, ImportError> {
    let specs = doc.into_specs();
    let mut out: Vec<Planned> = Vec::new();
    // Labels declared in this document, so siblings can reference each other
    // without the caller knowing a UUID in advance.
    let mut labels: HashMap<String, TaskId> = HashMap::new();
    let mut titles: HashMap<String, TaskId> = HashMap::new();

    for (i, spec) in specs.iter().enumerate() {
        plan_one(
            spec,
            &format!("tasks[{i}]"),
            now,
            &mut resolve,
            &mut labels,
            &mut titles,
            None,
            &mut out,
        )?;
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn plan_one(
    spec: &TaskSpec,
    path: &str,
    now: DateTime<Utc>,
    resolve: &mut impl FnMut(&str) -> Option<TaskId>,
    labels: &mut HashMap<String, TaskId>,
    titles: &mut HashMap<String, TaskId>,
    // Set when this spec was reached through `subtasks`: the parent to attach
    // to, which always wins over anything the child names itself.
    forced_parent: Option<TaskId>,
    out: &mut Vec<Planned>,
) -> Result<(), ImportError> {
    // A nested task is a child; a top-level one may still name a parent.
    let mut task = Task::new(spec.title.clone(), now);

    if let Some(d) = &spec.due {
        task.due_at = Some(parse_due(d, now).ok_or_else(|| ImportError {
            path: format!("{path}.due"),
            message: format!(
                "could not parse due date {:?}; try 2026-11-01, 2026-11-01T14:30, +3d, today, or eod",
                d.raw()
            ),
        })?);
    }
    if let Some(e) = &spec.estimate {
        task.estimate_minutes = Some(parse_estimate(e).ok_or_else(|| ImportError {
            path: format!("{path}.estimate"),
            message: format!(
                "could not parse estimate {}; try 4h, 90m, 2d, 1d 4h, or bare minutes",
                e.raw()
            ),
        })?);
    }
    task.status = match &spec.status {
        Some(s) => Status::parse(s).ok_or_else(|| ImportError {
            path: format!("{path}.status"),
            message: format!(
                "unknown status {s:?}; expected one of: todo, doing, blocked, done, cancelled"
            ),
        })?,
        None => Status::Todo,
    };
    task.priority = match &spec.priority {
        Some(p) => Priority::parse(p).ok_or_else(|| ImportError {
            path: format!("{path}.priority"),
            message: format!("unknown priority {p:?}; expected one of: low, normal, high, urgent"),
        })?,
        None => Priority::Normal,
    };
    task.description = spec.description.clone();
    task.tags = spec.tags.clone();
    if task.status == Status::Doing {
        task.started_at = Some(now);
    }

    for (j, a) in spec.attachments.iter().enumerate() {
        let apath = format!("{path}.attachments[{j}]");
        let mut att = match a.kind.as_deref() {
            Some("url") => Attachment::url(a.target.clone(), a.label.clone()),
            Some("pdf") => Attachment::pdf(a.target.clone(), a.label.clone()),
            Some(other) => {
                return Err(ImportError {
                    path: format!("{apath}.kind"),
                    message: format!("unknown attachment kind {other:?}; expected url or pdf"),
                });
            }
            // Infer from the target so the common case needs no `kind` at all.
            None if a.target.contains("://") || a.target.starts_with("mailto:") => {
                Attachment::url(a.target.clone(), a.label.clone())
            }
            None => Attachment::pdf(a.target.clone(), a.label.clone()),
        };
        att.added_at = now;
        att.validate().map_err(|e| ImportError {
            path: apath,
            message: e.to_string(),
        })?;
        task.attachments.push(att);
    }

    // Resolve the parent. Nesting is one level, so a nested child inherits its
    // parent's id and any `parent` it names itself is ignored rather than
    // creating a third level.
    let parent = match forced_parent {
        Some(id) => Ok(Some(id)),
        None => match &spec.parent {
            None => Ok(None),
            Some(r) => match labels.get(r) {
                Some(id) => Ok(Some(*id)),
                None => resolve(r).map(Some).ok_or_else(|| ImportError {
                    path: format!("{path}.parent"),
                    message: format!("no task matches parent reference {r:?}"),
                }),
            },
        },
    };
    let parent = parent?;
    task.parent_id = parent;

    task.validate().map_err(|e| ImportError {
        path: path.to_string(),
        message: e.to_string(),
    })?;

    if let Some(label) = &spec.id {
        labels.insert(label.clone(), task.id);
    }
    titles.insert(spec.title.clone(), task.id);

    let id = task.id;
    out.push(Planned { task, parent });

    for (k, child) in spec.subtasks.iter().enumerate() {
        plan_one(
            child,
            &format!("{path}.subtasks[{k}]"),
            now,
            resolve,
            labels,
            titles,
            Some(id),
            out,
        )?;
    }
    Ok(())
}

impl DateSpec {
    fn raw(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::OffsetMinutes(n) => format!("{n}m"),
        }
    }
}

impl EstimateSpec {
    fn raw(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Minutes(n) => n.to_string(),
        }
    }
}

fn parse_due(spec: &DateSpec, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match spec {
        DateSpec::Text(s) => crate::model::parse_datetime(s, now),
        DateSpec::OffsetMinutes(n) => now.checked_add_signed(chrono::Duration::minutes(*n)),
    }
}

fn parse_estimate(spec: &EstimateSpec) -> Option<u32> {
    match spec {
        EstimateSpec::Text(s) => crate::model::parse_estimate_minutes(s),
        EstimateSpec::Minutes(n) => Some(*n),
    }
}

/// Reject a planned set that would break the one-level nesting rule.
///
/// The existing store is needed as well as the planned tasks: a task may take
/// its parent from the store rather than from this document, and validating the
/// planned set alone would report that parent as missing.
///
/// `Planned::parent` is authoritative — it is what the caller will insert — so
/// it is written back onto the task before validating.
pub fn check_graph(planned: &[Planned], existing: &TaskSet) -> Result<(), ValidationError> {
    let mut set = existing.clone();
    for p in planned {
        let mut t = p.task.clone();
        t.parent_id = p.parent;
        set.tasks.push(t);
    }
    let errors = crate::model::validate_graph(&set);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors[0].clone())
    }
}

/// Titles already in the store, used by `skip_if_exists`.
#[must_use]
pub fn existing_titles(existing: &TaskSet) -> Vec<String> {
    existing.tasks.iter().map(|t| t.title.clone()).collect()
}

/// The kind an attachment will be stored as, for documentation and tests.
#[must_use]
pub fn attachment_kind(spec: &AttachmentSpec) -> AttachmentKind {
    match spec.kind.as_deref() {
        Some("url") => AttachmentKind::Url,
        _ => AttachmentKind::Pdf,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).expect("valid timestamp")
    }

    fn plan_str(json: &str) -> Result<Vec<Planned>, ImportError> {
        let doc = parse(json).expect("parses");
        plan(doc, now(), |_| None)
    }

    #[test]
    fn a_bare_task_object_is_accepted() {
        let out = plan_str(r#"{"title":"Solo"}"#).expect("planned");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].task.title, "Solo");
        assert_eq!(out[0].task.status, Status::Todo);
    }

    #[test]
    fn a_batch_default_parent_applies_to_every_task() {
        // An unresolvable default is an error, not something to skip quietly:
        // dropping it would create tasks at the wrong level.
        let err = plan_str(
            r#"{"parent":"Epic","tasks":[{"title":"One"},{"title":"Two","priority":"high"}]}"#,
        )
        .expect_err("unresolvable default parent is reported");
        assert_eq!(err.path, "tasks[0].parent");

        let mut set = TaskSet::new();
        let epic = Task::new(String::from("Epic"), now());
        set.tasks.push(epic.clone());
        let doc = parse(
            r#"{"parent":"Epic","tasks":[{"title":"One"},{"title":"Two","priority":"high"}]}"#,
        )
        .unwrap();
        let out = plan(doc, now(), |r| {
            set.resolve(r)
                .ok()
                .and_then(|id| set.get(&id).map(|t| t.id))
        })
        .expect("planned");
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].task.priority, Priority::High);
        assert!(out.iter().all(|p| p.parent == Some(epic.id)));

        // An explicit parent overrides the batch default, including when it is
        // unknown -- the caller's own value wins, and its error is reported.
        let doc = parse(r#"{"parent":"Epic","tasks":[{"title":"Own","parent":"Other"}]}"#).unwrap();
        let err = plan(doc, now(), |r| {
            set.resolve(r)
                .ok()
                .and_then(|id| set.get(&id).map(|t| t.id))
        })
        .expect_err("explicit unknown parent wins and is reported");
        assert_eq!(err.path, "tasks[0].parent");
    }

    #[test]
    fn nested_subtasks_are_planned_after_their_parent() {
        let out =
            plan_str(r#"{"title":"Parent","subtasks":[{"title":"Child A"},{"title":"Child B"}]}"#)
                .expect("planned");
        assert_eq!(out.len(), 3);
        assert_eq!(out[1].parent, Some(out[0].task.id));
        assert_eq!(out[2].parent, Some(out[0].task.id));
        check_graph(&out, &TaskSet::new()).expect("valid graph");
    }

    #[test]
    fn a_sibling_label_can_be_used_as_a_parent() {
        let out = plan_str(
            r#"{"tasks":[{"id":"root","title":"Root"},{"title":"Leaf","parent":"root"}]}"#,
        )
        .expect("planned");
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].parent, Some(out[0].task.id));
    }

    #[test]
    fn dates_accept_relative_and_absolute_forms() {
        let out =
            plan_str(r#"{"tasks":[{"title":"A","due":"+3d"},{"title":"B","due":"2026-11-01"}]}"#)
                .expect("planned");
        assert!(out[0].task.due_at.is_some());
        assert_eq!(
            out[1].task.due_at.unwrap().format("%Y-%m-%d").to_string(),
            "2026-11-01"
        );
    }

    #[test]
    fn dates_accept_minute_offsets() {
        let out = plan_str(r#"{"title":"A","due":90}"#).expect("planned");
        let due = out[0].task.due_at.expect("has a due date");
        assert_eq!((due - now()).num_minutes(), 90);
    }

    #[test]
    fn estimates_accept_strings_and_minutes() {
        let out =
            plan_str(r#"{"tasks":[{"title":"A","estimate":"4h"},{"title":"B","estimate":90}]}"#)
                .expect("planned");
        assert_eq!(out[0].task.estimate_minutes, Some(240));
        assert_eq!(out[1].task.estimate_minutes, Some(90));
    }

    #[test]
    fn attachment_kind_is_inferred_from_the_target() {
        let out = plan_str(
            r#"{"tasks":[
                {"title":"A","attachments":[{"target":"https://example.com"}]},
                {"title":"B","attachments":[{"target":"./spec.pdf"}]}
            ]}"#,
        )
        .expect("planned");
        assert_eq!(out[0].task.attachments[0].kind, AttachmentKind::Url);
        assert_eq!(out[1].task.attachments[0].kind, AttachmentKind::Pdf);
    }

    #[test]
    fn a_bad_url_is_rejected_with_its_path() {
        let err = plan_str(r#"{"tasks":[{"title":"A","attachments":[{"target":"nope"}]}]}"#)
            .expect_err("rejected");
        assert_eq!(err.path, "tasks[0].attachments[0]");
    }

    #[test]
    fn errors_name_the_offending_path() {
        let err = plan_str(r#"{"tasks":[{"title":"A"},{"title":"B","due":"soonish"}]}"#)
            .expect_err("rejected");
        assert_eq!(err.path, "tasks[1].due");

        let err = plan_str(r#"{"title":"A","priority":"nope"}"#).expect_err("rejected");
        // A bare object is normalised to a batch of one, so paths are uniform.
        assert_eq!(err.path, "tasks[0].priority");

        let err = plan_str(r#"{"tasks":[{"title":"","subtasks":[{"title":"x"}]}]}"#)
            .expect_err("rejected");
        assert_eq!(err.path, "tasks[0]");
    }

    #[test]
    fn an_unresolvable_parent_is_rejected() {
        let err = plan_str(r#"{"tasks":[{"title":"A","parent":"ghost"}]}"#).expect_err("rejected");
        assert_eq!(err.path, "tasks[0].parent");
        assert!(err.message.contains("ghost"));
    }

    #[test]
    fn the_store_is_used_to_resolve_a_parent() {
        let mut set = TaskSet::new();
        let parent = Task::new("Existing", now());
        set.tasks.push(parent.clone());
        let doc = parse(r#"{"tasks":[{"title":"Child","parent":"Existing"}]}"#).unwrap();
        let out = plan(doc, now(), |r| {
            set.resolve(r)
                .ok()
                .and_then(|id| set.get(&id).map(|t| t.id))
        })
        .expect("planned");
        assert_eq!(out[0].parent, Some(parent.id));
    }

    #[test]
    fn nested_children_cannot_themselves_nest() {
        // A child naming a parent would be a third level; `plan` drops the
        // reference rather than creating an invalid graph.
        let out = plan_str(r#"{"title":"P","subtasks":[{"title":"C","parent":"Other"}]}"#)
            .expect("planned");
        // The child inherits its real parent; the self-declared one is ignored.
        assert_eq!(out[1].parent, Some(out[0].task.id));
        check_graph(&out, &TaskSet::new()).expect("valid graph");
    }

    #[test]
    fn malformed_json_is_reported_clearly() {
        let err = parse("{not json").expect_err("rejected");
        assert!(err.contains("could not parse JSON"));
    }

    #[test]
    fn doing_tasks_record_a_start_time() {
        let out = plan_str(r#"{"title":"A","status":"doing"}"#).expect("planned");
        assert!(out[0].task.started_at.is_some());
    }

    #[test]
    fn check_graph_rejects_a_cycle() {
        let mut a = Task::new(String::from("A"), now());
        let mut b = Task::new(String::from("B"), now());
        a.id = "11111111-1111-7111-8111-111111111111".parse().unwrap();
        b.id = "22222222-2222-7222-8222-222222222222".parse().unwrap();
        // `Planned::parent` is what gets inserted, so the cycle has to be
        // expressed there for the check to see it.
        let (aid, bid) = (a.id, b.id);
        let planned = vec![
            Planned {
                task: a,
                parent: Some(bid),
            },
            Planned {
                task: b,
                parent: Some(aid),
            },
        ];
        assert!(check_graph(&planned, &TaskSet::new()).is_err());

        // A parent that lives in the store is not "missing".
        let mut set = TaskSet::new();
        let parent = Task::new(String::from("Existing"), now());
        set.tasks.push(parent.clone());
        let ok = vec![Planned {
            task: Task::new(String::from("Child"), now()),
            parent: Some(parent.id),
        }];
        assert!(check_graph(&ok, &set).is_ok());
    }
}
