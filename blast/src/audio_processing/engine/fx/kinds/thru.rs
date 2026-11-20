//! A test-only effect: a pass-through with one setting, `amt`. It proves that
//! adding an effect is this file and one line in `fx_kinds!`.

use super::{Effect, FxSpec, FxType};
use crate::audio_processing::commands::lang::schema::{
    FX_GAIN, FX_MIX, SettingSpec, SettingType, setting,
};

const SETTINGS: &[SettingSpec] = &[
    setting(
        "amt",
        'a',
        SettingType::Number { min: 0.0, max: 1.0 },
        "0",
        "an amount that does nothing",
        None,
    )
    .glides(),
    FX_MIX,
    FX_GAIN,
];

#[derive(Clone, Copy, Debug)]
pub struct Thru;

impl Effect for Thru {
    const SPEC: &'static FxSpec = &FxSpec::new(
        FxType::Thru,
        "thru",
        "a thru",
        "a test effect: passes the signal through",
        &[],
        SETTINGS,
        &[],
    );

    fn new() -> Self {
        Thru
    }

    fn run(&mut self, _variant: u8, _p: &[f32], _frame: &mut [f32], _sr: f32) {}
}
