// REPL input line: buffer + cursor, kept free of terminal I/O
// so the editing and redraw logic can be tested
//
// the cursor is a char index (not a byte index), so multibyte
// UTF-8 chars can't split; it's converted to a byte offset for
// String ops. render assumes one column per char, which is wrong
// for wide (e.g. CJK) chars, but commands are ASCII anyway
//
#[derive(Debug, Default)]
pub struct LineEditor {
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

    // byte offset of char index `idx` (buf.len() past the end)
    fn byte_idx(&self, idx: usize) -> usize {
        self.buf.char_indices().nth(idx).map_or(self.buf.len(), |(i, _)| i)
    }

    // insert at the cursor and move past it;
    // control chars are dropped
    pub fn insert(&mut self, c: char) {
        if c.is_control() { return; }
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
        if self.cur > 0 { self.cur -= 1; }
    }

    pub fn right(&mut self) {
        if self.cur < self.len() { self.cur += 1; }
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
        let back = self.len() - self.cur;
        if back > 0 {
            out.push_str(&format!("\x1b[{}D", back));
        }
        out
    }
}

// one decoded keypress from the raw input stream
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
    // unknown/unsupported escape sequence (consumed in full), or end of input
    Ignore,
}

// read one keypress from a raw byte stream
//
// escape sequences are consumed in full, so no trailing
// bytes (e.g. the '~' of ESC[3~) leak into the line.
// an ESC that doesn't start a CSI (ESC[) or SS3 (ESCO) sequence
// is dropped and the following byte is read as a normal key,
// so Alt+x (ESC x) types 'x' and ESC ESC [ D is still Left.
// printable ASCII and complete UTF-8 chars become Key::Char;
// other control bytes (Tab, Ctrl-D, ...) and malformed UTF-8 are Ignore
// (a malformed sequence also eats the byte that broke it)
pub fn parse_key<I: Iterator<Item = u8>>(bytes: &mut I) -> Key {
    let mut b = match bytes.next() {
        Some(b) => b,
        None => return Key::Ignore,
    };

    while b == 27 {
        match bytes.next() {
            Some(b'[') => return parse_csi(bytes),
            Some(b'O') => return parse_ss3(bytes),
            Some(next) => b = next,
            None => return Key::Ignore,
        }
    }

    match b {
        b'\n' | b'\r' => Key::Enter,
        127 | 8 => Key::Backspace,
        3 => Key::Interrupt,
        0x20..=0x7E => Key::Char(b as char),
        0xC2..=0xF4 => parse_utf8(b, bytes),
        _ => Key::Ignore,
    }
}

// rest of a multibyte UTF-8 char after its lead byte
fn parse_utf8<I: Iterator<Item = u8>>(lead: u8, bytes: &mut I) -> Key {
    let n = match lead {
        0xF0.. => 4,
        0xE0.. => 3,
        _ => 2,
    };
    let mut enc = [lead, 0, 0, 0];
    for i in 1..n {
        match bytes.next() {
            Some(c) if c & 0xC0 == 0x80 => enc[i] = c,
            _ => return Key::Ignore,
        }
    }
    match std::str::from_utf8(&enc[..n]).ok().and_then(|s| s.chars().next()) {
        Some(c) if !c.is_control() => Key::Char(c),
        _ => Key::Ignore,
    }
}

// after ESC[: parameter/intermediate bytes (0x20-0x3F),
// then one final byte (0x40-0x7E)
fn parse_csi<I: Iterator<Item = u8>>(bytes: &mut I) -> Key {
    let mut params = Vec::new();
    let fin = loop {
        match bytes.next() {
            Some(b @ 0x20..=0x3F) => params.push(b),
            Some(b) => break b,
            None => return Key::Ignore,
        }
    };

    match (params.as_slice(), fin) {
        (b"", b'A') => Key::Up,
        (b"", b'B') => Key::Down,
        (b"", b'C') => Key::Right,
        (b"", b'D') => Key::Left,
        (b"", b'H') | (b"1", b'~') | (b"7", b'~') => Key::Home,
        (b"", b'F') | (b"4", b'~') | (b"8", b'~') => Key::End,
        (b"3", b'~') => Key::Delete,
        _ => Key::Ignore,
    }
}

// after ESCO: one final byte (application cursor mode)
fn parse_ss3<I: Iterator<Item = u8>>(bytes: &mut I) -> Key {
    match bytes.next() {
        Some(b'A') => Key::Up,
        Some(b'B') => Key::Down,
        Some(b'C') => Key::Right,
        Some(b'D') => Key::Left,
        Some(b'H') => Key::Home,
        Some(b'F') => Key::End,
        _ => Key::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // parse a whole byte stream; a trailing Ignore (end of input) is dropped
    fn keys(bytes: &[u8]) -> Vec<Key> {
        let mut it = bytes.iter().copied();
        let mut out = Vec::new();
        while it.len() > 0 {
            out.push(parse_key(&mut it));
        }
        out
    }

    #[test]
    fn parse_arrows() {
        assert_eq!(keys(&[27, 91, 68]), vec![Key::Left]);
        assert_eq!(keys(&[27, 91, 67]), vec![Key::Right]);
        assert_eq!(keys(&[27, 91, 65]), vec![Key::Up]);
        assert_eq!(keys(&[27, 91, 66]), vec![Key::Down]);
        assert_eq!(keys(&[27, b'O', b'D']), vec![Key::Left]);
    }

    #[test]
    fn parse_delete_leaves_no_tilde() {
        assert_eq!(keys(&[27, 91, 51, 126]), vec![Key::Delete]);
        assert_eq!(keys(&[27, 91, 51, 126, b'a']), vec![Key::Delete, Key::Char('a')]);
    }

    #[test]
    fn parse_home_end() {
        assert_eq!(keys(b"\x1b[H\x1b[F"), vec![Key::Home, Key::End]);
        assert_eq!(keys(b"\x1b[1~\x1b[4~"), vec![Key::Home, Key::End]);
        assert_eq!(keys(b"\x1bOH\x1bOF"), vec![Key::Home, Key::End]);
    }

    #[test]
    fn parse_unknown_csi_consumed_in_full() {
        // F5 = ESC[15~, Ctrl+Right = ESC[1;5C
        assert_eq!(keys(b"\x1b[15~a"), vec![Key::Ignore, Key::Char('a')]);
        assert_eq!(keys(b"\x1b[1;5Cb"), vec![Key::Ignore, Key::Char('b')]);
    }

    #[test]
    fn parse_esc_then_key_keeps_key() {
        assert_eq!(keys(&[27, 120]), vec![Key::Char('x')]);
        assert_eq!(keys(&[27, 27, 91, 68]), vec![Key::Left]);
        assert_eq!(keys(&[27, 13]), vec![Key::Enter]);
    }

    #[test]
    fn parse_plain_keys() {
        assert_eq!(keys(&[127]), vec![Key::Backspace]);
        assert_eq!(keys(&[13]), vec![Key::Enter]);
        assert_eq!(keys(&[10]), vec![Key::Enter]);
        assert_eq!(keys(&[3]), vec![Key::Interrupt]);
        assert_eq!(keys(b"ab"), vec![Key::Char('a'), Key::Char('b')]);
    }

    #[test]
    fn parse_truncated_or_empty_is_ignored() {
        assert_eq!(keys(&[27, 91]), vec![Key::Ignore]);
        assert_eq!(keys(&[27]), vec![Key::Ignore]);
        assert_eq!(parse_key(&mut [0u8; 0].iter().copied()), Key::Ignore);
    }

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

    // apply the editing keys the REPL thread handles
    fn feed(ed: &mut LineEditor, bytes: &[u8]) {
        let mut it = bytes.iter().copied();
        while it.len() > 0 {
            match parse_key(&mut it) {
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

    #[test]
    fn parse_utf8_chars() {
        assert_eq!(keys("é".as_bytes()), vec![Key::Char('é')]);
        assert_eq!(keys("€".as_bytes()), vec![Key::Char('€')]);
        assert_eq!(keys("🥁a".as_bytes()), vec![Key::Char('🥁'), Key::Char('a')]);
    }

    #[test]
    fn parse_bad_utf8_and_control_bytes_ignored() {
        // stray continuation byte, invalid lead, truncated char
        assert_eq!(keys(&[0xA9, b'a']), vec![Key::Ignore, Key::Char('a')]);
        assert_eq!(keys(&[0xFF, b'a']), vec![Key::Ignore, Key::Char('a')]);
        assert_eq!(keys(&[0xC3]), vec![Key::Ignore]);
        // tab, ctrl-d, NUL
        assert_eq!(keys(&[9, 4, 0]), vec![Key::Ignore, Key::Ignore, Key::Ignore]);
    }

    #[test]
    fn non_ascii_editing_never_panics() {
        let mut ed = LineEditor::new();
        feed(&mut ed, &[0xC3, 0xA9, b'a']);
        assert_eq!(ed.buf(), "éa");
        assert_eq!(ed.cursor(), 2);

        feed(&mut ed, &[27, b'[', b'D']); // left
        feed(&mut ed, "ü".as_bytes());
        assert_eq!(ed.buf(), "éüa");
        assert_eq!(ed.cursor(), 2);

        feed(&mut ed, &[127, 127]);
        assert_eq!(ed.buf(), "a");
        assert_eq!(ed.cursor(), 0);

        feed(&mut ed, &[27, b'[', b'C', 27, b'[', b'C', 27, b'[', b'D']);
        assert_eq!(ed.cursor(), 0);
        feed(&mut ed, "é".as_bytes());
        feed(&mut ed, &[27, b'[', b'3', b'~']); // delete 'a'
        assert_eq!(ed.buf(), "é");
        assert_eq!(ed.cursor(), 1);
    }

    #[test]
    fn control_bytes_stay_out_of_buffer() {
        let mut ed = LineEditor::new();
        feed(&mut ed, &[b'a', 9, 4, b'b']);
        assert_eq!(ed.buf(), "ab");
        ed.insert('\t');
        assert_eq!(ed.buf(), "ab");
    }

    #[test]
    fn non_ascii_set_and_render_count_chars() {
        let mut ed = LineEditor::new();
        ed.set("éé".to_string());
        assert_eq!(ed.cursor(), 2);
        assert_eq!(ed.render('>'), "\r> éé\x1b[K");
        ed.home();
        assert_eq!(ed.render('>'), "\r> éé\x1b[K\x1b[2D");
        ed.end();
        assert_eq!(ed.cursor(), 2);
    }
}
