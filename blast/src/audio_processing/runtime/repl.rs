// The REPL: the input thread's body. It polls keys with a timeout, so engine
// events, garbage and stream status are drained on a timer, not only after a
// keypress.

use super::{
    audio::StreamStatus,
    history::{HISTORY_CAP, History, history_path},
    line_editor::{Key, LineEditor, Prompt},
    terminal::{Term, key_from},
};
use crate::audio_processing::{
    commands::{CmdProcessor, CmdSender, Command},
    engine::{EngineRx, Event, events::describe},
};
use cpal::ErrorKind;
use crossterm::event::{self as ct, Event as CtEvent};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// Why the REPL ended; the main thread acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quit {
    /// `quit` was applied by the engine.
    Command,
    /// Ctrl-C, SIGTERM, or stdin closed.
    Interrupt,
    /// The audio stream can't continue.
    StreamFailed,
}

// how long one pass waits for a key
const POLL: Duration = Duration::from_millis(20);

// when to report the stream's latency, once
const LATENCY_AFTER: Duration = Duration::from_secs(1);

pub struct Repl {
    processor: CmdProcessor,
    sender: CmdSender,
    rx: EngineRx,
    term: Term,
    keys: Keys,
    status: Arc<StreamStatus>,
    quit: mpsc::Sender<Quit>,
    // commands the REPL made itself (a take's track) that the full queue
    // didn't take yet
    deferred: Vec<Command>,
}

impl Repl {
    pub fn new(
        processor: CmdProcessor,
        sender: CmdSender,
        rx: EngineRx,
        term: Term,
        status: Arc<StreamStatus>,
        quit: mpsc::Sender<Quit>,
    ) -> Self {
        let path = history_path(
            std::env::var_os("XDG_STATE_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        );
        let (history, warning) = History::open(path, HISTORY_CAP);
        if let Some(warning) = warning {
            term.print_line(&warning);
        }
        Repl {
            processor,
            sender,
            rx,
            term,
            keys: Keys::new(history),
            status,
            quit,
            deferred: Vec::new(),
        }
    }

    /// The input thread's body: returns after sending a `Quit`.
    pub fn run(mut self) {
        // SIGTERM ends the REPL like Ctrl-C
        let sigterm = Arc::new(AtomicBool::new(false));
        if let Err(e) =
            signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&sigterm))
        {
            self.term
                .print_line(&format!("Warn: no SIGTERM handler: {e}"));
        }

        let started = Instant::now();
        let mut latency_reported = false;
        let mut seen_xruns = 0;
        let mut seen_error = None;

        loop {
            if sigterm.load(Ordering::Relaxed) {
                return self.end(Quit::Interrupt);
            }

            match ct::poll(POLL) {
                Ok(false) => (),
                Ok(true) => match ct::read() {
                    Ok(CtEvent::Key(event)) => {
                        if let Some(key) = key_from(&event)
                            && self.handle(key)
                        {
                            return self.end(Quit::Interrupt);
                        }
                    }
                    Ok(CtEvent::Resize(..)) => self.term.edit(|_| ()),
                    Ok(_) => (),
                    // stdin closed
                    Err(_) => return self.end(Quit::Interrupt),
                },
                Err(_) => return self.end(Quit::Interrupt),
            }

            // drain what the engine sent since the last pass
            let mut quit = false;
            for event in self.rx.events() {
                if let Event::ScheduleFired {
                    seq_no,
                    total,
                    applied,
                } = event
                {
                    self.processor
                        .engine_state
                        .note_schedule_fired(seq_no, total, applied);
                    continue;
                }
                if let Event::BufferEnded { slot, take, .. } = event {
                    self.processor.engine_state.note_buffer_ended(slot, take);
                    continue;
                }
                self.term
                    .print_line(&describe(&event, &self.processor.engine_state));
                quit |= event == Event::Quit;
            }
            if quit {
                return self.end(Quit::Command);
            }
            // takes with `to=` that ended: files are written, tracks made
            for (slot, ticket, data, frames) in self.rx.drain_saves() {
                let cmds = self.processor.note_buffer_save(slot, ticket, data, frames);
                self.deferred.extend(cmds);
            }
            while !self.deferred.is_empty() {
                match self.sender.try_push(self.deferred.remove(0)) {
                    Ok(()) => (),
                    Err(cmd) => {
                        self.deferred.insert(0, cmd);
                        break;
                    }
                }
            }
            // timed scripts: send what the fired segments make room for
            for line in self.processor.pump(&mut self.sender) {
                self.term.print_line(&line);
            }
            for line in self.processor.buffer_notices() {
                self.term.print_line(&line);
            }

            let xruns = self.status.xruns();
            if xruns != seen_xruns {
                seen_xruns = xruns;
                self.term.print_line(&format!("Warn: {xruns} xruns so far"));
            }
            let error = self.status.last_error();
            if error != seen_error {
                seen_error = error;
                if let Some(kind) = error {
                    self.term
                        .print_line(&format!("Warn: audio stream error: {kind}"));
                    if matches!(
                        kind,
                        ErrorKind::StreamInvalidated | ErrorKind::DeviceNotAvailable
                    ) {
                        return self.end(Quit::StreamFailed);
                    }
                }
            }

            if !latency_reported && started.elapsed() >= LATENCY_AFTER {
                latency_reported = true;
                self.term.print_line(&format!(
                    "Latency: {} frames/callback, ~{:.1} ms",
                    self.status.period_frames(),
                    f64::from(self.status.latency_us()) / 1000.0
                ));
            }
        }
    }

    fn end(&mut self, why: Quit) {
        // running takes with `to=` are stopped and written first; a dead
        // stream can't answer
        if why != Quit::StreamFailed {
            self.processor
                .finish_buffers(&mut self.sender, &mut self.rx, Duration::from_secs(5));
            for line in self.processor.buffer_notices() {
                self.term.print_line(&line);
            }
        }
        let _ = self.quit.send(why);
    }

    // one bound key; true when it ends the session (the second Ctrl-C)
    fn handle(&mut self, key: Key) -> bool {
        let pending = self.processor.pending();
        let processor = &self.processor;
        let keys = &mut self.keys;
        let action = self.term.edit(|e| {
            keys.on_key(e, key, pending, &|line, cursor| {
                processor.completions(line, cursor)
            })
        });
        match action {
            Action::Nothing => (),
            Action::Quit => return true,
            Action::Print(text) => self.term.print_line(&text),
            Action::CancelPending => {
                self.keys.block.clear();
                self.processor.cancel_pending();
            }
            Action::Submit(cmd) => self.submit(&cmd),
        }
        false
    }

    fn submit(&mut self, cmd: &str) {
        self.term.echo(cmd, self.processor.pending());

        // checks the queue before parsing, so a full
        // queue can't desync the mirror from the engine
        let result = self.processor.submit(cmd, &mut self.sender);
        // a `clear` wipes the window before anything the line prints
        if std::mem::take(&mut self.processor.clear_requested) {
            self.term.clear();
        }
        match result {
            Ok(Some(text)) => self.term.print_line(&text),
            Ok(None) => (),
            Err(error) => self.term.print_line(&format!("Err: {error}")),
        }
        // a block is one history entry, added when it is complete (or failed)
        let pending = self.processor.pending();
        let mut warnings = Vec::new();
        warnings.extend(self.keys.entry(cmd, pending));
        let from_file = std::mem::take(&mut self.processor.history_out);
        warnings.extend(self.keys.history.push_many(&from_file));
        for warning in warnings {
            self.term.print_line(&warning);
        }
        let prompt = prompt_for(pending);
        self.term.edit(|e| e.set_prompt(prompt));
    }
}

// what the REPL does after a key
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Nothing,
    /// Send this line.
    Submit(String),
    /// Print this above the prompt (the completion list).
    Print(String),
    /// Drop the text of an open block.
    CancelPending,
    /// The second Ctrl-C on an empty prompt.
    Quit,
}

const QUIT_HINT: &str = "(press Ctrl-C again to quit)";

fn prompt_for(pending: bool) -> Prompt {
    if pending {
        Prompt::Continuation
    } else {
        Prompt::Normal
    }
}

// Ctrl-R state
struct Search {
    query: String,
    // history index of the match shown
    hit: Option<usize>,
    // the line as it was typed before Ctrl-R
    typed: String,
    failed: bool,
}

// The key logic, apart from the terminal and the processor so it can be
// tested: history, Ctrl-R, and the two-Ctrl-C rule.
struct Keys {
    history: History,
    search: Option<Search>,
    quit_armed: bool,
    // the lines of the open block typed so far
    block: Vec<String>,
    // Tab is cycling through a list of candidates
    cycle: Option<Cycle>,
}

// Tab state after the candidates were listed
struct Cycle {
    list: Vec<String>,
    // the candidate shown (None before the first Tab)
    idx: Option<usize>,
    // the word being replaced, as char indices
    start: usize,
    len: usize,
    // an `=` follows the word, so a candidate's own `=` is left off
    eq_after: bool,
}

// `lo|=3` completing to `loop=` would make `loop==3`: when the text after
// the word starts with `=`, the candidate's `=` is dropped
fn without_dup_eq(candidate: &str, eq_after: bool) -> &str {
    match eq_after {
        true => candidate.strip_suffix('=').unwrap_or(candidate),
        false => candidate,
    }
}

// the most candidates a Tab lists
const MAX_LISTED: usize = 40;

// an entry as one editor line: lines join with `; ` (newline and `;` both
// separate statements), or with a space after `{ | ,`, after `and`/`then`/`thenif` and after a `\`
// (dropped); comments would swallow the rest of the line, so they go
fn flatten(entry: &str) -> String {
    if !entry.contains('\n') {
        return entry.to_string();
    }
    let mut out = String::new();
    for raw in entry.lines() {
        let line = without_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            match out.chars().last() {
                Some('{' | '|' | ',' | ';') => (),
                _ if ends_in_joiner(&out) => (),
                Some('\\') => {
                    out.pop();
                    out.truncate(out.trim_end().len());
                }
                _ => out.push(';'),
            }
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

// whether `text` ends in `and`, `then` or `thenif`, which go on to the next line
fn ends_in_joiner(text: &str) -> bool {
    let word = text
        .rsplit(|c: char| c.is_whitespace())
        .next()
        .unwrap_or("");
    matches!(word, "and" | "then" | "thenif")
}

// `line` up to a `#` that is not inside a string
fn without_comment(line: &str) -> &str {
    let (mut in_str, mut esc) = (false, false);
    for (i, c) in line.char_indices() {
        match (in_str, c) {
            (true, _) if esc => esc = false,
            (true, '\\') => esc = true,
            (true, '"') => in_str = false,
            (false, '"') => in_str = true,
            (false, '#') => return &line[..i],
            _ => (),
        }
    }
    line
}

impl Keys {
    fn new(history: History) -> Self {
        Keys {
            history,
            search: None,
            quit_armed: false,
            block: Vec::new(),
            cycle: None,
        }
    }

    // a line was sent; `pending` is the processor's state after it. The
    // lines of a block are kept and become one history entry when it
    // closes.
    fn entry(&mut self, line: &str, pending: bool) -> Option<String> {
        self.block.push(line.to_string());
        if pending {
            return None;
        }
        let entry = std::mem::take(&mut self.block).join("\n");
        self.history.push(&entry)
    }

    // applies `key` to the editor; `pending` is `CmdProcessor::pending()`
    // `complete` is `CmdProcessor::completions` (a stub in the tests)
    fn on_key(
        &mut self,
        ed: &mut LineEditor,
        key: Key,
        pending: bool,
        complete: &dyn Fn(&str, usize) -> Vec<String>,
    ) -> Action {
        if key != Key::Tab {
            self.cycle = None;
        }
        if self.quit_armed && key != Key::Interrupt {
            self.quit_armed = false;
            ed.set_hint(None);
        }
        if self.search.is_some() {
            match key {
                Key::Char(_) | Key::Backspace | Key::Search => {
                    self.search_key(ed, key);
                    return Action::Nothing;
                }
                Key::Cancel | Key::Interrupt => {
                    if let Some(s) = self.search.take() {
                        ed.set(s.typed);
                    }
                    ed.set_prompt(prompt_for(pending));
                    return Action::Nothing;
                }
                // anything else keeps the hit as the line and acts on it
                _ => {
                    self.search = None;
                    ed.set_prompt(prompt_for(pending));
                }
            }
        }
        match key {
            Key::Enter => Action::Submit(ed.take()),
            Key::Tab => self.tab(ed, complete),
            Key::Interrupt => self.interrupt(ed, pending),
            Key::Backspace => act(ed, LineEditor::backspace),
            Key::Delete => act(ed, LineEditor::delete),
            Key::Home => act(ed, LineEditor::home),
            Key::End => act(ed, LineEditor::end),
            Key::Left => act(ed, LineEditor::left),
            Key::Right => act(ed, LineEditor::right),
            Key::WordLeft => act(ed, LineEditor::word_left),
            Key::WordRight => act(ed, LineEditor::word_right),
            Key::KillWordBack => act(ed, LineEditor::kill_word_back),
            Key::KillToEnd => act(ed, LineEditor::kill_to_end),
            Key::KillToStart => act(ed, LineEditor::kill_to_start),
            Key::Up => {
                if let Some(line) = self.history.prev(ed.buf()) {
                    ed.set(flatten(line));
                }
                Action::Nothing
            }
            Key::Down => {
                if let Some(line) = self.history.next() {
                    ed.set(flatten(line));
                }
                Action::Nothing
            }
            Key::Char(c) => {
                ed.insert(c);
                Action::Nothing
            }
            Key::Search => {
                self.search = Some(Search {
                    query: String::new(),
                    hit: None,
                    typed: ed.buf().to_string(),
                    failed: false,
                });
                self.show_search(ed);
                Action::Nothing
            }
            Key::Cancel | Key::Ignore => Action::Nothing,
        }
    }

    // Tab: one candidate is inserted; several give their common prefix,
    // else a list, after which each Tab shows the next candidate
    fn tab(
        &mut self,
        ed: &mut LineEditor,
        complete: &dyn Fn(&str, usize) -> Vec<String>,
    ) -> Action {
        if let Some(c) = self.cycle.as_mut() {
            let i = c.idx.map_or(0, |i| (i + 1) % c.list.len());
            c.idx = Some(i);
            let text = without_dup_eq(&c.list[i], c.eq_after);
            ed.replace_word(c.start, c.start + c.len, text);
            c.len = text.chars().count();
            return Action::Nothing;
        }
        let cursor = ed.cursor();
        let byte = ed
            .buf()
            .char_indices()
            .nth(cursor)
            .map_or(ed.buf().len(), |(i, _)| i);
        let list = complete(ed.buf(), byte);
        if list.is_empty() {
            return Action::Nothing;
        }
        // the candidates start with the word typed so far: the longest
        // stretch before the cursor that they all start with
        let before: Vec<char> = ed.buf().chars().take(cursor).collect();
        let typed = (0..=before.len())
            .rev()
            .find(|&n| {
                let word: String = before[before.len() - n..].iter().collect();
                list.iter().all(|c| c.starts_with(&word))
            })
            .unwrap_or(0);
        let start = cursor - typed;
        let end = ed.word_bounds().1.max(cursor);
        let eq_after = ed.buf().chars().nth(end) == Some('=');
        if let [only] = list.as_slice() {
            ed.replace_word(start, end, without_dup_eq(only, eq_after));
            return Action::Nothing;
        }
        let prefix = common_prefix(&list);
        if prefix.chars().count() > end - start {
            ed.replace_word(start, end, without_dup_eq(&prefix, eq_after));
            return Action::Nothing;
        }
        let mut text = list[..list.len().min(MAX_LISTED)].join("  ");
        if list.len() > MAX_LISTED {
            text.push_str(&format!("  … {} more", list.len() - MAX_LISTED));
        }
        self.cycle = Some(Cycle {
            list,
            idx: None,
            start,
            len: end - start,
            eq_after,
        });
        Action::Print(text)
    }

    // Ctrl-C outside a search: clear the line (and any open block); on an
    // empty prompt the first one arms the reminder and the next one quits
    fn interrupt(&mut self, ed: &mut LineEditor, pending: bool) -> Action {
        if !ed.is_empty() || pending {
            ed.clear();
            ed.set_prompt(Prompt::Normal);
            return if pending {
                Action::CancelPending
            } else {
                Action::Nothing
            };
        }
        if self.quit_armed {
            return Action::Quit;
        }
        self.quit_armed = true;
        ed.set_hint(Some(QUIT_HINT));
        Action::Nothing
    }

    // a key inside Ctrl-R that edits the query or steps to an older match
    fn search_key(&mut self, ed: &mut LineEditor, key: Key) {
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let newest = self.history.len();
        let before = match key {
            Key::Char(c) => {
                s.query.push(c);
                newest
            }
            Key::Backspace => {
                s.query.pop();
                newest
            }
            _ => s.hit.unwrap_or(newest),
        };
        match self.history.search(&s.query, before) {
            Some(i) => {
                s.hit = Some(i);
                s.failed = false;
            }
            None if s.query.is_empty() => {
                // nothing to look for: back to the line as typed
                s.hit = None;
                s.failed = false;
            }
            None => s.failed = true,
        }
        match (s.hit, s.failed) {
            (Some(i), false) => {
                if let Some(line) = self.history.get(i) {
                    ed.set(flatten(line));
                }
            }
            (None, false) => ed.set(s.typed.clone()),
            _ => (),
        }
        self.show_search(ed);
    }

    fn show_search(&self, ed: &mut LineEditor) {
        if let Some(s) = &self.search {
            ed.set_prompt(Prompt::Search {
                query: s.query.clone(),
                failed: s.failed,
            });
        }
    }
}

fn common_prefix(words: &[String]) -> String {
    let mut prefix: Vec<char> = words[0].chars().collect();
    for w in &words[1..] {
        let same = prefix
            .iter()
            .zip(w.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        prefix.truncate(same);
    }
    prefix.into_iter().collect()
}

fn act(ed: &mut LineEditor, f: fn(&mut LineEditor)) -> Action {
    f(ed);
    Action::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::commands::{CmdProcessor, EngineState, inspect::NameKind};

    fn keys(lines: &[&str]) -> Keys {
        let mut history = History::in_memory(50);
        for l in lines {
            history.push(l);
        }
        Keys::new(history)
    }

    fn press(k: &mut Keys, ed: &mut LineEditor, key: Key) -> Action {
        k.on_key(ed, key, false, &|_, _| Vec::new())
    }

    fn type_str(k: &mut Keys, ed: &mut LineEditor, s: &str) {
        for c in s.chars() {
            press(k, ed, Key::Char(c));
        }
    }

    #[test]
    fn ctrl_c_clears_line_then_needs_two_on_empty() {
        let mut k = keys(&[]);
        let mut ed = LineEditor::new();
        type_str(&mut k, &mut ed, "abc");
        assert_eq!(press(&mut k, &mut ed, Key::Interrupt), Action::Nothing);
        assert_eq!(ed.buf(), "");
        assert!(!k.quit_armed);

        // empty: armed with the hint, not quit
        assert_eq!(press(&mut k, &mut ed, Key::Interrupt), Action::Nothing);
        assert!(k.quit_armed);
        assert!(ed.render('>', 80).contains(QUIT_HINT));
        // the second one quits
        assert_eq!(press(&mut k, &mut ed, Key::Interrupt), Action::Quit);
    }

    #[test]
    fn other_key_disarms_ctrl_c() {
        let mut k = keys(&[]);
        let mut ed = LineEditor::new();
        press(&mut k, &mut ed, Key::Interrupt);
        assert!(k.quit_armed);
        press(&mut k, &mut ed, Key::Char('a'));
        assert!(!k.quit_armed);
        assert!(!ed.render('>', 80).contains(QUIT_HINT));
        // the line is `a`: Ctrl-C clears it instead of quitting
        assert_eq!(press(&mut k, &mut ed, Key::Interrupt), Action::Nothing);
        assert_eq!(ed.buf(), "");
        assert!(!k.quit_armed);
    }

    #[test]
    fn ctrl_c_in_an_open_block_cancels_it() {
        let mut k = keys(&[]);
        let mut ed = LineEditor::new();
        assert_eq!(
            k.on_key(&mut ed, Key::Interrupt, true, &|_, _| Vec::new()),
            Action::CancelPending
        );
        assert!(!k.quit_armed);
    }

    #[test]
    fn continuation_prompt_while_pending() {
        let mut cp = CmdProcessor::new(EngineState::new(Vec::new(), 2));
        assert_eq!(prompt_for(cp.pending()), Prompt::Normal);
        assert!(cp.parse("kick {").is_ok());
        assert!(cp.pending());
        assert_eq!(prompt_for(cp.pending()), Prompt::Continuation);
        cp.cancel_pending();
        assert_eq!(prompt_for(cp.pending()), Prompt::Normal);
    }

    #[test]
    fn up_down_browse_and_keep_typing() {
        let mut k = keys(&["one", "two"]);
        let mut ed = LineEditor::new();
        type_str(&mut k, &mut ed, "wip");
        press(&mut k, &mut ed, Key::Up);
        assert_eq!(ed.buf(), "two");
        press(&mut k, &mut ed, Key::Up);
        assert_eq!(ed.buf(), "one");
        press(&mut k, &mut ed, Key::Down);
        press(&mut k, &mut ed, Key::Down);
        assert_eq!(ed.buf(), "wip");
    }

    #[test]
    fn ctrl_r_finds_steps_and_cancels() {
        let mut k = keys(&["kick a", "snare", "kick b"]);
        let mut ed = LineEditor::new();
        type_str(&mut k, &mut ed, "wip");
        press(&mut k, &mut ed, Key::Search);
        type_str(&mut k, &mut ed, "kick");
        assert_eq!(ed.buf(), "kick b");
        press(&mut k, &mut ed, Key::Search);
        assert_eq!(ed.buf(), "kick a");
        // no older match: the prompt says so, the line stays
        press(&mut k, &mut ed, Key::Search);
        assert_eq!(ed.buf(), "kick a");
        assert!(ed.render('>', 80).starts_with("\r(failed search 'kick')"));
        // backspace searches again from the newest
        press(&mut k, &mut ed, Key::Backspace);
        assert_eq!(ed.buf(), "kick b");
        press(&mut k, &mut ed, Key::Cancel);
        assert_eq!(ed.buf(), "wip");
        assert!(k.search.is_none());
        assert!(ed.render('>', 80).starts_with("\r> "));
    }

    #[test]
    fn ctrl_r_enter_runs_hit_and_editing_key_keeps_it() {
        let mut k = keys(&["alpha", "beta"]);
        let mut ed = LineEditor::new();
        press(&mut k, &mut ed, Key::Search);
        type_str(&mut k, &mut ed, "alp");
        assert_eq!(
            press(&mut k, &mut ed, Key::Enter),
            Action::Submit("alpha".into())
        );
        assert!(k.search.is_none());

        press(&mut k, &mut ed, Key::Search);
        type_str(&mut k, &mut ed, "bet");
        press(&mut k, &mut ed, Key::Home);
        assert_eq!(ed.buf(), "beta");
        assert_eq!(ed.cursor(), 0);
        assert!(k.search.is_none());
    }

    // ---- Tab ----

    fn stub(list: &'static [&'static str]) -> impl Fn(&str, usize) -> Vec<String> {
        move |_, _| list.iter().map(|s| s.to_string()).collect()
    }

    fn tab(k: &mut Keys, ed: &mut LineEditor, list: &'static [&'static str]) -> Action {
        k.on_key(ed, Key::Tab, false, &stub(list))
    }

    #[test]
    fn tab_single_candidate_inserts() {
        let mut k = keys(&[]);
        let mut ed = LineEditor::new();
        type_str(&mut k, &mut ed, "voice ki");
        assert_eq!(tab(&mut k, &mut ed, &["kick"]), Action::Nothing);
        assert_eq!(ed.buf(), "voice kick");
        assert_eq!(ed.cursor(), 10);
        // nothing to offer: nothing changes
        assert_eq!(tab(&mut k, &mut ed, &[]), Action::Nothing);
        assert_eq!(ed.buf(), "voice kick");
        // in the middle of a word the rest of the word is replaced
        let mut ed = LineEditor::new();
        type_str(&mut k, &mut ed, "voice kiXX hat");
        for _ in 0..7 {
            press(&mut k, &mut ed, Key::Left);
        }
        // cursor is after "ki", before "XX"
        tab(&mut k, &mut ed, &["kick"]);
        assert_eq!(ed.buf(), "voice kick hat");
    }

    #[test]
    fn tab_mid_word_before_equals_keeps_one_equals() {
        let mut k = keys(&[]);
        let mut ed = LineEditor::new();
        type_str(&mut k, &mut ed, "kick lo=3");
        for _ in 0..2 {
            press(&mut k, &mut ed, Key::Left);
        }
        // cursor is after "lo", before "=3"
        tab(&mut k, &mut ed, &["loop="]);
        assert_eq!(ed.buf(), "kick loop=3");
        // a candidate without one is untouched
        let mut ed = LineEditor::new();
        type_str(&mut k, &mut ed, "kick lo=3");
        for _ in 0..2 {
            press(&mut k, &mut ed, Key::Left);
        }
        tab(&mut k, &mut ed, &["loop"]);
        assert_eq!(ed.buf(), "kick loop=3");
        // and with no `=` after the word, the candidate keeps its own
        let mut ed = LineEditor::new();
        type_str(&mut k, &mut ed, "kick lo");
        tab(&mut k, &mut ed, &["loop="]);
        assert_eq!(ed.buf(), "kick loop=");
    }

    #[test]
    fn tab_common_prefix_then_list() {
        let mut k = keys(&[]);
        let mut ed = LineEditor::new();
        type_str(&mut k, &mut ed, "kick lo");
        let all = &["loop=", "loopend=", "loopstart="];
        // the common prefix first
        assert_eq!(tab(&mut k, &mut ed, all), Action::Nothing);
        assert_eq!(ed.buf(), "kick loop");
        // then, as it adds nothing, the list
        assert_eq!(
            tab(&mut k, &mut ed, all),
            Action::Print("loop=  loopend=  loopstart=".into())
        );
        assert_eq!(ed.buf(), "kick loop");
    }

    #[test]
    fn tab_list_is_capped() {
        let mut k = keys(&[]);
        let mut ed = LineEditor::new();
        let names: Vec<String> = (0..45).map(|i| format!("n{i:02}")).collect();
        // the first Tab inserts the common prefix `n`
        k.on_key(&mut ed, Key::Tab, false, &|_, _| names.clone());
        assert_eq!(ed.buf(), "n");
        let Action::Print(text) = k.on_key(&mut ed, Key::Tab, false, &|_, _| names.clone()) else {
            panic!("expected a list");
        };
        assert!(text.starts_with("n00  n01"));
        assert!(text.contains("n39"));
        assert!(!text.contains("n40"));
        assert!(text.ends_with("… 5 more"));
    }

    #[test]
    fn tab_cycles_and_other_key_stops() {
        let mut k = keys(&[]);
        let mut ed = LineEditor::new();
        type_str(&mut k, &mut ed, "x ba");
        assert!(matches!(
            tab(&mut k, &mut ed, &["bass", "bat"]),
            Action::Print(_)
        ));
        // the list is kept: the stub is not asked again
        let none = |_: &str, _: usize| -> Vec<String> { panic!("asked again") };
        for want in ["x bass", "x bat", "x bass"] {
            k.on_key(&mut ed, Key::Tab, false, &none);
            assert_eq!(ed.buf(), want);
        }
        // another key ends cycling: Tab asks again
        press(&mut k, &mut ed, Key::Char('!'));
        assert_eq!(ed.buf(), "x bass!");
        assert!(k.cycle.is_none());
        tab(&mut k, &mut ed, &["bass!x"]);
        assert_eq!(ed.buf(), "x bass!x");
    }

    // ---- blocks ----

    #[test]
    fn flatten_joins_lines() {
        assert_eq!(flatten("kick gain 0.5"), "kick gain 0.5");
        assert_eq!(
            flatten("kick {\n  gain 0.5\n  pan 0\n}"),
            "kick { gain 0.5; pan 0; }"
        );
        assert_eq!(flatten("a |\n b"), "a | b");
        assert_eq!(flatten("a and\n b,\n c"), "a and b, c");
        assert_eq!(flatten("a then\n b thenif\n c"), "a then b thenif c");
        // a name that ends in the word is not the word
        assert_eq!(flatten("brand\n b"), "brand; b");
        assert_eq!(flatten("a \\\n  b"), "a b");
        // comments are dropped, `#` in a string is not a comment
        assert_eq!(
            flatten("a { # c\n # whole\n b \"x#y\" # d\n}"),
            "a { b \"x#y\"; }"
        );
    }

    #[test]
    fn typed_block_is_one_history_entry() {
        let mut k = keys(&[]);
        assert!(k.entry("kick {", true).is_none());
        assert!(k.entry("  gain 0.5", true).is_none());
        assert_eq!(k.history.len(), 0);
        assert!(k.entry("}", false).is_none());
        assert_eq!(k.history.len(), 1);
        assert_eq!(k.history.get(0), Some("kick {\n  gain 0.5\n}"));
        // a plain line is its own entry
        k.entry("hat", false);
        assert_eq!(k.history.len(), 2);
        // Up recalls the block on one line
        let mut ed = LineEditor::new();
        press(&mut k, &mut ed, Key::Up);
        press(&mut k, &mut ed, Key::Up);
        assert_eq!(ed.buf(), "kick { gain 0.5; }");
    }

    fn two_voices() -> CmdProcessor {
        use crate::audio_processing::test_util::track;
        let files = vec![track("kick", 1, 16), track("hat", 1, 16)];
        let mut cp = CmdProcessor::new(EngineState::new(files, 2));
        cp.boot();
        for l in ["voice kick from \"kick\"", "voice hat from \"hat\""] {
            assert!(cp.parse(l).is_ok(), "{l}");
        }
        cp
    }

    #[test]
    fn recalled_block_parses_the_same() {
        let entries = [
            "group drums {\n  kick # first\n  # a note\n  hat\n}",
            "kick gain=0.5 and\n  hat gain=0.25",
            "kick \\\n  gain=0.5",
        ];
        for entry in entries {
            let mut typed = two_voices();
            for line in entry.lines() {
                assert!(typed.parse(line).is_ok(), "{entry}");
            }
            assert!(!typed.pending(), "{entry}");
            let mut recalled = two_voices();
            let flat = flatten(entry);
            assert!(!flat.contains('\n') && !flat.contains('#'), "{flat}");
            assert!(recalled.parse(&flat).is_ok(), "{flat}");
            assert!(!recalled.pending(), "{flat}");
            assert_eq!(
                recalled.names(NameKind::Any),
                typed.names(NameKind::Any),
                "{entry}"
            );
            assert_eq!(
                recalled.state(None).unwrap(),
                typed.state(None).unwrap(),
                "{entry}"
            );
        }
    }
}
