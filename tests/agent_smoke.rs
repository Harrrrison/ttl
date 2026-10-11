//! End-to-end smoke test written the way an AI agent would drive the store:
//! append-only writes, then a read back through the JSON file itself.

use chrono::{Duration, Utc};
use tempfile::TempDir;
use ttm::model::{Attachment, Priority, Status, Task, TaskSet};
use ttm::store::{Event, EventKind, Store};

#[test]
fn agent_can_drive_the_store_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    // Backdated so the overdue task (due -2d) is legitimately past its creation.
    let now = Utc::now() - Duration::days(7);

    // --- write three tasks, one per mutate call, as an agent would ---
    let specs: [(&str, i64, u32, Priority); 3] = [
        ("Review architecture doc", 4, 5, Priority::High),
        ("Write release notes", 1, 2, Priority::Normal),
        ("Fix login timeout", -2, 30, Priority::Urgent),
    ];

    let mut ids = Vec::new();
    for (title, due_days, est_hours, priority) in specs {
        let id = store
            .mutate(|set, events| {
                let mut t = Task::new(title, now).due(now + Duration::days(due_days));
                t.priority = priority;
                t.estimate_minutes = Some(est_hours * 60);
                t.attachments
                    .push(Attachment::url("https://example.com/adr-042", "ADR-042"));
                set.insert(t.clone())?;
                events.push(
                    Event::new(EventKind::TaskCreated, Some(t.id)).with_title(t.title.clone()),
                );
                Ok(t.id)
            })
            .unwrap();
        ids.push(id);
    }

    // --- a subtask, so nesting is exercised too ---
    store
        .mutate(|set, events| {
            let mut child = Task::new("draft summary", now)
                .due(now + Duration::days(3))
                .estimate(Duration::minutes(45));
            child.parent_id = Some(ids[0]);
            child.status = Status::Doing;
            set.insert(child.clone())?;
            events.push(Event::new(EventKind::TaskCreated, Some(child.id)));
            Ok(())
        })
        .unwrap();

    // --- complete one task ---
    store
        .mutate(|set, events| {
            let t = set.get_mut(&ids[1]).expect("task present");
            t.set_status(Status::Done, now);
            let title = t.title.clone();
            events.push(
                Event::new(EventKind::StatusChanged, Some(ids[1]))
                    .with_title(title)
                    .with_patch(serde_json::json!({"status": "done"})),
            );
            Ok(())
        })
        .unwrap();

    // --- read back through the public API ---
    let set = store.load().unwrap();
    assert_eq!(set.tasks.len(), 4);

    assert!(set.get(&ids[0]).is_some());
    assert_eq!(set.children(&ids[0]).len(), 1, "one subtask");
    assert_eq!(set.rollup(&ids[0]).total, 1);
    assert_eq!(set.rolled_estimate_minutes(&ids[0]), 5 * 60 + 45);
    assert_eq!(set.get(&ids[1]).unwrap().status, Status::Done);
    assert!(set.get(&ids[1]).unwrap().completed_at.is_some());

    // An overdue urgent task sorts to the front of the default order.
    let mut ordered = set.tasks.clone();
    ttm::model::default_order(&mut ordered);
    assert_eq!(
        ordered[0].title, "Fix login timeout",
        "overdue urgent first"
    );

    // --- the event log is a complete audit trail ---
    let events = store.read_events(None).unwrap();
    assert_eq!(events.len(), 5, "4 creates + 1 status change");
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event == EventKind::TaskCreated)
            .count(),
        4
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event == EventKind::StatusChanged)
            .count(),
        1
    );

    // --- and the JSON file is valid, self-describing, agent-readable ---
    let raw = std::fs::read_to_string(store.tasks_path()).unwrap();
    let parsed: TaskSet = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed.tasks.len(), 4);

    let first: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&parsed.tasks[0]).unwrap()).unwrap();
    let keys: Vec<&String> = first.as_object().unwrap().keys().collect();
    assert!(keys.contains(&&"id".to_string()));
    assert!(keys.contains(&&"title".to_string()));
    assert!(keys.contains(&&"due_at".to_string()));
    assert!(keys.contains(&&"estimate_minutes".to_string()));

    println!("--- tasks.json ({} bytes) ---", raw.len());
    println!("{}", &raw[..raw.len().min(700)]);
    println!("--- events.jsonl ---");
    println!("{}", std::fs::read_to_string(store.events_path()).unwrap());
}
