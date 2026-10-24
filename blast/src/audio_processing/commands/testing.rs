//! Helpers shared by the command tests.

use super::*;
use crate::audio_processing::test_util::track;

pub(super) fn processor(names: &[&str]) -> CmdProcessor {
    let files = names.iter().map(|n| track(n, 1, 16)).collect();
    CmdProcessor::new(EngineState::new(files, 2))
}

/// Parses an engine command, panicking on failure.
pub(super) fn ok(cp: &mut CmdProcessor, cmd: &str) -> Command {
    match cp.parse(cmd) {
        Ok(Parsed::Engine(command)) => command,
        Ok(Parsed::Local(_)) => panic!("'{}' is a local command", cmd),
        Err(error) => panic!("'{}' failed: {}", cmd, error),
    }
}

pub(super) fn err(cp: &mut CmdProcessor, cmd: &str) -> CmdErr {
    match cp.parse(cmd) {
        Ok(_) => panic!("'{}' should have failed", cmd),
        Err(error) => error,
    }
}

pub(super) fn v_idx(cp: &CmdProcessor, name: &str) -> usize {
    cp.engine_state.voices.get(name).unwrap().idx
}

pub(super) fn gv_idx(cp: &CmdProcessor, group: &str, voice: &str) -> usize {
    cp.engine_state
        .groups
        .get(group)
        .unwrap()
        .voices
        .get(voice)
        .unwrap()
        .idx
}

pub(super) fn seq_tempo(cmd: Command) -> TempoRepr {
    match cmd {
        Command::Seq(args) => args.tempo,
        _ => panic!("not a Seq"),
    }
}

pub(super) fn load_tempo(cmd: Command) -> TempoRepr {
    match cmd {
        Command::Load(args) => args.tempo_repr,
        _ => panic!("not a Load"),
    }
}

pub(super) fn group_args(cmd: Command) -> GroupArgs {
    match cmd {
        Command::Group(args) => args,
        _ => panic!("not a Group"),
    }
}

pub(super) fn tc_tempo(cmd: Command) -> TempoRepr {
    match cmd {
        Command::Tc(args) => args.tempo,
        _ => panic!("not a Tc"),
    }
}

pub(super) fn grouped(names: &[&str], group: &str) -> CmdProcessor {
    let mut cp = processor(names);
    for n in names {
        ok(&mut cp, &format!("load {}", n));
    }
    ok(&mut cp, group);
    cp
}

pub(super) fn loaded(names: &[&str]) -> CmdProcessor {
    let mut cp = processor(names);
    for n in names {
        ok(&mut cp, &format!("load {}", n));
    }
    cp
}

pub(super) fn assert_untouched(cp: &CmdProcessor, names: &[&str]) {
    assert!(cp.engine_state.groups.is_empty());
    assert_eq!(cp.engine_state.voices.len(), names.len());
    for (i, n) in names.iter().enumerate() {
        assert_eq!(v_idx(cp, n), i, "{}", n);
    }
}
