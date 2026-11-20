//! `state` (REQ-056 §7): the session printed in the language itself, so that
//! feeding it back changes nothing (D2). It prints what the mirror holds:
//! clocks, imported tracks, groups with their members, voices, and the voices' seqs. Defaults
//! and relations that follow the parent are left out; steps print 1-based.
//! Wave 5 adds what it builds (voice settings, receivers) to this file.

use super::ast::{Ident, PitchUnit};
use super::errors::{LangErr, Span};
use super::names::{Named, fx_language_kind, lookup};
use super::schema::{db_to_linear, kind_word, pitch_semitones};
use crate::audio_processing::blast_time::blast_time::TempoUnit;
use crate::audio_processing::commands::procs::env::curve_word;
use crate::audio_processing::commands::reprs::{DEFAULT_PERIOD, EngineState, MAIN};
use crate::audio_processing::commands::track::engine_rate;
use crate::audio_processing::commands::{
    AnyId, BufferRepr, Bypass, CmdErr, CmdProcessor, CmdResult, DEFAULT_TABLE_FRAMES,
    DEFAULT_TABLE_SLICES, FxOwner, FxRepr, GroupId, LenSpec, PacketRepr, ProcId, ProcOwner,
    RecvSpec, ResampleMode, SeqStep, ShiftMode, TempoId, TempoLink, TrigTarget, VoiceId, VoiceRepr,
    VoiceSrc, WaveFrom, WaveRepr,
};
use crate::audio_processing::engine::default_bounce;
use crate::audio_processing::engine::fx::{defaults, own_settings, spec};
use crate::audio_processing::engine::shift::default_window;
use crate::audio_processing::engine::{BufSource, ChoirMode, LoopMode, VoiceParam};
use crate::audio_processing::engine::{WaveParam, WaveShape, wave_defaults};
use crate::audio_processing::processes::{EnvCurve, LfoShape};

/// The default rate of `main` (G2); `state` omits it.
const MAIN_BPM: f32 = 120.0;

fn clock_text(unit: TempoUnit, interval: f32) -> String {
    let letter = match unit {
        TempoUnit::Bpm => "b",
        TempoUnit::Millis => "ms",
        TempoUnit::Samples => "f",
    };
    format!("{interval}{letter}")
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A step position as written (1-based): the shortest text that reads back
/// as the stored 0-based position.
fn position(at: f32) -> String {
    let short = format!("{}", at + 1.0);
    let reads_back = short.parse::<f64>().is_ok_and(|p| ((p - 1.0) as f32) == at);
    if reads_back {
        short
    } else {
        format!("{}", f64::from(at) + 1.0)
    }
}

/// A step's timing feel: how early and how late it may land (ticks).
type Feel = (f32, f32);

/// The seq's base feel, printed as settings on the seq line. A one-sided
/// feel (early differs from late) wins, since only settings can make one;
/// otherwise the feel most steps share, ties going to the earliest step.
fn base_feel(steps: &[SeqStep]) -> Feel {
    if let Some(s) = steps.iter().find(|s| s.early != s.late) {
        return (s.early, s.late);
    }
    let mut best: Option<(usize, Feel)> = None;
    for s in steps {
        let f = (s.early, s.late);
        let n = steps.iter().filter(|o| (o.early, o.late) == f).count();
        if best.is_none_or(|(m, _)| n > m) {
            best = Some((n, f));
        }
    }
    best.map_or((0.0, 0.0), |(_, f)| f)
}

/// The seq-wide settings that reproduce `steps`' chance and base feel.
fn seq_settings_text(steps: &[SeqStep], base: Feel) -> String {
    let mut out = String::new();
    if let Some(c) = shared_chance(steps) {
        out.push_str(&format!(" chance={c}"));
    }
    let (early, late) = base;
    if early == late {
        if early != 0.0 {
            out.push_str(&format!(" jitter={early}"));
        }
    } else {
        if early != 0.0 {
            out.push_str(&format!(" early={early}"));
        }
        if late != 0.0 {
            out.push_str(&format!(" late={late}"));
        }
    }
    out
}

/// The chance every step has, when it is not 100 and there are two or more
/// steps (then it is written once as `chance=`).
fn shared_chance(steps: &[SeqStep]) -> Option<f32> {
    let first = steps.first()?.chance;
    (steps.len() >= 2 && first != 100.0 && steps.iter().all(|s| s.chance == first)).then_some(first)
}

fn step_text(s: &SeqStep, base: Feel, chance_is_shared: bool) -> String {
    let mut out = position(s.at);
    if s.chance != 100.0 && !chance_is_shared {
        out.push_str(&format!("%{}", s.chance));
    }
    if (s.early, s.late) != base {
        // a step's `~a` is symmetric
        out.push_str(&format!("~{}", s.early.max(s.late)));
    }
    out
}

/// A pitch (kept in semitones) as it is written back: a ratio when that
/// reads back as the same pitch (`2`, `0.5`, `1.25`), else semitones (`7st`).
fn pitch_text(semitones: f32) -> String {
    let ratio = (f64::from(semitones) / 12.0).exp2();
    let rounded = (ratio * 1e4).round() / 1e4;
    match pitch_semitones(rounded, PitchUnit::Ratio) {
        Some(st) if st as f32 == semitones => format!("{rounded}"),
        _ => format!("{semitones}st"),
    }
}

/// A depth as it is written back: on pitch always in semitones (`0.5st`), so
/// it doesn't read as a ratio; bare elsewhere.
fn depth_text(target: &str, depth: f32) -> String {
    match target {
        "pitch" => format!("{depth}st"),
        _ => depth.to_string(),
    }
}

/// A gain (or a filter's `base`) as the shorter of its linear and decibel
/// spellings, taking decibels only when they read back as the same value:
/// `gain=-6db` prints `-6db`, not `0.5011872`, and `gain=0.5` stays `0.5`.
fn gain_text(g: f32) -> String {
    let linear = g.to_string();
    if g > 0.0 {
        let db = (20.0 * f64::from(g).log10() * 100.0).round() / 100.0;
        let text = format!("{db}db");
        if db_to_linear(db) as f32 == g && text.len() < linear.len() {
            return text;
        }
    }
    linear
}

/// ` name=value` for each setting that isn't at its default, in schema
/// order (gain in decibels when that reads better; `slice` counts from 1).
fn voice_settings_text(r: &VoiceRepr) -> String {
    let param = |p: VoiceParam| r.params[p.index()];
    let mut out = String::new();
    let mut put = |name: &str, value: String| out.push_str(&format!(" {name}={value}"));
    for p in [
        VoiceParam::Gain,
        VoiceParam::Pan,
        VoiceParam::Rate,
        VoiceParam::Pitch,
        VoiceParam::Stretch,
        VoiceParam::Start,
        VoiceParam::End,
    ] {
        if param(p) != p.default_value() {
            let text = match p {
                VoiceParam::Pitch => pitch_text(param(p)),
                VoiceParam::Gain => gain_text(param(p)),
                _ => param(p).to_string(),
            };
            put(p.name(), text);
        }
    }
    if r.loop_mode != LoopMode::Off {
        let word = match r.loop_mode {
            LoopMode::PingPong => "pingpong",
            _ => "forward",
        };
        put("loop", word.into());
    }
    for p in [VoiceParam::LoopStart, VoiceParam::LoopEnd] {
        if param(p) != p.default_value() {
            put(p.name(), param(p).to_string());
        }
    }
    if r.slices != 0 {
        put("slices", r.slices.to_string());
    }
    if param(VoiceParam::Slice) != 0.0 {
        put("slice", (param(VoiceParam::Slice) + 1.0).to_string());
    }
    if r.choir != 1 {
        put("choir", r.choir.to_string());
    }
    if r.choir_mode != ChoirMode::Sequential {
        put("choirmode", "sim".into());
    }
    if param(VoiceParam::Intonation) != VoiceParam::Intonation.default_value() {
        put("intonation", param(VoiceParam::Intonation).to_string());
    }
    if r.shift != ShiftMode::Grains {
        put("shift", "spectral".into());
    }
    if r.resample != ResampleMode::initial() {
        let word = match r.resample {
            ResampleMode::Cubic => "cubic",
            ResampleMode::Iir => "iir",
        };
        put("resample", word.into());
    }
    if r.window != default_window(engine_rate() as f32) {
        // whole milliseconds read best; anything else is exact in frames
        let rate = u64::from(engine_rate());
        put(
            "window",
            match (r.window as u64 * 1000).is_multiple_of(rate) {
                true => format!("{}ms", r.window as u64 * 1000 / rate),
                false => format!("{}f", r.window),
            },
        );
    }
    if r.bounce != default_bounce(engine_rate() as f32) {
        let rate = u64::from(engine_rate());
        put(
            "bounce",
            match (u64::from(r.bounce) * 1000).is_multiple_of(rate) {
                true => format!("{}ms", u64::from(r.bounce) * 1000 / rate),
                false => format!("{}f", r.bounce),
            },
        );
    }
    out
}

/// `filter lo type=hp cutoff=800`: an effect with the settings that differ
/// from the defaults, in schema order (gain and base through `gain_text`).
fn fx_line(f: &FxRepr) -> String {
    let mut out = format!("{} {}", kind_word(fx_language_kind(f.kind)), f.name);
    let defaults = defaults(f.kind.ty);
    if f.kind.variant != 0
        && let Some(word) = spec(f.kind.ty).types.get(usize::from(f.kind.variant))
    {
        out.push_str(&format!(" type={word}"));
    }
    for (i, s) in own_settings(f.kind.ty).iter().enumerate() {
        if f.params[i] != defaults[i] {
            let value = match s.db {
                true => gain_text(f.params[i]),
                false => f.params[i].to_string(),
            };
            out.push_str(&format!(" {}={value}", s.name));
        }
    }
    out
}

/// Whether a table wave's source was removed: no script can say it.
fn table_gone(w: &WaveRepr) -> bool {
    w.shape == WaveShape::Table && w.from == Some(WaveFrom::Gone)
}

impl EngineState {
    /// `wave s1 shape=saw freq=110`: a wave with the settings that differ
    /// from the defaults, in schema order (a table wave says where it was cut
    /// from, and into how many cycles of how many samples).
    fn wave_line(&self, r: &VoiceRepr, w: &WaveRepr) -> String {
        // the modulator is named only if a script can write it
        let names: Vec<&str> = self
            .writable_waves(r)
            .filter(|x| w.fm >> x.id & 1 == 1)
            .map(|m| m.name.as_str())
            .collect();
        let fm = (!names.is_empty()).then(|| names.join("+"));
        let mut out = wave_line(w, fm.as_deref());
        if w.shape != WaveShape::Table {
            return out;
        }
        let from = match w.from {
            Some(WaveFrom::Track(t)) => self.track_name(t).map(quoted),
            Some(WaveFrom::Buffer(b)) => self.buffer_by_id(b).map(|b| b.name.clone()),
            Some(WaveFrom::Gone) | None => None,
        };
        let mut table = String::new();
        if let Some(from) = from {
            table.push_str(&format!(" from {from}"));
        }
        if w.slices != DEFAULT_TABLE_SLICES {
            table.push_str(&format!(" slices={}", w.slices));
        }
        if w.frames != DEFAULT_TABLE_FRAMES {
            table.push_str(&format!(" frames={}", w.frames));
        }
        // right after `shape=table`
        let at = " shape=table";
        match out.find(at) {
            Some(i) => out.insert_str(i + at.len(), &table),
            None => out.push_str(&table),
        }
        out
    }

    /// The waves of voice `r` a script can write, in evaluation order:
    /// a modulator before the waves it modulates (otherwise as they were
    /// made), so that what is printed replays.
    fn writable_waves<'a>(&self, r: &'a VoiceRepr) -> std::vec::IntoIter<&'a WaveRepr> {
        let mut left: Vec<&WaveRepr> = r.waves.iter().filter(|w| !table_gone(w)).collect();
        let mut ordered: Vec<&WaveRepr> = Vec::with_capacity(left.len());
        while !left.is_empty() {
            // a wave is ready when what it follows is placed (or not written)
            let ready = left.iter().position(|w| {
                (0..8u8).all(|m| {
                    w.fm >> m & 1 == 0
                        || ordered.iter().any(|o| o.id == m)
                        || !left.iter().any(|o| o.id == m)
                })
            });
            // (the mirror has no loops; one would be printed as it stands)
            ordered.push(left.remove(ready.unwrap_or(0)));
        }
        ordered.into_iter()
    }

    /// True for a voice of waves only that has none a script can write: it
    /// prints as a comment, and so does everything that belongs to it.
    fn voice_hidden(&self, v: VoiceId) -> bool {
        self.voices
            .get(&v)
            .is_some_and(|r| r.source == VoiceSrc::Waves && self.writable_waves(r).next().is_none())
    }

    // the comment for one table wave whose source was removed
    fn table_note(voice: &str, w: &WaveRepr) -> String {
        format!(
            "# wave {voice}.{} was cut from a track or buffer that was removed",
            w.name
        )
    }

    // the target (as written) of an lfo or env of a voice when it is a wave
    // a script can't write
    fn hidden_target<'a>(
        &self,
        r: &'a crate::audio_processing::commands::ProcRepr,
    ) -> Option<&'a str> {
        let ProcOwner::Voice(v) = r.owner else {
            return None;
        };
        let (target, text) = match (&r.lfo, &r.env) {
            (Some(l), _) => (l.target, l.target_text.as_str()),
            (None, Some(e)) => (e.target, e.target_text.as_str()),
            (None, None) => return None,
        };
        let crate::audio_processing::processes::LfoTarget::Wave { id, .. } = target else {
            return None;
        };
        let voice = self.voices.get(&v)?;
        let w = voice.waves.iter().find(|w| w.id == id)?;
        (table_gone(w) || self.voice_hidden(v)).then_some(text)
    }

    /// What a voice that prints as a comment took with it: one comment
    /// naming its seqs, lfos, envs and effects (none if it had none).
    fn hidden_voice_notes(&self, v: VoiceId) -> Vec<String> {
        let Some(r) = self.voices.get(&v) else {
            return Vec::new();
        };
        let mut names: Vec<String> = r
            .procs
            .iter()
            .map(|(n, p)| {
                let kind = match self.procs.get(p) {
                    Some(pr) if pr.lfo.is_some() => "lfo",
                    Some(pr) if pr.env.is_some() => "env",
                    _ => "seq",
                };
                format!("{kind} {n}")
            })
            .collect();
        names.extend(r.fx.iter().map(|f| format!("effect {}", f.name)));
        match names.is_empty() {
            true => Vec::new(),
            false => vec![format!(
                "# {} is a comment, so these are dropped with it: {}",
                r.name,
                names.join(", ")
            )],
        }
    }

    /// A comment for each table wave of voice `v` whose source was removed
    /// (a voice with nothing else to say has its own comment).
    fn table_notes(&self, v: VoiceId) -> Vec<String> {
        let Some(r) = self.voices.get(&v) else {
            return Vec::new();
        };
        if r.source == VoiceSrc::Waves && self.writable_waves(r).next().is_none() {
            return Vec::new();
        }
        r.waves
            .iter()
            .filter(|w| table_gone(w))
            .map(|w| Self::table_note(&r.name, w))
            .collect()
    }
}

// the part of a wave line that doesn't need the mirror (`fm`: the name of its
// modulator, if it has one a script can write)
fn wave_line(w: &WaveRepr, fm: Option<&str>) -> String {
    let mut out = format!("wave {}", w.name);
    if w.shape != WaveShape::Sine {
        out.push_str(&format!(" shape={}", w.shape.name()));
    }
    let defaults = wave_defaults();
    for p in [
        WaveParam::Freq,
        WaveParam::Gain,
        WaveParam::Phase,
        WaveParam::Pw,
        WaveParam::Pos,
        WaveParam::Ratio,
        WaveParam::Index,
        WaveParam::Fb,
    ] {
        let v = w.params[p.index()];
        // a position means something to a table only; in ratio mode the
        // frequency is the ratio, which is always said (it is the mode)
        let shown = match p {
            WaveParam::Pos => v != defaults[p.index()] && w.shape == WaveShape::Table,
            WaveParam::Freq => v != defaults[p.index()] && !w.ratio_mode,
            WaveParam::Ratio => w.ratio_mode,
            _ => v != defaults[p.index()],
        };
        if shown {
            let text = match p {
                WaveParam::Gain => gain_text(v),
                WaveParam::Ratio if w.ratio_text.is_some() => {
                    w.ratio_text.clone().unwrap_or_default()
                }
                _ => v.to_string(),
            };
            out.push_str(&format!(" {}={text}", p.name()));
        }
        // the modulator comes between the settings that precede `index`
        if p == WaveParam::Ratio
            && let Some(m) = fm
        {
            out.push_str(&format!(" fm={m}"));
        }
    }
    if !w.retrig {
        out.push_str(" retrig=no");
    }
    if !w.out {
        out.push_str(" out=no");
    }
    out
}

impl EngineState {
    /// `stop kick.lo` / `pause kick.lo` for each bypassed effect or wave of `v`.
    fn fx_bypass_lines(&self, v: VoiceId) -> Vec<String> {
        let Some(name) = self.display_name(v) else {
            return Vec::new();
        };
        if self.voice_hidden(v) {
            return Vec::new();
        }
        let mut lines = Self::bypass_lines(&name, &self.voices[&v].fx);
        for w in &self.voices[&v].waves {
            let word = match w.bypass {
                Some(Bypass::Stopped) => "stop",
                Some(Bypass::Paused) => "pause",
                None => continue,
            };
            lines.push(match table_gone(w) {
                true => format!("# {word} {name}.{} was dropped with the wave", w.name),
                false => format!("{word} {name}.{}", w.name),
            });
        }
        lines
    }

    // the same for a list of effects under `name`
    fn bypass_lines(name: &str, fx: &[FxRepr]) -> Vec<String> {
        fx.iter()
            .filter_map(|f| {
                let word = match f.bypass? {
                    Bypass::Stopped => "stop",
                    Bypass::Paused => "pause",
                };
                Some(format!("{word} {name}.{}", f.name))
            })
            .collect()
    }

    /// `lfo wob to=pan depth=0.3 freq=2hz shape=tri`: an lfo with the
    /// settings that differ from the defaults.
    fn lfo_line(&self, p: ProcId) -> Option<String> {
        // what a waiting line will make isn't there yet
        if self.is_pending(AnyId::Proc(p)) {
            return None;
        }
        let r = self.procs.get(&p)?;
        let l = r.lfo.as_ref()?;
        let mut out = format!(
            "lfo {} to={} depth={}",
            r.name,
            l.target_text,
            depth_text(&l.target_text, l.depth)
        );
        if !l.freq_text.is_empty() {
            out.push_str(&format!(" freq={}", l.freq_text));
        }
        if l.shape != LfoShape::Sine {
            let word = match l.shape {
                LfoShape::Tri => "tri",
                LfoShape::Square => "square",
                LfoShape::Saw => "saw",
                LfoShape::SampleHold => "sh",
                LfoShape::Sine => "sine",
            };
            out.push_str(&format!(" shape={word}"));
        }
        if l.offset != 0.0 {
            out.push_str(&format!(" offset={}", l.offset));
        }
        if l.phase != 0.0 {
            out.push_str(&format!(" phase={}", l.phase));
        }
        if let Some(seed) = l.seed {
            out.push_str(&format!(" seed={seed}"));
        }
        Some(out)
    }

    /// `env amp attack=5ms decay=200ms sustain=0 to=lo.cutoff depth=3`: an
    /// env with the settings that differ from the defaults.
    fn env_line(&self, p: ProcId) -> Option<String> {
        // what a waiting line will make isn't there yet
        if self.is_pending(AnyId::Proc(p)) {
            return None;
        }
        let r = self.procs.get(&p)?;
        let e = r.env.as_ref()?;
        let mut out = format!("env {}", r.name);
        for (name, text) in [("attack", &e.text.attack), ("decay", &e.text.decay)] {
            if !text.is_empty() {
                out.push_str(&format!(" {name}={text}"));
            }
        }
        if e.shape.sustain != 1.0 {
            out.push_str(&format!(" sustain={}", e.shape.sustain));
        }
        if !e.text.hold.is_empty() && e.text.hold != "retrigger" {
            out.push_str(&format!(" hold={}", e.text.hold));
        }
        if !e.text.release.is_empty() {
            out.push_str(&format!(" release={}", e.text.release));
        }
        if e.target_text != "gain" {
            out.push_str(&format!(" to={}", e.target_text));
        }
        if e.depth != 1.0 {
            out.push_str(&format!(" depth={}", depth_text(&e.target_text, e.depth)));
        }
        if e.shape.curve != EnvCurve::Exp {
            out.push_str(&format!(" curve={}", curve_word(e.shape.curve)));
        }
        Some(out)
    }

    /// The lfos and envs `owner` holds, one line each, after its effects
    /// (they may target them).
    fn lfo_lines(&self, owner: ProcOwner) -> Vec<String> {
        let Some(procs) = self.owner_procs(owner) else {
            return Vec::new();
        };
        procs
            .values()
            .filter_map(|&p| {
                let line = self.lfo_line(p).or_else(|| self.env_line(p))?;
                let r = self.procs.get(&p)?;
                Some(match self.hidden_target(r) {
                    Some(to) => format!(
                        "# {} was dropped: it targeted {to}, which has no table",
                        r.name
                    ),
                    None => line,
                })
            })
            .collect()
    }

    /// `stop kick.wob` / `pause kick.wob` for each lfo of `owner` that is.
    fn lfo_bypass_lines(&self, owner: ProcOwner, owner_name: &str) -> Vec<String> {
        let Some(procs) = self.owner_procs(owner) else {
            return Vec::new();
        };
        procs
            .values()
            .filter_map(|p| self.procs.get(p))
            .filter_map(|r| {
                let (paused, stopped) = match (&r.lfo, &r.env) {
                    (Some(l), _) => (l.paused, l.stopped),
                    (None, Some(e)) => (e.paused, e.stopped),
                    (None, None) => return None,
                };
                let word = if stopped {
                    "stop"
                } else if paused {
                    "pause"
                } else {
                    return None;
                };
                if self.hidden_target(r).is_some() {
                    return Some(format!(
                        "# {word} {owner_name}.{} was dropped with it",
                        r.name
                    ));
                }
                Some(format!("{word} {owner_name}.{}", r.name))
            })
            .collect()
    }

    // the same for the lfos of voice `v`
    fn lfo_bypass_lines_of(&self, v: VoiceId) -> Vec<String> {
        match self.display_name(v) {
            Some(_) if self.voice_hidden(v) => Vec::new(),
            Some(name) => self.lfo_bypass_lines(ProcOwner::Voice(v), &name),
            None => Vec::new(),
        }
    }

    fn clock_name(&self, tempo: TempoId) -> Option<&str> {
        self.context_names
            .iter()
            .find(|(_, t)| **t == tempo)
            .map(|(n, _)| n.as_str())
    }

    fn track_name(&self, id: crate::audio_processing::commands::TrackId) -> Option<&str> {
        self.tracks
            .iter()
            .find(|(_, t)| t.id == id)
            .map(|(n, _)| n.as_str())
    }

    /// ` on …` for an object with this clock link, or nothing when it just
    /// follows its parent. `parent` is the group a voice is a member of.
    fn on_text(&self, link: TempoLink, tempo: TempoId, parent: Option<GroupId>) -> String {
        match link {
            TempoLink::Parent | TempoLink::Voice => String::new(),
            TempoLink::Group(g) if Some(g) == parent => String::new(),
            TempoLink::Group(g) => self
                .groups
                .get(&g)
                .map(|g| format!(" on {}", g.name))
                .unwrap_or_default(),
            TempoLink::Follow(v) => self
                .voices
                .get(&v)
                .map(|v| format!(" on {}", v.name))
                .unwrap_or_default(),
            TempoLink::Context => self
                .clock_name(tempo)
                .map(|n| format!(" on {n}"))
                .unwrap_or_default(),
            TempoLink::Own => self
                .tempos
                .get(&tempo)
                .map(|t| format!(" on {}", clock_text(t.unit, t.interval)))
                .unwrap_or_default(),
        }
    }

    fn voice_text(&self, v: VoiceId) -> Option<String> {
        let r = self.voices.get(&v)?;
        let mut out = format!("voice {}", r.name);
        match r.source {
            VoiceSrc::Track(t) => {
                if let Some(t) = self.track_name(t) {
                    out.push_str(&format!(" from {}", quoted(t)));
                }
            }
            VoiceSrc::Buffer(b) => {
                if let Some(b) = self.buffer_by_id(b) {
                    out.push_str(&format!(" from {}", b.name));
                }
            }
            // a voice of waves says no source: its waves are its block (it
            // always has one: the last can't be removed)
            VoiceSrc::Waves if self.writable_waves(r).next().is_none() => {
                return Some(format!(
                    "# voice {} plays a table cut from a track or buffer that was removed",
                    r.name
                ));
            }
            VoiceSrc::Waves => {}
            // its buffer was removed: nothing a script could say
            VoiceSrc::Gone => {
                return Some(format!(
                    "# voice {} played a buffer that was removed",
                    r.name
                ));
            }
        }
        out.push_str(&self.on_text(r.link, r.tempo, r.group));
        if r.period != DEFAULT_PERIOD {
            out.push_str(&format!(" in {}", r.period));
        }
        out.push_str(&voice_settings_text(r));
        if r.source == VoiceSrc::Waves {
            let waves: Vec<String> = self
                .writable_waves(r)
                .map(|w| self.wave_line(r, w))
                .collect();
            out.push_str(&format!(" {{ {} }}", waves.join("; ")));
        }
        Some(out)
    }

    fn group_text(&self, g: GroupId) -> Option<String> {
        let r = self.groups.get(&g)?;
        let mut out = format!("group {}", r.name);
        out.push_str(&self.on_text(r.link, r.tempo, None));
        if r.period != DEFAULT_PERIOD {
            out.push_str(&format!(" in {}", r.period));
        }
        if r.gain != 1.0 {
            out.push_str(&format!(" gain={}", gain_text(r.gain)));
        }
        // the group's own seqs follow its members in the block; the ones
        // with a pipe of their own come after every voice (`owner_extras`),
        // since the pipe may name a voice that isn't printed yet
        let seqs: Vec<String> = r
            .procs
            .values()
            .filter(|&&p| self.plain_seq(p))
            .filter_map(|&p| self.seq_text(p, None))
            .collect();
        let mut fx: Vec<String> = r.fx.iter().map(fx_line).collect();
        // after the effects: an lfo may target one
        fx.extend(self.lfo_lines(ProcOwner::Group(g)));
        if !r.members.is_empty() || !seqs.is_empty() || !fx.is_empty() {
            out.push_str(" {\n");
            for &m in &r.members {
                if let Some(line) = self.voice_text(m) {
                    out.push_str(&format!("  {line}\n"));
                }
            }
            for line in seqs.iter().chain(&fx) {
                out.push_str(&format!("  {line}\n"));
            }
            out.push('}');
        }
        Some(out)
    }

    /// True for a seq with the pipe it has when none is written (it
    /// triggers its owner, G6) and no packet: it prints inside its owner.
    fn plain_seq(&self, p: ProcId) -> bool {
        self.procs
            .get(&p)
            .is_some_and(|r| r.packet.is_none() && r.seq.recv == RecvSpec::owner_trig())
    }

    /// ` | trig hat, melo bass 36 43`.
    fn pipe_text(&self, recv: &[RecvSpec]) -> String {
        let voice = |v: &VoiceId| {
            self.voices
                .get(v)
                .map_or_else(String::new, |v| v.name.clone())
        };
        let parts: Vec<String> = recv
            .iter()
            .map(|r| match r {
                RecvSpec::Trig(TrigTarget::Owner) => "trig".to_string(),
                RecvSpec::Trig(TrigTarget::Voice(v)) => format!("trig {}", voice(v)),
                RecvSpec::Trig(TrigTarget::Group(g)) => format!(
                    "trig {}",
                    self.groups.get(g).map_or("", |g| g.name.as_str())
                ),
                RecvSpec::Melo { target, notes, .. } => {
                    let notes: Vec<String> = notes.iter().map(|n| n.to_string()).collect();
                    format!("melo {} {}", voice(target), notes.join(" "))
                }
            })
            .collect();
        if parts.is_empty() {
            return String::new();
        }
        format!(" | {}", parts.join(", "))
    }

    /// `seq name steps [in N] [on X] [| pipe]`. A seq without a pipe
    /// triggers its owner (G6); one in a packet has the packet's. `owner` is
    /// the path that names the owner, for a seq printed outside its block
    /// (`seq drums.hat …`).
    fn seq_text(&self, p: ProcId, owner: Option<&str>) -> Option<String> {
        // what a waiting line will make isn't there yet
        if self.is_pending(AnyId::Proc(p)) {
            return None;
        }
        let r = self.procs.get(&p)?;
        if r.lfo.is_some() || r.env.is_some() {
            return None;
        }
        // the owner's period, and the group whose clock it reads by default
        let (owner_period, parent) = match r.owner {
            ProcOwner::Voice(v) => {
                let owner = self.voices.get(&v)?;
                (owner.period, owner.group)
            }
            ProcOwner::Group(g) => (self.groups.get(&g)?.period, Some(g)),
            ProcOwner::Session => (DEFAULT_PERIOD, None),
        };
        let mut out = match owner {
            Some(o) => format!("seq {o}.{}", r.name),
            None => format!("seq {}", r.name),
        };
        // written steps with a grid or rotation print as written (slots)
        let written: Vec<SeqStep> = match &r.seq.slots {
            Some(sg) => sg.slot_steps(r.seq.feel),
            None => r.seq.steps.clone(),
        };
        let base = base_feel(&written);
        let shared = shared_chance(&written).is_some();
        if let Some(g) = r.seq.r#gen {
            // the generator is printed, not the steps; the seq-wide values
            // are the generated steps' own
            out.push_str(&format!(" steps={}", g.hits));
            if let Some(grid) = g.grid {
                out.push_str(&format!(" grid={grid}"));
            }
            if g.rot != 0 {
                out.push_str(&format!(" rot={}", g.rot));
            }
        } else {
            for s in &written {
                out.push(' ');
                out.push_str(&step_text(s, base, shared));
            }
            if let Some(sg) = &r.seq.slots {
                if let Some(grid) = sg.grid {
                    out.push_str(&format!(" grid={grid}"));
                }
                if sg.rot != 0 {
                    out.push_str(&format!(" rot={}", sg.rot));
                }
            }
        }
        if r.seq.r#gen.is_some() {
            let [chance, early, late] = r.seq.feel;
            let step = SeqStep {
                at: 0.0,
                chance,
                early,
                late,
                own_chance: false,
                own_jit: false,
            };
            out.push_str(&seq_settings_text(&[step, step], (early, late)));
        } else {
            out.push_str(&seq_settings_text(&written, base));
        }
        if let Some(seed) = r.seq.seed {
            out.push_str(&format!(" seed={seed}"));
        }
        if r.seq.period != owner_period {
            out.push_str(&format!(" in {}", r.seq.period));
        }
        out.push_str(&self.on_text(r.link, r.tempo, parent));
        if r.packet.is_none() && r.seq.recv != RecvSpec::owner_trig() {
            out.push_str(&self.pipe_text(&r.seq.recv));
        }
        Some(out)
    }

    /// `packet name { seq … } | pipe`, as lines; `owner` names the owner
    /// for a packet printed outside its block (`packet drums.fills`).
    fn packet_lines(&self, pk: &PacketRepr, owner: Option<&str>) -> Vec<String> {
        let name = match owner {
            Some(o) => format!("{o}.{}", pk.name),
            None => pk.name.clone(),
        };
        let mut lines = vec![format!("packet {name} {{")];
        for &p in &pk.seqs {
            lines.extend(self.seq_text(p, None).map(|l| format!("  {l}")));
        }
        let default = pk.owner != ProcOwner::Session && pk.recv == RecvSpec::owner_trig();
        let pipe = if default {
            String::new()
        } else {
            self.pipe_text(&pk.recv)
        };
        lines.push(format!("}}{pipe}"));
        lines
    }

    /// What an owner's block holds: its seqs, then its packets, each line
    /// at the owner's indent.
    fn block_lines(&self, owner: ProcOwner) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(procs) = self.owner_procs(owner) {
            for &p in procs.values() {
                let in_packet = self.procs.get(&p).is_some_and(|r| r.packet.is_some());
                if !in_packet {
                    lines.extend(self.seq_text(p, None));
                }
            }
        }
        for pk in self.owner_packets(owner) {
            lines.extend(self.packet_lines(pk, None));
        }
        // the effects run in the order they are written
        if let ProcOwner::Voice(v) = owner
            && let Some(r) = self.voices.get(&v)
        {
            lines.extend(r.fx.iter().map(fx_line));
            // a voice of waves only writes them in its own statement
            if r.source != VoiceSrc::Waves {
                lines.extend(self.writable_waves(r).map(|w| self.wave_line(r, w)));
            }
            lines.extend(self.lfo_lines(owner));
        }
        lines
    }

    /// The seqs of voice `v`: `kick { seq … }`, or a block when several.
    fn seqs_text(&self, v: VoiceId) -> Option<String> {
        let owner = self.voices.get(&v)?;
        if self.voice_hidden(v) {
            let notes = self.hidden_voice_notes(v);
            return (!notes.is_empty()).then(|| notes.join("\n"));
        }
        let lines = self.block_lines(ProcOwner::Voice(v));
        match lines.as_slice() {
            [] => None,
            [one] if !one.contains('\n') && !one.ends_with('{') && !one.starts_with('#') => {
                Some(format!("{} {{ {one} }}", owner.name))
            }
            many => {
                let mut out = format!("{} {{\n", owner.name);
                for line in many {
                    out.push_str(&format!("  {line}\n"));
                }
                out.push('}');
                Some(out)
            }
        }
    }

    /// A group's seqs with a pipe of their own, and its packets, written
    /// by path after every voice exists.
    fn group_extras(&self, g: GroupId) -> Vec<String> {
        let Some(group) = self.groups.get(&g) else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        lines.extend(Self::bypass_lines(&group.name, &group.fx));
        lines.extend(self.lfo_bypass_lines(ProcOwner::Group(g), &group.name));
        for &p in group.procs.values() {
            let in_packet = self.procs.get(&p).is_some_and(|r| r.packet.is_some());
            if !in_packet && !self.plain_seq(p) {
                lines.extend(self.seq_text(p, Some(&group.name)));
            }
        }
        for pk in self.owner_packets(ProcOwner::Group(g)) {
            lines.push(self.packet_lines(pk, Some(&group.name)).join("\n"));
        }
        lines
    }

    /// The seqs and packets of the session itself.
    fn session_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for &p in self.session_procs.values() {
            if self.procs.get(&p).is_some_and(|r| r.packet.is_none()) {
                lines.extend(self.seq_text(p, None));
            }
        }
        for pk in self.owner_packets(ProcOwner::Session) {
            lines.push(self.packet_lines(pk, None).join("\n"));
        }
        lines
    }

    /// `track "bass" from "path"` for each track a statement imported, with
    /// its raw settings. Tracks from the assets folder aren't printed: they
    /// come back with the folder.
    fn track_lines(&self) -> Vec<String> {
        let mut tracks: Vec<_> = self
            .tracks
            .iter()
            .filter_map(|(name, t)| t.source.as_ref().map(|s| (name, s)))
            .collect();
        tracks.sort_by(|a, b| a.0.cmp(b.0));
        tracks
            .into_iter()
            .map(|(name, src)| {
                let mut line = format!("track {} from {}", quoted(name), quoted(&src.written));
                if let Some(f) = &src.format {
                    line.push_str(&format!(" format={f}"));
                }
                if let Some(r) = src.rate {
                    line.push_str(&format!(" rate={r}"));
                }
                if let Some(c) = src.channels {
                    line.push_str(&format!(" channels={c}"));
                }
                line
            })
            .collect()
    }

    fn clock_line(&self, t: TempoId, name: &str) -> Option<String> {
        let r = self.tempos.get(&t)?;
        Some(format!("clock {name} {}", clock_text(r.unit, r.interval)))
    }

    /// Voices in the order `state` prints them: each group's members in
    /// order, then the voices outside groups.
    fn voices_in_order(&self) -> Vec<VoiceId> {
        let mut out: Vec<VoiceId> = Vec::new();
        for g in self.groups.values() {
            out.extend(&g.members);
        }
        out.extend(
            self.voices
                .values()
                .filter(|v| v.group.is_none())
                .map(|v| v.id),
        );
        out.retain(|v| !self.is_pending(AnyId::Voice(*v)));
        out
    }

    /// A buffer as `buffer NAME …`, with what is not at its default. A
    /// source of `out` is the default; `buffer_source_lines` says the rest.
    fn buffer_line(&self, b: &BufferRepr) -> String {
        let mut out = format!("buffer {}", b.name);
        match b.length {
            LenSpec::Secs(s) if (s - 30.0).abs() > f64::EPSILON => {
                out.push_str(&format!(" length={s}s"));
            }
            LenSpec::Ticks(t) => out.push_str(&format!(" length={t}b")),
            LenSpec::Secs(_) => {}
        }
        if b.looping {
            out.push_str(" loop=yes");
        }
        if b.live {
            out.push_str(" live=yes");
        }
        if b.bits != 32 {
            out.push_str(&format!(" bits={}", b.bits));
        }
        if let Some(to) = &b.to {
            out.push_str(&format!(" to=\"{}\"", to.written));
        }
        if let Some(c) = b.clock.and_then(|c| self.clock_name(c)) {
            out.push_str(&format!(" on {c}"));
        }
        out
    }

    /// `buffer NAME from=…` for each buffer that records a voice or a group.
    fn buffer_source_lines(&self) -> Vec<String> {
        self.buffers
            .values()
            .filter_map(|b| {
                let from = match b.source {
                    BufSource::Out => return None,
                    BufSource::Voice(v) => self.display_name(v)?,
                    BufSource::Group(g) => self.groups.get(&g)?.name.clone(),
                };
                Some(format!("buffer {} from={from}", b.name))
            })
            .collect()
    }

    fn state_all(&self) -> String {
        let mut lines: Vec<String> = Vec::new();

        // clocks: main when it isn't the default, then the others
        let mut clocks: Vec<(&String, &TempoId)> = self
            .context_names
            .iter()
            .filter(|(_, t)| !self.is_pending(AnyId::Tempo(**t)))
            .collect();
        clocks.sort_by_key(|(_, t)| **t);
        for (name, &t) in clocks {
            let is_default = name == MAIN
                && self
                    .tempos
                    .get(&t)
                    .is_some_and(|r| r.unit == TempoUnit::Bpm && r.interval == MAIN_BPM);
            if !is_default && let Some(line) = self.clock_line(t, name) {
                lines.push(line);
            }
        }

        // tracks first: the voices read them
        lines.extend(self.track_lines());
        // buffers next, without a source (voices play them); what records a
        // voice or a group is restated once those exist
        lines.extend(self.buffers.values().map(|b| self.buffer_line(b)));

        // what a waiting line will make isn't there yet; what one will
        // remove still is
        let there = |id: AnyId| !self.is_pending(id);
        let groups: Vec<GroupId> = self
            .groups
            .keys()
            .copied()
            .filter(|g| there(AnyId::Group(*g)))
            .collect();
        for &g in &groups {
            lines.extend(self.group_text(g));
        }
        for v in self
            .voices
            .values()
            .filter(|v| v.group.is_none() && there(AnyId::Voice(v.id)))
        {
            lines.extend(self.voice_text(v.id));
        }
        for v in self.voices_in_order() {
            lines.extend(self.seqs_text(v));
            lines.extend(self.table_notes(v));
            lines.extend(self.fx_bypass_lines(v));
            lines.extend(self.lfo_bypass_lines_of(v));
        }
        for &g in &groups {
            lines.extend(self.group_extras(g));
        }
        lines.extend(self.buffer_source_lines());
        lines.extend(self.session_lines());

        if lines.is_empty() {
            return "# nothing here yet".to_string();
        }
        lines.join("\n")
    }

    fn state_named(&self, named: Named, name: &str) -> String {
        let text = match named {
            Named::Clock(t) => self.clock_line(t, name),
            Named::Group(g) => {
                let mut parts: Vec<String> = self.group_text(g).into_iter().collect();
                if let Some(group) = self.groups.get(&g) {
                    parts.extend(group.members.iter().filter_map(|&m| self.seqs_text(m)));
                }
                parts.extend(self.group_extras(g));
                Some(parts.join("\n"))
            }
            Named::Voice(v) => {
                let mut parts: Vec<String> = self.voice_text(v).into_iter().collect();
                parts.extend(self.seqs_text(v));
                parts.extend(self.fx_bypass_lines(v));
                parts.extend(self.lfo_bypass_lines_of(v));
                Some(parts.join("\n"))
            }
            Named::Lfo(p) | Named::Env(p) => self.procs.get(&p).and_then(|r| {
                let line = self.lfo_line(p).or_else(|| self.env_line(p))?;
                let (owner_name, text) = match r.owner {
                    ProcOwner::Voice(v) => {
                        let owner = self.voices.get(&v)?;
                        (
                            self.display_name(v)?,
                            format!("{} {{ {line} }}", owner.name),
                        )
                    }
                    ProcOwner::Group(g) => {
                        let owner = self.groups.get(&g)?;
                        (owner.name.clone(), format!("{} {{ {line} }}", owner.name))
                    }
                    ProcOwner::Session => return None,
                };
                let mut parts = vec![text];
                parts.extend(
                    self.lfo_bypass_lines(r.owner, &owner_name)
                        .into_iter()
                        .filter(|l| l.ends_with(&format!(".{}", r.name))),
                );
                Some(parts.join("\n"))
            }),
            Named::Fx {
                owner: FxOwner::Group(g),
                id,
                ..
            } => self.groups.get(&g).and_then(|owner| {
                let f = owner.fx.iter().find(|f| f.id == id)?;
                let mut parts = vec![format!("{} {{ {} }}", owner.name, fx_line(f))];
                parts.extend(
                    Self::bypass_lines(&owner.name, &owner.fx)
                        .into_iter()
                        .filter(|l| l.ends_with(&format!(".{}", f.name))),
                );
                Some(parts.join("\n"))
            }),
            Named::Fx {
                owner: FxOwner::Voice(v),
                id,
                ..
            } => self.voices.get(&v).and_then(|owner| {
                let f = owner.fx.iter().find(|f| f.id == id)?;
                let mut parts = vec![format!("{} {{ {} }}", owner.name, fx_line(f))];
                parts.extend(
                    self.fx_bypass_lines(v)
                        .into_iter()
                        .filter(|l| l.ends_with(&format!(".{}", f.name))),
                );
                Some(parts.join("\n"))
            }),
            Named::Wave { voice, id } => self.voices.get(&voice).and_then(|owner| {
                let w = owner.waves.iter().find(|w| w.id == id)?;
                if table_gone(w) {
                    return Some(Self::table_note(&owner.name, w));
                }
                let mut parts = vec![format!("{} {{ {} }}", owner.name, self.wave_line(owner, w))];
                parts.extend(
                    self.fx_bypass_lines(voice)
                        .into_iter()
                        .filter(|l| l.ends_with(&format!(".{}", w.name))),
                );
                Some(parts.join("\n"))
            }),
            Named::Seq(p) => self.procs.get(&p).and_then(|r| {
                // a seq in a packet is told as its packet
                if let Some(pk) = r.packet.and_then(|k| self.packets.get(&k)) {
                    return Some(self.packet_text(pk));
                }
                match r.owner {
                    ProcOwner::Voice(v) => {
                        let owner = self.voices.get(&v)?;
                        Some(format!("{} {{ {} }}", owner.name, self.seq_text(p, None)?))
                    }
                    ProcOwner::Group(g) => {
                        let owner = self.groups.get(&g)?;
                        if self.plain_seq(p) {
                            let line = self.seq_text(p, None)?;
                            return Some(format!("{} {{ {line} }}", owner.name));
                        }
                        self.seq_text(p, Some(&owner.name))
                    }
                    ProcOwner::Session => self.seq_text(p, None),
                }
            }),
            Named::Packet(k) => self.packets.get(&k).map(|pk| self.packet_text(pk)),
            Named::Buffer(b) => self.buffer_by_id(b).map(|r| {
                let mut parts = vec![self.buffer_line(r)];
                parts.extend(
                    self.buffer_source_lines()
                        .into_iter()
                        .filter(|l| l.starts_with(&format!("buffer {} ", r.name))),
                );
                parts.join("\n")
            }),
        };
        text.unwrap_or_default()
    }
}

impl EngineState {
    /// A packet by itself: by path when an owner holds it.
    fn packet_text(&self, pk: &PacketRepr) -> String {
        let owner = match pk.owner {
            ProcOwner::Voice(v) => self.voices.get(&v).map(|v| v.name.as_str()),
            ProcOwner::Group(g) => self.groups.get(&g).map(|g| g.name.as_str()),
            ProcOwner::Session => None,
        };
        self.packet_lines(pk, owner).join("\n")
    }
}

impl CmdProcessor {
    /// The session as a script; `path` (`drums`, `kick.beat`) limits it to
    /// one object.
    pub fn state(&self, path: Option<&str>) -> CmdResult<String> {
        let idents: Option<Vec<Ident>> = path.map(|p| {
            p.split('.')
                .map(|name| Ident {
                    name: name.to_string(),
                    span: Span::default(),
                })
                .collect()
        });
        self.state_text(idents.as_deref())
            .map_err(|e| CmdErr::Lang(vec![e]))
    }

    pub(super) fn state_text(&self, path: Option<&[Ident]>) -> Result<String, LangErr> {
        let es = &self.engine_state;
        let Some(path) = path else {
            return Ok(es.state_all());
        };
        let named = lookup(es, path)?;
        let name = path.last().map_or("", |i| i.name.as_str());
        Ok(es.state_named(named, name))
    }
}

#[cfg(test)]
mod gain_text_tests {
    use super::{db_to_linear, gain_text};

    #[test]
    fn decibels_only_when_they_read_back_and_are_shorter() {
        let db = |d: f64| db_to_linear(d) as f32;
        assert_eq!(gain_text(db(-6.0)), "-6db");
        assert_eq!(gain_text(db(12.0)), "12db");
        assert_eq!(gain_text(db(-4.5)), "-4.5db");
        assert_eq!(gain_text(db(-6.25)), "-6.25db");
        // short linear values stay linear
        assert_eq!(gain_text(0.5), "0.5");
        assert_eq!(gain_text(1.0), "1");
        assert_eq!(gain_text(2.0), "2");
        assert_eq!(gain_text(0.0), "0");
        // no round decibel value gives it back: linear
        assert_eq!(gain_text(0.123_456_7), "0.1234567");
    }
}
