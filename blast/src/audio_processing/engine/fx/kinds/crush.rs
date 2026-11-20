//! The crush: bit depth and sample-rate reduction.

use super::{Effect, FxSpec, FxType};
use crate::audio_processing::commands::lang::schema::{
    FX_GAIN, FX_MIX, SettingSpec, SettingType, setting,
};
use crate::audio_processing::engine::voice::MAX_VOICE_CHANNELS;

const SETTINGS: &[SettingSpec] = &[
    setting(
        "bits",
        'b',
        SettingType::Number {
            min: 1.0,
            max: 24.0,
        },
        "8",
        "bit depth (fractions allowed)",
        None,
    )
    .glides(),
    setting(
        "down",
        'n',
        SettingType::Int { min: 1, max: 64 },
        "1",
        "hold each sample this many frames",
        None,
    ),
    FX_MIX,
    FX_GAIN,
];

/// Crush memory: the sample being held and how long it has been.
#[derive(Clone, Copy, Debug)]
pub struct Crush {
    held: [f32; MAX_VOICE_CHANNELS],
    count: u32,
}

impl Crush {
    /// Starts a frame: takes a new sample every `down` frames.
    #[inline]
    fn tick(&mut self, down: u32) -> bool {
        if self.count >= down {
            self.count = 0;
        }
        let fresh = self.count == 0;
        self.count += 1;
        fresh
    }

    /// The crushed value of channel `c`; `fresh` (from `tick`) quantises a
    /// new sample, otherwise the held one repeats. `bits` may be fractional.
    #[inline]
    fn crush(&mut self, c: usize, x: f32, bits: f32, fresh: bool) -> f32 {
        if fresh {
            let levels = (bits - 1.0).exp2();
            self.held[c] = (x * levels).round().clamp(-levels, levels - 1.0) / levels;
        }
        self.held[c]
    }
}

impl Effect for Crush {
    const SPEC: &'static FxSpec = &FxSpec::new(
        FxType::Crush,
        "crush",
        "a crush",
        "a voice effect: bit depth and sample-rate reduction, run in the order written; `voice.NAME bits=4` edits it",
        &[],
        SETTINGS,
        &[],
    );

    fn new() -> Self {
        Self {
            held: [0.0; MAX_VOICE_CHANNELS],
            count: 0,
        }
    }

    fn run(&mut self, _variant: u8, p: &[f32], frame: &mut [f32], _sr: f32) {
        let bits = p[0];
        let down = p[1].round().max(1.0) as u32;
        let fresh = self.tick(down);
        for (c, s) in frame.iter_mut().enumerate() {
            *s = self.crush(c, *s, bits, fresh);
        }
    }
}
