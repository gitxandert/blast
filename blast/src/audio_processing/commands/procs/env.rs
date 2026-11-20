//! `env`: an envelope on one parameter of a voice or group, fired by the
//! hits of its owner.

use crate::audio_processing::commands::errors::{CmdErr, CmdResult};
use crate::audio_processing::commands::process::{ProcRepr, SeqRepr};
use crate::audio_processing::commands::reprs::{ProcId, ProcOwner, TempoId, TempoLink, alloc};
use crate::audio_processing::commands::tempo::TempoUse;
use crate::audio_processing::commands::{BatchArgs, CmdProcessor, Command};
use crate::audio_processing::processes::{EnvCurve, EnvShape, LfoTarget};

/// Arguments for `Command::Env`.
pub struct EnvArgs {
    pub proc: ProcId,
    pub owner: ProcOwner,
    /// The clock of its owner (an env reads no clock, but a process slot
    /// has one).
    pub tempo: TempoUse,
    pub shape: EnvShape,
    pub target: LfoTarget,
    pub depth: f32,
}

/// Arguments for `Command::EnvEdit`: what to change in an env, without
/// removing it. A new shape takes effect at the next hit.
pub struct EnvEditArgs {
    pub id: ProcId,
    pub shape: Option<EnvShape>,
    pub target: Option<LfoTarget>,
    pub depth: Option<f32>,
}

/// The durations of an env as written (`200ms`, `0.5`, `hit`); empty for a
/// default. `state` prints them back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnvText {
    pub(crate) attack: String,
    pub(crate) decay: String,
    pub(crate) hold: String,
    pub(crate) release: String,
}

/// What the mirror keeps of an env, for `state` and the no-op check.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvRepr {
    /// In frames, as lowered.
    pub(crate) shape: EnvShape,
    pub(crate) text: EnvText,
    pub(crate) target: LfoTarget,
    /// The target as written (`gain`, `lo.cutoff`).
    pub(crate) target_text: String,
    pub(crate) depth: f32,
    pub(crate) paused: bool,
    pub(crate) stopped: bool,
}

/// An env to make: the language's `env` statement, lowered.
pub(crate) struct NewEnv {
    pub shape: EnvShape,
    pub text: EnvText,
    pub target: LfoTarget,
    pub target_text: String,
    pub depth: f32,
}

/// What a restated env changes (`edit_env`).
#[derive(Default)]
pub(crate) struct EnvChange {
    pub shape: Option<EnvShape>,
    pub text: Option<EnvText>,
    pub target: Option<(LfoTarget, String)>,
    pub depth: Option<f32>,
    /// Another spelling of the same target.
    pub target_text: Option<String>,
}

impl EnvChange {
    pub(crate) fn is_empty(&self) -> bool {
        self.shape.is_none()
            && self.text.is_none()
            && self.target.is_none()
            && self.depth.is_none()
            && self.target_text.is_none()
    }
}

impl CmdProcessor {
    /// Creates an env called `name` on `owner`, a voice or a group, on the
    /// owner's clock `tempo` (linked by `link`).
    pub(in crate::audio_processing::commands) fn make_env(
        &mut self,
        owner: ProcOwner,
        name: &str,
        new: &NewEnv,
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
                lfo: None,
                env: Some(EnvRepr {
                    shape: new.shape,
                    text: new.text.clone(),
                    target: new.target,
                    target_text: new.target_text.clone(),
                    depth: new.depth,
                    paused: false,
                    stopped: false,
                }),
            },
        );
        if let Some(procs) = es.owner_procs_mut(owner) {
            procs.insert(name.to_string(), proc);
        }
        Ok(Command::Env(EnvArgs {
            proc,
            owner,
            tempo: TempoUse::Shared(tempo),
            shape: new.shape,
            target: new.target,
            depth: new.depth,
        }))
    }

    /// Changes an env in the mirror and returns the `EnvEdit` that does the
    /// same in the engine.
    pub(in crate::audio_processing::commands) fn edit_env(
        &mut self,
        id: ProcId,
        change: EnvChange,
    ) -> Command {
        let Some(env) = self
            .engine_state
            .procs
            .get_mut(&id)
            .and_then(|r| r.env.as_mut())
        else {
            return Command::Batch(BatchArgs { cmds: Vec::new() });
        };
        if let Some(shape) = change.shape {
            env.shape = shape;
        }
        if let Some(text) = change.text {
            env.text = text;
        }
        if let Some((target, text)) = &change.target {
            env.target = *target;
            env.target_text = text.clone();
        }
        if let Some(text) = &change.target_text {
            env.target_text = text.clone();
        }
        if let Some(depth) = change.depth {
            env.depth = depth;
        }
        Command::EnvEdit(EnvEditArgs {
            id,
            shape: change.shape,
            target: change.target.map(|t| t.0),
            depth: change.depth,
        })
    }
}

/// The word for a curve, as written.
pub(crate) fn curve_word(curve: EnvCurve) -> &'static str {
    match curve {
        EnvCurve::Lin => "lin",
        EnvCurve::Exp => "exp",
    }
}
