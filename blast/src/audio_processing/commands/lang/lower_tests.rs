//! LANGb's tests: the schema, names and lowering onto the mirror.

use super::Code;
use crate::audio_processing::blast_time::blast_time::TempoUnit;
use crate::audio_processing::commands::testing::*;
use crate::audio_processing::commands::*;
use crate::audio_processing::engine::VoiceParam;

fn batch(cmd: Command) -> Vec<Command> {
    match cmd {
        Command::Batch(args) => args.cmds,
        other => panic!("expected a Batch, got a {}", name_of(&other)),
    }
}

// a Batch's commands, or the one command
fn batch_or_one(cmd: Command) -> Vec<Command> {
    match cmd {
        Command::Batch(args) => args.cmds,
        other => vec![other],
    }
}

fn name_of(cmd: &Command) -> &'static str {
    match cmd {
        Command::Load(_) => "Load",
        Command::Start(_) => "Start",
        Command::Pause(_) => "Pause",
        Command::Resume(_) => "Resume",
        Command::Stop(_) => "Stop",
        Command::Unload(_) => "Unload",
        Command::SetParam(_) => "SetParam",
        Command::SetMode(_) => "SetMode",
        Command::SetTrack(_) => "SetTrack",
        Command::SetShifters(_) => "SetShifters",
        Command::AddTrack(_) => "AddTrack",
        Command::DropTrack(_) => "DropTrack",
        Command::Group(_) => "Group",
        Command::GroupEdit(_) => "GroupEdit",
        Command::GroupGain(_) => "GroupGain",
        Command::Tc(_) => "Tc",
        Command::TempoSet(_) => "TempoSet",
        Command::TempoFree(_) => "TempoFree",
        Command::TempoAssign(_) => "TempoAssign",
        Command::Seq(_) => "Seq",
        Command::SeqEdit(_) => "SeqEdit",
        Command::SeqRecv(_) => "SeqRecv",
        Command::Lfo(_) => "Lfo",
        Command::LfoEdit(_) => "LfoEdit",
        Command::Env(_) => "Env",
        Command::EnvEdit(_) => "EnvEdit",
        Command::ProcEdit(_) => "ProcEdit",
        Command::Fx(_) => "Fx",
        Command::Wave(_) => "Wave",
        Command::Buffer(_) => "Buffer",
        Command::BufferEdit(_) => "BufferEdit",
        Command::SetTaps(_) => "SetTaps",
        Command::SetSource(_) => "SetSource",
        Command::Schedule(_) => "Schedule",
        Command::MarkGlides(_) => "MarkGlides",
        Command::SetMark(_) => "SetMark",
        Command::CancelScript(_) => "CancelScript",
        Command::Batch(_) => "Batch",
        Command::Quit(_) => "Quit",
    }
}

fn names(cmds: &[Command]) -> Vec<&'static str> {
    cmds.iter().map(name_of).collect()
}

fn local(cp: &mut CmdProcessor, line: &str) -> String {
    match cp.parse(line) {
        Ok(Parsed::Local(text)) => text,
        Ok(Parsed::Engine(c)) => panic!("'{line}' made a {}", name_of(&c)),
        Err(e) => panic!("'{line}' failed: {e}"),
    }
}

#[test]
fn boot_creates_main() {
    let mut cp = processor(&[]);
    let cmds = batch(cp.boot());
    let Command::Tc(tc) = &cmds[0] else {
        panic!("first is not a Tc")
    };
    assert_eq!(tc.tempo.id, TempoId(0));
    assert_eq!(tc.tempo.unit, TempoUnit::Bpm);
    assert_eq!(tc.tempo.interval, 120.0);
    // main runs from the start, so voices that follow it don't warn
    assert_eq!(names(&cmds), ["Tc", "Start"]);
    assert_eq!(cp.engine_state.context_id("main").unwrap(), TempoId(0));
    // `start main` is a clock target
    assert!(matches!(
        lok(&mut cp, "start main"),
        Command::Start(StartArgs {
            target: Target::Tempo(TempoId(0))
        })
    ));
}

#[test]
fn voice_follows_main() {
    let mut cp = lang_processor();
    let Command::Load(args) = lok(&mut cp, "voice kick from \"kick\"") else {
        panic!("not a Load")
    };
    assert_eq!(args.tempo, TempoUse::Shared(TempoId(0)));
    assert!(args.adopts);
    let v = &cp.engine_state.voices[&args.voice];
    assert_eq!(v.link, TempoLink::Parent);
    assert_eq!(v.tempo, TempoId(0));
    assert_eq!(v.period, 4);
}

#[test]
fn voice_own_name() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice bd from \"kick\"");
    assert!(cp.engine_state.voice_id("bd").is_ok());
    assert!(cp.engine_state.voice_id("kick").is_err());
}

#[test]
fn voice_on_forms() {
    let mut cp = lang_processor();
    let Command::Load(a) = lok(&mut cp, "voice a from \"kick\" on main in 8") else {
        panic!()
    };
    assert!(!a.adopts);
    assert_eq!(a.tempo, TempoUse::Shared(TempoId(0)));
    assert_eq!(cp.engine_state.voices[&a.voice].link, TempoLink::Context);
    assert_eq!(cp.engine_state.voices[&a.voice].period, 8);

    let Command::Load(b) = lok(&mut cp, "voice b from \"hat\" on 93b") else {
        panic!()
    };
    let TempoUse::Own(t) = b.tempo else {
        panic!("not an own tempo")
    };
    assert_eq!((t.unit, t.interval), (TempoUnit::Bpm, 93.0));
    assert_eq!(cp.engine_state.voices[&b.voice].link, TempoLink::Own);
    assert_eq!(
        cp.engine_state.tempos[&t.id].owner,
        TempoOwner::Voice(b.voice)
    );

    lok(&mut cp, "group drums");
    let Command::Load(c) = lok(&mut cp, "voice c from \"snare\" on drums") else {
        panic!()
    };
    let g = cp.engine_state.group_id("drums").unwrap();
    assert_eq!(cp.engine_state.voices[&c.voice].link, TempoLink::Group(g));
    assert!(!c.adopts);
}

#[test]
fn group_block_members_and_decls() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\"");
    let cmds = batch(lok(&mut cp, "group drums { kick; voice hat from \"hat\" }"));
    assert_eq!(names(&cmds), ["Load", "Group"]);
    let (Command::Load(hat), Command::Group(g)) = (&cmds[0], &cmds[1]) else {
        panic!()
    };
    let kick = cp.engine_state.voice_id("drums.kick").unwrap();
    assert_eq!(g.members, vec![kick, hat.voice]);
    let drums = &cp.engine_state.groups[&g.group];
    // hat follows drums
    assert_eq!(
        cp.engine_state.voices[&hat.voice].link,
        TempoLink::Group(g.group)
    );
    assert_eq!(cp.engine_state.voices[&hat.voice].tempo, drums.tempo);
    assert_eq!(cp.engine_state.voices[&kick].tempo, drums.tempo);
}

#[test]
fn grouped_names_keep() {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice hat from \"hat\" and voice snare from \"snare\"",
    );
    lok(&mut cp, "group drums { hat }");
    let hat = cp.engine_state.voice_id("drums.hat").unwrap();
    // both forms resolve, to the same voice
    lok(&mut cp, "start hat");
    lok(&mut cp, "start drums.hat");
    assert_eq!(
        cp.engine_state.display_name(hat).as_deref(),
        Some("drums.hat")
    );
    let errs = lerr(&mut cp, "start snare.hat");
    assert_eq!(errs[0].code, Code::AmbiguousPath);
    let errs = lerr(&mut cp, "start drums.snare");
    assert_eq!(errs[0].code, Code::AmbiguousPath);
}

#[test]
fn names_unique_across_kinds() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\"");
    assert_eq!(lerr(&mut cp, "clock kick 90b")[0].code, Code::KindClash);
    assert_eq!(lerr(&mut cp, "group kick")[0].code, Code::KindClash);
    assert_eq!(
        lerr(&mut cp, "voice main from \"hat\"")[0].code,
        Code::KindClash
    );
    // a typo never creates
    let errs = lerr(&mut cp, "kik rate=2");
    assert_eq!(errs[0].code, Code::UnknownName);
    assert_eq!(errs[0].suggestion.as_deref(), Some("did you mean 'kick'?"));
}

#[test]
fn statement_is_atomic() {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice kick from \"kick\" and voice hat from \"hat\"",
    );
    let (voices, tempos, groups) = {
        let es = &cp.engine_state;
        (
            es.ids.voices.live(),
            es.ids.tempos.live(),
            es.ids.groups.live(),
        )
    };
    let errs = lerr(&mut cp, "group drums { kick; hta }");
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].code, Code::UnknownName);
    assert_eq!(errs[0].suggestion.as_deref(), Some("did you mean 'hat'?"));
    let es = &cp.engine_state;
    assert_eq!(
        (
            es.ids.voices.live(),
            es.ids.tempos.live(),
            es.ids.groups.live()
        ),
        (voices, tempos, groups)
    );
    assert!(es.groups.is_empty() && es.group_names.is_empty());
    assert!(es.voice_names.contains_key("kick") && es.voice_names.contains_key("hat"));
    assert_eq!(es.voices[&es.voice_names["kick"]].group, None);
    // and a failing declaration inside a block leaves nothing behind
    lerr(&mut cp, "group g2 { voice snare from \"snare\"; nope }");
    assert!(!cp.engine_state.voice_names.contains_key("snare"));
    assert_eq!(cp.engine_state.ids.voices.live(), voices);
}

#[test]
fn chain_is_one_batch() {
    let mut cp = lang_processor();
    let cmds = batch(lok(
        &mut cp,
        "voice a from \"kick\" and voice b from \"hat\"",
    ));
    assert_eq!(names(&cmds), ["Load", "Load"]);
}

#[test]
fn restate_is_noop() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\"");
    let text = local(&mut cp, "voice kick from \"kick\"");
    assert!(text.contains("W101"), "{text}");
    assert_eq!(cp.engine_state.ids.voices.live(), 1);
    // so is a bare name, and restating a group's members
    assert!(local(&mut cp, "kick").contains("W101"));
    lok(&mut cp, "group drums { kick }");
    assert!(local(&mut cp, "group drums { kick }").contains("W101"));
    assert!(local(&mut cp, "clock main 120b").contains("W101"));
}

#[test]
fn rate_update() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\"");
    let Command::SetParam(v) = lok(&mut cp, "kick rate=0.5") else {
        panic!("not a SetParam")
    };
    assert_eq!(v.param, VoiceParam::Rate);
    assert_eq!(v.value, 0.5);
    assert_eq!(v.voice, cp.engine_state.voice_id("kick").unwrap());
    assert!(local(&mut cp, "kick rate=0.5").contains("W101"));
    // on create too
    let cmds = batch(lok(&mut cp, "voice hat from \"hat\" rate=-2"));
    assert_eq!(names(&cmds), ["Load", "SetParam"]);
    assert_eq!(lerr(&mut cp, "kick rate=40")[0].code, Code::OutOfRange);
    assert_eq!(lerr(&mut cp, "kick rate=fast")[0].code, Code::WrongType);
}

#[test]
fn transport() {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice kick from \"kick\" and voice hat from \"hat\"",
    );
    let cmds = batch(lok(&mut cp, "start kick hat"));
    assert_eq!(names(&cmds), ["Start", "Start"]);
    // all: clocks, groups, then top-level voices
    lok(&mut cp, "group drums { hat }");
    let cmds = batch(lok(&mut cp, "stop all"));
    assert_eq!(names(&cmds), ["Stop", "Stop", "Stop"]);
    let Command::Stop(first) = &cmds[0] else {
        panic!()
    };
    assert_eq!(first.target, Target::Tempo(TempoId(0)));
    // a single target is not a Batch
    assert!(matches!(
        lok(&mut cp, "pause drums"),
        Command::Pause(PauseArgs {
            target: Target::Group(_)
        })
    ));
    assert!(matches!(lok(&mut cp, "resume kick"), Command::Resume(_)));
    assert_eq!(lerr(&mut cp, "start \"kick\"")[0].code, Code::WrongType);
}

#[test]
fn start_clock_starts_followers() {
    let mut cp = lang_processor();
    lok(&mut cp, "clock slow 93b");
    lok(&mut cp, "voice a from \"kick\" on slow");
    lok(&mut cp, "voice b from \"snare\"");
    lok(&mut cp, "group g on slow { voice c from \"hat\" }");
    let slow = cp.engine_state.context_id("slow").unwrap();
    let a = cp.engine_state.voice_id("a").unwrap();
    let b = cp.engine_state.voice_id("b").unwrap();
    let g = cp.engine_state.group_id("g").unwrap();
    let targets = |cmds: Vec<Command>| -> Vec<Target> {
        cmds.iter()
            .map(|c| match c {
                Command::Start(s) => s.target,
                _ => panic!("not a Start"),
            })
            .collect()
    };
    let cmds = batch(lok(&mut cp, "start slow"));
    assert_eq!(
        targets(cmds),
        [Target::Tempo(slow), Target::Voice(a), Target::Group(g)]
    );
    let cmds = batch(lok(&mut cp, "start main"));
    let t = targets(cmds);
    assert_eq!(t[0], Target::Tempo(TempoId(0)));
    assert!(t.contains(&Target::Voice(b)) && !t.contains(&Target::Voice(a)));
    // stop is the clock only
    assert!(matches!(lok(&mut cp, "stop slow"), Command::Stop(_)));
}

#[test]
fn start_clock_alone() {
    let mut cp = lang_processor();
    lok(&mut cp, "clock lonely 60b");
    assert!(matches!(lok(&mut cp, "start lonely"), Command::Start(_)));
}

#[test]
fn not_built_yet() {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice kick from \"kick\" and voice hat from \"hat\"",
    );
    lok(&mut cp, "group drums { hat }");
    let errs = lerr(&mut cp, "remove main2");
    assert_eq!(errs[0].code, Code::UnknownName);
    // `wave` is built (WAVa), but a wave belongs to a voice
    let errs = lerr(&mut cp, "wave wob");
    assert_eq!(errs[0].code, Code::WrongType);
}

#[test]
fn schema_errors() {
    let mut cp = lang_processor();
    let errs = lerr(&mut cp, "voice kick from \"kick\" gian=1");
    assert_eq!(errs[0].code, Code::UnknownSetting);
    assert_eq!(errs[0].suggestion.as_deref(), Some("did you mean 'gain'?"));
    assert_eq!(lerr(&mut cp, "voice kick")[0].code, Code::MissingItem);
    assert_eq!(lerr(&mut cp, "clock c1")[0].code, Code::MissingItem);
    assert_eq!(
        lerr(&mut cp, "voice kick from \"nope\"")[0].code,
        Code::UnknownName
    );
    let errs = lerr(&mut cp, "voice kick from \"kik\"");
    assert_eq!(errs[0].suggestion.as_deref(), Some("did you mean 'kick'?"));
    assert_eq!(
        lerr(&mut cp, "voice k from \"kick\" in 0")[0].code,
        Code::OutOfRange
    );
    assert_eq!(lerr(&mut cp, "clock c1 0b")[0].code, Code::OutOfRange);
    assert_eq!(
        lerr(&mut cp, "group g from \"kick\"")[0].code,
        Code::WrongType
    );
    assert_eq!(
        lerr(&mut cp, "voice k from \"kick\" | trig")[0].code,
        Code::WrongType
    );
    assert_eq!(
        lerr(&mut cp, "voice all from \"kick\"")[0]
            .code
            .as_str()
            .len(),
        4
    );
    // a failed statement leaves nothing
    assert!(cp.engine_state.voices.is_empty());
    // clocks
    let Command::Tc(tc) = lok(&mut cp, "clock slow 500ms") else {
        panic!()
    };
    assert_eq!(
        (tc.tempo.unit, tc.tempo.interval),
        (TempoUnit::Millis, 500.0)
    );
    let Command::Tc(tc) = lok(&mut cp, "clock fr 24000f") else {
        panic!()
    };
    assert_eq!(tc.tempo.unit, TempoUnit::Samples);
}

#[test]
fn incomplete_then_complete() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\"");
    assert!(matches!(cp.parse("group drums {"), Ok(Parsed::Local(t)) if t.is_empty()));
    assert!(cp.pending());
    assert!(cp.engine_state.groups.is_empty());
    let Command::Group(g) = lok(&mut cp, "kick }") else {
        panic!("not a Group")
    };
    assert!(!cp.pending());
    assert_eq!(g.members.len(), 1);

    // an error drops the pending text
    assert!(cp.parse("group g2 {").is_ok());
    assert!(cp.pending());
    assert!(cp.parse("nope }").is_err());
    assert!(!cp.pending());
}

#[test]
fn submit_maps_incomplete_to_none() {
    let mut cp = lang_processor();
    let (mut tx, mut rx) = cmd_queue(4);
    assert!(matches!(cp.submit("group drums {", &mut tx), Ok(None)));
    assert!(rx.try_pop().is_none());
    assert!(matches!(cp.submit("}", &mut tx), Ok(None)));
    assert!(matches!(rx.try_pop(), Some(Command::Group(_))));
    // a batch that ends inside a block is an error and changes nothing
    let lines = vec![
        "voice kick from \"kick\"".to_string(),
        "group g {".to_string(),
    ];
    assert!(cp.submit_batch(&lines, &mut tx).is_err());
    assert!(!cp.pending());
    assert!(cp.engine_state.voices.is_empty());
}

#[test]
fn script_is_one_unit() {
    let mut cp = lang_processor();
    let cmds = batch(lok(
        &mut cp,
        "voice a from \"kick\"\nvoice b from \"hat\"\nstart a b",
    ));
    assert_eq!(names(&cmds), ["Load", "Load", "Start", "Start"]);
    // a later line's error undoes the earlier ones
    lerr(&mut cp, "voice c from \"snare\"\nstart nope");
    assert!(!cp.engine_state.voice_names.contains_key("c"));
}

#[test]
fn remove_main_refused() {
    let mut cp = lang_processor();
    let errs = lerr(&mut cp, "remove main");
    assert_eq!(errs[0].code, Code::MainNotRemovable);
    assert!(cp.engine_state.context_names.contains_key("main"));
}

#[test]
fn remove_voice_and_quit() {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice kick from \"kick\" and voice hat from \"hat\"",
    );
    lok(&mut cp, "group drums { hat }");
    let Command::Unload(u) = lok(&mut cp, "remove drums.hat") else {
        panic!("not an Unload")
    };
    assert_eq!(u.voice, VoiceId(1));
    assert!(matches!(lok(&mut cp, "remove kick"), Command::Unload(_)));
    assert!(matches!(lok(&mut cp, "quit"), Command::Quit(_)));
    assert!(matches!(lok(&mut cp, "exit"), Command::Quit(_)));
    // the name is free again
    lok(&mut cp, "voice kick from \"kick\"");
}

#[test]
fn limits_map_to_e601() {
    let limits = crate::audio_processing::engine::arena::Limits {
        voices: 1,
        ..Default::default()
    };
    let mut cp = processor_with(&["kick", "hat"], limits);
    cp.boot();
    lok(&mut cp, "voice a from \"kick\"");
    let errs = lerr(&mut cp, "voice b from \"hat\"");
    assert_eq!(errs[0].code, Code::LimitReached);
}

#[test]
fn errors_print_one_per_line() {
    let mut cp = lang_processor();
    let err = cp
        .parse("voice a from \"kick\" gian=1 pna=2")
        .err()
        .unwrap();
    let text = err.to_string();
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(text.lines().all(|l| l.contains("E401")));
}

// ---- seqs (LANGc) ----

// the command a line made, without the `SetShifters` that a pitch or stretch
// target (or a melo) adds in front of it
fn without_shifters(cmd: Command) -> Command {
    let Command::Batch(BatchArgs { cmds }) = cmd else {
        return cmd;
    };
    let mut rest: Vec<Command> = cmds
        .into_iter()
        .filter(|c| !matches!(c, Command::SetShifters(_)))
        .collect();
    match rest.len() {
        1 => rest.remove(0),
        _ => Command::Batch(BatchArgs { cmds: rest }),
    }
}

fn seq_args(cmd: Command) -> SeqArgs {
    match without_shifters(cmd) {
        Command::Seq(args) => args,
        other => panic!("expected a Seq, got a {}", name_of(&other)),
    }
}

// a booted session with the voices kick (own clock 1000ms) and hat (follows main)
fn seq_session() -> CmdProcessor {
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\" on 1000ms");
    lok(&mut cp, "voice hat from \"hat\"");
    cp
}

fn proc_of(cp: &CmdProcessor, voice: &str, name: &str) -> ProcRepr {
    let es = &cp.engine_state;
    es.procs[&es.voices[&es.voice_id(voice).unwrap()].procs[name]].clone()
}

#[test]
fn seq_four_on_the_floor() {
    let mut cp = seq_session();
    let args = seq_args(lok(&mut cp, "kick { seq beat 1 2 3 4 in 4 }"));
    let kick = cp.engine_state.voice_id("kick").unwrap();
    assert_eq!(args.owner, ProcOwner::Voice(kick));
    assert_eq!(args.steps, vec![0.0, 1.0, 2.0, 3.0]);
    assert_eq!(args.period, 4);
    assert_eq!(args.chance, vec![100.0; 4]);
    assert_eq!(args.jit, vec![0.0; 16]);
    // it shares kick's tempo
    let kick_tempo = cp.engine_state.voices[&kick].tempo;
    assert_eq!(args.tempo, TempoUse::Shared(kick_tempo));
    let repr = proc_of(&cp, "kick", "beat");
    assert_eq!(repr.link, TempoLink::Voice);
    assert_eq!(repr.tempo, kick_tempo);
    assert_eq!(repr.seq.period, 4);
    assert_eq!(repr.seq.steps.len(), 4);
}

#[test]
fn seq_default_period() {
    let mut cp = seq_session();
    lok(&mut cp, "voice pad from \"snare\" in 8");
    let args = seq_args(lok(&mut cp, "kick { seq beat 1 3 }"));
    assert_eq!(args.period, 4);
    assert_eq!(args.steps, vec![0.0, 2.0]);
    // the owner's period, not 4
    let args = seq_args(lok(&mut cp, "pad { seq long 1 7 }"));
    assert_eq!(args.period, 8);
    assert_eq!(args.steps, vec![0.0, 6.0]);
}

#[test]
fn seq_marks() {
    let mut cp = seq_session();
    let args = seq_args(lok(&mut cp, "hat { seq off 1.5 2.5 3.5 4.5%40 }"));
    assert_eq!(args.steps, vec![0.5, 1.5, 2.5, 3.5]);
    assert_eq!(args.chance, vec![100.0, 100.0, 100.0, 40.0]);

    let args = seq_args(lok(&mut cp, "kick { seq back 2 4 late=0.03 }"));
    // [early_min, early_max, late_min, late_max] per step
    assert_eq!(args.jit, vec![0.0, 0.0, 0.0, 0.03, 0.0, 0.0, 0.0, 0.03]);
    // `~a` drifts both ways
    let args = seq_args(lok(&mut cp, "kick { seq both 1~0.25 }"));
    assert_eq!(args.jit, vec![0.0, 0.25, 0.0, 0.25]);
    // both marks, either order
    let args = seq_args(lok(&mut cp, "kick { seq mix 1%50~0.5 2~0.5%25 }"));
    assert_eq!(args.chance, vec![50.0, 25.0]);
    assert_eq!(args.jit, vec![0.0, 0.5, 0.0, 0.5, 0.0, 0.5, 0.0, 0.5]);
}

#[test]
fn seq_ranges() {
    let mut cp = seq_session();
    let args = seq_args(lok(&mut cp, "kick { seq all4 1..4%50 }"));
    assert_eq!(args.steps, vec![0.0, 1.0, 2.0, 3.0]);
    assert_eq!(args.chance, vec![50.0; 4]);
    // a range mixes with single steps; a fractional start steps by 1
    let args = seq_args(lok(&mut cp, "hat { seq mixed 1.5..3.5 4 }"));
    assert_eq!(args.steps, vec![0.5, 1.5, 2.5, 3.0]);
}

#[test]
fn seq_errors() {
    let mut cp = seq_session();
    let e = lerr(&mut cp, "kick { seq a 0 }");
    assert_eq!(e[0].code, Code::StepOutsideCycle);
    assert!(e[0].message.contains("counting starts at 1"), "{}", e[0]);
    assert_eq!(
        lerr(&mut cp, "kick { seq a 5 in 4 }")[0].code,
        Code::StepOutsideCycle
    );
    // the end of the cycle is not in it
    assert_eq!(
        lerr(&mut cp, "kick { seq b 5 in 4 }")[0].code,
        Code::StepOutsideCycle
    );
    assert_eq!(
        lerr(&mut cp, "kick { seq a 1%150 }")[0].code,
        Code::OutOfRange
    );
    assert_eq!(
        lerr(&mut cp, "kick { seq a 1~-1 }")[0].code,
        Code::OutOfRange
    );
    assert_eq!(lerr(&mut cp, "kick { seq a 1 1 }")[0].code, Code::WrongType);
    assert_eq!(
        lerr(&mut cp, "kick { seq a 1..2 2 }")[0].code,
        Code::WrongType
    );
    assert_eq!(
        lerr(&mut cp, "kick { seq a 3..1 }")[0].code,
        Code::WrongType
    );
    // steps are required when a seq is made
    assert_eq!(
        lerr(&mut cp, "kick { seq a in 4 }")[0].code,
        Code::MissingItem
    );
    assert_eq!(
        lerr(&mut cp, "kick { seq a 1 in 0 }")[0].code,
        Code::OutOfRange
    );
    // every bad step is reported
    assert_eq!(lerr(&mut cp, "kick { seq a 0 9 1%200 in 4 }").len(), 3);

    // a seq outside an owner has nothing to trigger without a pipe
    lok(&mut cp, "kick { seq beat 1 }");
    for line in ["seq a 1", "seq a 1 | trig"] {
        assert_eq!(
            lerr(&mut cp, line)[0].code,
            Code::NothingToTrigger,
            "{line}"
        );
    }
    assert_eq!(
        lerr(&mut cp, "kick { seq x 1 seed=-3 }")[0].code,
        Code::OutOfRange
    );
    // other names and paths
    assert_eq!(lerr(&mut cp, "seq nope.a 1")[0].code, Code::UnknownName);
    assert_eq!(lerr(&mut cp, "seq main.a 1")[0].code, Code::WrongType);
    assert_eq!(
        lerr(&mut cp, "kick { seq all 1 }")[0].code,
        Code::ReservedWord
    );
    assert_eq!(lerr(&mut cp, "kick.nope 1")[0].code, Code::UnknownName);
    // none of it changed the mirror
    assert_eq!(cp.engine_state.procs.len(), 1);
}

#[test]
fn seq_path_form() {
    let mut a = seq_session();
    let mut b = seq_session();
    let block = seq_args(lok(&mut a, "kick { seq beat 1 3 on 90b }"));
    let path = seq_args(lok(&mut b, "seq kick.beat 1 3 on 90b"));
    assert_eq!(block.steps, path.steps);
    assert_eq!(block.period, path.period);
    assert_eq!(block.chance, path.chance);
    assert_eq!(block.owner, path.owner);
    assert!(matches!(
        (block.tempo, path.tempo),
        (TempoUse::Own(x), TempoUse::Own(y)) if x == y
    ));
    // a restated seq is reached by its path alone, grouped or not
    assert!(local(&mut b, "kick.beat 1 3 on 90b").contains("W101"));
    lok(&mut b, "hat { seq off 2 }");
    lok(&mut b, "group drums { hat }");
    assert!(local(&mut b, "drums.hat.off 2").contains("W101"));
    assert!(local(&mut b, "hat.off 2").contains("W101"));
}

#[test]
fn seq_restate_noop() {
    let mut cp = seq_session();
    lok(&mut cp, "kick { seq beat 1 2 3 4 in 4 }");
    let before = (
        cp.engine_state.ids.procs.live(),
        cp.engine_state.ids.tempos.live(),
    );
    for line in [
        "kick { seq beat 1 2 3 4 in 4 }",
        "kick { seq beat 1 2 3 4 }",
        "kick { seq beat 1..4 }",
        "seq kick.beat 1 2 3 4 in 4 | trig",
        "kick.beat | trig",
        "kick.beat",
    ] {
        let text = local(&mut cp, line);
        assert!(text.contains("W101"), "{line}: {text}");
    }
    assert_eq!(
        before,
        (
            cp.engine_state.ids.procs.live(),
            cp.engine_state.ids.tempos.live()
        )
    );
    // different values edit it
    for (line, steps) in [
        ("kick { seq beat 1 2 3 }", 3),
        ("kick { seq beat 1 2 3 4%50 }", 4),
        ("kick { seq beat 1 2 3 4 in 8 }", 4),
    ] {
        assert_eq!(
            names(&batch_or_one(lok(&mut cp, line))),
            ["SeqEdit"],
            "{line}"
        );
        assert_eq!(
            proc_of(&cp, "kick", "beat").seq.steps.len(),
            steps,
            "{line}"
        );
    }
    assert_eq!(proc_of(&cp, "kick", "beat").seq.period, 8);
    // restating the clock it has is a no-op too
    lok(&mut cp, "kick { seq own 1 on 93b }");
    assert!(local(&mut cp, "kick { seq own 1 on 93b }").contains("W101"));
    lok(&mut cp, "kick { seq pinned 1 on main }");
    assert!(local(&mut cp, "kick.pinned on main").contains("W101"));
}

#[test]
fn seq_pipe_trig_is_owner() {
    let mut a = seq_session();
    let mut b = seq_session();
    let plain = seq_args(lok(&mut a, "kick { seq beat 1 2 3 4 }"));
    let piped = seq_args(lok(&mut b, "kick { seq beat 1 2 3 4 | trig }"));
    assert_eq!(plain.owner, piped.owner);
    assert_eq!(plain.tempo, piped.tempo);
    assert_eq!(plain.period, piped.period);
    assert_eq!(plain.steps, piped.steps);
    assert_eq!(plain.chance, piped.chance);
    assert_eq!(plain.jit, piped.jit);
}

#[test]
fn seq_clock_forms() {
    let mut cp = seq_session();
    lok(&mut cp, "clock slow 60b");
    lok(&mut cp, "group drums on 93b");
    // a clock, a group's clock, or a clock of its own
    let on_clock = seq_args(lok(&mut cp, "kick { seq a 1 on slow }"));
    let slow = cp.engine_state.context_id("slow").unwrap();
    assert_eq!(on_clock.tempo, TempoUse::Shared(slow));
    assert_eq!(proc_of(&cp, "kick", "a").link, TempoLink::Context);
    let g = cp.engine_state.group_id("drums").unwrap();
    let on_group = seq_args(lok(&mut cp, "kick { seq b 1 on drums }"));
    let group_tempo = cp.engine_state.groups[&g].tempo;
    assert_eq!(on_group.tempo, TempoUse::Shared(group_tempo));
    assert_eq!(proc_of(&cp, "kick", "b").link, TempoLink::Group(g));
    let own = seq_args(lok(&mut cp, "kick { seq c 1 on 120b }"));
    let TempoUse::Own(t) = own.tempo else {
        panic!("not an own tempo")
    };
    assert_eq!((t.unit, t.interval), (TempoUnit::Bpm, 120.0));
    assert_eq!(
        cp.engine_state.tempos[&t.id].owner,
        TempoOwner::Voice(cp.engine_state.voice_id("kick").unwrap())
    );
    assert_eq!(
        lerr(&mut cp, "kick { seq d 1 on nope }")[0].code,
        Code::UnknownName
    );
    // a rejected seq took nothing
    assert_eq!(cp.engine_state.procs.len(), 3);
}

#[test]
fn seq_follows_its_owner_into_a_group() {
    let mut cp = seq_session();
    // hat follows main; its seq follows hat
    lok(&mut cp, "hat { seq follow 1 3 }");
    let hat = cp.engine_state.voice_id("hat").unwrap();
    assert_eq!(cp.engine_state.voices[&hat].tempo, TempoId(0));
    assert_eq!(proc_of(&cp, "hat", "follow").tempo, TempoId(0));
    lok(&mut cp, "group drums on 93b { hat }");
    let g = cp.engine_state.group_id("drums").unwrap();
    let gt = cp.engine_state.groups[&g].tempo;
    assert_eq!(proc_of(&cp, "drums.hat", "follow").tempo, gt);
    assert_eq!(proc_of(&cp, "drums.hat", "follow").link, TempoLink::Voice);
}

#[test]
fn seq_on_main_moves_with_a_grouped_voice() {
    // A seq with an explicit `on main` on a voice that follows main reads
    // the same tempo as the voice, so when the voice is grouped the adopt
    // moves the seq to the group's clock with it (the engine does the same;
    // see engine/group_tests.rs). To keep a seq on main, give the voice a
    // clock of its own.
    let mut cp = seq_session();
    lok(&mut cp, "hat { seq pinned 1 3 on main }");
    assert_eq!(proc_of(&cp, "hat", "pinned").tempo, TempoId(0));
    lok(&mut cp, "group drums on 93b { hat }");
    let g = cp.engine_state.group_id("drums").unwrap();
    let gt = cp.engine_state.groups[&g].tempo;
    assert_ne!(gt, TempoId(0));
    assert_eq!(proc_of(&cp, "drums.hat", "pinned").tempo, gt);
    assert_eq!(proc_of(&cp, "drums.hat", "pinned").link, TempoLink::Context);
    // main itself is untouched
    assert!(cp.engine_state.tempos.contains_key(&TempoId(0)));

    // a voice with its own clock keeps `on main` seqs where they are
    let mut cp = seq_session();
    lok(&mut cp, "kick { seq pinned 1 3 on main }");
    lok(&mut cp, "group drums { kick }");
    assert_eq!(proc_of(&cp, "drums.kick", "pinned").tempo, TempoId(0));
}

#[test]
fn seq_transport_and_remove_paths() {
    let mut cp = seq_session();
    lok(&mut cp, "kick { seq beat 1 }");
    for (line, ops) in [
        ("pause kick.beat", vec![ProcOp::Disable]),
        ("resume kick.beat", vec![ProcOp::Enable]),
        ("start kick.beat", vec![ProcOp::Enable, ProcOp::Reset]),
        ("stop kick.beat", vec![ProcOp::Disable, ProcOp::Reset]),
    ] {
        let got: Vec<ProcOp> = batch_or_one(lok(&mut cp, line))
            .into_iter()
            .map(|c| match c {
                Command::ProcEdit(a) => a.op,
                _ => panic!("{line}: not a ProcEdit"),
            })
            .collect();
        assert_eq!(got, ops, "{line}");
    }
    // paused is in the mirror, and `state` doesn't show it
    lok(&mut cp, "pause kick.beat");
    assert!(!proc_of(&cp, "kick", "beat").enabled);
    assert!(!cp.state(None).unwrap().contains("pause"));
    lok(&mut cp, "start all");
    assert!(proc_of(&cp, "kick", "beat").enabled);
    // a removed seq leaves the mirror and frees its ID
    let removed = batch_or_one(lok(&mut cp, "remove kick.beat"));
    assert!(matches!(removed[0], Command::ProcEdit(ref a) if a.op == ProcOp::Remove));
    assert!(cp.engine_state.procs.is_empty());
    assert_eq!(cp.engine_state.ids.procs.live(), 0);
    lok(&mut cp, "kick { seq beat 1 }");
    // removing the owner takes the seq (and its mirror entry) with it
    assert!(matches!(lok(&mut cp, "remove kick"), Command::Unload(_)));
    assert!(cp.engine_state.procs.is_empty());
    assert_eq!(cp.engine_state.ids.procs.live(), 0);
}

#[test]
fn seq_declared_with_its_voice() {
    let mut cp = lang_processor();
    let cmds = batch(lok(
        &mut cp,
        "voice kick from \"kick\" rate=2 { seq beat 1 2 3 4 }",
    ));
    assert_eq!(names(&cmds), ["Load", "SetParam", "Seq"]);
    // a failing seq undoes the voice too (G3)
    lerr(&mut cp, "voice hat from \"hat\" { seq beat 9 }");
    assert!(!cp.engine_state.voice_names.contains_key("hat"));
    assert_eq!(cp.engine_state.ids.procs.live(), 1);
}

#[test]
fn seq_limit_maps_to_e601() {
    let limits = crate::audio_processing::engine::arena::Limits {
        procs: 1,
        ..Default::default()
    };
    let mut cp = processor_with(&["kick"], limits);
    cp.boot();
    lok(&mut cp, "voice kick from \"kick\"");
    lok(&mut cp, "kick { seq a 1 }");
    let before = cp.engine_state.ids.tempos.live();
    assert_eq!(
        lerr(&mut cp, "kick { seq b 1 on 90b }")[0].code,
        Code::LimitReached
    );
    assert_eq!(cp.engine_state.ids.tempos.live(), before);
}

#[test]
fn clear_parses_and_requests_clear() {
    let mut cp = lang_processor();
    assert!(!cp.clear_requested);
    // nothing to print and nothing for the engine
    assert_eq!(local(&mut cp, "clear"), "");
    assert!(cp.clear_requested);
    // the REPL takes the request
    cp.clear_requested = false;
    local(&mut cp, "state");
    assert!(!cp.clear_requested);
    // a line that fails doesn't leave a request behind
    lerr(&mut cp, "clear and voice x from \"nope\"");
    assert!(!cp.clear_requested);
}

#[test]
fn clear_is_reserved() {
    let mut cp = lang_processor();
    let errs = lerr(&mut cp, "voice clear from \"kick\"");
    assert_eq!(errs[0].code, Code::ReservedWord);
    assert!(errs[0].to_string().contains("clear"), "{}", errs[0]);
}

// ---- LANGe: clocks spelled like durations, short forms, step feel ----

fn clock_of(cp: &CmdProcessor, name: &str) -> (TempoUnit, f32) {
    let id = cp.engine_state.context_id(name).unwrap();
    let t = &cp.engine_state.tempos[&id];
    (t.unit, t.interval)
}

#[test]
fn clock_spellings() {
    let mut cp = lang_processor();
    lok(&mut cp, "clock a 93b");
    lok(&mut cp, "clock b 500ms");
    lok(&mut cp, "clock c 0.5s");
    lok(&mut cp, "clock d 24000f");
    local(&mut cp, "let n = 60");
    lok(&mut cp, "clock e ($n * 2)b");
    local(&mut cp, "let c = 93b");
    lok(&mut cp, "clock f $c");
    assert_eq!(clock_of(&cp, "a"), (TempoUnit::Bpm, 93.0));
    assert_eq!(clock_of(&cp, "b"), (TempoUnit::Millis, 500.0));
    assert_eq!(clock_of(&cp, "c"), (TempoUnit::Millis, 500.0));
    assert_eq!(clock_of(&cp, "d"), (TempoUnit::Samples, 24000.0));
    assert_eq!(clock_of(&cp, "e"), (TempoUnit::Bpm, 120.0));
    assert_eq!(clock_of(&cp, "f"), (TempoUnit::Bpm, 93.0));
}

#[test]
fn old_clock_spelling_is_a_name() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice b93 from \"kick\"");
    assert!(cp.engine_state.voice_id("b93").is_ok());
    assert!(!lerr(&mut cp, "clock x b93").is_empty());
}

#[test]
fn clock_needs_unit() {
    let mut cp = lang_processor();
    local(&mut cp, "let n = 93");
    let e = lerr(&mut cp, "clock x $n");
    assert_eq!(e[0].code, Code::WrongType);
    assert!(
        e[0].message.contains("a clock needs a unit: ($x)b"),
        "{}",
        e[0]
    );
}

#[test]
fn short_forms_work() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\" r=1.5");
    let id = cp.engine_state.voice_id("kick").unwrap();
    assert_eq!(
        cp.engine_state.voices[&id].params[VoiceParam::Rate.index()],
        1.5
    );
    let e = lerr(&mut cp, "kick r=1 rate=2");
    assert!(e[0].message.contains("one value"), "{}", e[0]);
}

fn feel_of(cp: &CmdProcessor, name: &str) -> Vec<(f32, f32, f32)> {
    proc_of(cp, "kick", name)
        .seq
        .steps
        .iter()
        .map(|s| (s.chance, s.early, s.late))
        .collect()
}

#[test]
fn feel_settings_lower() {
    let mut cp = seq_session();
    lok(&mut cp, "kick { seq g 1 2 late=0.04 }");
    assert_eq!(feel_of(&cp, "g"), [(100.0, 0.0, 0.04), (100.0, 0.0, 0.04)]);
    lok(&mut cp, "kick { seq h 1 2 jitter=0.03 }");
    assert_eq!(
        feel_of(&cp, "h"),
        [(100.0, 0.03, 0.03), (100.0, 0.03, 0.03)]
    );
    lok(&mut cp, "kick { seq i 1 2 early=0.02 }");
    assert_eq!(feel_of(&cp, "i"), [(100.0, 0.02, 0.0), (100.0, 0.02, 0.0)]);
    lok(&mut cp, "kick { seq j 1 2%35 chance=70 }");
    assert_eq!(feel_of(&cp, "j"), [(70.0, 0.0, 0.0), (35.0, 0.0, 0.0)]);
    // short forms
    lok(&mut cp, "kick { seq k 1 2 l=0.1 c=50 }");
    assert_eq!(feel_of(&cp, "k"), [(50.0, 0.0, 0.1), (50.0, 0.0, 0.1)]);
}

#[test]
fn feel_in_time_units() {
    // main runs at 120b: one tick is 0.5 s
    let mut cp = seq_session();
    lok(&mut cp, "hat { seq g 1 late=10ms }");
    let s = proc_of(&cp, "hat", "g").seq.steps[0];
    assert!((s.late - 0.02).abs() < 1e-6, "{}", s.late);
    // an own clock of 1000ms: 250ms is a quarter tick
    lok(&mut cp, "kick { seq g 1 early=250ms }");
    let s = proc_of(&cp, "kick", "g").seq.steps[0];
    assert!((s.early - 0.25).abs() < 1e-6, "{}", s.early);
    // `b` is ticks
    lok(&mut cp, "kick { seq h 1 late=2b }");
    assert_eq!(proc_of(&cp, "kick", "h").seq.steps[0].late, 2.0);
}

#[test]
fn jitter_and_late_conflict() {
    let mut cp = seq_session();
    for t in [
        "kick { seq g 1 jitter=0.1 late=0.1 }",
        "kick { seq g 1 early=0.1 jitter=0.1 }",
    ] {
        let e = lerr(&mut cp, t);
        assert_eq!(e[0].code, Code::WrongType, "{t}");
        assert!(e[0].message.contains("not both"), "{}", e[0]);
    }
}

#[test]
fn step_tilde_overrides() {
    let mut cp = seq_session();
    lok(&mut cp, "kick { seq g 1 2~0.05 late=0.04 }");
    assert_eq!(feel_of(&cp, "g"), [(100.0, 0.0, 0.04), (100.0, 0.05, 0.05)]);
    lok(&mut cp, "kick { seq h 1 2%35~0.05 }");
    lok(&mut cp, "kick { seq i 1 2~0.05%35 }");
    assert_eq!(feel_of(&cp, "h")[1], (35.0, 0.05, 0.05));
    assert_eq!(feel_of(&cp, "h"), feel_of(&cp, "i"));
}

#[test]
fn tilde_range_is_gone() {
    let mut cp = seq_session();
    let e = lerr(&mut cp, "kick { seq g 1~0..0.03 }");
    assert_eq!(e[0].code, Code::Unexpected);
    let hint = e[0].suggestion.clone().unwrap_or_default();
    assert!(hint.contains("late=0.04"), "{hint}");
}

// ---- TMP: retime, remove and move clocks ----

/// Sessions that run source text through a `CmdProcessor` into a `Conductor`
/// (1 channel, so a tempo ticks once per frame).
mod clocks {
    use super::*;
    use crate::audio_processing::blast_time::sample_rate;
    use crate::audio_processing::engine::Conductor;
    use crate::audio_processing::engine::arena::Limits;
    use crate::audio_processing::test_util::{pcm, render_i16, run, setup};
    use crate::file_parsing::decode_helpers::AudioFile;

    // first sample -1 and every other 1, so a frame renders -1 only when
    // the voice is (re)triggered
    fn spike(name: &str) -> AudioFile {
        let mut samples = vec![1i16; 400_000];
        samples[0] = -1;
        AudioFile::new(name, "wav", 48000, 1, 16, pcm(&samples))
    }

    fn session() -> (CmdProcessor, Conductor) {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![spike("kick"), spike("hat"), spike("snare")]);
        c.apply(cp.boot());
        (cp, c)
    }

    fn triggers(out: &[i16]) -> Vec<usize> {
        out.iter()
            .enumerate()
            .filter(|(_, s)| **s == -1)
            .map(|(f, _)| f)
            .collect()
    }

    fn near(got: &[usize], want: &[usize]) {
        assert_eq!(got.len(), want.len(), "got {got:?}, want {want:?}");
        for (g, w) in got.iter().zip(want) {
            assert!(g.abs_diff(*w) <= 1, "got {got:?}, want {want:?}");
        }
    }

    fn tempo_id(cp: &CmdProcessor, voice: &str) -> TempoId {
        let es = &cp.engine_state;
        es.voices[&es.voice_id(voice).unwrap()].tempo
    }

    fn link_of(cp: &CmdProcessor, voice: &str) -> TempoLink {
        let es = &cp.engine_state;
        es.voices[&es.voice_id(voice).unwrap()].link
    }

    fn vid_of(cp: &CmdProcessor, name: &str) -> VoiceId {
        cp.engine_state.voice_id(name).unwrap()
    }

    fn gid_of(cp: &CmdProcessor, name: &str) -> GroupId {
        cp.engine_state.group_id(name).unwrap()
    }

    fn state(cp: &mut CmdProcessor) -> String {
        match cp.parse("state") {
            Ok(Parsed::Local(text)) => text,
            _ => panic!("state is local"),
        }
    }

    /// Every tempo in the mirror is live in the engine and the other way
    /// round, and every voice, group and seq reads the same live tempo on
    /// both sides.
    fn tempo_ids_agree(cp: &CmdProcessor, c: &Conductor) {
        let es = &cp.engine_state;
        assert_eq!(c.tempos.len(), es.tempos.len(), "tempo counts");
        for id in es.tempos.keys() {
            assert!(c.tempos.get(id.index()).is_some(), "{id:?} not live");
        }
        assert_eq!(es.ids.tempos.live() as usize, es.tempos.len());
        for (id, v) in &es.voices {
            assert!(es.tempos.contains_key(&v.tempo), "voice {}", v.name);
            let ev = c.voices.get(id.index()).unwrap();
            assert_eq!(ev.state.tempo, v.tempo, "voice {}", v.name);
        }
        for (id, g) in &es.groups {
            assert!(es.tempos.contains_key(&g.tempo), "group {}", g.name);
            let eg = c.groups.get(id.index()).unwrap();
            assert_eq!(eg.state.tempo, g.tempo, "group {}", g.name);
        }
        for (id, p) in &es.procs {
            assert!(es.tempos.contains_key(&p.tempo), "seq {}", p.name);
            assert_eq!(c.procs.get(id.index()).unwrap().tempo, p.tempo, "seq");
        }
    }

    type Snapshot = (
        Vec<(String, TempoId, TempoLink)>,
        Vec<(TempoId, TempoOwner, u32)>,
        Vec<(String, TempoId)>,
        u32,
    );

    /// Every tempo, link and clock name in the mirror.
    fn snapshot(cp: &CmdProcessor) -> Snapshot {
        let es = &cp.engine_state;
        let mut objects: Vec<(String, TempoId, TempoLink)> = Vec::new();
        objects.extend(
            es.voices
                .values()
                .map(|v| (v.name.clone(), v.tempo, v.link)),
        );
        objects.extend(
            es.groups
                .values()
                .map(|g| (g.name.clone(), g.tempo, g.link)),
        );
        objects.extend(es.procs.values().map(|p| (p.name.clone(), p.tempo, p.link)));
        let tempos = es
            .tempos
            .values()
            .map(|t| (t.id, t.owner, t.interval.to_bits()))
            .collect();
        let mut names: Vec<(String, TempoId)> = es
            .context_names
            .iter()
            .map(|(n, t)| (n.clone(), *t))
            .collect();
        names.sort();
        (objects, tempos, names, es.ids.tempos.live())
    }

    fn steps_of(c: &Conductor, t: TempoId) -> f64 {
        c.tempos.get(t.index()).unwrap().state.steps()
    }

    // ---- retime ----

    // `kick` on the clock `clk` at `rate`, with a seq on steps 1 and 3 of 4;
    // all started
    fn seq_on_clk(rate: &str) -> (CmdProcessor, Conductor) {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, &format!("clock clk {rate}"));
        run(&mut cp, &mut c, "voice kick from \"kick\" on clk");
        run(&mut cp, &mut c, "kick { seq s 1 3 }");
        run(&mut cp, &mut c, "start clk and start kick");
        (cp, c)
    }

    #[test]
    fn seq_keeps_phase_when_clock_speeds_up() {
        let (mut cp, mut c) = seq_on_clk("1000f");
        let out = render_i16(&mut c, 1, 2500);
        assert_eq!(triggers(&out), vec![0, 2000]);
        // 2.5 steps in; the next step is 4.0, 1.5 steps of 500 away. Without
        // the anchor the position would jump to 5.0 and skip the step at 4.
        run(&mut cp, &mut c, "clock clk 500f");
        let out = render_i16(&mut c, 1, 2500);
        near(&triggers(&out), &[750, 1750]);
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn seq_keeps_phase_when_clock_slows_down() {
        let (mut cp, mut c) = seq_on_clk("500f");
        let out = render_i16(&mut c, 1, 1250);
        assert_eq!(triggers(&out), vec![0, 1000]);
        // 2.5 steps in; the step at 4.0 is 1.5 steps of 1000 away. Without
        // the anchor the position would fall back to 1.25 and fire step 2 again.
        run(&mut cp, &mut c, "clock clk 1000f");
        let out = render_i16(&mut c, 1, 4000);
        near(&triggers(&out), &[1500, 3500]);
    }

    #[test]
    fn retime_right_after_a_step_doesnt_refire() {
        let (mut cp, mut c) = seq_on_clk("1000f");
        let out = render_i16(&mut c, 1, 2002);
        assert_eq!(triggers(&out), vec![0, 2000]);
        run(&mut cp, &mut c, "clock clk 500f");
        let out = render_i16(&mut c, 1, 2000);
        // 4.0 is 1.998 steps of 500 away, and 6.0 is 1000 later
        near(&triggers(&out), &[999, 1999]);
    }

    #[test]
    fn retime_main_prints_in_state() {
        let (mut cp, mut c) = session();
        let before = steps_of(&c, TempoId(0));
        run(&mut cp, &mut c, "clock main 124b");
        assert_eq!(steps_of(&c, TempoId(0)), before);
        assert!(state(&mut cp).contains("clock main 124b"));
        run(&mut cp, &mut c, "clock main 120b");
        assert!(!state(&mut cp).contains("clock main"));
    }

    #[test]
    fn retime_command_carries_the_rate_as_written() {
        let mut cp = lang_processor();
        lok(&mut cp, "clock slow 60b");
        for (line, unit, interval) in [
            ("clock slow 500ms", TempoUnit::Millis, 500.0),
            ("clock slow 1s", TempoUnit::Millis, 1000.0),
            ("clock slow 24000f", TempoUnit::Samples, 24000.0),
        ] {
            let Command::TempoSet(set) = lok(&mut cp, line) else {
                panic!("{line}: not a TempoSet")
            };
            assert_eq!((set.unit, set.interval), (unit, interval), "{line}");
            let repr = &cp.engine_state.tempos[&set.id];
            assert_eq!((repr.unit, repr.interval), (unit, interval), "{line}");
        }
        assert!(local(&mut cp, "clock slow 24000f").contains("W101"));
        assert_eq!(lerr(&mut cp, "clock slow 0b")[0].code, Code::OutOfRange);
    }

    // ---- remove ----

    #[test]
    fn remove_unused_clock() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "clock slow 60b");
        let slow = ctx(&cp, "slow");
        run(&mut cp, &mut c, "remove slow");
        let es = &cp.engine_state;
        assert!(!es.context_names.contains_key("slow"));
        assert!(!es.tempos.contains_key(&slow));
        assert_eq!(es.ids.tempos.live(), 1);
        assert!(c.tempos.get(slow.index()).is_none());
        tempo_ids_agree(&cp, &c);
        // the name and the ID can be used again
        run(&mut cp, &mut c, "clock slow 90b");
        assert_eq!(ctx(&cp, "slow"), slow);
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn remove_used_clock_lists_users() {
        let mut cp = lang_processor();
        lok(&mut cp, "clock slow 60b");
        lok(&mut cp, "voice clave from \"kick\" on slow");
        lok(&mut cp, "clave { seq pulse 1 }");
        let before = snapshot(&cp);
        let errs = lerr(&mut cp, "remove slow");
        assert_eq!(errs[0].code, Code::InUse);
        assert_eq!(errs[0].code.as_str(), "E407");
        assert_eq!(
            errs[0].message,
            "can't remove clock 'slow': used by voice clave, seq clave.pulse"
        );
        assert_eq!(snapshot(&cp), before);
        // once the users are gone it goes
        lok(&mut cp, "remove clave");
        assert!(matches!(lok(&mut cp, "remove slow"), Command::TempoFree(_)));
    }

    #[test]
    fn remove_main_refused() {
        let mut cp = lang_processor();
        let before = snapshot(&cp);
        let errs = lerr(&mut cp, "remove main");
        assert_eq!(errs[0].code, Code::MainNotRemovable);
        assert!(errs[0].message.contains("session's clock"), "{}", errs[0]);
        assert_eq!(snapshot(&cp), before);
    }

    #[test]
    fn remove_followed_voice_refused() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "voice kick from \"kick\" on 93b");
        run(&mut cp, &mut c, "voice hat from \"hat\" on kick");
        run(&mut cp, &mut c, "hat { seq s 1 }");
        let before = snapshot(&cp);
        let errs = lerr(&mut cp, "remove kick");
        assert_eq!(errs[0].code, Code::InUse);
        assert_eq!(
            errs[0].message,
            "can't remove voice 'kick': used by voice hat"
        );
        assert_eq!(snapshot(&cp), before);
        // a voice's own seqs don't hold it back
        run(&mut cp, &mut c, "remove hat");
        run(&mut cp, &mut c, "kick { seq own 1 on kick }");
        run(&mut cp, &mut c, "remove kick");
        tempo_ids_agree(&cp, &c);
        assert_eq!(cp.engine_state.ids.tempos.live(), 1);
    }

    // ---- move ----

    #[test]
    fn move_voice_to_named_clock() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "clock slow 60b");
        run(&mut cp, &mut c, "voice kick from \"kick\" on 93b");
        run(&mut cp, &mut c, "kick { seq a 1 and seq b 2 on 90b }");
        let (slow, own) = (ctx(&cp, "slow"), tempo_id(&cp, "kick"));
        let b_tempo = proc_of(&cp, "kick", "b").tempo;
        assert_ne!(own, slow);
        tempo_ids_agree(&cp, &c);

        run(&mut cp, &mut c, "kick on slow");
        assert_eq!(tempo_id(&cp, "kick"), slow);
        assert_eq!(link_of(&cp, "kick"), TempoLink::Context);
        // the seq that follows its voice goes along; the one on a clock of
        // its own stays
        assert_eq!(proc_of(&cp, "kick", "a").tempo, slow);
        assert_eq!(proc_of(&cp, "kick", "b").tempo, b_tempo);
        // kick's own clock is freed in both
        assert!(!cp.engine_state.tempos.contains_key(&own));
        assert!(c.tempos.get(own.index()).is_none());
        tempo_ids_agree(&cp, &c);
        assert_eq!(cp.engine_state.ids.tempos.live(), 3);
    }

    #[test]
    fn move_voice_literal_retimes_own() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "voice hat from \"hat\" on 93b");
        run(&mut cp, &mut c, "start hat");
        render_i16(&mut c, 1, 5000);
        let own = tempo_id(&cp, "hat");
        let before = steps_of(&c, own);
        assert!(before > 0.0);

        let Command::TempoSet(set) = lok(&mut cp, "hat on 100b") else {
            panic!("not a TempoSet")
        };
        assert_eq!(
            (set.id, set.unit, set.interval),
            (own, TempoUnit::Bpm, 100.0)
        );
        run(&mut cp, &mut c, "hat on 100b");
        assert_eq!(tempo_id(&cp, "hat"), own);
        assert_eq!(cp.engine_state.tempos[&own].interval, 100.0);
        // the phase is kept
        assert_eq!(steps_of(&c, own), before);
        assert_eq!(cp.engine_state.ids.tempos.live(), 2);
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn move_doesnt_drag_sharers() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "clock clk 100b");
        run(&mut cp, &mut c, "voice a from \"kick\" on clk");
        run(&mut cp, &mut c, "voice b from \"hat\" on clk");
        let clk = ctx(&cp, "clk");
        run(&mut cp, &mut c, "a on 90b");
        assert_eq!(tempo_id(&cp, "b"), clk);
        assert_eq!(link_of(&cp, "b"), TempoLink::Context);
        assert_ne!(tempo_id(&cp, "a"), clk);
        assert_eq!(link_of(&cp, "a"), TempoLink::Own);
        let a = tempo_id(&cp, "a");
        assert_eq!(
            cp.engine_state.tempos[&a].owner,
            TempoOwner::Voice(vid_of(&cp, "a"))
        );
        // clk is a named clock: not freed
        assert!(cp.engine_state.tempos.contains_key(&clk));
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn move_group_moves_adopted_members() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "clock slow 60b");
        run(&mut cp, &mut c, "voice a from \"kick\"");
        run(&mut cp, &mut c, "voice b from \"hat\" on 90b");
        run(&mut cp, &mut c, "voice s from \"snare\"");
        run(&mut cp, &mut c, "a { seq q 1 }");
        run(&mut cp, &mut c, "group g on 240b { a; b; s }");
        let (g, slow) = (gid_of(&cp, "g"), ctx(&cp, "slow"));
        let old = cp.engine_state.groups[&g].tempo;
        let b = tempo_id(&cp, "g.b");
        tempo_ids_agree(&cp, &c);

        run(&mut cp, &mut c, "g on slow");
        let es = &cp.engine_state;
        assert_eq!(es.groups[&g].tempo, slow);
        assert_eq!(es.groups[&g].link, TempoLink::Context);
        // a and s adopted the group's clock, so they and a's seq move; b has
        // a clock of its own
        assert_eq!(tempo_id(&cp, "g.a"), slow);
        assert_eq!(link_of(&cp, "g.a"), TempoLink::Group(g));
        assert_eq!(proc_of(&cp, "g.a", "q").tempo, slow);
        assert_eq!(tempo_id(&cp, "g.s"), slow);
        assert_eq!(tempo_id(&cp, "g.b"), b);
        // the group's own clock is freed
        assert!(!es.tempos.contains_key(&old));
        assert!(c.tempos.get(old.index()).is_none());
        tempo_ids_agree(&cp, &c);

        // a member moved alone leaves the others where they are
        run(&mut cp, &mut c, "s on 77b");
        assert_eq!(tempo_id(&cp, "g.a"), slow);
        assert_ne!(tempo_id(&cp, "g.s"), slow);
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn move_group_to_a_literal_retimes_its_clock() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "voice a from \"kick\"");
        run(&mut cp, &mut c, "group g on 240b { a }");
        let g = gid_of(&cp, "g");
        let own = cp.engine_state.groups[&g].tempo;
        let Command::TempoSet(set) = lok(&mut cp, "g on 200b") else {
            panic!("not a TempoSet")
        };
        assert_eq!((set.id, set.interval), (own, 200.0));
        // asking for a clock of its own when it reads another is a move
        run(&mut cp, &mut c, "clock slow 60b");
        run(&mut cp, &mut c, "g on slow");
        run(&mut cp, &mut c, "g on 100b");
        assert_ne!(cp.engine_state.groups[&g].tempo, ctx(&cp, "slow"));
        assert_eq!(cp.engine_state.groups[&g].link, TempoLink::Own);
        assert_eq!(tempo_id(&cp, "g.a"), cp.engine_state.groups[&g].tempo);
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn move_seq_only() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "clock slow 60b");
        run(&mut cp, &mut c, "voice kick from \"kick\" on 93b");
        run(&mut cp, &mut c, "kick { seq hats 1 2 3 4 and seq other 1 }");
        let own = tempo_id(&cp, "kick");
        let slow = ctx(&cp, "slow");

        run(&mut cp, &mut c, "kick.hats on slow");
        assert_eq!(proc_of(&cp, "kick", "hats").tempo, slow);
        assert_eq!(proc_of(&cp, "kick", "hats").link, TempoLink::Context);
        assert_eq!(tempo_id(&cp, "kick"), own);
        assert_eq!(proc_of(&cp, "kick", "other").tempo, own);
        tempo_ids_agree(&cp, &c);

        // the same statement with its steps is a move too
        run(&mut cp, &mut c, "kick { seq hats 1 2 3 4 on 120b }");
        assert_ne!(proc_of(&cp, "kick", "hats").tempo, slow);
        assert_eq!(proc_of(&cp, "kick", "hats").link, TempoLink::Own);
        // other changes apply too, after the move
        run(&mut cp, &mut c, "kick { seq hats 1 2 on slow }");
        assert_eq!(proc_of(&cp, "kick", "hats").tempo, slow);
        assert_eq!(proc_of(&cp, "kick", "hats").seq.steps.len(), 2);
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn move_running_seq_doesnt_misfire() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "clock clk 1000f");
        run(&mut cp, &mut c, "clock other 1000f");
        run(&mut cp, &mut c, "voice kick from \"kick\" on clk");
        run(&mut cp, &mut c, "kick { seq s 1 3 }");
        run(&mut cp, &mut c, "start clk and start kick");
        render_i16(&mut c, 1, 300);
        // `other` starts 300 frames after clk: out of phase
        run(&mut cp, &mut c, "start other");
        let out = render_i16(&mut c, 1, 2200);
        // clk crossed step 2 at its frame 2000, which is frame 1700 here
        assert_eq!(triggers(&out), vec![1700]);
        // `other` is at 2.2 steps. The step at 2 is behind it; 4.0 is 1.8
        // steps ahead.
        run(&mut cp, &mut c, "kick.s on other");
        let out = render_i16(&mut c, 1, 2500);
        near(&triggers(&out), &[1800]);
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn fresh_clock_starts_on_running_voice() {
        let (mut cp, mut c) = session();
        run(
            &mut cp,
            &mut c,
            "voice kick from \"kick\" and voice hat from \"hat\"",
        );
        run(&mut cp, &mut c, "kick { seq s 1 3 }");
        run(&mut cp, &mut c, "start kick");
        render_i16(&mut c, 1, 100);
        run(&mut cp, &mut c, "kick on 1000f");
        let own = tempo_id(&cp, "kick");
        assert!(c.tempos.get(own.index()).unwrap().state.active);
        let out = render_i16(&mut c, 1, 2500);
        // the seq reads the voice's clock, which began at the move
        assert_eq!(triggers(&out), vec![0, 2000]);
        assert_eq!(steps_of(&c, own), 2.5);
        // a voice that isn't playing gets a clock that waits for its start
        run(&mut cp, &mut c, "hat on 1000f");
        let idle = tempo_id(&cp, "hat");
        assert!(!c.tempos.get(idle.index()).unwrap().state.active);
        run(&mut cp, &mut c, "start hat");
        assert!(c.tempos.get(idle.index()).unwrap().state.active);
        tempo_ids_agree(&cp, &c);
    }

    // ---- follow ----

    #[test]
    fn follow_voice() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "clock slow 60b");
        run(&mut cp, &mut c, "voice kick from \"kick\" on 93b");
        run(&mut cp, &mut c, "voice hat from \"hat\" on kick");
        let kick = vid_of(&cp, "kick");
        assert_eq!(tempo_id(&cp, "hat"), tempo_id(&cp, "kick"));
        assert_eq!(link_of(&cp, "hat"), TempoLink::Follow(kick));
        // the follower shares the clock without owning it
        assert_eq!(
            cp.engine_state.tempos[&tempo_id(&cp, "hat")].owner,
            TempoOwner::Voice(kick)
        );
        tempo_ids_agree(&cp, &c);

        // retiming the followed voice's own clock: the follower hears it
        let kt = tempo_id(&cp, "kick");
        run(&mut cp, &mut c, "kick on 100b");
        assert_eq!(tempo_id(&cp, "kick"), kt);
        assert_eq!(tempo_id(&cp, "hat"), kt);
        assert_eq!(cp.engine_state.tempos[&kt].interval, 100.0);
        let samples = c.tempos.get(kt.index()).unwrap().state.interval;
        assert!((samples - 28800.0).abs() < 0.01, "{samples}");

        // moving it takes the follower along, and frees the old clock
        run(&mut cp, &mut c, "kick on slow");
        let slow = ctx(&cp, "slow");
        assert_eq!(tempo_id(&cp, "kick"), slow);
        assert_eq!(tempo_id(&cp, "hat"), slow);
        assert_eq!(link_of(&cp, "hat"), TempoLink::Follow(kick));
        assert!(!cp.engine_state.tempos.contains_key(&kt));
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn follow_transitive() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "clock slow 60b");
        run(&mut cp, &mut c, "voice a from \"kick\" on 93b");
        run(&mut cp, &mut c, "voice b from \"hat\" on a");
        run(&mut cp, &mut c, "voice c from \"snare\" on b");
        run(&mut cp, &mut c, "c { seq s 1 }");
        run(&mut cp, &mut c, "start a");
        tempo_ids_agree(&cp, &c);
        run(&mut cp, &mut c, "a on slow");
        let slow = ctx(&cp, "slow");
        for v in ["a", "b", "c"] {
            assert_eq!(tempo_id(&cp, v), slow, "{v}");
        }
        assert_eq!(proc_of(&cp, "c", "s").tempo, slow);
        assert_eq!(cp.engine_state.ids.tempos.live(), 2);
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn follow_cycle_errors() {
        let mut cp = lang_processor();
        lok(&mut cp, "voice a from \"kick\" on 93b");
        lok(&mut cp, "voice b from \"hat\" on a");
        lok(&mut cp, "voice c from \"snare\" on b");
        let before = snapshot(&cp);
        for (line, who) in [
            ("a on a", "a"),
            ("a on c", "a"),
            ("a on b", "a"),
            ("b on c", "b"),
        ] {
            let e = lerr(&mut cp, line);
            assert_eq!(e[0].code, Code::WrongType, "{line}");
            assert_eq!(
                e[0].message,
                format!("'{who}' would follow itself"),
                "{line}"
            );
            assert_eq!(snapshot(&cp), before, "{line}");
        }
        // a group can't follow what follows it
        lok(&mut cp, "voice m from \"bass\"");
        lok(&mut cp, "group g { m }");
        let before = snapshot(&cp);
        for line in ["g on g", "g on m", "g on g.m"] {
            let e = lerr(&mut cp, line);
            assert_eq!(e[0].code, Code::WrongType, "{line}: {}", e[0]);
            assert_eq!(snapshot(&cp), before, "{line}");
        }
    }

    #[test]
    fn follow_then_literal_moves_only_follower() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "voice kick from \"kick\" on 93b");
        run(&mut cp, &mut c, "voice hat from \"hat\" on kick");
        let kt = tempo_id(&cp, "kick");
        run(&mut cp, &mut c, "hat on 100b");
        assert_eq!(tempo_id(&cp, "kick"), kt);
        assert_eq!(cp.engine_state.tempos[&kt].interval, 93.0);
        let ht = tempo_id(&cp, "hat");
        assert_ne!(ht, kt);
        assert_eq!(link_of(&cp, "hat"), TempoLink::Own);
        assert_eq!(
            cp.engine_state.tempos[&ht].owner,
            TempoOwner::Voice(vid_of(&cp, "hat"))
        );
        assert_eq!(cp.engine_state.tempos[&ht].interval, 100.0);
        tempo_ids_agree(&cp, &c);
        // kick may now be removed
        run(&mut cp, &mut c, "remove kick");
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn follow_a_voice_that_adopted_a_group_clock() {
        let (mut cp, mut c) = session();
        run(&mut cp, &mut c, "clock slow 60b");
        run(&mut cp, &mut c, "voice a from \"kick\"");
        run(&mut cp, &mut c, "group g on 240b { a }");
        // a adopted g's clock, so a fresh follower of a reads it
        let gt = cp.engine_state.groups[&gid_of(&cp, "g")].tempo;
        assert_eq!(tempo_id(&cp, "g.a"), gt);
        run(&mut cp, &mut c, "voice d from \"snare\" on a");
        assert_eq!(tempo_id(&cp, "d"), gt);
        // moving the group moves a, and d with it
        run(&mut cp, &mut c, "g on slow");
        assert_eq!(tempo_id(&cp, "g.a"), ctx(&cp, "slow"));
        assert_eq!(tempo_id(&cp, "d"), ctx(&cp, "slow"));
        tempo_ids_agree(&cp, &c);
    }

    #[test]
    fn state_prints_follow() {
        let mut cp = lang_processor();
        lok(&mut cp, "voice kick from \"kick\" on 93b");
        lok(&mut cp, "voice hat from \"hat\" on kick");
        lok(&mut cp, "hat { seq s 1 2 on kick }");
        let text = state(&mut cp);
        assert!(text.contains("voice hat from \"hat\" on kick"), "{text}");
        assert!(text.contains("seq s 1 2 on kick"), "{text}");

        // it reads back to the same session
        let mut again = lang_processor();
        if let Err(e) = again.parse(&text) {
            panic!("{e}\n{text}");
        }
        assert_eq!(state(&mut again), text);
        assert_eq!(
            link_of(&again, "hat"),
            TempoLink::Follow(vid_of(&again, "kick"))
        );
    }

    #[test]
    fn on_seq_is_an_error() {
        let mut cp = lang_processor();
        lok(&mut cp, "voice kick from \"kick\" on 93b");
        lok(&mut cp, "kick { seq hats 1 }");
        lok(&mut cp, "voice hat from \"hat\"");
        let before = snapshot(&cp);
        let e = lerr(&mut cp, "hat on kick.hats");
        assert_eq!(e[0].code, Code::WrongType);
        assert_eq!(
            e[0].message,
            "'on' takes a clock, a group, a voice, or a rate like 93b"
        );
        let e = lerr(&mut cp, "voice snare from \"snare\" on kick.hats");
        assert_eq!(e[0].code, Code::WrongType);
        assert_eq!(snapshot(&cp), before);
    }

    // ---- errors change nothing ----

    #[test]
    fn move_rejects_bad_targets_atomically() {
        let mut cp = lang_processor();
        lok(&mut cp, "clock slow 60b");
        lok(&mut cp, "voice kick from \"kick\" on 93b");
        lok(&mut cp, "voice hat from \"hat\" on kick");
        lok(&mut cp, "voice snare from \"snare\"");
        lok(&mut cp, "kick { seq s 1 }");
        lok(&mut cp, "group drums { snare }");
        let before = snapshot(&cp);
        for (line, code) in [
            ("hat on nope", Code::UnknownName),
            ("nope on slow", Code::UnknownName),
            ("hat on 0b", Code::OutOfRange),
            ("drums on drums", Code::WrongType),
            ("kick on hat", Code::WrongType),
            ("hat on kick.s", Code::WrongType),
            // a good move followed by a bad one is one unit
            ("hat on slow\nhat on nope", Code::UnknownName),
            ("kick on slow\nremove slow", Code::InUse),
            ("remove kick", Code::InUse),
        ] {
            let errs = lerr(&mut cp, line);
            assert_eq!(errs[0].code, code, "{line}: {}", errs[0]);
            assert_eq!(snapshot(&cp), before, "{line}");
        }
    }

    #[test]
    fn move_with_no_free_tempo_changes_nothing() {
        let limits = Limits {
            tempos: 2,
            ..Default::default()
        };
        let mut cp = processor_with(&["kick", "hat"], limits);
        cp.boot();
        lok(&mut cp, "voice kick from \"kick\" on 93b");
        lok(&mut cp, "voice hat from \"hat\"");
        let before = snapshot(&cp);
        let e = lerr(&mut cp, "hat on 100b");
        assert_eq!(e[0].code, Code::LimitReached);
        assert_eq!(snapshot(&cp), before);
        // a move onto an existing clock needs no new ID
        lok(&mut cp, "hat on kick");
        assert_eq!(tempo_id(&cp, "hat"), tempo_id(&cp, "kick"));
    }

    #[test]
    fn restating_the_clock_it_reads_is_a_no_op() {
        let mut cp = lang_processor();
        lok(&mut cp, "clock slow 60b");
        lok(&mut cp, "voice kick from \"kick\" on slow");
        lok(&mut cp, "voice hat from \"hat\" on 93b");
        for line in ["kick on slow", "hat on 93b"] {
            assert!(local(&mut cp, line).contains("W101"), "{line}");
        }
        // a voice that follows main, told so, stays as it is
        lok(&mut cp, "voice snare from \"snare\"");
        assert!(local(&mut cp, "snare on main").contains("W101"));
        assert_eq!(link_of(&cp, "snare"), TempoLink::Parent);
    }
}

// ---- PRC: editing seqs, seeds, group seqs ----

mod prc {
    use super::*;

    fn text_round_trips(cp: &CmdProcessor) -> String {
        let text = cp.state(None).unwrap();
        let mut fresh = lang_processor();
        lok(&mut fresh, &text);
        assert_eq!(fresh.state(None).unwrap(), text);
        text
    }

    #[test]
    fn seed_state_round_trip() {
        let mut cp = lang_processor();
        lok(&mut cp, "voice kick from \"kick\"");
        lok(&mut cp, "kick { seq s 1 2 chance=50 seed=7 }");
        lok(&mut cp, "kick { seq t 3 }");
        let text = text_round_trips(&cp);
        // `seed=` prints only when one was given
        assert!(text.contains("seq s 1 2 chance=50 seed=7"), "{text}");
        assert!(!text.contains("seq t 3 seed"), "{text}");
        assert_eq!(proc_of(&cp, "kick", "s").seq.seed, Some(7));
        assert_eq!(proc_of(&cp, "kick", "t").seq.seed, None);
        // short form, and a new seed is an edit
        let cmd = lok(&mut cp, "kick.s s=9");
        assert!(matches!(cmd, Command::SeqEdit(ref a) if a.rng.is_some()));
        assert_eq!(proc_of(&cp, "kick", "s").seq.seed, Some(9));
    }

    #[test]
    fn explicit_period_is_exact() {
        let mut cp = lang_processor();
        lok(&mut cp, "voice kick from \"kick\" in 8");
        lok(
            &mut cp,
            "kick { seq a 1 and seq b 1 in 8 and seq c 1 in 2 }",
        );
        assert!(!proc_of(&cp, "kick", "a").seq.explicit_period);
        assert!(proc_of(&cp, "kick", "b").seq.explicit_period);
        // the voice's period moves the seqs that follow it, even a seq whose
        // explicit period happens to equal the voice's
        let cmds = batch_or_one(lok(&mut cp, "kick in 6"));
        assert_eq!(names(&cmds), ["SeqEdit"]);
        assert_eq!(proc_of(&cp, "kick", "a").seq.period, 6);
        assert_eq!(proc_of(&cp, "kick", "b").seq.period, 8);
        assert_eq!(proc_of(&cp, "kick", "c").seq.period, 2);
        // restating `in` makes a seq explicit
        lok(&mut cp, "kick.a in 6");
        assert!(proc_of(&cp, "kick", "a").seq.explicit_period);
        local(&mut cp, "kick in 5");
        assert_eq!(proc_of(&cp, "kick", "a").seq.period, 6);
    }

    #[test]
    fn edit_commands() {
        let mut cp = lang_processor();
        lok(&mut cp, "voice kick from \"kick\" on 1000ms");
        lok(&mut cp, "kick { seq s 1 2 3 }");
        let Command::SeqEdit(a) = lok(&mut cp, "kick.s 2 4%50") else {
            panic!("not a SeqEdit");
        };
        assert_eq!(a.steps, vec![1.0, 3.0]);
        assert_eq!(a.chance, vec![100.0, 50.0]);
        assert_eq!(a.mask, vec![1 | 2, 2]);
        assert_eq!(a.period, 0);
        // only the setting: the steps stay
        let Command::SeqEdit(a) = lok(&mut cp, "kick.s chance=30 over 2") else {
            panic!("not a SeqEdit");
        };
        assert!(a.steps.is_empty());
        let edit = a.feel[0].unwrap();
        assert_eq!(edit.value, 30.0);
        // 2 periods of 4 ticks at 1000ms
        assert_eq!(edit.frames, 2 * 4 * 48_000);
        // the followers hold the target, the step with its own % doesn't
        let steps = proc_of(&cp, "kick", "s").seq.steps;
        assert_eq!((steps[0].chance, steps[1].chance), (30.0, 50.0));
        // the feel survives a new list; steps without marks follow it
        lok(&mut cp, "kick.s 1 3");
        let steps = proc_of(&cp, "kick", "s").seq.steps;
        assert_eq!((steps[0].chance, steps[1].chance), (30.0, 30.0));
        // a step outside a shorter cycle is refused
        assert_eq!(lerr(&mut cp, "kick.s in 2")[0].code, Code::StepOutsideCycle);
        assert_eq!(proc_of(&cp, "kick", "s").seq.period, 4);
    }

    #[test]
    fn group_seq_state_round_trip() {
        let mut cp = lang_processor();
        lok(&mut cp, "voice kick from \"kick\"");
        lok(&mut cp, "voice hat from \"hat\"");
        lok(
            &mut cp,
            "group drums on 120b { kick; hat; seq hits 1 3 seed=4 }",
        );
        let g = cp.engine_state.group_id("drums").unwrap();
        let repr = &cp.engine_state.groups[&g];
        assert_eq!(repr.procs.len(), 1);
        let hits = &cp.engine_state.procs[&repr.procs["hits"]];
        assert_eq!(hits.owner, ProcOwner::Group(g));
        // it reads the group's clock
        assert_eq!(hits.tempo, repr.tempo);
        assert_eq!(hits.link, TempoLink::Group(g));
        let text = text_round_trips(&cp);
        assert!(text.contains("  seq hits 1 3 seed=4"), "{text}");
        assert_eq!(
            cp.state(Some("drums.hits")).unwrap(),
            "drums { seq hits 1 3 seed=4 }"
        );

        // the other ways to write it, and the path
        lok(&mut cp, "drums { seq more 2 }");
        assert!(local(&mut cp, "drums.more 2").contains("W101"));
        lok(&mut cp, "drums.more 2 4");
        assert_eq!(cp.engine_state.procs.len(), 2);
        // a group moves its seqs with it
        lok(&mut cp, "drums on 90b");
        let repr = &cp.engine_state.groups[&g];
        assert_eq!(cp.engine_state.procs[&repr.procs["hits"]].tempo, repr.tempo);
        // removing a seq or the group leaves nothing behind
        lok(&mut cp, "remove drums.more");
        assert_eq!(cp.engine_state.procs.len(), 1);
        lok(&mut cp, "remove drums");
        assert!(cp.engine_state.procs.is_empty());
        assert_eq!(cp.engine_state.ids.procs.live(), 0);
    }

    #[test]
    fn group_seq_names() {
        let mut cp = lang_processor();
        lok(&mut cp, "voice kick from \"kick\"");
        lok(&mut cp, "group drums { kick }");
        // a seq can't take a member's name
        lerr(&mut cp, "drums { seq kick 1 }");
        assert!(cp.engine_state.procs.is_empty());
        // the group's period is the default for its seqs
        local(&mut cp, "drums in 8");
        lok(&mut cp, "drums { seq a 1 }");
        assert_eq!(cp.engine_state.procs.values().last().unwrap().seq.period, 8);
        lok(&mut cp, "drums in 6");
        assert_eq!(cp.engine_state.procs.values().last().unwrap().seq.period, 6);
    }

    #[test]
    fn paused_seqs_dont_print() {
        let mut cp = lang_processor();
        lok(&mut cp, "voice kick from \"kick\"");
        lok(&mut cp, "kick { seq s 1 }");
        let before = cp.state(None).unwrap();
        lok(&mut cp, "pause kick.s");
        assert_eq!(cp.state(None).unwrap(), before);
        lok(&mut cp, "resume kick.s");
        assert!(proc_of(&cp, "kick", "s").enabled);
    }
}

// ---- receivers, packets and session seqs (RCV) ----

mod rcv {
    use super::*;

    fn session() -> CmdProcessor {
        let mut cp = lang_processor();
        for v in ["kick", "bass", "hat", "snare"] {
            lok(&mut cp, &format!("voice {v} from \"{v}\""));
        }
        cp
    }

    fn recv_of(cp: &CmdProcessor, voice: &str, seq: &str) -> Vec<RecvSpec> {
        proc_of(cp, voice, seq).seq.recv
    }

    fn id(cp: &CmdProcessor, voice: &str) -> VoiceId {
        cp.engine_state.voice_id(voice).unwrap()
    }

    fn code(cp: &mut CmdProcessor, line: &str) -> Code {
        lerr(cp, line)[0].code
    }

    fn round_trips(cp: &CmdProcessor) -> String {
        let text = cp.state(None).unwrap();
        let mut fresh = lang_processor();
        lok(&mut fresh, &text);
        assert_eq!(fresh.state(None).unwrap(), text, "{text}");
        text
    }

    #[test]
    fn pipes_lower_to_receivers() {
        let mut cp = session();
        let cmd = lok(
            &mut cp,
            "kick { seq beat 1 2 3 4 in 4 | trig, melo bass 36 36 43 41 }",
        );
        let want = vec![
            RecvSpec::Trig(TrigTarget::Owner),
            RecvSpec::melo(id(&cp, "bass"), vec![36.0, 36.0, 43.0, 41.0]),
        ];
        assert_eq!(seq_args(cmd).recv, want);
        assert_eq!(recv_of(&cp, "kick", "beat"), want);
        // no pipe triggers the owner (G6); a target names another voice
        lok(&mut cp, "hat { seq off 2 }");
        assert_eq!(recv_of(&cp, "hat", "off"), RecvSpec::owner_trig());
        lok(&mut cp, "hat { seq up 3 | trig snare }");
        assert_eq!(
            recv_of(&cp, "hat", "up"),
            vec![RecvSpec::Trig(TrigTarget::Voice(id(&cp, "snare")))]
        );
        // a melo without a target plays the owner
        lok(&mut cp, "bass { seq line 1 | melo 48 }");
        assert_eq!(
            recv_of(&cp, "bass", "line"),
            vec![RecvSpec::melo(id(&cp, "bass"), vec![48.0])]
        );
        // a group is a trig target
        lok(&mut cp, "group drums { hat; snare }");
        lok(&mut cp, "kick { seq g 1 | trig drums }");
        let g = cp.engine_state.group_id("drums").unwrap();
        assert_eq!(
            recv_of(&cp, "kick", "g"),
            vec![RecvSpec::Trig(TrigTarget::Group(g))]
        );
    }

    #[test]
    fn restate_pipe_replaces_receivers() {
        let mut cp = session();
        lok(&mut cp, "kick { seq beat 1 2 | melo bass 36 }");
        let cmd = lok(&mut cp, "kick.beat | trig hat");
        let Command::SeqRecv(args) = cmd else {
            panic!("expected a SeqRecv");
        };
        assert_eq!(
            args.recv,
            vec![RecvSpec::Trig(TrigTarget::Voice(id(&cp, "hat")))]
        );
        assert_eq!(recv_of(&cp, "kick", "beat"), args.recv);
        // the steps were not touched
        assert_eq!(proc_of(&cp, "kick", "beat").seq.steps.len(), 2);
        // the same pipe again changes nothing
        assert!(local(&mut cp, "kick.beat | trig hat").contains("W101"));
        // a statement without a pipe leaves them alone
        lok(&mut cp, "kick.beat 1 3");
        assert_eq!(recv_of(&cp, "kick", "beat"), args.recv);
    }

    #[test]
    fn bad_receiver_target_errors() {
        let mut cp = session();
        lok(&mut cp, "kick { seq beat 1 }");
        lok(&mut cp, "group drums { hat }");
        // a clock, a seq and a group (for melo) aren't targets
        for line in [
            "kick.beat | trig main",
            "kick.beat | trig kick.beat",
            "kick.beat | melo drums 60",
            "kick.beat | melo main 60",
        ] {
            assert_eq!(code(&mut cp, line), Code::BadReceiver, "{line}");
        }
        assert_eq!(code(&mut cp, "kick.beat | trig nope"), Code::UnknownName);
        // melo without a target needs a voice owner
        assert_eq!(
            code(&mut cp, "drums { seq h 1 | melo 60 }"),
            Code::BadReceiver
        );
        // notes belong to melo
        assert_eq!(code(&mut cp, "kick.beat | trig bass 60"), Code::WrongType);
        // none of it changed the pipe
        assert_eq!(recv_of(&cp, "kick", "beat"), RecvSpec::owner_trig());
        assert!(cp.engine_state.group_id("drums").is_ok());
        assert_eq!(cp.engine_state.procs.len(), 1);
    }

    #[test]
    fn melo_note_range() {
        let mut cp = session();
        lok(&mut cp, "kick { seq beat 1 }");
        assert_eq!(code(&mut cp, "kick.beat | melo bass 128"), Code::OutOfRange);
        assert_eq!(
            code(&mut cp, "kick.beat | melo bass 60 -1"),
            Code::OutOfRange
        );
        assert_eq!(code(&mut cp, "kick.beat | melo bass"), Code::MissingItem);
        assert_eq!(
            code(&mut cp, "kick.beat | melo bass 60..64"),
            Code::WrongType
        );
        assert_eq!(
            code(&mut cp, "kick.beat | melo bass 60%50"),
            Code::WrongType
        );
        // the ends are fine
        lok(&mut cp, "kick.beat | melo bass 0 127 60.5");
        assert_eq!(
            recv_of(&cp, "kick", "beat"),
            vec![RecvSpec::melo(id(&cp, "bass"), vec![0.0, 127.0, 60.5])]
        );
    }

    #[test]
    fn session_seq_with_pipe() {
        let mut cp = session();
        let args = seq_args(lok(&mut cp, "seq x 1 3 | trig kick"));
        assert_eq!(args.owner, ProcOwner::Session);
        // it follows main
        let main = cp.engine_state.main_id().unwrap();
        assert_eq!(args.tempo, TempoUse::Shared(main));
        assert_eq!(
            args.recv,
            vec![RecvSpec::Trig(TrigTarget::Voice(id(&cp, "kick")))]
        );
        assert_eq!(cp.engine_state.session_procs.len(), 1);
        // reachable by name, and its owner is the session
        assert!(local(&mut cp, "x | trig kick").contains("W101"));
        lok(&mut cp, "pause x");
        assert!(!cp.engine_state.procs.values().next().unwrap().enabled);
        // on another clock
        lok(&mut cp, "clock slow 60b");
        lok(&mut cp, "seq y 2 on slow | melo bass 40");
        // a name is taken once
        assert_eq!(code(&mut cp, "seq kick 1 | trig hat"), Code::KindClash);
        assert_eq!(code(&mut cp, "voice x from \"kick\""), Code::KindClash);
        lok(&mut cp, "remove x y");
        assert!(cp.engine_state.procs.is_empty());
        assert!(cp.engine_state.session_procs.is_empty());
    }

    #[test]
    fn session_seq_without_pipe_e502() {
        let mut cp = session();
        for line in [
            "seq x 1 3",
            "seq x 1 3 | trig",
            "seq x 1 3 | melo 60",
            "packet p { seq a 1 }",
            "packet p { seq a 1 } | trig",
        ] {
            assert_eq!(code(&mut cp, line), Code::NothingToTrigger, "{line}");
        }
        assert!(cp.engine_state.procs.is_empty());
        assert!(cp.engine_state.packets.is_empty());
    }

    #[test]
    fn packets_lower_to_seqs_with_its_pipe() {
        let mut cp = session();
        let cmd = lok(
            &mut cp,
            "packet fills {\n  seq a 4 4.5 in 4\n  seq b 2.75 4.25 in 8\n} | trig snare",
        );
        let snare = vec![RecvSpec::Trig(TrigTarget::Voice(id(&cp, "snare")))];
        let seqs: Vec<SeqArgs> = match cmd {
            Command::Batch(b) => b.cmds.into_iter().map(seq_args).collect(),
            other => vec![seq_args(other)],
        };
        assert_eq!(seqs.len(), 2);
        for s in &seqs {
            assert_eq!(s.owner, ProcOwner::Session);
            assert_eq!(s.recv, snare);
            assert_eq!(s.pend.len(), s.steps.len());
        }
        assert_eq!((seqs[0].period, seqs[1].period), (4, 8));
        let es = &cp.engine_state;
        assert_eq!(es.packets.len(), 1);
        let pk = es.packets.values().next().unwrap();
        assert_eq!((pk.name.as_str(), pk.seqs.len()), ("fills", 2));
        // a seq in a packet can't pipe on its own
        assert_eq!(
            code(&mut cp, "packet more { seq c 1 | trig hat } | trig snare"),
            Code::Unexpected
        );
        assert_eq!(
            code(&mut cp, "fills { seq a 1 | trig hat }"),
            Code::Unexpected
        );
        // a packet needs seqs
        assert_eq!(
            code(&mut cp, "packet empty | trig snare"),
            Code::MissingItem
        );
        // its name is one name
        assert_eq!(code(&mut cp, "voice fills from \"kick\""), Code::KindClash);
        assert_eq!(code(&mut cp, "seq fills 1 | trig hat"), Code::KindClash);
    }

    #[test]
    fn packet_defaults_in_owner_and_restating() {
        let mut cp = session();
        lok(
            &mut cp,
            "kick { packet runs in 8 { seq a 1 5; seq b 2 in 4 } | trig snare }",
        );
        let a = proc_of(&cp, "kick", "a");
        let b = proc_of(&cp, "kick", "b");
        // `in` is the default for its seqs
        assert_eq!((a.seq.period, b.seq.period), (8, 4));
        assert!(a.packet.is_some() && a.packet == b.packet);
        assert_eq!(a.seq.recv, b.seq.recv);
        // reachable as `kick.runs`; a restated pipe reaches every seq
        let cmd = lok(&mut cp, "kick.runs | trig hat, trig");
        let Command::Batch(batch) = cmd else {
            panic!("expected one SeqRecv per seq");
        };
        assert_eq!(names(&batch.cmds), ["SeqRecv", "SeqRecv"]);
        let want = vec![
            RecvSpec::Trig(TrigTarget::Voice(id(&cp, "hat"))),
            RecvSpec::Trig(TrigTarget::Owner),
        ];
        assert_eq!(recv_of(&cp, "kick", "a"), want);
        assert_eq!(recv_of(&cp, "kick", "b"), want);
        // `in` alone moves the seqs; the same again says it changes nothing
        lok(&mut cp, "kick.runs in 16");
        assert_eq!(proc_of(&cp, "kick", "a").seq.period, 16);
        assert!(local(&mut cp, "kick.runs in 16").contains("W101"));
        // a new seq in the block joins; one from outside it can't
        lok(&mut cp, "kick.runs { seq c 3 }");
        assert_eq!(
            cp.engine_state.packets.values().next().unwrap().seqs.len(),
            3
        );
        assert_eq!(recv_of(&cp, "kick", "c"), want);
        lok(&mut cp, "kick { seq loose 1 }");
        assert_eq!(code(&mut cp, "kick.runs { seq loose 2 }"), Code::WrongType);
    }

    #[test]
    fn packet_actions_and_remove() {
        let mut cp = session();
        lok(&mut cp, "packet fills { seq a 1; seq b 2 } | trig snare");
        for (line, ops) in [
            ("pause fills", vec![ProcOp::Disable; 2]),
            ("resume fills", vec![ProcOp::Enable; 2]),
            (
                "start fills",
                vec![ProcOp::Enable, ProcOp::Reset, ProcOp::Enable, ProcOp::Reset],
            ),
        ] {
            let cmd = lok(&mut cp, line);
            let cmds = match cmd {
                Command::Batch(b) => b.cmds,
                other => vec![other],
            };
            let got: Vec<ProcOp> = cmds
                .into_iter()
                .filter_map(|c| match c {
                    Command::ProcEdit(a) => Some(a.op),
                    _ => None,
                })
                .collect();
            assert_eq!(got, ops, "{line}");
        }
        // one seq out of it leaves the packet
        lok(&mut cp, "remove a");
        assert_eq!(
            cp.engine_state.packets.values().next().unwrap().seqs.len(),
            1
        );
        lok(&mut cp, "remove fills");
        assert!(cp.engine_state.packets.is_empty());
        assert!(cp.engine_state.procs.is_empty());
        assert_eq!(cp.engine_state.ids.procs.live(), 0);
        // a voice's packet goes with the voice
        lok(&mut cp, "kick { packet p { seq a 1 } }");
        lok(&mut cp, "remove kick");
        assert!(cp.engine_state.packets.is_empty());
    }

    #[test]
    fn removing_a_target_strips_the_receiver() {
        let mut cp = session();
        lok(&mut cp, "kick { seq beat 1 | trig, melo bass 36 }");
        lok(&mut cp, "seq x 1 | trig bass");
        let cmd = lok(&mut cp, "remove bass");
        let cmds = match cmd {
            Command::Batch(b) => b.cmds,
            other => vec![other],
        };
        // `x` triggered only bass, so it goes with it
        assert_eq!(names(&cmds), ["SeqRecv", "Batch", "Unload"]);
        assert_eq!(recv_of(&cp, "kick", "beat"), RecvSpec::owner_trig());
        assert!(!cp.engine_state.session_procs.contains_key("x"));
    }

    #[test]
    fn remove_target_warns_w102() {
        let mut cp = session();
        lok(&mut cp, "kick { seq beat 1 | trig, melo bass 36 }");
        lok(&mut cp, "seq x 1 | trig bass");
        lok(&mut cp, "remove bass");
        let out = std::mem::take(&mut cp.outbox);
        assert!(
            out.contains("W102 kick.beat no longer triggers bass"),
            "{out}"
        );
        assert!(out.contains("W102 x triggered only bass; removed"), "{out}");
        assert_eq!(Code::ReceiverDropped.as_str(), "W102");
        assert!(Code::ReceiverDropped.is_warning());
    }

    #[test]
    fn emptied_pipe_removes_seq_and_packet() {
        let mut cp = session();
        lok(&mut cp, "seq x 1 | trig snare");
        lok(
            &mut cp,
            "kick { packet p { seq a 1; seq b 2 } | trig snare }",
        );
        lok(&mut cp, "packet q { seq c 1 } | trig snare, trig bass");
        let cmd = lok(&mut cp, "remove snare");
        let out = std::mem::take(&mut cp.outbox);
        assert!(
            out.contains("W102 x triggered only snare; removed"),
            "{out}"
        );
        assert!(
            out.contains("W102 kick.p triggered only snare; removed"),
            "{out}"
        );
        assert!(out.contains("W102 q no longer triggers snare"), "{out}");
        let es = &cp.engine_state;
        // x, and p with both its seqs, are gone; q and its seq stay
        assert!(!es.session_procs.contains_key("x") && es.session_procs.contains_key("c"));
        assert_eq!(es.packets.len(), 1);
        assert_eq!(es.procs.len(), 1);
        assert_eq!(es.ids.procs.live(), 1);
        // the engine is told: three removals and one pipe
        let mut removes = 0;
        let mut recvs = 0;
        fn walk(c: &Command, removes: &mut u32, recvs: &mut u32) {
            match c {
                Command::Batch(b) => b.cmds.iter().for_each(|c| walk(c, removes, recvs)),
                Command::ProcEdit(a) if a.op == ProcOp::Remove => *removes += 1,
                Command::SeqRecv(_) => *recvs += 1,
                _ => {}
            }
        }
        walk(&cmd, &mut removes, &mut recvs);
        assert_eq!((removes, recvs), (3, 1));
    }

    #[test]
    fn state_round_trips_after_target_removed() {
        let mut cp = session();
        lok(&mut cp, "seq x 1 | trig snare");
        lok(&mut cp, "kick { seq beat 1 | trig, melo bass 36 }");
        lok(&mut cp, "packet q { seq c 1 } | trig snare, trig bass");
        lok(&mut cp, "remove snare");
        lok(&mut cp, "remove bass");
        // nothing prints as an empty pipe, and the text builds the same session
        let text = round_trips(&cp);
        assert!(
            !text.contains("| trig\n") && !text.trim_end().ends_with("| trig"),
            "{text}"
        );
        assert!(!text.contains("seq x"), "{text}");
    }

    #[test]
    fn state_round_trips_pipes_and_packets() {
        let mut cp = session();
        lok(
            &mut cp,
            "kick { seq beat 1 2 3 4 in 4 | trig, melo bass 36 36 43 41 }",
        );
        lok(&mut cp, "hat { seq off 1.5 2.5 | trig snare }");
        lok(&mut cp, "hat { seq plain 3 }");
        lok(
            &mut cp,
            "packet fills {\n  seq a 4 4.5 in 4\n  seq b 2.75 4.25 in 8\n} | trig snare",
        );
        lok(&mut cp, "kick { packet runs { seq r 2 } | trig hat }");
        lok(&mut cp, "kick { packet owned { seq o 3 } }");
        lok(&mut cp, "clock slow 90b");
        lok(&mut cp, "seq x 1 3 on slow | trig kick, melo bass 60");
        lok(
            &mut cp,
            "group drums { snare; seq hits 1 3 | trig kick; seq mine 2 }",
        );
        lok(&mut cp, "drums { packet gp { seq g 1 } | trig bass }");
        let text = round_trips(&cp);
        assert!(
            text.contains("seq beat 1 2 3 4 | trig, melo bass 36 36 43 41"),
            "{text}"
        );
        assert!(text.contains("} | trig snare"), "{text}");
        assert!(
            text.contains("seq x 1 3 on slow | trig kick, melo bass 60"),
            "{text}"
        );
        assert!(text.contains("seq drums.hits 1 3 | trig kick"), "{text}");
        assert!(text.contains("packet drums.gp {"), "{text}");
        // the default pipe isn't printed
        assert!(
            text.contains("seq plain 3\n") || text.contains("seq plain 3 "),
            "{text}"
        );
        // single objects print on their own
        assert!(
            cp.state(Some("fills"))
                .unwrap()
                .starts_with("packet fills {")
        );
        assert!(cp.state(Some("x")).unwrap().starts_with("seq x 1 3"));
        assert!(
            cp.state(Some("kick.runs"))
                .unwrap()
                .starts_with("packet kick.runs {")
        );
    }

    #[test]
    fn remove_all_takes_session_seqs() {
        let mut cp = session();
        lok(&mut cp, "seq x 1 | trig kick");
        lok(&mut cp, "packet p { seq a 1 } | trig kick");
        lok(&mut cp, "remove all");
        assert!(cp.engine_state.procs.is_empty());
        assert!(cp.engine_state.packets.is_empty());
        assert!(cp.engine_state.session_procs.is_empty());
        assert_eq!(cp.engine_state.ids.procs.live(), 0);
    }
}

// ---- euclidean seqs (EUC) ----

fn at_of(cp: &CmdProcessor, voice: &str, name: &str) -> Vec<f32> {
    proc_of(cp, voice, name)
        .seq
        .steps
        .iter()
        .map(|s| s.at)
        .collect()
}

#[test]
fn euclid_seq_lowers_to_steps() {
    let mut cp = seq_session();
    let args = seq_args(lok(&mut cp, "hat { seq tres steps=3 in 8 }"));
    assert_eq!(args.steps, vec![0.0, 3.0, 6.0]);
    assert_eq!(args.period, 8);
    let repr = proc_of(&cp, "hat", "tres");
    assert_eq!(repr.seq.r#gen.map(|g| g.hits), Some(3));
    // short forms and the other examples
    let args = seq_args(lok(&mut cp, "hat { seq four n=4 in 4 }"));
    assert_eq!(args.steps, vec![0.0, 1.0, 2.0, 3.0]);
    let args = seq_args(lok(&mut cp, "hat { seq turned steps=3 in 8 r=1 }"));
    assert_eq!(args.steps, vec![1.0, 4.0, 7.0]);
    let args = seq_args(lok(&mut cp, "hat { seq clave steps=5 grid=16 in 4 }"));
    assert_eq!(args.steps, vec![0.0, 0.75, 1.5, 2.25, 3.0]);
}

#[test]
fn euclid_default_grid_is_period() {
    let mut cp = seq_session();
    // no `in`: kick's cycle of 4
    let args = seq_args(lok(&mut cp, "kick { seq a steps=2 }"));
    assert_eq!(args.period, 4);
    assert_eq!(args.steps, vec![0.0, 2.0]);
}

#[test]
fn euclid_grid_not_dividing_period() {
    let mut cp = seq_session();
    // E(3,5) fills slots 0, 2 and 4 of five slots in four ticks
    let args = seq_args(lok(&mut cp, "kick { seq a steps=3 grid=5 in 4 }"));
    assert_eq!(args.steps, vec![0.0, 8.0_f32 / 5.0, 16.0_f32 / 5.0]);
}

#[test]
fn euclid_restate_rot_regenerates() {
    let mut cp = seq_session();
    lok(&mut cp, "hat { seq tres steps=3 in 8 }");
    assert_eq!(
        names(&batch_or_one(lok(&mut cp, "hat.tres rot=1"))),
        ["SeqEdit"]
    );
    assert_eq!(at_of(&cp, "hat", "tres"), [1.0, 4.0, 7.0]);
    // `in` regenerates from the stored generator, keeping rot
    lok(&mut cp, "hat.tres in 16");
    assert_eq!(at_of(&cp, "hat", "tres"), [1.0, 6.0, 11.0]);
    lok(&mut cp, "hat.tres steps=5");
    let g = proc_of(&cp, "hat", "tres").seq.r#gen.unwrap();
    assert_eq!((g.hits, g.rot), (5, 1));
    assert_eq!(at_of(&cp, "hat", "tres").len(), 5);
}

#[test]
fn euclid_written_steps_replace() {
    let mut cp = seq_session();
    lok(&mut cp, "hat { seq tres steps=3 in 8 }");
    lok(&mut cp, "hat.tres 1 3");
    assert_eq!(proc_of(&cp, "hat", "tres").seq.r#gen, None);
    assert_eq!(at_of(&cp, "hat", "tres"), [0.0, 2.0]);
}

#[test]
fn euclid_hits_replace_written() {
    let mut cp = seq_session();
    lok(&mut cp, "hat { seq a 1 2 3 4 in 8 }");
    assert_eq!(
        names(&batch_or_one(lok(&mut cp, "hat.a steps=3"))),
        ["SeqEdit"]
    );
    assert!(proc_of(&cp, "hat", "a").seq.r#gen.is_some());
    assert_eq!(at_of(&cp, "hat", "a"), [0.0, 3.0, 6.0]);
}

#[test]
fn euclid_same_restatement_is_noop() {
    let mut cp = seq_session();
    lok(&mut cp, "hat { seq tres steps=3 rot=1 in 8 }");
    for line in [
        "hat { seq tres steps=3 rot=1 in 8 }",
        "hat.tres steps=3",
        "hat.tres rot=1",
        "hat.tres in 8",
        "hat.tres",
    ] {
        let text = local(&mut cp, line);
        assert!(text.contains("W101"), "{line}: {text}");
    }
}

#[test]
fn euclid_errors() {
    let mut cp = seq_session();
    lok(&mut cp, "hat { seq w 1 2 in 8 }");
    lok(&mut cp, "hat { seq g steps=3 in 8 }");
    let procs = cp.engine_state.procs.len();
    let before = (proc_of(&cp, "hat", "w").seq, proc_of(&cp, "hat", "g").seq);
    for (line, code) in [
        ("hat { seq a steps=9 in 8 }", Code::OutOfRange),
        ("hat.g steps=9", Code::OutOfRange),
        ("hat { seq a 1 2 steps=2 }", Code::WrongType),
        ("hat.w steps=2 1 2", Code::WrongType),
        ("hat { seq a steps=0 in 8 }", Code::OutOfRange),
        ("hat { seq a steps=3 over 2 in 8 }", Code::WrongType),
        ("hat { seq a steps=3 rot=1 over 2 in 8 }", Code::WrongType),
    ] {
        assert_eq!(lerr(&mut cp, line)[0].code, code, "{line}");
    }
    let e = lerr(&mut cp, "hat { seq a steps=9 in 8 }");
    assert!(
        e[0].message.contains("steps=9 needs grid=9 or more"),
        "{}",
        e[0]
    );
    assert_eq!(cp.engine_state.procs.len(), procs);
    assert_eq!(
        before,
        (proc_of(&cp, "hat", "w").seq, proc_of(&cp, "hat", "g").seq)
    );
}

#[test]
fn euclid_follows_owner_period() {
    let mut cp = seq_session();
    lok(&mut cp, "voice pad from \"snare\" in 8");
    lok(&mut cp, "pad { seq a steps=3 }");
    assert_eq!(at_of(&cp, "pad", "a"), [0.0, 3.0, 6.0]);
    lok(&mut cp, "pad in 16");
    assert_eq!(proc_of(&cp, "pad", "a").seq.period, 16);
    assert_eq!(at_of(&cp, "pad", "a"), [0.0, 5.0, 10.0]);
}

#[test]
fn euclid_with_chance_and_pipe() {
    let mut cp = seq_session();
    lok(&mut cp, "kick { seq a steps=2 in 4 chance=50 }");
    let repr = proc_of(&cp, "kick", "a");
    assert!(repr.seq.steps.iter().all(|s| s.chance == 50.0));
    let args = seq_args(lok(&mut cp, "hat { seq b steps=2 in 4 | trig kick }"));
    assert_eq!(args.steps, vec![0.0, 2.0]);
    assert!(matches!(args.recv[0], RecvSpec::Trig(TrigTarget::Voice(_))));
}

// FXa: voice effects
mod fx {
    use super::*;
    use crate::audio_processing::engine::fx::{defaults, param_of};
    use crate::audio_processing::engine::{FxKind, FxType};

    const LOWPASS: FxKind = FxKind::new(FxType::Filter);
    const HIGHPASS: FxKind = FxKind {
        ty: FxType::Filter,
        variant: 1,
    };
    const DRIVE_FX: FxKind = FxKind::new(FxType::Drive);
    const CRUSH_FX: FxKind = FxKind::new(FxType::Crush);

    // the parameter index of a setting of a kind
    fn ix(ty: FxType, name: &str) -> usize {
        param_of(ty, name).unwrap()
    }
    const FILTER: FxType = FxType::Filter;
    const DRIVE: FxType = FxType::Drive;
    const CRUSH: FxType = FxType::Crush;

    fn session() -> CmdProcessor {
        let mut cp = lang_processor();
        lok(&mut cp, "voice kick from \"kick\"");
        lok(&mut cp, "voice hat from \"hat\"");
        cp
    }

    fn fx_args(cmd: Command) -> FxArgs {
        match cmd {
            Command::Fx(a) => a,
            other => panic!("expected an Fx, got a {}", name_of(&other)),
        }
    }

    // the Fx commands of a line
    fn fx_cmds(cmd: Command) -> Vec<FxArgs> {
        batch_or_one(cmd).into_iter().map(fx_args).collect()
    }

    fn chain<'a>(cp: &'a CmdProcessor, voice: &str) -> &'a [FxRepr] {
        &cp.engine_state.voices[&vid(cp, voice)].fx
    }

    fn effect<'a>(cp: &'a CmdProcessor, voice: &str, name: &str) -> &'a FxRepr {
        chain(cp, voice)
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no effect {name}"))
    }

    fn names(cp: &CmdProcessor, voice: &str) -> Vec<String> {
        chain(cp, voice).iter().map(|f| f.name.clone()).collect()
    }

    fn insert(a: &FxArgs) -> (FxKind, [f32; 8], Option<usize>) {
        match &a.op {
            FxOp::Insert { kind, params, room } => {
                (*kind, *params, room.as_ref().map(|r| r.capacity()))
            }
            _ => panic!("not an Insert"),
        }
    }

    // the text `state` printed on dev (before FXR) for these lines
    #[test]
    fn state_text_of_all_three_effects_is_unchanged() {
        let mut cp = session();
        for l in [
            "kick { filter lo type=hp cutoff=800 q=0.9 base=-12db mix=0.5 gain=3db }",
            "kick { filter plain }",
            "kick { filter bp2 type=bp cutoff=250.5 q=4 }",
            "kick { filter nt type=notch gain=0.5 }",
            "kick { drive dirt amount=24 mix=0.5 gain=-6db }",
            "kick { crush bit bits=6.5 down=4 mix=0.7 gain=1.5 }",
            "group g { kick; hat }",
            "g { filter dark cutoff=1200 base=0.25 }",
            "g { drive warm amount=3 }",
            "g { crush grit down=2 }",
            "hat { filter hh type=hp cutoff=9000 }",
            "stop kick.lo",
            "pause hat.hh",
        ] {
            lok(&mut cp, l);
        }
        let want = "group g {
  voice kick from \"kick\"
  voice hat from \"hat\"
  filter dark cutoff=1200 base=0.25
  drive warm amount=3
  crush grit down=2
}
kick {
  filter lo type=hp cutoff=800 q=0.9 base=-12db mix=0.5 gain=3db
  filter plain
  filter bp2 type=bp cutoff=250.5 q=4
  filter nt type=notch gain=0.5
  drive dirt amount=24 mix=0.5 gain=-6db
  crush bit bits=6.5 down=4 mix=0.7 gain=1.5
}
stop g.kick.lo
hat { filter hh type=hp cutoff=9000 }
pause g.hat.hh";
        assert_eq!(local(&mut cp, "state"), want);
    }

    // a test-only effect (`kinds/thru.rs`, one line in the registry) works
    // everywhere an effect does, with no other edit
    #[test]
    fn thru_is_an_effect_with_one_file_and_one_line() {
        use crate::audio_processing::commands::lang::lexer::{Tok, lex};
        use crate::audio_processing::commands::lang::{ast::Kind, card};
        let lexed = lex("thru");
        assert_eq!(lexed.tokens[0].kind, Tok::Kind(Kind::Fx(FxType::Thru)));
        let mut cp = session();
        assert!(local(&mut cp, "help thru").contains("thru"));
        assert!(local(&mut cp, "help").contains("thru"));
        assert!(card().contains("thru"));
        // it lowers, with the setting in its own slot
        let a = fx_args(lok(&mut cp, "kick { thru t amt=0.5 }"));
        let (kind, p, _) = insert(&a);
        assert_eq!(kind, FxKind::new(FxType::Thru));
        assert_eq!(p[ix(FxType::Thru, "amt")], 0.5);
        // state prints it back
        assert!(local(&mut cp, "state").contains("thru t amt=0.5"));
        // an lfo targets it
        lok(&mut cp, "kick { lfo l to=t.amt depth=0.1 freq=1hz }");
        let es = &cp.engine_state;
        let lfo = es.procs[&es.voices[&es.voice_id("kick").unwrap()].procs["l"]]
            .lfo
            .clone()
            .unwrap();
        assert_eq!(
            lfo.target,
            crate::audio_processing::processes::LfoTarget::Fx { id: 0, param: 0 }
        );
        // a glide on it works, and is named by its setting
        let edit = fx_cmds(lok(&mut cp, "kick.t amt=1 over 4"));
        assert!(matches!(
            edit[0].op,
            FxOp::Set { param: 0, value, frames } if value == 1.0 && frames > 0
        ));
        let glide = GlideRef::Fx {
            owner: edit[0].owner,
            id: edit[0].id,
            param: 0,
        };
        assert_eq!(cp.engine_state.glide_name(glide), "kick.t.amt");
    }

    #[test]
    fn effects_create_in_block_and_edit_by_path() {
        let mut cp = session();
        let a = fx_args(lok(&mut cp, "kick { drive dirt amount=24 mix=0.5 }"));
        let (kind, p, room) = insert(&a);
        assert_eq!((a.id, kind, room), (0, DRIVE_FX, Some(1)));
        assert_eq!(
            (
                p[ix(DRIVE, "amount")],
                p[ix(DRIVE, "mix")],
                p[ix(DRIVE, "gain")]
            ),
            (24.0, 0.5, 1.0)
        );
        assert_eq!(a.owner, FxOwner::Voice(vid(&cp, "kick")));

        let b = fx_args(lok(&mut cp, "kick { filter lo type=hp cutoff=800 q=0.9 }"));
        let (kind, p, room) = insert(&b);
        assert_eq!((b.id, kind, room), (1, HIGHPASS, Some(2)));
        assert_eq!(
            (
                p[ix(FILTER, "cutoff")],
                p[ix(FILTER, "q")],
                p[ix(FILTER, "base")]
            ),
            (800.0, 0.9, 0.0)
        );

        let c = fx_args(lok(&mut cp, "kick { crush bit bits=6 down=4 }"));
        let (kind, p, room) = insert(&c);
        assert_eq!((c.id, kind, room), (2, CRUSH_FX, Some(4)));
        assert_eq!((p[ix(CRUSH, "bits")], p[ix(CRUSH, "down")]), (6.0, 4.0));
        assert_eq!(
            chain(&cp, "kick").iter().map(|f| f.id).collect::<Vec<_>>(),
            [0, 1, 2]
        );

        // by path, with a glide on one and not the other
        let edit = fx_cmds(lok(&mut cp, "kick.lo cutoff=2000 over 4 q=2"));
        assert_eq!(edit.len(), 2);
        assert!(matches!(
            edit[0].op,
            FxOp::Set { param, value, frames }
                if param == ix(FILTER, "cutoff") as u8 && value == 2000.0 && frames > 0
        ));
        assert!(matches!(
            edit[1].op,
            FxOp::Set { param, value, frames: 0 } if param == ix(FILTER, "q") as u8 && value == 2.0
        ));
        assert_eq!(
            effect(&cp, "kick", "lo").params[ix(FILTER, "cutoff")],
            2000.0
        );
        // short names
        lok(&mut cp, "kick.dirt a=3 m=1");
        assert_eq!(effect(&cp, "kick", "dirt").params[ix(DRIVE, "amount")], 3.0);
        assert_eq!(effect(&cp, "kick", "dirt").params[ix(DRIVE, "mix")], 1.0);

        // the same name on another voice is another effect
        let other = fx_args(lok(&mut cp, "hat { drive dirt }"));
        assert_eq!(other.id, 0);
        // a top-level `filter voice.NAME` creates too
        let top = fx_args(lok(&mut cp, "filter kick.top cutoff=300"));
        assert_eq!(top.id, 3);
        assert_eq!(chain(&cp, "kick").len(), 4);
    }

    #[test]
    fn many_effects_grow_the_chain() {
        let mut cp = session();
        let mut rooms = Vec::new();
        for i in 0..20 {
            let a = fx_args(lok(&mut cp, &format!("kick {{ drive d{i} amount={i} }}")));
            assert_eq!(a.id, i as u16);
            rooms.push(insert(&a).2);
        }
        // a bigger empty Vec comes when the chain is full: at 0, 1, 2, 4, 8
        // and 16 effects, with twice the room each time
        let given: Vec<(usize, usize)> = rooms
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.map(|r| (i, r)))
            .collect();
        assert_eq!(given, [(0, 1), (1, 2), (2, 4), (4, 8), (8, 16), (16, 32)]);
        assert_eq!(
            names(&cp, "kick"),
            (0..20).map(|i| format!("d{i}")).collect::<Vec<_>>()
        );
    }

    #[test]
    fn drive_setting_on_filter_is_e401() {
        let mut cp = session();
        lok(&mut cp, "kick { filter lo }");
        let errs = lerr(&mut cp, "kick.lo amount=3");
        assert_eq!(errs[0].code, Code::UnknownSetting);
        let text = errs[0].to_string();
        assert!(text.contains("cutoff") && text.contains("base"), "{text}");
        let errs = lerr(&mut cp, "kick { drive d bits=3 }");
        assert_eq!(errs[0].code, Code::UnknownSetting);
        // the mirror is as it was
        assert_eq!(chain(&cp, "kick").len(), 1);
    }

    #[test]
    fn filter_retype_keeps_cutoff_q() {
        let mut cp = session();
        lok(&mut cp, "kick { filter lo type=lp cutoff=1500 q=2 }");
        let cmds = fx_cmds(lok(&mut cp, "kick.lo type=hp"));
        assert_eq!(cmds.len(), 1);
        let FxOp::Retype { kind, params } = &cmds[0].op else {
            panic!("not a Retype");
        };
        assert_eq!(*kind, HIGHPASS);
        assert_eq!(
            (params[ix(FILTER, "cutoff")], params[ix(FILTER, "q")]),
            (1500.0, 2.0)
        );
        let lo = effect(&cp, "kick", "lo");
        assert_eq!(lo.kind, HIGHPASS);
        assert_eq!(
            (lo.params[ix(FILTER, "cutoff")], lo.params[ix(FILTER, "q")]),
            (1500.0, 2.0)
        );
        // its place is kept
        lok(&mut cp, "kick { drive tail }");
        lok(&mut cp, "kick.lo t=notch");
        assert_eq!(names(&cp, "kick"), ["lo", "tail"]);
        // a type is a filter's: not a drive's
        assert_eq!(
            lerr(&mut cp, "kick.tail type=hp")[0].code,
            Code::UnknownSetting
        );
        assert_eq!(
            lerr(&mut cp, "kick.lo type=lowpass")[0].code,
            Code::WrongType
        );
    }

    #[test]
    fn other_kind_same_name_e407() {
        let mut cp = session();
        lok(&mut cp, "kick { filter lo }");
        let errs = lerr(&mut cp, "kick { drive lo }");
        assert_eq!(errs[0].code, Code::InUse);
        assert!(
            errs[0].to_string().contains("'lo' is a filter"),
            "{}",
            errs[0]
        );
        assert_eq!(lerr(&mut cp, "crush kick.lo")[0].code, Code::InUse);
        // the effect shares its voice's scope with seqs and packets
        assert_eq!(
            lerr(&mut cp, "kick { seq lo 1 2 }")[0].code,
            Code::KindClash
        );
        lok(&mut cp, "kick { seq beat 1 2 }");
        assert_eq!(
            lerr(&mut cp, "kick { drive beat }")[0].code,
            Code::KindClash
        );
        assert_eq!(
            lerr(&mut cp, "kick { drive all }")[0].code,
            Code::ReservedWord
        );
        assert_eq!(chain(&cp, "kick").len(), 1);
    }

    #[test]
    fn effect_at_session_level_is_e402() {
        let mut cp = session();
        assert_eq!(lerr(&mut cp, "filter lo")[0].code, Code::WrongType);
        assert_eq!(lerr(&mut cp, "drive kick")[0].code, Code::WrongType);
        // a clock can't hold one
        assert_eq!(lerr(&mut cp, "drive main.lo")[0].code, Code::WrongType);
        // a group's block holds a group effect (FXb)
        lok(&mut cp, "group g { kick }");
        assert!(chain(&cp, "g.kick").is_empty());
    }

    #[test]
    fn base_and_gain_take_db() {
        let mut cp = session();
        let a = fx_args(lok(
            &mut cp,
            "kick { filter lows type=lp cutoff=400 base=-12db gain=6db }",
        ));
        let (_, p, _) = insert(&a);
        assert!(
            (p[ix(FILTER, "base")] - 0.251_188_64).abs() < 1e-6,
            "{}",
            p[ix(FILTER, "base")]
        );
        assert!(
            (p[ix(FILTER, "gain")] - 1.995_262_3).abs() < 1e-6,
            "{}",
            p[ix(FILTER, "gain")]
        );
        // the plan's shelf and bell examples
        for line in [
            "kick { filter shelf type=lp cutoff=400 base=-12db gain=12db }",
            "kick { filter cut type=hp cutoff=400 base=-6db }",
            "kick { filter mid type=notch cutoff=2000 q=2 base=-6db }",
        ] {
            lok(&mut cp, line);
        }
        // linear works too, and so does dB on drive's and crush's gain
        lok(&mut cp, "kick { drive d gain=-6db }");
        lok(&mut cp, "kick { crush c gain=2 }");
        // base can't be above 1: 3db is 1.41
        assert_eq!(
            lerr(&mut cp, "kick.lows base=3db")[0].code,
            Code::OutOfRange
        );
        // only settings that take dB do
        assert_eq!(lerr(&mut cp, "kick.lows q=3db")[0].code, Code::WrongType);
        assert_eq!(lerr(&mut cp, "kick.d amount=3db")[0].code, Code::WrongType);
        // the voice's own gain still takes it
        lok(&mut cp, "kick gain=-3db");
        assert_eq!(lerr(&mut cp, "kick pan=3db")[0].code, Code::WrongType);
    }

    #[test]
    fn remove_frees_slot() {
        let mut cp = session();
        lok(&mut cp, "kick { drive a }");
        lok(&mut cp, "kick { drive b }");
        lok(&mut cp, "kick { drive c }");
        let cmd = fx_args(lok(&mut cp, "remove kick.b"));
        assert_eq!(cmd.id, 1);
        assert!(matches!(cmd.op, FxOp::Remove));
        assert_eq!(names(&cp, "kick"), ["a", "c"]);
        // the freed id is reused, at the end of the chain
        let again = fx_args(lok(&mut cp, "kick { filter d }"));
        assert_eq!(again.id, 1);
        assert_eq!(names(&cp, "kick"), ["a", "c", "d"]);
        // the engine keeps its room: nothing more is sent while it fits
        assert_eq!(insert(&again).2, None);
        assert_eq!(lerr(&mut cp, "remove kick.b")[0].code, Code::UnknownName);
        // removing the voice takes its effects
        lok(&mut cp, "remove kick");
        assert_eq!(lerr(&mut cp, "kick.a mix=0")[0].code, Code::UnknownName);
    }

    #[test]
    fn same_restatement_noop() {
        let mut cp = session();
        lok(&mut cp, "kick { filter lo cutoff=500 q=1 }");
        assert!(local(&mut cp, "kick { filter lo cutoff=500 q=1 }").contains("W101"));
        assert!(local(&mut cp, "kick.lo cutoff=500").contains("W101"));
        assert!(local(&mut cp, "kick.lo").contains("W101"));
        assert_eq!(chain(&cp, "kick").len(), 1);
        // a change in one setting sends just that one
        let cmds = fx_cmds(lok(&mut cp, "kick.lo cutoff=500 q=3"));
        assert_eq!(cmds.len(), 1);
    }

    #[test]
    fn stop_start_pause_resume_bypass() {
        let mut cp = session();
        lok(&mut cp, "kick { filter lo }");
        let bypass = |cp: &mut CmdProcessor, line: &str| {
            let cmds = fx_cmds(lok(cp, line));
            assert_eq!(cmds.len(), 1, "{line}");
            match cmds[0].op {
                FxOp::Bypass { on, clear } => (on, clear),
                _ => panic!("not a Bypass"),
            }
        };
        let state = |cp: &CmdProcessor| effect(cp, "kick", "lo").bypass;
        assert_eq!(bypass(&mut cp, "stop kick.lo"), (true, true));
        assert_eq!(state(&cp), Some(Bypass::Stopped));
        assert!(local(&mut cp, "stop kick.lo").contains("W101"));
        assert!(local(&mut cp, "pause kick.lo").contains("W101"));
        assert_eq!(bypass(&mut cp, "start kick.lo"), (false, true));
        assert_eq!(state(&cp), None);
        assert!(local(&mut cp, "start kick.lo").contains("W101"));
        assert!(local(&mut cp, "resume kick.lo").contains("W101"));
        assert_eq!(bypass(&mut cp, "pause kick.lo"), (true, false));
        assert_eq!(state(&cp), Some(Bypass::Paused));
        assert_eq!(bypass(&mut cp, "resume kick.lo"), (false, false));
        // paused, then stopped: the memory goes
        bypass(&mut cp, "pause kick.lo");
        assert_eq!(bypass(&mut cp, "stop kick.lo"), (true, true));
    }

    #[test]
    fn stop_all_leaves_effects() {
        let mut cp = session();
        lok(&mut cp, "kick { filter lo }");
        for line in ["stop all", "start all", "pause all", "resume all"] {
            let cmds = batch_or_one(lok(&mut cp, line));
            assert!(cmds.iter().all(|c| !matches!(c, Command::Fx(_))), "{line}");
        }
        assert_eq!(effect(&cp, "kick", "lo").bypass, None);
    }

    #[test]
    fn state_prints_effects_and_bypass_and_round_trips() {
        let mut cp = session();
        lok(&mut cp, "kick { drive dirt amount=24 mix=0.5 }");
        lok(
            &mut cp,
            "kick { filter lo type=hp cutoff=800 q=0.9 base=-12db gain=2 }",
        );
        lok(&mut cp, "kick { crush bit bits=6 down=4 }");
        lok(&mut cp, "kick { seq beat 1 3 }");
        lok(&mut cp, "hat { filter plain }");
        lok(&mut cp, "stop kick.lo");
        lok(&mut cp, "pause hat.plain");
        let text = cp.state(None).unwrap();
        assert!(text.contains("drive dirt amount=24 mix=0.5"), "{text}");
        assert!(
            text.contains("filter lo type=hp cutoff=800 q=0.9 base=-12db gain=2"),
            "{text}"
        );
        assert!(text.contains("crush bit bits=6 down=4"), "{text}");
        assert!(text.contains("hat { filter plain }"), "{text}");
        assert!(text.contains("stop kick.lo"), "{text}");
        assert!(text.contains("pause hat.plain"), "{text}");
        // in chain order
        let (d, l, c) = (
            text.find("drive dirt").unwrap(),
            text.find("filter lo").unwrap(),
            text.find("crush bit").unwrap(),
        );
        assert!(d < l && l < c, "{text}");

        // the text recreates the session, and feeding it back changes nothing
        let mut fresh = lang_processor();
        lok(&mut fresh, &text);
        assert_eq!(fresh.state(None).unwrap(), text);
        let answer = local(&mut fresh, &text);
        for line in answer.lines() {
            assert!(line.contains("W101"), "{line}");
        }
        // one effect by itself
        let one = cp.state(Some("kick.lo")).unwrap();
        assert!(
            one.contains("filter lo") && one.contains("stop kick.lo"),
            "{one}"
        );
    }

    fn gchain<'a>(cp: &'a CmdProcessor, group: &str) -> &'a [FxRepr] {
        let g = cp.engine_state.group_names[group];
        &cp.engine_state.groups[&g].fx
    }

    // FXb: group effects

    #[test]
    fn fx_in_group_block_belongs_to_group() {
        let mut cp = session();
        lok(&mut cp, "group g { kick; hat }");
        let g = cp.engine_state.group_names["g"];
        let a = fx_args(lok(&mut cp, "g { filter lo type=lp cutoff=1200 }"));
        assert_eq!(a.owner, FxOwner::Group(g));
        assert_eq!(a.id, 0);
        // inside a member's block it is the member's
        let b = fx_args(lok(&mut cp, "kick { drive dirt amount=12 }"));
        assert_eq!(b.owner, FxOwner::Voice(vid(&cp, "g.kick")));
        assert_eq!(gchain(&cp, "g").len(), 1);
        assert_eq!(chain(&cp, "g.kick").len(), 1);
        // edits by path, and a bypass, reach the group's effect
        let e = fx_cmds(lok(&mut cp, "g.lo cutoff=300 over 8"));
        assert_eq!(e[0].owner, FxOwner::Group(g));
        assert!(matches!(e[0].op, FxOp::Set { frames, .. } if frames > 0));
        let s = fx_cmds(lok(&mut cp, "stop g.lo"));
        assert!(matches!(s[0].op, FxOp::Bypass { on: true, .. }));
        assert!(gchain(&cp, "g")[0].bypass.is_some());
        // a group declaration with an effect inside creates it
        lok(
            &mut cp,
            "group g2 { voice sn from \"hat\"; crush c bits=4 }",
        );
        assert_eq!(gchain(&cp, "g2").len(), 1);
    }

    #[test]
    fn group_fx_e601() {
        let mut cp = session();
        lok(&mut cp, "group g { kick }");
        let g = cp.engine_state.group_names["g"];
        // fill the mirror directly: ids are 16 bits
        let (list, _) = cp.engine_state.fx_list_mut(FxOwner::Group(g)).unwrap();
        for i in 0..=u16::MAX {
            list.push(FxRepr {
                name: format!("e{i}"),
                id: i,
                kind: LOWPASS,
                params: defaults(FILTER),
                bypass: None,
            });
        }
        let errs = lerr(&mut cp, "g { filter one_too_many }");
        assert_eq!(errs[0].code, Code::LimitReached);
        assert_eq!(gchain(&cp, "g").len(), 65_536);
    }

    #[test]
    fn remove_group_drops_fx() {
        let mut cp = session();
        lok(&mut cp, "group g { kick }");
        lok(&mut cp, "g { filter lo }");
        lok(&mut cp, "g { drive d }");
        let cmd = fx_args(lok(&mut cp, "remove g.lo"));
        assert!(matches!(cmd.op, FxOp::Remove));
        assert_eq!(gchain(&cp, "g").len(), 1);
        assert_eq!(lerr(&mut cp, "remove g.lo")[0].code, Code::UnknownName);
        lok(&mut cp, "remove g");
        assert_eq!(lerr(&mut cp, "g.d mix=0")[0].code, Code::UnknownName);
        assert!(cp.engine_state.groups.values().all(|r| r.fx.is_empty()));
    }

    #[test]
    fn state_round_trip_group_fx() {
        let mut cp = session();
        lok(&mut cp, "group g { kick; hat }");
        lok(&mut cp, "g { filter lo type=hp cutoff=800 gain=2 }");
        lok(&mut cp, "g { crush bit bits=6 }");
        lok(&mut cp, "kick { drive dirt amount=24 }");
        lok(&mut cp, "stop g.lo");
        let text = cp.state(None).unwrap();
        assert!(text.contains("filter lo type=hp cutoff=800"), "{text}");
        assert!(text.contains("crush bit bits=6"), "{text}");
        assert!(text.contains("stop g.lo"), "{text}");
        let mut fresh = lang_processor();
        lok(&mut fresh, &text);
        assert_eq!(fresh.state(None).unwrap(), text);
        for line in local(&mut fresh, &text).lines() {
            assert!(line.contains("W101"), "{line}");
        }
    }
}

// ---- grid= and rot= on written steps ----

#[test]
fn grid_counts_slots_for_written_steps() {
    let mut cp = seq_session();
    // sixteenth slots in a cycle of 4 beats: 1 5 9 13 are the beats
    let args = seq_args(lok(&mut cp, "hat { seq e 1 5 9 13 grid=16 in 4 }"));
    assert_eq!(args.steps, vec![0.0, 1.0, 2.0, 3.0]);
    // fractions are fine; the marks stay in beats
    let args = seq_args(lok(&mut cp, "hat { seq f 1 2.5%50 3~0.1 grid=8 in 4 }"));
    assert_eq!(args.steps, vec![0.0, 0.75, 1.0]);
    let s = &proc_of(&cp, "hat", "f").seq.steps;
    assert_eq!((s[1].chance, s[2].early), (50.0, 0.1));
    // ranges count slots
    let args = seq_args(lok(&mut cp, "hat { seq r 1..4 grid=16 in 4 }"));
    assert_eq!(args.steps, vec![0.0, 0.25, 0.5, 0.75]);
    // no grid: nothing changes
    let args = seq_args(lok(&mut cp, "hat { seq p 1 2.5 in 4 }"));
    assert_eq!(args.steps, vec![0.0, 1.5]);
    assert_eq!(proc_of(&cp, "hat", "p").seq.slots, None);
}

#[test]
fn rot_moves_written_steps_and_wraps() {
    let mut cp = seq_session();
    // without grid, rot is in beats
    lok(&mut cp, "hat { seq a 1 2 in 4 rot=1 }");
    assert_eq!(at_of(&cp, "hat", "a"), [1.0, 2.0]);
    lok(&mut cp, "hat { seq b 3 4 in 4 rot=2 }");
    assert_eq!(at_of(&cp, "hat", "b"), [0.0, 1.0]);
    // negative rot wraps the other way
    lok(&mut cp, "hat { seq c 1 2 in 4 rot=-1 }");
    assert_eq!(at_of(&cp, "hat", "c"), [0.0, 3.0]);
    // with a grid, in slots
    lok(&mut cp, "hat { seq d 1 5 9 13 grid=16 rot=2 in 4 }");
    assert_eq!(at_of(&cp, "hat", "d"), [0.5, 1.5, 2.5, 3.5]);
    lok(&mut cp, "hat { seq e 15 16 grid=16 rot=3 in 4 }");
    assert_eq!(at_of(&cp, "hat", "e"), [0.25, 0.5]);
}

#[test]
fn written_steps_must_be_inside_the_grid() {
    let mut cp = seq_session();
    let e = lerr(&mut cp, "hat { seq a 1 17 grid=16 in 4 }");
    assert_eq!(e[0].code, Code::StepOutsideCycle);
    assert!(
        e[0].message.contains("step 17 is outside the grid of 16"),
        "{}",
        e[0]
    );
    // without a grid it is still the cycle
    let e = lerr(&mut cp, "hat { seq a 1 5 in 4 }");
    assert!(e[0].message.contains("outside the cycle of 4"), "{}", e[0]);
    // a step valid in the cycle but not in a smaller grid
    assert_eq!(
        lerr(&mut cp, "hat { seq b 1 3 grid=2 in 4 }")[0].code,
        Code::StepOutsideCycle
    );
    // and restating the grid checks the steps the seq has
    lok(&mut cp, "hat { seq c 1 9 grid=16 in 4 }");
    assert_eq!(
        lerr(&mut cp, "hat.c grid=8")[0].code,
        Code::StepOutsideCycle
    );
}

#[test]
fn restating_grid_or_rot_moves_written_steps() {
    let mut cp = seq_session();
    lok(&mut cp, "hat { seq e 1 5 9 13 grid=16 in 4 }");
    lok(&mut cp, "hat.e rot=1");
    assert_eq!(at_of(&cp, "hat", "e"), [0.25, 1.25, 2.25, 3.25]);
    // grid= keeps the rot, in= keeps both
    lok(&mut cp, "hat.e grid=32");
    assert_eq!(at_of(&cp, "hat", "e"), [0.125, 0.625, 1.125, 1.625]);
    lok(&mut cp, "hat.e in 8");
    assert_eq!(at_of(&cp, "hat", "e"), [0.25, 1.25, 2.25, 3.25]);
    // a restated list keeps grid and rot
    lok(&mut cp, "hat.e 1 17 in 8");
    assert_eq!(at_of(&cp, "hat", "e"), [0.25, 4.25]);
    let g = proc_of(&cp, "hat", "e").seq.slots.unwrap();
    assert_eq!((g.grid, g.rot), (Some(32), 1));
    // plain written steps take a grid, their numbers becoming slots
    lok(&mut cp, "hat { seq p 1 3 in 4 }");
    lok(&mut cp, "hat.p grid=8");
    assert_eq!(at_of(&cp, "hat", "p"), [0.0, 1.0]);
    // grid= alone (no steps anywhere) is still an error
    assert_eq!(
        lerr(&mut cp, "hat { seq z grid=4 in 4 }")[0].code,
        Code::MissingItem
    );
    // steps= drops the written slots
    lok(&mut cp, "hat.e steps=3");
    assert_eq!(proc_of(&cp, "hat", "e").seq.slots, None);
}

#[test]
fn state_prints_written_grid_and_rot_and_round_trips() {
    let mut cp = seq_session();
    lok(&mut cp, "hat { seq e 1 5%50 9 13 grid=16 rot=2 in 4 }");
    let text = cp.state(Some("hat.e")).unwrap();
    assert_eq!(text, "hat { seq e 1 5%50 9 13 grid=16 rot=2 }");
    let before = proc_of(&cp, "hat", "e").seq;
    // run it back in a fresh session
    let mut cp2 = seq_session();
    lok(&mut cp2, &text);
    let after = proc_of(&cp2, "hat", "e").seq;
    assert_eq!(before.steps, after.steps);
    assert_eq!(before.slots, after.slots);
    assert_eq!(cp2.state(Some("hat.e")).unwrap(), text);
    // rot alone prints too
    lok(&mut cp, "hat { seq r 1 2 in 4 rot=-1 }");
    assert_eq!(cp.state(Some("hat.r")).unwrap(), "hat { seq r 1 2 rot=-1 }");
}

#[test]
fn window_clamps_with_a_warning() {
    crate::audio_processing::blast_time::sample_rate::set(48000);
    let mut cp = lang_processor();
    let window = |cp: &CmdProcessor| cp.engine_state.voices.values().next().unwrap().window;
    lok(&mut cp, "voice kick from \"kick\" shift=spectral pitch=2st");
    assert_eq!(window(&cp), 4320);
    // a bare number is a fraction of the voice's period, as for any duration
    for (line, frames, said) in [
        ("kick window=5ms", 960, Some("clamped to 20ms")),
        ("kick w=1s", 12000, Some("clamped to 250ms")),
        ("kick window=60ms", 2880, None),
        ("kick window=20ms", 960, None),
        ("kick window=250ms", 12000, None),
    ] {
        lok(&mut cp, line);
        assert_eq!(window(&cp), frames, "{line}");
        match said {
            Some(text) => {
                assert!(cp.outbox.contains(text), "{line}: {}", cp.outbox);
                assert!(cp.outbox.contains("W104"), "{line}: {}", cp.outbox);
            }
            None => assert!(!cp.outbox.contains("clamped"), "{line}: {}", cp.outbox),
        }
    }
    // a rate is not a length
    assert_eq!(lerr(&mut cp, "kick window=2hz")[0].code, Code::WrongType);
}

#[test]
fn bounce_clamps_with_a_warning_and_sends_only_changes() {
    crate::audio_processing::blast_time::sample_rate::set(48000);
    let mut cp = lang_processor();
    let bounce = |cp: &CmdProcessor| cp.engine_state.voices.values().next().unwrap().bounce;
    lok(&mut cp, "voice kick from \"kick\" loop=pingpong");
    // the default is 2 ms
    assert_eq!(bounce(&cp), 96);
    for (line, frames, said) in [
        ("kick bounce=9ms", 240, Some("clamped to 5ms")),
        ("kick u=1ms", 48, None),
        ("kick bounce=0", 0, None),
        ("kick bounce=5ms", 240, None),
        ("kick bounce=2ms", 96, None),
    ] {
        lok(&mut cp, line);
        assert_eq!(bounce(&cp), frames, "{line}");
        match said {
            Some(text) => {
                assert!(cp.outbox.contains(text), "{line}: {}", cp.outbox);
                assert!(cp.outbox.contains("W104"), "{line}: {}", cp.outbox);
            }
            None => assert!(!cp.outbox.contains("clamped"), "{line}: {}", cp.outbox),
        }
    }
    // a rate is not a length
    assert_eq!(lerr(&mut cp, "kick bounce=2hz")[0].code, Code::WrongType);
}

// ---- and, then, thenif, wait (PKa) ----

#[test]
fn and_is_same_instant() {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice kick from \"kick\" and voice hat from \"hat\"",
    );
    let cmds = batch(lok(&mut cp, "kick gain=0.2 and hat pan=-1"));
    assert_eq!(names(&cmds), ["SetParam", "SetParam"]);
}

#[test]
fn amp_is_e105() {
    let mut cp = lang_processor();
    let errs = lerr(&mut cp, "voice kick from \"kick\" & voice hat from \"hat\"");
    assert_eq!(errs[0].code, Code::AmpRemoved);
    assert_eq!(errs[0].code.as_str(), "E105");
    assert_eq!(errs[0].message, "'&' is now 'and'");
    // nothing was made
    assert!(cp.engine_state.voices.is_empty());
}

#[test]
fn ampamp_is_e105_then() {
    let mut cp = lang_processor();
    let errs = lerr(
        &mut cp,
        "voice kick from \"kick\" && voice hat from \"hat\"",
    );
    assert_eq!(errs[0].code, Code::AmpRemoved);
    assert_eq!(errs[0].message, "'&&' is now 'then'");
}

#[test]
fn then_is_reserved_e303() {
    let mut cp = lang_processor();
    for word in ["and", "then", "thenif", "wait"] {
        let errs = lerr(&mut cp, &format!("voice {word} from \"kick\""));
        assert_eq!(errs[0].code, Code::ReservedWord, "{word}");
    }
    assert!(cp.engine_state.voices.is_empty());
}

#[test]
fn chain_shapes_lower_to_entries() {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice kick from \"kick\" and voice hat from \"hat\"",
    );
    // a then b: the mark, a, then one entry for b
    let cmds = batch(lok(&mut cp, "kick gain=0 over 1b then hat gain=0.5"));
    assert_eq!(names(&cmds), ["MarkGlides", "SetParam", "Schedule"]);
    assert_eq!(schedule_count_of(&cmds), 1);
    // three parts: head, statement, tail for the part after
    let cmds = batch(lok(
        &mut cp,
        "kick gain=1 over 1b then hat gain=0 over 1b then kick gain=0.5",
    ));
    assert_eq!(
        names(&cmds),
        ["MarkGlides", "SetParam", "Schedule", "Schedule", "Schedule"]
    );
    // the tail holds the rest of the line and no statement of its own
    let Command::Schedule(tail) = &cmds[4] else {
        panic!("a Schedule")
    };
    assert!(matches!(
        &*tail.cmd,
        Command::Batch(_) | Command::Schedule(_)
    ));
    // numbers go by order in the tree, outermost first
    let mut seen = Vec::new();
    fn numbers(cmd: &Command, seen: &mut Vec<u64>) {
        match cmd {
            Command::Schedule(a) => {
                seen.push(a.seq_no);
                numbers(&a.cmd, seen);
            }
            Command::Batch(a) => a.cmds.iter().for_each(|c| numbers(c, seen)),
            _ => {}
        }
    }
    cmds.iter().for_each(|c| numbers(c, &mut seen));
    // the head, the statement and the tail, then the last statement inside it
    assert_eq!(seen, [1, 2, 3, 4]);
}

fn schedule_count_of(cmds: &[Command]) -> u64 {
    cmds.iter()
        .map(crate::audio_processing::commands::reprs::schedule_count)
        .sum()
}

#[test]
fn wait_after_and_pieces() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\"");
    // a wait at the front is just the timing of what follows
    let cmd = lok(&mut cp, "wait 2 then kick gain=0.5");
    let Command::Schedule(a) = cmd else {
        panic!("a Schedule")
    };
    assert!(matches!(a.when, WhenSpec::After { ticks, .. } if ticks == 2.0));
    // a wait can't be a name
    assert_eq!(
        lerr(&mut cp, "wait 0 then kick gain=0.1")[0].code,
        Code::OutOfRange
    );
    assert_eq!(
        lerr(&mut cp, "wait 1ms by nope then kick gain=0.1")[0].code,
        Code::UnknownName
    );
}
