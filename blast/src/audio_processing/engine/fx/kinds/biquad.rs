//! A biquad filter (RBJ cookbook), transposed direct form II, one memory per
//! channel.

use crate::audio_processing::engine::voice::MAX_VOICE_CHANNELS;

/// Which response the coefficients give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Lowpass,
    Highpass,
    /// Constant 0 dB peak.
    Bandpass,
    Notch,
}

/// A filter's coefficients and per-channel memory.
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: [f32; MAX_VOICE_CHANNELS],
    z2: [f32; MAX_VOICE_CHANNELS],
}

impl Biquad {
    /// A filter that passes its input until `set` gives it a response.
    pub const fn new() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: [0.0; MAX_VOICE_CHANNELS],
            z2: [0.0; MAX_VOICE_CHANNELS],
        }
    }

    /// Computes the coefficients for `shape` at `cutoff` Hz and `q`, for
    /// sample rate `sr`. The cutoff is kept below 0.45 of the sample rate.
    pub fn set(&mut self, shape: Shape, cutoff: f32, q: f32, sr: f32) {
        let cutoff = cutoff.clamp(1.0, 0.45 * sr);
        let w0 = std::f32::consts::TAU * cutoff / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q.max(0.01));
        let (b0, b1, b2) = match shape {
            Shape::Lowpass => ((1.0 - cos) * 0.5, 1.0 - cos, (1.0 - cos) * 0.5),
            Shape::Highpass => ((1.0 + cos) * 0.5, -(1.0 + cos), (1.0 + cos) * 0.5),
            Shape::Bandpass => (alpha, 0.0, -alpha),
            Shape::Notch => (1.0, -2.0 * cos, 1.0),
        };
        let a0 = 1.0 + alpha;
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = -2.0 * cos / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    /// Filters one sample of channel `c` (below `MAX_VOICE_CHANNELS`).
    #[inline]
    pub fn run(&mut self, c: usize, x: f32) -> f32 {
        let y = self.b0 * x + self.z1[c];
        self.z1[c] = self.b1 * x - self.a1 * y + self.z2[c];
        self.z2[c] = self.b2 * x - self.a2 * y;
        y
    }
}

impl Default for Biquad {
    fn default() -> Self {
        Self::new()
    }
}
