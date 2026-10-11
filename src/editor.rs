//! An editable text buffer with vim's INSERT-mode motions.
//!
//! Positions are **character indices**, not byte offsets, so multi-byte text
//! behaves: `Backspace` on `é` removes the whole glyph. Byte offsets are
//! computed only where the `String` API needs them.
//!
//! Everything here is pure and free of I/O, which is what makes the editing
//! behaviour cheap to test exhaustively.

use unicode_width::UnicodeWidthChar;

/// Byte offset of character index `idx`.
#[must_use]
pub fn char_to_byte(s: &str, idx: usize) -> usize {
    s.char_indices().nth(idx).map_or(s.len(), |(b, _)| b)
}

/// Character index of byte offset `byte`.
#[must_use]
pub fn byte_to_char(s: &str, byte: usize) -> usize {
    if byte >= s.len() {
        return s.chars().count();
    }
    s.char_indices().take_while(|(b, _)| *b < byte).count()
}

/// Display width of a string in terminal columns.
#[must_use]
pub fn width_of(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// A one-line or multi-line editable buffer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextBuffer {
    text: String,
    /// Cursor position, as a character index into `text`.
    cursor: usize,
    /// Sticky column for vertical movement, so `Up` then `Down` returns to the
    /// same column even through a shorter line.
    goal_col: Option<usize>,
    /// Single-slot yank buffer, written by the `d`-family and read by `Ctrl-y`.
    yanked: String,
    /// Whether newlines are allowed. Titles and dates must stay on one line.
    multiline: bool,
}

impl TextBuffer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn multiline(text: impl Into<String>) -> Self {
        let mut b = Self::new();
        b.text = text.into();
        b.cursor = b.text.chars().count();
        b.multiline = true;
        b
    }

    #[must_use]
    pub fn single_line(text: impl Into<String>) -> Self {
        let mut b = Self::new();
        b.text = text.into();
        b.cursor = b.text.chars().count();
        b
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Trimmed text, which is what fields actually store.
    #[must_use]
    pub fn trimmed(&self) -> String {
        self.text.trim().to_string()
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    #[must_use]
    pub fn yanked(&self) -> &str {
        &self.yanked
    }

    /// Replace the contents and put the cursor at the end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        let mut text = text.into();
        if !self.multiline {
            // Strip newlines so a pasted multi-line value cannot corrupt a
            // single-line field.
            text = text.replace(['\n', '\r'], " ");
        }
        self.text = text;
        self.cursor = self.text.chars().count();
        self.goal_col = None;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.goal_col = None;
    }

    fn byte_at_cursor(&self) -> usize {
        char_to_byte(&self.text, self.cursor)
    }

    /// The cursor's logical line number, column within that line, and the
    /// line's text.
    fn current_line(&self) -> (usize, usize, String) {
        let before = &self.text[..self.byte_at_cursor()];
        let line_no = before.matches('\n').count();
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let line_end = self.text[line_start..]
            .find('\n')
            .map_or(self.text.len(), |i| line_start + i);
        let line = self.text[line_start..line_end].to_string();
        let col = self.text[line_start..self.byte_at_cursor()].chars().count();
        (line_no, col, line)
    }

    /// Number of lines in the buffer.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.text.lines().count().max(1)
    }

    // -- insertion ----------------------------------------------------------

    /// Insert one character at the cursor.
    pub fn insert(&mut self, c: char) {
        if c == '\n' && !self.multiline {
            return;
        }
        let at = self.byte_at_cursor();
        self.text.insert(at, c);
        self.cursor += 1;
        self.goal_col = None;
    }

    /// Insert a run of text, honouring the multiline setting.
    pub fn insert_str(&mut self, s: &str) {
        let cleaned: String = if self.multiline {
            s.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            s.replace(['\n', '\r'], " ")
        };
        if cleaned.is_empty() {
            return;
        }
        let at = self.byte_at_cursor();
        let count = cleaned.chars().count();
        self.text.insert_str(at, &cleaned);
        self.cursor += count;
        self.goal_col = None;
    }

    /// Insert a newline. A no-op in a single-line buffer.
    pub fn newline(&mut self) {
        if self.multiline {
            self.insert('\n');
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_at_cursor();
        let prev = self.text[..at]
            .char_indices()
            .next_back()
            .map_or(0, |(b, _)| b);
        self.text.replace_range(prev..at, "");
        self.cursor -= 1;
        self.goal_col = None;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.char_len() {
            return;
        }
        let at = self.byte_at_cursor();
        let next = self.text[at..]
            .chars()
            .next()
            .map_or(at, |c| at + c.len_utf8());
        self.text.replace_range(at..next, "");
        self.goal_col = None;
    }

    // -- deletion by word and line -----------------------------------------

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Character index of the start of the word before the cursor.
    fn word_start_back(&self, from: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = from;
        // Skip whitespace first.
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i == 0 {
            return 0;
        }
        let target_word = Self::is_word_char(chars[i - 1]);
        while i > 0 && Self::is_word_char(chars[i - 1]) == target_word {
            i -= 1;
        }
        i
    }

    /// Character index just past the word after the cursor.
    fn word_end_forward(&self, from: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let len = chars.len();
        let mut i = from;
        if i >= len {
            return len;
        }
        let target_word = Self::is_word_char(chars[i]);
        while i < len && Self::is_word_char(chars[i]) == target_word {
            i += 1;
        }
        // Consume trailing whitespace too, as vim's `w` does.
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    fn byte_range_to_char_range(&mut self, start: usize, end: usize) -> (usize, usize) {
        let lo = start.min(end);
        let hi = start.max(end).min(self.char_len());
        (lo, hi)
    }

    /// `Ctrl-w`: delete the word before the cursor, into the yank buffer.
    pub fn delete_word_back(&mut self) {
        let start = self.word_start_back(self.cursor);
        if start == self.cursor {
            return;
        }
        let (lo, hi) = self.byte_range_to_char_range(start, self.cursor);
        let from = char_to_byte(&self.text, lo);
        let to = char_to_byte(&self.text, hi);
        self.yanked = self.text[from..to].to_string();
        self.text.replace_range(from..to, "");
        self.cursor = lo;
        self.goal_col = None;
    }

    /// `Alt-b`: move to the start of the previous word.
    pub fn word_left(&mut self) {
        let target = self.word_start_back(self.cursor);
        self.cursor = target;
        self.goal_col = None;
    }

    /// `Alt-f` / `Ctrl-f` within a field: move to the start of the next word.
    pub fn word_right(&mut self) {
        let target = self.word_end_forward(self.cursor);
        self.cursor = target;
        self.goal_col = None;
    }

    /// `Ctrl-a`: start of the current line.
    pub fn line_start(&mut self) {
        let (_, _, _) = self.current_line();
        self.cursor = self.line_start_index();
        self.goal_col = None;
    }

    /// `Ctrl-e`: end of the current line.
    pub fn line_end(&mut self) {
        self.cursor = self.line_end_index();
        self.goal_col = None;
    }

    fn line_start_index(&self) -> usize {
        self.text[..self.byte_at_cursor()]
            .rfind('\n')
            .map_or(0, |i| byte_to_char(&self.text, i + 1))
    }

    fn line_end_index(&self) -> usize {
        let start = char_to_byte(&self.text, self.line_start_index());
        self.text[start..]
            .find('\n')
            .map_or(self.char_len(), |i| byte_to_char(&self.text, start + i))
    }

    /// `Ctrl-u`: delete from the cursor back to the start of the line.
    pub fn delete_to_line_start(&mut self) {
        let start = self.line_start_index();
        if start == self.cursor {
            return;
        }
        let from = char_to_byte(&self.text, start);
        let to = char_to_byte(&self.text, self.cursor);
        self.yanked = self.text[from..to].to_string();
        self.text.replace_range(from..to, "");
        self.cursor = start;
        self.goal_col = None;
    }

    /// `Ctrl-k`: delete from the cursor to the end of the line.
    pub fn delete_to_line_end(&mut self) {
        let end = self.line_end_index();
        if end == self.cursor {
            return;
        }
        let from = char_to_byte(&self.text, self.cursor);
        let to = char_to_byte(&self.text, end);
        self.yanked = self.text[from..to].to_string();
        self.text.replace_range(from..to, "");
        self.goal_col = None;
    }

    /// `Ctrl-y`: paste the most recently deleted text.
    pub fn yank(&mut self) {
        if self.yanked.is_empty() {
            return;
        }
        let yanked = self.yanked.clone();
        self.insert_str(&yanked);
    }

    // -- motion -------------------------------------------------------------

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        self.goal_col = None;
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_len());
        self.goal_col = None;
    }

    pub fn home(&mut self) {
        self.line_start();
    }

    pub fn end(&mut self) {
        self.line_end();
    }

    /// `Up`, preserving the column across shorter lines.
    pub fn up(&mut self) {
        let (line, col, _) = self.current_line();
        if line == 0 {
            return;
        }
        let goal = self.goal_col.unwrap_or(col);
        self.goal_col = Some(goal);

        // Walk to the previous line and clamp the goal column to its length.
        let mut idx = self.line_start_index();
        idx -= 1; // step onto the previous line's final character
        let start = self.text[..char_to_byte(&self.text, idx)]
            .rfind('\n')
            .map_or(0, |i| byte_to_char(&self.text, i + 1));
        let end = idx;
        self.cursor = (start + goal).min(end);
    }

    /// `Down`, preserving the column across shorter lines.
    pub fn down(&mut self) {
        let (line, col, _) = self.current_line();
        if line + 1 >= self.line_count() {
            return;
        }
        let goal = self.goal_col.unwrap_or(col);
        self.goal_col = Some(goal);

        let next_start = self.line_end_index() + 1;
        let next_end = self.text[char_to_byte(&self.text, next_start)..]
            .find('\n')
            .map_or(self.char_len(), |i| {
                byte_to_char(&self.text, char_to_byte(&self.text, next_start) + i)
            });
        self.cursor = (next_start + goal).min(next_end);
    }

    /// Put the cursor at the very end.
    pub fn move_to_end(&mut self) {
        self.cursor = self.char_len();
        self.goal_col = None;
    }

    pub fn move_to_start(&mut self) {
        self.cursor = 0;
        self.goal_col = None;
    }

    // -- rendering ----------------------------------------------------------

    /// Lay the buffer out into wrapped display lines for a field of `width`
    /// columns, starting at display row `scroll`.
    ///
    /// Returns the lines plus where the cursor landed, so the caller can drive
    /// the real terminal cursor.
    #[must_use]
    pub fn wrap(&self, width: usize, scroll: usize) -> Wrapped {
        let width = width.max(1);
        let mut out: Vec<WrappedLine> = Vec::new();
        let mut cursor_row = 0usize;

        let mut base = 0usize; // char index where the current logical line starts
        for (line_no, logical) in self.text.split('\n').enumerate() {
            let line_len = logical.chars().count();

            // Split the logical line into (offset_in_line, text) chunks.
            let mut offset = 0usize;
            for chunk in wrap_line(logical, width) {
                let chunk_len = chunk.chars().count();
                let row = out.len();
                out.push(WrappedLine {
                    logical_line: line_no,
                    text: chunk,
                    continuation: offset > 0,
                    char_offset: base + offset,
                });
                // The cursor belongs to the row whose span contains it. On a
                // wrap boundary prefer the row the cursor is sitting at the
                // start of, matching how the caret is drawn.
                if self.cursor >= base + offset && self.cursor <= base + offset + chunk_len {
                    cursor_row = row;
                }
                offset += chunk_len;
            }

            base += line_len + 1; // +1 for the newline that `split` removed
        }

        if out.is_empty() {
            out.push(WrappedLine {
                logical_line: 0,
                text: String::new(),
                continuation: false,
                char_offset: 0,
            });
        }

        let total_rows = out.len();
        let start = scroll.min(total_rows.saturating_sub(1));
        Wrapped {
            lines: out.into_iter().skip(start).collect(),
            total_rows,
            cursor_row: cursor_row.saturating_sub(start),
        }
    }

    /// The caret's `(row, column)` inside a wrapped layout of `width` columns,
    /// scrolled by `scroll` rows.
    ///
    /// This is what the renderer needs to place the real terminal cursor, and
    /// keeping it here means the wrap logic has exactly one owner.
    #[must_use]
    pub fn caret_position(&self, width: usize, scroll: usize) -> (usize, usize) {
        let w = self.wrap(width, scroll);
        let row = w.cursor_row;
        let Some(line) = w.lines.get(row) else {
            return (row, 0);
        };
        let start = line.char_offset;
        let within = self.cursor.saturating_sub(start);
        let from = char_to_byte(&self.text, start);
        let to = char_to_byte(&self.text, start + within);
        (row, width_of(&self.text[from..to]))
    }

    /// Column of the cursor within its logical line, in terminal columns.
    #[must_use]
    pub fn cursor_screen_col(&self) -> usize {
        let (_, col, logical) = self.current_line();
        width_of(&logical.chars().take(col).collect::<String>())
    }
}

/// One rendered display line of a buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedLine {
    pub logical_line: usize,
    pub text: String,
    /// True for the overflow rows of a logical line that needed wrapping.
    pub continuation: bool,
    /// Character index in the buffer where this row starts.
    pub char_offset: usize,
}

/// The result of laying a buffer out for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapped {
    pub lines: Vec<WrappedLine>,
    pub total_rows: usize,
    /// Row of the cursor within `lines`, after scrolling.
    pub cursor_row: usize,
}

/// Greedy word wrap that breaks over-long words.
fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    let mut word = String::new();
    let mut word_w = 0usize;

    let flush_word = |word: &mut String,
                      word_w: &mut usize,
                      current: &mut String,
                      current_w: &mut usize,
                      chunks: &mut Vec<String>| {
        if word.is_empty() {
            return;
        }
        if *current_w + *word_w > width && !current.is_empty() {
            chunks.push(std::mem::take(current));
            *current_w = 0;
        }
        // A single word longer than the line is hard-split.
        if *word_w > width {
            let mut piece = String::new();
            let mut piece_w = 0usize;
            for c in word.chars() {
                let cw = UnicodeWidthChar::width(c).unwrap_or(0);
                if piece_w + cw > width && !piece.is_empty() {
                    chunks.push(std::mem::take(&mut piece));
                    piece_w = 0;
                }
                piece.push(c);
                piece_w += cw;
            }
            *current = piece;
            *current_w = piece_w;
        } else {
            current.push_str(word);
            *current_w += *word_w;
        }
        word.clear();
        *word_w = 0;
    };

    for c in chars {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if c == ' ' {
            flush_word(
                &mut word,
                &mut word_w,
                &mut current,
                &mut current_w,
                &mut chunks,
            );
            if current_w + cw > width {
                chunks.push(std::mem::take(&mut current));
                current_w = 0;
                // Drop the space at the wrap point.
                continue;
            }
            current.push(c);
            current_w += cw;
        } else {
            word.push(c);
            word_w += cw;
        }
    }
    flush_word(
        &mut word,
        &mut word_w,
        &mut current,
        &mut current_w,
        &mut chunks,
    );

    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> TextBuffer {
        TextBuffer::single_line(s)
    }

    fn text(b: &TextBuffer) -> String {
        b.text().to_string()
    }

    // -- construction --

    #[test]
    fn starts_with_the_cursor_at_the_end() {
        let b = buf("hello");
        assert_eq!(b.cursor(), 5);
        assert_eq!(text(&b), "hello");
    }

    #[test]
    fn set_text_flattens_newlines_in_single_line_fields() {
        let mut b = TextBuffer::new();
        b.set_text("one\ntwo");
        assert_eq!(text(&b), "one two", "a title cannot span lines");
        assert_eq!(b.line_count(), 1);
    }

    #[test]
    fn multiline_buffers_keep_newlines() {
        let mut b = TextBuffer::multiline("a");
        b.newline();
        b.insert_str("b");
        assert_eq!(text(&b), "a\nb");
        assert_eq!(b.line_count(), 2);
    }

    #[test]
    fn single_line_buffers_ignore_newline_input() {
        let mut b = buf("a");
        b.newline();
        b.insert('\n');
        assert_eq!(
            text(&b),
            "a",
            "neither a newline call nor a literal newline inserts"
        );
        b.insert('b');
        assert_eq!(text(&b), "ab");
    }

    // -- insertion --

    #[test]
    fn inserts_at_the_cursor_not_the_end() {
        let mut b = buf("ac");
        b.left();
        b.insert('b');
        assert_eq!(text(&b), "abc");
    }

    #[test]
    fn inserts_multibyte_text_correctly() {
        let mut b = buf("日本");
        b.left();
        b.insert('語');
        assert_eq!(text(&b), "日語本");
        assert_eq!(b.cursor(), 2, "one character, not three bytes");
    }

    #[test]
    fn insert_str_pastes_a_run() {
        let mut b = buf("");
        b.insert_str("hello world");
        assert_eq!(text(&b), "hello world");
        assert_eq!(b.cursor(), 11);
    }

    // -- deletion --

    #[test]
    fn backspace_removes_one_whole_character() {
        let mut b = buf("héllo");
        b.backspace();
        assert_eq!(text(&b), "héll");
    }

    #[test]
    fn backspace_at_the_start_is_a_no_op() {
        let mut b = buf("a");
        b.move_to_start();
        b.backspace();
        assert_eq!(text(&b), "a");
    }

    #[test]
    fn backspace_joins_lines() {
        let mut b = TextBuffer::multiline("one\ntwo");
        b.move_to_start();
        // Cursor is at 0; step to the start of line 1.
        b.down();
        assert_eq!(b.cursor(), 4);
        b.backspace();
        assert_eq!(text(&b), "onetwo");
    }

    #[test]
    fn delete_removes_forward() {
        let mut b = buf("abc");
        b.move_to_start();
        b.delete();
        assert_eq!(text(&b), "bc");
    }

    // -- word operations --

    #[test]
    fn ctrl_w_deletes_the_previous_word() {
        let mut b = buf("one two three");
        b.delete_word_back();
        assert_eq!(text(&b), "one two ");
    }

    #[test]
    fn ctrl_w_deletes_repeatedly_and_stops_cleanly() {
        let mut b = buf("alpha beta");
        b.delete_word_back();
        b.delete_word_back();
        b.delete_word_back(); // nothing left
        assert_eq!(text(&b), "");
    }

    #[test]
    fn ctrl_w_handles_punctuation_as_a_word() {
        let mut b = buf("call foo.bar()");
        b.delete_word_back();
        assert_eq!(text(&b), "call foo.bar", "only `()` is deleted");
        b.delete_word_back();
        assert_eq!(text(&b), "call foo.");
    }

    #[test]
    fn word_left_and_right_travel_by_words() {
        let mut b = buf("one two three");
        b.move_to_start();
        b.word_right();
        assert_eq!(b.cursor(), 4, "start of `two`");
        b.word_right();
        assert_eq!(b.cursor(), 8, "start of `three`");
        b.word_left();
        assert_eq!(b.cursor(), 4);
    }

    // -- line operations --

    #[test]
    fn ctrl_a_and_e_jump_to_line_ends() {
        let mut b = TextBuffer::multiline("hello\nworldly");
        b.move_to_start();
        b.line_end();
        assert_eq!(b.cursor(), 5);
        b.line_start();
        assert_eq!(b.cursor(), 0);
        b.down();
        b.line_end();
        // "worldly" occupies characters 6..=13.
        assert_eq!(b.cursor(), 13);
    }

    #[test]
    fn ctrl_u_deletes_to_line_start() {
        let mut b = TextBuffer::multiline("hello\nworldly");
        b.move_to_end();
        b.line_start();
        b.insert_str("XYZ");
        assert_eq!(text(&b), "hello\nXYZworldly");
        b.delete_to_line_start();
        assert_eq!(text(&b), "hello\nworldly");
        assert_eq!(b.cursor(), 6, "cursor stays at the line start");
    }

    #[test]
    fn ctrl_k_deletes_to_line_end() {
        let mut b = TextBuffer::multiline("hello\nworldly");
        b.move_to_start();
        b.down();
        b.delete_to_line_end();
        assert_eq!(text(&b), "hello\n");
    }

    #[test]
    fn delete_and_yank_round_trips() {
        let mut b = buf("one two");
        b.delete_word_back();
        assert_eq!(b.yanked(), "two");
        b.yank();
        assert_eq!(text(&b), "one two");
    }

    // -- vertical movement --

    #[test]
    fn up_and_down_preserve_the_column() {
        let mut b = TextBuffer::multiline("longest line\nshort\nanother long line");
        b.move_to_start();
        b.end();
        assert_eq!(b.cursor(), 12, "end of `longest line`");
        b.down();
        assert_eq!(
            b.cursor(),
            18,
            "clamped to the end of the short line `short`"
        );
        b.down();
        assert_eq!(
            b.cursor(),
            31,
            "goal column restored on the long final line"
        );
        b.up();
        assert_eq!(b.cursor(), 18, "and preserved on the way back up");
    }

    #[test]
    fn up_at_the_first_line_does_nothing() {
        let mut b = TextBuffer::multiline("a\nb");
        b.move_to_start();
        b.up();
        assert_eq!(b.cursor(), 0);
    }

    #[test]
    fn down_at_the_last_line_does_nothing() {
        let mut b = TextBuffer::multiline("a\nb");
        b.move_to_end();
        let end = b.cursor();
        b.down();
        assert_eq!(b.cursor(), end);
    }

    // -- wrapping --

    #[test]
    fn wrapping_breaks_on_spaces_and_keeps_every_character() {
        let chunks = wrap_line("the quick brown fox jumps", 10);
        assert!(chunks.iter().all(|c| width_of(c) <= 10), "{chunks:?}");
        let rejoined: String = chunks.join("");
        assert_eq!(rejoined.replace(' ', ""), "thequickbrownfoxjumps");
    }

    #[test]
    fn a_word_longer_than_the_line_is_hard_split() {
        let chunks = wrap_line("supercalifragilistic", 5);
        assert!(chunks.iter().all(|c| width_of(c) <= 5), "{chunks:?}");
        assert_eq!(chunks.concat(), "supercalifragilistic");
    }

    #[test]
    fn wrapping_respects_wide_glyphs() {
        let chunks = wrap_line("日本語日本語", 4);
        assert!(chunks.iter().all(|c| width_of(c) <= 4), "{chunks:?}");
        assert_eq!(chunks.concat(), "日本語日本語");
    }

    #[test]
    fn wrapped_output_places_the_cursor() {
        let b = TextBuffer::multiline("hello world again");
        let w = b.wrap(6, 0);
        assert!(w.total_rows >= 3, "should wrap: {w:?}");
        let row_text: String = w.lines[w.cursor_row].text.clone();
        // The cursor is at the very end, which is on the last row.
        assert_eq!(row_text, "again");
    }

    #[test]
    fn scrolling_shifts_the_cursor_row_with_the_viewport() {
        let mut b = TextBuffer::multiline("line one\nline two\nline three\nline four");
        b.move_to_end();
        let top = b.wrap(20, 0);
        assert_eq!(top.cursor_row, top.total_rows - 1, "cursor at the bottom");

        let scrolled = b.wrap(20, 2);
        assert_eq!(
            scrolled.cursor_row,
            top.cursor_row - 2,
            "cursor row tracks the scroll offset"
        );
    }

    #[test]
    fn an_empty_buffer_still_renders_one_row() {
        let b = TextBuffer::multiline("");
        let w = b.wrap(10, 0);
        assert_eq!(w.total_rows, 1);
        assert_eq!(w.lines[0].text, "");
    }

    // -- positions --

    #[test]
    fn caret_position_tracks_the_wrapped_row_and_column() {
        let mut b = TextBuffer::multiline("hello world again");
        b.move_to_start();
        let (row, col) = b.caret_position(6, 0);
        assert_eq!((row, col), (0, 0));

        b.delete_to_line_end();
        b.insert_str("abcdefghijkl");
        let (row, col) = b.caret_position(6, 0);
        assert_eq!(
            (row, col),
            (1, 6),
            "12 columns at width 6 is the end of row 1"
        );

        // With a wide glyph, the column is measured in display columns.
        // `multiline` leaves the cursor at the end, so start from the front.
        let mut c = TextBuffer::multiline("日本語");
        c.move_to_start();
        c.right();
        c.right();
        let (_, col) = c.caret_position(20, 0);
        assert_eq!(col, 4);
    }

    #[test]
    fn cursor_position_is_measured_in_display_columns() {
        let mut b = TextBuffer::multiline("日本語");
        b.move_to_start();
        b.right();
        assert_eq!(b.cursor_screen_col(), 2, "one wide glyph is two columns");
    }
}
