# AGENTS.md

Guidance for agents (and scripts) driving `ttm`, the terminal task manager.

`ttm` stores tasks in plain JSON with no daemon. The contract is the files; the
CLI is the safe way to read and write them. This document describes exactly what
the data looks like and how to drive it correctly.

If you are **creating** tasks in bulk, read [`IMPORT.md`](IMPORT.md) first —
`ttm import` takes a whole JSON document atomically and is the right tool for
generated input.

## The store

Location, in precedence order:

1. `-H/--home <DIR>` on any command
2. `$TTM_HOME`
3. `~/.local/share/ttm`

Three files:

| File | Purpose |
| --- | --- |
| `tasks.json` | Current state: `{ "schema_version": 1, "tasks": [...] }` |
| `events.jsonl` | Append-only audit log, one JSON event per line |
| `store.lock` | Advisory lock file, held during writes |

### Reading the store

Reading `tasks.json` directly is supported and safe — it is always a complete,
valid document. But **prefer the CLI** for anything that writes.

To locate the files without guessing paths:

```sh
ttm info --paths -j     # {"root": ..., "tasks_path": ..., "events_path": ..., "lock_path": ...}
```

### Writing the store

Do not hand-edit `tasks.json`. Writes go through the CLI so that:

- the write is **atomic** (temp file + `rename`; a reader never sees a partial file),
- the **lock** is taken (`store.lock`), so concurrent writers queue instead of clobbering,
- an **event** is appended to `events.jsonl` for audit and for `undo`.

Editing the file by hand silently bypasses all three: you lose the audit trail
and you risk a torn write if anything else touches the store at the same time.

Concurrent `ttm` invocations are safe. Two simultaneous `ttm add` calls both
succeed and both land.

## Reading tasks

### Via the CLI

```sh
ttm list -j                            # every task
ttm list -j --status todo              # filter by status
ttm list -j --due overdue              # any|overdue|today|week|nodue
ttm list -j -q "search text"           # title, description, tags, attachments
ttm list -j --tag ops --roots          # top-level only
ttm list -j --parent "Some title"      # subtasks of one task
ttm list -j --sort due --reverse
ttm list --ids                         # ids only, one per line (NOT json)
```

### The `list` envelope

`list -j` returns an object, **not** a bare array:

```json
{
  "count": 2,
  "summary": {
    "blocked": 0, "cancelled": 0, "doing": 0, "done": 0,
    "due_today": 0, "overdue": 0, "todo": 2, "total": 2,
    "total_estimate_minutes": 300
  },
  "tasks": [ /* Task objects, see below */ ]
}
```

Iterate `.tasks[]`, not the top level:

```sh
ttm list -j --status todo | jq -r '.tasks[] | select(.estimate_minutes > 240) | .title'
```

`summary -j` returns the same `summary` block on its own.

### A single task

```sh
ttm show <ref> -j                 # one task
ttm show <ref> -j --rollup        # adds subtask rollups
ttm show <ref> -j --history       # adds the event history
```

### The Task object

```json
{
  "id": "01a12c16-3f61-7375-8436-220c56676225",
  "parent_id": null,
  "title": "Parent task",
  "description": "",
  "status": "todo",
  "priority": "urgent",
  "created_at": "2026-10-11T17:50:17.953302Z",
  "updated_at": "2026-10-11T17:50:17.953302Z",
  "started_at": null,
  "completed_at": null,
  "due_at": "2026-10-16T17:50:17.953302Z",
  "estimate_minutes": 240,
  "tags": ["a", "b"],
  "attachments": []
}
```

| Field | Type | Notes |
| --- | --- | --- |
| `id` | string | UUIDv7. Time-sortable, so lexicographic order ≈ creation order. |
| `parent_id` | string \| null | Non-null means subtask. **One level of nesting only.** |
| `title` | string | Required, non-empty. |
| `description` | string | May contain newlines. |
| `status` | string | `todo`, `doing`, `blocked`, `done`, `cancelled` |
| `priority` | string | `low`, `normal`, `high`, `urgent` |
| `created_at` / `updated_at` | string | ISO-8601 UTC. |
| `started_at` / `completed_at` | string \| null | ISO-8601 UTC, set by `start` / `done`. |
| `due_at` | string \| null | ISO-8601 UTC. **May be in the past** — that is an overdue task, not an error. |
| `estimate_minutes` | integer \| null | Effort estimate. |
| `tags` | array of string | |
| `attachments` | array of object | `{kind, target, label, added_at}`; `kind` is `url` or `pdf`. |

### Rollup fields

`show --rollup` adds these to a task that has subtasks:

| Field | Meaning |
| --- | --- |
| `subtask_count` | Number of direct subtasks |
| `subtasks_done` | How many of them are `done` |
| `rolled_estimate_minutes` | Own estimate plus every subtask's |
| `rolled_due_at` | The **earliest** subtask deadline (the real completion date) |
| `subtasks` | The subtask objects themselves |

`list` does **not** include rollups. Ask for them with `show --rollup` when a
parent's true cost or true deadline matters.

### The timeline

```sh
ttm timeline -j --before 7 --days 30
```

Returns `{count, from, now, to, items}`. Each item is a task plus derived
fields: `days_remaining`, `hours_remaining`, `overdue`, `subtask_count`,
`subtasks_done`, `rolled_estimate_minutes`. `--days` is the runway and
`--before` the history; they default to 30 and 7.

## Addressing tasks

Any command taking a task accepts a `<REFERENCE>`:

- an **id prefix** — at least **12 hexadecimal characters**, matching exactly
  one task. Ids are UUIDv7, so tasks created in the same millisecond share a long
  prefix and need more characters to separate; when in doubt use the full id,
  which always works.
- or an **exact title**.

Anything shorter than 12 hex characters is treated as a **title**, not an id —
so `ttm show 01a1` is a title lookup, not a prefix lookup.

Titles are matched exactly, so quoting matters: `ttm show "Ship release"`.
Prefer ids: capture one with `ttm list --ids | head -1` and reuse it verbatim.

A reference that matches nothing, or more than one task, exits `3`:
`"…" is ambiguous; candidates: …`.

## Writing tasks

| Operation | Command |
| --- | --- |
| Create | `ttm add -t "Title" --due +7d -e 4h -p high -T tag` |
| Create many at once | `ttm import plan.json` — see [IMPORT.md](IMPORT.md) |
| Update | `ttm edit <ref> --status doing --due +3d` |
| Mark doing | `ttm start <ref>` |
| Mark done | `ttm done <ref>` |
| Delete | `ttm remove <ref>` (cascades to subtasks) |
| Attach | `ttm link <ref> --url https://…` or `--pdf ./f.pdf` |
| Undo | `ttm undo <ref>` or `ttm undo` (last change) |

`add`, `edit`, `done`, `start`, `remove`, `link`, and `undo` all accept
`--dry-run`, which validates and prints the result **without writing**.

### Creating several tasks at once

Use `ttm import` rather than a loop over `ttm add`. It takes one JSON document
and writes it atomically, so a bad entry rejects the whole batch instead of
leaving it half-applied:

```sh
ttm import plan.json --dry-run -j     # validate, write nothing
ttm import plan.json                   # commit
echo '{"tasks":[{"title":"Generated","due":"+3d"}]}' | ttm import - -j
```

See [`IMPORT.md`](IMPORT.md) for the field reference and recipes.

### Date formats

`--due` accepts, and normalizes to UTC ISO-8601 on write:

| Form | Example |
| --- | --- |
| Relative offset | `+3d`, `-1d` |
| Named day | `today`, `tomorrow`, `eod` |
| Absolute date | `2026-11-01` |
| Absolute date-time | `2026-11-01T14:30` |

### Estimate formats

`--estimate` / `-e`: `4h`, `90m`, `2d`, `1d 4h`, or bare minutes (`45`).

### Subtasks

```sh
ttm add -t "Child" --parent <ref>
ttm edit <ref> --no-parent      # detach
```

Nesting is one level deep. `--parent` on a task that is itself a subtask is
rejected rather than silently creating a deeper tree.

### Clearing fields

```sh
ttm edit <ref> --no-due --no-estimate
ttm edit <ref> --untag obsolete
```

## Exit codes

Branch on these; do not parse stderr text.

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `2` | Bad input — unparseable date, unknown status, bad filter |
| `3` | Task not found, or ambiguous reference |
| `4` | Store problem — locked, unparsable, or validation failed |

```sh
if ttm done "$id"; then ...; fi
```

## Output mode

`--json` forces machine output. Without it, output is human-readable text
(colored when stdout is a TTY, plain otherwise). `--text` forces plain text
even when piped. **Always pass `-j` when parsing.**

## Agent conventions

- **Dry-run before you write.** For anything batch or destructive, run it with
  `--dry-run` first and inspect the output. `ttm import --dry-run` reports every
  task it *would* create without touching the store.
- **Prefer `ttm import` for generated work.** A loop of `ttm add` calls is not
  atomic: a failure halfway leaves the store inconsistent. `ttm import` is.
- **Never hand-write `tasks.json`.** Use the CLI (see above).
- **Idempotency is your job.** `import` will happily create duplicates on a second
  run; pass `--skip-existing` if a job may repeat.
- **Prefer ids over titles.** Titles are mutable and matched exactly.
- **Serialize writes.** Concurrent writes are safe (they lock), but if you are
  building a task from several dependent writes, do them in sequence so a
  failure partway is meaningful.
- **`undo` is your safety net.** `ttm undo <ref>` restores the previous field
  values; `ttm undo` with no ref walks back the most recent change anywhere.
  `ttm show <ref> --history` shows what happened.
- **Estimates are minutes.** Multiply by `/60` for hours; do not round-trip the
  display string.

## Worked examples

```sh
# Everything overdue, oldest first, as one line each.
ttm list --due overdue --sort due -j | jq -r '.tasks[] | "\(.due_at)\t\(.title)"'

# Create a parent with two subtasks, then verify the rollup.
ttm add -t "Migration" --due +10d -e 8h
ttm add -t "Write migration" --parent "Migration" --due +7d -e 3h
ttm add -t "Test migration"  --parent "Migration" -e 2h
ttm show "Migration" -j --rollup | jq '{est: .rolled_estimate_minutes, due: .rolled_due_at}'
# => {"est": 780, "due": "<earliest subtask deadline>"}

# Preview a change without writing it.
ttm edit "Migration" --due +14d --dry-run

# Take back an edit. `undo` restores the *previous field values*, so there
# must have been an edit: undoing a task that was only ever created exits 3.
ttm edit "Migration" --status doing
ttm undo "Migration"
ttm show "Migration" -j | jq -r .status     # => todo

# Mark everything due today as in progress.
for id in $(ttm list --due today --ids); do ttm start "$id"; done

# Total open effort, in hours.
ttm summary -j | jq '.summary.total_estimate_minutes / 60'

# Nesting is one level deep; a subtask cannot become a parent.
ttm add -t "Too deep" --parent "Write migration"    # exits 4
```

## Where things live

| Concern | File |
| --- | --- |
| Task, status, priority, date and estimate parsing | `src/model.rs` |
| JSON store, locking, event log | `src/store.rs` |
| CLI definitions and behaviour | `src/cli.rs` |
| JSON bulk creation | `src/import.rs` |
| Timeline geometry (pure, no terminal) | `src/timeline.rs` |
| Key bindings | `src/input.rs` |

`src/timeline.rs` holds the chart layout as pure functions over dates, so date
correctness is testable without a terminal. If you change date handling, that is
the file to read first.