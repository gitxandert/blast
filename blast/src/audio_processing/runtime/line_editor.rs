// REPL input line: buffer + cursor, kept free of terminal I/O
// so the editing and redraw logic can be tested
//
// the cursor is a char index (not a byte index), so multibyte
// UTF-8 chars can't split; it's converted to a byte offset for
// String ops. render assumes one column per char, which is wrong
// for wide (e.g. CJK) chars, but commands are ASCII anyway
//
/// What is drawn before the line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// `"{marker} "`
    #[default]
    Normal,
    /// `"{marker}.. "`: the line continues an open block or a `\`.
    Continuation,
    /// Ctrl-R: `"(search 'query') "`, or `"(failed search 'query') "`.
    Search { query: String, failed: bool },
}

// a word ends at whitespace and at these (H7, as INS's completion)
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, ',' | ';' | '{' | '}' | '|' | '=' | '.')
}

#[derive(Debug, Default)]
pub struct LineEditor {
    prompt: Prompt,
    hint: Option<&'static str>,
    // first char shown when the line is wider than the window
    scroll: std::cell::Cell<usize>,
    buf: String,
    // insertion point as a char index, 0..=len()
    cur: usize,
}

impl LineEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn buf(&self) -> &str {
        &self.buf
    }

    pub fn cursor(&self) -> usize {
        self.cur
    }

    // length in chars
    pub fn len(&self) -> usize {
        self.buf.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    // byte offset of char index `idx` (buf.len() past the end)
    fn byte_idx(&self, idx: usize) -> usize {
        self.buf
            .char_indices()
            .nth(idx)
            .map_or(self.buf.len(), |(i, _)| i)
    }

    // insert at the cursor and move past it;
    // control chars are dropped
    pub fn insert(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        let at = self.byte_idx(self.cur);
        self.buf.insert(at, c);
        self.cur += 1;
    }

    // delete the char left of the cursor
    pub fn backspace(&mut self) {
        if self.cur > 0 {
            let at = self.byte_idx(self.cur - 1);
            self.buf.remove(at);
            self.cur -= 1;
        }
    }

    // delete the char under the cursor
    pub fn delete(&mut self) {
        if self.cur < self.len() {
            let at = self.byte_idx(self.cur);
            self.buf.remove(at);
        }
    }

    pub fn home(&mut self) {
        self.cur = 0;
    }

    pub fn end(&mut self) {
        self.cur = self.len();
    }

    pub fn left(&mut self) {
        if self.cur > 0 {
            self.cur -= 1;
        }
    }

    pub fn right(&mut self) {
        if self.cur < self.len() {
            self.cur += 1;
        }
    }

    // replace the line (e.g. from history); cursor goes to the end
    pub fn set(&mut self, line: String) {
        self.buf = line;
        self.cur = self.len();
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.cur = 0;
    }

    pub fn set_prompt(&mut self, p: Prompt) {
        self.prompt = p;
    }

    /// Dim text after the line (the Ctrl-C reminder); not part of the line.
    pub fn set_hint(&mut self, hint: Option<&'static str>) {
        self.hint = hint;
    }

    // char index of the start of the word before `cur` (separators skipped)
    fn word_start(&self) -> usize {
        let chars: Vec<char> = self.buf.chars().collect();
        let mut i = self.cur;
        while i > 0 && is_separator(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && !is_separator(chars[i - 1]) {
            i -= 1;
        }
        i
    }

    /// Moves to the start of the word before the cursor.
    pub fn word_left(&mut self) {
        self.cur = self.word_start();
    }

    /// Moves to the end of the next word.
    pub fn word_right(&mut self) {
        let chars: Vec<char> = self.buf.chars().collect();
        let mut i = self.cur;
        while i < chars.len() && is_separator(chars[i]) {
            i += 1;
        }
        while i < chars.len() && !is_separator(chars[i]) {
            i += 1;
        }
        self.cur = i;
    }

    /// The word touching the cursor as char indices `(start, end)`: the run
    /// of non-separator chars around it (empty when it sits between
    /// separators).
    pub fn word_bounds(&self) -> (usize, usize) {
        let chars: Vec<char> = self.buf.chars().collect();
        let mut start = self.cur;
        while start > 0 && !is_separator(chars[start - 1]) {
            start -= 1;
        }
        let mut end = self.cur;
        while end < chars.len() && !is_separator(chars[end]) {
            end += 1;
        }
        (start, end)
    }

    /// Replaces the chars in `start..end` (char indices) with `with` and
    /// puts the cursor after it.
    pub fn replace_word(&mut self, start: usize, end: usize, with: &str) {
        let end = end.min(self.len());
        let start = start.min(end);
        let (a, b) = (self.byte_idx(start), self.byte_idx(end));
        self.buf.replace_range(a..b, with);
        self.cur = start + with.chars().count();
    }

    // delete the chars in `from..to` (char indices) and put the cursor at `from`
    fn kill(&mut self, from: usize, to: usize) {
        let (a, b) = (self.byte_idx(from), self.byte_idx(to));
        self.buf.replace_range(a..b, "");
        self.cur = from;
    }

    /// Ctrl-W: deletes from the start of the previous word to the cursor.
    pub fn kill_word_back(&mut self) {
        self.kill(self.word_start(), self.cur);
    }

    /// Ctrl-K: deletes from the cursor to the end of the line.
    pub fn kill_to_end(&mut self) {
        self.kill(self.cur, self.len());
    }

    /// Ctrl-U: deletes from the start of the line to the cursor.
    pub fn kill_to_start(&mut self) {
        self.kill(0, self.cur);
    }

    // return the line and reset the editor
    pub fn take(&mut self) -> String {
        self.cur = 0;
        std::mem::take(&mut self.buf)
    }

    // exact bytes to print to redraw the line:
    // go to column 0, print marker + line, clear whatever is left
    // of a longer previous line, then step back to the cursor
    // (only when needed: CSI 0 D still moves one column).
    // The row never exceeds width - 1 columns, so it can't wrap: a long
    // line shows the window holding the cursor, which scrolls by the least
    // amount when the cursor leaves it
    pub fn render(&self, marker: char, width: usize) -> String {
        let prefix = match &self.prompt {
            Prompt::Normal => format!("{marker} "),
            Prompt::Continuation => format!("{marker}.. "),
            Prompt::Search { query, failed } => {
                // a long query shows its tail so the prompt leaves room
                let room = width.saturating_sub(22).max(1);
                let skip = query.chars().count().saturating_sub(room);
                let q: String = query.chars().skip(skip).collect();
                let word = if *failed { "failed search" } else { "search" };
                format!("({word} '{q}') ")
            }
        };
        let prefix_len = prefix.chars().count();
        let len = self.len();
        // chars that fit after the prompt, keeping a column for the cursor
        let cap = width.saturating_sub(prefix_len + 1).max(1);
        let mut off = self.scroll.get().min(len);
        if self.cur < off {
            off = self.cur;
        } else if self.cur >= off + cap {
            off = self.cur + 1 - cap;
        }
        self.scroll.set(off);
        let shown: String = self.buf.chars().skip(off).take(cap).collect();
        let shown_len = shown.chars().count();
        let end = off + shown_len;
        let mut out = format!("\r{prefix}{shown}");
        // the hint takes what is left of the row (a space, then its text)
        let room = width
            .saturating_sub(1)
            .saturating_sub(prefix_len + shown_len + 1);
        let mut hint_cols = 0;
        if let Some(hint) = self.hint.filter(|_| room > 0) {
            let text: String = hint.chars().take(room).collect();
            hint_cols = 1 + text.chars().count();
            out.push_str(&format!("\x1b[2m {text}\x1b[0m"));
        }
        out.push_str("\x1b[K");
        let back = end - self.cur + hint_cols;
        if back > 0 {
            out.push_str(&format!("\x1b[{}D", back));
        }
        out
    }
}

// one keypress, mapped from a crossterm event (see `terminal::key_from`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Interrupt,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    /// Ctrl/Alt + Left, Alt-B
    WordLeft,
    /// Ctrl/Alt + Right, Alt-F
    WordRight,
    /// Ctrl-W
    KillWordBack,
    /// Ctrl-K
    KillToEnd,
    /// Ctrl-U
    KillToStart,
    /// Ctrl-R
    Search,
    /// Esc, Ctrl-G
    Cancel,
    /// Tab: complete the word
    Tab,
    // a key we don't bind
    Ignore,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_home_end() {
        let mut ed = editor("abc", 1);
        ed.delete();
        assert_eq!(ed.buf(), "ac");
        assert_eq!(ed.cursor(), 1);
        ed.end();
        assert_eq!(ed.cursor(), 2);
        ed.delete();
        assert_eq!(ed.buf(), "ac");
        ed.home();
        assert_eq!(ed.cursor(), 0);
        ed.delete();
        assert_eq!(ed.buf(), "c");
        assert_eq!(ed.cursor(), 0);
    }

    fn editor(line: &str, cur: usize) -> LineEditor {
        let mut ed = LineEditor::new();
        ed.set(line.to_string());
        ed.cur = cur;
        ed
    }

    #[test]
    fn insert_at_end() {
        let mut ed = LineEditor::new();
        ed.insert('a');
        ed.insert('b');
        assert_eq!(ed.buf(), "ab");
        assert_eq!(ed.cursor(), 2);
    }

    #[test]
    fn insert_in_middle() {
        let mut ed = editor("ac", 1);
        ed.insert('b');
        assert_eq!(ed.buf(), "abc");
        assert_eq!(ed.cursor(), 2);
    }

    #[test]
    fn insert_at_start() {
        let mut ed = editor("bc", 0);
        ed.insert('a');
        assert_eq!(ed.buf(), "abc");
        assert_eq!(ed.cursor(), 1);
    }

    #[test]
    fn backspace_at_end() {
        let mut ed = editor("abc", 3);
        ed.backspace();
        assert_eq!(ed.buf(), "ab");
        assert_eq!(ed.cursor(), 2);
    }

    #[test]
    fn backspace_in_middle_deletes_left_of_cursor() {
        let mut ed = editor("abc", 2);
        ed.backspace();
        assert_eq!(ed.buf(), "ac");
        assert_eq!(ed.cursor(), 1);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut ed = editor("abc", 0);
        ed.backspace();
        assert_eq!(ed.buf(), "abc");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn backspace_on_empty_is_noop() {
        let mut ed = LineEditor::new();
        ed.backspace();
        assert_eq!(ed.buf(), "");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn backspace_and_insert_agree_after_moving_left() {
        let mut ed = editor("abc", 3);
        ed.left();
        ed.backspace();
        assert_eq!(ed.buf(), "ac");
        ed.insert('x');
        assert_eq!(ed.buf(), "axc");
        assert_eq!(ed.cursor(), 2);
    }

    #[test]
    fn left_right_clamp() {
        let mut ed = editor("ab", 0);
        ed.left();
        assert_eq!(ed.cursor(), 0);
        ed.right();
        ed.right();
        ed.right();
        assert_eq!(ed.cursor(), 2);
        ed.left();
        assert_eq!(ed.cursor(), 1);
    }

    #[test]
    fn set_puts_cursor_at_end() {
        let mut ed = editor("xyz", 1);
        ed.set("load kick".to_string());
        assert_eq!(ed.buf(), "load kick");
        assert_eq!(ed.cursor(), 9);
    }

    #[test]
    fn take_and_clear_reset() {
        let mut ed = editor("abc", 1);
        assert_eq!(ed.take(), "abc");
        assert_eq!(ed.buf(), "");
        assert_eq!(ed.cursor(), 0);

        let mut ed = editor("abc", 2);
        ed.clear();
        assert_eq!(ed.buf(), "");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn render_at_end_has_no_move_back() {
        let ed = editor("abc", 3);
        let out = ed.render('>', 80);
        assert_eq!(out, "\r> abc\x1b[K");
        assert!(!out.contains('D'));
    }

    #[test]
    fn render_moves_back_to_cursor() {
        let ed = editor("abcd", 2);
        let out = ed.render('^', 80);
        assert_eq!(out, "\r^ abcd\x1b[K\x1b[2D");
        assert!(out.ends_with("\x1b[2D"));
    }

    #[test]
    fn render_at_start() {
        let ed = editor("ab", 0);
        assert_eq!(ed.render('X', 80), "\rX ab\x1b[K\x1b[2D");
    }

    #[test]
    fn render_empty() {
        assert_eq!(LineEditor::new().render('Z', 80), "\rZ \x1b[K");
    }

    #[test]
    fn render_short_line_unchanged() {
        let ed = editor("abc", 3);
        assert_eq!(ed.render('>', 80), "\r> abc\x1b[K");
    }

    // visible chars of a render (between "{marker} " and the clear)
    fn visible(out: &str) -> &str {
        let body = out.strip_prefix('\r').unwrap();
        &body[2..body.find('\x1b').unwrap()]
    }

    #[test]
    fn render_fits_width() {
        let text: String = ('a'..='z').cycle().take(200).collect();
        let mut ed = editor(&text, 200);
        let out = ed.render('>', 40);
        assert!(2 + visible(&out).chars().count() <= 39);
        assert!(text.ends_with(visible(&out)));
        assert!(out.ends_with("\x1b[K"));
        ed.home();
        let out = ed.render('>', 40);
        assert!(text.starts_with(visible(&out)));
        assert!(2 + visible(&out).chars().count() <= 39);
        // moving right inside the window doesn't scroll; past the edge, by one
        for _ in 0..36 {
            ed.right();
        }
        let before = visible(&ed.render('>', 40)).to_string();
        ed.right();
        let after = visible(&ed.render('>', 40)).to_string();
        assert_eq!(before[1..], after[..after.len() - 1]);
        assert_ne!(before, after);
    }

    #[test]
    fn replace_word_in_middle_of_line() {
        let mut ed = editor("voice ki hat", 8);
        assert_eq!(ed.word_bounds(), (6, 8));
        ed.replace_word(6, 8, "kick");
        assert_eq!(ed.buf(), "voice kick hat");
        assert_eq!(ed.cursor(), 10);
        // the cursor can sit inside a word: the whole word is its span
        let mut ed = editor("a loopend=3", 4);
        assert_eq!(ed.word_bounds(), (2, 9));
        ed.replace_word(2, 9, "é");
        assert_eq!(ed.buf(), "a é=3");
        assert_eq!(ed.cursor(), 3);
        // between separators the word is empty
        let ed = editor("a, b", 2);
        assert_eq!(ed.word_bounds(), (2, 2));
    }

    #[test]
    fn word_left_right_stop_at_separators() {
        let text = "kick { lfo w to=pan.x }";
        let mut ed = editor(text, text.chars().count());
        let mut stops = Vec::new();
        for _ in 0..7 {
            ed.word_left();
            stops.push(ed.cursor());
        }
        assert_eq!(stops, [20, 16, 13, 11, 7, 0, 0]);
        let mut stops = Vec::new();
        for _ in 0..8 {
            ed.word_right();
            stops.push(ed.cursor());
        }
        assert_eq!(stops, [4, 10, 12, 15, 19, 21, 23, 23]);
    }

    #[test]
    fn kill_word_back_stops_at_separator() {
        let mut ed = editor("kick.beat", 9);
        ed.kill_word_back();
        assert_eq!(ed.buf(), "kick.");
        assert_eq!(ed.cursor(), 5);
        ed.kill_word_back();
        assert_eq!(ed.buf(), "");
        let mut ed = editor("play  kick ", 11);
        ed.kill_word_back();
        assert_eq!(ed.buf(), "play  ");
    }

    #[test]
    fn kill_to_end_and_start() {
        let mut ed = editor("abcdé", 2);
        ed.kill_to_end();
        assert_eq!(ed.buf(), "ab");
        assert_eq!(ed.cursor(), 2);
        let mut ed = editor("abcdé", 3);
        ed.kill_to_start();
        assert_eq!(ed.buf(), "dé");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn render_continuation_and_search_prompts_fit_width() {
        let mut ed = editor("abc", 3);
        ed.set_prompt(Prompt::Continuation);
        assert_eq!(ed.render('>', 80), "\r>.. abc\x1b[K");
        ed.set_prompt(Prompt::Search {
            query: "kk".into(),
            failed: false,
        });
        assert_eq!(ed.render('>', 80), "\r(search 'kk') abc\x1b[K");
        ed.set_prompt(Prompt::Search {
            query: "kk".into(),
            failed: true,
        });
        assert!(ed.render('>', 80).starts_with("\r(failed search 'kk') abc"));

        // a long line and a long query still fit in width - 1 columns
        let text: String = ('a'..='z').cycle().take(200).collect();
        for width in [30, 40, 80] {
            for prompt in [
                Prompt::Continuation,
                Prompt::Search {
                    query: text.clone(),
                    failed: true,
                },
            ] {
                let mut ed = editor(&text, 200);
                ed.set_prompt(prompt);
                for _ in 0..3 {
                    let out = ed.render('>', width);
                    let row = out.trim_start_matches('\r');
                    let row = &row[..row.find('\x1b').unwrap()];
                    assert!(row.chars().count() < width, "{width}: {row}");
                    ed.home();
                }
            }
        }
    }

    #[test]
    fn render_hint_after_line_fits_width() {
        let mut ed = editor("ab", 2);
        ed.set_hint(Some("hint"));
        assert_eq!(ed.render('>', 80), "\r> ab\x1b[2m hint\x1b[0m\x1b[K\x1b[5D");
        ed.set_hint(None);
        assert_eq!(ed.render('>', 80), "\r> ab\x1b[K");
        // too narrow for all of it: truncated, never wider than width - 1
        ed.set_hint(Some("a long reminder"));
        let out = ed.render('>', 12);
        assert!(out.contains("\x1b[2m a long\x1b[0m"), "{out:?}");
        // no room at all: no hint
        let out = ed.render('>', 6);
        assert!(!out.contains("\x1b[2m"), "{out:?}");
        // the hint is not part of the line
        assert_eq!(ed.buf(), "ab");
    }

    // apply the editing keys the REPL thread handles
    fn feed(ed: &mut LineEditor, keys: &[Key]) {
        for key in keys {
            match *key {
                Key::Char(c) => ed.insert(c),
                Key::Backspace => ed.backspace(),
                Key::Delete => ed.delete(),
                Key::Left => ed.left(),
                Key::Right => ed.right(),
                Key::Home => ed.home(),
                Key::End => ed.end(),
                _ => (),
            }
            assert!(ed.cursor() <= ed.len());
        }
    }

    fn chars(s: &str) -> Vec<Key> {
        s.chars().map(Key::Char).collect()
    }

    #[test]
    fn non_ascii_editing_never_panics() {
        let mut ed = LineEditor::new();
        feed(&mut ed, &chars("éa"));
        assert_eq!(ed.buf(), "éa");
        assert_eq!(ed.cursor(), 2);

        feed(&mut ed, &[Key::Left]);
        feed(&mut ed, &chars("ü"));
        assert_eq!(ed.buf(), "éüa");
        assert_eq!(ed.cursor(), 2);

        feed(&mut ed, &[Key::Backspace, Key::Backspace]);
        assert_eq!(ed.buf(), "a");
        assert_eq!(ed.cursor(), 0);

        feed(&mut ed, &[Key::Right, Key::Right, Key::Left]);
        assert_eq!(ed.cursor(), 0);
        feed(&mut ed, &chars("é"));
        feed(&mut ed, &[Key::Delete]); // delete 'a'
        assert_eq!(ed.buf(), "é");
        assert_eq!(ed.cursor(), 1);
    }

    #[test]
    fn control_chars_stay_out_of_buffer() {
        let mut ed = LineEditor::new();
        feed(&mut ed, &[Key::Char('a'), Key::Char('\t'), Key::Char('b')]);
        assert_eq!(ed.buf(), "ab");
        ed.insert('\t');
        assert_eq!(ed.buf(), "ab");
    }

    #[test]
    fn non_ascii_set_and_render_count_chars() {
        let mut ed = LineEditor::new();
        ed.set("éé".to_string());
        assert_eq!(ed.cursor(), 2);
        assert_eq!(ed.render('>', 80), "\r> éé\x1b[K");
        ed.home();
        assert_eq!(ed.render('>', 80), "\r> éé\x1b[K\x1b[2D");
        ed.end();
        assert_eq!(ed.cursor(), 2);
    }
}
