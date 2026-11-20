//! Helpers shared by the command tests. They speak the language.

use super::*;
use crate::audio_processing::engine::arena::Limits;
use crate::audio_processing::test_util::track;
pub(super) use crate::audio_processing::test_util::{ctx, gid, vid};

/// A processor that knows one-frame tracks called `names`. It is not booted
/// (no `main`), so every object a test makes states its clock with `on`.
pub(super) fn processor(names: &[&str]) -> CmdProcessor {
    processor_with(names, Limits::default())
}

pub(super) fn processor_with(names: &[&str], limits: Limits) -> CmdProcessor {
    let files = names.iter().map(|n| track(n, 1, 16)).collect();
    CmdProcessor::new(EngineState::with_limits(files, 2, limits))
}

/// Like `processor`, with `main` booted: objects without `on` follow it.
pub(super) fn booted(names: &[&str]) -> CmdProcessor {
    let mut cp = processor(names);
    cp.boot();
    cp
}

/// Parses a line of the language to an engine command, panicking on failure.
pub(super) fn ok(cp: &mut CmdProcessor, line: &str) -> Command {
    match cp.parse(line) {
        Ok(Parsed::Engine(command)) => command,
        Ok(Parsed::Local(text)) => panic!("'{line}' is local: {text}"),
        Err(error) => panic!("'{line}' failed: {error}"),
    }
}

/// The error of a line that must fail.
pub(super) fn err(cp: &mut CmdProcessor, line: &str) -> CmdErr {
    match cp.parse(line) {
        Ok(_) => panic!("'{line}' should have failed"),
        Err(error) => error,
    }
}

/// The text a line answers on the command thread (a warning, for example),
/// panicking if it goes to the engine or fails.
pub(super) fn local(cp: &mut CmdProcessor, line: &str) -> String {
    match cp.parse(line) {
        Ok(Parsed::Local(text)) => text,
        Ok(Parsed::Engine(_)) => panic!("'{line}' made a command"),
        Err(error) => panic!("'{line}' failed: {error}"),
    }
}

pub(super) fn seq_tempo(cmd: Command) -> TempoUse {
    match cmd {
        Command::Seq(args) => args.tempo,
        _ => panic!("not a Seq"),
    }
}

pub(super) fn load_args(cmd: Command) -> LoadArgs {
    match cmd {
        Command::Load(args) => args,
        _ => panic!("not a Load"),
    }
}

pub(super) fn group_args(cmd: Command) -> GroupArgs {
    match cmd {
        Command::Group(args) => args,
        _ => panic!("not a Group"),
    }
}

pub(super) fn tc_tempo(cmd: Command) -> TempoInit {
    match cmd {
        Command::Tc(args) => args.tempo,
        _ => panic!("not a Tc"),
    }
}

/// `voice <n> from "<n>" on 1000ms` for each name: every voice has a clock of
/// its own (tempo IDs 0, 1, ...), as a voice loaded with no `-t` used to.
pub(super) fn loaded(names: &[&str]) -> CmdProcessor {
    let mut cp = processor(names);
    for n in names {
        ok(&mut cp, &format!("voice {n} from \"{n}\" on 1000ms"));
    }
    cp
}

/// `loaded`, then the group statement `group`.
pub(super) fn grouped(names: &[&str], group: &str) -> CmdProcessor {
    let mut cp = loaded(names);
    ok(&mut cp, group);
    cp
}

/// No group, and every voice still top-level with the ID its load gave it.
pub(super) fn assert_untouched(cp: &CmdProcessor, names: &[&str]) {
    let es = &cp.engine_state;
    assert!(es.groups.is_empty());
    assert!(es.group_names.is_empty());
    assert_eq!(es.ids.groups.live(), 0);
    assert_eq!(es.voices.len(), names.len());
    assert_eq!(es.ids.tempos.live() as usize, names.len());
    for (i, n) in names.iter().enumerate() {
        assert_eq!(vid(cp, n), VoiceId(i as u32), "{}", n);
        assert_eq!(es.voices[&VoiceId(i as u32)].group, None, "{}", n);
    }
}

/// A booted processor (`main` exists) with the tracks kick, hat, snare, bass.
pub(super) fn lang_processor() -> CmdProcessor {
    booted(&["kick", "hat", "snare", "bass"])
}

/// Same as `ok`.
pub(super) fn lok(cp: &mut CmdProcessor, line: &str) -> Command {
    ok(cp, line)
}

/// The errors of a line of the language that must fail.
pub(super) fn lerr(cp: &mut CmdProcessor, line: &str) -> Vec<lang::LangErr> {
    match err(cp, line) {
        CmdErr::Lang(errs) => errs,
        other => panic!("'{line}' failed with a family error: {other}"),
    }
}
