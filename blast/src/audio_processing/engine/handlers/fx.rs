//! Effect commands (FXa).

use crate::audio_processing::commands::*;
use crate::audio_processing::engine::{Conductor, Garbage};

impl Conductor {
    /// Applies an op to one effect of a voice's chain. Nothing allocates:
    /// an insert's bigger `Vec` was built on the command thread, and the old
    /// one leaves as garbage.
    pub(crate) fn fx(&mut self, args: FxArgs) {
        let (chain, active) = match args.owner {
            FxOwner::Voice(v) => match self.voices.get_mut(v.index()) {
                Some(voice) => (&mut voice.fx, voice.state.active),
                None => {
                    debug_assert!(false, "effect command for a missing voice {v:?}");
                    return;
                }
            },
            FxOwner::Group(g) => match self.groups.get_mut(g.index()) {
                Some(group) => (&mut group.fx, group.state.active),
                None => {
                    debug_assert!(false, "effect command for a missing group {g:?}");
                    return;
                }
            },
        };
        let id = args.id;
        match args.op {
            FxOp::Insert { kind, params, room } => {
                if let Some(old) = chain.insert(id, kind, params, room) {
                    self.retire(Garbage::FxChain(old));
                }
            }
            FxOp::Retype { kind, params } => chain.retype(id, kind, params),
            FxOp::Set {
                param,
                value,
                frames,
            } => {
                // an owner that isn't running can't glide (nothing advances it)
                let frames = if active { frames } else { 0 };
                let used = chain.set(id, param, value, frames);
                self.note_glide(used);
            }
            FxOp::Bypass { on, clear } => chain.bypass(id, on, clear),
            FxOp::Remove => chain.remove(id),
        }
    }
}
