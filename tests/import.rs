//! End-to-end tests for `ttm import`, driven the way an agent would: a JSON
//! document in, tasks out, with the failure modes checked as carefully as the
//! happy path.

use std::io::Write;
use std::process::Command;

use tempfile::TempDir;

/// Run `ttm` against `home` with the given arguments and raw stdin.
fn run(home: &std::path::Path, args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ttm"))
        .arg("-H")
        .arg(home)
        .args(args)
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("ttm runs");
    if let Some(text) = stdin {
        child
            .stdin
            .as_mut()
            .expect("stdin is piped")
            .write_all(text.as_bytes())
            .expect("write stdin");
    }
    let out = child.wait_with_output().expect("ttm finishes");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Count tasks in a store, using the CLI so the test never reads internals.
fn count(home: &std::path::Path) -> usize {
    let (_, out, _) = run(home, &["list", "--ids"], None);
    out.lines().filter(|l| !l.trim().is_empty()).count()
}

fn json(home: &std::path::Path, doc: &str) -> (i32, String, String) {
    run(home, &["import", doc, "-j"], None)
}

#[test]
fn a_batch_with_subtasks_lands_atomically() {
    let tmp = TempDir::new().unwrap();
    let doc = r#"{"tasks":[
        {"title":"Parent","due":"+10d","estimate":"2d","priority":"high",
         "tags":["a","b"],
         "attachments":[{"target":"https://example.com/x","label":"Spec"}],
         "subtasks":[
            {"title":"Child one","due":"+3d","estimate":"1h"},
            {"title":"Child two","estimate":"90m"}
         ]},
        {"title":"Sibling","due":60,"estimate":45,"status":"doing"}
    ]}"#;

    let (code, out, err) = json(tmp.path(), doc);
    assert_eq!(code, 0, "import failed: {err}");
    assert!(out.contains("\"count\": 4"), "expected 4 tasks: {out}");
    assert_eq!(count(tmp.path()), 4);

    // The child really is a child, and the dates really were parsed.
    let (_, tree, _) = run(tmp.path(), &["list", "--tree", "-j"], None);
    assert!(tree.contains("Parent"), "{tree}");
    assert!(tree.contains("Child one"), "{tree}");
    assert_eq!(
        tree.matches("\"parent_id\": null").count(),
        2,
        "only Parent and Sibling are roots: {tree}"
    );
}

#[test]
fn a_bad_task_in_the_batch_creates_nothing() {
    let tmp = TempDir::new().unwrap();
    let doc = r#"{"tasks":[{"title":"Good"},{"title":"Bad","due":"not-a-date"}]}"#;

    let (code, _, err) = json(tmp.path(), doc);
    assert_eq!(code, 2, "a malformed date is a usage error");
    assert!(err.contains("tasks[1].due"), "the path is reported: {err}");
    assert_eq!(count(tmp.path()), 0, "nothing may be written");
}

#[test]
fn errors_name_the_offending_path() {
    let tmp = TempDir::new().unwrap();
    for (doc, expected) in [
        (
            r#"{"tasks":[{"title":"a"},{"title":"b","status":"nope"}]}"#,
            "tasks[1].status",
        ),
        (
            r#"{"tasks":[{"title":"a","priority":"nope"}]}"#,
            "tasks[0].priority",
        ),
        (
            r#"{"tasks":[{"title":"a","attachments":[{"target":"bad"}]}]}"#,
            "tasks[0].attachments[0]",
        ),
        (
            r#"{"tasks":[{"title":"a","parent":"ghost"}]}"#,
            "tasks[0].parent",
        ),
        (
            r#"{"tasks":[{"title":"a","subtasks":[{"title":"","estimate":"x"}]}]}"#,
            "tasks[0].subtasks[0].estimate",
        ),
    ] {
        let (code, _, err) = json(tmp.path(), doc);
        assert_eq!(code, 2, "{doc} should be rejected: {err}");
        assert!(
            err.contains(expected),
            "{doc}: wanted {expected}, got {err}"
        );
    }
    assert_eq!(count(tmp.path()), 0);
}

#[test]
fn a_batch_read_from_stdin_works() {
    let tmp = TempDir::new().unwrap();
    let doc = r#"{"tasks":[{"title":"Piped","due":"+2d"}]}"#;
    let (code, out, err) = run(tmp.path(), &["import", "-", "-j"], Some(doc));
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Piped"), "{out}");
    assert_eq!(count(tmp.path()), 1);
}

#[test]
fn a_single_task_object_is_accepted() {
    let tmp = TempDir::new().unwrap();
    let (code, out, err) = json(tmp.path(), r#"{"title":"Solo","estimate":"4h"}"#);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"estimate_minutes\": 240"), "{out}");
    assert_eq!(count(tmp.path()), 1);
}

#[test]
fn re_importing_with_skip_existing_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let doc = r#"{"tasks":[{"title":"Alpha"},{"title":"Beta"}]}"#;

    let (code, out, _) = run(tmp.path(), &["import", doc, "--skip-existing", "-j"], None);
    assert_eq!(code, 0);
    assert!(out.contains("\"count\": 2"), "{out}");
    assert_eq!(count(tmp.path()), 2);

    // Re-running the same document changes nothing.
    let (code, out, _) = run(tmp.path(), &["import", doc, "--skip-existing", "-j"], None);
    assert_eq!(code, 0);
    assert!(out.contains("\"count\": 0"), "nothing new to create: {out}");
    assert_eq!(count(tmp.path()), 2, "still two tasks");
}

#[test]
fn no_duplicates_rejects_an_existing_title() {
    let tmp = TempDir::new().unwrap();
    let (code, _, _) = json(tmp.path(), r#"{"tasks":[{"title":"Alpha"}]}"#);
    assert_eq!(code, 0);

    let (code, _, err) = run(
        tmp.path(),
        &[
            "import",
            r#"{"tasks":[{"title":"Alpha"}]}"#,
            "--no-duplicates",
            "-j",
        ],
        None,
    );
    assert_eq!(code, 4, "a duplicate title is a store-level failure: {err}");
    assert!(err.contains("Alpha"), "{err}");
    assert_eq!(count(tmp.path()), 1);
}

#[test]
fn a_batch_default_parent_must_resolve() {
    let tmp = TempDir::new().unwrap();

    // Unknown parent: rejected outright, nothing written.
    let (code, _, err) = json(
        tmp.path(),
        r#"{"parent":"Epic","tasks":[{"title":"Child"}]}"#,
    );
    assert_eq!(code, 2, "{err}");
    assert_eq!(count(tmp.path()), 0);

    // Create the parent, then the default applies.
    let (code, out, _) = json(tmp.path(), r#"{"tasks":[{"title":"Epic"}]}"#);
    assert_eq!(code, 0, "{out}");
    let (code, out, err) = json(
        tmp.path(),
        r#"{"parent":"Epic","tasks":[{"title":"Child A"},{"title":"Child B"}]}"#,
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"count\": 2"), "{out}");
    let (_, tree, _) = run(tmp.path(), &["list", "--tree", "-j"], None);
    assert_eq!(
        tree.matches("\"parent_id\": null").count(),
        1,
        "only Epic: {tree}"
    );
}

#[test]
fn a_dry_run_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let doc = r#"{"tasks":[{"title":"Planned","due":"+3d","estimate":"1d"}]}"#;
    let (code, out, err) = run(tmp.path(), &["import", doc, "--dry-run", "-j"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"dry_run\": true"), "{out}");
    assert!(out.contains("Planned"), "the report names the task: {out}");
    assert_eq!(count(tmp.path()), 0, "a dry run must not write");
}

#[test]
fn a_dry_run_reports_the_bad_path_too() {
    let tmp = TempDir::new().unwrap();
    let (code, _, err) = json(tmp.path(), r#"{"tasks":[{"title":"a","due":"nope"}]}"#);
    assert_eq!(code, 2);
    assert!(err.contains("tasks[0].due"), "{err}");
}

#[test]
fn dates_and_estimates_accept_both_forms() {
    let tmp = TempDir::new().unwrap();
    let doc = r#"{"tasks":[
        {"title":"A","due":"+3d","estimate":"4h"},
        {"title":"B","due":"2030-01-01","estimate":90},
        {"title":"C","due":"eod"},
        {"title":"D","due":120}
    ]}"#;
    let (code, out, err) = json(tmp.path(), doc);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json out");
    let created = v["created"].as_array().expect("created array");
    assert_eq!(created.len(), 4);
    assert_eq!(created[0]["estimate_minutes"], 240);
    assert_eq!(created[1]["estimate_minutes"], 90);
    for t in created {
        assert!(t["due_at"].is_string(), "every task got a due date: {t}");
    }
    assert!(
        created[1]["due_at"]
            .as_str()
            .unwrap()
            .starts_with("2030-01-01")
    );
}

#[test]
fn nesting_beyond_one_level_is_rejected() {
    let tmp = TempDir::new().unwrap();
    // `add` refuses to make a subtask a parent; the importer must agree.
    let (code, _, _) = json(tmp.path(), r#"{"tasks":[{"title":"P"}]}"#);
    assert_eq!(code, 0);
    let (code, _, _) = json(tmp.path(), r#"{"tasks":[{"title":"C","parent":"P"}]}"#);
    assert_eq!(code, 0);

    let (code, _, err) = json(
        tmp.path(),
        r#"{"tasks":[{"title":"G","subtasks":[{"title":"C2","subtasks":[{"title":"Too deep"}]}]}]}"#,
    );
    assert_ne!(code, 0, "a third level must be refused: {err}");
}

#[test]
fn a_missing_file_is_a_usage_error() {
    let tmp = TempDir::new().unwrap();
    let (code, _, err) = run(tmp.path(), &["import", "/nope/missing.json", "-j"], None);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("could not read"), "{err}");
}

#[test]
fn malformed_json_is_a_usage_error() {
    let tmp = TempDir::new().unwrap();
    let (code, _, err) = run(tmp.path(), &["import", "{not json", "-j"], None);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("could not parse JSON"), "{err}");
}

#[test]
fn imported_tasks_are_readable_through_the_json_surface() {
    let tmp = TempDir::new().unwrap();
    let doc = r#"{"tasks":[
        {"title":"Readable","due":"+1d","estimate":"2h","priority":"high",
         "tags":["x"],"attachments":[{"target":"https://example.com/y","label":"L"}]}
    ]}"#;
    let (code, _, err) = json(tmp.path(), doc);
    assert_eq!(code, 0, "{err}");

    let (_, out, _) = run(tmp.path(), &["list", "-j"], None);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let t = &v["tasks"][0];
    assert_eq!(t["title"], "Readable");
    assert_eq!(t["priority"], "high");
    assert_eq!(t["estimate_minutes"], 120);
    assert_eq!(t["tags"][0], "x");
    assert_eq!(t["attachments"][0]["label"], "L");
    assert_eq!(t["attachments"][0]["kind"], "url");
}
