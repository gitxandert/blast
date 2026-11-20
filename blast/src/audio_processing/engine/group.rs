//! Groups: Voices played and sequenced together. A grouped Voice stays in
//! the voice arena; its Group holds its ID.

use crate::audio_processing::commands::{TempoId, VoiceId};
use crate::audio_processing::engine::ModOp;
use crate::audio_processing::engine::fx::FxChain;

/// A Group's transport, gain and tempo.
pub struct GroupState {
    pub active: bool,
    pub gain: f32,
    /// Its own tempo or a context's.
    pub tempo: TempoId,
    /// Changes whenever the gain is set or starts a glide (`thenif`
    /// compares it).
    pub generation: u32,
    // a gain glide in progress (`over`): `step` per frame for `left` more
    // frames, ending exactly on `target`
    target: f32,
    step: f32,
    left: u32,
}

impl GroupState {
    /// Sets the gain its members are scaled by. With `frames > 0` and the
    /// group playing, it glides there in a straight line; a group that isn't
    /// playing can't glide (nothing advances it), so it jumps. Returns the
    /// frames the glide really takes (0 if it jumped).
    pub fn set_gain(&mut self, gain: f32, frames: u32) -> u32 {
        if !gain.is_finite() {
            return 0;
        }
        self.generation = self.generation.wrapping_add(1);
        if frames == 0 || !self.active {
            self.gain = gain;
            self.left = 0;
            return 0;
        }
        self.target = gain;
        self.step = (gain - self.gain) / frames as f32;
        self.left = frames;
        frames
    }

    /// Moves a gain glide one frame; a finished one lands exactly on target.
    pub fn advance(&mut self) {
        if self.left == 0 {
            return;
        }
        self.left -= 1;
        self.gain = if self.left == 0 {
            self.target
        } else {
            self.gain + self.step
        };
    }
}

/// Voices that start, stop and are sequenced together.
pub struct Group {
    pub state: GroupState,
    /// In command-line order. Moved in from `GroupArgs` with capacity
    /// `Limits::group_members`, so it never grows here.
    pub members: Vec<VoiceId>,
    /// Effects on the group's mix.
    pub fx: FxChain,
    /// The buffers recording the group's bus (slots).
    pub(crate) taps: Vec<u32>,
}

impl Group {
    pub(crate) fn new(tempo: TempoId, members: Vec<VoiceId>) -> Self {
        Self {
            state: GroupState {
                active: false,
                gain: 1.0,
                tempo,
                generation: 0,
                target: 1.0,
                step: 0.0,
                left: 0,
            },
            members,
            fx: FxChain::new(),
            taps: Vec::new(),
        }
    }

    /// Adds this frame's modulation to one parameter of one of the group's
    /// effects.
    pub fn modulate_fx(&mut self, id: u16, param: u8, op: ModOp) {
        self.fx.modulate(id, param, op);
    }
}
