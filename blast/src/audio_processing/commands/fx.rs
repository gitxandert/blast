//! Voice effects (FXa): the arguments of `Command::Fx`, the mirror of a
//! voice's chain, and the typed core the language lowers into. The mirror
//! decides the ids and when an insert must carry a bigger `Vec` (`room`);
//! the engine applies what arrives.

use super::errors::{CmdErr, CmdResult};
use super::reprs::{EngineState, GroupId, ProcOwner, VoiceId};
use super::{CmdProcessor, Command};
use crate::audio_processing::engine::{FX_PARAMS, FxKind, FxSlot};

/// Whose chain a command acts on: a voice's own, or a group's mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxOwner {
    Voice(VoiceId),
    Group(GroupId),
}

/// What a `Command::Fx` does to one effect.
pub enum FxOp {
    /// Appends an effect. `room` is a bigger empty `Vec`, sent when the chain
    /// is full; the engine moves the chain into it.
    Insert {
        kind: FxKind,
        params: [f32; FX_PARAMS],
        room: Option<Vec<FxSlot>>,
    },
    /// A filter changes type: it keeps its place and parameters, and its
    /// memory is cleared.
    Retype {
        kind: FxKind,
        params: [f32; FX_PARAMS],
    },
    /// Sets a parameter (an index among the kind's settings that aren't `type`), at once or as a
    /// glide of `frames`.
    Set {
        param: u8,
        value: f32,
        frames: u32,
    },
    /// `stop`: `(true, true)`; `pause`: `(true, false)`; `start`:
    /// `(false, true)`; `resume`: `(false, false)`.
    Bypass {
        on: bool,
        clear: bool,
    },
    Remove,
}

/// Arguments for `Command::Fx`.
pub struct FxArgs {
    pub owner: FxOwner,
    /// The effect, by its id within the owner (16 bits).
    pub id: u16,
    pub op: FxOp,
}

/// How a bypassed effect got that way (`stop` clears its memory, `pause`
/// keeps it); `state` prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bypass {
    Stopped,
    Paused,
}

/// An effect in the mirror.
#[derive(Clone)]
pub struct FxRepr {
    pub(crate) name: String,
    pub(crate) id: u16,
    pub(crate) kind: FxKind,
    /// Every parameter's value, as the engine will hold it (a glide's
    /// target), whether or not the kind uses it.
    pub(crate) params: [f32; FX_PARAMS],
    pub(crate) bypass: Option<Bypass>,
}

/// The most effects a voice can have: ids are 16 bits.
const MAX_FX: u32 = u16::MAX as u32 + 1;

impl EngineState {
    /// Whether `owner` has an effect called `name`.
    pub(crate) fn has_fx(&self, owner: ProcOwner, name: &str) -> bool {
        match owner {
            ProcOwner::Voice(v) => self
                .voices
                .get(&v)
                .is_some_and(|r| r.fx.iter().any(|f| f.name == name)),
            ProcOwner::Group(g) => self
                .groups
                .get(&g)
                .is_some_and(|r| r.fx.iter().any(|f| f.name == name)),
            ProcOwner::Session => false,
        }
    }
}

impl EngineState {
    /// The effects of an owner, in chain order (none if it doesn't exist).
    pub(crate) fn fx_list(&self, owner: FxOwner) -> &[FxRepr] {
        match owner {
            FxOwner::Voice(v) => self.voices.get(&v).map(|r| r.fx.as_slice()),
            FxOwner::Group(g) => self.groups.get(&g).map(|r| r.fx.as_slice()),
        }
        .unwrap_or(&[])
    }

    /// The effects and chain capacity of an owner, if it exists.
    pub(crate) fn fx_list_mut(&mut self, owner: FxOwner) -> Option<(&mut Vec<FxRepr>, &mut usize)> {
        match owner {
            FxOwner::Voice(v) => self.voices.get_mut(&v).map(|r| (&mut r.fx, &mut r.fx_cap)),
            FxOwner::Group(g) => self.groups.get_mut(&g).map(|r| (&mut r.fx, &mut r.fx_cap)),
        }
    }
}

impl CmdProcessor {
    // the owner's effect `id`, if both exist
    fn fx_repr_mut(&mut self, owner: FxOwner, id: u16) -> Option<&mut FxRepr> {
        self.engine_state
            .fx_list_mut(owner)?
            .0
            .iter_mut()
            .find(|f| f.id == id)
    }

    /// Appends an effect called `name` to the voice's chain, in the mirror,
    /// and returns the command. The id is the lowest free one; the command
    /// carries `room` when the chain is full.
    pub(crate) fn make_fx(
        &mut self,
        owner: FxOwner,
        name: &str,
        kind: FxKind,
        params: [f32; FX_PARAMS],
    ) -> CmdResult<Command> {
        let staged = self.engine_state.staging;
        let (list, cap) = self
            .engine_state
            .fx_list_mut(owner)
            .ok_or_else(|| match owner {
                FxOwner::Voice(v) => CmdErr::NoItem {
                    ty: "voice".to_string(),
                    name: v.0.to_string(),
                },
                FxOwner::Group(g) => CmdErr::NoItem {
                    ty: "group".to_string(),
                    name: g.0.to_string(),
                },
            })?;
        let id = (0..=u16::MAX)
            .find(|id| !list.iter().any(|f| f.id == *id))
            .ok_or(CmdErr::LimitReached {
                kind: "effects",
                limit: MAX_FX - 1,
            })?;
        // the engine's chain has room for `fx_cap` effects; a full one is
        // moved into a Vec with twice the room. An effect that waits always
        // brings room for the whole mirror list and leaves `fx_cap` alone: if
        // its line is dropped the engine's chain did not grow, and a later
        // insert (which sees a list longer than `fx_cap`) brings its own.
        let room = (staged || list.len() >= *cap).then(|| {
            let room = (*cap * 2).max(list.len() + 1).max(1);
            if !staged {
                *cap = room;
            }
            Vec::with_capacity(room)
        });
        list.push(FxRepr {
            name: name.to_string(),
            id,
            kind,
            params,
            bypass: None,
        });
        Ok(Command::Fx(FxArgs {
            owner,
            id,
            op: FxOp::Insert { kind, params, room },
        }))
    }

    /// Sets a parameter in the mirror and returns the command; `frames > 0`
    /// is a glide to `value`.
    pub(crate) fn set_fx_param(
        &mut self,
        owner: FxOwner,
        id: u16,
        param: usize,
        value: f32,
        frames: u32,
    ) -> Command {
        if let Some(r) = self.fx_repr_mut(owner, id) {
            r.params[param] = value;
        }
        Command::Fx(FxArgs {
            owner,
            id,
            op: FxOp::Set {
                param: param as u8,
                value,
                frames,
            },
        })
    }

    /// Switches a filter to another type, in the mirror, and returns the
    /// command. The parameters it carries are the ones the filter has now;
    /// settings that came with the type change follow as `Set`s.
    pub(crate) fn retype_fx(&mut self, owner: FxOwner, id: u16, kind: FxKind) -> Command {
        let mut params = [0.0; FX_PARAMS];
        if let Some(r) = self.fx_repr_mut(owner, id) {
            r.kind = kind;
            params = r.params;
        }
        Command::Fx(FxArgs {
            owner,
            id,
            op: FxOp::Retype { kind, params },
        })
    }

    /// Bypasses an effect or brings it back, in the mirror, and returns the
    /// command (`state`: `Some` when it is bypassed afterwards).
    pub(crate) fn bypass_fx(
        &mut self,
        owner: FxOwner,
        id: u16,
        state: Option<Bypass>,
        clear: bool,
    ) -> Command {
        if let Some(r) = self.fx_repr_mut(owner, id) {
            r.bypass = state;
        }
        Command::Fx(FxArgs {
            owner,
            id,
            op: FxOp::Bypass {
                on: state.is_some(),
                clear,
            },
        })
    }

    /// Takes an effect out of the chain, in the mirror, and returns the
    /// command. The ones after it move up; its id is free again.
    pub(crate) fn remove_fx(&mut self, owner: FxOwner, id: u16) -> Command {
        if let Some((list, _)) = self.engine_state.fx_list_mut(owner) {
            list.retain(|f| f.id != id);
            self.engine_state.freed_fx.push((owner, id));
        }
        Command::Fx(FxArgs {
            owner,
            id,
            op: FxOp::Remove,
        })
    }
}
