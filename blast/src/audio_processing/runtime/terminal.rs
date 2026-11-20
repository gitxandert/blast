// The screen: prompt marker, input line, and the only path to stdout.
//
// Every line blast prints goes through `Term::print_line`, which clears the
// prompt, prints, and redraws it, all under one lock so lines from different
// threads never interleave. A later layout change (e.g. a split screen)
// touches only this file.

use super::line_editor::{Key, LineEditor};
use crossterm::{
    cursor::MoveTo,
    event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode, size},
};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex, MutexGuard},
};

// the animated prompt marker
const MARKERS: [char; 7] = ['^', 'X', 'v', '>', 'X', '<', 'Z'];

struct Screen {
    editor: LineEditor,
    marker: usize,
    raw: bool,
}

impl Screen {
    fn new(raw: bool) -> Self {
        Screen {
            editor: LineEditor::new(),
            marker: 0,
            raw,
        }
    }

    fn marker_char(&self) -> char {
        MARKERS[self.marker]
    }

    fn tick(&mut self) {
        self.marker = (self.marker + 1) % MARKERS.len();
    }

    // the exact bytes `print_line` writes: clear the prompt line, `text` with
    // each '\n' as "\r\n" (raw mode doesn't translate), a final "\r\n", then
    // the prompt
    fn layout(text: &str, editor: &LineEditor, marker: char, width: usize) -> String {
        let mut out = String::from("\r\x1b[K");
        for line in text.split('\n') {
            out.push_str(line);
            out.push_str("\r\n");
        }
        out.push_str(&editor.render(marker, width));
        out
    }

    fn echo_text(marker: char, line: &str, continuation: bool) -> String {
        let dots = if continuation { ".." } else { "" };
        format!("{marker}{dots} {line}")
    }

    fn write(bytes: &str) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(bytes.as_bytes());
        let _ = out.flush();
    }

    // the terminal's width, read on each redraw (80 if unknown)
    fn width() -> usize {
        match size() {
            Ok((w, _)) if w > 0 => w as usize,
            _ => 80,
        }
    }

    fn redraw(&self) {
        Self::write(&self.editor.render(self.marker_char(), Self::width()));
    }

    fn restore(&mut self) {
        if self.raw {
            self.raw = false;
            Self::write("\r\x1b[K");
            let _ = disable_raw_mode();
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.restore();
    }
}

/// The screen: prompt marker, input line, and the only path to stdout.
#[derive(Clone)]
pub struct Term {
    screen: Arc<Mutex<Screen>>,
}

impl Term {
    /// Enters raw mode (restored by `restore`, the panic hook, or drop of the last handle).
    pub fn enter() -> io::Result<Term> {
        enable_raw_mode()?;
        // a panic anywhere must give the terminal back before its message
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            previous(info);
        }));
        Ok(Term {
            screen: Arc::new(Mutex::new(Screen::new(true))),
        })
    }

    // a panic elsewhere must not stop output
    fn lock(&self) -> MutexGuard<'_, Screen> {
        self.screen.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Prints `text` (any number of lines) above the prompt, then redraws it.
    pub fn print_line(&self, text: &str) {
        let screen = self.lock();
        let marker = screen.marker_char();
        Screen::write(&Screen::layout(
            text,
            &screen.editor,
            marker,
            Screen::width(),
        ));
    }

    /// Wipes the window (and its scrollback) and redraws the prompt.
    pub fn clear(&self) {
        let screen = self.lock();
        let mut out = io::stdout().lock();
        let _ = crossterm::queue!(
            out,
            Clear(ClearType::All),
            Clear(ClearType::Purge),
            MoveTo(0, 0)
        );
        let _ = out.write_all(
            screen
                .editor
                .render(screen.marker_char(), Screen::width())
                .as_bytes(),
        );
        let _ = out.flush();
    }

    /// Prints `"{marker} {line}"` (`"{marker}.. {line}"` for a continuation
    /// line) above the prompt, with the marker the
    /// screen shows now (a sent line keeps the character it had at Enter).
    pub fn echo(&self, line: &str, continuation: bool) {
        let screen = self.lock();
        let marker = screen.marker_char();
        let text = Screen::echo_text(marker, line, continuation);
        Screen::write(&Screen::layout(
            &text,
            &screen.editor,
            marker,
            Screen::width(),
        ));
    }

    /// Runs `f` on the line editor, then redraws.
    pub fn edit<R>(&self, f: impl FnOnce(&mut LineEditor) -> R) -> R {
        let mut screen = self.lock();
        let result = f(&mut screen.editor);
        screen.redraw();
        result
    }

    /// Advances the prompt marker and redraws (the redraw thread calls it).
    pub fn tick(&self) {
        let mut screen = self.lock();
        screen.tick();
        screen.redraw();
    }

    /// Clears the prompt and leaves raw mode. Idempotent.
    pub fn restore(&self) {
        self.lock().restore();
    }
}

/// One key, from a crossterm event (`None` for releases, repeats we ignore, and keys we don't bind).
pub fn key_from(event: &KeyEvent) -> Option<Key> {
    if event.kind != KeyEventKind::Press {
        return None;
    }
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    match event.code {
        KeyCode::Char('c') if ctrl => Some(Key::Interrupt),
        KeyCode::Char('a') if ctrl => Some(Key::Home),
        KeyCode::Char('e') if ctrl => Some(Key::End),
        KeyCode::Char('w') if ctrl => Some(Key::KillWordBack),
        KeyCode::Char('k') if ctrl => Some(Key::KillToEnd),
        KeyCode::Char('u') if ctrl => Some(Key::KillToStart),
        KeyCode::Char('r') if ctrl => Some(Key::Search),
        KeyCode::Char('g') if ctrl => Some(Key::Cancel),
        KeyCode::Char('b') if alt && !ctrl => Some(Key::WordLeft),
        KeyCode::Char('f') if alt && !ctrl => Some(Key::WordRight),
        KeyCode::Char(c) if !ctrl && !alt => Some(Key::Char(c)),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Delete => Some(Key::Delete),
        KeyCode::Home => Some(Key::Home),
        KeyCode::End => Some(Key::End),
        KeyCode::Left if ctrl || alt => Some(Key::WordLeft),
        KeyCode::Right if ctrl || alt => Some(Key::WordRight),
        KeyCode::Left => Some(Key::Left),
        KeyCode::Right => Some(Key::Right),
        KeyCode::Esc => Some(Key::Cancel),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn key_from_maps_bindings() {
        let none = KeyModifiers::NONE;
        let cases = [
            (KeyCode::Char('x'), none, Key::Char('x')),
            (KeyCode::Char('X'), KeyModifiers::SHIFT, Key::Char('X')),
            (KeyCode::Enter, none, Key::Enter),
            (KeyCode::Backspace, none, Key::Backspace),
            (KeyCode::Delete, none, Key::Delete),
            (KeyCode::Home, none, Key::Home),
            (KeyCode::End, none, Key::End),
            (KeyCode::Left, none, Key::Left),
            (KeyCode::Right, none, Key::Right),
            (KeyCode::Up, none, Key::Up),
            (KeyCode::Down, none, Key::Down),
            (KeyCode::Char('c'), KeyModifiers::CONTROL, Key::Interrupt),
            (KeyCode::Char('a'), KeyModifiers::CONTROL, Key::Home),
            (KeyCode::Char('e'), KeyModifiers::CONTROL, Key::End),
        ];
        for (code, modifiers, key) in cases {
            assert_eq!(key_from(&press(code, modifiers)), Some(key), "{code:?}");
        }
    }

    #[test]
    fn key_from_maps_word_and_search_keys() {
        let (none, ctrl, alt) = (KeyModifiers::NONE, KeyModifiers::CONTROL, KeyModifiers::ALT);
        let cases = [
            (KeyCode::Left, ctrl, Key::WordLeft),
            (KeyCode::Left, alt, Key::WordLeft),
            (KeyCode::Char('b'), alt, Key::WordLeft),
            (KeyCode::Right, ctrl, Key::WordRight),
            (KeyCode::Right, alt, Key::WordRight),
            (KeyCode::Right, ctrl | alt, Key::WordRight),
            (KeyCode::Char('f'), alt, Key::WordRight),
            (KeyCode::Char('w'), ctrl, Key::KillWordBack),
            (KeyCode::Char('k'), ctrl, Key::KillToEnd),
            (KeyCode::Char('u'), ctrl, Key::KillToStart),
            (KeyCode::Char('r'), ctrl, Key::Search),
            (KeyCode::Esc, none, Key::Cancel),
            (KeyCode::Tab, none, Key::Tab),
            (KeyCode::Char('g'), ctrl, Key::Cancel),
            (KeyCode::Left, none, Key::Left),
            (KeyCode::Right, none, Key::Right),
            (KeyCode::Char('b'), none, Key::Char('b')),
        ];
        for (code, modifiers, key) in cases {
            assert_eq!(
                key_from(&press(code, modifiers)),
                Some(key),
                "{code:?} {modifiers:?}"
            );
        }
    }

    #[test]
    fn key_from_ignores_release_and_unbound() {
        let mut release = press(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key_from(&release), None);
        assert_eq!(key_from(&press(KeyCode::F(5), KeyModifiers::NONE)), None);
        assert_eq!(
            key_from(&press(KeyCode::Char('x'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(key_from(&press(KeyCode::BackTab, KeyModifiers::NONE)), None);
        assert_eq!(
            key_from(&press(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn print_line_layout() {
        let mut editor = LineEditor::new();
        editor.set("hi".to_string());
        assert_eq!(
            Screen::layout("a\nb", &editor, '>', 80),
            format!("\r\x1b[Ka\r\nb\r\n{}", editor.render('>', 80))
        );
    }

    #[test]
    fn echo_uses_current_marker() {
        let mut screen = Screen::new(false);
        screen.tick();
        screen.tick();
        let text = Screen::echo_text(screen.marker_char(), "start a", false);
        assert_eq!(text, format!("{} start a", screen.marker_char()));
        let out = Screen::layout(&text, &screen.editor, screen.marker_char(), 80);
        assert!(out.starts_with(&format!("\r\x1b[K{} start a\r\n", 'v')));
    }

    #[test]
    fn echo_of_a_continuation_line_has_the_dots() {
        assert_eq!(Screen::echo_text('v', "gain 1", true), "v.. gain 1");
        assert_eq!(Screen::echo_text('v', "kick {", false), "v kick {");
    }

    #[test]
    fn marker_cycles() {
        let mut screen = Screen::new(false);
        let first = screen.marker_char();
        let mut seen = Vec::new();
        for _ in 0..MARKERS.len() {
            screen.tick();
            seen.push(screen.marker_char());
        }
        assert_eq!(screen.marker_char(), first);
        assert_eq!(seen, vec!['X', 'v', '>', 'X', '<', 'Z', '^']);
    }
}
