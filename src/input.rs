//! Key handling: a vim-flavoured, multi-key input grammar.
//!
//! Bindings are declared as data in [`BINDINGS`] so the help overlay and the
//! dispatcher can never disagree, and so a test can prove every documented
//! sequence resolves to a real action.
//!
//! Multi-key sequences (`gg`, `gG`) are handled by accumulating keys into a
//! buffer. The dispatcher reports `Incomplete` while a prefix could still grow,
//! which is what lets the UI show a pending-key indicator the way vim's
//! `showcmd` does.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[cfg(test)]
use ratatui::crossterm::event::{KeyCode as TestCode, KeyEvent as TestEvent};

#[cfg(test)]
use ratatui::crossterm::event::{KeyEventKind as TestKind, KeyModifiers as TestMods};

/// Everything the app can be asked to do.
///
/// Grouped to match the sections of the help overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    // Global
    Quit,
    Help,
    CloseOverlay,
    ToggleFilterPanel,
    SortNext,

    // Movement
    Down,
    Up,
    PageDown,
    PageUp,
    HalfPageDown,
    HalfPageUp,
    FirstLine,
    LastLine,
    CenterLine,

    // Selection and filtering
    ClearFilter,
    CycleFilterStatus,
    Refresh,

    // Task operations (implemented in step 5)
    Add,
    EditTask,
    OpenDetail,
    CycleStatus,
    SetDue,
    SetEstimate,
    AddLink,
    AddPdf,
    Delete,

    // Bulk (visual mode, step 8)
    EnterVisual,
    BulkSetStatus,
    BulkSetPriority,
    BulkDelete,

    // Display toggles
    ToggleTree,
    ToggleHelpPanel,

    /// Save the open form. In NORMAL mode nothing is pending — every mutation
    /// is written through — so this reports that rather than pretending.
    SaveForm,

    /// Timeline view and its zoom controls.
    ToggleTimeline,
    ZoomIn,
    ZoomOut,

    /// Detail-pane specific keys.
    OpenAttachment,
    RemoveAttachment,
    NextAttachment,
    PrevAttachment,
}

/// Which modal context a key is interpreted in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Context {
    /// Everything except a text-entry field.
    #[default]
    Global,
    /// While a text-entry field has focus; most keys are literal text.
    Insert,
    /// A search/filter prompt is open.
    Prompt,
}

impl Context {
    /// What the status bar calls this context.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Context::Global => "NORMAL",
            Context::Insert => "INSERT",
            Context::Prompt => "FILTER",
        }
    }
}

/// Which full-screen view a binding is active in.
///
/// Keys overlap legitimately between views — `o` opens an attachment in the
/// detail pane but sets priority in visual mode — so bindings declare their
/// scope and only same-scope collisions are a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The task list, including visual mode.
    List,
    /// The single-task detail pane.
    Detail,
    /// The gantt chart.
    Timeline,
}

impl Scope {
    const ALL: [Scope; 3] = [Scope::List, Scope::Detail, Scope::Timeline];

    fn index(self) -> usize {
        match self {
            Scope::List => 0,
            Scope::Detail => 1,
            Scope::Timeline => 2,
        }
    }

    /// Views inherit the list keymap, so a pane only declares what it changes.
    #[must_use]
    fn fallback(self) -> Option<Scope> {
        match self {
            Scope::List => None,
            Scope::Detail | Scope::Timeline => Some(Scope::List),
        }
    }
}

/// One documented binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Human-readable key sequence, as shown in the help overlay. Parsed by
    /// [`parse_sequence`] so the docs cannot describe keys that do nothing.
    pub keys: &'static str,
    pub action: Action,
    pub desc: &'static str,
    pub scope: Scope,
}

/// Every binding, grouped by the help overlay's section headings.
pub const BINDINGS: &[(&str, &[Binding])] = &[
    (
        "Global",
        &[
            Binding {
                keys: "q",
                action: Action::Quit,
                desc: "quit",
                scope: Scope::List,
            },
            Binding {
                keys: "?",
                action: Action::Help,
                desc: "toggle this help",
                scope: Scope::List,
            },
            Binding {
                keys: "<Esc>",
                action: Action::CloseOverlay,
                desc: "close help / cancel",
                scope: Scope::List,
            },
            Binding {
                keys: "<C-s>",
                action: Action::SaveForm,
                desc: "save the open form",
                scope: Scope::List,
            },
            Binding {
                keys: "r",
                action: Action::Refresh,
                desc: "re-read the store from disk",
                scope: Scope::List,
            },
        ],
    ),
    (
        "Move",
        &[
            Binding {
                keys: "j",
                action: Action::Down,
                desc: "down one",
                scope: Scope::List,
            },
            Binding {
                keys: "k",
                action: Action::Up,
                desc: "up one",
                scope: Scope::List,
            },
            Binding {
                keys: "<C-d>",
                action: Action::HalfPageDown,
                desc: "half page down",
                scope: Scope::List,
            },
            Binding {
                keys: "<C-u>",
                action: Action::HalfPageUp,
                desc: "half page up",
                scope: Scope::List,
            },
            Binding {
                keys: "<C-f>",
                action: Action::PageDown,
                desc: "full page down",
                scope: Scope::List,
            },
            Binding {
                keys: "<C-b>",
                action: Action::PageUp,
                desc: "full page up",
                scope: Scope::List,
            },
            Binding {
                keys: "<PgDn>",
                action: Action::PageDown,
                desc: "full page down",
                scope: Scope::List,
            },
            Binding {
                keys: "<PgUp>",
                action: Action::PageUp,
                desc: "full page up",
                scope: Scope::List,
            },
            Binding {
                keys: "gg",
                action: Action::FirstLine,
                desc: "first task",
                scope: Scope::List,
            },
            Binding {
                keys: "G",
                action: Action::LastLine,
                desc: "last task",
                scope: Scope::List,
            },
            Binding {
                keys: "zz",
                action: Action::CenterLine,
                desc: "centre the cursor",
                scope: Scope::List,
            },
        ],
    ),
    (
        "Filter & sort",
        &[
            Binding {
                keys: "/",
                action: Action::ToggleFilterPanel,
                desc: "search tasks",
                scope: Scope::List,
            },
            Binding {
                keys: "f",
                action: Action::CycleFilterStatus,
                desc: "cycle status filter",
                scope: Scope::List,
            },
            Binding {
                keys: "<C-c>",
                action: Action::ClearFilter,
                desc: "clear all filters",
                scope: Scope::List,
            },
            Binding {
                keys: "S",
                action: Action::SortNext,
                desc: "next sort key",
                scope: Scope::List,
            },
        ],
    ),
    (
        "Tasks",
        &[
            Binding {
                keys: "a",
                action: Action::Add,
                desc: "add a task",
                scope: Scope::List,
            },
            Binding {
                keys: "i",
                action: Action::EditTask,
                desc: "edit the selected task",
                scope: Scope::List,
            },
            Binding {
                keys: "<Enter>",
                action: Action::OpenDetail,
                desc: "open the detail pane",
                scope: Scope::List,
            },
            Binding {
                keys: "<Space>",
                action: Action::CycleStatus,
                desc: "cycle todo/doing/blocked/done",
                scope: Scope::List,
            },
            Binding {
                keys: "D",
                action: Action::SetDue,
                desc: "set a due date",
                scope: Scope::List,
            },
            Binding {
                keys: "E",
                action: Action::SetEstimate,
                desc: "set an estimate",
                scope: Scope::List,
            },
            Binding {
                keys: "l",
                action: Action::AddLink,
                desc: "attach a link",
                scope: Scope::List,
            },
            Binding {
                keys: "p",
                action: Action::AddPdf,
                desc: "attach a PDF",
                scope: Scope::List,
            },
            Binding {
                keys: "x",
                action: Action::Delete,
                desc: "delete the task",
                scope: Scope::List,
            },
        ],
    ),
    (
        "Timeline",
        &[
            Binding {
                keys: "T",
                action: Action::ToggleTimeline,
                scope: Scope::List,
                desc: "toggle the gantt timeline",
            },
            Binding {
                keys: "+",
                action: Action::ZoomIn,
                scope: Scope::List,
                desc: "coarser scale (days, weeks, months)",
            },
            Binding {
                keys: "-",
                action: Action::ZoomOut,
                scope: Scope::List,
                desc: "finer scale",
            },
        ],
    ),
    (
        "Detail",
        &[
            Binding {
                keys: "[",
                action: Action::PrevAttachment,
                desc: "previous attachment",
                scope: Scope::Detail,
            },
            Binding {
                keys: "]",
                action: Action::NextAttachment,
                desc: "next attachment",
                scope: Scope::Detail,
            },
            Binding {
                keys: "o",
                action: Action::OpenAttachment,
                desc: "open in the default app",
                scope: Scope::Detail,
            },
            Binding {
                keys: "d",
                action: Action::RemoveAttachment,
                desc: "remove the attachment",
                scope: Scope::Detail,
            },
            Binding {
                keys: "<Enter>",
                action: Action::OpenAttachment,
                desc: "open it",
                scope: Scope::Detail,
            },
        ],
    ),
    (
        "Visual",
        &[
            Binding {
                keys: "V",
                action: Action::EnterVisual,
                desc: "start a visual selection",
                scope: Scope::List,
            },
            Binding {
                keys: "t",
                action: Action::BulkSetStatus,
                desc: "set status on the selection",
                scope: Scope::List,
            },
            Binding {
                keys: "o",
                action: Action::BulkSetPriority,
                desc: "set priority on the selection",
                scope: Scope::List,
            },
            Binding {
                keys: "d",
                action: Action::BulkDelete,
                desc: "delete the selection",
                scope: Scope::List,
            },
        ],
    ),
    (
        "View",
        &[
            Binding {
                keys: "w",
                action: Action::ToggleTree,
                desc: "toggle parent/child nesting",
                scope: Scope::List,
            },
            Binding {
                keys: "<C-p>",
                action: Action::ToggleHelpPanel,
                desc: "toggle the key cheatsheet",
                scope: Scope::List,
            },
        ],
    ),
];

/// What the dispatcher decided about a key buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A complete binding matched.
    Bound(Action),
    /// The buffer is a proper prefix of a longer binding; wait for more keys.
    Incomplete,
    /// Nothing matches; the buffer should be discarded.
    Unbound,
}

/// One step in a key sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Alt(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Space,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// Translate a crossterm key event into our vocabulary.
///
/// Returns `None` for events that carry no input (mouse, focus, resize), which
/// the caller should treat as "ignore".
#[must_use]
pub fn key_of(event: &KeyEvent) -> Option<Key> {
    // Windows emits both Press and Release; ignore everything but Press.
    if event.kind != KeyEventKind::Press {
        return None;
    }
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);

    Some(match event.code {
        // A combined Ctrl+Alt chord is not bound to anything; treating it as a
        // bare Ctrl would fire the wrong action.
        KeyCode::Char(_) if ctrl && alt => return None,
        // The space bar arrives as a character; normalise it so a `<Space>`
        // binding is actually reachable.
        KeyCode::Char(' ') if !ctrl && !alt => Key::Space,
        KeyCode::Char(c) if ctrl => Key::Ctrl(c.to_ascii_lowercase()),
        KeyCode::Char(c) if alt => Key::Alt(c),
        // Uppercase arrives as Shift+Char, so normalise to the literal char.
        KeyCode::Char(c) => Key::Char(if shift { c.to_ascii_uppercase() } else { c }),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::F(n) => Key::F(n),
        // Arrows with modifiers arrive as bare codes plus flags; treat a
        // modified arrow as unbound rather than guessing.
        _ if ctrl || alt => return None,
        _ => return None,
    })
}

/// Parse a documented sequence such as `"gg"`, `"<C-s>"`, or `"<Space>"`.
///
/// Used by the tests to prove the help text matches the dispatcher; it is also
/// handy for building a keymap file later.
#[must_use]
pub fn parse_sequence(spec: &str) -> Option<Vec<Key>> {
    if spec.is_empty() {
        return None;
    }

    let mut out = Vec::new();
    let mut chars = spec.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '<' {
            // A bare character. Uppercase in the docs means Shift was held.
            out.push(Key::Char(c));
            continue;
        }

        // Consume up to the matching '>'.
        let mut name = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == '>' {
                closed = true;
                break;
            }
            name.push(c);
        }
        if !closed {
            return None;
        }

        let lower = name.to_ascii_lowercase();
        out.push(match lower.as_str() {
            "c-s" => Key::Ctrl('s'),
            "c-d" => Key::Ctrl('d'),
            "c-u" => Key::Ctrl('u'),
            "c-f" => Key::Ctrl('f'),
            "c-b" => Key::Ctrl('b'),
            "c-c" => Key::Ctrl('c'),
            "c-p" => Key::Ctrl('p'),
            "enter" | "cr" | "return" => Key::Enter,
            "esc" => Key::Esc,
            "tab" => Key::Tab,
            "s-tab" | "backtab" => Key::BackTab,
            "space" => Key::Space,
            "bs" | "backspace" => Key::Backspace,
            "delete" | "del" => Key::Delete,
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" | "pgup" => Key::PageUp,
            "pagedown" | "pgdn" => Key::PageDown,
            other => {
                // `<F5>` and friends.
                if let Some(n) = other.strip_prefix('f')
                    && let Ok(n) = n.parse::<u8>()
                {
                    Key::F(n)
                } else {
                    return None;
                }
            }
        });
    }

    Some(out)
}

/// Render a key sequence the way the help overlay shows it.
#[must_use]
pub fn format_sequence(keys: &[Key]) -> String {
    keys.iter()
        .map(|k| match k {
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("<C-{c}>"),
            Key::Alt(c) => format!("<M-{c}>"),
            Key::Enter => "<Enter>".into(),
            Key::Esc => "<Esc>".into(),
            Key::Tab => "<Tab>".into(),
            Key::BackTab => "<S-Tab>".into(),
            Key::Backspace => "<BS>".into(),
            Key::Delete => "<Del>".into(),
            Key::Space => "<Space>".into(),
            Key::Up => "<Up>".into(),
            Key::Down => "<Down>".into(),
            Key::Left => "<Left>".into(),
            Key::Right => "<Right>".into(),
            Key::Home => "<Home>".into(),
            Key::End => "<End>".into(),
            Key::PageUp => "<PgUp>".into(),
            Key::PageDown => "<PgDn>".into(),
            Key::F(n) => format!("<F{n}>"),
        })
        .collect()
}

/// Short label for a single key, used by the pending-key indicator.
#[must_use]
pub fn format_key(key: Key) -> String {
    format_sequence(std::slice::from_ref(&key))
}

/// The dispatcher.
///
/// Lookup is a linear scan over a small, static table, which beats a trie here
/// and keeps the binding list the only place a mapping is written down.
#[derive(Debug)]
pub struct Dispatcher {
    /// `(sequence, action)` pairs per [`Scope`], in declaration order.
    sequences: [Vec<(Vec<Key>, Action)>; 3],
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatcher {
    #[must_use]
    pub fn new() -> Self {
        let mut sequences: [Vec<(Vec<Key>, Action)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (_, group) in BINDINGS {
            for b in group.iter() {
                let seq = parse_sequence(b.keys).unwrap_or_else(|| {
                    panic!(
                        "binding {:?} does not parse; BINDINGS is the source of truth",
                        b.keys
                    )
                });
                sequences[b.scope.index()].push((seq, b.action));
            }
        }
        Self { sequences }
    }

    /// Longest sequence length, which bounds how long a pending buffer can get.
    #[must_use]
    pub fn max_len(&self) -> usize {
        Scope::ALL
            .iter()
            .flat_map(|s| self.sequences[s.index()].iter())
            .map(|(s, _)| s.len())
            .max()
            .unwrap_or(1)
    }

    /// Resolve an accumulated key buffer in the given view.
    ///
    /// A view's bindings *override* the list's rather than replacing them: the
    /// detail pane inherits `j`/`k`/`gg`/`G` from the list and re-binds only
    /// the handful of keys that mean something different there.
    ///
    /// A single-key binding wins over a multi-key one when both match, so `g`
    /// does not get stuck waiting when only `G` is bound.
    #[must_use]
    pub fn resolve(&self, buf: &[Key], scope: Scope) -> Outcome {
        let mut current = Some(scope);
        while let Some(s) = current {
            let outcome = self.resolve_in(buf, s);
            match outcome {
                Outcome::Unbound => current = s.fallback(),
                other => return other,
            }
        }
        Outcome::Unbound
    }

    fn resolve_in(&self, buf: &[Key], scope: Scope) -> Outcome {
        if buf.is_empty() {
            return Outcome::Unbound;
        }

        let mut best: Option<(usize, Action)> = None;
        let mut prefix_of_longer = false;

        for (seq, action) in self.sequences[scope.index()].iter() {
            if seq.len() < buf.len() {
                continue;
            }
            if seq[..buf.len()] != *buf {
                continue;
            }
            match seq.len() {
                len if len == buf.len() => {
                    let candidate = (len, *action);
                    // Prefer the shortest exact match, then declaration order.
                    if best.is_none_or(|(bl, _)| candidate.0 < bl) {
                        best = Some(candidate);
                    }
                }
                _ => prefix_of_longer = true,
            }
        }

        if let Some((_, action)) = best {
            return Outcome::Bound(action);
        }
        if prefix_of_longer {
            Outcome::Incomplete
        } else {
            Outcome::Unbound
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d() -> Dispatcher {
        Dispatcher::new()
    }

    #[test]
    fn single_keys_resolve() {
        assert_eq!(
            d().resolve(&[Key::Char('j')], Scope::List),
            Outcome::Bound(Action::Down)
        );
        assert_eq!(
            d().resolve(&[Key::Char('k')], Scope::List),
            Outcome::Bound(Action::Up)
        );
        assert_eq!(
            d().resolve(&[Key::Char('q')], Scope::List),
            Outcome::Bound(Action::Quit)
        );
        assert_eq!(
            d().resolve(&[Key::Char('?')], Scope::List),
            Outcome::Bound(Action::Help)
        );
        assert_eq!(
            d().resolve(&[Key::Enter], Scope::List),
            Outcome::Bound(Action::OpenDetail)
        );
        assert_eq!(
            d().resolve(&[Key::Space], Scope::List),
            Outcome::Bound(Action::CycleStatus)
        );
    }

    #[test]
    fn uppercase_is_distinct_from_lowercase() {
        assert_eq!(
            d().resolve(&[Key::Char('G')], Scope::List),
            Outcome::Bound(Action::LastLine)
        );
        assert_eq!(
            d().resolve(&[Key::Char('S')], Scope::List),
            Outcome::Bound(Action::SortNext)
        );
        assert_eq!(
            d().resolve(&[Key::Char('D')], Scope::List),
            Outcome::Bound(Action::SetDue)
        );
        assert_eq!(
            d().resolve(&[Key::Char('E')], Scope::List),
            Outcome::Bound(Action::SetEstimate)
        );
        assert_eq!(
            d().resolve(&[Key::Char('V')], Scope::List),
            Outcome::Bound(Action::EnterVisual)
        );
    }

    #[test]
    fn control_keys_resolve() {
        assert_eq!(
            d().resolve(&[Key::Ctrl('s')], Scope::List),
            Outcome::Bound(Action::SaveForm)
        );
        assert_eq!(
            d().resolve(&[Key::Ctrl('d')], Scope::List),
            Outcome::Bound(Action::HalfPageDown)
        );
        assert_eq!(
            d().resolve(&[Key::Ctrl('c')], Scope::List),
            Outcome::Bound(Action::ClearFilter)
        );
    }

    #[test]
    fn multi_key_sequences_report_incomplete_then_bind() {
        let dd = d();
        assert_eq!(
            dd.resolve(&[Key::Char('g')], Scope::List),
            Outcome::Incomplete
        );
        assert_eq!(
            dd.resolve(&[Key::Char('g'), Key::Char('g')], Scope::List),
            Outcome::Bound(Action::FirstLine)
        );

        assert_eq!(
            dd.resolve(&[Key::Char('z')], Scope::List),
            Outcome::Incomplete
        );
        assert_eq!(
            dd.resolve(&[Key::Char('z'), Key::Char('z')], Scope::List),
            Outcome::Bound(Action::CenterLine)
        );
    }

    #[test]
    fn a_wrong_second_key_is_unbound_not_stuck() {
        let dd = d();
        assert_eq!(
            dd.resolve(&[Key::Char('g'), Key::Char('x')], Scope::List),
            Outcome::Unbound
        );
        // And a fresh buffer still works afterwards.
        assert_eq!(
            dd.resolve(&[Key::Char('j')], Scope::List),
            Outcome::Bound(Action::Down)
        );
    }

    #[test]
    fn unmapped_keys_are_unbound() {
        assert_eq!(
            d().resolve(&[Key::Char('\u{1}')], Scope::List),
            Outcome::Unbound
        );
        assert_eq!(d().resolve(&[Key::F(12)], Scope::List), Outcome::Unbound);
        assert_eq!(d().resolve(&[], Scope::List), Outcome::Unbound);
    }

    /// The help overlay is generated from `BINDINGS`, so a typo in the docs
    /// would otherwise ship a key that silently does nothing.
    #[test]
    fn every_documented_binding_resolves_to_its_action() {
        let dd = d();
        for (section, group) in BINDINGS {
            for b in *group {
                let seq = parse_sequence(b.keys)
                    .unwrap_or_else(|| panic!("{section}: {:?} is not parseable", b.keys));
                assert!(!seq.is_empty(), "{section}: empty sequence");
                assert!(
                    seq.len() <= dd.max_len(),
                    "{section}: {:?} is longer than max_len",
                    b.keys
                );
                assert_eq!(
                    dd.resolve(&seq, b.scope),
                    Outcome::Bound(b.action),
                    "{section}: {:?} does not resolve to its documented action in {:?}",
                    b.keys,
                    b.scope
                );
            }
        }
    }

    #[test]
    fn documented_keys_round_trip_through_the_parser() {
        for (_, group) in BINDINGS {
            for b in *group {
                let seq = parse_sequence(b.keys).expect("parses");
                assert_eq!(
                    &format_sequence(&seq),
                    b.keys,
                    "docs disagree with the formatter"
                );
            }
        }
    }

    #[test]
    fn every_action_is_documented() {
        let documented: std::collections::HashSet<Action> = BINDINGS
            .iter()
            .flat_map(|(_, g)| g.iter())
            .map(|b| b.action)
            .collect();
        let all = [
            Action::Quit,
            Action::Help,
            Action::CloseOverlay,
            Action::ToggleFilterPanel,
            Action::SortNext,
            Action::Down,
            Action::Up,
            Action::PageDown,
            Action::PageUp,
            Action::HalfPageDown,
            Action::HalfPageUp,
            Action::FirstLine,
            Action::LastLine,
            Action::CenterLine,
            Action::ClearFilter,
            Action::CycleFilterStatus,
            Action::Refresh,
            Action::Add,
            Action::EditTask,
            Action::OpenDetail,
            Action::CycleStatus,
            Action::SetDue,
            Action::SetEstimate,
            Action::AddLink,
            Action::AddPdf,
            Action::Delete,
            Action::EnterVisual,
            Action::BulkSetStatus,
            Action::BulkSetPriority,
            Action::BulkDelete,
            Action::ToggleTree,
            Action::ToggleHelpPanel,
        ];
        for a in all {
            assert!(documented.contains(&a), "{a:?} has no documented binding");
        }
    }

    #[test]
    fn no_binding_is_duplicated_within_a_view() {
        let mut seen: std::collections::HashMap<(Scope, Vec<Key>), (&str, Action)> =
            std::collections::HashMap::new();
        for (section, group) in BINDINGS {
            for b in *group {
                let seq = parse_sequence(b.keys).expect("parses");
                let key = (b.scope, seq);
                if let Some(prev) = seen.insert(key, (b.keys, b.action)) {
                    panic!(
                        "{section}: {:?} in {:?} duplicates {:?} ({:?})",
                        b.keys, b.scope, prev.0, prev.1
                    );
                }
            }
        }
    }

    /// The detail pane has its own keys; they must all be documented, and
    /// documented keys must resolve in that view.
    #[test]
    fn the_detail_view_has_its_own_bindings() {
        let detail: Vec<&Binding> = BINDINGS
            .iter()
            .flat_map(|(_, g)| g.iter())
            .filter(|b| b.scope == Scope::Detail)
            .collect();

        let keys: Vec<&str> = detail.iter().map(|b| b.keys).collect();
        for expected in ["[", "]", "o", "<Enter>", "d"] {
            assert!(
                keys.contains(&expected),
                "the detail view is missing {expected:?}: {keys:?}"
            );
        }

        let dd = d();
        for b in detail {
            let seq = parse_sequence(b.keys).expect("parses");
            assert_eq!(
                dd.resolve(&seq, Scope::Detail),
                Outcome::Bound(b.action),
                "{:?} should work in the detail view",
                b.keys
            );
        }
    }

    /// `o` means different things in different views, which is exactly why
    /// bindings are scoped rather than globally unique.
    #[test]
    fn the_same_key_can_mean_different_things_per_view() {
        let dd = d();
        assert_eq!(
            dd.resolve(&[Key::Char('o')], Scope::List),
            Outcome::Bound(Action::BulkSetPriority)
        );
        assert_eq!(
            dd.resolve(&[Key::Char('o')], Scope::Detail),
            Outcome::Bound(Action::OpenAttachment)
        );
    }

    #[test]
    fn crossterm_events_translate() {
        use TestCode as C;
        use TestEvent as E;
        use TestKind as K;
        use TestMods as M;

        let press = |code, mods| E::new(code, mods);

        assert_eq!(key_of(&press(C::Char('j'), M::NONE)), Some(Key::Char('j')));
        // The space bar arrives as a character, and must normalise to Key::Space.
        assert_eq!(key_of(&press(C::Char(' '), M::NONE)), Some(Key::Space));
        assert_eq!(
            key_of(&press(C::Char('s'), M::CONTROL)),
            Some(Key::Ctrl('s'))
        );
        assert_eq!(key_of(&press(C::Enter, M::NONE)), Some(Key::Enter));
        assert_eq!(key_of(&press(C::Char('G'), M::SHIFT)), Some(Key::Char('G')));

        // Release events carry no input.
        let mut release = press(C::Char('j'), M::NONE);
        release.kind = K::Release;
        assert_eq!(key_of(&release), None);

        // Mouse-ish and unmapped codes are ignored rather than misread.
        assert_eq!(key_of(&press(C::Null, M::NONE)), None);
        assert_eq!(key_of(&press(C::Char('x'), M::ALT | M::CONTROL)), None);
    }

    #[test]
    fn sequence_parser_rejects_malformed_specs() {
        assert_eq!(parse_sequence(""), None);
        assert_eq!(parse_sequence("<C"), None);
        assert_eq!(parse_sequence("<Nonsense>"), None);
        assert_eq!(parse_sequence("j<"), None);
        assert!(parse_sequence("g<Esc>").is_some());
    }

    #[test]
    fn control_specs_are_case_insensitive() {
        assert_eq!(parse_sequence("<c-s>"), parse_sequence("<C-S>"));
    }

    #[test]
    fn context_labels_are_vim_terminology() {
        assert_eq!(Context::Global.label(), "NORMAL");
        assert_eq!(Context::Insert.label(), "INSERT");
        assert_eq!(Context::Prompt.label(), "FILTER");
    }
}
