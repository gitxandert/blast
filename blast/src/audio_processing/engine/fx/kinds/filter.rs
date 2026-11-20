//! The filter: a biquad (lowpass, highpass, bandpass, notch) with a smoothed
//! cutoff and a `base` blend of the part it takes out.

use super::biquad::{Biquad, Shape};
use super::{Effect, FxSpec, FxType};
use crate::audio_processing::commands::lang::schema::{
    FX_GAIN, FX_MIX, SettingSpec, SettingType, UNIT, setting,
};

const SETTINGS: &[SettingSpec] = &[
    setting(
        "type",
        't',
        SettingType::Word(&["lp", "hp", "bp", "notch"]),
        "lp",
        "lowpass, highpass, bandpass or notch; changing it restarts the filter",
        None,
    ),
    setting(
        "cutoff",
        'c',
        SettingType::Number {
            min: 20.0,
            max: 20_000.0,
        },
        "1000",
        "corner or centre frequency in Hz",
        None,
    )
    .glides(),
    setting(
        "q",
        'q',
        SettingType::Number { min: 0.1, max: 24.0 },
        "0.707",
        "resonance",
        None,
    )
    .glides(),
    setting(
        "base",
        'b',
        UNIT,
        "0",
        "how loud the part the filter takes out stays: linear or decibels (-12db); 1 = the filter does nothing",
        None,
    )
    .glides()
    .db(),
    FX_MIX,
    FX_GAIN,
];

/// The filter's memory: the coefficients and per-channel state, the smoothed
/// cutoff, and what the coefficients were last computed for.
#[derive(Clone, Copy, Debug)]
pub struct Filter {
    biquad: Biquad,
    // NaN = nothing yet, so the next frame computes them
    cutoff: f32,
    computed: (f32, f32),
}

impl Effect for Filter {
    const SPEC: &'static FxSpec = &FxSpec::new(
        FxType::Filter,
        "filter",
        "a filter",
        "a voice effect: a biquad filter, run in the order written; `voice.NAME cutoff=2000 over 4` edits it; stop and start bypass it; base= makes shelves and bells",
        &["lp", "hp", "bp", "notch"],
        SETTINGS,
        &["cutoff"],
    );

    fn new() -> Self {
        Self {
            biquad: Biquad::new(),
            cutoff: f32::NAN,
            computed: (f32::NAN, f32::NAN),
        }
    }

    fn run(&mut self, variant: u8, p: &[f32], frame: &mut [f32], sr: f32) {
        let shape = match variant {
            1 => Shape::Highpass,
            2 => Shape::Bandpass,
            3 => Shape::Notch,
            _ => Shape::Lowpass,
        };
        let (target, q, base) = (p[0], p[1], p[2]);
        // a ~2 ms one-pole, so glides and modulation don't zipper
        if self.cutoff.is_nan() {
            self.cutoff = target;
        } else {
            self.cutoff += (target - self.cutoff) * (1.0 - (-1.0 / (0.002 * sr)).exp());
            if (self.cutoff - target).abs() < target * 1e-4 {
                self.cutoff = target;
            }
        }
        if (self.cutoff, q) != self.computed {
            self.biquad.set(shape, self.cutoff, q, sr);
            self.computed = (self.cutoff, q);
        }
        for (c, s) in frame.iter_mut().enumerate() {
            let x = *s;
            let f = self.biquad.run(c, x);
            *s = base * x + (1.0 - base) * f;
        }
    }
}
