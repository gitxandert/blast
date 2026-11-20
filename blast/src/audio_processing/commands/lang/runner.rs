//! Timed scripts (PKb): a script with `wait`s runs in the background.
//!
//! The script's lines are cut into **segments** at each lone `wait` (a line
//! of its own). Segment 0 goes to the engine at once, like any script. Each
//! later segment is lowered ahead of time and sent as one `Schedule` due
//! `AfterMark`: `wait` ticks after the position where the segment before it
//! really fired (the engine records that position, `SetMark`). Waits add up
//! without drift, and a paused clock delays everything after it.
//!
//! The runner keeps at most two segments sent beyond the one that sounds.
//! When the mirror reports that the oldest one has fired, `pump` lowers and
//! sends the next. So the mirror is up to two segments ahead of the sound,
//! and an object a later segment names may be removed in the meantime: that
//! line is skipped (W107).
//!
//! `end` stops what the script started (`start` statements) and cancels
//! what it still has waiting in the engine (`CancelScript`). The script
//! leaves the list when its last segment has fired.

use std::collections::VecDeque;
use std::path::Path;

use super::ast::{ActionWord, EndStmt, Line, Part, Script, SourceStmt, Stmt, Wait};
use super::errors::{Code, LangErr, Span};
use super::lower::{Errs, Out};
use super::script::{lone_source, shown, tag};
use crate::audio_processing::commands::{
    AnyId, BatchArgs, CancelScriptArgs, CmdProcessor, CmdSender, Command, MarkGlidesArgs,
    ScheduleArgs, SetMarkArgs, Target as Eng, TempoId, WhenSpec, cannot_stage,
    reprs::schedule_count,
};

/// Segments sent beyond the one that sounds, live.
const AHEAD: usize = 2;

/// The scripts that are running, and the one being lowered.
#[derive(Clone)]
pub struct Runner {
    /// Segments sent beyond the one that sounds: `AHEAD` live; `--export`
    /// has no audio thread to keep up with and sets it high (the table of
    /// waiting lines, `Limits::scheduled`, still bounds it).
    ahead: usize,
    scripts: Vec<Running>,
    /// Scripts that are over but still have tagged entries in the engine:
    /// their slots stay taken until those retire.
    draining: Vec<Drain>,
    /// While a script's lines are lowered: which one.
    cur: Option<Cur>,
    /// Set while the lines of a later segment are lowered: they are
    /// numbered and counted when the runner has wrapped them.
    pub(super) defer_numbering: bool,
    /// In a check: a lone `wait` has been passed.
    waited: bool,
}

/// A slot held until the entries tagged with it (by number) have retired.
#[derive(Clone)]
struct Drain {
    slot: u8,
    seqs: Vec<u64>,
}

#[derive(Clone)]
struct Cur {
    slot: u8,
    /// What `start` statements started, with the names they had.
    started: Vec<(Eng, String)>,
}

/// A script that is running.
#[derive(Clone)]
struct Running {
    /// The file's name without its extension: what `end NAME` takes.
    name: String,
    slot: u8,
    /// Segments not lowered yet, next first.
    rest: VecDeque<Segment>,
    /// The number of the next segment to lower.
    next: u32,
    /// Segments sent whose entry has not fired, oldest first.
    sent: VecDeque<Sent>,
    /// Entries (by number) that hold an `end` and have not fired.
    end_seqs: Vec<u64>,
    started: Vec<(Eng, String)>,
    /// Every entry sent for it that carries its slot, by number.
    seqs: Vec<u64>,
    /// An `end` of its own fired: nothing more is sent.
    ended: bool,
}

/// A segment sent to the engine.
#[derive(Clone)]
struct Sent {
    seq_no: u64,
    /// The clock its wait counts on.
    tempo: TempoId,
    /// Where the wait that leads to it is written.
    line: u32,
    text: String,
}

/// A line of a script with the file it came from (sourced files are
/// spliced in).
#[derive(Clone)]
pub(super) struct FlatLine {
    pub(super) line: Line,
    pub(super) file: String,
}

/// The lines between two waits.
#[derive(Clone)]
struct Segment {
    /// The wait that leads to it; `None` for the first.
    wait: Option<Wait>,
    /// The wait as written, and its line.
    text: String,
    line: u32,
    lines: Vec<FlatLine>,
}

impl Default for Runner {
    fn default() -> Self {
        Runner {
            ahead: AHEAD,
            scripts: Vec::new(),
            draining: Vec::new(),
            cur: None,
            defer_numbering: false,
            waited: false,
        }
    }
}

impl Runner {
    /// A copy to check against: the names and slots in use, nothing to send.
    pub(super) fn fork(&self) -> Runner {
        Runner {
            ahead: self.ahead,
            scripts: self
                .scripts
                .iter()
                .map(|r| Running {
                    rest: VecDeque::new(),
                    sent: VecDeque::new(),
                    ..r.clone()
                })
                .collect(),
            draining: self.draining.clone(),
            cur: None,
            defer_numbering: false,
            waited: false,
        }
    }

    // a script that is over: its slot is held until its entries retire
    fn drain(&mut self, run: Running) {
        self.draining.push(Drain {
            slot: run.slot,
            seqs: run.seqs,
        });
    }

    /// The running scripts as a comment for `state`; empty if none.
    pub(super) fn describe(&self) -> String {
        if self.scripts.is_empty() {
            return String::new();
        }
        let items: Vec<String> = self
            .scripts
            .iter()
            .map(|r| match r.sent.front() {
                Some(s) => format!("{}  line {}, next {}", r.name, s.line, s.text),
                None => format!("{}  ending", r.name),
            })
            .collect();
        format!("\n# scripts: {}", items.join("; "))
    }

    fn free_slot(&self, limit: u32) -> Option<u8> {
        (0..limit.min(u32::from(u8::MAX)) as u8).find(|slot| {
            self.scripts.iter().all(|r| r.slot != *slot)
                && self.draining.iter().all(|d| d.slot != *slot)
                && self.cur.as_ref().is_none_or(|c| c.slot != *slot)
        })
    }
}

/// The `end` a line consists of, if it is nothing else.
fn lone_end(line: &Line) -> Option<&EndStmt> {
    match (line.parts.as_slice(), &line.when) {
        ([Part::And(stmts)], None) => match stmts.as_slice() {
            [Stmt::End(e)] => Some(e),
            _ => None,
        },
        _ => None,
    }
}

/// Whether the lines make a timed script: a lone `wait`, or an `end`.
pub(super) fn is_timed(flat: &[FlatLine]) -> bool {
    flat.iter()
        .any(|f| f.line.lone_wait().is_some() || f.line.stmts().any(|s| matches!(s, Stmt::End(_))))
}

/// A line's text without its first word (`wait 4 by clk` → `4 by clk`).
fn after_first_word(text: &str) -> &str {
    text.split_once(char::is_whitespace)
        .map_or("", |(_, rest)| rest.trim())
}

/// Cuts the lines into segments at each lone `wait`. A lone `end` is the
/// last line that runs; `end after D` is a wait, then an `end`.
fn segments_of(flat: Vec<FlatLine>) -> Vec<Segment> {
    let mut segs = vec![Segment {
        wait: None,
        text: String::new(),
        line: 0,
        lines: Vec::new(),
    }];
    for f in flat {
        if let Some(w) = f.line.lone_wait() {
            segs.push(Segment {
                wait: Some(w.clone()),
                text: format!("wait {}", after_first_word(&f.line.text)),
                line: f.line.span.line,
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(e) = lone_end(&f.line) {
            if let Some(w) = &e.after {
                // the same line without its `after`
                let mut line = f.line.clone();
                if let Some(Part::And(stmts)) = line.parts.first_mut()
                    && let Some(Stmt::End(end)) = stmts.first_mut()
                {
                    end.after = None;
                }
                segs.push(Segment {
                    wait: Some(w.clone()),
                    text: format!("end {}", after_first_word(&f.line.text)),
                    line: f.line.span.line,
                    lines: vec![FlatLine { line, file: f.file }],
                });
            } else if let Some(last) = segs.last_mut() {
                last.lines.push(f);
            }
            // nothing after a written `end` runs
            break;
        }
        if let Some(last) = segs.last_mut() {
            last.lines.push(f);
        }
    }
    segs
}

fn cannot_wait(span: Span) -> LangErr {
    LangErr::new(
        Code::CannotWait,
        span,
        "that can't wait yet: buffers, tracks, removing an effect and moving onto a clock of its own are not staged",
    )
    .with_hint("do it before the wait, or at the prompt")
}

// every `Schedule` in the tree belongs to script `slot`
fn tag_script(cmd: &mut Command, slot: u8) {
    match cmd {
        Command::Schedule(a) => {
            a.script = Some(slot);
            tag_script(&mut a.cmd, slot);
        }
        Command::Batch(b) => {
            for c in &mut b.cmds {
                tag_script(c, slot);
            }
        }
        _ => {}
    }
}

// whether `cmd` cancels a script itself (not in an entry nested in it)
fn cancels(cmd: &Command) -> bool {
    match cmd {
        Command::CancelScript(_) => true,
        Command::Batch(b) => b.cmds.iter().any(cancels),
        _ => false,
    }
}

// the numbers of the entries in the tree that hold an `end`
fn end_entries(cmd: &Command, out: &mut Vec<u64>) {
    match cmd {
        Command::Schedule(a) => {
            if cancels(&a.cmd) {
                out.push(a.seq_no);
            }
            end_entries(&a.cmd, out);
        }
        Command::Batch(b) => {
            for c in &b.cmds {
                end_entries(c, out);
            }
        }
        _ => {}
    }
}

// the numbers of the entries in the tree that carry a script tag
fn tagged_seqs(cmd: &Command, out: &mut Vec<u64>) {
    match cmd {
        Command::Schedule(a) => {
            if a.script.is_some() {
                out.push(a.seq_no);
            }
            tagged_seqs(&a.cmd, out);
        }
        Command::Batch(b) => {
            for c in &b.cmds {
                tagged_seqs(c, out);
            }
        }
        _ => {}
    }
}

fn one(mut cmds: Vec<Command>) -> Command {
    match cmds.len() {
        1 => cmds.remove(0),
        _ => Command::Batch(BatchArgs { cmds }),
    }
}

/// A script's name: its file name without the extension.
pub(super) fn script_name(path: &Path) -> String {
    path.file_stem()
        .map_or_else(|| shown(path), |s| s.to_string_lossy().into_owned())
}

impl CmdProcessor {
    /// The lines of a script file, with the files it sources spliced in;
    /// `None` if it can't be read (the usual run reports why).
    pub(super) fn flatten_file(&mut self, path: &Path, span: Span) -> Option<Vec<FlatLine>> {
        let mut out = Vec::new();
        self.with_file(path, span, |me, script, name| {
            me.flatten_script(script, name, &mut out)
        })
        .ok()?
        .ok()?;
        Some(out)
    }

    /// `flatten_file` for text.
    pub(super) fn flatten_text(&mut self, name: &str, text: &str) -> Option<Vec<FlatLine>> {
        let mut out = Vec::new();
        self.with_text(name, text, |me, script, name| {
            me.flatten_script(script, name, &mut out)
        })
        .ok()?
        .ok()?;
        Some(out)
    }

    fn flatten_script(
        &mut self,
        script: &Script,
        file: &str,
        out: &mut Vec<FlatLine>,
    ) -> Result<(), Vec<LangErr>> {
        for line in &script.lines {
            if let Some(src) = lone_source(line) {
                let path = self.resolve_source(&src.path);
                self.with_file(&path, src.span, |me, sub, name| {
                    me.flatten_script(sub, name, out)
                })
                .map_err(|e| tag(e, file))??;
                continue;
            }
            out.push(FlatLine {
                line: line.clone(),
                file: file.to_string(),
            });
        }
        Ok(())
    }

    /// A lone `wait` in a check: its clock and duration must be good.
    pub(super) fn check_wait(&mut self, wait: &Wait, file: &str, errs: &mut Vec<LangErr>) {
        if let Err(Errs(es)) = self.wait_ticks(wait) {
            errs.extend(tag(es, file));
        }
        self.runner.waited = true;
    }

    /// In a check: a lone `wait` has been passed.
    pub(super) fn waited_in_check(&self) -> bool {
        self.runner.waited
    }

    /// In a check: a line after a wait may create and remove (PKb2), except
    /// what the mirror can't take back yet.
    pub(super) fn check_after_wait(&self, line: &Line, out: &Out) -> Result<(), Errs> {
        if self.runner.waited && out.cmds.iter().any(cannot_stage) {
            return Err(cannot_wait(line.span).into());
        }
        Ok(())
    }

    /// Sets how many segments a script keeps sent beyond the one that
    /// sounds (`--export`: no audio thread, so the runner may run far
    /// ahead).
    pub fn set_runner_ahead(&mut self, ahead: usize) {
        self.runner.ahead = ahead.max(1);
    }

    /// Whether no script is running or draining any more: every tagged
    /// entry of every script has retired.
    pub fn scripts_idle(&self) -> bool {
        self.runner.scripts.is_empty() && self.runner.draining.is_empty()
    }

    /// Seconds in one beat of `main` now (`None` before `boot`).
    pub fn main_beat_secs(&self) -> Option<f64> {
        let r = self
            .engine_state
            .tempos
            .get(&self.engine_state.main_id()?)?;
        Some(super::lower::tick_secs(r.unit, r.interval))
    }

    /// For each running script, how many segments are sent and not fired.
    #[cfg(test)]
    pub(super) fn runner_ahead(&self) -> Vec<usize> {
        self.runner.scripts.iter().map(|r| r.sent.len()).collect()
    }

    // ---- statements ----

    /// Records what a `start` started, for the script being lowered.
    pub(super) fn note_started(&mut self, targets: &[Eng]) {
        let names: Vec<(Eng, String)> = targets
            .iter()
            .map(|&t| (t, self.engine_state.name_of(any_id(t))))
            .collect();
        if let Some(cur) = &mut self.runner.cur {
            for (t, name) in names {
                if !cur.started.iter().any(|(s, _)| *s == t) {
                    cur.started.push((t, name));
                }
            }
        }
    }

    // the targets in `started` that still exist under the name they had
    fn live_started(&self, started: &[(Eng, String)]) -> Vec<Eng> {
        let es = &self.engine_state;
        started
            .iter()
            .filter(|(t, name)| {
                let id = any_id(*t);
                let exists = match *t {
                    Eng::Voice(v) => es.voices.contains_key(&v),
                    Eng::Group(g) => es.groups.contains_key(&g),
                    Eng::Tempo(t) => es.tempos.contains_key(&t),
                };
                exists && es.name_of(id) == *name
            })
            .map(|(t, _)| *t)
            .collect()
    }

    // `CancelScript` for `slot`, stopping what `started` still names
    fn cancel_command(&mut self, slot: u8, started: &[(Eng, String)]) -> Command {
        let targets = self.live_started(started);
        let stops = if targets.is_empty() {
            Vec::new()
        } else {
            match self.transport(ActionWord::Stop, &targets) {
                Command::Batch(b) => b.cmds,
                cmd => vec![cmd],
            }
        };
        Command::CancelScript(CancelScriptArgs { slot, stops })
    }

    /// `end`, `end NAME`.
    pub(super) fn lower_end(&mut self, e: &EndStmt, out: &mut Out) -> Result<(), Errs> {
        if let Some(w) = &e.after {
            return Err(LangErr::new(
                Code::Unexpected,
                w.span,
                "'end after' is a whole line of a script",
            )
            .with_hint("write: wait D, then end")
            .into());
        }
        if let Some(name) = &e.name {
            return self.end_named(&name.name, name.span, out);
        }
        if let Some(cur) = self.runner.cur.take() {
            let cmd = self.cancel_command(cur.slot, &cur.started);
            self.runner.cur = Some(cur);
            out.push(cmd);
            return Ok(());
        }
        // checking a script file: nothing is running yet
        if !self.src_stack.is_empty() {
            return Ok(());
        }
        let message = match self.runner.scripts.first() {
            None => "end: no script is running".to_string(),
            Some(r) => format!("end: name the script (end {})", r.name),
        };
        Err(LangErr::new(Code::NoScript, e.span, message).into())
    }

    // `end NAME` at the prompt: the script's `end`, from outside
    fn end_named(&mut self, name: &str, span: Span, out: &mut Out) -> Result<(), Errs> {
        self.prompt_only("end NAME", span)?;
        let found = self.take_running(name, span)?;
        for run in found {
            let cmd = self.cancel_command(run.slot, &run.started);
            out.push(cmd);
        }
        Ok(())
    }

    /// `source NAME stop`: cancels a running script and stops nothing.
    pub(super) fn source_stop(&mut self, s: &SourceStmt, out: &mut Out) -> Result<(), Errs> {
        self.prompt_only("source NAME stop", s.span)?;
        let name = script_name(Path::new(s.path.text()));
        for run in self.take_running(&name, s.span)? {
            out.push(Command::CancelScript(CancelScriptArgs {
                slot: run.slot,
                stops: Vec::new(),
            }));
        }
        Ok(())
    }

    fn prompt_only(&self, what: &str, span: Span) -> Result<(), Errs> {
        if self.src_stack.is_empty() && self.runner.cur.is_none() {
            return Ok(());
        }
        Err(LangErr::new(
            Code::Unexpected,
            span,
            format!("'{what}' can only be typed at the prompt"),
        )
        .into())
    }

    // removes the running scripts called `name`; E301 if there are none
    fn take_running(&mut self, name: &str, span: Span) -> Result<Vec<Running>, Errs> {
        let (found, kept): (Vec<Running>, Vec<Running>) = std::mem::take(&mut self.runner.scripts)
            .into_iter()
            .partition(|r| r.name == name);
        self.runner.scripts = kept;
        for run in &found {
            self.runner.draining.push(Drain {
                slot: run.slot,
                seqs: run.seqs.clone(),
            });
        }
        if found.is_empty() {
            let err = LangErr::new(
                Code::UnknownName,
                span,
                format!("no script '{name}' is running"),
            )
            .with_suggestion(name, self.runner.scripts.iter().map(|r| r.name.as_str()));
            return Err(err.into());
        }
        Ok(found)
    }

    // ---- running ----

    /// Starts a timed script: segment 0 and the next two are lowered and
    /// their commands pushed to `out`. Errors name the file they are in.
    pub(super) fn start_timed(
        &mut self,
        name: &str,
        flat: Vec<FlatLine>,
        out: &mut Out,
    ) -> Result<(), Errs> {
        let limit = self.engine_state.limits.scripts;
        self.prune_drains();
        let Some(slot) = self.runner.free_slot(limit) else {
            return Err(LangErr::new(
                Code::LimitReached,
                Span::default(),
                format!("too many scripts are running ({limit})"),
            )
            .with_hint("end one: source NAME stop")
            .into());
        };
        let mut segments: VecDeque<Segment> = segments_of(flat).into();
        let first = segments.pop_front().unwrap_or(Segment {
            wait: None,
            text: String::new(),
            line: 0,
            lines: Vec::new(),
        });
        let mut run = Running {
            name: name.to_string(),
            slot,
            rest: segments,
            next: 1,
            sent: VecDeque::new(),
            end_seqs: Vec::new(),
            started: Vec::new(),
            seqs: Vec::new(),
            ended: false,
        };
        self.runner.cur = Some(Cur {
            slot,
            started: Vec::new(),
        });
        let done = self.send_first(&mut run, first, out);
        self.runner.cur = None;
        self.runner.defer_numbering = false;
        done?;
        if !run.rest.is_empty() || !run.sent.is_empty() || !run.end_seqs.is_empty() {
            self.runner.scripts.push(run);
        } else {
            self.runner.drain(run);
        }
        Ok(())
    }

    fn send_first(&mut self, run: &mut Running, first: Segment, out: &mut Out) -> Result<(), Errs> {
        // the mark first: the engine refuses the entries of a script that has
        // none
        let tempo = match run.rest.front().and_then(|s| s.wait.as_ref()) {
            Some(w) => self
                .wait_clock(w)
                .map_err(|Errs(es)| Errs(tag(es, &first_file(run))))?,
            None => self.main_clock_or_zero(),
        };
        let mut cmds = vec![Command::SetMark(SetMarkArgs {
            slot: run.slot,
            seq: 0,
            tempo,
        })];
        for f in &first.lines {
            let mut lo = Out::default();
            self.lower_line(&f.line, &mut lo)
                .map_err(|Errs(es)| Errs(tag(es, &f.file)))?;
            self.send_shifters(&mut lo.cmds);
            for c in &mut lo.cmds {
                tag_script(c, run.slot);
                end_entries(c, &mut run.end_seqs);
                tagged_seqs(c, &mut run.seqs);
            }
            cmds.append(&mut lo.cmds);
            out.texts.append(&mut lo.texts);
            out.warns.append(&mut lo.warns);
            out.writes.append(&mut lo.writes);
        }
        if let Some(cur) = self.runner.cur.take() {
            run.started = cur.started;
        }
        let mut texts = Vec::new();
        while !run.rest.is_empty() && run.sent.len() < self.runner.ahead {
            match self.lower_next(run, &mut texts) {
                Ok(Some(c)) => cmds.push(c),
                Ok(None) => {}
                // a long lookahead fills the table: the rest goes later
                Err(Errs(es))
                    if !run.sent.is_empty()
                        && es.first().is_some_and(|e| e.code == Code::LimitReached) =>
                {
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        out.texts.append(&mut texts);
        out.cmds.extend(cmds);
        Ok(())
    }

    fn main_clock_or_zero(&self) -> TempoId {
        self.engine_state.main_id().unwrap_or(TempoId(0))
    }

    /// Lowers the next segment of `run` into the `Schedule` that carries it
    /// (`None` if it came to nothing). A line that no longer lowers is
    /// skipped with W107, and says so in `texts`. On `Err` nothing changed
    /// (the mirror is as before) and the segment stays next.
    fn lower_next(
        &mut self,
        run: &mut Running,
        texts: &mut Vec<String>,
    ) -> Result<Option<Command>, Errs> {
        let Some(seg) = run.rest.front().cloned() else {
            return Ok(None);
        };
        let Some(wait) = seg.wait.clone() else {
            run.rest.pop_front();
            return Ok(None);
        };
        let file = seg
            .lines
            .first()
            .map_or_else(String::new, |f| f.file.clone());
        let (tempo, ticks) = self
            .wait_ticks(&wait)
            .map_err(|Errs(es)| Errs(tag(es, &file)))?;
        let next_tempo = match run.rest.get(1).and_then(|s| s.wait.as_ref()) {
            Some(w) => Some(self.wait_clock(w)?),
            None => None,
        };
        let before = (
            self.engine_state.clone(),
            self.vars.clone(),
            run.started.clone(),
        );
        self.runner.cur = Some(Cur {
            slot: run.slot,
            started: std::mem::take(&mut run.started),
        });
        self.runner.defer_numbering = true;
        // the lines of a later segment wait: they may create and remove, and
        // name what an earlier one creates (PKb2)
        self.engine_state.staging = true;
        let mut body = vec![Command::MarkGlides(MarkGlidesArgs {})];
        let mut line_texts = Vec::new();
        let mut fresh_texts = Vec::new();
        let mut early = Vec::new();
        for f in &seg.lines {
            let snapshot = (self.engine_state.clone(), self.vars.clone());
            let mut lo = Out::default();
            let lowered = self
                .lower_line(&f.line, &mut lo)
                .and_then(|()| self.check_not_creating(&f.line, &lo));
            if let Err(Errs(es)) = lowered {
                (self.engine_state, self.vars) = snapshot;
                let why = es.first().map_or_else(String::new, |e| e.message.clone());
                let line = f.line.span.line;
                fresh_texts.push(
                    LangErr::new(
                        Code::ScriptLineSkipped,
                        f.line.span,
                        format!("script {} line {line} skipped: {why}", run.name),
                    )
                    .in_file(&f.file)
                    .to_string(),
                );
                continue;
            }
            // the shifters of a voice that exists already go out at once,
            // not in the entry: the mirror counts them built now, so an entry
            // the engine drops (`stop`, a removed clock) must not take them
            // along. Installing them early is inert.
            early.extend(self.owed_shifters().into_iter().map(|(_, c)| c));
            fresh_texts.extend(lo.texts);
            fresh_texts.extend(
                lo.warns
                    .iter()
                    .map(|w| w.clone().in_file(&f.file).to_string()),
            );
            let (wrote, failed) = self.perform_writes(lo.writes);
            fresh_texts.extend(wrote);
            fresh_texts.extend(failed.iter().map(ToString::to_string));
            if lo.cmds.is_empty() {
                continue;
            }
            let mut cmd = Command::Schedule(ScheduleArgs {
                when: WhenSpec::AfterGlides { watch: None },
                cmd: Box::new(one(lo.cmds)),
                seq_no: 0,
                script: None,
            });
            tag_script(&mut cmd, run.slot);
            body.push(cmd);
            line_texts.push(f.line.text.clone());
        }
        let seq = run.next;
        if let Some(tempo) = next_tempo {
            body.push(Command::SetMark(SetMarkArgs {
                slot: run.slot,
                seq,
                tempo,
            }));
        }
        self.runner.defer_numbering = false;
        self.engine_state.staging = false;
        if let Some(cur) = self.runner.cur.take() {
            run.started = cur.started;
        }
        let mut cmd = Command::Schedule(ScheduleArgs {
            when: WhenSpec::AfterMark {
                slot: run.slot,
                seq: seq - 1,
                tempo,
                ticks,
            },
            cmd: Box::new(Command::Batch(BatchArgs { cmds: body })),
            seq_no: 0,
            script: Some(run.slot),
        });
        let limit = u64::from(self.engine_state.limits.scheduled);
        if self.engine_state.scheduled_pending() + schedule_count(&cmd) > limit {
            (self.engine_state, self.vars, run.started) = before;
            return Err(LangErr::new(
                Code::LimitReached,
                wait.span,
                format!("too many waiting lines ({limit})"),
            )
            .with_hint("wait for one to fire")
            .into());
        }
        let seq_no = self
            .engine_state
            .note_segment(&seg.text, &line_texts, &mut cmd);
        end_entries(&cmd, &mut run.end_seqs);
        tagged_seqs(&cmd, &mut run.seqs);
        run.sent.push_back(Sent {
            seq_no,
            tempo,
            line: seg.line,
            text: seg.text,
        });
        run.rest.pop_front();
        run.next += 1;
        texts.extend(fresh_texts);
        if early.is_empty() {
            return Ok(Some(cmd));
        }
        early.push(cmd);
        Ok(Some(Command::Batch(BatchArgs { cmds: early })))
    }

    // E410 for a line of a later segment that does what can't be staged
    fn check_not_creating(&self, line: &Line, out: &Out) -> Result<(), Errs> {
        if out.cmds.iter().any(cannot_stage) {
            return Err(cannot_wait(line.span).into());
        }
        Ok(())
    }

    /// Runs the scripts that can go on: for each whose oldest sent segment
    /// has fired, lowers and queues the next. Call it after feeding the
    /// engine's events to the mirror. Answers the texts to print (skipped
    /// lines, warnings, waiting lines that were dropped).
    pub fn pump(&mut self, queue: &mut CmdSender) -> Vec<String> {
        // lines the engine dropped after they made or removed something
        let mut texts = self.engine_state.take_notes();
        self.prune_drains();
        if self.runner.scripts.is_empty() {
            return texts;
        }
        let scripts = std::mem::take(&mut self.runner.scripts);
        let mut kept = Vec::with_capacity(scripts.len());
        for mut run in scripts {
            if self.advance(&mut run, queue, &mut texts) {
                kept.push(run);
            } else {
                self.runner.drain(run);
            }
        }
        // nothing is added while pumping
        self.runner.scripts = kept;
        texts
    }

    // frees the slots whose tagged entries have all retired
    fn prune_drains(&mut self) {
        let es = &self.engine_state;
        self.runner
            .draining
            .retain(|d| d.seqs.iter().any(|s| es.is_waiting(*s)));
    }

    // true while the script goes on
    fn advance(
        &mut self,
        run: &mut Running,
        queue: &mut CmdSender,
        texts: &mut Vec<String>,
    ) -> bool {
        while run
            .sent
            .front()
            .is_some_and(|s| !self.engine_state.is_waiting(s.seq_no))
        {
            run.sent.pop_front();
        }
        let before = run.end_seqs.len();
        run.end_seqs.retain(|s| self.engine_state.is_waiting(*s));
        if run.end_seqs.len() < before && !run.ended {
            // its `end` fired (or a `thenif` dropped it): the script is over.
            // Cancelling again is harmless if it fired.
            run.ended = true;
            run.rest.clear();
            if !queue.is_full() {
                let _ = queue.try_push(Command::CancelScript(CancelScriptArgs {
                    slot: run.slot,
                    stops: Vec::new(),
                }));
            }
        }
        if !run.ended
            && let Some(s) = run.sent.front()
            && !self.engine_state.tempos.contains_key(&s.tempo)
        {
            texts.push(format!(
                "script {} is cancelled: the clock its wait counts on was removed",
                run.name
            ));
            let _ = queue.try_push(Command::CancelScript(CancelScriptArgs {
                slot: run.slot,
                stops: Vec::new(),
            }));
            return false;
        }
        while !run.rest.is_empty() && run.sent.len() < self.runner.ahead && !queue.is_full() {
            match self.lower_next(run, texts) {
                Ok(Some(cmd)) => {
                    // can't fail: there was room, and this thread is the
                    // only producer
                    let _ = queue.try_push(cmd);
                }
                Ok(None) => {}
                Err(Errs(es)) => {
                    let still_going = !run.sent.is_empty();
                    // out of room for now: try again when something fired
                    if still_going && es.first().is_some_and(|e| e.code == Code::LimitReached) {
                        break;
                    }
                    for e in &es {
                        texts.push(format!("script {} is cancelled: {e}", run.name));
                    }
                    if !queue.is_full() {
                        let _ = queue.try_push(Command::CancelScript(CancelScriptArgs {
                            slot: run.slot,
                            stops: Vec::new(),
                        }));
                    }
                    return false;
                }
            }
        }
        !(run.rest.is_empty() && run.sent.is_empty() && run.end_seqs.is_empty())
    }
}

fn first_file(run: &Running) -> String {
    run.rest
        .front()
        .and_then(|s| s.lines.first())
        .map_or_else(String::new, |f| f.file.clone())
}

// the ID a target names
fn any_id(t: Eng) -> AnyId {
    match t {
        Eng::Voice(v) => AnyId::Voice(v),
        Eng::Group(g) => AnyId::Group(g),
        Eng::Tempo(t) => AnyId::Tempo(t),
    }
}
