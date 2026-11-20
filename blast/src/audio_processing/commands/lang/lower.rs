//! Lowering: a parsed script becomes one `Command` for the engine, checked
//! against the mirror as it evolves. A script applies whole or not at all
//! (G3): any error restores the mirror to what it was before.

use super::ast::*;
use super::errors::{Code, LangErr, Span};
use super::names::{Named, OUT, any_id, article, check_new, fx_language_kind, lookup};
use super::schema::{
    Checked, CheckedSetting, check_items, db_to_linear, eval_num, hz_is_a_rate, kind_word,
    not_built, pitch_semitones, schema,
};
use super::vars::{self, VarValue};
use crate::audio_processing::blast_time::blast_time::TempoUnit;
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::procs::lfo::LfoChange;
use crate::audio_processing::commands::procs::seq::{
    Euclid, NewSeq, RecvSpec, SeqChange, SeqStep, SlotGen, TrigTarget,
};
use crate::audio_processing::commands::reprs::{
    DEFAULT_PERIOD, EngineState, PacketId, Target as Eng, schedule_count,
};
use crate::audio_processing::commands::tempo::{
    NewTempo, TempoChoice, TempoUser, clock_of, describe_users, movers, voice_followers,
};
use crate::audio_processing::commands::track::{TrackSource, engine_rate, is_raw_path, read_track};
use crate::audio_processing::commands::{
    AnyId, BatchArgs, ClockRun, CmdErr, CmdProcessor, Command, FxOwner, GroupId, Parsed, ProcId,
    ProcOp, ProcOwner, QuitArgs, RESERVED_ALL, ResampleMode, ShiftMode, TempoId, TempoLink,
    TempoOwner, TempoTarget, VoiceId, VoiceMode, VoiceSrc, WhenSpec, cannot_stage,
    creates_or_removes,
};
use crate::audio_processing::engine::BOUNCE_RANGE_S;
use crate::audio_processing::engine::shift::WINDOW_RANGE_S;
use crate::audio_processing::engine::{ChoirMode, LoopMode, VoiceParam};
use crate::audio_processing::processes::LfoRate;
use crate::file_parsing::raw::RawParams;
use crate::file_parsing::{load_assets, scan_assets};

/// One or more errors from lowering.
pub(super) struct Errs(pub Vec<LangErr>);

impl From<LangErr> for Errs {
    fn from(e: LangErr) -> Self {
        Errs(vec![e])
    }
}

impl From<Vec<LangErr>> for Errs {
    fn from(e: Vec<LangErr>) -> Self {
        Errs(e)
    }
}

mod buffer;
mod chain;
mod env;
mod fx;
mod wave;
pub(crate) use wave::note_hz;
mod lfo;

use chain::{Chain, Lowered};

type Res<T> = Result<T, Errs>;

/// What an `on` asks of an object (see `on_change`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnChange {
    Same,
    Link,
    Move,
}

/// One voice setting, ready for the engine (`slice` is 0-based).
#[derive(Clone, Copy, Debug, PartialEq)]
enum VoiceSetting {
    Param(VoiceParam, f32, Glide),
    Mode(VoiceMode),
    /// `window=`: the spectral shifter's window, a length. Mirror-only: the
    /// shifters it rebuilds are sent by `send_shifters`.
    Window(Glide),
    /// `bounce=`: the fade at a ping-pong turnaround, a length. Resolved to
    /// frames (clamped) and sent as `VoiceMode::Bounce`.
    Bounce(Glide),
}

/// How long a setting takes to get to its value (`over`), before the
/// voice's clock turns it into frames.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Glide {
    Instant,
    /// Periods of the voice (`in N`).
    Periods(f64),
    Ticks(f64),
    Secs(f64),
}

impl Glide {
    fn of(over: Option<&Over>) -> Result<Glide, LangErr> {
        let Some(o) = over else {
            return Ok(Glide::Instant);
        };
        Glide::of_value(&o.value, o.span)
    }

    // a length written as a value: a bare number is periods
    fn of_value(value: &Value, span: Span) -> Result<Glide, LangErr> {
        Ok(match value {
            Value::Num(n) => Glide::Periods(eval_num(n)?),
            Value::Duration(n, unit) => {
                let n = eval_num(n)?;
                match unit {
                    DurUnit::Ticks => Glide::Ticks(n),
                    DurUnit::S => Glide::Secs(n),
                    DurUnit::Ms => Glide::Secs(n / 1e3),
                    DurUnit::Us => Glide::Secs(n / 1e6),
                    DurUnit::Ns => Glide::Secs(n / 1e9),
                    DurUnit::Frames => Glide::Secs(n / frame_rate()),
                    DurUnit::Hz => return Err(hz_is_a_rate(span)),
                }
            }
            _ => Glide::Instant,
        })
    }
}

/// E402: a depth in `st` or `c` on something that isn't pitch.
pub(super) fn check_depth_unit(
    checked: &Checked,
    target: Option<&str>,
    now: Option<&str>,
) -> Result<(), LangErr> {
    let Some(d) = checked.setting("depth") else {
        return Ok(());
    };
    if !matches!(d.value, Value::Pitch(..)) || target.or(now) == Some("pitch") {
        return Ok(());
    }
    Err(wrong_type(
        d.span,
        "st and c are for a depth on pitch (to=pitch depth=7st)",
    ))
}

fn frame_rate() -> f64 {
    match sample_rate::get() {
        0 => 48_000.0,
        n => f64::from(n),
    }
}

/// What lowering produces: commands, and warnings for statements that
/// change nothing.
#[derive(Default)]
pub(super) struct Out {
    pub(super) cmds: Vec<Command>,
    pub(super) warns: Vec<LangErr>,
    /// What queries (`state`, `help`, `check`) answered.
    pub(super) texts: Vec<String>,
    /// Files `state … to NAME` will write, once the whole input lowered.
    pub(super) writes: Vec<Write>,
}

/// A file to write: where, the text, and where it was asked for.
pub(super) struct Write {
    pub(super) path: std::path::PathBuf,
    pub(super) text: String,
    pub(super) span: Span,
}

impl Out {
    // a Batch is flattened, so a chain is one Batch
    pub(super) fn push(&mut self, cmd: Command) {
        match cmd {
            Command::Batch(args) => self.cmds.extend(args.cmds),
            cmd => self.cmds.push(cmd),
        }
    }

    fn warn_unchanged(&mut self, span: Span, what: &str) {
        self.warns.push(LangErr::new(
            Code::ChangesNothing,
            span,
            format!("{what} is already as stated"),
        ));
    }
}

/// E407: something still reads what was asked to go.
fn in_use(span: Span, what: &str, path: &[Ident], users: &str) -> Errs {
    let name = path.last().map_or("", |i| i.name.as_str());
    LangErr::new(
        Code::InUse,
        span,
        format!("can't remove {what} '{name}': used by {users}"),
    )
    .into()
}

fn wrong_type(span: Span, msg: impl Into<String>) -> LangErr {
    LangErr::new(Code::WrongType, span, msg)
}

fn clock_ref_span(c: &ClockRef) -> Span {
    match c {
        ClockRef::Lit(l) => l.span,
        ClockRef::Path(p) => p.first().map(|i| i.span).unwrap_or_default(),
    }
}

/// The unit and interval a clock literal stands for.
fn clock_lit(c: &ClockLit) -> Res<(TempoUnit, f32)> {
    let v = eval_num(&c.n)?;
    if !(v.is_finite() && v > 0.0) {
        return Err(LangErr::new(
            Code::OutOfRange,
            c.span,
            format!("a clock rate must be above 0, not {v}"),
        )
        .into());
    }
    let Some(unit) = c.unit else {
        return Err(LangErr::new(Code::WrongType, c.span, "a clock needs a unit: ($x)b").into());
    };
    let (unit, v) = match unit {
        DurUnit::Ticks => (TempoUnit::Bpm, v),
        DurUnit::Ms => (TempoUnit::Millis, v),
        DurUnit::S => (TempoUnit::Millis, v * 1000.0),
        DurUnit::Us => (TempoUnit::Millis, v / 1000.0),
        DurUnit::Ns => (TempoUnit::Millis, v / 1.0e6),
        DurUnit::Frames => (TempoUnit::Samples, v),
        DurUnit::Hz => return Err(hz_is_a_rate(c.span).into()),
    };
    Ok((unit, v as f32))
}

/// The process kinds a block may hold that aren't built yet (their tasks
/// build them).
fn process_task(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::Wave => schema(kind).built,
        _ => None,
    }
}

/// The most steps one range may expand to.
const MAX_RANGE: f64 = 65_536.0;

/// A seq's timing feel from its settings: what a step uses when it has no
/// `%` or `~` of its own. Chance is a percentage; early and late are ticks.
#[derive(Clone, Copy, Debug)]
struct Feel {
    chance: f64,
    early: f64,
    late: f64,
}

impl Default for Feel {
    fn default() -> Self {
        Feel {
            chance: 100.0,
            early: 0.0,
            late: 0.0,
        }
    }
}

/// A seq's feel with the glide each of its values asked for (`over`):
/// chance, early, late.
#[derive(Clone, Copy, Debug)]
struct FeelSet {
    feel: Feel,
    glide: [Glide; 3],
}

impl Feel {
    fn of(values: [f32; 3]) -> Feel {
        Feel {
            chance: f64::from(values[0]),
            early: f64::from(values[1]),
            late: f64::from(values[2]),
        }
    }

    fn values(self) -> [f32; 3] {
        [self.chance as f32, self.early as f32, self.late as f32]
    }
}

/// What a seq already has for its steps.
struct Have<'a> {
    r#gen: Option<Euclid>,
    slots: Option<&'a SlotGen>,
    /// The steps of a seq that wasn't generated.
    written: &'a [SeqStep],
}

/// Where a seq's steps come from: the steps to put (if any), the generator
/// and the written grid and rotation the seq ends up with.
struct StepSource(Option<Vec<SeqStep>>, Option<Euclid>, Option<SlotGen>);

/// A generator's steps for a cycle of `period` ticks, with the seq-wide
/// `[chance, early, late]`. E403 if it has more hits than slots.
fn generated_steps(g: Euclid, period: u32, feel: [f32; 3], at: Span) -> Res<Vec<SeqStep>> {
    let grid = g.grid.unwrap_or(period);
    if g.hits > grid {
        let hint = match g.grid {
            Some(_) => "raise grid= or lower steps=".to_string(),
            None => format!("the grid is the cycle's {period} beats; raise 'in' or set grid="),
        };
        return Err(LangErr::new(
            Code::OutOfRange,
            at,
            format!("steps={} needs grid={} or more", g.hits, g.hits),
        )
        .with_hint(hint)
        .into());
    }
    let mut steps = g.steps(period);
    for s in &mut steps {
        s.chance = feel[0];
        s.early = feel[1];
        s.late = feel[2];
    }
    Ok(steps)
}

/// Whether two step lists hold the same positions and values (the marks
/// the steps wrote don't matter).
fn same_values(a: &[SeqStep], b: &[SeqStep]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.at == y.at && x.chance == y.chance && x.early == y.early && x.late == y.late
        })
}

/// Seconds in one tick of a clock.
pub(super) fn tick_secs(unit: TempoUnit, interval: f32) -> f64 {
    let sr = match sample_rate::get() {
        0 => 48_000.0,
        n => f64::from(n),
    };
    let interval = f64::from(interval);
    match unit {
        TempoUnit::Bpm => 60.0 / interval,
        TempoUnit::Millis => interval / 1000.0,
        TempoUnit::Samples => interval / sr,
    }
}

/// The period of a clock, in ticks, for a bare `over N` (a clock has no `in`).
const CLOCK_PERIOD: u32 = 4;

/// A glide in frames of the voice's clock at this moment (0 = instant).
fn glide_frames(g: Glide, period: u32, tick_secs: f64) -> u32 {
    let secs = match g {
        Glide::Instant => return 0,
        Glide::Secs(s) => s,
        Glide::Ticks(t) => t * tick_secs,
        Glide::Periods(p) => p * f64::from(period) * tick_secs,
    };
    (secs * frame_rate()).round().max(0.0) as u32
}

/// A feel setting in ticks: a bare number or `b` is beats; time units are
/// converted with `tick` (seconds per tick of the seq's clock).
fn feel_ticks(value: &Value, tick: &dyn Fn() -> Res<f64>) -> Res<f64> {
    let (n, unit) = match value {
        Value::Num(n) => return Ok(eval_num(n)?),
        Value::Duration(n, unit) => (eval_num(n)?, *unit),
        _ => return Ok(0.0),
    };
    let secs = match unit {
        DurUnit::Ticks => return Ok(n),
        DurUnit::S => n,
        DurUnit::Ms => n / 1e3,
        DurUnit::Us => n / 1e6,
        DurUnit::Ns => n / 1e9,
        DurUnit::Frames => {
            let sr = match sample_rate::get() {
                0 => 48_000.0,
                n => f64::from(n),
            };
            n / sr
        }
        // the schema refuses hz and b before it gets here
        DurUnit::Hz => return Ok(0.0),
    };
    Ok(secs / tick()?)
}

/// A seq's steps in a cycle of `period` ticks: ranges expanded (inclusive,
/// step 1), positions checked against `1 <= p < period + 1` and stored
/// 0-based, marks read (`%` chance, `~` jitter), duplicates refused.
///
/// With a `grid` the numbers are slots of it: checked against `1 <= p < grid
/// + 1` and stored as slots (0-based); the caller places them on the cycle.
fn lower_steps(steps: &[&Step], period: u32, grid: Option<u32>, feel: Feel) -> Res<Vec<SeqStep>> {
    let mut errs: Vec<LangErr> = Vec::new();
    let mut out: Vec<SeqStep> = Vec::new();
    let size = grid.unwrap_or(period);
    let end = f64::from(size) + 1.0;

    for step in steps {
        let mut one = || -> Result<(), LangErr> {
            let at = eval_num(&step.at)?;
            let to = step.to.as_ref().map(eval_num).transpose()?.unwrap_or(at);
            if to < at {
                return Err(wrong_type(
                    step.span,
                    format!("a range runs upward, not {at}..{to}"),
                ));
            }
            if to - at >= MAX_RANGE {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    step.span,
                    format!("a range holds at most {MAX_RANGE} steps"),
                ));
            }

            let chance = match &step.chance {
                Some(n) => {
                    let c = eval_num(n)?;
                    if !(0.0..=100.0).contains(&c) {
                        return Err(LangErr::new(
                            Code::OutOfRange,
                            n.span(),
                            format!("a chance is a percentage from 0 to 100, not {c}"),
                        ));
                    }
                    c
                }
                None => feel.chance,
            };
            let (early, late) = match &step.jitter {
                Some(n) => {
                    let a = eval_num(n)?;
                    if a < 0.0 {
                        return Err(LangErr::new(
                            Code::OutOfRange,
                            n.span(),
                            format!("a jitter can't be negative, not {a}"),
                        ));
                    }
                    (a, a)
                }
                None => (feel.early, feel.late),
            };

            let mut p = at;
            while p <= to + 1e-9 {
                if p < 1.0 {
                    let why = if p < 0.5 {
                        "counting starts at 1".to_string()
                    } else {
                        "the cycle starts at 1".to_string()
                    };
                    return Err(LangErr::new(
                        Code::StepOutsideCycle,
                        step.span,
                        format!("step {p}: {why}"),
                    ));
                }
                if p >= end {
                    let what = match grid {
                        Some(g) => format!("step {p} is outside the grid of {g} (1 to {g})"),
                        None => {
                            format!("step {p} is outside the cycle of {period} (1 to {period})")
                        }
                    };
                    return Err(LangErr::new(Code::StepOutsideCycle, step.span, what));
                }
                let at0 = (p - 1.0) as f32;
                if out.iter().any(|s| s.at == at0) {
                    return Err(wrong_type(step.span, format!("step {p} is listed twice")));
                }
                out.push(SeqStep {
                    at: at0,
                    chance: chance as f32,
                    early: early as f32,
                    late: late as f32,
                    own_chance: step.chance.is_some(),
                    own_jit: step.jitter.is_some(),
                });
                p += 1.0;
            }
            Ok(())
        };
        if let Err(e) = one() {
            errs.push(e);
        }
    }
    if errs.is_empty() {
        Ok(out)
    } else {
        Err(errs.into())
    }
}

// E410 for what the mirror can't take back yet if its line is dropped
/// E302: the object is there until the waiting line that removes it runs.
fn in_use_waiting(span: Span, name: &str) -> Errs {
    LangErr::new(
        Code::KindClash,
        span,
        format!("'{name}' is still in use: a waiting line removes it"),
    )
    .with_hint("name it differently, or create it in a line that waits too")
    .into()
}

fn cannot_stage_err(span: Span) -> LangErr {
    LangErr::new(
        Code::CannotWait,
        span,
        "that can't wait yet: buffers, tracks, removing an effect or a wave and moving onto a clock of its own are not staged",
    )
    .with_hint("do it before the wait, or at the prompt")
}

// whether `cmd` (or a command in a batch) loads `voice`
fn loads_voice(cmd: &Command, voice: VoiceId) -> bool {
    match cmd {
        Command::Load(a) => a.voice == voice,
        Command::Batch(b) => b.cmds.iter().any(|c| loads_voice(c, voice)),
        _ => false,
    }
}

impl CmdProcessor {
    /// Lowers a whole script into one command (a Batch if several), or
    /// the warnings as text if nothing needs the engine. On any error the
    /// mirror is restored (G3).
    pub(in crate::audio_processing::commands) fn lower_script(
        &mut self,
        script: &Script,
    ) -> Result<Parsed, Vec<LangErr>> {
        let snapshot = (self.engine_state.clone(), self.vars.clone());
        let runner_before = self.runner.clone();
        let clear_before = self.clear_requested;
        self.outbox.clear();
        let mut out = Out::default();
        for line in &script.lines {
            if let Err(Errs(errs)) = self.lower_line(line, &mut out) {
                (self.engine_state, self.vars) = snapshot;
                // the scripts that run follow the mirror
                self.runner = runner_before;
                self.clear_requested = clear_before;
                return Err(errs);
            }
        }
        let Out {
            mut cmds,
            warns,
            mut texts,
            writes,
        } = out;
        self.send_shifters(&mut cmds);
        // the writes wait until every line lowered; a failed write is
        // reported but rolls nothing back (the mirror has the line)
        let (wrote, failed) = self.perform_writes(writes);
        texts.extend(wrote);
        if !failed.is_empty() {
            if cmds.is_empty() {
                return Err(failed);
            }
            texts.extend(failed.iter().map(ToString::to_string));
        }
        let warned: Vec<String> = warns.iter().map(ToString::to_string).collect();
        Ok(match cmds.len() {
            0 => Parsed::Local(
                texts
                    .into_iter()
                    .chain(warns.iter().map(ToString::to_string))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            n => {
                // the engine gets the commands; the answers of queries wait
                // for `submit` to print them
                texts.extend(warned);
                self.outbox = texts.join("\n");
                if n == 1 {
                    Parsed::Engine(cmds.remove(0))
                } else {
                    Parsed::Engine(Command::Batch(BatchArgs { cmds }))
                }
            }
        })
    }

    // puts the shifters the script made voices want in front of the other
    // commands, so they are in place before `pitch=` or `stretch=` moves; for
    // a voice the script loads, right after its `Load`. Every path that
    // queues lowered commands calls it (`apply_lines`, the runner's
    // segments), not just `lower_script`.
    pub(super) fn send_shifters(&mut self, cmds: &mut Vec<Command>) {
        for (voice, shifters) in self.owed_shifters() {
            let at = cmds
                .iter()
                .position(|c| loads_voice(c, voice))
                .map_or(0, |i| i + 1);
            cmds.insert(at, shifters);
        }
    }

    pub(super) fn lower_line(&mut self, line: &Line, out: &mut Out) -> Res<()> {
        if line.is_simple() && line.when.is_none() {
            for stmt in line.stmts() {
                self.lower_stmt(stmt, out)?;
            }
        } else {
            self.lower_timed(line, out)?;
        }
        // what the line removed: pipes it emptied (W102), waiting lines it
        // dropped (W103)
        for (code, msg) in self.engine_state.settle_warnings() {
            out.warns.push(LangErr::new(code, line.span, msg));
        }
        Ok(())
    }

    /// A line with `at`, `after`, `then`, `thenif` or `wait` (SCHED, PKa):
    /// each part is lowered like any other, in order, then the parts are
    /// wrapped so the engine holds them until their turn. Every check is
    /// here; the engine applies what arrives. The mirror takes the whole
    /// line's changes at once, so `state` shows them before they sound.
    fn lower_timed(&mut self, line: &Line, out: &mut Out) -> Res<()> {
        let when = line.when.as_ref();
        let n = line.parts.len();
        if let Some(when) = when {
            let beats = vars::eval(&self.vars, &when.n)?;
            if !(beats.is_finite() && beats > 0.0) {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    when.n.span(),
                    "the wait must be more than 0 beats",
                )
                .into());
            }
        }
        if let Some(Part::Wait(w)) = line.parts.last() {
            return Err(LangErr::new(
                Code::Unexpected,
                w.span,
                "wait needs 'then' and something to run; a wait of its own goes in a script",
            )
            .into());
        }
        for join in &line.joins {
            if let Join::WaitAnd(span) = join {
                out.warns.push(LangErr::new(
                    Code::WaitAnd,
                    *span,
                    "'and' after a wait means 'then'",
                ));
            }
        }
        // the statements that wait must be about objects
        for (i, part) in line.parts.iter().enumerate() {
            let Part::And(stmts) = part else { continue };
            if when.is_none() && i == 0 {
                continue;
            }
            for stmt in stmts {
                let span = match stmt {
                    Stmt::Object(_) | Stmt::Action(_) => continue,
                    // the running script's own `end` waits like any statement
                    Stmt::End(e) if e.name.is_none() && e.after.is_none() => continue,
                    Stmt::End(e) => e.span,
                    Stmt::Let(l) => l.span,
                    Stmt::Source(s) => s.span,
                    Stmt::Query(_) => when.map_or(line.span, |w| w.span),
                };
                return Err(LangErr::new(
                    Code::Unexpected,
                    span,
                    "only a statement about an object can wait, not a let, source or query",
                )
                .into());
            }
        }
        // the clock is judged before the line changes anything; a line that
        // waits may name what another waiting line makes
        let prev_staging = self.engine_state.staging;
        self.engine_state.staging = prev_staging || when.is_some();
        let counting = match when {
            Some(w) => match &w.by {
                Some(by) => Some(self.by_clock(by)?),
                None => match line.parts.first() {
                    Some(Part::And(stmts)) => stmts.first().and_then(|s| self.stmt_clock(s)),
                    _ => self.main_clock(),
                },
            },
            None => None,
        };
        self.engine_state.staging = prev_staging;
        // a stopped clock never counts, so the line would never fire; a
        // paused one is fine, the line waits for `resume`
        if let (Some(t), Some(w)) = (counting, when)
            && self.clock_is_stopped(t)
        {
            let name = self.engine_state.name_of(AnyId::Tempo(t));
            let fix = match w.by {
                Some(_) => "count on a running clock",
                None => "add by main",
            };
            return Err(LangErr::new(
                Code::ClockOff,
                w.span,
                format!("clock '{name}' is off, so it can't count; {fix}"),
            )
            .into());
        }

        // each part against the mirror as the one before left it
        let mut lowered = Vec::with_capacity(n);
        for (i, part) in line.parts.iter().enumerate() {
            let waits_for = |j: usize| -> Span {
                match (j, when, line.joins.get(j.wrapping_sub(1))) {
                    (0, Some(w), _) => w.span,
                    (_, _, Some(Join::Then(s) | Join::ThenIf(s) | Join::WaitAnd(s))) => *s,
                    (_, Some(w), _) => w.span,
                    _ => line.span,
                }
            };
            match part {
                Part::And(stmts) => {
                    let mut each = Vec::with_capacity(stmts.len());
                    for stmt in stmts {
                        let mut inner = Out::default();
                        // the right side of `then` and the lines of a
                        // segment may create and remove (PKb2); the first
                        // part of an `at` or `after` line may not
                        let prev = self.engine_state.staging;
                        let waits = prev || i > 0 || when.is_some();
                        self.engine_state.staging = waits;
                        let lowered_stmt = self.lower_stmt(stmt, &mut inner);
                        self.engine_state.staging = prev;
                        lowered_stmt?;
                        let Out {
                            cmds, warns, texts, ..
                        } = inner;
                        out.warns.extend(warns);
                        out.texts.extend(texts);
                        if i == 0 && when.is_some() && cmds.iter().any(creates_or_removes) {
                            return Err(LangErr::new(
                                Code::CannotWait,
                                waits_for(i),
                                "creating or removing can't wait for a beat",
                            )
                            .with_hint("start it with 'then', or schedule a start instead")
                            .into());
                        }
                        if waits && cmds.iter().any(cannot_stage) {
                            return Err(cannot_stage_err(waits_for(i)).into());
                        }
                        each.push(cmds);
                    }
                    lowered.push(Lowered::Stmts(each));
                }
                Part::Wait(w) => {
                    let (tempo, ticks) = self.wait_ticks(w)?;
                    lowered.push(Lowered::Wait { tempo, ticks });
                }
            }
        }

        let mut chain = Chain::new(lowered, line.joins.clone());
        let mut cmds = chain.run(0);
        if cmds.is_empty() {
            return Ok(());
        }
        if let Some(w) = when {
            let Some(tempo) = counting else {
                return Err(LangErr::new(
                    Code::Unexpected,
                    w.span,
                    "this line has no clock to count on",
                )
                .with_hint("name one: ... at 4 by main")
                .into());
            };
            let beats = vars::eval(&self.vars, &w.n)?;
            let when_spec = match w.word {
                WhenWord::At => WhenSpec::At {
                    tempo,
                    every: beats,
                },
                WhenWord::After => WhenSpec::After {
                    tempo,
                    ticks: beats,
                },
            };
            cmds = vec![Chain::schedule(when_spec, cmds)];
        }
        // a segment of a timed script is numbered and counted as a whole by
        // the runner, once its lines are wrapped
        if self.runner.defer_numbering {
            out.push(match cmds.len() {
                1 => cmds.remove(0),
                _ => Command::Batch(BatchArgs { cmds }),
            });
            return Ok(());
        }
        // every Schedule in the tree takes a slot in the engine's table
        let limit = u64::from(self.engine_state.limits.scheduled);
        let count: u64 = cmds.iter().map(schedule_count).sum();
        if self.engine_state.scheduled_pending() + count > limit {
            return Err(LangErr::new(
                Code::LimitReached,
                when.map_or(line.span, |w| w.span),
                format!("too many waiting lines ({limit})"),
            )
            .with_hint("wait for one to fire")
            .into());
        }
        let mut cmd = match cmds.len() {
            1 => cmds.remove(0),
            _ => Command::Batch(BatchArgs { cmds }),
        };
        self.engine_state.note_scheduled(&line.text, &mut cmd);
        out.push(cmd);
        Ok(())
    }

    pub(super) fn clock_is_stopped(&self, t: TempoId) -> bool {
        self.engine_state
            .tempos
            .get(&t)
            .is_some_and(|r| r.run == ClockRun::Stopped)
    }

    /// The session clock, which a `wait` counts on unless it says `by`.
    fn main_clock(&self) -> Option<TempoId> {
        self.engine_state.main_id()
    }

    /// The clock a `wait` counts on and how many of its ticks it lasts. A
    /// bare number and `b` are ticks (beats); the time units are clock time,
    /// turned into ticks at the clock's rate now.
    pub(super) fn wait_ticks(&self, w: &Wait) -> Res<(TempoId, f64)> {
        let tempo = self.wait_clock(w)?;
        let secs = |s: f64| -> Res<f64> {
            let Some(r) = self.engine_state.tempos.get(&tempo) else {
                return Err(wrong_type(w.span, "that has no clock to count on").into());
            };
            Ok(s / tick_secs(r.unit, r.interval))
        };
        let ticks = match &w.value {
            Value::Num(n) => vars::eval(&self.vars, n)?,
            Value::Duration(n, unit) => {
                let n = vars::eval(&self.vars, n)?;
                match unit {
                    DurUnit::Ticks => n,
                    DurUnit::S => secs(n)?,
                    DurUnit::Ms => secs(n / 1e3)?,
                    DurUnit::Us => secs(n / 1e6)?,
                    DurUnit::Ns => secs(n / 1e9)?,
                    DurUnit::Frames => secs(n / frame_rate())?,
                    DurUnit::Hz => return Err(hz_is_a_rate(w.span).into()),
                }
            }
            _ => 0.0,
        };
        if !(ticks.is_finite() && ticks > 0.0) {
            return Err(
                LangErr::new(Code::OutOfRange, w.span, "the wait must be more than 0").into(),
            );
        }
        Ok((tempo, ticks))
    }

    /// The clock a `wait` counts on: `main` or its `by`. It must be running.
    pub(super) fn wait_clock(&self, w: &Wait) -> Res<TempoId> {
        let tempo = match &w.by {
            Some(by) => self.by_clock(by)?,
            None => self.main_clock().ok_or_else(|| {
                LangErr::new(
                    Code::Unexpected,
                    w.span,
                    "there is no 'main' clock to wait on",
                )
                .with_hint("name one: wait 4 by clk")
            })?,
        };
        if self.clock_is_stopped(tempo) {
            let name = self.engine_state.name_of(AnyId::Tempo(tempo));
            return Err(LangErr::new(
                Code::ClockOff,
                w.span,
                format!("clock '{name}' is off, so it can't count; wait on a running clock"),
            )
            .into());
        }
        Ok(tempo)
    }

    /// The clock `by` names: a clock, or whatever clock a group, voice or
    /// seq reads.
    fn by_clock(&self, by: &ClockRef) -> Res<TempoId> {
        let ClockRef::Path(path) = by else {
            return Err(wrong_type(clock_ref_span(by), "'by' takes the name of a clock").into());
        };
        let named = lookup(&self.engine_state, path)?;
        self.named_clock(named)
            .ok_or_else(|| wrong_type(clock_ref_span(by), "that has no clock to count on").into())
    }

    /// The clock a named thing reads.
    fn named_clock(&self, named: Named) -> Option<TempoId> {
        let es = &self.engine_state;
        match named {
            Named::Clock(t) => Some(t),
            Named::Voice(v) => es.voices.get(&v).map(|r| r.tempo),
            Named::Group(g) => es.groups.get(&g).map(|r| r.tempo),
            Named::Buffer(b) => es.buffer_by_id(b).and_then(|r| r.clock),
            Named::Seq(p) | Named::Lfo(p) | Named::Env(p) => es.procs.get(&p).map(|r| r.tempo),
            Named::Fx {
                owner: FxOwner::Voice(v),
                ..
            } => es.voices.get(&v).map(|r| r.tempo),
            Named::Fx {
                owner: FxOwner::Group(g),
                ..
            } => es.groups.get(&g).map(|r| r.tempo),
            Named::Wave { voice, .. } => es.voices.get(&voice).map(|r| r.tempo),
            Named::Packet(pk) => es
                .packets
                .get(&pk)
                .and_then(|p| p.seqs.first())
                .and_then(|p| es.procs.get(p))
                .map(|r| r.tempo),
        }
    }

    /// The clock of a statement's first target, if it names one that exists.
    fn stmt_clock(&self, stmt: &Stmt) -> Option<TempoId> {
        let path_clock = |t: &Target| match t {
            Target::Path(path) => lookup(&self.engine_state, path)
                .ok()
                .and_then(|n| self.named_clock(n)),
            Target::Track(..) => None,
        };
        match stmt {
            Stmt::Object(o) => path_clock(&o.target),
            Stmt::Action(a) => match &a.targets {
                ActionTargets::List(list) => list.first().and_then(path_clock),
                ActionTargets::All(_) => self.every_target().first().and_then(|t| match *t {
                    Eng::Tempo(t) => Some(t),
                    Eng::Voice(v) => self.engine_state.voices.get(&v).map(|r| r.tempo),
                    Eng::Group(g) => self.engine_state.groups.get(&g).map(|r| r.tempo),
                }),
            },
            _ => None,
        }
    }

    // A statement of a line that waits is filed as it lowers (PKb2): what it
    // makes and removes in the mirror, until its entry is numbered.
    fn lower_stmt(&mut self, stmt: &Stmt, out: &mut Out) -> Res<()> {
        if !self.engine_state.staging {
            return self.lower_stmt_now(stmt, out);
        }
        let mark = self.engine_state.stage_begin();
        let n = out.cmds.len();
        self.lower_stmt_now(stmt, out)?;
        // a voice that doesn't exist yet gets its shifters with the statement
        // that makes it want them
        for (_, cmd) in self.owed_shifters_pending() {
            out.cmds.push(cmd);
        }
        self.engine_state.stage_end(mark, &out.cmds[n..]);
        Ok(())
    }

    fn lower_stmt_now(&mut self, stmt: &Stmt, out: &mut Out) -> Res<()> {
        // `$x` and `(expr)` become plain numbers first
        let mut stmt = stmt.clone();
        vars::substitute(&self.vars, &mut stmt)?;
        let stmt = &stmt;
        match stmt {
            Stmt::Object(o) => {
                self.lower_object(o, out)?;
                Ok(())
            }
            Stmt::Action(a) => self.lower_action(a, out),
            Stmt::Let(l) => {
                let value: VarValue = vars::let_value(&self.vars, l)?;
                self.vars.set(&l.name.name, value);
                Ok(())
            }
            Stmt::Source(s) => self.lower_source(s, out),
            Stmt::End(e) => self.lower_end(e, out),
            Stmt::Query(Query::Quit) => {
                out.push(Command::Quit(QuitArgs {}));
                Ok(())
            }
            Stmt::Query(Query::Clear) => {
                // only a line typed at the prompt clears the window; a
                // sourced file (or --script) has its file on the stack
                if self.src_stack.is_empty() {
                    self.clear_requested = true;
                }
                Ok(())
            }
            Stmt::Query(Query::State(path, to)) => {
                let mut text = self.state_text(path.as_deref())?;
                match to {
                    None => {
                        // the scripts that are running, as a comment (the
                        // output stays valid input)
                        if path.is_none() {
                            text.push_str(&self.runner.describe());
                        }
                        out.texts.push(text)
                    }
                    Some(to) => out.writes.push(Write {
                        path: self.write_target(to),
                        text,
                        span: to.span(),
                    }),
                }
                Ok(())
            }
            Stmt::Query(Query::Help(topic)) => {
                out.texts.push(super::help::help(topic.as_ref())?);
                Ok(())
            }
            Stmt::Query(Query::Check(path)) => {
                let warns = self.check_written(path)?;
                let mut text = String::from("ok");
                for w in &warns {
                    text.push('\n');
                    text.push_str(&w.to_string());
                }
                out.texts.push(text);
                Ok(())
            }
        }
    }

    // ---- errors ----

    /// A family error from a typed core, with the statement's position.
    fn cmd_err(&self, e: CmdErr, span: Span) -> LangErr {
        match e {
            CmdErr::LimitReached { kind, limit } => LangErr::new(
                Code::LimitReached,
                span,
                format!("too many {kind} (limit {limit})"),
            ),
            CmdErr::QueueFull => LangErr::new(Code::QueueFull, span, "command queue full"),
            CmdErr::NoItem { ty, name } if ty == "track" => {
                LangErr::new(Code::UnknownName, span, format!("unknown track '{name}'"))
                    .with_suggestion(&name, self.engine_state.tracks.keys().map(String::as_str))
            }
            other => LangErr::new(Code::WrongType, span, other.to_string()),
        }
    }

    // ---- objects ----

    /// Lowers an object statement; true if it changed something.
    fn lower_object(&mut self, o: &Object, out: &mut Out) -> Res<bool> {
        if let Some((kind, span)) = o.kind
            && let Some(task) = schema(kind).built
        {
            return Err(not_built(span, &format!("'{}'", kind_word(kind)), task).into());
        }
        let path = match &o.target {
            Target::Path(path) => path,
            Target::Track(name, span) => return self.lower_track(o, name, *span, out),
        };
        let Some(name) = path.last() else {
            return Err(wrong_type(o.span, "a statement needs a name").into());
        };

        // a seq is named by a path through its owner, so it has its own rules
        if matches!(o.kind, Some((Kind::Seq, _))) {
            return self.lower_seq_stmt(o, None, out);
        }
        // so is a packet
        if matches!(o.kind, Some((Kind::Packet, _))) {
            return self.lower_packet_stmt(o, None, out);
        }
        // and an effect
        if matches!(o.kind, Some((Kind::Fx(_), _))) {
            return self.lower_fx_stmt(o, None, out);
        }
        // and a wave
        if matches!(o.kind, Some((Kind::Wave, _))) {
            return self.lower_wave_stmt(o, None, out);
        }
        // and an lfo
        if matches!(o.kind, Some((Kind::Lfo, _))) {
            return self.lower_lfo_stmt(o, None, out);
        }
        // and an env
        if matches!(o.kind, Some((Kind::Env, _))) {
            return self.lower_env_stmt(o, None, out);
        }

        let found = lookup(&self.engine_state, path);
        // a name whose object waits for its removal is still in use (PKb3)
        if let Ok(n) = &found
            && o.kind.is_some()
            && o.items
                .iter()
                .any(|i| matches!(i, Item::From(..) | Item::FromName(_)))
            && !self.engine_state.staging
            && self.engine_state.in_limbo(any_id(*n))
        {
            return Err(LangErr::new(
                Code::KindClash,
                name.span,
                format!("'{}' is still in use: a waiting line removes it", name.name),
            )
            .with_hint("name it differently, or create it in a line that waits too")
            .into());
        }
        if let Ok(Named::Clock(t)) = &found
            && matches!(o.kind, Some((Kind::Clock, _)))
            && !self.engine_state.staging
            && self.engine_state.in_limbo(AnyId::Tempo(*t))
        {
            return Err(in_use_waiting(name.span, &name.name));
        }
        let (kind, existing) = match (o.kind, found) {
            (None, Ok(n)) => (n.kind(), Some(n)),
            (Some((k, _)), Ok(n)) if n.kind() == k => (k, Some(n)),
            (Some((k, _)), Ok(n)) => {
                return Err(LangErr::new(
                    Code::KindClash,
                    name.span,
                    format!(
                        "'{}' is already {}, not {}",
                        name.name,
                        article(n.kind()),
                        article(k)
                    ),
                )
                .into());
            }
            (Some((k, _)), Err(e)) if path.len() == 1 && e.code == Code::UnknownName => (k, None),
            (_, Err(e)) => return Err(e.into()),
        };

        // `kick.beat ...`: an update of a seq
        if kind == Kind::Seq {
            return self.lower_seq_stmt(o, None, out);
        }
        if kind == Kind::Packet {
            return self.lower_packet_stmt(o, None, out);
        }
        if kind == Kind::Lfo {
            return self.lower_lfo_stmt(o, None, out);
        }
        if kind == Kind::Env {
            return self.lower_env_stmt(o, None, out);
        }
        if matches!(existing, Some(Named::Fx { .. })) {
            return self.lower_fx_stmt(o, None, out);
        }
        if matches!(existing, Some(Named::Wave { .. })) {
            return self.lower_wave_stmt(o, None, out);
        }

        if let Some(receivers) = &o.pipe {
            let span = receivers.first().map_or(o.span, |r| r.kind.1);
            return Err(wrong_type(span, format!("{} takes no pipe", kind_word(kind))).into());
        }

        let creating = existing.is_none();
        if creating {
            check_new(&self.engine_state, name)?;
        }
        let sc = schema(kind);
        let checked = check_items(sc, &name.name, &o.items, creating, o.span)?;

        let changed = match (kind, existing) {
            (Kind::Clock, e) => self.lower_clock(o, name, &checked, e, out)?,
            (Kind::Voice, e) => {
                let id = match e {
                    Some(Named::Voice(v)) => Some(v),
                    _ => None,
                };
                self.lower_voice(o, name, &checked, id, out)?
            }
            (Kind::Group, e) => {
                let id = match e {
                    Some(Named::Group(g)) => Some(g),
                    _ => None,
                };
                self.lower_group(o, name, &checked, id, out)?
            }
            (Kind::Buffer, e) => {
                let id = match e {
                    Some(Named::Buffer(b)) => Some(b),
                    _ => None,
                };
                self.lower_buffer(o, name, &checked, id, out)?
            }
            (k, _) => {
                let task = schema(k).built.unwrap_or("LANGc");
                return Err(not_built(o.span, &format!("'{}'", kind_word(k)), task).into());
            }
        };
        if !changed {
            out.warn_unchanged(o.span, &format!("'{}'", name.name));
        }
        Ok(changed)
    }

    /// `track "name" from "path" [format= rate= channels=]`: imports a file as
    /// a track; restating an existing track with another file swaps its
    /// samples (and those of every voice playing it). True if it changed
    /// anything.
    fn lower_track(&mut self, o: &Object, name: &str, name_span: Span, out: &mut Out) -> Res<bool> {
        if let Some((k, kind_span)) = o.kind
            && k != Kind::Track
        {
            return Err(
                wrong_type(kind_span, format!("'{}' can't be a track", kind_word(k))).into(),
            );
        }
        if name.is_empty() {
            return Err(wrong_type(name_span, "a track needs a name").into());
        }
        if o.block.is_some() || o.pipe.is_some() {
            return Err(wrong_type(o.span, "a track takes no block or pipe").into());
        }
        let existing = self.engine_state.tracks.get(name).map(|t| t.source.clone());
        let checked = check_items(
            schema(Kind::Track),
            name,
            &o.items,
            existing.is_none(),
            o.span,
        )?;
        let word = |n: &str| match checked.setting(n).map(|s| s.value) {
            Some(Value::Word(w)) => Some(w.name.clone()),
            _ => None,
        };
        let number = |n: &str| match checked.setting(n).map(|s| s.value) {
            Some(Value::Num(v)) => eval_num(v).ok().map(|v| v as u32),
            _ => None,
        };

        let Some((from, from_span)) = checked.from else {
            // only an existing track gets here, since a new one needs `from`
            if let Some(s) = checked.settings.first() {
                return Err(wrong_type(
                    s.span,
                    format!("{} needs 'from \"file\"' with it", s.spec.name),
                )
                .into());
            }
            out.warn_unchanged(o.span, &format!("'{name}'"));
            return Ok(false);
        };
        let source = TrackSource {
            written: from.to_string(),
            path: self.track_path(from),
            format: word("format"),
            rate: number("rate"),
            channels: number("channels"),
        };
        if let Some(Some(old)) = &existing
            && *old == source
        {
            out.warn_unchanged(o.span, &format!("'{name}'"));
            return Ok(false);
        }
        let given = source.raw_params();
        if given != RawParams::default() && !is_raw_path(&source.path) {
            let at = checked.settings.first().map_or(from_span, |s| s.span);
            return Err(wrong_type(
                at,
                "format, rate and channels are for raw PCM (.raw, .pcm or no extension)",
            )
            .into());
        }
        // decoding happens here, on the command thread (REQ-040)
        let file = read_track(
            name,
            &source.path,
            given,
            engine_rate(),
            self.engine_state.out_channels() as u32,
        )
        .map_err(|why| {
            LangErr::new(
                Code::ImportFailed,
                from_span,
                format!("could not import '{}': {why}", source.path.display()),
            )
        })?;
        if existing.is_none() {
            let cmd = self
                .engine_state
                .add_track_command(file, Some(source))
                .map_err(|e| self.cmd_err(e, name_span))?;
            out.push(cmd);
        } else {
            for cmd in self.reimport_track(name, file, source) {
                out.push(cmd);
            }
        }
        Ok(true)
    }

    /// `rescan`: imports the assets whose names aren't tracks yet, in one
    /// batch; the capacity is checked first.
    fn lower_rescan(&mut self, span: Span, out: &mut Out) -> Res<()> {
        let fail = |why: String| LangErr::new(Code::ImportFailed, span, why);
        let Some(dir) = self.engine_state.assets_dir().map(ToOwned::to_owned) else {
            return Err(fail("there is no assets folder to scan".to_string()).into());
        };
        let assets = scan_assets(&dir)
            .map_err(|e| fail(format!("could not scan '{}': {e}", dir.display())))?;
        let new: Vec<_> = assets
            .into_iter()
            .filter(|a| !self.engine_state.tracks.contains_key(&a.name))
            .collect();
        if new.is_empty() {
            out.texts.push("rescan: no new files".to_string());
            return Ok(());
        }
        let free = self.engine_state.free_track_slots() as usize;
        if new.len() > free {
            return Err(LangErr::new(
                Code::LimitReached,
                span,
                format!(
                    "too many tracks (limit {}): {} new files, room for {free}",
                    self.engine_state.ids.tracks.limit(),
                    new.len()
                ),
            )
            .into());
        }
        let files = load_assets(&new, engine_rate(), self.engine_state.out_channels() as u32)
            .map_err(|e| fail(format!("could not load from '{}': {e}", dir.display())))?;
        let mut names = Vec::with_capacity(files.len());
        for file in files {
            names.push(file.file_name.clone());
            let cmd = self
                .engine_state
                .add_track_command(file, None)
                .map_err(|e| self.cmd_err(e, span))?;
            out.push(cmd);
        }
        out.texts.push(format!(
            "rescan: {} new ({})",
            names.len(),
            names.join(", ")
        ));
        Ok(())
    }

    /// `remove "name"`: refused while voices play the track.
    fn remove_track(&mut self, name: &str, span: Span) -> Res<Command> {
        let es = &self.engine_state;
        let Some(repr) = es.tracks.get(name) else {
            return Err(
                LangErr::new(Code::UnknownName, span, format!("unknown track '{name}'"))
                    .with_suggestion(name, es.tracks.keys().map(String::as_str))
                    .into(),
            );
        };
        let users: Vec<String> = es
            .voices_using_track(repr.id)
            .into_iter()
            .filter_map(|v| es.display_name(v))
            .collect();
        if !users.is_empty() {
            return Err(LangErr::new(
                Code::InUse,
                span,
                format!("can't remove track '{name}': used by {}", users.join(", ")),
            )
            .with_hint("remove those voices first (remove kick)")
            .into());
        }
        // checked above, so the track is there
        self.engine_state
            .drop_track_command(name)
            .ok_or_else(|| wrong_type(span, "that track is gone").into())
    }

    fn lower_clock(
        &mut self,
        o: &Object,
        name: &Ident,
        checked: &Checked,
        existing: Option<Named>,
        out: &mut Out,
    ) -> Res<bool> {
        if let Some(Named::Clock(t)) = existing {
            let Some(lit) = checked.clock else {
                return Ok(false);
            };
            let (unit, interval) = clock_lit(lit)?;
            // a glide needs a clock that has started; a paused one waits for
            // `resume`. The engine never refuses (README 1.2), so this is the
            // only check.
            if lit.over.is_some()
                && self
                    .engine_state
                    .tempos
                    .get(&t)
                    .is_some_and(|r| r.run == ClockRun::Stopped)
            {
                return Err(LangErr::new(
                    Code::ClockOff,
                    lit.span,
                    format!("clock '{}' is stopped, so it can't glide", name.name),
                )
                .with_hint("start it first, or change the rate without over")
                .into());
            }
            let same = self
                .engine_state
                .tempos
                .get(&t)
                .is_some_and(|r| r.unit == unit && r.interval == interval);
            if same {
                return Ok(false);
            }
            // the beat position carries on; seqs on it neither skip nor repeat
            let frames = match lit.over.as_ref() {
                Some(o) => {
                    let g = Glide::of(Some(o))?;
                    // a clock has no `in`: a bare `over N` counts periods of
                    // CLOCK_PERIOD ticks, at the rate the clock has now
                    self.engine_state.tempos.get(&t).map_or(0, |r| {
                        glide_frames(g, CLOCK_PERIOD, tick_secs(r.unit, r.interval))
                    })
                }
                None => 0,
            };
            let cmd = self.retime_clock(t, unit, interval, frames);
            out.push(cmd);
            return Ok(true);
        }
        let Some(lit) = checked.clock else {
            return Err(wrong_type(o.span, "a clock needs a rate").into());
        };
        let (unit, interval) = clock_lit(lit)?;
        if lit.over.is_some() {
            return Err(LangErr::new(
                Code::ClockOff,
                lit.span,
                format!(
                    "clock '{}' doesn't exist yet, so it has no rate to glide from",
                    name.name
                ),
            )
            .with_hint("make it without over, then glide it once it runs")
            .into());
        }
        let cmd = self
            .make_clock(&name.name, unit, interval)
            .map_err(|e| self.cmd_err(e, o.span))?;
        out.push(cmd);
        Ok(true)
    }

    /// The tempo an `on` (or its absence) asks for.
    fn resolve_on(&self, on: Option<&ClockRef>, at: Span) -> Res<(TempoChoice, TempoLink)> {
        let es = &self.engine_state;
        let Some(on) = on else {
            let main = es.main_id().ok_or_else(|| {
                LangErr::new(
                    Code::UnknownClock,
                    at,
                    "there is no clock 'main' in this session",
                )
            })?;
            return Ok((TempoChoice::Shared(main), TempoLink::Parent));
        };
        match on {
            ClockRef::Lit(c) => {
                let (unit, interval) = clock_lit(c)?;
                Ok((
                    TempoChoice::Own(NewTempo { unit, interval }),
                    TempoLink::Own,
                ))
            }
            ClockRef::Path(path) => match lookup(es, path)? {
                Named::Clock(t) => Ok((TempoChoice::Shared(t), TempoLink::Context)),
                Named::Group(g) => Ok((
                    TempoChoice::Shared(es.groups[&g].tempo),
                    TempoLink::Group(g),
                )),
                Named::Voice(v) => Ok((
                    TempoChoice::Shared(es.voices[&v].tempo),
                    TempoLink::Follow(v),
                )),
                Named::Seq(_)
                | Named::Lfo(_)
                | Named::Env(_)
                | Named::Packet(_)
                | Named::Buffer(_)
                | Named::Wave { .. }
                | Named::Fx { .. } => Err(wrong_type(
                    clock_ref_span(on),
                    "'on' takes a clock, a group, a voice, or a rate like 93b",
                )
                .into()),
            },
        }
    }

    /// True if `tempo` is what `choice` asks for, with `owner` owning an
    /// own tempo.
    fn same_clock(&self, tempo: TempoId, choice: TempoChoice, owner: TempoOwner) -> bool {
        match choice {
            TempoChoice::Shared(id) => id == tempo,
            TempoChoice::Own(n) => {
                self.engine_state.tempos.get(&tempo).is_some_and(|t| {
                    t.owner == owner && t.unit == n.unit && t.interval == n.interval
                })
            }
        }
    }

    /// What stating `on` asks of an object that now reads `tempo` through
    /// `link`: nothing, a new link to the clock it already reads, or a move.
    /// A voice or group that restates the clock it reads keeps its link (as
    /// before), unless a voice is followed; a seq's link must match too.
    fn on_change(
        &self,
        now: (TempoId, TempoLink, TempoOwner),
        asked: (TempoChoice, TempoLink),
        strict_link: bool,
    ) -> OnChange {
        let ((tempo, link, owner), (choice, new_link)) = (now, asked);
        if !self.same_clock(tempo, choice, owner) {
            return OnChange::Move;
        }
        let follow =
            matches!(link, TempoLink::Follow(_)) || matches!(new_link, TempoLink::Follow(_));
        if link == new_link || !(strict_link || follow) {
            OnChange::Same
        } else {
            OnChange::Link
        }
    }

    /// `x on …` for an existing voice, group or seq: moves it (and what
    /// follows it) onto the clock, retimes its own clock, or does nothing if
    /// it is already there. True if it changed anything.
    fn put_on_clock(
        &mut self,
        target: TempoTarget,
        name: &str,
        on: &ClockRef,
        strict_link: bool,
        out: &mut Out,
    ) -> Res<bool> {
        let span = clock_ref_span(on);
        let asked = self.resolve_on(Some(on), span)?;
        let Some(now) = clock_of(&self.engine_state, target) else {
            return Ok(false);
        };
        // a clock can't be read from something that reads it from this
        let leader = match asked.1 {
            TempoLink::Follow(v) => Some(TempoTarget::Voice(v)),
            TempoLink::Group(g) => Some(TempoTarget::Group(g)),
            _ => None,
        };
        if let Some(leader) = leader
            && movers(&self.engine_state, target).contains(&leader)
        {
            return Err(LangErr::new(
                Code::WrongType,
                span,
                format!("'{name}' would follow itself"),
            )
            .into());
        }
        if self.on_change(now, asked, strict_link) == OnChange::Same {
            return Ok(false);
        }
        let cmd = self
            .move_to_clock(target, asked.0, asked.1)
            .map_err(|e| self.cmd_err(e, span))?;
        out.push(cmd);
        Ok(true)
    }

    /// A voice's block holds processes; only seqs are built. True if it
    /// held a statement.
    fn lower_voice_block(
        &mut self,
        block: &Block,
        voice: VoiceId,
        owner: &str,
        out: &mut Out,
    ) -> Res<bool> {
        let holds = || format!("{owner}'s block holds processes, effects and waves");
        let mut any = false;
        for line in &block.lines {
            for stmt in line.stmts() {
                any = true;
                let Stmt::Object(c) = stmt else {
                    return Err(wrong_type(line.span, holds()).into());
                };
                match c.kind {
                    Some((Kind::Seq, _)) => {
                        self.lower_seq_stmt(c, Some(ProcOwner::Voice(voice)), out)?;
                    }
                    Some((Kind::Packet, _)) => {
                        self.lower_packet_stmt(c, Some(ProcOwner::Voice(voice)), out)?;
                    }
                    Some((Kind::Fx(_), _)) => {
                        self.lower_fx_stmt(c, Some(FxOwner::Voice(voice)), out)?;
                    }
                    Some((Kind::Wave, _)) => {
                        self.lower_wave_stmt(c, Some(voice), out)?;
                    }
                    Some((Kind::Lfo, _)) => {
                        self.lower_lfo_stmt(c, Some(ProcOwner::Voice(voice)), out)?;
                    }
                    Some((Kind::Env, _)) => {
                        self.lower_env_stmt(c, Some(ProcOwner::Voice(voice)), out)?;
                    }
                    Some((k, s)) => {
                        return Err(match process_task(k) {
                            Some(task) => not_built(s, &format!("'{}'", kind_word(k)), task),
                            None => wrong_type(c.span, holds()),
                        }
                        .into());
                    }
                    None => return Err(wrong_type(c.span, holds()).into()),
                }
            }
        }
        Ok(any)
    }

    /// A `seq` statement: in a voice's or group's block (`owner` is that
    /// voice or group), or a path `voice.name` / `group.name`. A restated
    /// seq is edited in place; it says so itself (W101) when it changes
    /// nothing. True if it changed something.
    fn lower_seq_stmt(&mut self, o: &Object, owner: Option<ProcOwner>, out: &mut Out) -> Res<bool> {
        self.lower_seq_in(o, owner, None, out)
    }

    /// `lower_seq_stmt`, for a seq that may be inside a packet (`packet`):
    /// it takes the packet's receivers and can't pipe on its own.
    fn lower_seq_in(
        &mut self,
        o: &Object,
        owner: Option<ProcOwner>,
        packet: Option<&PacketCtx>,
        out: &mut Out,
    ) -> Res<bool> {
        let Target::Path(path) = &o.target else {
            return Err(wrong_type(o.span, "a seq is named by a word, not a track").into());
        };
        let Some((name, owner_path)) = path.split_last() else {
            return Err(wrong_type(o.span, "a statement needs a name").into());
        };
        let path_span = path
            .first()
            .map_or(o.span, |first| first.span.to(name.span));

        let owner = match owner {
            Some(p) if owner_path.is_empty() => p,
            Some(_) => {
                return Err(wrong_type(path_span, "in a block a seq is named by one word").into());
            }
            // a seq outside any owner is the session's, and follows `main`
            None if owner_path.is_empty() => ProcOwner::Session,
            None => match lookup(&self.engine_state, owner_path)? {
                Named::Voice(v) => ProcOwner::Voice(v),
                Named::Group(g) => ProcOwner::Group(g),
                other => {
                    return Err(wrong_type(
                        path_span,
                        format!("{} can't hold a seq", article(other.kind())),
                    )
                    .into());
                }
            },
        };

        let existing = self.engine_state.proc_named(owner, &name.name);
        if let Some(p) = existing
            && o.kind.is_some()
            && !self.engine_state.staging
            && self.engine_state.in_limbo(AnyId::Proc(p))
        {
            return Err(in_use_waiting(name.span, &name.name));
        }
        let creating = existing.is_none();
        if creating && name.name == RESERVED_ALL {
            return Err(LangErr::new(
                Code::ReservedWord,
                name.span,
                format!("'{}' is reserved", name.name),
            )
            .into());
        }
        if creating && owner == ProcOwner::Session {
            // a top-level name is one in the session's namespace (D3)
            check_new(&self.engine_state, name)?;
        }
        if creating
            && owner != ProcOwner::Session
            && self.engine_state.packet_of(owner, &name.name).is_some()
        {
            return Err(LangErr::new(
                Code::KindClash,
                name.span,
                format!("'{}' is already a packet", name.name),
            )
            .into());
        }
        if creating && let Some(what) = self.engine_state.effect_or_wave(owner, &name.name) {
            return Err(LangErr::new(
                Code::KindClash,
                name.span,
                format!("'{}' is already {what}", name.name),
            )
            .into());
        }
        if o.block.is_some() {
            return Err(wrong_type(o.span, "seq takes no block").into());
        }
        if let Some(pipe) = &o.pipe
            && let Some(first) = pipe.first()
        {
            let own_packet = existing
                .and_then(|p| self.engine_state.procs.get(&p))
                .is_some_and(|r| r.packet.is_some());
            if packet.is_some() || own_packet {
                return Err(LangErr::new(
                    Code::Unexpected,
                    first.kind.1,
                    "a seq in a packet takes the packet's pipe, not its own",
                )
                .with_hint("restate the packet to change the pipe")
                .into());
            }
        }
        // what a firing step does: the pipe, or (no pipe) trigger the owner
        // (G6). A session seq has no owner to trigger, so it needs a pipe.
        let pipe_recv = match &o.pipe {
            Some(rs) => Some(self.lower_pipe(rs, owner)?),
            None => None,
        };
        if creating && pipe_recv.is_none() && packet.is_none() && owner == ProcOwner::Session {
            return Err(nothing_to_trigger(o.span));
        }

        let checked = check_items(schema(Kind::Seq), &name.name, &o.items, creating, o.span)?;

        // the cycle: `in`, else the seq's own, else its owner's
        let repr = existing.map(|p| self.engine_state.procs[&p].clone());
        let owner_period = self
            .engine_state
            .owner_period(owner)
            .unwrap_or(DEFAULT_PERIOD);
        let period = match (&checked.period, &repr) {
            (Some((n, _)), _) => *n,
            (None, Some(r)) => r.seq.period,
            (None, None) => owner_period,
        };
        let base = repr
            .as_ref()
            .map_or(Feel::default(), |r| Feel::of(r.seq.feel));
        let set = self.seq_feel(
            &checked,
            owner,
            repr.as_ref().map(|r| r.tempo),
            base,
            o.span,
        )?;
        let stored_gen = repr.as_ref().and_then(|r| r.seq.r#gen);
        let stored_slots = repr.as_ref().and_then(|r| r.seq.slots.clone());
        let written: &[SeqStep] = match &repr {
            Some(r) if r.seq.r#gen.is_none() => &r.seq.steps,
            _ => &[],
        };
        let have = Have {
            r#gen: stored_gen,
            slots: stored_slots.as_ref(),
            written,
        };
        let StepSource(steps, target_gen, target_slots) =
            self.seq_step_source(&checked, have, period, set.feel, repr.is_none(), o.span)?;
        // what the generator changes to: `Some(None)` = it is dropped
        let gen_change = (target_gen != stored_gen).then_some(target_gen);
        let slots_change = (target_slots != stored_slots).then_some(target_slots.clone());
        let seed = match checked.setting("seed").map(|s| s.value) {
            Some(Value::Num(n)) => Some(eval_num(n)? as u64),
            _ => None,
        };

        let changed = match repr {
            None => {
                let (choice, link) = match checked.on {
                    Some(on) => self.resolve_on(Some(on), o.span)?,
                    // a session seq follows `main`
                    None if owner == ProcOwner::Session => self.resolve_on(None, o.span)?,
                    // a seq follows its owner's clock (G2)
                    None => {
                        let tempo = self.engine_state.owner_tempo(owner);
                        match (owner, tempo) {
                            (ProcOwner::Voice(_), Some(t)) => {
                                (TempoChoice::Shared(t), TempoLink::Voice)
                            }
                            (ProcOwner::Group(g), Some(t)) => {
                                (TempoChoice::Shared(t), TempoLink::Group(g))
                            }
                            (ProcOwner::Session, _) | (_, None) => {
                                return Err(
                                    wrong_type(o.span, "the seq's owner isn't known").into()
                                );
                            }
                        }
                    }
                };
                let recv = match (packet, pipe_recv) {
                    (Some(p), _) => p.recv.clone(),
                    (None, Some(r)) => r,
                    (None, None) => RecvSpec::owner_trig(),
                };
                // `steps` is there: a new seq must have some (check_items)
                let new = NewSeq {
                    steps: steps.unwrap_or_default(),
                    period,
                    explicit_period: checked.period.is_some(),
                    r#gen: target_gen,
                    slots: target_slots,
                    feel: set.feel.values(),
                    seed,
                    recv,
                    packet: packet.map(|p| p.id),
                };
                let cmd = self
                    .make_seq(owner, &name.name, &new, choice, link)
                    .map_err(|e| self.cmd_err(e, o.span))?;
                out.push(cmd);
                if let Some(p) = packet
                    && let Some(&id) = self
                        .engine_state
                        .owner_procs(owner)
                        .and_then(|procs| procs.get(&name.name))
                    && let Some(repr) = self.engine_state.packets.get_mut(&p.id)
                {
                    repr.seqs.push(id);
                }
                true
            }
            Some(repr) => {
                let mut changed = false;
                if let Some(p) = packet
                    && repr.packet != Some(p.id)
                {
                    return Err(wrong_type(
                        o.span,
                        format!("'{}' is a seq outside this packet", name.name),
                    )
                    .into());
                }
                // a restated pipe replaces the receivers (G6)
                if let Some(recv) = pipe_recv
                    && recv != repr.seq.recv
                {
                    let cmd = self.set_recv(repr.id, recv);
                    out.push(cmd);
                    changed = true;
                }
                // a new `on` moves the seq first; the rest applies on the
                // clock it ends up on
                if let Some(on) = checked.on {
                    changed |=
                        self.put_on_clock(TempoTarget::Proc(repr.id), &name.name, on, true, out)?;
                }

                // the steps stay as they were unless a restated list differs
                // (marks and all, but not how the values were written)
                let new_steps = steps.filter(|s| !same_values(s, &repr.seq.steps));
                let period_changed = period != repr.seq.period;
                let explicit_flips = checked.period.is_some() && !repr.seq.explicit_period;
                if period_changed
                    && new_steps.is_none()
                    && let Some(bad) = repr.seq.steps.iter().find(|s| s.at >= period as f32)
                {
                    return Err(LangErr::new(
                        Code::StepOutsideCycle,
                        o.span,
                        format!(
                            "step {} is outside the cycle of {period} (1 to {period})",
                            bad.at + 1.0
                        ),
                    )
                    .into());
                }

                let mut feel = [None; 3];
                for (i, new) in set.feel.values().into_iter().enumerate() {
                    if new != repr.seq.feel[i] {
                        let frames =
                            self.proc_glide_frames(repr.id, set.glide[i], period, o.span)?;
                        feel[i] = Some((new, frames));
                    }
                }

                if new_steps.is_some()
                    || gen_change.is_some()
                    || slots_change.is_some()
                    || period_changed
                    || explicit_flips
                    || seed.is_some()
                    || feel.iter().any(Option::is_some)
                {
                    let cmd = self.edit_seq(
                        repr.id,
                        SeqChange {
                            steps: new_steps,
                            r#gen: gen_change,
                            slots: slots_change,
                            period: period_changed.then_some(period),
                            explicit_period: checked.period.is_some(),
                            seed,
                            feel,
                        },
                    );
                    out.push(cmd);
                    changed = true;
                }
                changed
            }
        };
        if !changed {
            out.warn_unchanged(o.span, &format!("'{}'", name.name));
        }
        Ok(changed)
    }

    /// A glide in frames, by the clock the seq reads right now.
    fn proc_glide_frames(&self, proc: ProcId, g: Glide, period: u32, at: Span) -> Res<u32> {
        if g == Glide::Instant {
            return Ok(0);
        }
        let es = &self.engine_state;
        let (unit, interval) = es
            .procs
            .get(&proc)
            .and_then(|p| es.tempos.get(&p.tempo))
            .map(|t| (t.unit, t.interval))
            .ok_or_else(|| wrong_type(at, "the seq's clock isn't known"))?;
        Ok(glide_frames(g, period, tick_secs(unit, interval)))
    }

    /// Where a seq statement's steps come from: written (`steps`, the
    /// generator dropped), generated by `steps=`/`grid=`/`rot=` (or by the
    /// stored generator, restated with some of them or with a new period),
    /// or not at all (they stay). Returns the steps to put, if any, and the
    /// generator the seq ends up with.
    fn seq_step_source(
        &self,
        checked: &Checked,
        have: Have,
        period: u32,
        feel: Feel,
        creating: bool,
        stmt: Span,
    ) -> Res<StepSource> {
        let Have {
            r#gen: stored,
            slots: stored_slots,
            written,
        } = have;
        let int = |name: &str| -> Res<Option<i64>> {
            match checked.setting(name).map(|s| s.value) {
                Some(Value::Num(n)) => Ok(Some(eval_num(n)? as i64)),
                _ => Ok(None),
            }
        };
        let (hits, grid, rot) = (int("steps")?, int("grid")?, int("rot")?);
        let at = checked.setting("steps").map_or(stmt, |s| s.span);
        let extra = checked.setting("grid").or(checked.setting("rot"));

        if !checked.steps.is_empty() {
            // written steps: grid= and rot= (or the ones the seq has) say
            // what the numbers count and how far they move
            let (grid0, rot0) = stored_slots.map_or((None, 0), |s| (s.grid, s.rot));
            let grid = grid.map(|g| g as u32).or(grid0);
            let rot = rot.map_or(rot0, |r| r as i32);
            let mut steps = lower_steps(&checked.steps, period, grid, feel)?;
            if grid.is_none() && rot == 0 {
                return Ok(StepSource(Some(steps), None, None));
            }
            steps.sort_by(|a, b| a.at.total_cmp(&b.at));
            let sg = SlotGen { grid, rot, steps };
            return Ok(StepSource(
                Some(sg.beats(period, feel.values())),
                None,
                Some(sg),
            ));
        }

        // written steps restated with a new grid, rotation or period: the
        // seq's own numbers (plain steps count as slots of the cycle)
        if hits.is_none() && stored.is_none() {
            let cur = match stored_slots {
                Some(s) => Some(s.clone()),
                None if extra.is_some() && !written.is_empty() => Some(SlotGen {
                    grid: None,
                    rot: 0,
                    steps: written.to_vec(),
                }),
                None => None,
            };
            if let Some(cur) = cur {
                if extra.is_none() && checked.period.is_none() {
                    return Ok(StepSource(None, None, stored_slots.cloned()));
                }
                let sg = SlotGen {
                    grid: grid.map(|g| g as u32).or(cur.grid),
                    rot: rot.map_or(cur.rot, |r| r as i32),
                    steps: cur.steps,
                };
                if let Some(bad) = sg.first_outside(period) {
                    let what = match sg.grid {
                        Some(g) => {
                            format!("step {} is outside the grid of {g} (1 to {g})", bad + 1.0)
                        }
                        None => format!(
                            "step {} is outside the cycle of {period} (1 to {period})",
                            bad + 1.0
                        ),
                    };
                    return Err(LangErr::new(Code::StepOutsideCycle, stmt, what).into());
                }
                let beats = sg.beats(period, feel.values());
                let keep = (sg.grid.is_some() || sg.rot != 0).then_some(sg);
                return Ok(StepSource(Some(beats), None, keep));
            }
        }

        let base = match (hits, stored) {
            (Some(h), _) => Euclid {
                hits: h as u32,
                grid: stored.and_then(|g| g.grid),
                rot: stored.map_or(0, |g| g.rot),
            },
            (None, Some(g)) => g,
            (None, None) => {
                return match extra {
                    Some(s) => Err(wrong_type(
                        s.span,
                        "grid= and rot= go with written steps or steps=",
                    )
                    .into()),
                    // nothing about the steps was said
                    None => Ok(StepSource(None, None, None)),
                };
            }
        };
        let g = Euclid {
            grid: grid.map(|g| g as u32).or(base.grid),
            rot: rot.map_or(base.rot, |r| r as i32),
            ..base
        };
        // a stored generator regenerates only when something it depends on
        // was restated (`in` too, since its default grid is the period)
        let restated = hits.is_some() || extra.is_some() || checked.period.is_some();
        if !restated && !creating {
            return Ok(StepSource(None, stored, None));
        }
        let steps = generated_steps(g, period, feel.values(), at)?;
        Ok(StepSource(Some(steps), Some(g), None))
    }

    /// The seqs of `owner` that follow its period (they never wrote `in`)
    /// move to `period`, each by a `SeqEdit`. A step that would fall outside
    /// the new cycle is E406.
    fn follow_period(&mut self, owner: ProcOwner, period: u32, at: Span, out: &mut Out) -> Res<()> {
        let ids: Vec<ProcId> = self
            .engine_state
            .owner_procs(owner)
            .map(|p| p.values().copied().collect())
            .unwrap_or_default();
        for id in ids {
            let r = &self.engine_state.procs[&id];
            // an env keeps the frames it was written in
            if r.env.is_some() {
                continue;
            }
            // an lfo whose cycle is written in periods follows them
            if let Some(l) = &r.lfo {
                if let LfoRate::Clock { ticks } = l.rate
                    && let Some(n) = lfo::periods_of(&l.freq_text)
                    && ticks != (n * f64::from(period)) as f32
                {
                    let change = LfoChange {
                        freq: Some(((n * f64::from(period)) as f32, 0)),
                        ..LfoChange::default()
                    };
                    let cmd = self.edit_lfo(id, change);
                    out.push(cmd);
                }
                continue;
            }
            if r.seq.explicit_period || r.seq.period == period {
                continue;
            }
            // a generated seq spreads its hits over the new cycle
            if let Some(g) = r.seq.r#gen {
                let steps = generated_steps(g, period, r.seq.feel, at)?;
                let cmd = self.edit_seq(
                    id,
                    SeqChange {
                        steps: Some(steps),
                        period: Some(period),
                        ..SeqChange::default()
                    },
                );
                out.push(cmd);
                continue;
            }
            if let Some(sg) = r.seq.slots.clone() {
                if let Some(bad) = sg.first_outside(period) {
                    return Err(LangErr::new(
                        Code::StepOutsideCycle,
                        at,
                        format!(
                            "seq '{}' has step {}, outside the cycle of {period} (1 to {period})",
                            r.name,
                            bad + 1.0
                        ),
                    )
                    .into());
                }
                let cmd = self.edit_seq(
                    id,
                    SeqChange {
                        steps: Some(sg.beats(period, r.seq.feel)),
                        period: Some(period),
                        ..SeqChange::default()
                    },
                );
                out.push(cmd);
                continue;
            }
            if let Some(bad) = r.seq.steps.iter().find(|s| s.at >= period as f32) {
                return Err(LangErr::new(
                    Code::StepOutsideCycle,
                    at,
                    format!(
                        "seq '{}' has step {}, outside the cycle of {period} (1 to {period})",
                        r.name,
                        bad.at + 1.0
                    ),
                )
                .into());
            }
            let cmd = self.edit_seq(
                id,
                SeqChange {
                    period: Some(period),
                    ..SeqChange::default()
                },
            );
            out.push(cmd);
        }
        Ok(())
    }

    /// The seq-wide feel (`chance=`, `jitter=`, `late=`, `early=`) on top of
    /// `base`, with the glide each asked for.
    fn seq_feel(
        &self,
        checked: &Checked,
        owner: ProcOwner,
        own_tempo: Option<TempoId>,
        base: Feel,
        stmt: Span,
    ) -> Res<FeelSet> {
        let mut feel = base;
        let mut glide = [Glide::Instant; 3];
        if let Some(c) = checked.setting("chance")
            && let Value::Num(n) = c.value
        {
            feel.chance = eval_num(n)?;
            glide[0] = Glide::of(c.over)?;
        }
        let (jitter, late, early) = (
            checked.setting("jitter"),
            checked.setting("late"),
            checked.setting("early"),
        );
        if let Some(j) = jitter
            && let Some(other) = late.or(early)
        {
            return Err(LangErr::new(
                Code::WrongType,
                j.span.to(other.span),
                "jitter, or early/late, not both",
            )
            .into());
        }
        // seconds in one tick of the seq's clock, found only when a time
        // unit needs it
        let tick = || -> Res<f64> {
            let es = &self.engine_state;
            let (unit, interval) = match (checked.on, own_tempo) {
                (Some(on), _) => match self.resolve_on(Some(on), stmt)?.0 {
                    TempoChoice::Own(n) => Some((n.unit, n.interval)),
                    TempoChoice::Shared(t) => es.tempos.get(&t).map(|t| (t.unit, t.interval)),
                },
                (None, Some(t)) => es.tempos.get(&t).map(|t| (t.unit, t.interval)),
                (None, None) => es
                    .owner_tempo(owner)
                    .and_then(|t| es.tempos.get(&t))
                    .map(|t| (t.unit, t.interval)),
            }
            .ok_or_else(|| wrong_type(stmt, "the seq's clock isn't known"))?;
            Ok(tick_secs(unit, interval))
        };
        if let Some(j) = jitter {
            let a = feel_ticks(j.value, &tick)?;
            feel.early = a;
            feel.late = a;
            glide[1] = Glide::of(j.over)?;
            glide[2] = glide[1];
        }
        if let Some(l) = late {
            feel.late = feel_ticks(l.value, &tick)?;
            glide[2] = Glide::of(l.over)?;
        }
        if let Some(e) = early {
            feel.early = feel_ticks(e.value, &tick)?;
            glide[1] = Glide::of(e.over)?;
        }
        Ok(FeelSet { feel, glide })
    }

    /// The settings a voice statement gave, in schema order. `slice` is
    /// 0-based from here on. Decibels only reach here for `gain`.
    fn voice_settings(checked: &Checked) -> Res<Vec<(VoiceSetting, Span)>> {
        let mut given = Vec::new();
        for spec in schema(Kind::Voice).settings {
            let Some(s) = checked.setting(spec.name) else {
                continue;
            };
            let num = |v: &Value| -> Res<f64> {
                match v {
                    // a bare number on pitch is a ratio
                    Value::Num(n) if spec.name == "pitch" => {
                        pitch_semitones(eval_num(n)?, PitchUnit::Ratio).ok_or_else(|| {
                            wrong_type(s.span, "a pitch ratio must be above 0").into()
                        })
                    }
                    Value::Num(n) => Ok(eval_num(n)?),
                    Value::Db(n) => Ok(db_to_linear(eval_num(n)?)),
                    Value::Pitch(n, unit) => pitch_semitones(eval_num(n)?, *unit)
                        .ok_or_else(|| wrong_type(s.span, "a pitch ratio must be above 0").into()),
                    _ => Err(wrong_type(s.span, format!("{} takes a number", spec.name)).into()),
                }
            };
            let word = |v: &Value| match v {
                Value::Word(w) => w.name.clone(),
                _ => String::new(),
            };
            let glide = Glide::of(s.over)?;
            let setting = match spec.name {
                "loop" => VoiceSetting::Mode(VoiceMode::Loop(match word(s.value).as_str() {
                    "forward" => LoopMode::Forward,
                    "pingpong" => LoopMode::PingPong,
                    _ => LoopMode::Off,
                })),
                "shift" => VoiceSetting::Mode(VoiceMode::Shift(match word(s.value).as_str() {
                    "spectral" => ShiftMode::Spectral,
                    _ => ShiftMode::Grains,
                })),
                "resample" => {
                    VoiceSetting::Mode(VoiceMode::Resample(match word(s.value).as_str() {
                        "cubic" => ResampleMode::Cubic,
                        _ => ResampleMode::Iir,
                    }))
                }
                "window" => VoiceSetting::Window(Glide::of_value(s.value, s.span)?),
                "bounce" => VoiceSetting::Bounce(Glide::of_value(s.value, s.span)?),
                "slices" => VoiceSetting::Mode(VoiceMode::Slices(num(s.value)? as u16)),
                "choir" => VoiceSetting::Mode(VoiceMode::Choir(num(s.value)? as u8)),
                "choirmode" => {
                    VoiceSetting::Mode(VoiceMode::ChoirMode(match word(s.value).as_str() {
                        "sim" | "simultaneous" => ChoirMode::Simultaneous,
                        _ => ChoirMode::Sequential,
                    }))
                }
                "slice" => {
                    VoiceSetting::Param(VoiceParam::Slice, (num(s.value)? - 1.0) as f32, glide)
                }
                name => match VoiceParam::from_name(name) {
                    Some(p) => VoiceSetting::Param(p, num(s.value)? as f32, glide),
                    None => continue,
                },
            };
            given.push((setting, s.span));
        }
        Ok(given)
    }

    /// E403 for settings that contradict each other once `given` is applied
    /// over `params` and `slices`.
    fn check_voice_settings(
        given: &[(VoiceSetting, Span)],
        mut params: [f32; VoiceParam::COUNT],
        mut slices: u16,
    ) -> Res<()> {
        let span_of = |want: &dyn Fn(&VoiceSetting) -> bool| {
            given.iter().rev().find(|(s, _)| want(s)).map(|(_, sp)| *sp)
        };
        for (s, _) in given {
            match *s {
                VoiceSetting::Param(p, v, _) => params[p.index()] = v,
                VoiceSetting::Mode(VoiceMode::Slices(n)) => slices = n,
                VoiceSetting::Mode(_) | VoiceSetting::Window(_) | VoiceSetting::Bounce(_) => {}
            }
        }
        let touches = |a: VoiceParam, b: VoiceParam| {
            span_of(&|s| matches!(s, VoiceSetting::Param(p, _, _) if *p == a || *p == b))
        };
        let get = |p: VoiceParam| params[p.index()];
        for (lo, hi) in [
            (VoiceParam::Start, VoiceParam::End),
            (VoiceParam::LoopStart, VoiceParam::LoopEnd),
        ] {
            if let Some(span) = touches(lo, hi)
                && get(lo) >= get(hi)
            {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    span,
                    format!(
                        "{} ({}) must be below {} ({})",
                        lo.name(),
                        get(lo),
                        hi.name(),
                        get(hi)
                    ),
                )
                .into());
            }
        }
        let touched = span_of(&|s| {
            matches!(
                s,
                VoiceSetting::Param(VoiceParam::Slice, _, _)
                    | VoiceSetting::Mode(VoiceMode::Slices(_))
            )
        });
        let slice = get(VoiceParam::Slice) as u16 + 1;
        if let Some(span) = touched
            && slices > 0
            && slice > slices
        {
            return Err(LangErr::new(
                Code::OutOfRange,
                span,
                format!("slice {slice} is above slices={slices}"),
            )
            .into());
        }
        Ok(())
    }

    /// A glide in frames, by the clock the voice is on right now.
    fn voice_glide_frames(&self, voice: VoiceId, g: Glide, at: Span) -> Res<u32> {
        if g == Glide::Instant {
            return Ok(0);
        }
        let es = &self.engine_state;
        let repr = &es.voices[&voice];
        let (unit, interval) = es
            .tempos
            .get(&repr.tempo)
            .map(|t| (t.unit, t.interval))
            .ok_or_else(|| wrong_type(at, "the voice's clock isn't known"))?;
        Ok(glide_frames(g, repr.period, tick_secs(unit, interval)))
    }

    /// Sets a voice's `window=` in the mirror, in frames, clamped to
    /// `WINDOW_RANGE_S` with a warning. Sends nothing: a changed window
    /// makes `send_shifters` rebuild the voice's shifters. True if it changed.
    fn set_voice_window(
        &mut self,
        voice: VoiceId,
        length: Glide,
        at: Span,
        out: &mut Out,
    ) -> Res<bool> {
        let want = self.voice_glide_frames(voice, length, at)? as usize;
        let rate = frame_rate();
        let (lo, hi) = WINDOW_RANGE_S;
        let (lo, hi) = ((lo * rate).round() as usize, (hi * rate).round() as usize);
        let frames = want.clamp(lo, hi);
        if frames != want {
            let ms = |f: usize| (f as f64 * 1e3 / rate).round();
            out.warns.push(LangErr::new(
                Code::Clamped,
                at,
                format!(
                    "window is {}ms: clamped to {}ms (the range is {}ms..{}ms)",
                    ms(want),
                    ms(frames),
                    ms(lo),
                    ms(hi)
                ),
            ));
        }
        let Some(repr) = self.engine_state.voices.get_mut(&voice) else {
            return Ok(false);
        };
        let changed = repr.window != frames;
        repr.window = frames;
        Ok(changed)
    }

    /// Sets a voice's `bounce=` in the mirror, in frames, clamped to
    /// `BOUNCE_RANGE_S` with a warning, and returns the `SetMode` if the
    /// value changed.
    fn set_voice_bounce(
        &mut self,
        voice: VoiceId,
        length: Glide,
        at: Span,
        out: &mut Out,
    ) -> Res<Option<Command>> {
        let want = self.voice_glide_frames(voice, length, at)?;
        let rate = frame_rate();
        let (lo, hi) = BOUNCE_RANGE_S;
        let (lo, hi) = ((lo * rate).round() as u32, (hi * rate).round() as u32);
        let frames = want.clamp(lo, hi);
        if frames != want {
            let ms = |f: u32| (f64::from(f) * 1e3 / rate * 100.0).round() / 100.0;
            out.warns.push(LangErr::new(
                Code::Clamped,
                at,
                format!(
                    "bounce is {}ms: clamped to {}ms (the range is {}ms..{}ms)",
                    ms(want),
                    ms(frames),
                    ms(lo),
                    ms(hi)
                ),
            ));
        }
        let unchanged = self
            .engine_state
            .voices
            .get(&voice)
            .is_none_or(|r| r.bounce == frames);
        Ok((!unchanged).then(|| self.set_voice_mode(voice, VoiceMode::Bounce(frames))))
    }

    // W108 for each setting that only a recorded source reads, given to a
    // voice that plays waves only (the setting is kept and sent as usual)
    fn warn_waves_only(checked: &Checked, voice: &str, out: &mut Out) {
        const RECORDED_ONLY: [&str; 12] = [
            "start",
            "end",
            "loopstart",
            "loopend",
            "slices",
            "slice",
            "stretch",
            "loop",
            "bounce",
            "shift",
            "resample",
            "window",
        ];
        for s in &checked.settings {
            if RECORDED_ONLY.contains(&s.spec.name) {
                out.warns.push(LangErr::new(
                    Code::WavesOnly,
                    s.span,
                    format!("{voice} plays waves only: {} does nothing", s.spec.name),
                ));
            }
        }
    }

    // what a voice statement's `from` names: a quoted track, or a buffer
    // (`waves`: the statement's block holds a wave, which is a source too)
    fn voice_source(&self, checked: &Checked, at: Span, waves: bool) -> Res<VoiceSrc> {
        let es = &self.engine_state;
        if let Some(id) = checked.from_name {
            return match es.buffers.get(&id.name) {
                Some(b) => Ok(VoiceSrc::Buffer(b.id)),
                None => {
                    let mut e = LangErr::new(
                        Code::UnknownName,
                        id.span,
                        format!("unknown buffer '{}'", id.name),
                    )
                    .with_suggestion(&id.name, es.buffers.keys().map(String::as_str));
                    if es.tracks.contains_key(&id.name) {
                        e = e.with_hint(format!("a track is quoted: from \"{}\"", id.name));
                    }
                    Err(e.into())
                }
            };
        }
        let Some((from, from_span)) = checked.from else {
            return match waves {
                true => Ok(VoiceSrc::Waves),
                false => Err(LangErr::new(
                    Code::MissingItem,
                    at,
                    "a voice plays a track, a buffer or waves",
                )
                .with_hint("add from \"track\", or a wave in its block")
                .into()),
            };
        };
        match es.track(from) {
            Ok(t) => Ok(VoiceSrc::Track(t.id)),
            Err(e) => {
                let mut err = self.cmd_err(e, from_span);
                if es.buffers.contains_key(from) {
                    err = err.with_hint(format!("a buffer is not quoted: from {from}"));
                }
                Err(err.into())
            }
        }
    }

    fn lower_voice(
        &mut self,
        o: &Object,
        name: &Ident,
        checked: &Checked,
        existing: Option<VoiceId>,
        out: &mut Out,
    ) -> Res<bool> {
        let given = Self::voice_settings(checked)?;

        let Some(voice) = existing else {
            let has_waves = o.block.as_ref().is_some_and(|b| {
                b.lines.iter().flat_map(|l| l.stmts()).any(
                    |s| matches!(s, Stmt::Object(c) if matches!(c.kind, Some((Kind::Wave, _)))),
                )
            });
            let source = self.voice_source(checked, o.span, has_waves)?;
            if source == VoiceSrc::Waves {
                Self::warn_waves_only(checked, &name.name, out);
            }
            let defaults = VoiceParam::ALL.map(VoiceParam::default_value);
            Self::check_voice_settings(&given, defaults, 0)?;
            let (choice, link) = self.resolve_on(checked.on, o.span)?;
            let period = checked.period.map_or(DEFAULT_PERIOD, |p| p.0);
            let cmd = self
                .load_voice(&name.name, source, choice, link, period)
                .map_err(|e| self.cmd_err(e, o.span))?;
            out.push(cmd);
            if let Some(&id) = self.engine_state.voice_names.get(&name.name) {
                // the engine starts at the defaults, so only the rest is sent
                for (setting, span) in &given {
                    let span = *span;
                    match *setting {
                        VoiceSetting::Param(p, v, g) if v != defaults[p.index()] => {
                            let frames = self.voice_glide_frames(id, g, o.span)?;
                            let cmd = self.set_voice_param(id, p, v, frames);
                            out.push(cmd);
                        }
                        VoiceSetting::Mode(m)
                            if !matches!(
                                m,
                                VoiceMode::Loop(LoopMode::Off)
                                    | VoiceMode::Slices(0)
                                    | VoiceMode::Choir(1)
                                    | VoiceMode::ChoirMode(ChoirMode::Sequential)
                                    | VoiceMode::Shift(ShiftMode::Grains)
                            ) && m != VoiceMode::Resample(ResampleMode::initial()) =>
                        {
                            let cmd = self.set_voice_mode(id, m);
                            out.push(cmd);
                        }
                        VoiceSetting::Window(g) => {
                            self.set_voice_window(id, g, span, out)?;
                        }
                        // sent only when it differs from the default the
                        // engine starts with
                        VoiceSetting::Bounce(g) => {
                            if let Some(cmd) = self.set_voice_bounce(id, g, span, out)? {
                                out.push(cmd);
                            }
                        }
                        _ => {}
                    }
                }
            }
            if let Some(block) = &o.block
                && let Some(&id) = self.engine_state.voice_names.get(&name.name)
            {
                self.lower_voice_block(block, id, &name.name, out)?;
            }
            return Ok(true);
        };

        // an update: change what is built, refuse what isn't, and leave
        // alone what is already as stated
        let repr = self.engine_state.voices[&voice].clone();
        let mut new_source = None;
        if checked.from.is_some() || checked.from_name.is_some() {
            let source = self.voice_source(checked, o.span, false)?;
            if source != repr.source {
                new_source = Some(source);
            }
        }
        if repr.source == VoiceSrc::Waves && new_source.is_none() {
            Self::warn_waves_only(checked, &name.name, out);
        }
        let mut new_period = None;
        if let Some((period, _)) = checked.period
            && period != repr.period
        {
            new_period = Some(period);
        }
        Self::check_voice_settings(&given, repr.params, repr.slices)?;

        let mut changed = false;
        if let Some(on) = checked.on {
            changed |= self.put_on_clock(TempoTarget::Voice(voice), &name.name, on, false, out)?;
        }
        match new_source {
            Some(VoiceSrc::Track(track)) => {
                let cmd = self.set_voice_track(voice, track);
                out.push(cmd);
                changed = true;
            }
            Some(VoiceSrc::Buffer(buffer)) => {
                if let Some(cmd) = self.set_voice_buffer(voice, buffer) {
                    out.push(cmd);
                    changed = true;
                }
            }
            Some(VoiceSrc::Waves | VoiceSrc::Gone) | None => {}
        }
        if let Some(period) = new_period
            && let Some(r) = self.engine_state.voices.get_mut(&voice)
        {
            r.period = period;
            changed = true;
            // the seqs that never wrote `in` follow it
            self.follow_period(ProcOwner::Voice(voice), period, o.span, out)?;
        }
        for (setting, span) in &given {
            let span = *span;
            match *setting {
                VoiceSetting::Window(g) => {
                    changed |= self.set_voice_window(voice, g, span, out)?;
                }
                VoiceSetting::Bounce(g) => {
                    if let Some(cmd) = self.set_voice_bounce(voice, g, span, out)? {
                        out.push(cmd);
                        changed = true;
                    }
                }
                VoiceSetting::Param(p, v, g) if v != repr.params[p.index()] => {
                    let frames = self.voice_glide_frames(voice, g, o.span)?;
                    let cmd = self.set_voice_param(voice, p, v, frames);
                    out.push(cmd);
                    changed = true;
                }
                VoiceSetting::Mode(m) => {
                    let same = match m {
                        VoiceMode::Loop(l) => l == repr.loop_mode,
                        VoiceMode::Slices(n) => n == repr.slices,
                        VoiceMode::Choir(n) => n == repr.choir,
                        VoiceMode::ChoirMode(m) => m == repr.choir_mode,
                        VoiceMode::Shift(m) => m == repr.shift,
                        VoiceMode::Resample(m) => m == repr.resample,
                        VoiceMode::Bounce(n) => n == repr.bounce,
                    };
                    if !same {
                        let cmd = self.set_voice_mode(voice, m);
                        out.push(cmd);
                        changed = true;
                    }
                }
                VoiceSetting::Param(..) => {}
            }
        }
        // the seqs in the block say for themselves if they changed anything
        if let Some(block) = &o.block {
            changed |= self.lower_voice_block(block, voice, &name.name, out)?;
        }
        Ok(changed)
    }

    fn lower_group(
        &mut self,
        o: &Object,
        name: &Ident,
        checked: &Checked,
        existing: Option<GroupId>,
        out: &mut Out,
    ) -> Res<bool> {
        let gain = match checked.setting("gain") {
            Some(s) => Some((Self::group_gain_value(s)?, Glide::of(s.over)?, s.span)),
            None => None,
        };

        let mut members: Vec<VoiceId> = Vec::new();
        // seqs written in a new group's block wait until the group exists
        let mut pending: Vec<Object> = Vec::new();
        let mut changed = false;
        if let Some(block) = &o.block {
            for line in &block.lines {
                for stmt in line.stmts() {
                    let Stmt::Object(child) = stmt else {
                        return Err(wrong_type(
                            line.span,
                            "a group's block holds members, processes and effects",
                        )
                        .into());
                    };
                    changed |=
                        self.lower_group_child(child, existing, &mut members, &mut pending, out)?;
                }
            }
        }

        let Some(group) = existing else {
            let (choice, link) = self.resolve_on(checked.on, o.span)?;
            let period = checked.period.map_or(DEFAULT_PERIOD, |p| p.0);
            let cmd = self
                .make_group(&name.name, choice, link, &members, period)
                .map_err(|e| self.cmd_err(e, o.span))?;
            out.push(cmd);
            if let Some((value, _, _)) = gain
                && value != 1.0
                && let Some(&id) = self.engine_state.group_names.get(&name.name)
            {
                // a new group isn't playing, so the value lands at once
                let cmd = self.set_group_gain(id, value, 0);
                out.push(cmd);
            }
            if let Some(&id) = self.engine_state.group_names.get(&name.name) {
                for seq in &pending {
                    let owner = Some(ProcOwner::Group(id));
                    match seq.kind {
                        Some((Kind::Fx(_), _)) => {
                            self.lower_fx_stmt(seq, Some(FxOwner::Group(id)), out)?
                        }
                        Some((Kind::Lfo, _)) => self.lower_lfo_stmt(seq, owner, out)?,
                        Some((Kind::Env, _)) => self.lower_env_stmt(seq, owner, out)?,
                        Some((Kind::Packet, _)) => self.lower_packet_stmt(seq, owner, out)?,
                        _ => self.lower_seq_stmt(seq, owner, out)?,
                    };
                }
            }
            return Ok(true);
        };

        // an update: the block adds and moves members (G4); the period is
        // the group's own, which its members don't read
        if let Some((period, _)) = checked.period
            && let Some(r) = self.engine_state.groups.get_mut(&group)
            && period != r.period
        {
            r.period = period;
            changed = true;
            // the seqs that never wrote `in` follow it
            self.follow_period(ProcOwner::Group(group), period, o.span, out)?;
        }
        if let Some(on) = checked.on {
            changed |= self.put_on_clock(TempoTarget::Group(group), &name.name, on, false, out)?;
        }
        if let Some((value, glide, span)) = gain
            && value != self.engine_state.groups[&group].gain
        {
            let frames = self.group_glide_frames(group, glide, span)?;
            let cmd = self.set_group_gain(group, value, frames);
            out.push(cmd);
            changed = true;
        }
        Ok(changed)
    }

    // the linear value of a group's `gain=` (decibels convert)
    fn group_gain_value(s: &CheckedSetting) -> Res<f32> {
        Ok(match s.value {
            Value::Num(n) => eval_num(n)? as f32,
            Value::Db(n) => db_to_linear(eval_num(n)?) as f32,
            _ => return Err(wrong_type(s.span, "gain takes a number").into()),
        })
    }

    /// A glide in frames, by the clock the group is on right now.
    fn group_glide_frames(&self, group: GroupId, g: Glide, at: Span) -> Res<u32> {
        if g == Glide::Instant {
            return Ok(0);
        }
        let es = &self.engine_state;
        let repr = &es.groups[&group];
        let (unit, interval) = es
            .tempos
            .get(&repr.tempo)
            .map(|t| (t.unit, t.interval))
            .ok_or_else(|| wrong_type(at, "the group's clock isn't known"))?;
        Ok(glide_frames(g, repr.period, tick_secs(unit, interval)))
    }

    /// One statement of a group's block: a member, a voice declaration, or a
    /// process. Voices to put in a new group are collected in `members`.
    /// True if it changed the group.
    fn lower_group_child(
        &mut self,
        c: &Object,
        group: Option<GroupId>,
        members: &mut Vec<VoiceId>,
        pending: &mut Vec<Object>,
        out: &mut Out,
    ) -> Res<bool> {
        let Target::Path(path) = &c.target else {
            return Err(wrong_type(c.span, "a group's block holds voices, not tracks").into());
        };
        let Some(name) = path.last() else {
            return Err(wrong_type(c.span, "a statement needs a name").into());
        };
        match c.kind {
            None => {
                if !c.items.is_empty() || c.block.is_some() || c.pipe.is_some() {
                    return Err(wrong_type(
                        c.span,
                        "in a block a bare name is membership; change an object outside the block",
                    )
                    .into());
                }
                match lookup(&self.engine_state, path)? {
                    Named::Voice(v) => self.add_member(v, name, group, members, out),
                    other => Err(wrong_type(
                        name.span,
                        format!(
                            "'{}' is {}; only voices can be members",
                            name.name,
                            article(other.kind())
                        ),
                    )
                    .into()),
                }
            }
            Some((Kind::Voice, _)) => {
                let before = self.engine_state.voices.len();
                // a declaration is an ordinary voice statement
                self.lower_object(c, out)?;
                let voice = match lookup(&self.engine_state, path)? {
                    Named::Voice(v) => v,
                    _ => return Err(wrong_type(c.span, "expected a voice").into()),
                };
                debug_assert!(self.engine_state.voices.len() >= before);
                // the declaration says for itself if it changed anything
                self.add_member(voice, name, group, members, out)
            }
            Some((Kind::Fx(_), _)) => {
                match group {
                    Some(g) => {
                        self.lower_fx_stmt(c, Some(FxOwner::Group(g)), out)?;
                    }
                    None => pending.push(c.clone()),
                }
                Ok(true)
            }
            Some((Kind::Lfo, _)) => {
                match group {
                    Some(g) => {
                        self.lower_lfo_stmt(c, Some(ProcOwner::Group(g)), out)?;
                    }
                    None => pending.push(c.clone()),
                }
                Ok(true)
            }
            Some((Kind::Env, _)) => {
                match group {
                    Some(g) => {
                        self.lower_env_stmt(c, Some(ProcOwner::Group(g)), out)?;
                    }
                    None => pending.push(c.clone()),
                }
                Ok(true)
            }
            Some((kind @ (Kind::Seq | Kind::Packet), _)) => {
                match group {
                    // the group is there: the seq says for itself if it changed
                    Some(g) => {
                        let owner = Some(ProcOwner::Group(g));
                        if kind == Kind::Packet {
                            self.lower_packet_stmt(c, owner, out)?;
                        } else {
                            self.lower_seq_stmt(c, owner, out)?;
                        }
                    }
                    // the group is made first
                    None => pending.push(c.clone()),
                }
                Ok(true)
            }
            Some((k, span)) => Err(match process_task(k) {
                Some(task) => not_built(span, &format!("'{}'", kind_word(k)), task),
                None => wrong_type(
                    span,
                    format!("a group's block holds voices, not {}", article(k)),
                ),
            }
            .into()),
        }
    }

    // Puts a voice in the group the statement is about: into an existing one
    // now, or into the list a new one is made from. A voice in another group
    // leaves it first (a move). True if anything changed.
    fn add_member(
        &mut self,
        voice: VoiceId,
        name: &Ident,
        group: Option<GroupId>,
        members: &mut Vec<VoiceId>,
        out: &mut Out,
    ) -> Res<bool> {
        let in_group = self.engine_state.voices.get(&voice).and_then(|v| v.group);
        if in_group.is_some() && in_group == group {
            return Ok(false);
        }
        if in_group.is_some() {
            let cmd = self.leave_group(voice, true);
            out.push(cmd);
        }
        match group {
            Some(target) => {
                let cmd = self
                    .join_group(voice, target)
                    .map_err(|e| self.cmd_err(e, name.span))?;
                out.push(cmd);
            }
            None => {
                if !members.contains(&voice) {
                    members.push(voice);
                }
            }
        }
        Ok(true)
    }

    // ---- actions ----

    fn lower_action(&mut self, a: &Action, out: &mut Out) -> Res<()> {
        let (word, word_span) = a.word;
        match word {
            ActionWord::Rescan => return self.lower_rescan(word_span, out),
            ActionWord::Ungroup => return self.lower_ungroup(&a.targets, out),
            _ => {}
        }
        let list = match &a.targets {
            ActionTargets::All(_) if word == ActionWord::Remove => {
                self.remove_all(out);
                return Ok(());
            }
            ActionTargets::All(_) => None,
            ActionTargets::List(list) => Some(list),
        };

        if word == ActionWord::Remove {
            for t in list.map(Vec::as_slice).unwrap_or_default() {
                let cmd = self.remove_target(t)?;
                out.push(cmd);
            }
            return Ok(());
        }

        // seq targets (`pause kick.beat`) become process commands
        let mut proc_cmds: Vec<Command> = Vec::new();
        let targets = match list {
            None => self.every_target(),
            Some(list) => {
                let mut targets = Vec::with_capacity(list.len());
                for t in list {
                    match t {
                        Target::Track(_, span) => {
                            return Err(
                                wrong_type(*span, "a track can't be started or stopped").into()
                            );
                        }
                        Target::Path(path) => {
                            let named = match lookup(&self.engine_state, path)? {
                                Named::Clock(t) if word == ActionWord::Start => {
                                    // a clock starts everything on it
                                    for e in self.clock_and_followers(t) {
                                        if !targets.contains(&e) {
                                            targets.push(e);
                                        }
                                    }
                                    continue;
                                }
                                Named::Clock(t) => Eng::Tempo(t),
                                Named::Voice(v) => Eng::Voice(v),
                                Named::Group(g) => Eng::Group(g),
                                Named::Buffer(b) => {
                                    self.buffer_action(word, b, path_span(path), out)?;
                                    continue;
                                }
                                Named::Seq(p) | Named::Lfo(p) | Named::Env(p) => {
                                    for &op in seq_ops(word) {
                                        proc_cmds.push(self.proc_op(p, op));
                                    }
                                    continue;
                                }
                                Named::Fx { owner, id, .. } => {
                                    let at = path_span(path);
                                    proc_cmds.extend(self.fx_action(word, owner, id, at, out));
                                    continue;
                                }
                                Named::Wave { voice, id } => {
                                    let at = path_span(path);
                                    proc_cmds.extend(self.wave_action(word, voice, id, at, out));
                                    continue;
                                }
                                // a packet acts on its seqs
                                Named::Packet(pk) => {
                                    let seqs = self.engine_state.packets[&pk].seqs.clone();
                                    for p in seqs {
                                        for &op in seq_ops(word) {
                                            proc_cmds.push(self.proc_op(p, op));
                                        }
                                    }
                                    continue;
                                }
                            };
                            targets.push(named);
                        }
                    }
                }
                targets
            }
        };
        // `start all` and `stop all` bring every paused seq back
        if list.is_none() && matches!(word, ActionWord::Start | ActionWord::Stop) {
            let paused: Vec<ProcId> = self
                .engine_state
                .procs
                .values()
                .filter(|p| !p.enabled)
                .map(|p| p.id)
                .collect();
            for p in paused {
                proc_cmds.push(self.proc_op(p, ProcOp::Enable));
            }
        }
        if word == ActionWord::Start {
            self.note_started(&targets);
        }
        let cmd = self.transport(word, &targets);
        out.push(cmd);
        for cmd in proc_cmds {
            out.push(cmd);
        }
        Ok(())
    }

    /// `ungroup`: a group dissolves, a member returns to the top level.
    fn lower_ungroup(&mut self, targets: &ActionTargets, out: &mut Out) -> Res<()> {
        let list = match targets {
            ActionTargets::All(_) => {
                let es = &self.engine_state;
                let groups: Vec<GroupId> = es
                    .groups
                    .keys()
                    .copied()
                    .filter(|g| !(es.staging && es.in_limbo(AnyId::Group(*g))))
                    .collect();
                for g in groups {
                    let cmd = self.dissolve_group(g);
                    out.push(cmd);
                }
                return Ok(());
            }
            ActionTargets::List(list) => list,
        };
        for t in list {
            let path = match t {
                Target::Track(_, span) => {
                    return Err(wrong_type(*span, "a track isn't in a group").into());
                }
                Target::Path(path) => path,
            };
            let span = path
                .first()
                .zip(path.last())
                .map_or_else(Span::default, |(a, b)| a.span.to(b.span));
            let cmd = match lookup(&self.engine_state, path)? {
                Named::Group(g) => self.dissolve_group(g),
                Named::Voice(v) if self.engine_state.voices[&v].group.is_some() => {
                    self.leave_group(v, false)
                }
                Named::Voice(_) => {
                    return Err(wrong_type(span, "that voice isn't in a group").into());
                }
                other => {
                    return Err(wrong_type(
                        span,
                        format!(
                            "ungroup takes a group or a member, not {}",
                            article(other.kind())
                        ),
                    )
                    .into());
                }
            };
            out.push(cmd);
        }
        Ok(())
    }

    /// `remove all`: every group (with its members), every top-level voice,
    /// then every clock but `main`. Tracks stay. What a waiting line will
    /// make isn't there to remove.
    fn remove_all(&mut self, out: &mut Out) {
        let es = &self.engine_state;
        let there = |id: AnyId| es.staging || !es.is_pending(id);
        // the session's own seqs first: nothing they name is gone yet
        let seqs: Vec<ProcId> = self
            .engine_state
            .session_procs
            .values()
            .copied()
            .filter(|p| there(AnyId::Proc(*p)) && !es.hidden(AnyId::Proc(*p)))
            .collect();
        for p in seqs {
            let cmd = self.remove_proc(p);
            out.push(cmd);
        }
        if !self.engine_state.staging {
            self.engine_state
                .packets
                .retain(|_, p| p.owner != ProcOwner::Session);
        }
        // buffers before what they record, so none is warned about
        let buffers: Vec<String> = self.engine_state.buffers.keys().cloned().collect();
        for b in buffers {
            for cmd in self.remove_buffer(&b) {
                out.push(cmd);
            }
        }
        let es = &self.engine_state;
        let groups: Vec<GroupId> = es
            .groups
            .keys()
            .copied()
            .filter(|g| !es.hides_group(*g))
            .collect();
        for g in groups {
            let cmd = self.remove_group_unchecked(g);
            out.push(cmd);
        }
        let es = &self.engine_state;
        let voices: Vec<VoiceId> = es
            .voices
            .values()
            .filter(|v| es.sees_voice(v))
            .map(|v| v.id)
            .filter(|v| es.staging || !es.is_pending(AnyId::Voice(*v)))
            .collect();
        for v in voices {
            if let Ok(cmd) = self.engine_state.remove_voice(v) {
                out.push(cmd);
            }
        }
        let main = self.engine_state.main_id();
        let es = &self.engine_state;
        let mut clocks: Vec<TempoId> = es
            .context_names
            .values()
            .copied()
            .filter(|t| es.staging || !es.is_pending(AnyId::Tempo(*t)))
            .filter(|t| !es.hidden(AnyId::Tempo(*t)))
            .collect();
        clocks.sort_unstable();
        for t in clocks {
            if Some(t) != main
                && let Ok(cmd) = self.remove_clock(t)
            {
                out.push(cmd);
            }
        }
    }

    /// A clock, then the top-level voices and groups that follow it, each in
    /// id order. Grouped voices start through their group.
    fn clock_and_followers(&self, t: TempoId) -> Vec<Eng> {
        let es: &EngineState = &self.engine_state;
        let mut targets = vec![Eng::Tempo(t)];
        targets.extend(
            es.voices
                .values()
                .filter(|v| v.group.is_none() && v.tempo == t)
                .map(|v| Eng::Voice(v.id)),
        );
        targets.extend(
            es.groups
                .iter()
                .filter(|(_, g)| g.tempo == t)
                .map(|(&id, _)| Eng::Group(id)),
        );
        targets
    }

    /// What `all` stands for: every clock, group and top-level voice.
    fn every_target(&self) -> Vec<Eng> {
        let es: &EngineState = &self.engine_state;
        // what a waiting line will make isn't there for a line typed now
        let there = |id: AnyId| es.staging || !es.is_pending(id);
        let mut targets: Vec<Eng> = Vec::new();
        let mut clocks: Vec<TempoId> = es
            .context_names
            .values()
            .copied()
            .filter(|t| there(AnyId::Tempo(*t)) && !es.hidden(AnyId::Tempo(*t)))
            .collect();
        clocks.sort_unstable();
        targets.extend(clocks.into_iter().map(Eng::Tempo));
        targets.extend(
            es.groups
                .keys()
                .filter(|g| !es.hides_group(**g))
                .map(|&g| Eng::Group(g)),
        );
        targets.extend(
            es.voices
                .values()
                .filter(|v| v.group.is_none() && es.sees_voice(v) && there(AnyId::Voice(v.id)))
                .map(|v| Eng::Voice(v.id)),
        );
        targets
    }

    fn remove_target(&mut self, t: &Target) -> Res<Command> {
        let path = match t {
            Target::Track(name, span) => return self.remove_track(name, *span),
            Target::Path(path) => path,
        };
        let span = path
            .first()
            .zip(path.last())
            .map_or_else(Span::default, |(a, b)| a.span.to(b.span));
        match lookup(&self.engine_state, path)? {
            Named::Clock(t) if Some(t) == self.engine_state.main_id() => Err(LangErr::new(
                Code::MainNotRemovable,
                span,
                "main is the session's clock and can't be removed",
            )
            .into()),
            Named::Clock(t) => self.remove_clock(t).map_err(|users| {
                let list = describe_users(&self.engine_state, &users);
                in_use(span, "clock", path, &list)
            }),
            Named::Group(g) => self.remove_group(g).map_err(|users| {
                let list = describe_users(&self.engine_state, &users);
                in_use(span, "group", path, &list)
            }),
            Named::Buffer(b) => Ok(self.remove_buffer_target(b)),
            Named::Seq(p) | Named::Lfo(p) | Named::Env(p) => Ok(self.remove_proc(p)),
            Named::Fx { owner, id, .. } => {
                // an lfo that modulated it goes first, with a warning
                let mut cmds = self.drop_lfos_of_fx(owner, id);
                cmds.extend(self.drop_envs_of_fx(owner, id));
                cmds.push(self.remove_fx(owner, id));
                Ok(Command::Batch(BatchArgs { cmds }))
            }
            Named::Wave { voice, id } => {
                // a voice with no track or buffer needs a wave to be a voice
                // (a wave a waiting line will make doesn't count: that line may
                // be dropped)
                let es = &self.engine_state;
                let real = es
                    .wave_list(voice)
                    .iter()
                    .filter(|w| !es.is_pending(AnyId::Wave { voice, id: w.id }))
                    .count();
                if es
                    .voices
                    .get(&voice)
                    .is_some_and(|r| r.source == VoiceSrc::Waves)
                    && real <= 1
                    // taking out a wave that is itself pending never lowers
                    // the real count
                    && !es.is_pending(AnyId::Wave { voice, id })
                {
                    let owner = path
                        .iter()
                        .rev()
                        .nth(1)
                        .map_or("the voice", |i| i.name.as_str());
                    return Err(LangErr::new(
                        Code::InUse,
                        span,
                        format!("{owner}'s last wave can't go: nothing would be left to play"),
                    )
                    .with_hint(format!("remove {owner} instead, or add another wave first"))
                    .into());
                }
                // a waiting line that sets an `fm=` naming it would find it
                // gone: it stays until that line has run
                if let Some(line) = es.waiting_fm_line(AnyId::Wave { voice, id }) {
                    let owner = path
                        .iter()
                        .rev()
                        .nth(1)
                        .map_or("the voice", |i| i.name.as_str());
                    let wave = path.last().map_or("it", |i| i.name.as_str());
                    return Err(LangErr::new(
                        Code::InUse,
                        span,
                        format!(
                            "{owner}.{wave} is named by a waiting fm= line ({})",
                            line.text
                        ),
                    )
                    .with_hint("wait for it, or remove the carrier first")
                    .into());
                }
                // an lfo or env that modulated it goes first, with a warning
                let mut cmds = self.drop_lfos_of_wave(voice, id);
                cmds.extend(self.drop_envs_of_wave(voice, id));
                cmds.push(self.remove_wave(voice, id));
                Ok(Command::Batch(BatchArgs { cmds }))
            }
            Named::Packet(pk) => {
                // its seqs go (each leaves the packet), then the packet
                let seqs = self.engine_state.packets[&pk].seqs.clone();
                let seqs_left = seqs.clone();
                let mut cmds = Vec::new();
                for p in seqs {
                    match self.remove_proc(p) {
                        Command::Batch(args) => cmds.extend(args.cmds),
                        cmd => cmds.push(cmd),
                    }
                }
                if self.engine_state.staging && !seqs_left.is_empty() {
                    // the packet goes with the last of its seqs
                    for l in &mut self.engine_state.limbo {
                        if matches!(l.id, AnyId::Proc(p) if seqs_left.contains(&p)) {
                            l.packet = Some(pk);
                        }
                    }
                } else {
                    self.engine_state.packets.remove(&pk);
                }
                Ok(Command::Batch(BatchArgs { cmds }))
            }
            Named::Voice(v) => {
                // a voice's own seqs go with it; anything else that follows
                // its clock would be left without one
                let followers: Vec<_> = voice_followers(&self.engine_state, v)
                    .into_iter()
                    .filter(|u| {
                        !matches!(u, TempoUser::Proc(p)
                            if self.engine_state.procs.get(p).is_some_and(|r| r.owner == ProcOwner::Voice(v)))
                    })
                    .collect();
                if !followers.is_empty() {
                    let list = describe_users(&self.engine_state, &followers);
                    return Err(in_use(span, "voice", path, &list));
                }
                Ok(self
                    .engine_state
                    .remove_voice(v)
                    .map_err(|e| self.cmd_err(e, span))?)
            }
        }
    }
}

/// What a seq inside a packet takes from it.
struct PacketCtx {
    id: PacketId,
    /// The packet's receivers, which its new seqs start with.
    recv: Vec<RecvSpec>,
}

/// E502: a seq or packet outside an owner has no owner to trigger.
fn nothing_to_trigger(span: Span) -> Errs {
    LangErr::new(
        Code::NothingToTrigger,
        span,
        "nothing to trigger: outside a voice or group, a seq needs a pipe that names one",
    )
    .with_hint("seq x 1 3 | trig kick")
    .into()
}

/// The actions' effect on a seq: start and stop also go back to step 1.
fn seq_ops(word: ActionWord) -> &'static [ProcOp] {
    match word {
        ActionWord::Start => &[ProcOp::Enable, ProcOp::Reset],
        ActionWord::Stop => &[ProcOp::Disable, ProcOp::Reset],
        ActionWord::Pause => &[ProcOp::Disable],
        _ => &[ProcOp::Enable],
    }
}

fn item_span(i: &Item) -> Span {
    match i {
        Item::From(_, s) => *s,
        Item::FromName(id) => id.span,
        Item::On(c) => clock_ref_span(c),
        Item::In(n) => n.span(),
        Item::Setting(id, ..) => id.span,
        Item::Step(s) => s.span,
        Item::Clock(c) => c.span,
    }
}

fn path_span(path: &[Ident]) -> Span {
    path.first()
        .zip(path.last())
        .map_or_else(Span::default, |(a, b)| a.span.to(b.span))
}

impl CmdProcessor {
    /// The receivers of a pipe, checked against the mirror. A bare `trig`
    /// (or `melo`) means the owner.
    fn lower_pipe(&self, receivers: &[Receiver], owner: ProcOwner) -> Res<Vec<RecvSpec>> {
        let es = &self.engine_state;
        let mut out = Vec::with_capacity(receivers.len());
        for r in receivers {
            let kind_span = r.kind.1;
            let target = match &r.target {
                Some(path) => {
                    let name = path.last().map_or("", |i| i.name.as_str());
                    Some((lookup(es, path)?, path_span(path), name))
                }
                None => None,
            };
            match r.kind.0 {
                ReceiverKind::Trig => {
                    if let Some(item) = r.items.first() {
                        return Err(wrong_type(
                            item_span(item),
                            "trig takes a target, not notes (melo plays notes)",
                        )
                        .into());
                    }
                    out.push(RecvSpec::Trig(match target {
                        None if owner == ProcOwner::Session => {
                            return Err(nothing_to_trigger(kind_span));
                        }
                        None => TrigTarget::Owner,
                        Some((Named::Voice(v), ..)) => TrigTarget::Voice(v),
                        Some((Named::Group(g), ..)) => TrigTarget::Group(g),
                        Some((other, span, name)) => {
                            return Err(LangErr::new(
                                Code::BadReceiver,
                                span,
                                format!(
                                    "'{name}' is {}; trig takes a voice or a group",
                                    article(other.kind())
                                ),
                            )
                            .into());
                        }
                    }));
                }
                ReceiverKind::Melo => {
                    let voice = match target {
                        None => match owner {
                            ProcOwner::Voice(v) => v,
                            ProcOwner::Session => return Err(nothing_to_trigger(kind_span)),
                            ProcOwner::Group(_) => {
                                return Err(LangErr::new(
                                    Code::BadReceiver,
                                    kind_span,
                                    "melo needs a voice to play: melo NAME 36 43",
                                )
                                .into());
                            }
                        },
                        Some((Named::Voice(v), ..)) => v,
                        Some((other, span, name)) => {
                            return Err(LangErr::new(
                                Code::BadReceiver,
                                span,
                                format!(
                                    "'{name}' is {}; melo takes a voice",
                                    article(other.kind())
                                ),
                            )
                            .into());
                        }
                    };
                    out.push(RecvSpec::melo(voice, melo_notes(&r.items, kind_span)?));
                }
            }
        }
        Ok(out)
    }

    /// `packet NAME [on X] [in N] { seq … } [| receivers]`, in a block
    /// (`owner` is the voice or group) or by a path (`kick.fills`, or a
    /// top-level name). Restating changes the pipe, adds or edits the seqs
    /// in the block, and applies `on` and `in` to the seqs. True if it
    /// changed something.
    fn lower_packet_stmt(
        &mut self,
        o: &Object,
        owner: Option<ProcOwner>,
        out: &mut Out,
    ) -> Res<bool> {
        let Target::Path(path) = &o.target else {
            return Err(wrong_type(o.span, "a packet is named by a word, not a track").into());
        };
        let Some((name, owner_path)) = path.split_last() else {
            return Err(wrong_type(o.span, "a statement needs a name").into());
        };
        let owner = match owner {
            Some(p) if owner_path.is_empty() => p,
            Some(_) => {
                return Err(wrong_type(
                    path_span(path),
                    "in a block a packet is named by one word",
                )
                .into());
            }
            None if owner_path.is_empty() => ProcOwner::Session,
            None => match lookup(&self.engine_state, owner_path)? {
                Named::Voice(v) => ProcOwner::Voice(v),
                Named::Group(g) => ProcOwner::Group(g),
                other => {
                    return Err(wrong_type(
                        path_span(path),
                        format!("{} can't hold a packet", article(other.kind())),
                    )
                    .into());
                }
            },
        };

        let existing = self.engine_state.packet_of(owner, &name.name);
        let creating = existing.is_none();
        if creating {
            if name.name == RESERVED_ALL {
                return Err(LangErr::new(
                    Code::ReservedWord,
                    name.span,
                    format!("'{}' is reserved", name.name),
                )
                .into());
            }
            let taken = match owner {
                ProcOwner::Session => check_new(&self.engine_state, name).err(),
                _ => self
                    .engine_state
                    .proc_named(owner, &name.name)
                    .is_some()
                    .then(|| {
                        LangErr::new(
                            Code::KindClash,
                            name.span,
                            format!("'{}' is already a seq", name.name),
                        )
                    })
                    .or_else(|| {
                        self.engine_state
                            .effect_or_wave(owner, &name.name)
                            .map(|what| {
                                LangErr::new(
                                    Code::KindClash,
                                    name.span,
                                    format!("'{}' is already {what}", name.name),
                                )
                            })
                    }),
            };
            if let Some(e) = taken {
                return Err(e.into());
            }
        }
        check_items(schema(Kind::Packet), &name.name, &o.items, creating, o.span)?;
        // what `on` and `in` default its seqs to (the syntax, to hand down)
        let defaults: Vec<Item> = o
            .items
            .iter()
            .filter(|i| matches!(i, Item::On(_) | Item::In(_)))
            .cloned()
            .collect();

        let mut lines: Vec<&Object> = Vec::new();
        if let Some(block) = &o.block {
            for line in &block.lines {
                for stmt in line.stmts() {
                    match stmt {
                        Stmt::Object(c) if matches!(c.kind, Some((Kind::Seq, _))) => {
                            lines.push(c);
                        }
                        _ => {
                            return Err(wrong_type(line.span, "a packet's block holds seqs").into());
                        }
                    }
                }
            }
        }
        let pipe_recv = match &o.pipe {
            Some(rs) => Some(self.lower_pipe(rs, owner)?),
            None => None,
        };

        let mut changed = false;
        let id = match existing {
            Some(id) => id,
            None => {
                if lines.is_empty() {
                    return Err(LangErr::new(
                        Code::MissingItem,
                        o.span,
                        format!("packet '{}' needs a block of seqs", name.name),
                    )
                    .into());
                }
                let recv = match pipe_recv.clone() {
                    Some(r) => r,
                    None if owner == ProcOwner::Session => {
                        return Err(nothing_to_trigger(o.span));
                    }
                    None => RecvSpec::owner_trig(),
                };
                changed = true;
                self.make_packet(owner, &name.name, recv)
            }
        };
        if !creating
            && let Some(recv) = pipe_recv
            && self.engine_state.packets[&id].recv != recv
        {
            let seqs = self.engine_state.packets[&id].seqs.clone();
            for p in seqs {
                let cmd = self.set_recv(p, recv.clone());
                out.push(cmd);
            }
            if let Some(repr) = self.engine_state.packets.get_mut(&id) {
                repr.recv = recv;
            }
            changed = true;
        }

        let ctx = PacketCtx {
            id,
            recv: self.engine_state.packets[&id].recv.clone(),
        };
        // a seq written without its own `on` or `in` takes the packet's
        let with_defaults = |c: &Object| {
            let mut c = c.clone();
            for d in &defaults {
                let has = c.items.iter().any(|i| {
                    matches!(
                        (i, d),
                        (Item::On(_), Item::On(_)) | (Item::In(_), Item::In(_))
                    )
                });
                if !has {
                    c.items.push(d.clone());
                }
            }
            c
        };
        for c in &lines {
            self.lower_seq_in(&with_defaults(c), Some(owner), Some(&ctx), out)?;
            changed = true;
        }
        // `on` or `in` alone restates every seq with it
        if lines.is_empty() && !creating && !defaults.is_empty() {
            let seqs = self.engine_state.packets[&id].seqs.clone();
            for p in seqs {
                let Some(seq_name) = self.engine_state.procs.get(&p).map(|r| r.name.clone()) else {
                    continue;
                };
                let restate = Object {
                    kind: Some((Kind::Seq, o.span)),
                    target: Target::Path(vec![Ident {
                        name: seq_name,
                        span: name.span,
                    }]),
                    items: defaults.clone(),
                    block: None,
                    pipe: None,
                    span: o.span,
                };
                changed |= self.lower_seq_in(&restate, Some(owner), Some(&ctx), out)?;
            }
        }
        if !changed && lines.is_empty() {
            out.warn_unchanged(o.span, &format!("'{}'", name.name));
        }
        Ok(changed)
    }
}

/// The notes of a `melo`: plain numbers from 0 to 127 (MIDI; 60 is no
/// shift).
fn melo_notes(items: &[Item], at: Span) -> Res<Vec<f32>> {
    if items.is_empty() {
        return Err(
            LangErr::new(Code::MissingItem, at, "melo needs notes: melo bass 36 43").into(),
        );
    }
    let mut notes = Vec::with_capacity(items.len());
    for item in items {
        let Item::Step(s) = item else {
            return Err(wrong_type(item_span(item), "melo takes plain note numbers").into());
        };
        if s.to.is_some() || s.chance.is_some() || s.jitter.is_some() {
            return Err(wrong_type(s.span, "a note is a plain number").into());
        }
        let n = eval_num(&s.at)?;
        if !(0.0..=127.0).contains(&n) {
            return Err(LangErr::new(
                Code::OutOfRange,
                s.span,
                format!("a note is from 0 to 127, not {n}"),
            )
            .into());
        }
        notes.push(n as f32);
    }
    Ok(notes)
}
