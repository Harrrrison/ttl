//! End-to-end tests driving the real `ttm` binary, the way an agent would.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::str::contains;
use serde_json::{Value, json};
use tempfile::TempDir;

/// A store in a throwaway directory plus a handle to the binary.
struct Cli {
    _tmp: TempDir,
    home: std::path::PathBuf,
}

impl Cli {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().to_path_buf();
        Self { _tmp: tmp, home }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("ttm").expect("ttm binary");
        c.env("TTM_HOME", &self.home);
        // Force the pipe-friendly output mode regardless of the test runner's tty.
        c.env_remove("TTM_PLAIN");
        c
    }

    /// Run and assert success, returning stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self
            .cmd()
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).expect("utf8 stdout")
    }

    fn ok_json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.ok(args)).unwrap_or_else(|e| {
            panic!(
                "expected JSON from `ttm {args:?}`: {e}\nraw: {}",
                self.ok(args)
            )
        })
    }

    fn fail(&self, args: &[&str]) -> std::process::Output {
        self.cmd()
            .args(args)
            .assert()
            .failure()
            .get_output()
            .clone()
    }

    /// Stderr from a failing invocation, for message assertions.
    fn fail_stderr(&self, args: &[&str]) -> String {
        String::from_utf8(self.fail(args).stderr).expect("utf8 stderr")
    }

    fn exit_code(&self, args: &[&str]) -> i32 {
        self.fail(args).status.code().unwrap_or(-1)
    }

    fn tasks_json(&self) -> Value {
        let raw = std::fs::read_to_string(self.home.join("tasks.json")).expect("tasks.json");
        serde_json::from_str(&raw).expect("valid tasks.json")
    }

    fn events(&self) -> Vec<Value> {
        let path = self.home.join("events.jsonl");
        match std::fs::read_to_string(path) {
            Ok(raw) => raw
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).expect("each line is JSON"))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Create a task and return its id.
    fn add(&self, title: &str, extra: &[&str]) -> String {
        let mut args = vec!["add", "--title", title, "--json"];
        args.extend_from_slice(extra);
        self.ok_json(&args)["created"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

// ---------------------------------------------------------------------------
// Query commands
// ---------------------------------------------------------------------------

#[test]
fn info_reports_an_empty_store() {
    let cli = Cli::new();
    let out = cli.ok(&["info"]);
    assert!(out.contains("tasks    0"), "{out}");
}

#[test]
fn info_json_exposes_paths() {
    let cli = Cli::new();
    let v = cli.ok_json(&["info", "--json"]);
    assert_eq!(
        v["tasks_path"],
        cli.home.join("tasks.json").to_str().unwrap()
    );
    assert_eq!(
        v["events_path"],
        cli.home.join("events.jsonl").to_str().unwrap()
    );
    assert_eq!(v["schema_version"], 1);
}

#[test]
fn list_is_empty_before_anything_is_added() {
    let cli = Cli::new();
    assert!(cli.ok(&["list"]).contains("no matching tasks"));
}

#[test]
fn list_summary_is_pipe_delimited_and_parseable() {
    let cli = Cli::new();
    cli.add("Write the report", &["--due", "+3d", "--est", "90m"]);
    cli.add("Alpha | pipe", &[]);

    let out = cli.ok(&["list", "--summary"]);
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2);

    for line in &lines {
        assert_eq!(
            line.split('|').count(),
            8,
            "every row must have exactly 8 fields: {line}"
        );
    }

    // The awkward title does not break the field count.
    assert!(lines.iter().any(|l| l.ends_with("Alpha / pipe")), "{out}");

    let report = lines
        .iter()
        .find(|l| l.ends_with("Write the report"))
        .unwrap();
    let f: Vec<&str> = report.split('|').collect();
    assert_eq!(f[0], "todo");
    assert_eq!(f[4], "1h 30m");
    assert!(f[3].starts_with("in "), "countdown: {}", f[3]);
}

#[test]
fn list_json_carries_summary_counts() {
    let cli = Cli::new();
    cli.add("a", &[]);
    let done = cli.add("b", &[]);
    cli.ok(&["done", &done]);

    let v = cli.ok_json(&["list", "--json"]);
    assert_eq!(v["count"], 2);
    assert_eq!(v["summary"]["todo"], 1);
    assert_eq!(v["summary"]["done"], 1);
}

#[test]
fn list_ids_prints_bare_ids() {
    let cli = Cli::new();
    let a = cli.add("a", &[]);
    let b = cli.add("b", &[]);

    let out = cli.ok(&["list", "--ids"]);
    let mut ids: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    ids.sort_unstable();
    let mut expect = vec![a.as_str(), b.as_str()];
    expect.sort_unstable();
    assert_eq!(ids, expect);
}

#[test]
fn list_filters_by_status_tag_and_text() {
    let cli = Cli::new();
    let work = cli.add("Work item", &["--tag", "work"]);
    cli.add("Home item", &["--tag", "home"]);
    cli.ok(&["done", &work]);

    assert_eq!(
        cli.ok(&["list", "--ids", "--status", "done"])
            .lines()
            .count(),
        1
    );
    assert_eq!(
        cli.ok(&["list", "--ids", "--status", "todo"])
            .lines()
            .count(),
        1
    );
    assert_eq!(cli.ok(&["list", "--ids", "--open"]).lines().count(), 1);
    assert_eq!(
        cli.ok(&["list", "--ids", "--tag", "work"]).lines().count(),
        1
    );
    assert_eq!(
        cli.ok(&["list", "--ids", "--search", "HOME"])
            .lines()
            .count(),
        1
    );
    assert_eq!(
        cli.ok(&["list", "--ids", "--search", "nothing"])
            .lines()
            .count(),
        0
    );
}

#[test]
fn list_open_and_all_are_mutually_exclusive() {
    let cli = Cli::new();
    cli.add("a", &[]);
    cli.fail(&["list", "--open", "--all"]);
}

#[test]
fn list_limit_truncates() {
    let cli = Cli::new();
    for i in 0..5 {
        cli.add(&format!("task {i}"), &[]);
    }
    assert_eq!(cli.ok(&["list", "--ids", "-n", "2"]).lines().count(), 2);
}

#[test]
fn list_reverse_flips_the_order() {
    let cli = Cli::new();
    cli.add("a", &["--due", "2026-01-01"]);
    cli.add("b", &["--due", "2026-12-01"]);

    let forward = cli.ok(&["list", "--ids"]);
    let backward = cli.ok(&["list", "--ids", "--reverse"]);
    let f: Vec<&str> = forward.lines().collect();
    let b: Vec<&str> = backward.lines().collect();

    assert_eq!(f.first(), b.last());
}

#[test]
fn overdue_filter_finds_backfilled_past_due_dates() {
    let cli = Cli::new();
    cli.add("late", &["--due", "-2d"]);
    cli.add("soon", &["--due", "+2d"]);

    let ids = cli.ok(&["list", "--ids", "--due", "overdue"]);
    assert_eq!(ids.lines().count(), 1);
    assert!(
        cli.ok(&["list", "--summary", "--due", "overdue"])
            .contains("late")
    );
}

#[test]
fn show_prints_detail_and_json() {
    let cli = Cli::new();
    let id = cli.add(
        "Draft roadmap",
        &[
            "--desc",
            "line one\nline two",
            "--url",
            "https://example.com/spec",
            "--label",
            "spec",
        ],
    );

    let text = cli.ok(&["show", "Draft roadmap"]);
    assert!(text.contains("Draft roadmap"));
    assert!(text.contains("line two"));
    assert!(text.contains("[url] spec"));

    let v = cli.ok_json(&["show", &id, "--json"]);
    assert_eq!(v["title"], "Draft roadmap");
    assert_eq!(v["attachments"].as_array().unwrap().len(), 1);
    assert_eq!(v["attachments"][0]["label"], "spec");
}

#[test]
fn show_rollup_includes_subtasks() {
    let cli = Cli::new();
    let parent = cli.add("parent", &["--est", "2h"]);
    cli.add("kid one", &["--parent", &parent, "--est", "30m"]);
    let kid2 = cli.add("kid two", &["--parent", &parent, "--est", "15m"]);
    cli.ok(&["done", &kid2]);

    let v = cli.ok_json(&["show", "parent", "--rollup", "--json"]);
    assert_eq!(v["subtask_count"], 2);
    assert_eq!(v["subtasks_done"], 1);
    assert_eq!(v["rolled_estimate_minutes"], 120 + 30 + 15);
}

#[test]
fn show_history_lists_events_for_that_task_only() {
    let cli = Cli::new();
    let a = cli.add("alpha", &[]);
    cli.add("beta", &[]);
    cli.ok(&["edit", &a, "--est", "1h"]);

    let v = cli.ok_json(&["show", &a, "--history", "--json"]);
    let history = v["history"].as_array().unwrap();
    assert_eq!(history.len(), 2, "one create, one update");
    assert!(history.iter().all(|e| e["task_id"] == a.as_str()));
}

#[test]
fn summary_counts_the_store() {
    let cli = Cli::new();
    let a = cli.add("a", &["--est", "60m"]);
    cli.add("b", &["--est", "30m", "--due", "-1d"]);
    cli.ok(&["done", &a]);

    let v = cli.ok_json(&["summary", "--json"]);
    assert_eq!(v["summary"]["total"], 2);
    assert_eq!(v["summary"]["done"], 1);
    assert_eq!(v["summary"]["overdue"], 1);
    assert_eq!(v["summary"]["total_estimate_minutes"], 90);
}

#[test]
fn timeline_returns_items_sorted_by_deadline() {
    let cli = Cli::new();
    cli.add("far", &["--due", "+20d", "--est", "2h"]);
    cli.add("near", &["--due", "+1d", "--est", "15m"]);

    let v = cli.ok_json(&["timeline", "--json"]);
    let items = v["items"].as_array().unwrap();
    assert_eq!(items[0]["title"], "near");
    assert_eq!(items[1]["title"], "far");
    assert_eq!(items[0]["estimate_minutes"], 15);
    // Fractional days, so a deadline hours away is not reported as 0 days.
    let days = items[0]["days_remaining"].as_f64().unwrap();
    assert!((days - 1.0).abs() < 0.01, "expected ~1 day, got {days}");
    let hours = items[0]["hours_remaining"].as_i64().unwrap();
    assert!((hours - 24).abs() <= 1, "expected ~24h, got {hours}");
    assert_eq!(items[0]["overdue"], false);
}

#[test]
fn timeline_excludes_completed_and_supports_roots_only() {
    let cli = Cli::new();
    let parent = cli.add("parent", &["--due", "+3d"]);
    cli.add("child", &["--parent", &parent, "--due", "+2d"]);
    let done = cli.add("finished", &["--due", "+1d"]);
    cli.ok(&["done", &done]);

    let all = cli.ok_json(&["timeline", "--json"]);
    let titles: Vec<&str> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"parent"));
    assert!(titles.contains(&"child"));
    assert!(!titles.contains(&"finished"), "completed work is not open");

    let roots = cli.ok_json(&["timeline", "--roots", "--json"]);
    assert_eq!(roots["items"].as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

#[test]
fn add_accepts_every_documented_due_form() {
    for form in [
        "2026-11-01",
        "2026-11-01T14:30",
        "2026-11-01T14:30:00Z",
        "+3d",
        "-1d",
        "today",
        "tomorrow",
        "eod",
        "eow",
    ] {
        let cli = Cli::new();
        cli.add("t", &["--due", form]);
        let v = cli.ok_json(&["list", "--json"]);
        assert_ne!(
            v["tasks"][0]["due_at"],
            Value::Null,
            "`--due {form}` should have produced a timestamp"
        );
    }
}

#[test]
fn add_accepts_every_documented_estimate_form() {
    for (form, minutes) in [
        ("90", 90),
        ("90m", 90),
        ("4h", 240),
        ("2d", 2880),
        ("1d 4h", 1680),
    ] {
        let cli = Cli::new();
        cli.add("t", &["--est", form]);
        let v = cli.ok_json(&["list", "--json"]);
        assert_eq!(
            v["tasks"][0]["estimate_minutes"], minutes,
            "for `--est {form}`"
        );
    }
}

#[test]
fn add_rejects_an_empty_title() {
    let cli = Cli::new();
    assert_eq!(cli.exit_code(&["add", "--title", "  "]), 4);
}

#[test]
fn add_rejects_a_malformed_url_attachment() {
    let cli = Cli::new();
    assert_eq!(
        cli.exit_code(&["add", "--title", "t", "--url", "not-a-url"]),
        2
    );
    assert!(cli.ok(&["list"]).contains("no matching"));
}

#[test]
fn add_labels_pair_positionally_across_urls_and_pdfs() {
    let cli = Cli::new();
    cli.add(
        "t",
        &[
            "--url",
            "https://a.test",
            "--label",
            "alpha",
            "--pdf",
            "/tmp/b.pdf",
            "--label",
            "beta",
        ],
    );

    let v = cli.ok_json(&["show", "t", "--json"]);
    let atts = v["attachments"].as_array().unwrap();
    assert_eq!(atts.len(), 2);
    assert_eq!(atts[0]["label"], "alpha");
    assert_eq!(atts[1]["label"], "beta");
    assert_eq!(atts[1]["kind"], "pdf");
}

#[test]
fn add_with_starting_status_stamps_started_at() {
    let cli = Cli::new();
    cli.add("t", &["--status", "doing"]);
    let v = cli.ok_json(&["show", "t", "--json"]);
    assert_eq!(v["status"], "doing");
    assert!(!v["started_at"].is_null());
}

#[test]
fn add_rejects_an_unknown_parent() {
    let cli = Cli::new();
    assert_eq!(
        cli.exit_code(&["add", "--title", "t", "--parent", "ghost"]),
        3
    );
}

#[test]
fn nesting_is_capped_at_one_level() {
    let cli = Cli::new();
    let a = cli.add("a", &[]);
    let b = cli.add("b", &["--parent", &a]);
    // `b` is already a subtask, so `c` cannot hang off it.
    assert_eq!(cli.exit_code(&["add", "--title", "c", "--parent", &b]), 4);
}

#[test]
fn edit_changes_only_the_named_fields() {
    let cli = Cli::new();
    let id = cli.add("t", &["--est", "2h", "--tag", "keep"]);

    let v = cli.ok_json(&["edit", &id, "--est", "5h", "--json"]);
    let updated = &v["updated"];
    assert_eq!(updated["estimate_minutes"], 300);
    assert_eq!(updated["title"], "t", "untouched field preserved");
    assert_eq!(updated["tags"][0], "keep");
}

#[test]
fn edit_rejects_a_no_op() {
    let cli = Cli::new();
    let id = cli.add("t", &[]);
    assert_eq!(cli.exit_code(&["edit", &id]), 2);
}

#[test]
fn edit_can_add_and_remove_tags() {
    let cli = Cli::new();
    let id = cli.add("t", &["--tag", "a"]);

    cli.ok(&["edit", &id, "--tag", "b"]);
    cli.ok(&["edit", &id, "--tag", "c"]);
    let v = cli.ok_json(&["show", &id, "--json"]);
    assert_eq!(v["tags"], serde_json::json!(["a", "b", "c"]));

    cli.ok(&["edit", &id, "--untag", "b"]);
    let v = cli.ok_json(&["show", &id, "--json"]);
    assert_eq!(v["tags"], serde_json::json!(["a", "c"]));
}

#[test]
fn edit_can_clear_due_and_estimate() {
    let cli = Cli::new();
    let id = cli.add("t", &["--due", "+3d", "--est", "1h"]);

    cli.ok(&["edit", &id, "--no-due", "--no-estimate"]);
    let v = cli.ok_json(&["show", &id, "--json"]);
    assert!(v["due_at"].is_null());
    assert!(v["estimate_minutes"].is_null());
}

#[test]
fn due_and_no_due_conflict() {
    let cli = Cli::new();
    let id = cli.add("t", &["--due", "+3d"]);
    cli.fail(&["edit", &id, "--due", "+4d", "--no-due"]);
}

#[test]
fn done_and_start_transition_status() {
    let cli = Cli::new();
    let id = cli.add("t", &[]);

    let v = cli.ok_json(&["start", &id, "--json"]);
    assert_eq!(v["updated"]["status"], "doing");
    assert!(
        v["updated"]["completed_at"].is_null(),
        "starting work must not stamp a completion"
    );
    assert!(
        !v["updated"]["started_at"].is_null(),
        "start time is recorded"
    );

    let v = cli.ok_json(&["done", &id, "--json"]);
    assert_eq!(v["updated"]["status"], "done");
    assert!(
        !v["updated"]["completed_at"].is_null(),
        "completion is stamped"
    );
}

#[test]
fn link_attaches_urls_and_pdfs() {
    let cli = Cli::new();
    let id = cli.add("t", &[]);

    cli.ok(&[
        "link",
        &id,
        "--url",
        "https://example.com",
        "--label",
        "spec",
    ]);
    cli.ok(&["link", &id, "--pdf", "/tmp/report.pdf"]);

    let v = cli.ok_json(&["show", &id, "--json"]);
    let atts = v["attachments"].as_array().unwrap();
    assert_eq!(atts.len(), 2);
    assert_eq!(atts[0]["kind"], "url");
    assert_eq!(atts[1]["kind"], "pdf");
}

#[test]
fn link_requires_exactly_one_of_url_or_pdf() {
    let cli = Cli::new();
    let id = cli.add("t", &[]);
    assert_eq!(cli.exit_code(&["link", &id]), 2);
    assert_eq!(
        cli.exit_code(&["link", &id, "--url", "https://a.test", "--pdf", "/x.pdf"]),
        2
    );
}

#[test]
fn remove_cascades_to_subtasks() {
    let cli = Cli::new();
    let parent = cli.add("parent", &[]);
    cli.add("child", &["--parent", &parent]);

    let v = cli.ok_json(&["rm", &parent, "--json"]);
    assert_eq!(v["removed"].as_array().unwrap().len(), 2);
    assert!(cli.ok(&["list"]).contains("no matching tasks"));
}

#[test]
fn remove_dry_run_deletes_nothing() {
    let cli = Cli::new();
    let parent = cli.add("parent", &[]);
    cli.add("child", &["--parent", &parent]);

    let v = cli.ok_json(&["rm", &parent, "--dry-run", "--json"]);
    assert_eq!(v["would_delete"].as_array().unwrap().len(), 2);
    assert_eq!(
        cli.ok_json(&["list", "--json"])["count"],
        2,
        "nothing removed"
    );
}

// ---------------------------------------------------------------------------
// Dry-run
// ---------------------------------------------------------------------------

#[test]
fn dry_run_writes_nothing() {
    let cli = Cli::new();

    let v = cli.ok_json(&[
        "add",
        "--title",
        "ghost",
        "--est",
        "1h",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["would_create"]["estimate_minutes"], 60);
    assert!(
        !Path::new(&cli.home.join("tasks.json")).exists(),
        "no file written"
    );

    let id = cli.add("real", &["--est", "1h"]);
    let before = cli.tasks_json();

    cli.ok(&["edit", &id, "--est", "9h", "--dry-run"]);
    cli.ok(&["done", &id, "--dry-run"]);
    cli.ok(&["link", &id, "--url", "https://x.test", "--dry-run"]);
    cli.ok(&["rm", &id, "--dry-run"]);

    assert_eq!(cli.tasks_json(), before, "no dry-run may mutate the store");
}

#[test]
fn dry_run_still_reports_errors() {
    let cli = Cli::new();
    assert_eq!(
        cli.exit_code(&["edit", "ghost", "--est", "1h", "--dry-run"]),
        3
    );
    assert_eq!(
        cli.exit_code(&["add", "--title", "t", "--due", "nonsense", "--dry-run"]),
        2
    );
}

// ---------------------------------------------------------------------------
// Undo
// ---------------------------------------------------------------------------

#[test]
fn undo_restores_the_previous_estimate() {
    let cli = Cli::new();
    let id = cli.add("t", &["--est", "4h"]);

    cli.ok(&["edit", &id, "--est", "8h"]);
    assert_eq!(
        cli.ok_json(&["show", &id, "--json"])["estimate_minutes"],
        480
    );

    cli.ok(&["undo", &id]);
    assert_eq!(
        cli.ok_json(&["show", &id, "--json"])["estimate_minutes"],
        240,
        "undo must restore 4h, not re-apply 8h"
    );
}

#[test]
fn undo_restores_tags_in_bulk() {
    let cli = Cli::new();
    let id = cli.add("t", &["--tag", "a", "--tag", "b"]);

    cli.ok(&["edit", &id, "--untag", "a"]);
    assert_eq!(cli.ok_json(&["show", &id, "--json"])["tags"], json!(["b"]));

    cli.ok(&["undo", &id]);
    assert_eq!(
        cli.ok_json(&["show", &id, "--json"])["tags"],
        json!(["a", "b"])
    );
}

#[test]
fn undo_restores_a_status_change() {
    let cli = Cli::new();
    let id = cli.add("t", &[]);

    cli.ok(&["start", &id]);
    assert_eq!(cli.ok_json(&["show", &id, "--json"])["status"], "doing");

    cli.ok(&["undo", &id]);
    assert_eq!(cli.ok_json(&["show", &id, "--json"])["status"], "todo");
}

#[test]
fn undo_removes_an_added_attachment() {
    let cli = Cli::new();
    let id = cli.add("t", &[]);

    cli.ok(&["link", &id, "--url", "https://x.test"]);
    assert_eq!(
        cli.ok_json(&["show", &id, "--json"])["attachments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    cli.ok(&["undo", &id]);
    assert_eq!(
        cli.ok_json(&["show", &id, "--json"])["attachments"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn undo_walks_back_several_steps() {
    let cli = Cli::new();
    let id = cli.add("t", &["--est", "1h"]);

    cli.ok(&["edit", &id, "--est", "2h"]);
    cli.ok(&["edit", &id, "--est", "3h"]);
    assert_eq!(
        cli.ok_json(&["show", &id, "--json"])["estimate_minutes"],
        180
    );

    cli.ok(&["undo", &id, "-n", "1"]);
    assert_eq!(
        cli.ok_json(&["show", &id, "--json"])["estimate_minutes"],
        120
    );

    cli.ok(&["undo", &id, "-n", "2"]);
    assert_eq!(
        cli.ok_json(&["show", &id, "--json"])["estimate_minutes"],
        60
    );
}

#[test]
fn undo_is_scoped_to_the_named_task() {
    let cli = Cli::new();
    let a = cli.add("a", &["--est", "1h"]);
    let b = cli.add("b", &["--est", "2h"]);

    cli.ok(&["edit", &a, "--est", "5h"]);
    cli.ok(&["edit", &b, "--est", "6h"]);

    // Undoing `a` must leave `b` alone.
    cli.ok(&["undo", &a]);
    assert_eq!(cli.ok_json(&["show", &a, "--json"])["estimate_minutes"], 60);
    assert_eq!(
        cli.ok_json(&["show", &b, "--json"])["estimate_minutes"],
        360
    );
}

#[test]
fn undo_dry_run_reports_without_writing() {
    let cli = Cli::new();
    let id = cli.add("t", &["--est", "1h"]);
    cli.ok(&["edit", &id, "--est", "9h"]);

    let v = cli.ok_json(&["undo", &id, "--dry-run", "--json"]);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["would_update"]["estimate_minutes"], 60);
    assert_eq!(
        cli.ok_json(&["show", &id, "--json"])["estimate_minutes"],
        540
    );
}

#[test]
fn undo_is_an_append_only_audit_trail() {
    let cli = Cli::new();
    let id = cli.add("t", &["--est", "1h"]);
    cli.ok(&["edit", &id, "--est", "9h"]);
    let before = cli.events().len();
    cli.ok(&["undo", &id]);

    let events = cli.events();
    assert_eq!(events.len(), before + 1, "the undo is itself recorded");
    assert_eq!(
        events.last().unwrap()["patch"]["reverted_fields"],
        json!(["estimate_minutes"])
    );
}

// ---------------------------------------------------------------------------
// Event log
// ---------------------------------------------------------------------------

#[test]
fn every_mutation_writes_an_event() {
    let cli = Cli::new();
    let id = cli.add("t", &["--est", "1h"]);
    cli.ok(&["edit", &id, "--est", "2h"]);
    cli.ok(&["start", &id]);
    cli.ok(&["link", &id, "--url", "https://x.test"]);
    cli.ok(&["rm", &id]);

    let events = cli.events();
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "task_created",
            "task_updated",
            "status_changed",
            "attachment_added",
            "task_deleted",
        ]
    );
}

#[test]
fn events_carry_undo_payloads_for_reversible_changes() {
    let cli = Cli::new();
    let id = cli.add("t", &["--est", "1h"]);
    cli.ok(&["edit", &id, "--est", "2h"]);

    let ev = cli.events().last().unwrap().clone();
    assert_eq!(ev["patch"]["estimate_minutes"], 120);
    assert_eq!(ev["undo"]["estimate_minutes"], 60);
}

// ---------------------------------------------------------------------------
// Exit codes
// ---------------------------------------------------------------------------

#[test]
fn exit_codes_are_distinct_and_scriptable() {
    let cli = Cli::new();
    let id = cli.add("t", &[]);

    assert_eq!(cli.exit_code(&["show", "no-such-task"]), 3, "not found");
    assert_eq!(cli.exit_code(&["list", "--status", "bogus"]), 2, "usage");
    assert_eq!(
        cli.exit_code(&["add", "--title", "x", "--est", "bogus"]),
        2,
        "usage"
    );
    assert_eq!(
        cli.exit_code(&["add", "--title", ""]),
        4,
        "store/validation"
    );

    // A successful command exits 0.
    assert!(
        cli.cmd()
            .args(["show", &id])
            .assert()
            .success()
            .get_output()
            .status
            .success()
    );
}

#[test]
fn error_messages_are_actionable() {
    let cli = Cli::new();
    let out = cli.fail_stderr(&["add", "--title", "t", "--due", "tomorrowish"]);
    assert!(out.contains("could not parse due date"), "{out}");
    assert!(out.contains("+3d"), "should show valid forms: {out}");
}

// ---------------------------------------------------------------------------
// Store integrity under concurrent agents
// ---------------------------------------------------------------------------

#[test]
fn concurrent_adds_do_not_lose_writes() {
    use std::process::Stdio;

    let cli = Cli::new();
    let mut handles = Vec::new();

    for i in 0..10 {
        let mut c = Command::cargo_bin("ttm").unwrap();
        c.env("TTM_HOME", &cli.home)
            .args(["add", "--title", &format!("task {i}"), "--est", "30m"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        handles.push(c.spawn().expect("spawn"));
    }

    for mut h in handles {
        assert!(
            h.wait().expect("wait").success(),
            "every add should succeed"
        );
    }

    assert_eq!(cli.ok_json(&["list", "--json"])["count"], 10);
    assert_eq!(cli.events().len(), 10);
}

#[test]
fn tasks_json_stays_parseable_after_every_operation() {
    let cli = Cli::new();
    let id = cli.add("t", &["--est", "1h", "--tag", "x"]);
    cli.ok(&["edit", &id, "--title", "renamed"]);
    cli.ok(&["link", &id, "--pdf", "/tmp/a.pdf"]);
    cli.ok(&["start", &id]);
    cli.ok(&["done", &id]);
    cli.ok(&["undo", &id]);

    let v = cli.tasks_json();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
    assert!(v["tasks"][0]["id"].is_string());
}

#[test]
fn a_corrupt_store_reports_an_error_instead_of_wiping_it() {
    let cli = Cli::new();
    cli.add("t", &[]);

    std::fs::write(cli.home.join("tasks.json"), "{ not json").unwrap();

    let out = cli.fail_stderr(&["list"]);
    assert!(out.contains("could not parse"), "{out}");
    assert_eq!(
        std::fs::read_to_string(cli.home.join("tasks.json")).unwrap(),
        "{ not json",
        "the unreadable file must be left alone"
    );
}

#[test]
fn ttm_home_isolates_stores() {
    let a = Cli::new();
    let b = Cli::new();

    a.add("only in a", &[]);

    assert_eq!(a.ok_json(&["list", "--json"])["count"], 1);
    assert_eq!(b.ok_json(&["list", "--json"])["count"], 0);
}

#[test]
fn bare_ttm_asks_for_a_terminal_rather_than_hanging_or_exiting_zero() {
    let cli = Cli::new();
    // There is no TTY under the test harness, so the UI cannot start. It must
    // report that clearly instead of exiting 0 or blocking forever.
    assert_ne!(cli.exit_code(&[]), 0);
    let out = cli.fail_stderr(&[]);
    assert!(
        out.contains("terminal") || out.contains("TUI"),
        "unhelpful message: {out}"
    );
}

// `contains` is unused without the failure-path assertions above; keep the
// import honest.
#[test]
fn stderr_contains_the_error_prefix() {
    let cli = Cli::new();
    cli.cmd()
        .args(["show", "ghost"])
        .assert()
        .failure()
        .stderr(contains("ttm:"));
}
