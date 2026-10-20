# Creating tasks from JSON

Guidelines for driving `ttm` with structured input. Read this if you are an
agent, writing a script, or generating task data programmatically.

- For how to *read* tasks and drive the rest of the CLI, see [`AGENTS.md`](AGENTS.md).
- For a user-facing overview, see [`README.md`](README.md).

## Why `import` exists

`ttm add` takes flags. That is the right interface for a person typing, and the
wrong one for a caller holding structured data:

| Problem with flags | What `import` does |
| --- | --- |
| You must flatten a tree into a sequence of calls | `subtasks` nests one level in a single document |
| A partial batch can leave the store half-updated | The whole document lands or none of it does |
| No way to preview | `--dry-run` reports exactly what would be written |
| Re-running duplicates everything | `--skip-existing` makes re-runs idempotent |
| Errors don't say *which* item failed | Every error carries a JSON path |

Use `add` for one-off interactive creation. Use `import` whenever you are
generating tasks — from a script, an agent, a migration, or a fixture.

## The command

```sh
ttm import <SOURCE> [OPTIONS]
```

`SOURCE` can be any of:

```sh
ttm import plan.json                 # a file
ttm import -            < plan.json  # stdin ("-")
ttm import -                           # stdin is the default
ttm import '{"title":"Quick"}'        # inline JSON
```

Anything starting with `{` or `[` is treated as literal JSON; anything else is
treated as a path. So inline documents work without a flag.

### Options

| Flag | Effect |
| --- | --- |
| `--dry-run` | Validate and print what would be created; write nothing |
| `--skip-existing` | Silently skip tasks whose title is already in the store |
| `--no-duplicates` | Fail if any title already exists (conflicts with `--skip-existing`) |
| `-j`, `--json` | Machine-readable output |

`-H`/`--home` and `-j`/`--json` work as everywhere else.

### Exit codes

Identical to the rest of the CLI: `0` success, `2` bad input, `3` not found,
`4` store-level failure. A malformed document is `2`; a rejected duplicate
under `--no-duplicates` is `4`.

## Atomicity

**This is the property that matters most.**

The entire document is parsed, validated and planned *before* the store lock is
taken. Only then is it written, in a single atomic operation. So:

- A bad date in task 5 means **no** tasks are created, not four of them.
- No reader ever observes a partial batch.
- A crash mid-write leaves the previous state intact.

If you need a partial import, split it into several documents — that way each
one is atomic on its own and a failure is meaningful.

## Document shapes

### A single task

Any bare object works:

```json
{ "title": "Fix login timeout", "due": "+2d", "estimate": "2h" }
```

### A batch

```json
{
  "parent": "Optional default parent",
  "tasks": [ { "title": "…" }, { "title": "…" } ]
}
```

`parent` at the batch level applies to every task that does not name its own.

Both shapes report errors with the same path style — a bare object is treated as
a batch of one, so you get `tasks[0].due` either way.

## Task fields

Only `title` is required. Everything else is optional.

| Field | Type | Notes |
| --- | --- | --- |
| `title` | string | **Required**, non-empty, ≤500 characters |
| `description` | string | May contain `\n` |
| `due` | string or number | See [Dates](#dates) |
| `estimate` | string or number | See [Estimates](#estimates) |
| `status` | string | `todo` (default), `doing`, `blocked`, `done`, `cancelled` |
| `priority` | string | `low`, `normal` (default), `high`, `urgent` |
| `tags` | array of string | |
| `attachments` | array of object | `{kind?, target, label?}` |
| `parent` | string | Reference to an existing task — see [Parents](#parents) |
| `subtasks` | array of task | Nested children, one level |
| `id` | string | An **input label**, not a real id — see [Parents](#parents) |
| `skip_if_exists` | bool | Reserved for per-task dedup; prefer the flag |

### Dates

`due` accepts the same forms as `--due`, so you never have to pre-compute UTC:

| Form | Example |
| --- | --- |
| Relative offset | `"+3d"`, `"-1d"` |
| Named day | `"today"`, `"tomorrow"`, `"eod"` |
| Absolute date | `"2026-11-01"` |
| Absolute date-time | `"2026-11-01T14:30"` |
| Minutes from now | `90` |

The last form is a plain JSON number for callers that prefer arithmetic to
parsing. Omit the field entirely for a task with no deadline — a deadline in the
past is allowed and simply marks the task overdue.

### Estimates

```json
"estimate": "4h"        // 4h, 90m, 2d, "1d 4h"
"estimate": 240         // plain minutes
```

Stored as integer minutes.

### Attachments

```json
"attachments": [
  { "target": "https://example.com/spec", "label": "Spec" },
  { "target": "./diagram.pdf", "label": "Diagram" },
  { "kind": "url", "target": "mailto:a@b.c", "label": "Mail" }
]
```

`kind` is optional and inferred: a target containing `://` or starting with
`mailto:` becomes a `url`, anything else a `pdf`. A url must have a scheme; a
pdf must end in `.pdf`. Both are validated before anything is written.

### Parents

There are two ways to attach a subtask, depending on whether the parent is
already in the store.

**To an existing task** — use its id prefix (≥12 hex characters) or its exact
title:

```json
{ "tasks": [{ "title": "Write the migration", "parent": "Ship release" }] }
```

**Within one document** — declare an `id` label and reference it. This is how you
express a tree without knowing any UUID in advance:

```json
{
  "tasks": [
    { "id": "epic", "title": "Epic",
      "subtasks": [{ "title": "Part one" }, { "title": "Part two" }] },
    { "title": "Loose end", "parent": "epic" }
  ]
}
```

Nesting is **one level deep**, matching the rest of `ttm`. A nested child that
declares its own `parent` is attached to its nesting parent and the declaration
is ignored, rather than silently creating a third level. A top-level task whose
`parent` cannot be resolved is an **error**, not a quietly-ignored field.

### Subtasks

```json
{
  "title": "Ship the 2.0 release",
  "due": "+14d",
  "estimate": "3d",
  "subtasks": [
    { "title": "Freeze the branch", "due": "+7d", "estimate": "1h" },
    { "title": "Write release notes", "due": "+10d", "estimate": "4h" }
  ]
}
```

All of this lands in one write. The parent's `rolled_estimate_minutes` and
`rolled_due_at` are computed for you — see `ttm show <ref> --rollup`.

## Recipes

### The safe pattern: preview, then apply

```sh
ttm import plan.json --dry-run -j > preview.json   # inspect
ttm import plan.json -j                             # commit
```

Or inline, checking the exit code:

```sh
if ttm import plan.json -j; then echo "committed"; else echo "rejected"; fi
```

### Generating tasks from another tool

```sh
# From any command that emits JSON on stdout.
curl -s "$API/issues" | jq '{
  tasks: [.[] | {
    title: .title,
    due: "+14d",
    priority: (if .labels | any(. == "urgent") then "urgent" else "normal" end),
    tags: [.labels[]?]
  }]
}' | ttm import - -j
```

### An idempotent sync job

`--skip-existing` matches on title, so re-running the same document is safe:

```sh
ttm import plan.json --skip-existing -j | jq '.count'
```

Use `--no-duplicates` instead when a duplicate is a bug you want to hear about.

### Building a tree

```sh
jq -n '{
  tasks: [
    {
      title: "Migration",
      estimate: "8h",
      subtasks: [
        {title: "Write migration", estimate: "3h", due: "+7d"},
        {title: "Test migration",  estimate: "2h"}
      ]
    }
  ]
}' | ttm import - -j
```

### Inspecting what landed

```sh
ttm list -j --roots | jq -r '.tasks[] | .title'
ttm show "Migration" -j --rollup | jq '.rolled_estimate_minutes'
```

## Guidelines

1. **Preview first.** Run `--dry-run` on any document you did not write by hand.
   It costs nothing and it is the cheapest way to catch a bad date.
2. **Use relative dates** (`"+3d"`) rather than absolute ones when the task is
   about work relative to now. Absolute dates go stale; a plan re-run next month
   would create tasks due in the past.
3. **Make re-runs safe.** If a job may run twice, pass `--skip-existing` and
   match on titles you control.
4. **Split large documents by concern.** Atomicity is per-invocation, so a single
   huge document means one bad entry rejects everything. Group by project or by
   sprint instead.
5. **Do not set `id` to a real UUID.** The `id` field is an input label for
   referencing siblings within one document. It is never persisted, and it does
   not update an existing task — use `ttm edit` for that.
6. **Branch on exit codes, not message text.** Messages are for humans and may
   change; `2` and `4` will not.
7. **Read back after writing.** `import` returns the created task objects,
   including their real ids. Use those rather than assuming an id.

## Notes

- Ids are assigned by `ttm` (UUIDv7) and returned in the output. You cannot
  choose them.
- `created_at` is the import time for every task in a batch. Backdating is not
  supported; use `ttm edit` if you need to.
- `status: "doing"` sets `started_at` automatically.
- Events are appended to `events.jsonl` for every created task, so `ttm undo`
  and `ttm show --history` work as usual.