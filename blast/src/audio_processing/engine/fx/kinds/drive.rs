//! The drive: `tanh(g·x) / tanh(g)` saturation.

use super::{Effect, FxSpec, FxType};
use crate::audio_processing::commands::lang::schema::{
    FX_GAIN, FX_MIX, SettingSpec, SettingType, setting,
};

const SETTINGS: &[SettingSpec] = &[
    setting(
        "amount",
        'a',
        SettingType::Number {
            min: 0.0,
            max: 48.0,
        },
        "12",
        "decibels of gain into the shaper",
        None,
    )
    .glides(),
    FX_MIX,
    FX_GAIN,
];

/// The dB that `drive_gain` last saw, and the gain it gave.
#[derive(Clone, Copy, Debug)]
pub struct Drive {
    db: f32,
    gain: (f32, f32),
}

/// `tanh(g·x) / tanh(g)` with `g = 10^(db/20)`: a full-scale input stays at
/// full scale, and small signals are boosted by `g / tanh(g)`.
#[inline]
fn shape(x: f32, gain: f32, norm: f32) -> f32 {
    (gain * x).tanh() * norm
}

/// The linear gain into the shaper, and the factor that keeps full scale.
fn drive_gain(db: f32) -> (f32, f32) {
    let g = 10f32.powf(db / 20.0);
    (g, 1.0 / g.tanh().max(1e-6))
}

impl Effect for Drive {
    const SPEC: &'static FxSpec = &FxSpec::new(
        FxType::Drive,
        "drive",
        "a drive",
        "a voice effect: tanh saturation, run in the order written; `voice.NAME amount=24 over 4` edits it",
        &[],
        SETTINGS,
        &[],
    );

    fn new() -> Self {
        Self {
            db: f32::NAN,
            gain: (1.0, 1.0),
        }
    }

    fn run(&mut self, _variant: u8, p: &[f32], frame: &mut [f32], _sr: f32) {
        let db = p[0];
        if db != self.db {
            self.db = db;
            self.gain = drive_gain(db);
        }
        let (g, norm) = self.gain;
        for s in frame.iter_mut() {
            *s = shape(*s, g, norm);
        }
    }
}
