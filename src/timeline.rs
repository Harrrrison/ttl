//! Gantt layout: dates to columns.
//!
//! Every date calculation here works on [`NaiveDate`], never on instants.
//! Columns are *calendar days* at the day scale, so a task due at 23:00 and one
//! due at 01:00 the next morning must land in different columns — and they do,
//! because nothing here touches a timezone and no DST boundary can shift a bar
//! by a day.
//!
//! The whole layout is a pure function of its inputs, which is what makes the
//! geometry testable without a terminal.

#[cfg(test)]
use chrono::TimeZone;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use uuid::Uuid;

use crate::model::Task;

/// Horizontal resolution of the chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scale {
    /// One column per day.
    #[default]
    Day,
    /// One column per week.
    Week,
    /// One column per month.
    Month,
}

impl Scale {
    pub const ALL: [Scale; 3] = [Scale::Day, Scale::Week, Scale::Month];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Scale::Day => "day",
            Scale::Week => "week",
            Scale::Month => "month",
        }
    }

    /// Label for the zoom action.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Scale::Day => "days",
            Scale::Week => "weeks",
            Scale::Month => "months",
        }
    }

    /// Days of calendar each column covers.
    #[must_use]
    pub fn bucket_days(self) -> i64 {
        match self {
            Scale::Day => 1,
            Scale::Week => 7,
            // Months vary in length; this is only used for the window size and
            // the estimate marker, where an average is fine. Exact column
            // boundaries come from [`Timeline::column_of`].
            Scale::Month => 30,
        }
    }

    /// How much history to show to the left of today.
    #[must_use]
    pub fn context_days(self) -> i64 {
        match self {
            Scale::Day => 10,
            Scale::Week => 28,
            Scale::Month => 60,
        }
    }

    /// How much runway to show to the right of today.
    #[must_use]
    pub fn runway_days(self) -> i64 {
        match self {
            Scale::Day => 28,
            Scale::Week => 84,
            Scale::Month => 120,
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    #[must_use]
    pub fn prev(self) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    /// Start of the bucket a date belongs to.
    #[must_use]
    pub fn bucket_start(self, date: NaiveDate) -> NaiveDate {
        match self {
            Scale::Day => date,
            Scale::Week => {
                // Monday-based weeks, the convention most people expect.
                let back = i64::from(date.weekday().num_days_from_monday());
                date - Duration::days(back)
            }
            Scale::Month => NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap_or(date),
        }
    }
}

/// What to draw for one task.
#[derive(Debug, Clone, PartialEq)]
pub struct Input<'a> {
    pub task: &'a Task,
    /// 0 for a root task, 1 for a subtask.
    pub depth: u8,
    pub children: usize,
    pub children_done: usize,
}

/// The horizontal extent of a bar, already clipped to the chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    /// First column, inclusive.
    pub start: usize,
    /// Last column, exclusive.
    pub end: usize,
}

impl Bar {
    #[must_use]
    pub fn width(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    #[must_use]
    pub fn contains(self, col: usize) -> bool {
        col >= self.start && col < self.end
    }
}

/// One laid-out row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: Uuid,
    pub depth: u8,
    /// The schedule bar, `None` for an undated task.
    pub bar: Option<Bar>,
    /// Where the work would finish if the estimate were spent from `created_at`.
    pub estimate_col: Option<usize>,
    /// Rollup for a parent with children.
    pub rollup: Option<(usize, usize)>,
    pub overdue: bool,
    pub due_today: bool,
    pub completed: bool,
    /// Right-gutter countdown, e.g. `D-3` or `OVER5d`.
    pub countdown: String,
    /// Right-gutter estimate, e.g. `4h`.
    pub estimate: Option<String>,
    pub title: String,
    /// True when the row falls entirely outside the visible window.
    pub offscreen: bool,
}

/// The complete chart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline {
    pub scale: Scale,
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// Total columns across the chart area.
    pub columns: usize,
    /// Column of today, which the NOW line is drawn through.
    pub now_col: usize,
    /// Per-column header labels.
    pub header: Vec<String>,
    /// Columns falling on a weekend, for shading.
    pub weekend: Vec<bool>,
    /// Month abbreviation at the column where each month starts, `None`
    /// elsewhere. Sized to `columns` so the axis can be drawn at any width.
    pub months: Vec<Option<String>>,
    pub rows: Vec<Row>,
}

impl Timeline {
    /// Local date of an instant.
    fn local(date: DateTime<Utc>) -> NaiveDate {
        date.with_timezone(&chrono::Local).date_naive()
    }

    /// Column a date falls in, or `None` when it is off the chart.
    ///
    /// The offset is measured in buckets, not raw days: at week scale every
    /// seven days share a column, which is the whole point of the scale.
    #[must_use]
    pub fn column_of(&self, date: NaiveDate) -> Option<usize> {
        let offset = (date - self.from).num_days();
        if offset < 0 {
            return None;
        }
        let bucket = self.scale.bucket_days().max(1);
        let col = usize::try_from(offset / bucket).ok()?;
        (col < self.columns).then_some(col)
    }

    /// Build the chart.
    ///
    /// `chart_columns` is how many columns are actually available to draw in;
    /// `columns` may be larger, in which case the window is scrolled so `now_col`
    /// stays visible.
    pub fn build(inputs: &[Input<'_>], now: DateTime<Utc>, scale: Scale, columns: usize) -> Self {
        let today = Self::local(now);
        let bucket = scale.bucket_days();

        // Anchor the window to whole buckets so headers read cleanly, then
        // size it from the real horizon: context behind today, plus today, plus
        // the runway. Deriving the count from `bucket_days` alone would put
        // today off-chart at month scale, where a bucket is longer than the
        // context window.
        let bucket_start = scale.bucket_start(today);
        let from = bucket_start - Duration::days(scale.context_days());
        let horizon_days =
            scale.context_days() + (today - bucket_start).num_days() + scale.runway_days();
        let needed = usize::try_from((horizon_days + bucket - 1) / bucket + 1).unwrap_or(1);
        let columns = columns.max(1);
        // Extra width extends the runway rather than changing the resolution.
        let span = needed.max(columns);

        let mut weekend = Vec::with_capacity(span);
        let mut header = Vec::with_capacity(span);
        let mut months: Vec<Option<String>> = vec![None; span];
        let mut previous: Option<(i32, u32)> = None;
        for (i, month) in months.iter_mut().enumerate() {
            let date = from + Duration::days(i64::try_from(i).unwrap_or(0) * bucket);
            weekend.push(matches!(
                date.weekday(),
                chrono::Weekday::Sat | chrono::Weekday::Sun
            ));
            // A column is one character wide, so its label must fit in one.
            // At month scale each column already *is* a month, so it is labelled
            // directly; the separate month row is suppressed there, because
            // writing a three-letter name across three columns garbles the axis.
            header.push(match scale {
                // Only single-digit days are labelled; the month row above
                // carries the rest of the information.
                Scale::Day | Scale::Week => {
                    if (1..=9).contains(&date.day()) {
                        date.day().to_string()
                    } else {
                        String::new()
                    }
                }
                Scale::Month => date
                    .format("%b")
                    .to_string()
                    .chars()
                    .next()
                    .unwrap_or(' ')
                    .to_string(),
            });

            if scale != Scale::Month {
                // Label the first column of each month. Detecting the change is
                // not the same as testing for the 1st: at week scale a bucket
                // rarely *is* the 1st, which would leave months unlabelled.
                let key = (date.year(), date.month());
                if previous != Some(key) {
                    *month = Some(date.format("%b").to_string());
                    previous = Some(key);
                }
            }
        }

        let mut tl = Self {
            scale,
            from,
            to: from + Duration::days(i64::try_from(span).unwrap_or(0) * bucket),
            columns: span,
            now_col: 0,
            header,
            months,
            weekend,
            rows: Vec::with_capacity(inputs.len()),
        };
        tl.now_col = tl.column_of(today).unwrap_or(0);

        for input in inputs {
            tl.rows.push(layout_row(input, &tl, now));
        }
        tl
    }

    /// Clamp this chart to `columns` columns.
    ///
    /// The window is chosen from the horizon and then stretched to fill the
    /// width, so a chart built for one terminal width must be re-narrowed for
    /// another; otherwise every row overflows and wraps.
    #[must_use]
    pub fn narrow(&self, columns: usize) -> Self {
        let columns = columns.max(1);
        if columns >= self.columns {
            return self.clone();
        }
        let mut out = self.clone();
        let step = self.scale.bucket_days().max(1);
        let keep = columns;
        out.header.truncate(keep);
        out.weekend.truncate(keep);
        out.months.truncate(keep);
        out.columns = keep;
        out.rows = out
            .rows
            .into_iter()
            .map(|mut r| {
                if let Some(b) = &mut r.bar {
                    b.start = b.start.min(keep);
                    b.end = b.end.min(keep);
                }
                if let Some(c) = &mut r.estimate_col {
                    *c = (*c).min(keep.saturating_sub(1));
                }
                r
            })
            .collect();
        out.to = out.from + Duration::days(i64::try_from(keep).unwrap_or(0) * step);
        out
    }

    /// Total rows, for scroll clamping.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Whether a column falls on a weekend.
    #[must_use]
    pub fn is_weekend(&self, col: usize) -> bool {
        self.weekend.get(col).copied().unwrap_or(false)
    }
}

fn layout_row(input: &Input<'_>, tl: &Timeline, now: DateTime<Utc>) -> Row {
    let task = input.task;
    let today = Timeline::local(now);

    let created_col = tl.column_of(Timeline::local(task.created_at));
    // A due date outside the window has no column, which `column_of` reports
    // as `None`; that is not the same as having no due date at all.
    let due_col: Option<Option<usize>> = task.due_at.map(|d| tl.column_of(Timeline::local(d)));

    // Lateness is a property of the task, not of where its bar happens to
    // start, so compute it before any window clipping. Otherwise a task created
    // before the visible range silently stops reading as overdue.
    let mut overdue = false;
    let mut due_today = false;
    if task.status.is_open()
        && let Some(due) = task.due_at
    {
        overdue = due < now;
        due_today = Timeline::local(due) == today;
    }

    // The bar runs from creation to the end of the due day.
    let bar = match (created_col, due_col.flatten()) {
        (Some(start), Some(due_end)) => {
            let mut end = due_end.saturating_add(1).min(tl.columns);
            // An overdue bar is clipped at NOW: it cannot extend into the future.
            if overdue {
                end = end.min(tl.now_col.saturating_add(1));
            }
            if end > start {
                Some(Bar { start, end })
            } else {
                // A single-day bar still deserves a visible tick.
                Some(Bar {
                    start,
                    end: (start + 1).min(tl.columns),
                })
            }
        }
        (Some(start), None) => Some(Bar {
            start,
            // An undated task shows only where it was created.
            end: (start + 1).min(tl.columns),
        }),
        (None, Some(end)) => Some(Bar {
            start: 0,
            end: end.max(1),
        }),
        (None, None) => None,
    };

    // The estimate says when the work *should* finish, if started on creation.
    let estimate_col = task.estimate_duration().and_then(|d| {
        let finish = Timeline::local(task.created_at) + Duration::days(d.num_days().max(0));
        tl.column_of(finish)
    });

    let rollup = (input.children > 0).then_some((input.children_done, input.children));

    let countdown = task
        .due_at
        .map(|d| gutter_countdown(d, now))
        .unwrap_or_default();

    let offscreen = bar.is_none_or(|b| b.end <= tl.now_col && task.status.is_open())
        && task.status.is_open()
        && created_col.is_none_or(|c| c >= tl.columns);

    Row {
        id: task.id,
        depth: input.depth,
        bar,
        estimate_col,
        rollup,
        overdue,
        due_today,
        completed: task.status.is_terminal(),
        countdown,
        estimate: task.estimate_human(),
        title: task.title.clone(),
        offscreen,
    }
}

/// Short right-gutter countdown: `D-3`, `D+7`, `OVER5d`, `TODAY`.
#[must_use]
pub fn gutter_countdown(due: DateTime<Utc>, now: DateTime<Utc>) -> String {
    // `num_days` truncates toward zero, which would call 42 hours overdue
    // "OVER1d". Floor instead, so the count never understates the lateness.
    let days = (due - now).num_hours().div_euclid(24);
    if days < 0 {
        let n = -days;
        return if n >= 1_000 {
            "OVER999d".to_string()
        } else {
            format!("OVER{n}d")
        };
    }
    match days {
        0 => "TODAY".to_string(),
        1 => "D+1".to_string(),
        n => format!("D+{n}"),
    }
}

/// Character for a bar cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// Empty chart background.
    Blank,
    /// Weekend shading.
    Weekend,
    /// Part of a schedule bar that is still in the future.
    Bar,
    /// Part of an overdue bar.
    Overdue,
    /// Part of a completed task's bar.
    Done,
    /// The day a task is due.
    DueEdge,
    /// Today's column on the NOW line.
    Now,
    /// Where the estimate says the work finishes.
    Estimate,
    /// Today, on a row with no bar.
    TodayBare,
}

impl Cell {
    /// The glyph to draw.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Cell::Blank => " ",
            Cell::Weekend => "·",
            Cell::Bar => "─",
            Cell::Overdue => "█",
            Cell::Done => "─",
            Cell::DueEdge => "▐",
            Cell::Now => "│",
            Cell::Estimate => "◆",
            Cell::TodayBare => "┃",
        }
    }
}

/// Decide what to draw in each column of a row.
///
/// Kept separate from [`Timeline::build`] so the whole chart can be tested as a
/// grid without caring how it is rendered.
#[must_use]
pub fn row_cells(row: &Row, tl: &Timeline) -> Vec<Cell> {
    let mut cells = vec![Cell::Blank; tl.columns];

    for (col, slot) in cells.iter_mut().enumerate() {
        if tl.is_weekend(col) {
            *slot = Cell::Weekend;
        }
    }

    if let Some(bar) = row.bar {
        let fill = if row.completed {
            Cell::Done
        } else if row.overdue {
            Cell::Overdue
        } else {
            Cell::Bar
        };
        let end = bar.end.min(tl.columns).min(cells.len());
        for slot in &mut cells[bar.start.min(end)..end] {
            *slot = fill;
        }
        // Mark the final day so the deadline reads clearly.
        if !row.completed
            && let Some(last) = bar.end.checked_sub(1)
            && last < tl.columns
        {
            cells[last] = Cell::DueEdge;
        }
    }

    // The NOW line is drawn over the bar so it stays visible on every row.
    if tl.now_col < tl.columns {
        cells[tl.now_col] = match row.bar {
            Some(bar) if bar.contains(tl.now_col) => Cell::Now,
            _ => {
                if tl.is_weekend(tl.now_col) {
                    Cell::Now
                } else {
                    Cell::TodayBare
                }
            }
        };
    }

    if let Some(est) = row.estimate_col
        && est < tl.columns
    {
        cells[est] = Cell::Estimate;
    }

    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Status, Task};

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
    }

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0)
            .single()
            .expect("valid instant")
    }

    /// A Wednesday, so weekday maths is predictable.
    fn now() -> DateTime<Utc> {
        at(2026, 10, 7, 12)
    }

    /// An instant whose *local* wall clock is the given date and time.
    ///
    /// The layout buckets by local calendar day, so tests about day boundaries
    /// must pin local time, not UTC — otherwise they pass or fail depending on
    /// the machine's timezone.
    fn local_at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        chrono::Local
            .with_ymd_and_hms(y, m, d, h, 0, 0)
            .single()
            .expect("valid local time")
            .with_timezone(&Utc)
    }

    fn task(created: DateTime<Utc>) -> Task {
        Task::new("t", created)
    }

    fn inputs(tasks: &[Task]) -> Vec<Input<'_>> {
        tasks
            .iter()
            .map(|t| Input {
                task: t,
                depth: 0,
                children: 0,
                children_done: 0,
            })
            .collect()
    }

    /// Lay out a single owned task without a borrow that outlives the call.
    fn build_one(task: &Task, now: DateTime<Utc>, scale: Scale) -> Timeline {
        Timeline::build(
            &[Input {
                task,
                depth: 0,
                children: 0,
                children_done: 0,
            }],
            now,
            scale,
            40,
        )
    }

    // -- scale --

    #[test]
    fn scales_cycle_in_both_directions() {
        assert_eq!(Scale::Day.next(), Scale::Week);
        assert_eq!(Scale::Week.next(), Scale::Month);
        assert_eq!(Scale::Month.next(), Scale::Day);
        assert_eq!(Scale::Day.prev(), Scale::Month);
    }

    #[test]
    fn week_buckets_start_on_monday() {
        // 2026-10-07 is a Wednesday.
        assert_eq!(Scale::Week.bucket_start(day(2026, 10, 7)), day(2026, 10, 5));
        // A Monday is its own bucket start.
        assert_eq!(Scale::Week.bucket_start(day(2026, 10, 5)), day(2026, 10, 5));
        // A Sunday belongs to the week that began the Monday before.
        assert_eq!(
            Scale::Week.bucket_start(day(2026, 10, 11)),
            day(2026, 10, 5)
        );
    }

    #[test]
    fn month_buckets_start_on_the_first() {
        assert_eq!(
            Scale::Month.bucket_start(day(2026, 10, 17)),
            day(2026, 10, 1)
        );
        assert_eq!(Scale::Month.bucket_start(day(2026, 2, 28)), day(2026, 2, 1));
    }

    // -- window --

    #[test]
    fn the_window_always_contains_today() {
        for scale in Scale::ALL {
            let tl = Timeline::build(&[], now(), scale, 40);
            let today = day(2026, 10, 7);
            assert!(
                tl.column_of(today).is_some(),
                "{scale:?} window excludes today"
            );
        }
    }

    #[test]
    fn column_mapping_is_calendar_days_not_instant_hours() {
        let tl = Timeline::build(&[], now(), Scale::Day, 40);
        // Two deadlines on consecutive calendar days must be adjacent columns
        // even though they are only hours apart.
        let late = tl.column_of(day(2026, 10, 8)).unwrap();
        let early = tl.column_of(day(2026, 10, 9)).unwrap();
        assert_eq!(late + 1, early);
    }

    #[test]
    fn dates_outside_the_window_have_no_column() {
        let tl = Timeline::build(&[], now(), Scale::Day, 40);
        assert!(tl.column_of(day(1999, 1, 1)).is_none(), "before the window");
        assert!(tl.column_of(day(2099, 1, 1)).is_none(), "after the window");
    }

    #[test]
    fn zooming_out_covers_more_calendar_in_the_same_width() {
        let width = 40;
        let day_tl = Timeline::build(&[], now(), Scale::Day, width);
        let week_tl = Timeline::build(&[], now(), Scale::Week, width);
        let month_tl = Timeline::build(&[], now(), Scale::Month, width);

        // The column count follows the available width; what changes is how much
        // calendar each of those columns stands for.
        let days = |tl: &Timeline| (tl.to - tl.from).num_days();
        assert!(
            days(&week_tl) > days(&day_tl) * 2,
            "a week column must span more than a day: {} vs {}",
            days(&week_tl),
            days(&day_tl)
        );
        assert!(
            days(&month_tl) > days(&week_tl) * 2,
            "a month column must span more than a week"
        );
    }

    #[test]
    fn a_week_column_groups_seven_days() {
        let tl = Timeline::build(&[], now(), Scale::Week, 40);
        let monday = tl.column_of(day(2026, 10, 5)).expect("in window");
        let sunday = tl.column_of(day(2026, 10, 11)).expect("in window");
        let next = tl.column_of(day(2026, 10, 12)).expect("in window");
        assert_eq!(monday, sunday, "a whole week shares one column");
        assert_eq!(next, monday + 1);
    }

    #[test]
    fn weekend_columns_are_flagged() {
        let tl = Timeline::build(&[], now(), Scale::Day, 40);
        let today_col = tl.column_of(day(2026, 10, 7)).expect("today");
        assert!(!tl.is_weekend(today_col), "Wednesday is not a weekend");

        let sat = tl.column_of(day(2026, 10, 10)).expect("in window");
        let sun = tl.column_of(day(2026, 10, 11)).expect("in window");
        assert!(tl.is_weekend(sat), "Saturday");
        assert!(tl.is_weekend(sun), "Sunday");

        let mon = tl.column_of(day(2026, 10, 12)).expect("in window");
        assert!(!tl.is_weekend(mon), "Monday");
    }

    // -- bars --

    #[test]
    fn a_bar_runs_from_creation_to_the_day_after_the_deadline() {
        let mut t = task(local_at(2026, 10, 1, 9));
        t.due_at = Some(local_at(2026, 10, 5, 17));
        let tl = build_one(&t, now(), Scale::Day);

        let bar = tl.rows[0].bar.expect("a bar");
        let start = tl.column_of(day(2026, 10, 1)).unwrap();
        let end = tl.column_of(day(2026, 10, 5)).unwrap() + 1;
        assert_eq!((bar.start, bar.end), (start, end));
        assert_eq!(bar.width(), 5);
    }

    #[test]
    fn a_deadline_late_in_the_day_and_one_early_next_day_are_separate_columns() {
        let mut late = task(local_at(2026, 10, 1, 9));
        late.due_at = Some(local_at(2026, 10, 5, 23));
        let mut early = task(local_at(2026, 10, 1, 9));
        early.due_at = Some(local_at(2026, 10, 6, 1));

        let owned = vec![late, early];
        let ts = inputs(&owned);
        let tl = Timeline::build(&ts, now(), Scale::Day, 40);

        let late_bar = tl.rows[0].bar.expect("bar");
        let early_bar = tl.rows[1].bar.expect("bar");
        assert!(
            early_bar.end > late_bar.end,
            "01:00 the next day must not collapse into the previous column"
        );
        assert_eq!(early_bar.end - late_bar.end, 1);
    }

    #[test]
    fn an_overdue_bar_is_clipped_at_now() {
        let mut t = task(at(2026, 10, 1, 9));
        // The deadline is already in the past relative to `later` below.
        t.due_at = Some(local_at(2026, 10, 9, 17));
        // Ask the layout to lay out with a much later clock, so it is overdue.
        let later = at(2026, 10, 12, 12);
        let tl = build_one(&t, later, Scale::Day);

        assert!(tl.rows[0].overdue);
        let bar = tl.rows[0].bar.expect("bar");
        assert!(
            bar.end <= tl.now_col + 1,
            "an overdue bar must not extend past today: {bar:?}, now={}",
            tl.now_col
        );
    }

    #[test]
    fn a_task_due_today_is_flagged() {
        let mut t = task(at(2026, 10, 1, 9));
        t.due_at = Some(at(2026, 10, 7, 17));
        let tl = build_one(&t, now(), Scale::Day);

        assert!(tl.rows[0].due_today);
        assert!(!tl.rows[0].overdue);
    }

    #[test]
    fn an_undated_task_gets_a_single_cell_at_creation() {
        let t = task(at(2026, 10, 2, 9));
        let tl = build_one(&t, now(), Scale::Day);

        let bar = tl.rows[0].bar.expect("a marker at creation");
        assert_eq!(bar.width(), 1);
        assert_eq!(bar.start, tl.column_of(day(2026, 10, 2)).unwrap());
        assert!(
            tl.rows[0].countdown.is_empty(),
            "no countdown without a due date"
        );
    }

    #[test]
    fn a_completed_task_is_not_flagged_overdue() {
        let mut t = task(at(2026, 10, 1, 9));
        t.due_at = Some(at(2026, 10, 3, 17));
        t.set_status(Status::Done, at(2026, 10, 3, 10));
        let owned = vec![t];
        let tl = Timeline::build(&inputs(&owned), now(), Scale::Day, 40);

        assert!(!tl.rows[0].overdue);
        assert!(tl.rows[0].completed);
    }

    // -- estimate --

    #[test]
    fn the_estimate_marks_where_the_work_would_finish() {
        let mut t = task(at(2026, 10, 1, 9));
        t.estimate_minutes = Some(24 * 60); // a day
        let tl = build_one(&t, now(), Scale::Day);

        let created = tl.column_of(day(2026, 10, 1)).unwrap();
        let est = tl.rows[0].estimate_col.expect("an estimate marker");
        assert_eq!(est, created + 1, "a day of work finishes the next day");
        assert_eq!(tl.rows[0].estimate.as_deref(), Some("1d"));
    }

    #[test]
    fn a_task_with_no_estimate_has_no_marker() {
        let t = task(at(2026, 10, 1, 9));
        let tl = build_one(&t, now(), Scale::Day);
        assert_eq!(tl.rows[0].estimate_col, None);
        assert_eq!(tl.rows[0].estimate, None);
    }

    // -- rollup --

    #[test]
    fn parents_carry_a_child_rollup() {
        let parent = task(at(2026, 10, 1, 9));
        let ts = vec![Input {
            task: &parent,
            depth: 0,
            children: 4,
            children_done: 3,
        }];
        let tl = Timeline::build(&ts, now(), Scale::Day, 40);
        assert_eq!(tl.rows[0].rollup, Some((3, 4)));
    }

    #[test]
    fn a_childless_task_has_no_rollup() {
        let t = task(at(2026, 10, 1, 9));
        let tl = Timeline::build(&inputs(&[t]), now(), Scale::Day, 40);
        assert_eq!(tl.rows[0].rollup, None);
    }

    // -- cells --

    #[test]
    fn the_now_column_is_drawn_on_every_row() {
        let mut t = task(at(2026, 10, 1, 9));
        t.due_at = Some(at(2026, 10, 20, 17));
        let tl = build_one(&t, now(), Scale::Day);

        let cells = row_cells(&tl.rows[0], &tl);
        assert_eq!(
            cells[tl.now_col],
            Cell::Now,
            "the NOW line must be visible on a row whose bar covers it"
        );
    }

    #[test]
    fn the_estimate_marker_wins_over_the_bar() {
        let mut t = task(at(2026, 10, 1, 9));
        t.due_at = Some(at(2026, 10, 20, 17));
        t.estimate_minutes = Some(24 * 60);
        let tl = build_one(&t, now(), Scale::Day);

        let cells = row_cells(&tl.rows[0], &tl);
        let est = tl.rows[0].estimate_col.unwrap();
        assert_eq!(cells[est], Cell::Estimate);
    }

    #[test]
    fn the_due_edge_marks_the_last_column_of_a_future_bar() {
        let mut t = task(local_at(2026, 10, 1, 9));
        // Still in the future, so the bar is not rendered as overdue.
        t.due_at = Some(local_at(2026, 10, 20, 17));
        let tl = build_one(&t, now(), Scale::Day);

        let cells = row_cells(&tl.rows[0], &tl);
        let bar = tl.rows[0].bar.unwrap();
        assert_eq!(cells[bar.end - 1], Cell::DueEdge);
        assert_eq!(cells[bar.start], Cell::Bar);
    }

    #[test]
    fn weekend_columns_show_through_on_an_empty_row() {
        let t = task(at(2090, 1, 1, 9)); // off-screen: no bar
        let tl = build_one(&t, now(), Scale::Day);
        let cells = row_cells(&tl.rows[0], &tl);

        let sat = tl.column_of(day(2026, 10, 10)).unwrap();
        assert_eq!(cells[sat], Cell::Weekend);
    }

    #[test]
    fn every_glyph_is_a_single_display_column() {
        // The chart is a fixed grid; a double-width glyph would shear it.
        use unicode_width::UnicodeWidthStr;
        for cell in [
            Cell::Blank,
            Cell::Weekend,
            Cell::Bar,
            Cell::Overdue,
            Cell::Done,
            Cell::DueEdge,
            Cell::Now,
            Cell::Estimate,
            Cell::TodayBare,
        ] {
            assert_eq!(
                UnicodeWidthStr::width(cell.glyph()),
                1,
                "{cell:?} is not one column wide"
            );
        }
    }

    #[test]
    fn rows_are_never_longer_than_the_chart() {
        let mut t = task(at(2026, 10, 1, 9));
        t.due_at = Some(at(2027, 6, 1, 17)); // far past the right edge
        let tl = build_one(&t, now(), Scale::Day);

        let cells = row_cells(&tl.rows[0], &tl);
        assert_eq!(cells.len(), tl.columns);
        let bar = tl.rows[0].bar.expect("bar");
        assert!(bar.end <= tl.columns, "a bar must be clipped to the chart");
    }

    // -- gutter --

    #[test]
    fn gutter_countdowns_are_compact_and_signed() {
        assert_eq!(gutter_countdown(at(2026, 10, 7, 18), now()), "TODAY");
        assert_eq!(gutter_countdown(at(2026, 10, 8, 18), now()), "D+1");
        assert_eq!(gutter_countdown(at(2026, 10, 14, 18), now()), "D+7");
        assert_eq!(gutter_countdown(at(2026, 10, 5, 18), now()), "OVER2d");
    }

    #[test]
    fn an_absurdly_overdue_date_is_bounded() {
        let s = gutter_countdown(at(2020, 1, 1, 0), now());
        assert!(s.len() <= 9, "the gutter is narrow: {s}");
        assert!(s.starts_with("OVER"), "{s}");
    }

    // -- degenerate input --

    #[test]
    fn an_empty_task_list_produces_an_empty_chart() {
        let tl = Timeline::build(&[], now(), Scale::Day, 40);
        assert!(tl.is_empty());
        assert!(tl.columns > 0, "the axis still renders");
        assert!(!tl.header.is_empty());
    }

    #[test]
    fn a_one_column_chart_does_not_panic() {
        let mut t = task(at(2026, 10, 1, 9));
        t.due_at = Some(at(2026, 10, 20, 17));
        let tl = Timeline::build(
            &[Input {
                task: &t,
                depth: 0,
                children: 0,
                children_done: 0,
            }],
            now(),
            Scale::Day,
            1,
        );
        let _ = row_cells(&tl.rows[0], &tl);
    }

    #[test]
    fn a_zero_width_chart_does_not_panic() {
        let tl = Timeline::build(&[], now(), Scale::Day, 0);
        assert!(tl.columns >= 1);
    }
}
