//! Voices: load, unload, transport and velocity.

use super::errors::{CmdErr, CmdResult};
use super::process::ProcRepr;
use super::reprs::Idx;
use super::tempo::{TempoRepr, TempoSpec, parse_tempo_spec, rejected};
use super::{CmdProcessor, Command, Parsed};
use crate::audio_processing::blast_time::blast_time::TempoMode;
use std::collections::HashMap;

/// One row per command: (name, usage and summary).
pub const HELP: &[(&str, &str)] = &[
    (
        "load",
        "load <track> [-t s|m|b:<n> | c:<context> | g:<group>]: load a track as a voice",
    ),
    (
        "start",
        "start -v|-g|-t <name>: start a voice, group or tempo context from the top",
    ),
    ("pause", "pause -v|-g|-t <name>: pause without resetting"),
    ("resume", "resume -v|-g|-t <name>: resume after pause"),
    ("stop", "stop -v|-g|-t <name>: stop and rewind"),
    ("unload", "unload <voice|group.voice>: remove a voice"),
    (
        "velocity",
        "velocity <voice|group.voice> <rate>: playback rate (negative plays backwards)",
    ),
];

/// A Voice in the mirror.
pub struct VoiceRepr {
    pub(super) idx: usize,
    pub(super) tempo: TempoRepr,
    pub(super) processes: HashMap<String, ProcRepr>,
    pub(super) proc_tempi: HashMap<usize, TempoRepr>,
}

impl VoiceRepr {
    pub(super) fn new(idx: usize, tempo: TempoRepr) -> Self {
        Self {
            idx,
            tempo,
            processes: HashMap::new(),
            proc_tempi: HashMap::new(),
        }
    }
}

/// Arguments for `Command::Load`.
pub struct LoadArgs {
    pub track_idx: usize,
    pub tempo_repr: TempoRepr,
}

/// Arguments for `Command::Start`.
pub struct StartArgs {
    pub idx: Idx,
}

/// Arguments for `Command::Pause`.
pub struct PauseArgs {
    pub idx: Idx,
}

/// Arguments for `Command::Resume`.
pub struct ResumeArgs {
    pub idx: Idx,
}

/// Arguments for `Command::Stop`.
pub struct StopArgs {
    pub idx: Idx,
}

/// Arguments for `Command::Unload`.
pub struct UnloadArgs {
    /// Voice or GroupVoice.
    pub idx: Idx,
}

/// Arguments for `Command::Velocity`.
pub struct VelocityArgs {
    pub idx: Idx,
    pub val: f32,
}

// `<flag> <name>`, e.g. `-v kick`
fn type_and_name<'a>(args: &'a str, cmd: &str) -> CmdResult<(&'a str, &'a str)> {
    let mut args = args.split_whitespace();
    let first = args
        .next()
        .ok_or_else(|| CmdErr::missing("type and name", cmd))?;
    let second = args
        .next()
        .ok_or_else(|| CmdErr::missing("type or name", cmd))?;
    Ok((first, second))
}

impl CmdProcessor {
    // The engine turns LoadArgs into a Voice: it gets the Track, builds a
    // TempoState from the TempoRepr (checking its mode to see whether it
    // refers to an existing TempoState) and calls Voice::new.
    pub(super) fn try_load(&mut self, args: &str) -> CmdResult<Parsed> {
        let mut args = args.split_whitespace();
        let name = args.next().ok_or_else(|| CmdErr::missing("name", "load"))?;

        let track_idx = self.engine_state.track(name)?.idx;

        // a Voice by this name (currently the track name) can't exist twice
        if self.engine_state.voice(name).is_ok() {
            return Err(CmdErr::AlreadyIs {
                ty: "Voice".to_string(),
                name: name.to_string(),
            });
        }

        // idx 0: a Voice only ever has one personal TempoState
        let mut tempo_repr = TempoRepr::new(0);

        while let Some(arg) = args.next() {
            match arg {
                "-t" | "--tempo" => {
                    let spec = args
                        .next()
                        .ok_or_else(|| CmdErr::missing("unit", "load -t/--tempo"))?;
                    match parse_tempo_spec(spec, "load")? {
                        TempoSpec::Context(tc) => {
                            tempo_repr = TempoRepr::shared(self.engine_state.context(tc)?);
                        }
                        TempoSpec::Group(g) => {
                            let g = self.engine_state.group(g)?;
                            tempo_repr = TempoRepr::following(&g.tempo, g.idx);
                        }
                        TempoSpec::New { unit, interval } => {
                            tempo_repr.init(TempoMode::Voice, unit, interval);
                        }
                        TempoSpec::Voice => return Err(rejected(spec, "load")),
                    }
                }
                _ => return Err(CmdErr::invalid(arg, "load")),
            }
        }

        // the first Voice is indexed at 0
        let idx = self.engine_state.voices.len();
        self.engine_state
            .voices
            .insert(name.to_string(), VoiceRepr::new(idx, tempo_repr.clone()));

        Ok(Parsed::Engine(Command::Load(LoadArgs {
            track_idx,
            tempo_repr,
        })))
    }

    // these could start several things at once (e.g. *Args holding a
    // Vec<Idx>); maybe implement "all" as a reserved word
    fn transport_target(&self, args: &str, cmd: &str) -> CmdResult<Idx> {
        let (flag, name) = type_and_name(args, cmd)?;
        self.engine_state.target(flag, name)
    }

    pub(super) fn try_start(&mut self, args: &str) -> CmdResult<Parsed> {
        let idx = self.transport_target(args, "start")?;
        Ok(Parsed::Engine(Command::Start(StartArgs { idx })))
    }

    pub(super) fn try_pause(&mut self, args: &str) -> CmdResult<Parsed> {
        let idx = self.transport_target(args, "pause")?;
        Ok(Parsed::Engine(Command::Pause(PauseArgs { idx })))
    }

    pub(super) fn try_resume(&mut self, args: &str) -> CmdResult<Parsed> {
        let idx = self.transport_target(args, "resume")?;
        Ok(Parsed::Engine(Command::Resume(ResumeArgs { idx })))
    }

    pub(super) fn try_stop(&mut self, args: &str) -> CmdResult<Parsed> {
        let idx = self.transport_target(args, "stop")?;
        Ok(Parsed::Engine(Command::Stop(StopArgs { idx })))
    }

    // accepts voice or group.voice, and removes the VoiceRepr from its
    // owner (engine_state or a Group)
    pub(super) fn try_unload(&mut self, name: &str) -> CmdResult<Parsed> {
        let name = name.trim();
        let idx = self.engine_state.voice_idx(name)?;

        let (voices, v_name) = match name.split_once('.') {
            Some((group, voice)) => (&mut self.engine_state.group_mut(group)?.voices, voice),
            None => (&mut self.engine_state.voices, name),
        };
        let Some(removed) = voices.remove(v_name) else {
            // voice_idx found it above
            return Err(CmdErr::NoVoice {
                name: v_name.to_string(),
                group: None,
            });
        };

        // every later Voice shifts left in the engine; E1 removes this
        for voice in voices.values_mut() {
            if voice.idx > removed.idx {
                voice.idx -= 1;
            }
        }

        Ok(Parsed::Engine(Command::Unload(UnloadArgs { idx })))
    }

    pub(super) fn try_velocity(&mut self, args: &str) -> CmdResult<Parsed> {
        let mut args = args.splitn(2, ' ');
        let name = args
            .next()
            .ok_or_else(|| CmdErr::missing("name", "velocity"))?;

        // Idx::Voice or Idx::GroupVoice
        let idx = self.engine_state.target("-v", name)?;

        let Some(raw) = args.next() else {
            return Err(CmdErr::missing("value", "velocity"));
        };
        let Ok(val) = raw.parse::<f32>() else {
            return Err(CmdErr::invalid(raw, "velocity"));
        };

        Ok(Parsed::Engine(Command::Velocity(VelocityArgs { idx, val })))
    }
}

#[cfg(test)]
mod tests {
    use crate::audio_processing::blast_time::blast_time::{TempoMode, TempoUnit};
    use crate::audio_processing::commands::testing::*;
    use crate::audio_processing::commands::*;

    #[test]
    fn load_assigns_increasing_indices() {
        let mut cp = processor(&["kick", "snare"]);
        let t = load_tempo(ok(&mut cp, "load kick"));
        assert!(t.owned);
        assert_eq!(t.mode, TempoMode::TBD);

        let t = load_tempo(ok(&mut cp, "load snare -t b:120"));
        assert!(t.owned);
        assert_eq!(t.mode, TempoMode::Voice);
        assert_eq!(t.unit, TempoUnit::Bpm);
        assert_eq!(t.interval, 120.0);

        assert_eq!(v_idx(&cp, "kick"), 0);
        assert_eq!(v_idx(&cp, "snare"), 1);
    }

    #[test]
    fn load_errors() {
        let mut cp = processor(&["kick"]);
        assert!(matches!(err(&mut cp, "load nope"), CmdErr::NoItem { .. }));
        assert!(matches!(
            err(&mut cp, "load kick -t x:1"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "load kick -t c:nope"),
            CmdErr::NoItem { .. }
        ));
        assert!(matches!(
            err(&mut cp, "load kick -t g:nope"),
            CmdErr::NoItem { .. }
        ));
        assert!(cp.engine_state.voices.is_empty());
        ok(&mut cp, "load kick");
        assert!(matches!(
            err(&mut cp, "load kick"),
            CmdErr::AlreadyIs { .. }
        ));
    }

    #[test]
    fn unload_shifts_later_indices() {
        let mut cp = processor(&["a", "b", "c"]);
        ok(&mut cp, "load a");
        ok(&mut cp, "load b");
        ok(&mut cp, "load c");
        match ok(&mut cp, "unload a") {
            Command::Unload(args) => assert_eq!(args.idx, Idx::Voice(0)),
            _ => panic!("not an Unload"),
        }
        assert!(!cp.engine_state.voices.contains_key("a"));
        assert_eq!(v_idx(&cp, "b"), 0);
        assert_eq!(v_idx(&cp, "c"), 1);
        assert!(matches!(err(&mut cp, "unload a"), CmdErr::NoVoice { .. }));
    }

    #[test]
    fn velocity_targets_group_voice() {
        let mut cp = grouped(&["a", "b", "c"], "group g -v a,b");
        match ok(&mut cp, "velocity g.b 2") {
            Command::Velocity(args) => {
                assert_eq!(args.idx, Idx::GroupVoice(0, 1));
                assert_eq!(args.val, 2.0);
            }
            _ => panic!("not a Velocity"),
        }
        match ok(&mut cp, "velocity c -1.5") {
            Command::Velocity(args) => {
                assert_eq!(args.idx, Idx::Voice(0));
                assert_eq!(args.val, -1.5);
            }
            _ => panic!("not a Velocity"),
        }
        assert!(matches!(
            err(&mut cp, "velocity c"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "velocity c x"),
            CmdErr::InvalidArg { .. }
        ));
    }

    #[test]
    fn start_targets_group_voice() {
        let mut cp = grouped(&["a", "b"], "group g -v a,b");
        for (cmd, want) in [
            ("start -v g.a", Idx::GroupVoice(0, 0)),
            ("pause -v g.b", Idx::GroupVoice(0, 1)),
            ("start -g g", Idx::Group(0)),
        ] {
            let idx = match ok(&mut cp, cmd) {
                Command::Start(args) => args.idx,
                Command::Pause(args) => args.idx,
                _ => panic!("unexpected Command"),
            };
            assert_eq!(idx, want, "{}", cmd);
        }
    }

    #[test]
    fn missing_group_voice_errors() {
        let mut cp = grouped(&["a"], "group g -v a");
        match err(&mut cp, "start -v g.zzz") {
            CmdErr::NoVoice { name, group } => {
                assert_eq!(name, "zzz");
                assert_eq!(group, Some("g".to_string()));
            }
            other => panic!("wrong error {:?}", other),
        }
        assert!(matches!(
            err(&mut cp, "start -v nope.a"),
            CmdErr::NoItem { .. }
        ));
        assert!(matches!(
            err(&mut cp, "start -v a"),
            CmdErr::NoVoice { group: None, .. }
        ));
        assert!(matches!(
            err(&mut cp, "start -v g.a.b"),
            CmdErr::Formatting { .. }
        ));
    }
}
