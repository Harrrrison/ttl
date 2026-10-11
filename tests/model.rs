use chrono::{Duration, TimeZone, Timelike, Utc};
use ttm::model::*;
use uuid::Uuid;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 7, 9, 0, 0).unwrap()
}

fn at(y: i32, m: u32, d: u32, h: u32) -> chrono::DateTime<Utc> {
    utc(y, m, d, h, 0, 0)
}

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, s).unwrap()
}

// ---------------------------------------------------------------------------
// Serde round-trips
// ---------------------------------------------------------------------------

#[test]
fn task_round_trips_through_json() {
    let mut t = Task::new("Write the Q4 roadmap", now())
        .due(at(2026, 11, 1, 17))
        .estimate(Duration::hours(6))
        .with_tag("work")
        .with_attachment(Attachment::url("https://example.com/spec", "spec"))
        .with_attachment(Attachment::pdf("/Users/me/contract.pdf", "contract"));
    t.description = "Outline **three** themes.".into();
    t.priority = Priority::High;

    let json = serde_json::to_string(&t).unwrap();
    let back: Task = serde_json::from_str(&json).unwrap();

    assert_eq!(t, back);
}

#[test]
fn missing_optional_fields_default_cleanly() {
    // An agent hand-writing a minimal task must still load.
    let id = Uuid::now_v7();
    let json = format!(r#"{{"id":"{id}","title":"minimal","created_at":"2026-10-07T09:00:00Z"}}"#);
    let t: Task = serde_json::from_str(&json).unwrap();

    assert_eq!(t.title, "minimal");
    assert_eq!(t.status, Status::Todo);
    assert_eq!(t.priority, Priority::Normal);
    assert_eq!(t.due_at, None);
    assert_eq!(t.estimate_minutes, None);
    assert!(t.attachments.is_empty());
    assert!(t.tags.is_empty());
    assert!(t.validate().is_ok());
}

#[test]
fn unknown_fields_are_ignored_for_forward_compat() {
    let mut value = serde_json::json!({
        "id": Uuid::now_v7().to_string(),
        "title": "future",
        "created_at": "2026-10-07T09:00:00Z",
        "assignee": "alice",
        "points": 5,
        "colour": "#ff0000",
    });
    let json = value.to_string();
    let t: Task = serde_json::from_str(&json).unwrap();
    assert_eq!(t.title, "future");

    // Removing the unknown keys must not change the outcome.
    let obj = value.as_object_mut().unwrap();
    for key in ["assignee", "points", "colour"] {
        obj.remove(key);
    }
    let stripped: Task = serde_json::from_str(&value.to_string()).unwrap();
    assert_eq!(stripped.id, t.id);
}

#[test]
fn timestamps_are_full_iso8601_utc() {
    let mut t = Task::new("deadlined", now()).due(at(2026, 11, 1, 17));
    t.parent_id = None;
    let json = serde_json::to_string(&t).unwrap();

    assert!(
        json.contains(r#""due_at":"2026-11-01T17:00:00Z""#),
        "{json}"
    );
    assert!(
        json.contains(r#""created_at":"2026-10-07T09:00:00Z""#),
        "{json}"
    );
}

#[test]
fn task_set_round_trips_and_stamps_schema_version() {
    let mut set = TaskSet::new();
    set.insert(Task::new("one", now())).unwrap();
    set.insert(Task::new("two", now())).unwrap();

    let json = serde_json::to_string(&set).unwrap();
    assert!(json.contains(r#""schema_version":1"#));
    assert_eq!(serde_json::from_str::<TaskSet>(&json).unwrap(), set);
}

#[test]
fn status_and_priority_use_readable_serde_names() {
    assert_eq!(
        serde_json::to_string(&Status::Blocked).unwrap(),
        r#""blocked""#
    );
    assert_eq!(
        serde_json::to_string(&Priority::Urgent).unwrap(),
        r#""urgent""#
    );
    let a = Attachment::pdf("/a/b.pdf", "b");
    let json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
    assert_eq!(json["kind"], "pdf");
    assert_eq!(json["label"], "b");
    assert_eq!(json["target"], "/a/b.pdf");
    // serde emits `Z`-suffixed UTC, which is what we assert elsewhere.
    assert!(json["added_at"].as_str().unwrap().ends_with('Z'));

    let u = Attachment::url("https://a.test", "a");
    let json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&u).unwrap()).unwrap();
    assert_eq!(json["kind"], "url");
}

// ---------------------------------------------------------------------------
// Status transitions
// ---------------------------------------------------------------------------

#[test]
fn status_cycle_is_a_closed_loop() {
    let mut s = Status::Todo;
    let mut seen = Vec::new();
    for _ in 0..4 {
        s = s.next();
        seen.push(s);
    }
    assert_eq!(s, Status::Todo, "cycle returns to the start");
    assert_eq!(
        seen,
        vec![Status::Doing, Status::Blocked, Status::Done, Status::Todo]
    );

    // Cancelled also rejoins the cycle.
    assert_eq!(Status::Cancelled.next(), Status::Todo);
}

#[test]
fn moving_to_doing_stamps_start_only_once() {
    let mut t = Task::new("t", now());
    t.set_status(Status::Doing, at(2026, 10, 7, 10));
    assert_eq!(t.started_at, Some(at(2026, 10, 7, 10)));
    assert_eq!(t.completed_at, None);

    let later = at(2026, 10, 9, 15);
    t.set_status(Status::Blocked, later);
    t.set_status(Status::Doing, later);
    assert_eq!(t.started_at, Some(at(2026, 10, 7, 10)));
}

#[test]
fn completing_sets_both_stamps_and_reopening_clears_completion() {
    let mut t = Task::new("t", now());
    let done_at = at(2026, 10, 8, 16);
    t.set_status(Status::Done, done_at);
    assert_eq!(t.completed_at, Some(done_at));
    assert!(t.validate().is_ok());

    t.set_status(Status::Todo, at(2026, 10, 9, 9));
    assert_eq!(t.completed_at, None);
    assert_eq!(t.status, Status::Todo);
}

#[test]
fn updated_at_tracks_every_transition() {
    let mut t = Task::new("t", now());
    let later = at(2026, 10, 20, 12);
    t.set_status(Status::Doing, later);
    assert_eq!(t.updated_at, later);
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[test]
fn empty_title_is_rejected() {
    let mut t = Task::new("   ", now());
    assert_eq!(t.validate(), Err(ValidationError::EmptyTitle));
    t.title = "ok".into();
    assert!(t.validate().is_ok());
}

#[test]
fn overlong_title_reports_actual_length() {
    let mut t = Task::new("x".repeat(501), now());
    assert_eq!(t.validate(), Err(ValidationError::TitleTooLong(501)));
    t.title = "x".repeat(500);
    assert!(t.validate().is_ok());
}

#[test]
fn a_due_date_before_creation_is_allowed() {
    // Back-filling already-overdue work must be representable.
    let t = Task::new("missed the deadline", now()).due(at(2026, 10, 1, 17));
    assert!(t.validate().is_ok());
}

#[test]
fn completion_without_done_status_is_inconsistent() {
    let mut t = Task::new("t", now());
    t.completed_at = Some(at(2026, 10, 8, 9));
    assert_eq!(t.validate(), Err(ValidationError::CompletedWithoutDone));
}

#[test]
fn attachment_validation() {
    assert!(
        Attachment::url("https://example.com", "x")
            .validate()
            .is_ok()
    );
    assert!(Attachment::url("mailto:a@b.com", "x").validate().is_ok());
    assert!(matches!(
        Attachment::url("example.com", "x").validate(),
        Err(ValidationError::MalformedUrl(_))
    ));
    assert!(matches!(
        Attachment::url("://nohost", "x").validate(),
        Err(ValidationError::MalformedUrl(_))
    ));
    assert!(matches!(
        Attachment::pdf("/tmp/notes.txt", "x").validate(),
        Err(ValidationError::NotAPdf(_))
    ));
    assert!(Attachment::pdf("/tmp/report.PDF", "x").validate().is_ok());
    assert!(matches!(
        Attachment::pdf("  ", "x").validate(),
        Err(ValidationError::EmptyAttachmentTarget)
    ));
}

#[test]
fn graph_validation_catches_orphans_self_parents_and_deep_nesting() {
    let parent = Task::new("parent", now());
    let child = Task::new("child", now());
    let mut child = child;
    child.parent_id = Some(parent.id);
    let mut grandchild = Task::new("grandchild", now());
    grandchild.parent_id = Some(child.id);
    let mut orphan = Task::new("orphan", now());
    orphan.parent_id = Some(Uuid::now_v7());
    let mut self_parent = Task::new("selfparent", now());
    self_parent.parent_id = Some(self_parent.id);

    let set = TaskSet {
        schema_version: SCHEMA_VERSION,
        tasks: vec![
            parent.clone(),
            child.clone(),
            grandchild,
            orphan,
            self_parent,
        ],
        updated_at: now(),
    };

    let errs = validate_graph(&set);
    assert!(
        errs.contains(&ValidationError::SelfParent),
        "expected SelfParent, got {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::ParentNotFound(_))),
        "expected ParentNotFound, got {errs:?}"
    );
    assert!(
        errs.contains(&ValidationError::NestingTooDeep {
            child: grandchild_id(&set),
            parent: child.id,
        }) || errs
            .iter()
            .any(|e| matches!(e, ValidationError::NestingTooDeep { .. })),
        "expected NestingTooDeep, got {errs:?}"
    );
}

fn grandchild_id(set: &TaskSet) -> Uuid {
    set.tasks
        .iter()
        .find(|t| t.title == "grandchild")
        .expect("grandchild present")
        .id
}

#[test]
fn valid_single_level_tree_passes() {
    let parent = Task::new("parent", now());
    let mut child = Task::new("child", now());
    child.parent_id = Some(parent.id);

    let set = TaskSet {
        schema_version: SCHEMA_VERSION,
        tasks: vec![parent, child],
        updated_at: now(),
    };
    assert_eq!(validate_graph(&set), Vec::new());
}

// ---------------------------------------------------------------------------
// TaskSet operations
// ---------------------------------------------------------------------------

fn fixture() -> (TaskSet, Task, Task) {
    let parent = Task::new("parent", now());
    let mut child = Task::new("child", now());
    child.parent_id = Some(parent.id);
    child.status = Status::Done;
    child.estimate_minutes = Some(90);

    let mut set = TaskSet::new();
    set.insert(parent.clone()).unwrap();
    set.insert(child.clone()).unwrap();
    (set, parent, child)
}

#[test]
fn tree_exposes_roots_with_children() {
    let (set, parent, child) = fixture();
    let tree = set.tree();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].0.id, parent.id);
    assert_eq!(tree[0].1.len(), 1);
    assert_eq!(tree[0].1[0].id, child.id);
}

#[test]
fn rollup_counts_children() {
    let (set, parent, _) = fixture();
    assert_eq!(set.rollup(&parent.id), Rollup { total: 1, done: 1 });
}

#[test]
fn rolled_estimate_and_due_include_children() {
    let mut parent = Task::new("parent", now());
    parent.due_at = Some(at(2026, 12, 1, 17));
    parent.estimate_minutes = Some(30);
    let mut child = Task::new("child", now());
    child.parent_id = Some(parent.id);
    child.estimate_minutes = Some(90);
    child.due_at = Some(at(2026, 11, 1, 17));

    let mut set = TaskSet::new();
    set.insert(parent.clone()).unwrap();
    set.insert(child).unwrap();

    assert_eq!(set.rolled_estimate_minutes(&parent.id), 120);
    assert_eq!(set.rolled_due_at(&parent.id), Some(at(2026, 11, 1, 17)));
}

#[test]
fn removing_a_parent_cascades_to_children() {
    let (mut set, parent, child) = fixture();
    let removed = set.remove_cascade(&parent.id);

    assert_eq!(removed.len(), 2);
    assert!(removed.contains(&parent.id));
    assert!(removed.contains(&child.id));
    assert!(set.tasks.is_empty());
}

#[test]
fn removing_a_child_leaves_the_parent() {
    let (mut set, parent, child) = fixture();
    let removed = set.remove_cascade(&child.id);

    assert_eq!(removed, vec![child.id]);
    assert_eq!(set.tasks.len(), 1);
    assert_eq!(set.tasks[0].id, parent.id);
}

#[test]
fn resolve_accepts_full_uuid_prefix_and_exact_title() {
    let (set, parent, _) = fixture();
    let full = parent.id.to_string();

    assert_eq!(set.resolve(&full), Ok(parent.id));
    assert_eq!(set.resolve("PARENT"), Ok(parent.id));

    // Both fixture tasks share a creation instant, so the 12-digit timestamp
    // prefix is ambiguous by construction; a longer prefix disambiguates.
    let prefix = &full[..MIN_ID_PREFIX + 1];
    assert!(matches!(
        set.resolve(prefix),
        Err(ResolveError::Ambiguous { .. })
    ));
    // Unique prefixes are rare for ids minted in the same instant, so search
    // for the shortest distinguishing one rather than hardcoding a length.
    let mut resolved = None;
    for len in MIN_ID_PREFIX..=full.len() {
        if full.len() - len < 6 {
            break;
        }
        if let Ok(id) = set.resolve(&full[..len]) {
            resolved = Some(id);
            break;
        }
    }
    assert_eq!(resolved, Some(parent.id));

    // Fewer than 12 hex digits is treated as a title, not an id.
    assert!(matches!(
        set.resolve(&full[..MIN_ID_PREFIX - 1]),
        Err(ResolveError::NotFound(_))
    ));

    assert!(matches!(
        set.resolve("nope"),
        Err(ResolveError::NotFound(_))
    ));
    assert!(matches!(set.resolve("  "), Err(ResolveError::Empty)));
}

#[test]
fn resolve_reports_ambiguous_prefix() {
    let mut set = TaskSet::new();
    // Two ids that share a prefix of at least MIN_ID_PREFIX characters.
    // Two valid v7-shaped ids differing only in the last group.
    let shared = "0192f3a1-b2c3-7000-8000-00000000";
    for tail in ["000a", "000b"] {
        let mut t = Task::new("same title", now());
        t.id = Uuid::parse_str(&format!("{shared}{tail}")).unwrap();
        set.insert(t).unwrap();
    }
    let err = set.resolve(shared).unwrap_err();
    match err {
        ResolveError::Ambiguous { candidates, .. } => assert_eq!(candidates.len(), 2),
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn insert_rejects_duplicates() {
    let mut set = TaskSet::new();
    let t = Task::new("a", now());
    set.insert(t.clone()).unwrap();
    assert_eq!(
        set.insert(t.clone()),
        Err(ValidationError::DuplicateId(t.id))
    );
}

#[test]
fn insert_validates_before_storing() {
    let mut set = TaskSet::new();
    let bad = Task::new("", now());
    assert!(set.insert(bad).is_err());
    assert!(set.tasks.is_empty());
}

// ---------------------------------------------------------------------------
// Estimate parsing and formatting
// ---------------------------------------------------------------------------

#[test]
fn estimate_accepts_bare_minutes_and_units() {
    assert_eq!(parse_estimate_minutes("90"), Some(90));
    assert_eq!(parse_estimate_minutes("90m"), Some(90));
    assert_eq!(parse_estimate_minutes("4h"), Some(240));
    assert_eq!(parse_estimate_minutes("2d"), Some(2880));
    assert_eq!(parse_estimate_minutes("1w"), Some(10_080));
    assert_eq!(parse_estimate_minutes("1.5h"), Some(90));
    assert_eq!(parse_estimate_minutes("  4 H "), Some(240));
    assert_eq!(parse_estimate_minutes("4 h"), Some(240));
}

#[test]
fn estimate_accepts_compound_and_spaced_forms() {
    assert_eq!(parse_estimate_minutes("1d4h"), Some(1680));
    assert_eq!(parse_estimate_minutes("1d 4h"), Some(1680));
    assert_eq!(
        parse_estimate_minutes("1w 2d 3h 30m"),
        Some(10_080 + 2880 + 180 + 30)
    );
}

#[test]
fn estimate_rejects_garbage_and_non_positive() {
    assert_eq!(parse_estimate_minutes(""), None);
    assert_eq!(parse_estimate_minutes("abc"), None);
    assert_eq!(parse_estimate_minutes("0"), None);
    assert_eq!(parse_estimate_minutes("4x"), None);
    assert_eq!(parse_estimate_minutes("-30"), None);
}

#[test]
fn duration_formatting_is_human_readable() {
    assert_eq!(format_duration(Duration::minutes(0)), "0m");
    assert_eq!(format_duration(Duration::minutes(90)), "1h 30m");
    assert_eq!(format_duration(Duration::hours(4)), "4h");
    assert_eq!(format_duration(Duration::hours(24 * 2 + 3)), "2d 3h");
    assert_eq!(format_duration(Duration::weeks(1)), "1w");
}

#[test]
fn estimate_human_uses_format_duration() {
    let t = Task::new("t", now()).estimate(Duration::minutes(150));
    assert_eq!(t.estimate_human().as_deref(), Some("2h 30m"));
    assert_eq!(Task::new("n", now()).estimate_human(), None);
}

// ---------------------------------------------------------------------------
// Due-date parsing
// ---------------------------------------------------------------------------

#[test]
fn bare_date_resolves_to_end_of_day() {
    let parsed = parse_datetime("2026-11-01", now()).expect("parses");
    let local = parsed.with_timezone(&chrono::Local);
    assert_eq!(local.date_naive().to_string(), "2026-11-01");
    assert_eq!(local.hour(), 17);
}

#[test]
fn full_iso_input_is_preserved_exactly() {
    let parsed = parse_datetime("2026-11-01T14:30:00Z", now()).unwrap();
    assert_eq!(parsed, utc(2026, 11, 1, 14, 30, 0));
    assert_eq!(parsed.to_rfc3339(), "2026-11-01T14:30:00+00:00");
}

#[test]
fn offset_input_is_normalised_to_utc() {
    let parsed = parse_datetime("2026-11-01T14:30:00+02:00", now()).unwrap();
    assert_eq!(parsed, utc(2026, 11, 1, 12, 30, 0));
}

#[test]
fn relative_and_keyword_forms() {
    assert_eq!(
        parse_datetime("+3d", now()),
        Some(now() + Duration::days(3))
    );
    assert_eq!(
        parse_datetime("+2h", now()),
        Some(now() + Duration::hours(2))
    );
    assert_eq!(
        parse_datetime("+90m", now()),
        Some(now() + Duration::minutes(90))
    );

    let today = parse_datetime("today", now()).unwrap();
    let tomorrow = parse_datetime("tomorrow", now()).unwrap();
    assert!(tomorrow > today);
    assert_eq!((tomorrow - today).num_hours(), 24);
}

#[test]
fn unparseable_due_returns_none() {
    assert_eq!(parse_datetime("", now()), None);
    assert_eq!(parse_datetime("next tuesday-ish", now()), None);
    assert_eq!(parse_datetime("2026-13-45", now()), None);
}

// ---------------------------------------------------------------------------
// Countdown, summary, ordering
// ---------------------------------------------------------------------------

#[test]
fn countdown_describes_past_and_future() {
    assert_eq!(countdown(now(), now()), "due now");
    assert_eq!(countdown(now() + Duration::minutes(5), now()), "in 5m");
    assert_eq!(countdown(now() + Duration::hours(5), now()), "in 5h");
    assert_eq!(countdown(now() + Duration::hours(30), now()), "in 1d");
    assert_eq!(countdown(now() - Duration::days(5), now()), "overdue 5d");
    assert_eq!(countdown(now() - Duration::days(1), now()), "overdue 1d");
}

#[test]
fn countdown_rounds_rather_than_truncating() {
    // A deadline set to exactly +3d reads as "in 3d" even though the clock has
    // advanced a few microseconds since it was stored.
    let due = now() + Duration::days(3);
    let slightly_later = now() + Duration::microseconds(1);
    assert_eq!(countdown(due, slightly_later), "in 3d");
    assert_eq!(
        countdown(due + Duration::hours(11), slightly_later),
        "in 3d"
    );
    assert_eq!(
        countdown(due + Duration::hours(13), slightly_later),
        "in 4d"
    );
}

#[test]
fn summary_counts_by_status_and_pressure() {
    let mut a = Task::new("a", now());
    a.due_at = Some(now() - Duration::days(2));
    let mut b = Task::new("b", now());
    b.due_at = Some(now() + Duration::hours(4));
    b.estimate_minutes = Some(60);
    let mut c = Task::new("c", now());
    c.status = Status::Done;
    c.due_at = Some(now() - Duration::days(9)); // done, so not "overdue"

    let s = summarize(&[a, b, c], now());
    assert_eq!(s.total, 3);
    assert_eq!(s.todo, 2);
    assert_eq!(s.done, 1);
    assert_eq!(s.overdue, 1);
    assert_eq!(s.due_today, 1);
    assert_eq!(s.total_estimate_minutes, 60);
}

#[test]
fn default_order_puts_open_work_first() {
    let mut done = Task::new("done", now());
    done.status = Status::Done;
    done.due_at = Some(at(2026, 10, 1, 17));
    let mut soon = Task::new("soon", now());
    soon.due_at = Some(at(2026, 10, 10, 17));
    let mut later = Task::new("later", now());
    later.due_at = Some(at(2026, 11, 10, 17));

    let mut v = vec![later, done, soon];
    default_order(&mut v);
    let titles: Vec<&str> = v.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, vec!["soon", "later", "done"]);
}

#[test]
fn sort_keys_cycle_and_parse() {
    assert_eq!(SortKey::Due.next(), SortKey::Priority);
    assert_eq!(SortKey::Estimate.next(), SortKey::Manual);
    assert_eq!(SortKey::parse("due"), Some(SortKey::Due));
    assert_eq!(SortKey::parse("nonsense"), None);
}

#[test]
fn sort_by_priority_is_most_urgent_first() {
    let mut tasks = vec![
        Task {
            priority: Priority::Low,
            ..Task::new("low", now())
        },
        Task {
            priority: Priority::Urgent,
            ..Task::new("urgent", now())
        },
        Task {
            priority: Priority::Normal,
            ..Task::new("normal", now())
        },
    ];
    SortKey::Priority.apply(&mut tasks);
    let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, vec!["urgent", "normal", "low"]);
}

#[test]
fn sort_by_due_places_undated_last() {
    let mut tasks = vec![
        Task::new("none", now()),
        Task::new("late", now()).due(at(2026, 12, 1, 17)),
        Task::new("early", now()).due(at(2026, 10, 9, 17)),
    ];
    SortKey::Due.apply(&mut tasks);
    let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, vec!["early", "late", "none"]);
}

// ---------------------------------------------------------------------------
// Filter
// ---------------------------------------------------------------------------

#[test]
fn filter_by_status_and_text() {
    let mut done = Task::new("ship release", now()).with_tag("work");
    done.status = Status::Done;
    let open = Task::new("write tests", now()).with_tag("work");

    let f = Filter {
        statuses: vec![Status::Done],
        ..Filter::default()
    };
    assert!(f.matches(&done, now()));
    assert!(!f.matches(&open, now()));

    let f = Filter {
        text: Some("TESTS".into()),
        ..Filter::default()
    };
    assert!(f.matches(&open, now()));
    assert!(!f.matches(&done, now()));
}

#[test]
fn filter_by_due_window() {
    let overdue = Task::new("o", now()).due(now() - Duration::days(1));
    let today = Task::new("t", now()).due(now() + Duration::hours(3));
    let week = Task::new("w", now()).due(now() + Duration::days(3));
    let undated = Task::new("u", now());

    let w = DueWindow::Overdue;
    assert!(w.matches(&overdue, now()));
    assert!(!w.matches(&week, now()));

    let w = DueWindow::Week;
    assert!(w.matches(&week, now()));
    assert!(!w.matches(&overdue, now()));

    let w = DueWindow::NoDueDate;
    assert!(w.matches(&undated, now()));
    assert!(!w.matches(&week, now()));

    let w = DueWindow::Today;
    assert!(w.matches(&today, now()));
    assert!(!w.matches(&week, now()));
}

#[test]
fn filter_by_tag_is_conjunctive() {
    let t = Task::new("t", now()).with_tag("work").with_tag("urgent");
    let f = Filter {
        tags: vec!["work".into(), "urgent".into()],
        ..Filter::default()
    };
    assert!(f.matches(&t, now()));

    let f = Filter {
        tags: vec!["work".into(), "nope".into()],
        ..Filter::default()
    };
    assert!(!f.matches(&t, now()));
}

#[test]
fn filter_roots_only_and_parent_scope() {
    let (set, parent, child) = fixture();
    let f = Filter {
        roots_only: true,
        ..Filter::default()
    };
    assert!(f.matches(&parent, now()));
    assert!(!f.matches(&child, now()));

    let f = Filter {
        parent: Some(parent.id),
        ..Filter::default()
    };
    assert!(f.matches(&child, now()));
    assert!(!f.matches(&parent, now()));
    assert_eq!(set.children(&parent.id).len(), 1);
}

#[test]
fn filter_matches_attachment_targets() {
    let t = Task::new("t", now())
        .with_attachment(Attachment::url("https://example.com/design", "design"));
    let f = Filter {
        text: Some("example.com".into()),
        ..Filter::default()
    };
    assert!(f.matches(&t, now()));
}

#[test]
fn due_window_parsing() {
    assert_eq!(DueWindow::parse("today"), Some(DueWindow::Today));
    assert_eq!(DueWindow::parse("OVERDUE"), Some(DueWindow::Overdue));
    assert_eq!(DueWindow::parse("wat"), None);
}

#[test]
fn attachment_counts_group_by_kind() {
    let t = Task::new("t", now())
        .with_attachment(Attachment::url("https://a.test", "a"))
        .with_attachment(Attachment::url("https://b.test", "b"))
        .with_attachment(Attachment::pdf("/tmp/c.pdf", "c"));
    let counts = attachment_counts(&t);
    assert_eq!(counts.get("url"), Some(&2));
    assert_eq!(counts.get("pdf"), Some(&1));
}

#[test]
fn task_ids_are_sortable_and_unique() {
    let a = new_task_id();
    let b = new_task_id();
    assert_ne!(a, b);
    // UUIDv7: string order matches creation order.
    assert!(a.to_string() < b.to_string());
}
