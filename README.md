# ttm

A terminal task manager with a Gantt timeline and an agent-friendly JSON store.

`ttm` gives you a fast, Vim-style TUI for planning work against a calendar, and a
fully-scriptable CLI over the exact same data. There is no daemon, no database
server, and no background process: everything lives in three plain files you can
read, diff, and commit.

```
┌ timeline · months ───────────────────────────────────────────────────────────────────┐
│                    ASOONDJFMAMJJASONDJFMAMJJASONDJFMAMJJASONDJFMAMJJASONDJFMAMJJ        │
│▌Ship the 2.0 release      ██  ◆──▐                                    OVER2d     3d  │
│ Write RFC                 ·  ·  ·                                            D+3    1d  │
│ Fix login bug             ·  ·  ·                                            D+8    2d  │
│─ scheduled █ overdue │ today ◆ est finishes ▐ due ░ subtasks                          │
└───────────────────────────────────────────────────────────────────────────────────────┘
```

## Contents

- [Why](#why)
- [Install](#install)
- [How to use](#how-to-use)
  - [Quick start](#quick-start)
  - [The TUI](#the-tui)
  - [Keybindings](#keybindings)
  - [The timeline](#the-timeline)
  - [The CLI](#the-cli)
  - [Scripting and automation](#scripting-and-automation)
- [Data model](#data-model)
- [How storage works](#how-storage-works)
- [Exit codes](#exit-codes)
- [Design notes](#design-notes)
- [Development](#development)

## Why

Most task tools make you choose between something you can drive from the keyboard
at speed and something an agent can read and write without a UI. `ttm` is one
store with both front doors:

- **The TUI** is the human interface — Vim navigation, a Gantt timeline, forms,
  and a detail pane, all inside your terminal.
- **The CLI** is the scripting interface — every operation the TUI can perform is
  available as a flag with `--json` output and meaningful exit codes, so an agent
  can create, inspect, and update tasks without ever opening a terminal UI.
- **The files are the contract.** State is a single JSON document plus an
  append-only event log. Nothing is hidden in a binary format.

## Install

Requires a Rust toolchain (edition 2024, so **Rust 1.85+**; developed against
1.99).

```sh
# From a checkout:
cargo install --path .

# Or run it in place without installing:
cargo run -- add -t "Try ttm" --due +3d
cargo run --            # launch the TUI
```

## How to use

### Quick start

```sh
# Create a few tasks
ttm add -t "Write the RFC" --due +7d -e 2d -p high -T design
ttm add -t "Review docs" --due tomorrow -e 90m
ttm add -t "Ship 2.0" --due +14d -e 3d --url https://example.com/release

# Look at them
ttm list --tree
ttm summary

# Launch the TUI and press T for the timeline
ttm
```

Every subcommand accepts `--home <DIR>` (or `-H`) to point at a different store,
and `--dry-run` where a change is involved.

### The TUI

Run `ttm` with no subcommand:

```sh
ttm
```

The three panes are:

- **Left** — the task list. This is where the cursor lives; the timeline and the
  detail pane both share it.
- **Right** — the detail pane for the selected task, or the Gantt timeline when
  the timeline is toggled on.
- **Bottom** — the status bar and a one-line footer showing the next few keys for
  the current context.

Press `?` at any time for the full help overlay, or `C-p` for a compact key
cheatsheet that floats over the list.

A few behaviours worth knowing:

- **Forms** (`a` to add, `i` to edit) buffer locally and are committed in one
  atomic write, so you never leave a half-edited task in the store. `C-s` saves
  early, `Esc` cancels.
- **Attachments** (`l` for a URL, `p` for a PDF) are listed in the detail pane.
  `o` opens the selected one in your system's default application, `[` / `]`
  cycle through them, `d` removes one.
- **Visual mode** (`V`) selects a range of tasks so you can bulk-set status (`t`),
  priority (`o`), or delete (`d`).
- **The store is re-read from disk** with `r`. If you edit `tasks.json` in another
  terminal — or an agent does — `r` picks up the change.

### Keybindings

These are the full bindings, grouped as the in-app help overlay groups them.

**Global**

| Key | Action |
| --- | --- |
| `q` | Quit |
| `?` | Toggle help |
| `Esc` | Close help / cancel a form |
| `C-s` | Save the open form |
| `r` | Re-read the store from disk |

**Moving**

| Key | Action |
| --- | --- |
| `j` / `k` | Down / up one |
| `C-d` / `C-u` | Half page down / up |
| `C-f` / `C-b`, `PgDn` / `PgUp` | Full page down / up |
| `gg` / `G` | First / last task |
| `zz` | Centre the cursor |
| `/` | Search tasks |
| `f` | Cycle the status filter |
| `C-c` | Clear all filters |
| `S` | Next sort key |

**Tasks**

| Key | Action |
| --- | --- |
| `a` | Add a task |
| `i` | Edit the selected task |
| `Enter` | Open the detail pane |
| `Space` | Cycle status: todo → doing → blocked → done |
| `D` | Set a due date |
| `E` | Set an estimate |
| `l` | Attach a link |
| `p` | Attach a PDF |
| `x` | Delete the task |

**Detail pane**

| Key | Action |
| --- | --- |
| `[` / `]` | Previous / next attachment |
| `o`, `Enter` | Open the attachment |
| `d` | Remove the attachment |

**Visual mode**

| Key | Action |
| --- | --- |
| `V` | Start a visual selection |
| `t` | Set status on the selection |
| `o` | Set priority on the selection |
| `d` | Delete the selection |

**View**

| Key | Action |
| --- | --- |
| `T` | Toggle the Gantt timeline |
| `+` | Coarser scale (days → weeks → months) |
| `-` | Finer scale |
| `w` | Toggle parent/child nesting |
| `C-p` | Toggle the key cheatsheet |

The timeline keys (`T`, `+`, `-`) are also reachable from the CLI via
`ttm timeline`, so an agent can render the same layout to stdout.

### The timeline

Press `T` to toggle the Gantt view. Each row is a task; each column is a unit of
calendar time at the current scale. Use `+` and `-` to zoom between scales:

- **days** — one column per day, with weekend columns shaded.
- **weeks** — one column per week.
- **months** — one column per month.

Glyphs on the chart:

| Glyph | Meaning |
| --- | --- |
| `─` | Scheduled bar (from creation to due date) |
| `█` | Overdue portion |
| `│` | Today / NOW marker |
| `◆` | Estimate finishes here |
| `▐` | Due date |
| `░` | Subtasks (child tasks render shaded and indented) |

The right-hand gutter shows a live countdown (`OVER2d`, `D+3`) and the rolled-up
estimate for each row. The timeline shares its cursor with the list, so `j`/`k`
move through tasks in both, and `Enter` opens the detail for whatever is selected.

The axis adapts to the terminal width: at narrow widths the legend drops entries
rather than wrapping, and the chart keeps whatever runway fits.

### The CLI

Run `ttm --help` for the full surface; `ttm <command> --help` for each command.
The subcommands are:

| Command | Purpose |
| --- | --- |
| `list` (`ls`) | List tasks, filtered and sorted |
| `show` | Show one task in full, with optional history and rollups |
| `summary` | Aggregate counts and totals across the store |
| `timeline` | Tasks laid out against the calendar |
| `add` | Create a task |
| `edit` (`set`) | Change fields on an existing task |
| `done` | Mark a task done |
| `start` | Mark a task in progress |
| `remove` (`rm`) | Delete a task (and its subtasks) |
| `link` | Attach a link or a PDF |
| `undo` | Undo the most recent change by restoring previous field values |
| `info` | Print where the store lives and what is in it |

**Global flags** — `-H/--home <DIR>` (store location), `-j/--json` (JSON output),
`--text` (force plain text even when stdout is not a terminal).

#### Creating and editing

```sh
# A deadline can be relative, absolute, or a named day.
ttm add -t "Ship release" --due +14d
ttm add -t "Retro"        --due 2026-11-01 --estimate 1h
ttm add -t "Standup"      --due today --estimate 15m -p low
ttm add -t "Import data"  --due eod -T ops -T q3

# Estimates: 4h, 90m, 2d, "1d 4h", or bare minutes.
ttm add -t "Big lift" -e 2d

# Subtasks hang off a parent referenced by id prefix or exact title.
ttm add -t "Write the migration" --parent "Ship release"

# Attachments, with optional labels.
ttm add -t "Spec" --url https://example.com/spec --pdf ./spec.pdf --label Spec
```

`edit` mirrors `add` and adds the removers:

```sh
ttm edit "Write the migration" --status doing
ttm edit "Write the migration" --due +2d --estimate 3h
ttm edit "Write the migration" --untag q3
ttm edit "Write the migration" --no-due --no-estimate
ttm undo "Write the migration"          # step back one change
```

#### Filtering, sorting, and views

`list` and `summary` share the same filter set:

```sh
ttm list --status doing --status blocked   # repeatable
ttm list --due overdue                      # any|overdue|today|week|nodue
ttm list --tag ops -q "migration"          # text search over title/desc/tag/link
ttm list --roots                           # top-level only
ttm list --parent "Ship release"           # just that task's subtasks
ttm list --sort priority --reverse         # manual|due|priority|status|created|title|estimate
ttm list --tree                            # subtasks indented under parents
ttm list --all                             # include done/cancelled (hidden by default)
ttm list --summary                         # one compact line per task
ttm list --limit 10
```

#### Inspecting

```sh
ttm show "Ship release"             # full record
ttm show 01a1 --rollup              # include subtask rollups (id prefix works)
ttm show "Ship release" --history   # include the event log
ttm summary                        # counts + estimate totals
ttm info --paths                    # where the store lives, as JSON
ttm timeline --before 7 --days 30   # the Gantt, as text
```

### Scripting and automation

The CLI is designed to be driven by agents and shell scripts.

**JSON everywhere.** Any command that prints data accepts `-j/--json`:

```sh
ttm list --status todo -j | jq -r '.tasks[] | select(.estimate_minutes > 240) | .title'
ttm summary -j | jq .summary.overdue
ttm info --paths -j | jq .tasks_path
```

`list --json` returns an object with `count`, a `summary` block, and `tasks`, so
reach the rows with `.tasks[]` rather than iterating the top level.

**Stable ids, cheap lookups.** `--ids` prints one id per line, and any command
that takes a task accepts an id prefix or an exact title:

```sh
id=$(ttm list -q "migration" --ids | head -1)
ttm show "$id" -j
```

**Dry runs.** `add`, `edit`, `done`, `start`, `remove`, `link`, and `undo` all
accept `--dry-run`: they validate and print the result without writing to disk.

**Useful one-liners.**

```sh
# Everything overdue, as ids, newest deadline last.
ttm list --due overdue --ids

# Nudge a task's date and status in one atomic edit.
ttm edit "Ship release" --due +7d --status doing

# Total estimated hours remaining across open work.
ttm summary -j | jq '.summary.total_estimate_minutes / 60'

# Append a task from a script.
ttm add -t "Nightly build $(date +%F)" -e 30m -T ci
```

## Data model

A task is:

| Field | Type | Notes |
| --- | --- | --- |
| `id` | UUIDv7 string | Time-sortable; unique per task |
| `parent_id` | UUID or `null` | One level of nesting (subtasks) |
| `title` | string | Required, non-empty |
| `description` | string | Multi-line |
| `status` | `todo` `doing` `blocked` `done` `cancelled` | |
| `priority` | `low` `normal` `high` `urgent` | |
| `created_at` / `updated_at` | ISO-8601 UTC | |
| `started_at` / `completed_at` | ISO-8601 UTC or `null` | |
| `due_at` | ISO-8601 UTC or `null` | May be in the past (overdue is allowed) |
| `estimate_minutes` | integer or `null` | Effort estimate, in minutes |
| `tags` | list of strings | |
| `attachments` | list of `{kind, target, label, added_at}` | `kind` is `url` or `pdf` |

All timestamps are stored in UTC. Deadlines are accepted in several input forms
and normalized on write: relative offsets (`+3d`, `-1d`), named days (`today`,
`eod`), and absolute dates or date-times (`2026-11-01`,
`2026-11-01T14:30`).

Subtasks are supported one level deep: a task may have a `parent_id`, and a parent
renders with rolled-up counts and estimates.

## How storage works

The store is a directory — `$TTM_HOME`, or `~/.local/share/ttm` by default:

```
~/.local/share/ttm/
├── tasks.json     # the current state: { schema_version, tasks: [...] }
├── events.jsonl   # append-only audit log, one JSON event per line
└── store.lock     # advisory lock, held during writes
```

- **Atomic writes.** Every mutation writes a temporary file and `rename`s it over
  `tasks.json`, so a reader (or a crash) never sees a half-written document.
- **Locked writes.** Mutations take an exclusive advisory lock on `store.lock`;
  concurrent `ttm` invocations queue rather than clobber each other.
- **Audit trail.** Each change appends an event (`task_created`, field updates,
  deletes, …) to `events.jsonl`. `ttm undo` walks this log back, restoring the
  previous field values, and `ttm show --history` displays it.
- **Versioned schema.** `tasks.json` carries a `schema_version`. Unknown or
  future versions are rejected rather than silently misread.

Everything is plain, diffable JSON. Point `ttm` at a store you keep in a git repo
and every change is reviewable.

## Exit codes

`ttm` uses exit codes so scripts can react without parsing messages:

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `2` | Bad input — unparseable filter, unknown status, malformed date |
| `3` | The referenced task does not exist |
| `4` | Store-level problem — locked, unparsable, or validation failed |

## Design notes

- **One store, two front doors.** The TUI and CLI are thin layers over the same
  `model` + `store` core; neither owns logic the other lacks. That's why the
  timeline can be rendered to a terminal *and* dumped as text.
- **Pure layout, impure shell.** Timeline geometry (scales, buckets, bar spans,
  column indices) lives in pure functions in `src/timeline.rs` and is unit-tested
  without a terminal; `src/ui/timeline.rs` only turns that layout into pixels.
- **Central key routing.** Input is parsed and dispatched once, in
  `src/app.rs::route_key`, so bindings, scopes, and help stay in sync. The TUI
  only translates crossterm events into keys.
- **Local buffering.** Forms edit a local copy and commit once, keeping the store
  consistent and making cancel free.
- **No daemon.** The TUI is the only long-running process, and it holds no
  background state — the files are always the truth.

## Development

```sh
cargo build           # build
cargo test            # full test suite
cargo fmt             # format
cargo clippy --all-targets -- -D warnings   # lint (CI gate)
```

The test suite covers the model, store (including locking and concurrency),
CLI behaviour, timeline geometry, and golden TUI renders. Rendering tests are
deterministic, so changes to the layout show up as readable diffs.

### Layout

```
src/
├── main.rs       # entry point; maps failures onto exit codes
├── cli.rs        # clap definitions and all subcommand logic
├── model.rs      # Task, Status, Priority, date/estimate parsing
├── store.rs      # JSON store: atomic writes, locking, event log
├── timeline.rs   # pure timeline layout (scales, buckets, bars)
├── app.rs        # application state and key routing
├── tui.rs        # terminal setup and the event loop
├── input.rs      # key parsing and the binding table
├── form.rs       # add/edit forms
├── editor.rs     # text editing within the TUI
└── ui/           # rendering: list, detail, timeline, help, statusbar
tests/            # CLI and golden render tests
```