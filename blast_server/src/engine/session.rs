//! The session thread: what `runtime::repl::Repl` is for the terminal, with
//! browsers in place of the keyboard. It owns the `CmdProcessor` (the
//! mirror, the language, `source`, `state`), submits lines the way the REPL
//! does, keeps the history, and drains the engine's events on a timer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use blast::audio_processing::commands::{CmdProcessor, CmdSender, Command};
use blast::audio_processing::engine::{EngineRx, Event as EngineEvent, events::describe};
use blast::audio_processing::runtime::history::{HISTORY_CAP, History};
use tokio::sync::oneshot;

use super::hub::{Event, Hub};

const POLL: Duration = Duration::from_millis(20);

/// What the server asks of the session.
pub enum Req {
    /// A line typed at the prompt by `user`, whose history is `hist`.
    Submit {
        line: String,
        user: String,
        hist: HistKey,
    },
    /// Ctrl-C: drop an open block.
    Cancel,
    /// Tab: candidates for the word touching `cursor` (a byte offset).
    Complete {
        line: String,
        cursor: usize,
        reply: oneshot::Sender<Vec<String>>,
    },
    /// Every entry of a history, oldest first.
    History {
        hist: HistKey,
        reply: oneshot::Sender<Vec<String>>,
    },
    /// The session as a script (`state`).
    State { reply: oneshot::Sender<String> },
    /// Checks a script against a copy of the session: (errors, warnings).
    Check {
        name: String,
        text: String,
        reply: oneshot::Sender<(Vec<String>, Vec<String>)>,
    },
    /// The session's track names.
    Tracks { reply: oneshot::Sender<Vec<String>> },
    /// Ends the session: running takes are written, then the thread exits.
    End { reply: oneshot::Sender<()> },
}

/// Which history file a user's lines go to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HistKey {
    pub key: String,
    pub path: Option<PathBuf>,
}

/// Why the session thread returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// `quit` was typed: the supervisor starts a fresh session.
    Quit,
    /// `Req::End`, or the server is gone.
    Asked,
}

pub struct Session {
    pub processor: CmdProcessor,
    pub sender: CmdSender,
    pub rx: EngineRx,
    pub hub: Arc<Hub>,
    histories: HashMap<String, History>,
    // lines of an open block, one history entry when it closes
    block: Vec<String>,
    deferred: Vec<Command>,
}

impl Session {
    pub fn new(processor: CmdProcessor, sender: CmdSender, rx: EngineRx, hub: Arc<Hub>) -> Session {
        Session {
            processor,
            sender,
            rx,
            hub,
            histories: HashMap::new(),
            block: Vec::new(),
            deferred: Vec::new(),
        }
    }

    fn history(&mut self, hist: &HistKey) -> &mut History {
        let hub = &self.hub;
        self.histories.entry(hist.key.clone()).or_insert_with(|| {
            let (h, warning) = History::open(hist.path.clone(), HISTORY_CAP);
            if let Some(w) = warning {
                hub.print("warn", &w, None);
            }
            h
        })
    }

    /// The thread's body.
    pub fn run(mut self, reqs: mpsc::Receiver<Req>) -> Ended {
        loop {
            match reqs.recv_timeout(POLL) {
                Ok(req) => {
                    if let Some(ended) = self.handle(req) {
                        return ended;
                    }
                    // answer a burst (a pasted block) before draining
                    while let Ok(req) = reqs.try_recv() {
                        if let Some(ended) = self.handle(req) {
                            return ended;
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.finish();
                    return Ended::Asked;
                }
            }
            if self.drain() {
                self.finish();
                return Ended::Quit;
            }
        }
    }

    fn handle(&mut self, req: Req) -> Option<Ended> {
        match req {
            Req::Submit { line, user, hist } => self.submit(&line, &user, &hist),
            Req::Cancel => {
                self.block.clear();
                self.processor.cancel_pending();
                self.hub.prompt(false);
            }
            Req::Complete {
                line,
                cursor,
                reply,
            } => {
                let cursor = floor_char_boundary(&line, cursor);
                let _ = reply.send(self.processor.completions(&line, cursor));
            }
            Req::History { hist, reply } => {
                let h = self.history(&hist);
                let entries = (0..h.len())
                    .filter_map(|i| h.get(i).map(str::to_string))
                    .collect();
                let _ = reply.send(entries);
            }
            Req::State { reply } => {
                let _ = reply.send(self.processor.state(None).unwrap_or_default());
            }
            Req::Check { name, text, reply } => {
                let out = match self.processor.check_text(&name, &text) {
                    Ok(warns) => (Vec::new(), warns.iter().map(|w| w.to_string()).collect()),
                    Err(e) => (vec![e.to_string()], Vec::new()),
                };
                let _ = reply.send(out);
            }
            Req::Tracks { reply } => {
                let _ = reply.send(super::track_names(&self.processor));
            }
            Req::End { reply } => {
                self.finish();
                let _ = reply.send(());
                return Some(Ended::Asked);
            }
        }
        None
    }

    fn submit(&mut self, line: &str, user: &str, hist: &HistKey) {
        let continuation = self.processor.pending();
        self.hub.echo(line, continuation, user);

        let result = self.processor.submit(line, &mut self.sender);
        if std::mem::take(&mut self.processor.clear_requested) {
            self.hub.clear();
        }
        match result {
            Ok(Some(text)) => self.hub.print("out", &text, None),
            Ok(None) => (),
            Err(e) => self.hub.print("err", &format!("Err: {e}"), None),
        }
        let pending = self.processor.pending();

        // a block is one history entry, added when it is complete (or failed)
        let mut added = Vec::new();
        self.block.push(line.to_string());
        if !pending {
            let entry = std::mem::take(&mut self.block).join("\n");
            if !entry.trim().is_empty() {
                added.push(entry);
            }
        }
        added.extend(std::mem::take(&mut self.processor.history_out));
        if !added.is_empty() {
            let h = self.history(hist);
            let before = h.len();
            let mut warnings = Vec::new();
            for e in &added {
                warnings.extend(h.push(e));
            }
            // what actually went in (repeats and blanks are skipped)
            let pushed: Vec<String> = (before..h.len())
                .filter_map(|i| h.get(i).map(str::to_string))
                .collect();
            // at the cap old entries fall off, so the tail alone would mislead
            let full = h.len() >= HISTORY_CAP;
            for w in warnings {
                self.hub.print("warn", &w, None);
            }
            if !pushed.is_empty() || full {
                // at the cap the length doesn't grow: send the whole list
                if full {
                    let h = self.history(hist);
                    let all = (0..h.len())
                        .filter_map(|i| h.get(i).map(str::to_string))
                        .collect();
                    self.hub.emit(&Event::History {
                        key: hist.key.clone(),
                        entries: all,
                        replace: true,
                    });
                } else {
                    self.hub.emit(&Event::History {
                        key: hist.key.clone(),
                        entries: pushed,
                        replace: false,
                    });
                }
            }
        }
        self.hub.prompt(pending);
    }

    /// Drains what the engine sent; true when the session quit.
    fn drain(&mut self) -> bool {
        let mut quit = false;
        for event in self.rx.events() {
            match event {
                EngineEvent::ScheduleFired {
                    seq_no,
                    total,
                    applied,
                } => self
                    .processor
                    .engine_state
                    .note_schedule_fired(seq_no, total, applied),
                EngineEvent::BufferEnded { slot, take, .. } => {
                    self.processor.engine_state.note_buffer_ended(slot, take)
                }
                EngineEvent::Quit => quit = true,
                other => {
                    let text = describe(&other, &self.processor.engine_state);
                    let kind = if text.starts_with("Err") {
                        "err"
                    } else {
                        "warn"
                    };
                    self.hub.print(kind, &text, None);
                }
            }
        }
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
            self.hub.print("info", &line, None);
        }
        for line in self.processor.buffer_notices() {
            self.hub.print("info", &line, None);
        }
        quit
    }

    // running takes with `to=` are stopped and written
    fn finish(&mut self) {
        self.processor
            .finish_buffers(&mut self.sender, &mut self.rx, Duration::from_secs(5));
        for line in self.processor.buffer_notices() {
            self.hub.print("info", &line, None);
        }
    }
}

fn floor_char_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}
