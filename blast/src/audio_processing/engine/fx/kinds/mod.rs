//! The effects, and the registry that makes each one a single file.
//!
//! # Adding an effect
//!
//! 1. Write `kinds/NAME.rs`: a `Copy` struct that holds the effect's memory,
//!    and `impl Effect for NAME` with its `SPEC` (the language word, the
//!    article, the doc, the settings and their types) and its `run`.
//! 2. Add `mod NAME;` below and one line to `fx_kinds!`: `Name => name::Name,`.
//!
//! That is all. The schema, `help`, the card, completion, `state`, LFO and
//! envelope targets, lowering and the engine's dispatch read the registry.
//! `thru.rs` (test only) is the proof: it is a whole effect and nothing outside
//! its file and its macro line knows about it.
//!
//! A setting's range is its `SettingType` bounds, its default is the
//! `default` string, and its parameter index is its position among the
//! settings that are not `type`. Every effect ends with `FX_MIX` and
//! `FX_GAIN`; the chain applies them, so `run` only produces the wet frame.

use crate::audio_processing::commands::lang::ast::Kind;
use crate::audio_processing::commands::lang::schema::{
    BlockUse, KindSchema, Positional, Req, SettingSpec, SettingType,
};

pub(super) mod biquad;
mod crush;
mod drive;
mod filter;
#[cfg(test)]
mod thru;

use super::FX_PARAMS;

/// One effect: its memory and what it does to a frame.
pub trait Effect: Copy + std::fmt::Debug {
    /// The language word, article, doc, settings and types.
    const SPEC: &'static FxSpec;
    /// Fresh memory (a `start`, a retype, a voice that starts over).
    fn new() -> Self;
    /// Turns the dry frame into the wet frame in place. `p` holds the
    /// effective value of each of this effect's own settings, in the order
    /// written (mix and gain are not in it: the chain applies them).
    /// `variant` is the position of the `type=` word in `SPEC.types`.
    fn run(&mut self, variant: u8, p: &[f32], frame: &mut [f32], sr: f32);
}

/// What the language and the engine know about an effect.
pub struct FxSpec {
    pub word: &'static str,
    pub article: &'static str,
    pub doc: &'static str,
    /// `type=` words, the first is the default; empty if the effect has none.
    pub types: &'static [&'static str],
    /// Every setting, in schema order: `type` first if `types` isn't empty,
    /// then the effect's own, then `FX_MIX` and `FX_GAIN`.
    pub settings: &'static [SettingSpec],
    /// Settings whose modulation moves in octaves (filter: `cutoff`).
    pub octaves: &'static [&'static str],
    pub schema: KindSchema,
}

impl FxSpec {
    pub const fn new(
        ty: FxType,
        word: &'static str,
        article: &'static str,
        doc: &'static str,
        types: &'static [&'static str],
        settings: &'static [SettingSpec],
        octaves: &'static [&'static str],
    ) -> Self {
        Self {
            word,
            article,
            doc,
            types,
            settings,
            octaves,
            schema: KindSchema {
                kind: Kind::Fx(ty),
                doc,
                from: Req::No,
                on: Req::No,
                period: Req::No,
                positional: Positional::No,
                settings,
                block: BlockUse::No,
                pipe: false,
                built: None,
                update_built: None,
            },
        }
    }
}

/// Generates `FxType`, `spec`, `FxMem` from the list of effects.
macro_rules! fx_kinds {
    ($($(#[$m:meta])* $v:ident => $t:path),* $(,)?) => {
        /// An effect, by registry entry.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum FxType {
            $($(#[$m])* $v),*
        }

        impl FxType {
            /// Every effect, in the order `help` lists them.
            pub const ALL: &'static [FxType] = &[$($(#[$m])* FxType::$v),*];
        }

        /// The declaration of an effect.
        pub fn spec(ty: FxType) -> &'static FxSpec {
            match ty {
                $($(#[$m])* FxType::$v => <$t as Effect>::SPEC),*
            }
        }

        /// What an effect remembers between frames.
        #[derive(Clone, Copy, Debug)]
        pub enum FxMem {
            $($(#[$m])* $v($t)),*
        }

        impl FxMem {
            /// Fresh memory for `ty`.
            pub fn new(ty: FxType) -> Self {
                match ty {
                    $($(#[$m])* FxType::$v => FxMem::$v(<$t as Effect>::new())),*
                }
            }

            /// Runs the effect: dry frame in, wet frame out.
            pub fn run(&mut self, variant: u8, p: &[f32], frame: &mut [f32], sr: f32) {
                match self {
                    $($(#[$m])* FxMem::$v(e) => e.run(variant, p, frame, sr)),*
                }
            }
        }
    };
}

fx_kinds! {
    Filter => filter::Filter,
    Drive => drive::Drive,
    Crush => crush::Crush,
    #[cfg(test)]
    Thru => thru::Thru,
}

/// The settings that become parameters: everything but `type`. The last two
/// are mix and gain.
pub fn own_settings(ty: FxType) -> &'static [SettingSpec] {
    let s = spec(ty);
    &s.settings[usize::from(!s.types.is_empty())..]
}

/// How many parameters are the effect's own (before mix and gain).
pub fn own_count(ty: FxType) -> usize {
    own_settings(ty).len() - 2
}

/// The parameter a setting writes (its index among the settings that aren't
/// `type`), or `None` if the effect has no such setting.
pub fn param_of(ty: FxType, setting: &str) -> Option<usize> {
    own_settings(ty).iter().position(|s| s.name == setting)
}

/// The inclusive range a setting is clamped to.
pub fn range_of(s: &SettingSpec) -> (f32, f32) {
    match s.ty {
        SettingType::Number { min, max } => (min as f32, max as f32),
        SettingType::Int { min, max } => (min as f32, max as f32),
        _ => (0.0, 1.0),
    }
}

/// What a new effect's parameters start at (unused slots are 0). Parses the
/// `default` strings, so it belongs on the command thread.
pub fn defaults(ty: FxType) -> [f32; FX_PARAMS] {
    let mut out = [0.0; FX_PARAMS];
    for (o, s) in out.iter_mut().zip(own_settings(ty)) {
        *o = s.default.parse().unwrap_or(0.0);
    }
    out
}

/// Where an effect word lexes: the registry entry with that `word`.
pub fn from_word(word: &str) -> Option<FxType> {
    FxType::ALL.iter().copied().find(|t| spec(*t).word == word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::commands::lang::lexer::{KEYWORDS, Tok, lex};

    #[test]
    fn every_effect_spec_is_consistent() {
        for &ty in FxType::ALL {
            let s = spec(ty);
            let own = own_settings(ty);
            assert!(own.len() <= FX_PARAMS, "{}: too many settings", s.word);
            let n = s.settings.len();
            assert_eq!(s.settings[n - 2].name, "mix", "{}", s.word);
            assert_eq!(s.settings[n - 1].name, "gain", "{}", s.word);
            assert_eq!(
                !s.types.is_empty(),
                s.settings[0].name == "type",
                "{}: types iff the first setting is type",
                s.word
            );
            assert!(
                s.settings[1..].iter().all(|x| x.name != "type"),
                "{}: type is first",
                s.word
            );
            if let Some(SettingType::Word(words)) = s.settings.first().map(|x| x.ty)
                && !s.types.is_empty()
            {
                assert_eq!(words, s.types, "{}: type words", s.word);
            }
            for (i, x) in own.iter().enumerate() {
                let (lo, hi) = range_of(x);
                let d: f32 = x
                    .default
                    .parse()
                    .unwrap_or_else(|_| panic!("{}.{}: default parses", s.word, x.name));
                assert!(
                    (lo..=hi).contains(&d),
                    "{}.{}: default in range",
                    s.word,
                    x.name
                );
                assert_eq!(param_of(ty, x.name), Some(i));
            }
            for o in s.octaves {
                assert!(own.iter().any(|x| x.name == *o), "{}: octave {o}", s.word);
            }
            assert_eq!(s.schema.kind, Kind::Fx(ty));
        }
    }

    #[test]
    fn effect_words_are_keywords() {
        for &ty in FxType::ALL {
            let w = spec(ty).word;
            assert_eq!(from_word(w), Some(ty));
            assert_eq!(lex(w).tokens[0].kind, Tok::Kind(Kind::Fx(ty)), "{w} lexes");
            assert!(KEYWORDS.iter().all(|(k, _)| *k != w), "{w} is a keyword");
            assert_eq!(
                FxType::ALL.iter().filter(|t| spec(**t).word == w).count(),
                1,
                "{w} is in another spec"
            );
        }
    }
}
