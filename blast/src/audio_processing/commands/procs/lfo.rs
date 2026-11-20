//! `lfo`: a periodic value added to one parameter of a voice or group.

use crate::audio_processing::blast_rand::{X128P, fast_seed};
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::errors::{CmdErr, CmdResult};
use crate::audio_processing::commands::process::{ProcRepr, SeqRepr};
use crate::audio_processing::commands::procs::seq::FeelEdit;
use crate::audio_processing::commands::reprs::{ProcId, ProcOwner, TempoId, TempoLink, alloc};
use crate::audio_processing::commands::tempo::TempoUse;
use crate::audio_processing::commands::{BatchArgs, CmdProcessor, Command};
use crate::audio_processing::processes::{LfoRate, LfoShape, LfoTarget};

/// Arguments for `Command::Lfo`.
pub struct LfoArgs {
    pub proc: ProcId,
    pub owner: ProcOwner,
    /// The clock of its owner (a clock-locked lfo reads it).
    pub tempo: TempoUse,
    pub shape: LfoShape,
    pub target: LfoTarget,
    pub rate: LfoRate,
    /// Where in the cycle it starts, 0..1.
    pub phase: f32,
    pub depth: f32,
    pub offset: f32,
    pub rng: X128P,
    /// Frames per second, for a free-running lfo.
    pub sample_rate: f32,
}

/// Arguments for `Command::LfoEdit`: what to change in an lfo, without
/// removing it.
pub struct LfoEditArgs {
    pub id: ProcId,
    pub depth: Option<FeelEdit>,
    pub offset: Option<FeelEdit>,
    /// A glide of the speed within its kind (Hz, or ticks per cycle).
    pub freq: Option<FeelEdit>,
    pub shape: Option<LfoShape>,
    pub target: Option<LfoTarget>,
    /// The other kind of speed: the cycle starts over at `phase`.
    pub rate: Option<LfoRate>,
    pub phase: Option<f32>,
    /// Re-seeds `sh`.
    pub rng: Option<X128P>,
}

/// What the mirror keeps of an lfo, for `state` and the no-op check.
#[derive(Clone, Debug, PartialEq)]
pub struct LfoRepr {
    pub(crate) shape: LfoShape,
    pub(crate) target: LfoTarget,
    /// The target as written (`pan`, `lo.cutoff`).
    pub(crate) target_text: String,
    pub(crate) depth: f32,
    pub(crate) offset: f32,
    pub(crate) phase: f32,
    pub(crate) seed: Option<u64>,
    pub(crate) rate: LfoRate,
    /// The speed as written (`4b`, `2`, `5hz`); empty for the default.
    pub(crate) freq_text: String,
    pub(crate) paused: bool,
    pub(crate) stopped: bool,
}

/// An lfo to make: the language's `lfo` statement, lowered.
pub(crate) struct NewLfo {
    pub shape: LfoShape,
    pub target: LfoTarget,
    pub target_text: String,
    pub depth: f32,
    pub offset: f32,
    pub phase: f32,
    pub seed: Option<u64>,
    pub rate: LfoRate,
    pub freq_text: String,
}

/// What a restated lfo changes (`edit_lfo`).
#[derive(Default)]
pub(crate) struct LfoChange {
    /// New values, with the frames of their glides.
    pub depth: Option<(f32, u32)>,
    pub offset: Option<(f32, u32)>,
    pub freq: Option<(f32, u32)>,
    pub freq_text: Option<String>,
    pub shape: Option<LfoShape>,
    pub target: Option<(LfoTarget, String)>,
    /// The other kind of speed.
    pub rate: Option<LfoRate>,
    pub phase: Option<f32>,
    pub seed: Option<u64>,
}

impl CmdProcessor {
    /// Creates an lfo called `name` on `owner`, a voice or a group, reading
    /// the owner's clock `tempo` (linked by `link`).
    pub(in crate::audio_processing::commands) fn make_lfo(
        &mut self,
        owner: ProcOwner,
        name: &str,
        new: &NewLfo,
        tempo: TempoId,
        link: TempoLink,
    ) -> CmdResult<Command> {
        let es = &self.engine_state;
        let Some(procs) = es.owner_procs(owner) else {
            return Err(CmdErr::NoItem {
                ty: "Owner".to_string(),
                name: name.to_string(),
            });
        };
        let member = match owner {
            ProcOwner::Group(g) => es
                .groups
                .get(&g)
                .is_some_and(|g| g.member_names.contains_key(name)),
            ProcOwner::Voice(_) | ProcOwner::Session => false,
        };
        if procs
            .get(name)
            .is_some_and(|p| !es.hidden(crate::audio_processing::commands::AnyId::Proc(*p)))
            || member
        {
            return Err(CmdErr::AlreadyIs {
                ty: "Process".to_string(),
                name: name.to_string(),
            });
        }

        let proc = alloc(&mut self.engine_state.ids.procs, "processes")?;
        let es = &mut self.engine_state;
        es.procs.insert(
            proc,
            ProcRepr {
                id: proc,
                name: name.to_string(),
                owner,
                tempo,
                link,
                enabled: true,
                packet: None,
                seq: SeqRepr::default(),
                env: None,
                lfo: Some(LfoRepr {
                    shape: new.shape,
                    target: new.target,
                    target_text: new.target_text.clone(),
                    depth: new.depth,
                    offset: new.offset,
                    phase: new.phase,
                    seed: new.seed,
                    rate: new.rate,
                    freq_text: new.freq_text.clone(),
                    paused: false,
                    stopped: false,
                }),
            },
        );
        if let Some(procs) = es.owner_procs_mut(owner) {
            procs.insert(name.to_string(), proc);
        }
        Ok(Command::Lfo(LfoArgs {
            proc,
            owner,
            tempo: TempoUse::Shared(tempo),
            shape: new.shape,
            target: new.target,
            rate: new.rate,
            phase: new.phase,
            depth: new.depth,
            offset: new.offset,
            rng: X128P::new(new.seed.unwrap_or_else(fast_seed)),
            sample_rate: sample_rate::get() as f32,
        }))
    }

    /// Changes an lfo in the mirror and returns the `LfoEdit` that does the
    /// same in the engine, without restarting it.
    pub(in crate::audio_processing::commands) fn edit_lfo(
        &mut self,
        id: ProcId,
        change: LfoChange,
    ) -> Command {
        let Some(lfo) = self
            .engine_state
            .procs
            .get_mut(&id)
            .and_then(|r| r.lfo.as_mut())
        else {
            return Command::Batch(BatchArgs { cmds: Vec::new() });
        };
        let edit = |v: (f32, u32)| FeelEdit {
            value: v.0,
            frames: v.1,
        };
        if let Some((v, _)) = change.depth {
            lfo.depth = v;
        }
        if let Some((v, _)) = change.offset {
            lfo.offset = v;
        }
        if let Some((v, _)) = change.freq {
            lfo.rate = match lfo.rate {
                LfoRate::Hz(_) => LfoRate::Hz(v),
                LfoRate::Clock { .. } => LfoRate::Clock { ticks: v },
            };
        }
        if let Some(rate) = change.rate {
            lfo.rate = rate;
        }
        if let Some(text) = &change.freq_text {
            lfo.freq_text = text.clone();
        }
        if let Some(shape) = change.shape {
            lfo.shape = shape;
        }
        if let Some((target, text)) = &change.target {
            lfo.target = *target;
            lfo.target_text = text.clone();
        }
        if let Some(phase) = change.phase {
            lfo.phase = phase;
        }
        if let Some(seed) = change.seed {
            lfo.seed = Some(seed);
        }
        Command::LfoEdit(LfoEditArgs {
            id,
            depth: change.depth.map(edit),
            offset: change.offset.map(edit),
            freq: change.freq.map(edit),
            shape: change.shape,
            target: change.target.map(|t| t.0),
            rate: change.rate,
            phase: change.phase,
            rng: change.seed.map(X128P::new),
        })
    }
}
