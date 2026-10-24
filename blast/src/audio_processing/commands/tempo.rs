//! Tempo states, tempo contexts (`tc`) and the shared `-t` parser.

use super::errors::{CmdErr, CmdResult};
use super::{CmdProcessor, Command, Parsed};
use crate::audio_processing::blast_time::blast_time::{TempoMode, TempoUnit};

/// One row per command: (name, usage and summary).
pub const HELP: &[(&str, &str)] = &[("tc", "tc|tempocon <name> s|m|b:<n>: create a tempo context")];

/// A TempoState as the engine should build it. `owned` decides whether the
/// engine initialises a new TempoState or refers to an existing one.
#[derive(Clone, Debug, PartialEq)]
pub struct TempoRepr {
    pub idx: usize,
    pub owned: bool,
    pub mode: TempoMode,
    pub unit: TempoUnit,
    pub interval: f32,
}

impl TempoRepr {
    pub(super) fn new(idx: usize) -> Self {
        Self {
            idx,
            owned: true, // until shared
            mode: TempoMode::TBD,
            unit: TempoUnit::Samples,
            interval: 0f32,
        }
    }

    /// Refers to another object's TempoState.
    pub(super) fn shared(other: &TempoRepr) -> Self {
        Self {
            owned: false,
            ..other.clone()
        }
    }

    /// Like `shared`, for the TempoState of a Voice or Group: if that object
    /// owns its TempoState, point at the object's current idx in the engine
    /// (its TempoRepr's own idx is meaningless); otherwise it already refers
    /// to something else (e.g. a TempoContext), so keep that reference.
    pub(super) fn following(other: &TempoRepr, owner_idx: usize) -> Self {
        let mut repr = TempoRepr::shared(other);
        if other.owned {
            repr.idx = owner_idx;
        }
        repr
    }

    pub(super) fn init(&mut self, mode: TempoMode, unit: TempoUnit, interval: f32) {
        self.mode = mode;
        self.unit = unit;
        self.interval = interval;
    }
}

/// Arguments for `Command::Tc`.
pub struct TcArgs {
    pub tempo: TempoRepr,
}

/// A parsed `-t` value.
#[derive(Debug, PartialEq)]
pub(super) enum TempoSpec<'a> {
    /// `s|m|b:<n>`
    New { unit: TempoUnit, interval: f32 },
    /// `c:<name>`
    Context(&'a str),
    /// `g:<name>`
    Group(&'a str),
    /// `v`
    Voice,
}

/// `s`, `m` or `b`.
pub(super) fn parse_unit(u: &str, cmd: &str) -> CmdResult<TempoUnit> {
    match u {
        "s" => Ok(TempoUnit::Samples),
        "m" => Ok(TempoUnit::Millis),
        "b" => Ok(TempoUnit::Bpm),
        _ => Err(CmdErr::invalid(u, cmd)),
    }
}

/// Parses `s|m|b:<n>`, `c:<name>`, `g:<name>` or `v`; parts after the second
/// are ignored. `cmd` names the command in errors (`<cmd> -t`).
pub(super) fn parse_tempo_spec<'a>(spec: &'a str, cmd: &str) -> CmdResult<TempoSpec<'a>> {
    let cmd = format!("{cmd} -t");
    if spec == "v" {
        return Ok(TempoSpec::Voice);
    }
    let mut parts = spec.split(':');
    let u = parts.next().unwrap_or_default();
    let mut name = || parts.next().ok_or_else(|| CmdErr::missing("name", &cmd));
    match u {
        "c" => Ok(TempoSpec::Context(name()?)),
        "g" => Ok(TempoSpec::Group(name()?)),
        _ => {
            let unit = parse_unit(u, &cmd)?;
            let raw = parts
                .next()
                .ok_or_else(|| CmdErr::missing("interval", &cmd))?;
            let interval = raw.parse().map_err(|_| CmdErr::invalid(raw, &cmd))?;
            Ok(TempoSpec::New { unit, interval })
        }
    }
}

/// The error for a `-t` form a command doesn't accept: the spec's first part
/// is an invalid unit there.
pub(super) fn rejected(spec: &str, cmd: &str) -> CmdErr {
    CmdErr::invalid(
        spec.split(':').next().unwrap_or_default(),
        &format!("{cmd} -t"),
    )
}

/// The `unit:interval` shape `tc` and `seq -t` insist on.
pub(super) fn has_two_parts(spec: &str) -> bool {
    spec.split(':').count() == 2
}

impl CmdProcessor {
    pub(super) fn try_tc(&mut self, args: &str) -> CmdResult<Parsed> {
        let mut args = args.split_whitespace();
        let name = args
            .next()
            .ok_or_else(|| CmdErr::missing("name", "tempocon"))?;

        // a second context by the same name would replace the first in the
        // mirror but not in the engine, shifting every later idx
        if self.engine_state.tempo_cons.contains_key(name) {
            return Err(CmdErr::AlreadyIs {
                ty: "TempoContext".to_string(),
                name: name.to_string(),
            });
        }

        let spec = args
            .next()
            .ok_or_else(|| CmdErr::missing("tempo", "tempocon"))?;
        if !has_two_parts(spec) {
            return Err(CmdErr::TempoFormatting);
        }
        let TempoSpec::New { unit, interval } = parse_tempo_spec(spec, "tempocon")? else {
            return Err(rejected(spec, "tempocon"));
        };

        let mut tempo = TempoRepr::new(self.engine_state.tempo_cons.len());
        tempo.init(TempoMode::Context, unit, interval);
        self.engine_state
            .tempo_cons
            .insert(name.to_string(), tempo.clone());

        Ok(Parsed::Engine(Command::Tc(TcArgs { tempo })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::commands::CmdErr;
    use crate::audio_processing::commands::testing::*;

    #[test]
    fn tempo_spec_forms() {
        let new = |unit, interval| TempoSpec::New { unit, interval };
        let cases = [
            ("b:120", new(TempoUnit::Bpm, 120.0)),
            ("m:250", new(TempoUnit::Millis, 250.0)),
            ("s:10", new(TempoUnit::Samples, 10.0)),
            ("b:90:extra", new(TempoUnit::Bpm, 90.0)),
            ("c:x", TempoSpec::Context("x")),
            ("g:y", TempoSpec::Group("y")),
            ("v", TempoSpec::Voice),
        ];
        for (spec, want) in cases {
            assert_eq!(parse_tempo_spec(spec, "load").unwrap(), want, "{spec}");
        }
    }

    #[test]
    fn tempo_spec_errors() {
        for spec in ["c", "g", "b"] {
            assert!(
                matches!(
                    parse_tempo_spec(spec, "load"),
                    Err(CmdErr::MissingArg { .. })
                ),
                "{spec}"
            );
        }
        for spec in ["x:1", "b:fast", "v:1"] {
            assert!(
                matches!(
                    parse_tempo_spec(spec, "load"),
                    Err(CmdErr::InvalidArg { .. })
                ),
                "{spec}"
            );
        }
    }

    #[test]
    fn tc_parses_tempo() {
        let mut cp = processor(&[]);
        let t = tc_tempo(ok(&mut cp, "tc a b:120"));
        assert!(t.owned);
        assert_eq!(t.mode, TempoMode::Context);
        assert_eq!(t.unit, TempoUnit::Bpm);
        assert_eq!(t.interval, 120.0);
        assert_eq!(t.idx, 0);
        let t = tc_tempo(ok(&mut cp, "tempocon b m:500"));
        assert_eq!(t.unit, TempoUnit::Millis);
        assert_eq!(t.idx, 1);

        assert!(matches!(err(&mut cp, "tc"), CmdErr::MissingArg { .. }));
        assert!(matches!(err(&mut cp, "tc c"), CmdErr::MissingArg { .. }));
        assert!(matches!(err(&mut cp, "tc c b"), CmdErr::TempoFormatting));
        assert!(matches!(
            err(&mut cp, "tc c x:1"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "tc c b:fast"),
            CmdErr::InvalidArg { .. }
        ));
        assert_eq!(cp.engine_state.tempo_cons.len(), 2);
    }

    #[test]
    fn duplicate_tc_is_rejected() {
        let mut cp = processor(&[]);
        ok(&mut cp, "tc a b:120");
        assert!(matches!(
            err(&mut cp, "tc a b:90"),
            CmdErr::AlreadyIs { .. }
        ));
        let a = cp.engine_state.tempo_cons.get("a").unwrap();
        assert_eq!(a.interval, 120.0);
        assert_eq!(cp.engine_state.tempo_cons.len(), 1);

        // later contexts get their engine Vec position
        let t = tc_tempo(ok(&mut cp, "tc b b:60"));
        assert_eq!(t.idx, 1);
        assert_eq!(cp.engine_state.tempo_cons.get("b").unwrap().idx, 1);
    }
}
