//! Voice waves (WAVa): the arguments of `Command::Wave`, the mirror of a
//! voice's waves, and the typed core the language lowers into. The mirror
//! decides the ids (a wave's id is its engine slot); the engine applies what
//! arrives.

use super::errors::{CmdErr, CmdResult};
use super::fx::Bypass;
use super::reprs::{BufferId, EngineState, ProcOwner, TrackId, VoiceId};
use super::{CmdProcessor, Command};
use crate::audio_processing::engine::{MAX_WAVES, WAVE_PARAMS, WaveShape, WaveTable};
use std::sync::Arc;

/// What a `Command::Wave` does to one wave.
pub enum WaveOp {
    /// Makes a wave in its slot.
    Insert {
        shape: WaveShape,
        params: [f32; WAVE_PARAMS],
        retrig: bool,
    },
    /// Sets a `WaveParam` (by index), at once or as a glide of `frames`.
    Set {
        param: u8,
        value: f32,
        frames: u32,
    },
    /// Changes what the wave plays.
    Shape(WaveShape),
    /// Whether the wave restarts its phase on every trigger.
    Retrig(bool),
    /// `stop`: `(true, true)`; `pause`: `(true, false)`; `start`:
    /// `(false, true)`; `resume`: `(false, false)`.
    Bypass {
        on: bool,
        clear: bool,
    },
    Remove,
    /// The waves that phase-modulate this one (WAVc, WAVd): a bitmask over
    /// the voice's wave ids; 0: none.
    Fm(u8),
    /// Whether the wave is heard (`out=no`: a modulator only).
    Out(bool),
    /// Whether the frequency is `ratio` times the voice's note (`true`) or
    /// `freq`.
    RatioMode(bool),
    /// Gives a table wave its table (WAVb); the old one leaves as garbage.
    Table(Arc<WaveTable>),
}

/// Arguments for `Command::Wave`.
pub struct WaveArgs {
    pub voice: VoiceId,
    /// The wave, by its id within the voice (its slot, below `MAX_WAVES`).
    pub id: u8,
    pub op: WaveOp,
}

/// Where a table wave's table was cut from, in the mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveFrom {
    Track(TrackId),
    Buffer(BufferId),
    /// The track or buffer was removed; the table plays on (it owns its
    /// data) but can't be rebuilt from it.
    Gone,
}

/// The cycles of a new table wave when `slices` isn't given.
pub const DEFAULT_TABLE_SLICES: u32 = 1;
/// The cycle length of a new table wave when `frames` isn't given.
pub const DEFAULT_TABLE_FRAMES: u32 = 2048;

/// A wave in the mirror.
#[derive(Clone)]
pub struct WaveRepr {
    pub(crate) name: String,
    pub(crate) id: u8,
    pub(crate) shape: WaveShape,
    /// Every parameter's value, as the engine will hold it (a glide's
    /// target).
    pub(crate) params: [f32; WAVE_PARAMS],
    pub(crate) retrig: bool,
    /// The waves of the same voice that modulate this one: a bitmask over
    /// their ids (0: none).
    pub(crate) fm: u8,
    /// The text `ratio` was written as, when it can be printed back (command
    /// thread only; `None`: print the number).
    pub(crate) ratio_text: Option<String>,
    /// Whether it is heard.
    pub(crate) out: bool,
    /// Whether `ratio` (not `freq`) gives its frequency.
    pub(crate) ratio_mode: bool,
    pub(crate) bypass: Option<Bypass>,
    /// What the table was cut from (`None` until a table wave has one).
    pub(crate) from: Option<WaveFrom>,
    /// Cycles the table was cut into, and samples in each.
    pub(crate) slices: u32,
    pub(crate) frames: u32,
}

impl EngineState {
    /// Marks every wave whose table was cut from `from` as having lost it.
    pub(crate) fn forget_table_source(&mut self, from: WaveFrom) {
        for r in self.voices.values_mut() {
            for w in &mut r.waves {
                if w.from == Some(from) {
                    w.from = Some(WaveFrom::Gone);
                }
            }
        }
    }

    /// Whether voice `v` has a wave called `name`.
    pub(crate) fn has_wave(&self, v: VoiceId, name: &str) -> bool {
        self.voices
            .get(&v)
            .is_some_and(|r| r.waves.iter().any(|w| w.name == name))
    }

    /// What a name is in `owner`'s scope if it is an effect or a wave
    /// ("an effect", "a wave"): a new seq, packet, lfo, env, effect or wave
    /// can't take it.
    pub(crate) fn effect_or_wave(&self, owner: ProcOwner, name: &str) -> Option<&'static str> {
        if self.has_fx(owner, name) {
            return Some("an effect");
        }
        match owner {
            ProcOwner::Voice(v) if self.has_wave(v, name) => Some("a wave"),
            _ => None,
        }
    }

    /// Whether making the waves in the bitmask `modulators` the ones that
    /// modulate `carrier` would close a loop (any of them is the carrier or
    /// follows it down its chain).
    pub(crate) fn would_cycle(&self, v: VoiceId, carrier: u8, modulators: u8) -> bool {
        let waves = self.wave_list(v);
        let (mut seen, mut todo) = (0u8, modulators);
        while todo & !seen != 0 {
            let id = (todo & !seen).trailing_zeros() as u8;
            if id == carrier {
                return true;
            }
            seen |= 1 << id;
            todo |= waves.iter().find(|w| w.id == id).map_or(0, |w| w.fm);
        }
        false
    }

    /// The waves of voice `v`, in the order they were made.
    pub(crate) fn wave_list(&self, v: VoiceId) -> &[WaveRepr] {
        self.voices.get(&v).map_or(&[], |r| r.waves.as_slice())
    }
}

impl CmdProcessor {
    fn wave_repr_mut(&mut self, voice: VoiceId, id: u8) -> Option<&mut WaveRepr> {
        self.engine_state
            .voices
            .get_mut(&voice)?
            .waves
            .iter_mut()
            .find(|w| w.id == id)
    }

    /// Adds a wave called `name` to the voice, in the mirror, and returns the
    /// command. The id is the lowest free one.
    pub(crate) fn make_wave(
        &mut self,
        voice: VoiceId,
        name: &str,
        shape: WaveShape,
        params: [f32; WAVE_PARAMS],
        retrig: bool,
    ) -> CmdResult<Command> {
        let repr = self
            .engine_state
            .voices
            .get_mut(&voice)
            .ok_or_else(|| CmdErr::NoItem {
                ty: "voice".to_string(),
                name: voice.0.to_string(),
            })?;
        let id = (0..MAX_WAVES as u8)
            .find(|id| !repr.waves.iter().any(|w| w.id == *id))
            .ok_or(CmdErr::LimitReached {
                kind: "waves",
                limit: MAX_WAVES as u32,
            })?;
        repr.waves.push(WaveRepr {
            name: name.to_string(),
            id,
            shape,
            params,
            retrig,
            fm: 0,
            ratio_text: None,
            out: true,
            ratio_mode: false,
            bypass: None,
            from: None,
            slices: DEFAULT_TABLE_SLICES,
            frames: DEFAULT_TABLE_FRAMES,
        });
        Ok(Command::Wave(WaveArgs {
            voice,
            id,
            op: WaveOp::Insert {
                shape,
                params,
                retrig,
            },
        }))
    }

    /// Sets a parameter in the mirror and returns the command; `frames > 0`
    /// is a glide to `value`.
    pub(crate) fn set_wave_param(
        &mut self,
        voice: VoiceId,
        id: u8,
        param: usize,
        value: f32,
        frames: u32,
    ) -> Command {
        if let Some(r) = self.wave_repr_mut(voice, id) {
            r.params[param] = value;
        }
        Command::Wave(WaveArgs {
            voice,
            id,
            op: WaveOp::Set {
                param: param as u8,
                value,
                frames,
            },
        })
    }

    /// Changes what a wave plays, in the mirror, and returns the command.
    pub(crate) fn set_wave_shape(&mut self, voice: VoiceId, id: u8, shape: WaveShape) -> Command {
        if let Some(r) = self.wave_repr_mut(voice, id) {
            r.shape = shape;
        }
        Command::Wave(WaveArgs {
            voice,
            id,
            op: WaveOp::Shape(shape),
        })
    }

    /// Records what a table wave is cut from, in the mirror, without a table
    /// (its buffer was empty).
    pub(crate) fn set_wave_source(
        &mut self,
        voice: VoiceId,
        id: u8,
        from: WaveFrom,
        (slices, frames): (u32, u32),
    ) {
        if let Some(r) = self.wave_repr_mut(voice, id) {
            r.from = Some(from);
            r.slices = slices;
            r.frames = frames;
        }
    }

    /// Gives a table wave its table, in the mirror, and returns the command.
    pub(crate) fn set_wave_table(
        &mut self,
        voice: VoiceId,
        id: u8,
        from: WaveFrom,
        (slices, frames): (u32, u32),
        table: Arc<WaveTable>,
    ) -> Command {
        if let Some(r) = self.wave_repr_mut(voice, id) {
            r.from = Some(from);
            r.slices = slices;
            r.frames = frames;
        }
        Command::Wave(WaveArgs {
            voice,
            id,
            op: WaveOp::Table(table),
        })
    }

    /// Makes `carrier` follow the waves in the bitmask `modulators` (0:
    /// nothing), in the mirror, and returns the command.
    pub(crate) fn set_wave_fm(&mut self, voice: VoiceId, carrier: u8, modulators: u8) -> Command {
        if let Some(r) = self.wave_repr_mut(voice, carrier) {
            r.fm = modulators;
        }
        Command::Wave(WaveArgs {
            voice,
            id: carrier,
            op: WaveOp::Fm(modulators),
        })
    }

    /// Keeps the text `ratio` was typed as (or clears it), in the mirror.
    pub(crate) fn set_wave_ratio_text(&mut self, voice: VoiceId, id: u8, text: Option<String>) {
        if let Some(r) = self.wave_repr_mut(voice, id) {
            r.ratio_text = text;
        }
    }

    /// Sets whether a wave is heard, in the mirror, and returns the command.
    pub(crate) fn set_wave_out(&mut self, voice: VoiceId, id: u8, out: bool) -> Command {
        if let Some(r) = self.wave_repr_mut(voice, id) {
            r.out = out;
        }
        Command::Wave(WaveArgs {
            voice,
            id,
            op: WaveOp::Out(out),
        })
    }

    /// Sets whether a wave's frequency is `ratio` times the voice's note, in
    /// the mirror, and returns the command.
    pub(crate) fn set_wave_ratio_mode(&mut self, voice: VoiceId, id: u8, on: bool) -> Command {
        if let Some(r) = self.wave_repr_mut(voice, id) {
            r.ratio_mode = on;
        }
        Command::Wave(WaveArgs {
            voice,
            id,
            op: WaveOp::RatioMode(on),
        })
    }

    /// Sets whether a wave retriggers, in the mirror, and returns the
    /// command.
    pub(crate) fn set_wave_retrig(&mut self, voice: VoiceId, id: u8, retrig: bool) -> Command {
        if let Some(r) = self.wave_repr_mut(voice, id) {
            r.retrig = retrig;
        }
        Command::Wave(WaveArgs {
            voice,
            id,
            op: WaveOp::Retrig(retrig),
        })
    }

    /// Silences a wave or brings it back, in the mirror, and returns the
    /// command (`state`: `Some` when it is silenced afterwards).
    pub(crate) fn bypass_wave(
        &mut self,
        voice: VoiceId,
        id: u8,
        state: Option<Bypass>,
        clear: bool,
    ) -> Command {
        if let Some(r) = self.wave_repr_mut(voice, id) {
            r.bypass = state;
        }
        Command::Wave(WaveArgs {
            voice,
            id,
            op: WaveOp::Bypass {
                on: state.is_some(),
                clear,
            },
        })
    }

    /// Takes a wave out, in the mirror, and returns the command; its id is
    /// free again.
    pub(crate) fn remove_wave(&mut self, voice: VoiceId, id: u8) -> Command {
        if let Some(r) = self.engine_state.voices.get_mut(&voice) {
            r.waves.retain(|w| w.id != id);
            // whatever it modulated plays plain, as in the engine
            for w in &mut r.waves {
                w.fm &= !(1 << id);
            }
            self.engine_state.freed_waves.push((voice, id));
        }
        Command::Wave(WaveArgs {
            voice,
            id,
            op: WaveOp::Remove,
        })
    }
}
