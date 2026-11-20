//! Wave statements (WAVa): `wave NAME …` in a voice's block or as
//! `wave voice.NAME …`, a restated `voice.NAME setting=…`, and the
//! start/stop/pause/resume of one. `remove` is in `remove_target`.

use super::*;
use crate::audio_processing::commands::{
    Bypass, DEFAULT_TABLE_FRAMES, DEFAULT_TABLE_SLICES, WaveFrom, WaveRepr,
};
use crate::audio_processing::engine::{
    WaveParam, WaveShape, WaveTable, fits, frames_ok, wave_defaults,
};
use std::sync::Arc;

/// The frequency of a note name: a letter `a`..`g`, an optional `#` or `b`,
/// and an octave (`a4`, `c#4`, `eb2`, `f#-1`). 12-EDO with A4 = 440 Hz and
/// C4 the MIDI note 60, so `a3` is 220. `None` for anything else.
pub fn note_hz(word: &str) -> Option<f64> {
    let mut chars = word.chars().peekable();
    let semitone = match chars.next()?.to_ascii_lowercase() {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let accidental = match chars.peek() {
        Some('#') => 1,
        Some('b') => -1,
        _ => 0,
    };
    if accidental != 0 {
        chars.next();
    }
    let rest: String = chars.collect();
    let digits = rest.strip_prefix('-').unwrap_or(&rest);
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let octave: i32 = rest.parse().ok()?;
    let midi = (octave + 1) * 12 + semitone + accidental;
    Some(440.0 * (f64::from(midi - 69) / 12.0).exp2())
}

/// One setting of a wave statement, ready for the engine.
struct Given {
    param: WaveParam,
    value: f32,
    glide: Glide,
}

impl CmdProcessor {
    /// A `wave` statement, or a restated wave (`organ.s2 freq=660 over 2`).
    /// `block_voice` is the voice whose block it is in; outside a block the
    /// path is `voice.NAME`. A restated wave is edited in place and says so
    /// itself (W101) when nothing changed. True if it changed something.
    pub(super) fn lower_wave_stmt(
        &mut self,
        o: &Object,
        block_voice: Option<VoiceId>,
        out: &mut Out,
    ) -> Res<bool> {
        let Target::Path(path) = &o.target else {
            return Err(wrong_type(o.span, "a wave is named by a word, not a track").into());
        };
        let Some((name, owner_path)) = path.split_last() else {
            return Err(wrong_type(o.span, "a statement needs a name").into());
        };
        let at = path_span(path);
        let voice = match block_voice {
            Some(v) if owner_path.is_empty() => v,
            Some(_) => {
                return Err(wrong_type(at, "in a block a wave is named by one word").into());
            }
            None if owner_path.is_empty() => {
                return Err(wrong_type(
                    at,
                    "a wave belongs to a voice: write it in its block, or as voice.NAME",
                )
                .into());
            }
            None => match lookup(&self.engine_state, owner_path)? {
                Named::Voice(v) => v,
                other => {
                    return Err(wrong_type(
                        at,
                        format!("{} can't hold a wave", article(other.kind())),
                    )
                    .into());
                }
            },
        };

        let existing = self
            .engine_state
            .wave_list(voice)
            .iter()
            .find(|w| w.name == name.name)
            .cloned();
        if existing.is_none() && o.kind.is_none() {
            return Err(LangErr::new(
                Code::UnknownName,
                name.span,
                format!("unknown name '{}'", name.name),
            )
            .into());
        }
        let creating = existing.is_none();
        if creating {
            self.check_new_wave(voice, name)?;
        }
        if o.block.is_some() {
            return Err(wrong_type(o.span, "wave takes no block").into());
        }
        if let Some(receivers) = &o.pipe {
            let span = receivers.first().map_or(o.span, |r| r.kind.1);
            return Err(wrong_type(span, "wave takes no pipe").into());
        }
        let sc = schema(Kind::Wave);
        // `from="kick"` and `from=pad` say what `from "kick"` and `from pad` do
        let items: Vec<Item> = o
            .items
            .iter()
            .map(|item| match item {
                Item::Setting(id, Value::Str(s), None) if id.name == "from" => {
                    Item::From(s.clone(), id.span)
                }
                Item::Setting(id, Value::Word(w), None) if id.name == "from" => {
                    Item::FromName(w.clone())
                }
                other => other.clone(),
            })
            .collect();
        let checked = check_items(sc, &name.name, &items, creating, o.span)?;

        let mut shape = None;
        let mut retrig = None;
        let mut out_given = None;
        let mut fm_given: Option<(Vec<&Ident>, Span)> = None;
        // the text `ratio` was typed as: `Some(None)` clears it
        let mut ratio_text: Option<Option<String>> = None;
        // `ratio=2` (where it was written), or `ratio=none`
        let mut ratio_num: Option<Span> = None;
        let mut ratio_none = false;
        let mut freq_at: Option<Span> = None;
        let mut slices = None;
        let mut frames = None;
        let mut given: Vec<Given> = Vec::new();
        for setting in sc.settings {
            let Some(s) = checked.setting(setting.name) else {
                continue;
            };
            let param = match setting.name {
                "shape" => {
                    if let Value::Word(w) = s.value {
                        shape = WaveShape::from_name(&w.name);
                    }
                    continue;
                }
                "retrig" => {
                    if let Value::Word(w) = s.value {
                        retrig = Some(w.name == "yes");
                    }
                    continue;
                }
                "out" => {
                    if let Value::Word(w) = s.value {
                        out_given = Some(w.name == "yes");
                    }
                    continue;
                }
                "fm" => {
                    match s.value {
                        Value::Word(w) => {
                            fm_given =
                                Some((if w.name == "none" { vec![] } else { vec![w] }, s.span));
                        }
                        Value::Words(ws) => fm_given = Some((ws.iter().collect(), s.span)),
                        _ => {}
                    }
                    continue;
                }
                "ratio" => match s.value {
                    Value::Word(_) => {
                        ratio_none = true;
                        ratio_text = Some(None);
                        continue;
                    }
                    v => {
                        ratio_num = Some(s.span);
                        ratio_text = Some(match v {
                            Value::Typed(_, t) => Some(t.clone()),
                            _ => None,
                        });
                        WaveParam::Ratio
                    }
                },
                "freq" => {
                    freq_at = Some(s.span);
                    ratio_text = ratio_text.or(Some(None));
                    WaveParam::Freq
                }
                // the table's build settings: the schema checked the range
                "slices" | "frames" => {
                    let Value::Num(n) = s.value else { continue };
                    let n = eval_num(n)? as u32;
                    if setting.name == "frames" {
                        if !frames_ok(n as usize) {
                            return Err(LangErr::new(
                                Code::OutOfRange,
                                s.span,
                                format!("frames must be a power of two from 256 to 4096, not {n}"),
                            )
                            .into());
                        }
                        frames = Some((n, s.span));
                    } else {
                        slices = Some((n, s.span));
                    }
                    continue;
                }
                other => match WaveParam::from_name(other) {
                    Some(p) => p,
                    None => continue,
                },
            };
            let value = match (param, s.value) {
                (_, Value::Num(n) | Value::Typed(n, _)) => eval_num(n)?,
                (_, Value::Db(n)) => db_to_linear(eval_num(n)?),
                (WaveParam::Freq, Value::Duration(n, DurUnit::Hz)) => eval_num(n)?,
                (WaveParam::Freq, Value::Word(w)) => note_hz(&w.name)
                    .ok_or_else(|| wrong_type(s.span, "freq takes a frequency or a note"))?,
                _ => {
                    return Err(
                        wrong_type(s.span, format!("{} takes a number", setting.name)).into(),
                    );
                }
            };
            given.push(Given {
                param,
                value: value as f32,
                glide: Glide::of(s.over)?,
            });
        }

        // `from` is the table's; so are `slices` and `frames`
        let from = self.wave_from(&checked, o.span)?;
        let table = shape.or(existing.as_ref().map(|r| r.shape)) == Some(WaveShape::Table);
        if !table {
            let named = from
                .as_ref()
                .map(|f| ("from", f.1))
                .or(slices.map(|(_, at)| ("slices", at)))
                .or(frames.map(|(_, at)| ("frames", at)));
            if let Some((what, at)) = named {
                return Err(wrong_type(at, format!("only a table wave takes {what}")).into());
            }
        }
        let has_table = existing.as_ref().is_some_and(|r| r.from.is_some());
        if table && from.is_none() && !has_table {
            return Err(LangErr::new(
                Code::MissingItem,
                o.span,
                "a table wave needs from \"TRACK\" or from BUFFER",
            )
            .into());
        }

        // freq and ratio say the same thing two ways
        if let (Some(_), Some(at)) = (freq_at, ratio_num) {
            return Err(wrong_type(at, format!("{}: freq or ratio, not both", name.name)).into());
        }
        let fm_target = self.resolve_fm(voice, name, existing.as_ref(), fm_given)?;

        let Some(repr) = existing else {
            let mut params = wave_defaults();
            for g in given.iter().filter(|g| g.glide == Glide::Instant) {
                params[g.param.index()] = g.value;
            }
            let cmd = self
                .make_wave(
                    voice,
                    &name.name,
                    shape.unwrap_or(WaveShape::Sine),
                    params,
                    retrig.unwrap_or(true),
                )
                .map_err(|e| self.cmd_err(e, o.span))?;
            out.push(cmd);
            let id = self
                .engine_state
                .wave_list(voice)
                .last()
                .map_or(0, |w| w.id);
            // a glide on a new wave starts from the default
            for g in given.iter().filter(|g| g.glide != Glide::Instant) {
                if g.value != params[g.param.index()] {
                    let frames = self.voice_glide_frames(voice, g.glide, o.span)?;
                    let cmd = self.set_wave_param(voice, id, g.param.index(), g.value, frames);
                    out.push(cmd);
                }
            }
            if ratio_num.is_some() {
                out.push(self.set_wave_ratio_mode(voice, id, true));
            }
            if let Some(mask) = fm_target
                && mask != 0
            {
                out.push(self.set_wave_fm(voice, id, mask));
            }
            if let Some(Some(t)) = ratio_text {
                self.set_wave_ratio_text(voice, id, Some(t));
            }
            if out_given == Some(false) {
                out.push(self.set_wave_out(voice, id, false));
            }
            if let Some((from, at)) = from {
                let spec = (
                    slices.map_or(DEFAULT_TABLE_SLICES, |s| s.0),
                    frames.map_or(DEFAULT_TABLE_FRAMES, |f| f.0),
                );
                if let Some(cmd) =
                    self.table_command(voice, id, &name.name, (from, at), spec, out)?
                {
                    out.push(cmd);
                }
            }
            return Ok(true);
        };

        let mut changed = false;
        if let Some(shape) = shape
            && shape != repr.shape
        {
            let cmd = self.set_wave_shape(voice, repr.id, shape);
            out.push(cmd);
            changed = true;
        }
        if let Some(retrig) = retrig
            && retrig != repr.retrig
        {
            let cmd = self.set_wave_retrig(voice, repr.id, retrig);
            out.push(cmd);
            changed = true;
        }
        // freq and ratio switch the mode; `ratio=none` goes back to freq
        let ratio_mode = match (ratio_num, freq_at, ratio_none) {
            (Some(_), _, _) => true,
            (None, Some(_), _) | (None, None, true) => false,
            (None, None, false) => repr.ratio_mode,
        };
        if ratio_mode != repr.ratio_mode {
            let cmd = self.set_wave_ratio_mode(voice, repr.id, ratio_mode);
            out.push(cmd);
            changed = true;
        }
        if let Some(text) = ratio_text
            && text != repr.ratio_text
        {
            // `none` and `freq=` leave no ratio to print; a typed ratio whose
            // value didn't move is still a restatement
            self.set_wave_ratio_text(voice, repr.id, text);
            changed |= ratio_num.is_some();
        }
        if let Some(modulators) = fm_target
            && modulators != repr.fm
        {
            let cmd = self.set_wave_fm(voice, repr.id, modulators);
            out.push(cmd);
            changed = true;
        }
        if let Some(heard) = out_given
            && heard != repr.out
        {
            let cmd = self.set_wave_out(voice, repr.id, heard);
            out.push(cmd);
            changed = true;
        }
        for g in &given {
            if g.value != repr.params[g.param.index()] {
                let frames = self.voice_glide_frames(voice, g.glide, o.span)?;
                let cmd = self.set_wave_param(voice, repr.id, g.param.index(), g.value, frames);
                out.push(cmd);
                changed = true;
            }
        }
        // a table is rebuilt when its source is restated (a buffer re-read),
        // or its slices or frames change
        if table {
            let spec = (
                slices.map_or(repr.slices, |s| s.0),
                frames.map_or(repr.frames, |f| f.0),
            );
            if from.is_some() || spec != (repr.slices, repr.frames) {
                let source = match (from, repr.from) {
                    (Some(given), _) => Some(given),
                    (None, Some(f @ (WaveFrom::Track(_) | WaveFrom::Buffer(_)))) => {
                        Some((f, o.span))
                    }
                    (None, _) => None,
                };
                let Some((from, at)) = source else {
                    return Err(LangErr::new(
                        Code::UnknownName,
                        o.span,
                        "the track or buffer this table was cut from was removed",
                    )
                    .with_hint("restate it: from \"TRACK\" or from BUFFER")
                    .into());
                };
                if let Some(cmd) =
                    self.table_command(voice, repr.id, &name.name, (from, at), spec, out)?
                {
                    out.push(cmd);
                }
                changed = true;
            }
        }
        if !changed {
            out.warn_unchanged(o.span, &format!("'{}'", name.name));
        }
        Ok(changed)
    }

    // what `fm=` of a wave statement says: `None` when it isn't written,
    // `Some(0)` for `fm=none`, else the bitmask of the waves named. E405 for
    // a wave the voice doesn't have, E411 for one that is pending, E402 for
    // a name twice, E412 for a loop
    fn resolve_fm(
        &self,
        voice: VoiceId,
        name: &Ident,
        existing: Option<&WaveRepr>,
        given: Option<(Vec<&Ident>, Span)>,
    ) -> Res<Option<u8>> {
        let Some((targets, at)) = given else {
            return Ok(None);
        };
        let es = &self.engine_state;
        let owner = es.name_of(AnyId::Voice(voice));
        let mut mask = 0u8;
        for target in targets {
            if target.name == name.name {
                return Err(LangErr::new(
                    Code::Cycle,
                    at,
                    format!(
                        "{owner}.{0}.fm={0} would make a loop: a wave can't modulate itself",
                        name.name
                    ),
                )
                .with_hint("use fb= for feedback")
                .into());
            }
            let Some(modulator) = es.wave_list(voice).iter().find(|w| w.name == target.name) else {
                return Err(LangErr::new(
                    Code::MissingItem,
                    at,
                    format!("{owner} has no wave {}", target.name),
                )
                .with_hint("a wave is made before the one it modulates")
                .into());
            };
            // a line typed now may not name what a waiting line will make
            if !es.staging
                && es.is_pending(AnyId::Wave {
                    voice,
                    id: modulator.id,
                })
            {
                return Err(LangErr::new(
                    Code::NotCreatedYet,
                    at,
                    format!(
                        "'{owner}.{}' doesn't exist yet (a waiting line creates it)",
                        target.name
                    ),
                )
                .with_hint("name it in a line that waits too, or wait for it")
                .into());
            }
            if mask >> modulator.id & 1 == 1 {
                return Err(wrong_type(at, format!("fm names {} twice", target.name)).into());
            }
            mask |= 1 << modulator.id;
            if let Some(carrier) = existing
                && es.would_cycle(voice, carrier.id, 1 << modulator.id)
            {
                return Err(LangErr::new(
                    Code::Cycle,
                    at,
                    format!(
                        "{owner}.{0}.fm={1} would make a loop: {1} already follows {0}",
                        name.name, target.name
                    ),
                )
                .into());
            }
        }
        Ok(Some(mask))
    }

    // what a wave statement's `from` names, with where it was written
    fn wave_from(&self, checked: &Checked, at: Span) -> Res<Option<(WaveFrom, Span)>> {
        let span = match (checked.from, checked.from_name) {
            (Some((_, span)), _) => span,
            (None, Some(id)) => id.span,
            (None, None) => return Ok(None),
        };
        Ok(match self.voice_source(checked, at, false)? {
            VoiceSrc::Track(t) => Some((WaveFrom::Track(t), span)),
            VoiceSrc::Buffer(b) => Some((WaveFrom::Buffer(b), span)),
            VoiceSrc::Waves | VoiceSrc::Gone => None,
        })
    }

    // builds a wave's table from its source, here on the command thread, and
    // returns the command that hands it to the engine. A buffer with nothing
    // recorded makes no table (W109) and no command: the wave waits for
    // `from=` to be restated.
    fn table_command(
        &mut self,
        voice: VoiceId,
        id: u8,
        wave: &str,
        (from, at): (WaveFrom, Span),
        (slices, frames): (u32, u32),
        out: &mut Out,
    ) -> Res<Option<Command>> {
        if !fits(slices as usize, frames as usize) {
            return Err(LangErr::new(
                Code::OutOfRange,
                at,
                "table too large: fewer slices or frames",
            )
            .into());
        }
        let es = &self.engine_state;
        let (samples, channels, name): (Arc<[f32]>, usize, String) = match from {
            WaveFrom::Track(t) => {
                let Some(r) = es.tracks.values().find(|r| r.id == t) else {
                    return Err(wrong_type(at, "that track is gone").into());
                };
                (Arc::clone(&r.samples), r.channels as usize, r.name.clone())
            }
            WaveFrom::Buffer(b) => {
                let Some(r) = es.buffer_by_id(b) else {
                    return Err(wrong_type(at, "that buffer is gone").into());
                };
                // what has been heard so far, in the half voices hear (one
                // take: a take that begins in that half during the copy is
                // seen and the copy made again)
                let channels = usize::from(r.data.channels);
                (Arc::from(r.data.copy_heard()), channels, r.name.clone())
            }
            WaveFrom::Gone => return Err(wrong_type(at, "that source is gone").into()),
        };
        if samples.is_empty() && matches!(from, WaveFrom::Buffer(_)) {
            out.warns.push(LangErr::new(
                Code::EmptySource,
                at,
                format!("{name} is empty: {wave} stays silent until from={name} is restated"),
            ));
            self.set_wave_source(voice, id, from, (slices, frames));
            return Ok(None);
        }
        if samples.is_empty() {
            return Err(wrong_type(at, format!("{name} has no sound to make a table from")).into());
        }
        let table = Arc::new(WaveTable::build(
            &samples,
            channels,
            slices as usize,
            frames as usize,
        ));
        Ok(Some(self.set_wave_table(
            voice,
            id,
            from,
            (slices, frames),
            table,
        )))
    }

    // a name for a new wave: not `all`, and not a seq, packet, lfo, env or
    // effect of the same voice (they share its scope)
    fn check_new_wave(&self, voice: VoiceId, name: &Ident) -> Res<()> {
        if name.name == RESERVED_ALL {
            return Err(LangErr::new(
                Code::ReservedWord,
                name.span,
                format!("'{}' is reserved", name.name),
            )
            .into());
        }
        let es = &self.engine_state;
        let owner = ProcOwner::Voice(voice);
        let clash = if let Some(p) = es.proc_named(owner, &name.name) {
            Some(match es.procs.get(&p) {
                Some(r) if r.lfo.is_some() => "an lfo",
                Some(r) if r.env.is_some() => "an env",
                _ => "a seq",
            })
        } else if es.packet_of(owner, &name.name).is_some() {
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

    /// `start`, `stop`, `pause` or `resume` of one wave. `stop` silences it
    /// and puts its phase back, `pause` silences it and holds the phase;
    /// `start` and `resume` bring it back (`start` from its phase). One that
    /// is already in that state warns and does nothing.
    pub(super) fn wave_action(
        &mut self,
        word: ActionWord,
        voice: VoiceId,
        id: u8,
        at: Span,
        out: &mut Out,
    ) -> Option<Command> {
        let repr = self
            .engine_state
            .wave_list(voice)
            .iter()
            .find(|w| w.id == id)?;
        let (name, now) = (repr.name.clone(), repr.bypass);
        let (to, clear) = match word {
            ActionWord::Stop if now != Some(Bypass::Stopped) => (Some(Bypass::Stopped), true),
            ActionWord::Pause if now.is_none() => (Some(Bypass::Paused), false),
            ActionWord::Start if now.is_some() => (None, true),
            ActionWord::Resume if now.is_some() => (None, false),
            _ => {
                out.warn_unchanged(at, &format!("'{name}'"));
                return None;
            }
        };
        Some(self.bypass_wave(voice, id, to, clear))
    }
}
