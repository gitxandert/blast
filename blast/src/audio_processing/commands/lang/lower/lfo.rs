//! `lfo` statements (LFO): `lfo NAME …` in a voice's or group's block, a
//! restated `owner.NAME setting=…`, and the target and speed rules. The
//! actions (`pause`, `stop`, …) and `remove` are in `lower_action` and
//! `remove_target`, as for a seq.

use super::*;
use crate::audio_processing::commands::procs::lfo::{LfoChange, NewLfo};
use crate::audio_processing::engine::WaveParam;
use crate::audio_processing::engine::fx::{own_settings, param_of};
use crate::audio_processing::processes::{LfoRate, LfoShape, LfoTarget};

/// The speed an lfo statement asked for: the rate, and the text `state`
/// prints for it.
struct Speed {
    rate: LfoRate,
    text: String,
}

fn shape_of(word: &str) -> LfoShape {
    match word {
        "tri" => LfoShape::Tri,
        "square" => LfoShape::Square,
        "saw" => LfoShape::Saw,
        "sh" => LfoShape::SampleHold,
        _ => LfoShape::Sine,
    }
}

pub(super) fn unit_text(unit: DurUnit) -> &'static str {
    match unit {
        DurUnit::Ticks => "b",
        DurUnit::S => "s",
        DurUnit::Ms => "ms",
        DurUnit::Us => "us",
        DurUnit::Ns => "ns",
        DurUnit::Frames => "f",
        DurUnit::Hz => "hz",
    }
}

fn is_clock(rate: LfoRate) -> bool {
    matches!(rate, LfoRate::Clock { .. })
}

/// How many periods a speed written as `text` lasts, if it is a bare number
/// (or nothing, the default of 1).
pub(super) fn periods_of(text: &str) -> Option<f64> {
    if text.is_empty() {
        return Some(1.0);
    }
    text.parse().ok()
}

// the value a restated setting changes to, if it differs from `now`
fn changed(new: Option<f32>, now: f32) -> Option<f32> {
    new.filter(|v| *v != now)
}

impl CmdProcessor {
    // the owner's period in ticks
    pub(super) fn lfo_owner_period(&self, owner: ProcOwner) -> u32 {
        self.engine_state
            .owner_period(owner)
            .unwrap_or(DEFAULT_PERIOD)
    }

    /// The names an lfo can target on `owner`, for E401.
    fn lfo_target_list(&self, owner: ProcOwner) -> String {
        let mut names: Vec<String> = VoiceParam::ALL
            .iter()
            .map(|p| p.name().to_string())
            .collect();
        if let Some(fx) = fx_owner(owner) {
            for f in self.engine_state.fx_list(fx) {
                names.push(format!("{}.PARAM", f.name));
            }
        }
        if let ProcOwner::Voice(v) = owner {
            for w in self.engine_state.wave_list(v) {
                names.push(format!("{}.PARAM", w.name));
            }
        }
        names.join(", ")
    }

    /// Where `text` (`pan`, `lo.cutoff`) points, on `owner`. `who` is the
    /// kind that asks (`lfo`, `env`), for the errors.
    pub(super) fn lfo_target(
        &self,
        who: &str,
        text: &str,
        owner: ProcOwner,
        at: Span,
    ) -> Res<LfoTarget> {
        let unknown = |what: String| {
            LangErr::new(Code::UnknownSetting, at, what)
                .with_hint(format!("an {who} targets: {}", self.lfo_target_list(owner)))
        };
        let parts: Vec<&str> = text.split('.').collect();
        match parts.as_slice() {
            [one] => match VoiceParam::from_name(one) {
                Some(p) => Ok(LfoTarget::Voice(p)),
                None => Err(unknown(format!("{who} can't target '{one}'")).into()),
            },
            [name, setting] if matches!(owner, ProcOwner::Voice(v) if self.engine_state.has_wave(v, name)) =>
            {
                let ProcOwner::Voice(v) = owner else {
                    return Err(unknown(format!("{who} can't target '{text}'")).into());
                };
                let wave = self
                    .engine_state
                    .wave_list(v)
                    .iter()
                    .find(|w| w.name == *name);
                let id = wave.map_or(0, |w| w.id);
                // phase is read only when a wave restarts, so nothing moves it
                match WaveParam::from_name(setting).filter(|p| *p != WaveParam::Phase) {
                    Some(p) => Ok(LfoTarget::Wave {
                        id,
                        param: p.index() as u8,
                    }),
                    None => Err(LangErr::new(
                        Code::UnknownSetting,
                        at,
                        format!("wave has no setting '{setting}' to modulate"),
                    )
                    .with_hint("a wave takes: freq, gain, pw, pos")
                    .into()),
                }
            }
            [fx_name, setting] => {
                let Some(fx) = fx_owner(owner) else {
                    return Err(unknown(format!("{who} can't target '{text}'")).into());
                };
                let list = self.engine_state.fx_list(fx);
                let Some(f) = list.iter().find(|f| f.name == *fx_name) else {
                    let names = list.iter().map(|f| f.name.as_str());
                    return Err(unknown(format!("there is no effect '{fx_name}' here"))
                        .with_suggestion(fx_name, names)
                        .into());
                };
                let kind = fx_language_kind(f.kind);
                let settings: Vec<&str> = own_settings(f.kind.ty).iter().map(|s| s.name).collect();
                match param_of(f.kind.ty, setting) {
                    Some(param) => Ok(LfoTarget::Fx {
                        id: f.id,
                        param: param as u8,
                    }),
                    None => Err(LangErr::new(
                        Code::UnknownSetting,
                        at,
                        format!("{} has no setting '{setting}'", kind_word(kind)),
                    )
                    .with_hint(format!(
                        "{} takes: {}",
                        kind_word(kind),
                        settings.join(", ")
                    ))
                    .into()),
                }
            }
            _ => Err(unknown(format!("{who} can't target '{text}'")).into()),
        }
    }

    /// The speed a `freq=` value asks for, on `owner`.
    fn lfo_speed(&self, value: &Value, owner: ProcOwner, at: Span) -> Res<Speed> {
        match value {
            Value::Num(n) => {
                let v = eval_num(n)?;
                let ticks = v * f64::from(self.lfo_owner_period(owner));
                Ok(Speed {
                    rate: LfoRate::Clock {
                        ticks: ticks as f32,
                    },
                    text: format!("{v}"),
                })
            }
            Value::Duration(n, unit) => {
                let v = eval_num(n)?;
                let text = format!("{v}{}", unit_text(*unit));
                let secs = match unit {
                    DurUnit::Ticks => {
                        return Ok(Speed {
                            rate: LfoRate::Clock { ticks: v as f32 },
                            text,
                        });
                    }
                    DurUnit::Hz => {
                        return Ok(Speed {
                            rate: LfoRate::Hz(v as f32),
                            text,
                        });
                    }
                    DurUnit::S => v,
                    DurUnit::Ms => v / 1e3,
                    DurUnit::Us => v / 1e6,
                    DurUnit::Ns => v / 1e9,
                    DurUnit::Frames => v / frame_rate(),
                };
                let hz = 1.0 / secs;
                if !(0.001..=200.0).contains(&hz) {
                    return Err(LangErr::new(
                        Code::OutOfRange,
                        at,
                        format!("freq must be between 0.001hz and 200hz; {text} is {hz}hz"),
                    )
                    .into());
                }
                Ok(Speed {
                    rate: LfoRate::Hz(hz as f32),
                    text,
                })
            }
            _ => Err(wrong_type(at, "freq takes a rate or a cycle length").into()),
        }
    }

    /// An `lfo` statement, or a restated lfo (`kick.wob depth=1 over 4`).
    /// `block_owner` is the voice or group whose block it is in; outside a
    /// block the path is `owner.NAME`. A restated lfo is edited in place and
    /// says so itself (W101) when nothing changed. True if it changed
    /// something.
    pub(super) fn lower_lfo_stmt(
        &mut self,
        o: &Object,
        block_owner: Option<ProcOwner>,
        out: &mut Out,
    ) -> Res<bool> {
        let Target::Path(path) = &o.target else {
            return Err(wrong_type(o.span, "an lfo is named by a word, not a track").into());
        };
        let Some((name, owner_path)) = path.split_last() else {
            return Err(wrong_type(o.span, "a statement needs a name").into());
        };
        let at = path_span(path);
        let owner = match block_owner {
            Some(p) if owner_path.is_empty() => p,
            Some(_) => {
                return Err(wrong_type(at, "in a block an lfo is named by one word").into());
            }
            None if owner_path.is_empty() => {
                return Err(wrong_type(
                    at,
                    "an lfo needs an owner: write it in a voice's or group's block, or as owner.NAME",
                )
                .into());
            }
            None => match lookup(&self.engine_state, owner_path)? {
                Named::Voice(v) => ProcOwner::Voice(v),
                Named::Group(g) => ProcOwner::Group(g),
                other => {
                    return Err(wrong_type(
                        at,
                        format!("{} can't hold an lfo", article(other.kind())),
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
        if let Some(p) = existing
            && self.engine_state.procs[&p].lfo.is_none()
        {
            return Err(LangErr::new(
                Code::KindClash,
                name.span,
                format!("'{}' is a seq, not an lfo", name.name),
            )
            .into());
        }
        if creating {
            self.check_new_lfo(owner, name)?;
        }
        if o.block.is_some() {
            return Err(wrong_type(o.span, "lfo takes no block").into());
        }
        if let Some(pipe) = &o.pipe {
            let span = pipe.first().map_or(o.span, |r| r.kind.1);
            return Err(wrong_type(span, "lfo takes no pipe").into());
        }
        let checked = check_items(schema(Kind::Lfo), &name.name, &o.items, creating, o.span)?;

        // the settings that were written
        let num = |s: &str| -> Res<Option<f32>> {
            match checked.setting(s).map(|c| c.value) {
                Some(Value::Num(n)) => Ok(Some(eval_num(n)? as f32)),
                // a depth on pitch: semitones (7st) or cents (50c)
                Some(Value::Pitch(n, unit)) => {
                    let st = pitch_semitones(eval_num(n)?, *unit);
                    Ok(st.map(|v| v as f32))
                }
                _ => Ok(None),
            }
        };
        let (depth, offset, phase) = (num("depth")?, num("offset")?, num("phase")?);
        let shape = match checked.setting("shape").map(|c| c.value) {
            Some(Value::Word(w)) => Some(shape_of(&w.name)),
            _ => None,
        };
        let seed = match checked.setting("seed").map(|c| c.value) {
            Some(Value::Num(n)) => Some(eval_num(n)? as u64),
            _ => None,
        };
        let target = match checked.setting("to") {
            Some(CheckedSetting {
                value: Value::Word(w),
                span,
                ..
            }) => Some((
                self.lfo_target("lfo", &w.name, owner, *span)?,
                w.name.clone(),
            )),
            _ => None,
        };
        let now_text = existing.and_then(|id| {
            let procs = &self.engine_state.procs[&id];
            procs.lfo.as_ref().map(|r| r.target_text.clone())
        });
        check_depth_unit(
            &checked,
            target.as_ref().map(|t| t.1.as_str()),
            now_text.as_deref(),
        )?;
        let speed = match checked.setting("freq") {
            Some(s) => Some(self.lfo_speed(s.value, owner, s.span)?),
            None => None,
        };

        let Some(id) = existing else {
            let (Some((target, target_text)), Some(depth)) = (target, depth) else {
                // check_items said so already
                return Err(wrong_type(o.span, "an lfo needs to= and depth=").into());
            };
            let (tempo, link) = self.lfo_clock(owner, o.span)?;
            let (rate, freq_text) = match speed {
                Some(s) => (s.rate, s.text),
                None => (
                    LfoRate::Clock {
                        ticks: self.lfo_owner_period(owner) as f32,
                    },
                    String::new(),
                ),
            };
            let new = NewLfo {
                shape: shape.unwrap_or(LfoShape::Sine),
                target,
                target_text,
                depth,
                offset: offset.unwrap_or(0.0),
                phase: phase.unwrap_or(0.0),
                seed,
                rate,
                freq_text,
            };
            let cmd = self
                .make_lfo(owner, &name.name, &new, tempo, link)
                .map_err(|e| self.cmd_err(e, o.span))?;
            out.push(cmd);
            return Ok(true);
        };

        // restated: only what differs from the lfo's values changes
        let repr = self.engine_state.procs[&id].lfo.clone().unwrap_or_else(|| {
            unreachable!("checked above that the process is an lfo");
        });
        let period = self.lfo_owner_period(owner);
        let glide_of = |name: &str| -> Res<Glide> {
            Ok(Glide::of(checked.setting(name).and_then(|s| s.over))?)
        };
        let frames = |cp: &CmdProcessor, g: Glide| cp.proc_glide_frames(id, g, period, o.span);

        let mut change = LfoChange::default();
        if let Some(v) = changed(depth, repr.depth) {
            change.depth = Some((v, frames(self, glide_of("depth")?)?));
        }
        if let Some(v) = changed(offset, repr.offset) {
            change.offset = Some((v, frames(self, glide_of("offset")?)?));
        }
        if let Some(v) = changed(phase, repr.phase) {
            change.phase = Some(v);
        }
        change.shape = shape.filter(|s| *s != repr.shape);
        change.seed = seed.filter(|s| repr.seed != Some(*s));
        if let Some((t, text)) = target
            && t != repr.target
        {
            change.target = Some((t, text));
        } else if let Some(s) = checked.setting("to")
            && let Value::Word(w) = s.value
        {
            // the same target, spelled another way: only the text changes
            if w.name != repr.target_text {
                change.target = Some((repr.target, w.name.clone()));
            }
        }
        if let Some(s) = speed {
            let glide = glide_of("freq")?;
            let same_kind = is_clock(s.rate) == is_clock(repr.rate);
            if !same_kind && glide != Glide::Instant {
                return Err(LangErr::new(
                    Code::WrongType,
                    checked.setting("freq").map_or(o.span, |c| c.span),
                    "freq can't glide between a rate (hz, ms, …) and a cycle length (periods, b)",
                )
                .into());
            }
            if !same_kind {
                change.rate = Some(s.rate);
                change.freq_text = Some(s.text);
            } else if s.rate != repr.rate {
                let value = match s.rate {
                    LfoRate::Hz(v) | LfoRate::Clock { ticks: v } => v,
                };
                change.freq = Some((value, frames(self, glide)?));
                change.freq_text = Some(s.text);
            } else if s.text != repr.freq_text {
                // another way to write the same speed
                change.freq_text = Some(s.text);
            }
        }

        let nothing_new = change.depth.is_none()
            && change.offset.is_none()
            && change.phase.is_none()
            && change.shape.is_none()
            && change.seed.is_none()
            && change.target.is_none()
            && change.freq.is_none()
            && change.rate.is_none()
            && change.freq_text.is_none();
        if nothing_new {
            out.warn_unchanged(o.span, &format!("'{}'", name.name));
            return Ok(false);
        }
        let cmd = self.edit_lfo(id, change);
        out.push(cmd);
        Ok(true)
    }

    // the clock an lfo reads: its owner's, and the link that moves it along
    // with the owner
    pub(super) fn lfo_clock(&self, owner: ProcOwner, at: Span) -> Res<(TempoId, TempoLink)> {
        let tempo = self.engine_state.owner_tempo(owner);
        match (owner, tempo) {
            (ProcOwner::Voice(_), Some(t)) => Ok((t, TempoLink::Voice)),
            (ProcOwner::Group(g), Some(t)) => Ok((t, TempoLink::Group(g))),
            _ => Err(wrong_type(at, "the lfo's owner isn't known").into()),
        }
    }

    // a name for a new lfo: not `all`, and not a packet or effect of the
    // same owner (they share its scope)
    pub(super) fn check_new_lfo(&self, owner: ProcOwner, name: &Ident) -> Res<()> {
        if name.name == RESERVED_ALL {
            return Err(LangErr::new(
                Code::ReservedWord,
                name.span,
                format!("'{}' is reserved", name.name),
            )
            .into());
        }
        let es = &self.engine_state;
        let clash = if es.packet_of(owner, &name.name).is_some() {
            Some("a packet")
        } else {
            es.effect_or_wave(owner, &name.name)
        };
        match clash {
            Some(what) => Err(LangErr::new(
                Code::KindClash,
                name.span,
                format!("'{}' is already {what}", name.name),
            )
            .into()),
            None => Ok(()),
        }
    }

    /// Removes the lfos of `owner` that target effect `id`, in the mirror,
    /// and returns the commands for the engine. Each raises W102.
    pub(super) fn drop_lfos_of_fx(&mut self, owner: FxOwner, id: u16) -> Vec<Command> {
        let proc_owner = match owner {
            FxOwner::Voice(v) => ProcOwner::Voice(v),
            FxOwner::Group(g) => ProcOwner::Group(g),
        };
        let doomed: Vec<ProcId> = self
            .engine_state
            .owner_procs(proc_owner)
            .map(|p| p.values().copied().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|p| {
                self.engine_state.procs[p]
                    .lfo
                    .as_ref()
                    .is_some_and(|l| matches!(l.target, LfoTarget::Fx { id: i, .. } if i == id))
            })
            .collect();
        let mut cmds = Vec::new();
        for p in doomed {
            let path = self.engine_state.proc_path(p).unwrap_or_default();
            let to = self.engine_state.procs[&p]
                .lfo
                .as_ref()
                .map(|l| l.target_text.clone())
                .unwrap_or_default();
            match self.engine_state.remove_proc(p) {
                Command::Batch(b) => cmds.extend(b.cmds),
                cmd => cmds.push(cmd),
            }
            self.engine_state.warnings.push((
                Code::ReceiverDropped,
                format!("lfo '{path}' went with the effect it modulated ({to})"),
            ));
        }
        cmds
    }
}

impl CmdProcessor {
    /// Removes the lfos of voice `v` that target wave `id`, in the mirror,
    /// and returns the commands for the engine. Each raises W102.
    pub(super) fn drop_lfos_of_wave(&mut self, v: VoiceId, id: u8) -> Vec<Command> {
        let doomed: Vec<ProcId> =
            self.engine_state
                .owner_procs(ProcOwner::Voice(v))
                .map(|p| p.values().copied().collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .filter(|p| {
                    self.engine_state.procs[p].lfo.as_ref().is_some_and(
                        |l| matches!(l.target, LfoTarget::Wave { id: i, .. } if i == id),
                    )
                })
                .collect();
        let mut cmds = Vec::new();
        for p in doomed {
            let path = self.engine_state.proc_path(p).unwrap_or_default();
            let to = self.engine_state.procs[&p]
                .lfo
                .as_ref()
                .map(|l| l.target_text.clone())
                .unwrap_or_default();
            match self.engine_state.remove_proc(p) {
                Command::Batch(b) => cmds.extend(b.cmds),
                cmd => cmds.push(cmd),
            }
            self.engine_state.warnings.push((
                Code::ReceiverDropped,
                format!("lfo '{path}' went with the wave it modulated ({to})"),
            ));
        }
        cmds
    }
}

// the effect owner for a process owner
pub(super) fn fx_owner(owner: ProcOwner) -> Option<FxOwner> {
    match owner {
        ProcOwner::Voice(v) => Some(FxOwner::Voice(v)),
        ProcOwner::Group(g) => Some(FxOwner::Group(g)),
        ProcOwner::Session => None,
    }
}
