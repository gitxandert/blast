//! A glideable parameter, shared by effects and waves.

/// One parameter: its base value and the glide toward a new one (a straight
/// line, ending exactly on the target, as `VoiceState::set_param`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Param {
    pub(crate) base: f32,
    target: f32,
    step: f32,
    left: u32,
    // changes whenever the parameter is set or starts a glide (`thenif`
    // compares it)
    pub(crate) generation: u32,
}

impl Param {
    pub(crate) const fn new(base: f32) -> Self {
        Self {
            base,
            target: base,
            step: 0.0,
            left: 0,
            generation: 0,
        }
    }

    // one frame of the glide; true while it has more
    pub(crate) fn advance(&mut self) -> bool {
        if self.left == 0 {
            return false;
        }
        self.left -= 1;
        self.base = match self.left {
            0 => self.target,
            _ => self.base + self.step,
        };
        self.left > 0
    }

    /// Sets the (already clamped) value `v`. With `frames > 0` it glides
    /// there in a straight line from where it is, replacing any glide in
    /// progress. Returns the frames the glide takes (0 if it jumped).
    pub(crate) fn set(&mut self, v: f32, frames: u32) -> u32 {
        let generation = self.generation.wrapping_add(1);
        if frames == 0 {
            *self = Self::new(v);
            self.generation = generation;
            return 0;
        }
        *self = Self {
            base: self.base,
            target: v,
            step: (v - self.base) / frames as f32,
            left: frames,
            generation,
        };
        frames
    }
}
