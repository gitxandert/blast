// REPL input line: buffer + cursor, kept free of terminal I/O
// so the editing and redraw logic can be tested
//
#[derive(Debug, Default)]
pub struct LineEditor {
    buf: String,
    // insertion point, 0..=buf.len()
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

    // insert at the cursor and move past it
    pub fn insert(&mut self, c: char) {
        self.buf.insert(self.cur, c);
        self.cur += 1;
    }

    // delete the char left of the cursor
    pub fn backspace(&mut self) {
        if self.cur > 0 {
            self.buf.remove(self.cur - 1);
            self.cur -= 1;
        }
    }

    pub fn left(&mut self) {
        if self.cur > 0 { self.cur -= 1; }
    }

    pub fn right(&mut self) {
        if self.cur < self.buf.len() { self.cur += 1; }
    }

    // replace the line (e.g. from history); cursor goes to the end
    pub fn set(&mut self, line: String) {
        self.buf = line;
        self.cur = self.buf.len();
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.cur = 0;
    }

    // return the line and reset the editor
    pub fn take(&mut self) -> String {
        self.cur = 0;
        std::mem::take(&mut self.buf)
    }

    // exact bytes to print to redraw the line:
    // go to column 0, print marker + line, clear whatever is left
    // of a longer previous line, then step back to the cursor
    // (only when needed: CSI 0 D still moves one column)
    pub fn render(&self, marker: char) -> String {
        let mut out = format!("\r{} {}\x1b[K", marker, self.buf);
        let back = self.buf.len() - self.cur;
        if back > 0 {
            out.push_str(&format!("\x1b[{}D", back));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let out = ed.render('>');
        assert_eq!(out, "\r> abc\x1b[K");
        assert!(!out.contains('D'));
    }

    #[test]
    fn render_moves_back_to_cursor() {
        let ed = editor("abcd", 2);
        let out = ed.render('^');
        assert_eq!(out, "\r^ abcd\x1b[K\x1b[2D");
        assert!(out.ends_with("\x1b[2D"));
    }

    #[test]
    fn render_at_start() {
        let ed = editor("ab", 0);
        assert_eq!(ed.render('X'), "\rX ab\x1b[K\x1b[2D");
    }

    #[test]
    fn render_empty() {
        assert_eq!(LineEditor::new().render('Z'), "\rZ \x1b[K");
    }
}
