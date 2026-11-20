// `--agent`: the REPL's place when no person is typing. One JSON object per
// line on stdin (`script` or `check`), one JSON line back on stdout; notices
// (engine warnings, buffer saves, quit) are lines with an `event` and no
// `id`. Nothing else is written to stdout. `handle_line` and the line
// builders are plain functions over a `CmdProcessor`, so a socket front end
// can reuse them.

pub mod json;

use super::{audio::StreamStatus, repl::Quit};
use crate::audio_processing::{
    commands::{CmdErr, CmdProcessor, CmdSender, Command, EngineState, lang::LangErr},
    engine::{EngineRx, Event, events::describe},
};
use cpal::ErrorKind;
use json::{Json, parse, write_json, write_str};
use std::{
    io::{BufRead, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// The name requests are checked under; errors in it carry no `file`.
const REQUEST: &str = "request";

/// The line that ends every session.
pub const QUIT_LINE: &str = "{\"event\":\"quit\"}";

// how long one pass waits for a request
const POLL: Duration = Duration::from_millis(20);

// when to report the stream's latency, once (on stderr)
const LATENCY_AFTER: Duration = Duration::from_secs(1);

// `"id":…` for the answer: a number or a string as sent, else null
fn id_of(request: &Json) -> Json {
    match request.get("id") {
        Some(id @ (Json::Num(_) | Json::Str(_))) => id.clone(),
        _ => Json::Null,
    }
}

fn write_id(out: &mut String, id: &Json) {
    out.push_str("{\"id\":");
    write_json(out, id);
}

fn write_lang_err(out: &mut String, e: &LangErr) {
    out.push_str("{\"code\":");
    write_str(out, e.code.as_str());
    out.push_str(&format!(",\"line\":{},\"col\":{}", e.span.line, e.span.col));
    out.push_str(",\"message\":");
    write_str(out, &e.message);
    if let Some(s) = &e.suggestion {
        out.push_str(",\"suggestion\":");
        write_str(out, s);
    }
    // errors in the request's own text need no file
    if let Some(f) = e.file.as_deref().filter(|f| *f != REQUEST) {
        out.push_str(",\"file\":");
        write_str(out, f);
    }
    out.push('}');
}

fn write_list(out: &mut String, key: &str, errs: &[LangErr]) {
    out.push_str(&format!(",\"{key}\":["));
    for (i, e) in errs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_lang_err(out, e);
    }
    out.push(']');
}

// the rest of an answer (after the id): ok/output/warnings or ok/errors
fn write_result(out: &mut String, result: Result<(String, Vec<LangErr>), CmdErr>) {
    match result {
        Ok((output, warnings)) => {
            out.push_str(",\"ok\":true,\"output\":");
            write_str(out, &output);
            write_list(out, "warnings", &warnings);
        }
        Err(CmdErr::Lang(errs)) => {
            out.push_str(",\"ok\":false");
            write_list(out, "errors", &errs);
        }
        Err(other) => {
            // not a language error (a full queue, say): no position
            let code = if matches!(other, CmdErr::QueueFull) {
                "E602"
            } else {
                "E000"
            };
            out.push_str(&format!(
                ",\"ok\":false,\"errors\":[{{\"code\":\"{code}\",\"line\":0,\"col\":0,\"message\":"
            ));
            write_str(out, &other.to_string());
            out.push_str("}]");
        }
    }
    out.push('}');
}

/// `{"event":"script", …}`: the outcome of `--script`, run before the first
/// request.
pub fn script_event(result: Result<String, CmdErr>) -> String {
    let mut out = String::from("{\"event\":\"script\"");
    write_result(&mut out, result.map(|text| (text, Vec::new())));
    out
}

/// `{"event":KIND,"message":TEXT}`.
pub fn notice_line(kind: &str, message: &str) -> String {
    let mut out = format!("{{\"event\":\"{kind}\",\"message\":");
    write_str(&mut out, message);
    out.push('}');
    out
}

/// The line for an engine event, if it has one. Schedule and buffer events
/// feed the mirror and `Quit` ends the session, so they have none.
pub fn event_line(event: &Event, names: &EngineState) -> Option<String> {
    match event {
        Event::ScheduleFired { .. } | Event::BufferEnded { .. } | Event::Quit => None,
        _ => Some(notice_line("warn", &describe(event, names))),
    }
}

/// Answers one request line (without its newline) as one answer line
/// (without a newline). `script` is checked whole and applied; `check`
/// only checks, against a copy of the session.
pub fn handle_line(proc: &mut CmdProcessor, queue: &mut CmdSender, line: &str) -> String {
    let mut out = String::new();
    let not_request = |out: &mut String, id: &Json, line: u32, col: usize, why: &str| {
        write_id(out, id);
        out.push_str(&format!(
            ",\"ok\":false,\"errors\":[{{\"code\":\"J001\",\"line\":{line},\"col\":{col},\"message\":"
        ));
        write_str(out, &format!("not a request: {why}"));
        out.push_str("}]}");
    };
    let request = match parse(line) {
        Ok(r @ Json::Obj(_)) => r,
        Ok(_) => {
            not_request(&mut out, &Json::Null, 1, 1, "expected a JSON object");
            return out;
        }
        Err(e) => {
            not_request(&mut out, &Json::Null, 1, e.col, &e.message);
            return out;
        }
    };
    let id = id_of(&request);
    let text = |key: &str| match request.get(key) {
        Some(Json::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    write_id(&mut out, &id);
    let result = if let Some(script) = text("script") {
        let result = proc.run_text(REQUEST, script, queue);
        // nothing in agent mode listens to these
        proc.history_out.clear();
        proc.clear_requested = false;
        result
    } else if let Some(script) = text("check") {
        proc.check_text(REQUEST, script)
            .map(|warns| (String::new(), warns))
    } else {
        out.clear();
        not_request(&mut out, &id, 1, 1, "needs a string 'script' or 'check'");
        return out;
    };
    write_result(&mut out, result);
    out
}

fn emit(line: &str) -> bool {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}").is_ok() && out.flush().is_ok()
}

pub struct Agent {
    processor: CmdProcessor,
    sender: CmdSender,
    rx: EngineRx,
    status: Arc<StreamStatus>,
    quit: mpsc::Sender<Quit>,
    // stdin's lines; disconnects at its end
    lines: mpsc::Receiver<String>,
    // commands the loop made itself (a take's track) that the full queue
    // didn't take yet
    deferred: Vec<Command>,
}

impl Agent {
    /// Starts the thread that reads stdin.
    pub fn new(
        processor: CmdProcessor,
        sender: CmdSender,
        rx: EngineRx,
        status: Arc<StreamStatus>,
        quit: mpsc::Sender<Quit>,
    ) -> Self {
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match stdin.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                        if tx.send(line).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Agent {
            processor,
            sender,
            rx,
            status,
            quit,
            lines,
            deferred: Vec::new(),
        }
    }

    /// The loop that takes the REPL thread's place: returns after sending a
    /// `Quit`.
    pub fn run(mut self) {
        let sigterm = Arc::new(AtomicBool::new(false));
        if let Err(e) =
            signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&sigterm))
        {
            emit(&notice_line(
                "warn",
                &format!("Warn: no SIGTERM handler: {e}"),
            ));
        }

        let started = Instant::now();
        let mut latency_reported = false;
        let mut seen_xruns = 0;
        let mut seen_error = None;

        loop {
            if sigterm.load(Ordering::Relaxed) {
                return self.end(Quit::Interrupt);
            }

            match self.lines.recv_timeout(POLL) {
                Ok(line) if line.trim().is_empty() => (),
                Ok(line) => {
                    let answer = handle_line(&mut self.processor, &mut self.sender, &line);
                    if !emit(&answer) {
                        // stdout is gone
                        return self.end(Quit::Interrupt);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                // end of stdin
                Err(mpsc::RecvTimeoutError::Disconnected) => return self.end(Quit::Command),
            }

            // drain what the engine sent since the last pass
            let mut quit = false;
            for event in self.rx.events() {
                match event {
                    Event::ScheduleFired {
                        seq_no,
                        total,
                        applied,
                    } => self
                        .processor
                        .engine_state
                        .note_schedule_fired(seq_no, total, applied),
                    Event::BufferEnded { slot, take, .. } => {
                        self.processor.engine_state.note_buffer_ended(slot, take)
                    }
                    Event::Quit => quit = true,
                    _ => {
                        if let Some(line) = event_line(&event, &self.processor.engine_state) {
                            emit(&line);
                        }
                    }
                }
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
                emit(&notice_line("info", &line));
            }
            self.buffer_events();

            let xruns = self.status.xruns();
            if xruns != seen_xruns {
                seen_xruns = xruns;
                emit(&notice_line("warn", &format!("Warn: {xruns} xruns so far")));
            }
            let error = self.status.last_error();
            if error != seen_error {
                seen_error = error;
                if let Some(kind) = error {
                    emit(&notice_line(
                        "warn",
                        &format!("Warn: audio stream error: {kind}"),
                    ));
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
                eprintln!(
                    "Latency: {} frames/callback, ~{:.1} ms",
                    self.status.period_frames(),
                    f64::from(self.status.latency_us()) / 1000.0
                );
            }
        }
    }

    // the buffer notices as `buf` events
    fn buffer_events(&mut self) {
        for line in self.processor.buffer_notices() {
            emit(&notice_line("buf", &line));
        }
    }

    fn end(&mut self, why: Quit) {
        // running takes with `to=` are stopped and written first; a dead
        // stream can't answer
        if why != Quit::StreamFailed {
            self.processor
                .finish_buffers(&mut self.sender, &mut self.rx, Duration::from_secs(5));
            self.buffer_events();
        }
        emit(QUIT_LINE);
        let _ = self.quit.send(why);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::commands::{TempoOwner, cmd_queue};
    use crate::audio_processing::test_util::track;

    fn processor() -> CmdProcessor {
        let files = ["kick", "hat"].iter().map(|n| track(n, 1, 16)).collect();
        let mut cp = CmdProcessor::new(EngineState::new(files, 2));
        cp.boot();
        cp
    }

    fn req(cp: &mut CmdProcessor, tx: &mut CmdSender, line: &str) -> Json {
        let answer = handle_line(cp, tx, line);
        assert!(!answer.contains('\n'), "{answer}");
        parse(&answer).unwrap_or_else(|e| panic!("{answer}: {e}"))
    }

    fn first_err(answer: &Json) -> &Json {
        match answer.get("errors") {
            Some(Json::Arr(errs)) => &errs[0],
            other => panic!("no errors: {other:?}"),
        }
    }

    #[test]
    fn script_ok_answer_has_output_and_warnings() {
        let mut cp = processor();
        let (mut tx, mut rx) = cmd_queue(16);
        let a = req(
            &mut cp,
            &mut tx,
            r#"{"id": 7, "script": "voice kick from \"kick\"\nclock a 120b\nclock a 120b\nstate"}"#,
        );
        assert_eq!(a.get("id"), Some(&Json::Num(7.0)));
        assert_eq!(a.get("ok"), Some(&Json::Bool(true)));
        let Some(Json::Str(output)) = a.get("output") else {
            panic!("no output: {a:?}");
        };
        assert!(output.contains("kick"), "{output}");
        let Some(Json::Arr(warns)) = a.get("warnings") else {
            panic!("no warnings: {a:?}");
        };
        assert!(!warns.is_empty(), "{a:?}");
        assert_eq!(warns[0].get("code"), Some(&Json::Str("W101".into())));
        assert_eq!(warns[0].get("line"), Some(&Json::Num(3.0)));
        // the script ran
        assert!(rx.try_pop().is_some());
        assert_eq!(cp.engine_state.voices.len(), 1);
    }

    #[test]
    fn script_error_answer_has_code_line_col_suggestion() {
        let mut cp = processor();
        let (mut tx, _rx) = cmd_queue(16);
        let a = req(
            &mut cp,
            &mut tx,
            r#"{"id": 8, "script": "voice kick from \"kick\"\nkick { lfo w depth=0.3 to=pan in 8 }"}"#,
        );
        assert_eq!(a.get("ok"), Some(&Json::Bool(false)));
        let e = first_err(&a);
        assert!(
            matches!(e.get("code"), Some(Json::Str(c)) if c.starts_with('E')),
            "{a:?}"
        );
        assert_eq!(e.get("line"), Some(&Json::Num(2.0)));
        assert!(matches!(e.get("col"), Some(Json::Num(c)) if *c >= 1.0));
        assert!(matches!(e.get("message"), Some(Json::Str(m)) if !m.is_empty()));
        assert_eq!(e.get("file"), None);

        // a misspelt name carries a suggestion
        let a = req(&mut cp, &mut tx, r#"{"script": "voice hat from \"hta\""}"#);
        let e = first_err(&a);
        assert!(
            matches!(e.get("suggestion"), Some(Json::Str(s)) if s.contains("hat")),
            "{a:?}"
        );
    }

    #[test]
    fn check_does_not_change_session() {
        let mut cp = processor();
        let (mut tx, mut rx) = cmd_queue(16);
        let a = req(
            &mut cp,
            &mut tx,
            r#"{"id": "c", "check": "voice kick from \"kick\""}"#,
        );
        assert_eq!(a.get("ok"), Some(&Json::Bool(true)));
        assert_eq!(a.get("output"), Some(&Json::Str(String::new())));
        assert!(cp.engine_state.voices.is_empty());
        assert!(rx.try_pop().is_none());
        // and it reports errors
        let a = req(&mut cp, &mut tx, r#"{"check": "start nobody"}"#);
        assert_eq!(a.get("ok"), Some(&Json::Bool(false)));
    }

    #[test]
    fn script_is_whole_or_nothing() {
        let mut cp = processor();
        let (mut tx, mut rx) = cmd_queue(16);
        let a = req(
            &mut cp,
            &mut tx,
            r#"{"script": "voice kick from \"kick\"\nstart nobody"}"#,
        );
        assert_eq!(a.get("ok"), Some(&Json::Bool(false)));
        assert_eq!(first_err(&a).get("line"), Some(&Json::Num(2.0)));
        assert!(cp.engine_state.voices.is_empty());
        assert!(rx.try_pop().is_none());
    }

    #[test]
    fn bad_json_is_j001() {
        let mut cp = processor();
        let (mut tx, _rx) = cmd_queue(16);
        for line in [
            "not json",
            "[1]",
            "{\"id\": 3}",
            "{\"script\": 4}",
            "{\"id\":1} x",
        ] {
            let a = req(&mut cp, &mut tx, line);
            assert_eq!(a.get("ok"), Some(&Json::Bool(false)), "{line}");
            let e = first_err(&a);
            assert_eq!(e.get("code"), Some(&Json::Str("J001".into())), "{line}");
            let Some(Json::Str(m)) = e.get("message") else {
                panic!("{line}");
            };
            assert!(m.starts_with("not a request: "), "{m}");
        }
        // a request with an id keeps it in the J001
        let a = req(&mut cp, &mut tx, "{\"id\": 3}");
        assert_eq!(a.get("id"), Some(&Json::Num(3.0)));
    }

    #[test]
    fn missing_id_is_null() {
        let mut cp = processor();
        let (mut tx, _rx) = cmd_queue(16);
        let a = req(&mut cp, &mut tx, r#"{"script": "state"}"#);
        assert_eq!(a.get("id"), Some(&Json::Null));
        let a = req(&mut cp, &mut tx, r#"{"id": [1], "script": "state"}"#);
        assert_eq!(a.get("id"), Some(&Json::Null));
        assert_eq!(a.get("ok"), Some(&Json::Bool(true)));
    }

    #[test]
    fn string_id_echoed() {
        let mut cp = processor();
        let (mut tx, _rx) = cmd_queue(16);
        let a = req(&mut cp, &mut tx, r#"{"id": "a\"b", "script": "state"}"#);
        assert_eq!(a.get("id"), Some(&Json::Str("a\"b".into())));
    }

    #[test]
    fn history_is_ignored_in_agent_mode() {
        let mut cp = processor();
        let (mut tx, _rx) = cmd_queue(16);
        cp.history_out.push("x".into());
        req(&mut cp, &mut tx, r#"{"script": "state"}"#);
        assert!(cp.history_out.is_empty());
    }

    #[test]
    fn event_lines_for_engine_notices() {
        let cp = processor();
        let names = &cp.engine_state;
        let line = event_line(&Event::GarbageLeaked { count: 3 }, names).unwrap();
        let j = parse(&line).unwrap();
        assert_eq!(j.get("event"), Some(&Json::Str("warn".into())));
        assert!(matches!(j.get("message"), Some(Json::Str(m)) if m.contains("leaked 3")));
        let line = event_line(
            &Event::TempoInactive {
                owner: TempoOwner::Context,
            },
            names,
        )
        .unwrap();
        assert!(line.contains("tempo not active"), "{line}");
        // the mirror's events and quit have no warn line
        assert_eq!(event_line(&Event::Quit, names), None);
        assert_eq!(
            event_line(
                &Event::BufferEnded {
                    slot: 0,
                    take: 0,
                    frames: 1
                },
                names
            ),
            None
        );
        assert_eq!(
            parse(QUIT_LINE).unwrap().get("event"),
            Some(&Json::Str("quit".into()))
        );
        // a buffer notice
        let j = parse(&notice_line("buf", "buf b: wrote \"x\".wav")).unwrap();
        assert_eq!(j.get("event"), Some(&Json::Str("buf".into())));
        assert_eq!(
            j.get("message"),
            Some(&Json::Str("buf b: wrote \"x\".wav".into()))
        );
    }

    #[test]
    fn script_event_wraps_the_outcome() {
        let j = parse(&script_event(Ok("hi".into()))).unwrap();
        assert_eq!(j.get("event"), Some(&Json::Str("script".into())));
        assert_eq!(j.get("ok"), Some(&Json::Bool(true)));
        assert_eq!(j.get("output"), Some(&Json::Str("hi".into())));
        let j = parse(&script_event(Err(CmdErr::QueueFull))).unwrap();
        assert_eq!(j.get("ok"), Some(&Json::Bool(false)));
    }
}
