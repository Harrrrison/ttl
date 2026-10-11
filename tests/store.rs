use std::fs;
use std::io::Write;
use std::sync::{Arc, Barrier};

use chrono::{Duration, TimeZone, Utc};
use tempfile::TempDir;
use ttm::model::{Attachment, Priority, Status, Task, TaskSet, ValidationError};
use ttm::store::{Event, EventKind};
use ttm::store::{Store, StoreError};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 7, 9, 0, 0).unwrap()
}

fn temp() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn seeded(store: &Store) -> TaskSet {
    let mut set = TaskSet::new();
    set.insert(Task::new("alpha", now())).unwrap();
    set.insert(Task::new("beta", now()).estimate(Duration::hours(2)))
        .unwrap();
    store.save(&set).unwrap();
    set
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[test]
fn open_creates_the_directory() {
    let tmp = temp();
    let root = tmp.path().join("nested/deeper");
    let store = Store::open(&root).unwrap();

    assert!(root.is_dir());
    assert!(!store.exists(), "tasks.json not written until save");
}

#[test]
fn load_on_missing_file_yields_empty_set() {
    let store = Store::open(temp().path()).unwrap();
    let set = store.load().unwrap();
    assert!(set.tasks.is_empty());
    assert_eq!(set.schema_version, ttm::model::SCHEMA_VERSION);
}

#[test]
fn save_then_load_round_trips() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    let written = seeded(&store);

    let loaded = store.load().unwrap();
    assert_eq!(loaded.tasks.len(), written.tasks.len());
    for t in &loaded.tasks {
        let orig = written.get(&t.id).unwrap();
        assert_eq!(t, orig);
    }
}

#[test]
fn load_treats_empty_file_as_empty_store() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    fs::write(store.tasks_path(), "").unwrap();

    assert!(store.load().unwrap().tasks.is_empty());
}

#[test]
fn load_rejects_corrupt_json_without_destroying_it() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    seeded(&store);
    fs::write(store.tasks_path(), "{ this is not json").unwrap();

    let err = store.load().unwrap_err();
    assert!(matches!(err, StoreError::Parse { .. }), "got {err:?}");

    // The bad file is still on disk, untouched.
    assert_eq!(
        fs::read_to_string(store.tasks_path()).unwrap(),
        "{ this is not json"
    );

    let backup = store.backup().unwrap().expect("backup written");
    assert!(backup.exists());
    assert!(backup.to_string_lossy().contains("broken"));
}

#[test]
fn load_refuses_newer_schema_version() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    fs::write(
        store.tasks_path(),
        r#"{"schema_version":999,"tasks":[],"updated_at":"2026-10-07T09:00:00Z"}"#,
    )
    .unwrap();

    let err = store.load().unwrap_err();
    assert!(matches!(err, StoreError::SchemaTooNew { found: 999, .. }));
}

#[test]
fn save_stamps_updated_at_and_preserves_schema_version() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    let mut set = seeded(&store);
    let before = set.updated_at;

    std::thread::sleep(std::time::Duration::from_millis(10));
    store.save(&set).unwrap();
    set = store.load().unwrap();

    assert!(set.updated_at > before);
    assert_eq!(set.schema_version, ttm::model::SCHEMA_VERSION);
}

#[test]
fn save_rejects_an_invalid_set_and_leaves_the_old_file_intact() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    seeded(&store);
    let good = fs::read_to_string(store.tasks_path()).unwrap();

    let mut bad = TaskSet::new();
    bad.insert(Task::new("", now())).unwrap_err();
    bad.tasks.push(Task::new("  ", now()));

    assert!(matches!(
        store.save(&bad),
        Err(StoreError::Validation(ValidationError::EmptyTitle))
    ));
    assert_eq!(fs::read_to_string(store.tasks_path()).unwrap(), good);
}

#[test]
fn saved_json_is_pretty_printed_and_newline_terminated() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    seeded(&store);

    let raw = fs::read_to_string(store.tasks_path()).unwrap();
    assert!(raw.starts_with("{\n"), "expected indented JSON");
    assert!(raw.ends_with("}\n"), "expected trailing newline");
}

#[test]
fn no_temp_files_are_left_behind_after_save() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    for _ in 0..5 {
        seeded(&store);
    }

    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "left temp files: {leftovers:?}");
}

// ---------------------------------------------------------------------------
// Event log
// ---------------------------------------------------------------------------

#[test]
fn events_append_as_one_line_each() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    let t = Task::new("logged", now());

    store
        .append_event(&Event::new(EventKind::TaskCreated, Some(t.id)).with_title("logged"))
        .unwrap();
    store
        .append_event(&Event::new(EventKind::StatusChanged, Some(t.id)))
        .unwrap();

    let raw = fs::read_to_string(store.events_path()).unwrap();
    assert_eq!(raw.lines().count(), 2);
    assert!(raw.ends_with('\n'));

    let parsed = store.read_events(None).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].event, EventKind::TaskCreated);
    assert_eq!(parsed[0].title.as_deref(), Some("logged"));
}

#[test]
fn read_events_on_missing_file_is_empty() {
    let store = Store::open(temp().path()).unwrap();
    assert!(store.read_events(None).unwrap().is_empty());
}

#[test]
fn read_events_limit_returns_the_tail() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    let id = Task::new("x", now()).id;
    for i in 0..10 {
        store
            .append_event(&Event::new(EventKind::TaskUpdated, Some(id)).with_title(format!("v{i}")))
            .unwrap();
    }

    let tail = store.read_events(Some(3)).unwrap();
    assert_eq!(tail.len(), 3);
    assert_eq!(tail[0].title.as_deref(), Some("v7"));
    assert_eq!(tail[2].title.as_deref(), Some("v9"));
}

#[test]
fn events_for_filters_by_task_and_skips_corrupt_lines() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    let a = Task::new("a", now()).id;
    let b = Task::new("b", now()).id;

    store
        .append_event(&Event::new(EventKind::TaskCreated, Some(a)))
        .unwrap();
    store
        .append_event(&Event::new(EventKind::TaskDeleted, Some(b)))
        .unwrap();

    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(store.events_path())
        .unwrap();
    writeln!(f, "{{ truncated garbage").unwrap();
    drop(f);

    let for_a = store.events_for(&a).unwrap();
    assert_eq!(for_a.len(), 1);
    assert_eq!(for_a[0].event, EventKind::TaskCreated);

    // The whole log still parses; the bad line is skipped, not fatal.
    assert_eq!(store.read_events(None).unwrap().len(), 2);
}

#[test]
fn event_omits_empty_optional_fields() {
    let line = Event::new(EventKind::Note, None).to_line();
    assert!(!line.contains("task_id"));
    assert!(!line.contains("patch"));
    assert!(!line.contains("title"));
}

// ---------------------------------------------------------------------------
// Locking
// ---------------------------------------------------------------------------

#[test]
fn lock_is_exclusive_across_threads() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();

    let guard = store.lock().unwrap();
    assert!(guard.path().exists());

    let dir2 = dir.path().to_path_buf();
    let blocked = std::thread::spawn(move || {
        let other = Store::open(dir2).unwrap();
        other.lock().is_ok()
    });

    assert!(
        !blocked.join().unwrap(),
        "second lock should not be granted"
    );
    drop(guard);
}

#[test]
fn lock_is_released_on_drop_and_usable_again() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    drop(store.lock().unwrap());
    let again = store.lock().expect("lock is reusable after drop");
    drop(again);
}

#[test]
fn lock_and_load_returns_consistent_snapshot() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    seeded(&store);

    let (guard, set) = store.lock_and_load().unwrap();
    assert_eq!(set.tasks.len(), 2);
    drop(guard);
}

// ---------------------------------------------------------------------------
// mutate
// ---------------------------------------------------------------------------

#[test]
fn mutate_persists_task_and_event() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();

    let created = store
        .mutate(|set, events| {
            let t = Task::new("from mutate", now())
                .due(now() + Duration::days(3))
                .estimate(Duration::hours(4));
            set.insert(t.clone())?;
            events.push(Event::new(EventKind::TaskCreated, Some(t.id)).with_title(t.title.clone()));
            Ok(t)
        })
        .unwrap();

    assert_eq!(store.load().unwrap().tasks.len(), 1);
    let set = store.load().unwrap();
    let stored = set.get(&created.id).unwrap();
    assert_eq!(stored.estimate_minutes, Some(240));

    let log = store.read_events(None).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].event, EventKind::TaskCreated);
    assert_eq!(log[0].task_id, Some(created.id));
}

#[test]
fn mutate_writes_nothing_when_the_closure_fails() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    seeded(&store);
    let good = fs::read_to_string(store.tasks_path()).unwrap();

    let result = store.mutate::<()>(|set, events| {
        set.insert(Task::new("half-written", now()))?;
        events.push(Event::new(EventKind::TaskCreated, None));
        Err(StoreError::Validation(ValidationError::EmptyTitle))
    });

    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(store.tasks_path()).unwrap(),
        good,
        "failed mutation must not touch tasks.json"
    );
    assert!(!store.events_path().exists(), "no events on failure");
}

#[test]
fn mutate_is_serialised_so_concurrent_writers_do_not_lose_updates() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    let threads = 8;
    let per_thread = 5;
    let barrier = Arc::new(Barrier::new(threads));

    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for i in 0..per_thread {
                    store
                        .mutate(|set, events| {
                            let task = Task::new(format!("t{t}-{i}"), now());
                            set.insert(task.clone())?;
                            events.push(Event::new(EventKind::TaskCreated, Some(task.id)));
                            Ok(())
                        })
                        .unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    let set = store.load().unwrap();
    assert_eq!(
        set.tasks.len(),
        threads * per_thread,
        "every writer's task must survive"
    );

    let names: std::collections::HashSet<_> = set.tasks.iter().map(|t| &t.title).collect();
    assert_eq!(names.len(), threads * per_thread);
    assert_eq!(store.read_events(None).unwrap().len(), threads * per_thread);
}

#[test]
fn mutate_can_read_the_prior_state_it_is_extending() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();

    store
        .mutate(|set, _| {
            set.insert(Task::new("first", now()))?;
            Ok(())
        })
        .unwrap();

    let second = store
        .mutate(|set, events| {
            let count = set.tasks.len();
            let t = Task::new(format!("second sees {count}"), now());
            set.insert(t.clone())?;
            events.push(Event::new(EventKind::TaskCreated, Some(t.id)));
            Ok(t)
        })
        .unwrap();

    assert_eq!(second.title, "second sees 1");
    assert_eq!(store.load().unwrap().tasks.len(), 2);
}

#[test]
fn mutate_can_remove_cascade_and_log_it() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    let parent = Task::new("parent", now());
    let mut child = Task::new("child", now());
    child.parent_id = Some(parent.id);

    store
        .mutate(|set, _| {
            set.insert(parent.clone())?;
            set.insert(child)?;
            Ok(())
        })
        .unwrap();

    store
        .mutate(|set, events| {
            let removed = set.remove_cascade(&parent.id);
            events.push(
                Event::new(EventKind::TaskDeleted, Some(parent.id))
                    .with_title(format!("removed {} tasks", removed.len())),
            );
            Ok(())
        })
        .unwrap();

    assert!(store.load().unwrap().tasks.is_empty());
    assert!(
        store
            .read_events(None)
            .unwrap()
            .iter()
            .any(|e| e.title.as_deref() == Some("removed 2 tasks"))
    );
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

#[test]
fn mtime_reflects_writes() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();
    assert!(store.mtime().is_none());

    seeded(&store);
    let first = store.mtime().expect("mtime after save");

    std::thread::sleep(std::time::Duration::from_millis(20));
    seeded(&store);
    assert!(store.mtime().expect("mtime") > first);
}

#[test]
fn default_root_honours_ttm_home() {
    // Serialised against other env-touching tests via the single-threaded
    // assertion order in this file.
    let tmp = temp();
    // SAFETY: single-threaded portion of the test binary for env vars; the
    // concurrency test below spawns threads but does not read the env.
    unsafe { std::env::set_var("TTM_HOME", tmp.path()) };
    assert_eq!(ttm::store::default_root(), tmp.path());
    unsafe { std::env::remove_var("TTM_HOME") };
}

#[test]
fn paths_are_named_as_documented() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();

    assert!(store.tasks_path().ends_with("tasks.json"));
    assert!(store.events_path().ends_with("events.jsonl"));
    assert!(store.lock_path().ends_with("store.lock"));
}

#[test]
fn priority_and_status_survive_a_store_round_trip() {
    let dir = temp();
    let store = Store::open(dir.path()).unwrap();

    store
        .mutate(|set, _| {
            let mut t = Task::new("critical", now());
            t.priority = Priority::Urgent;
            t.status = Status::Blocked;
            t.attachments.push(Attachment::pdf("/tmp/spec.pdf", "spec"));
            set.insert(t)?;
            Ok(())
        })
        .unwrap();

    let set = store.load().unwrap();
    let t = &set.tasks[0];
    assert_eq!(t.priority, Priority::Urgent);
    assert_eq!(t.status, Status::Blocked);
    assert_eq!(t.attachments[0].kind, ttm::model::AttachmentKind::Pdf);
}
