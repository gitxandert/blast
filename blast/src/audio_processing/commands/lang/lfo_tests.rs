//! LFO's tests: the language, the mirror, and the engine end to end.

use super::Code;
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::testing::*;
use crate::audio_processing::commands::*;
use crate::audio_processing::engine::VoiceParam;
use crate::audio_processing::processes::{LfoRate, LfoShape, LfoTarget};
use crate::audio_processing::test_util::{pcm, render, run, setup, track};
use crate::file_parsing::decode_helpers::AudioFile;

// ---- lowering ----

fn session() -> CmdProcessor {
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\" on 1000ms");
    lok(&mut cp, "voice hat from \"hat\"");
    cp
}

fn lfo_of(cp: &CmdProcessor, voice: &str, name: &str) -> LfoRepr {
    let es = &cp.engine_state;
    es.procs[&es.voices[&es.voice_id(voice).unwrap()].procs[name]]
        .lfo
        .clone()
        .unwrap()
}

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

fn lfo_args(cmd: Command) -> LfoArgs {
    match without_shifters(cmd) {
        Command::Lfo(args) => args,
        other => panic!("expected an Lfo, got a {}", name_of(&other)),
    }
}

fn name_of(cmd: &Command) -> &'static str {
    match cmd {
        Command::Lfo(_) => "Lfo",
        Command::LfoEdit(_) => "LfoEdit",
        Command::ProcEdit(_) => "ProcEdit",
        Command::Batch(_) => "Batch",
        _ => "another command",
    }
}

fn edit_args(cmd: Command) -> LfoEditArgs {
    match cmd {
        Command::LfoEdit(args) => args,
        other => panic!("expected an LfoEdit, got a {}", name_of(&other)),
    }
}

#[test]
fn lfo_in_a_voice_block() {
    let mut cp = session();
    let args = lfo_args(lok(&mut cp, "kick { lfo wob shape=tri depth=0.3 to=pan }"));
    let kick = cp.engine_state.voice_id("kick").unwrap();
    assert_eq!(args.owner, ProcOwner::Voice(kick));
    assert_eq!(args.shape, LfoShape::Tri);
    assert_eq!(args.target, LfoTarget::Voice(VoiceParam::Pan));
    assert_eq!((args.depth, args.offset, args.phase), (0.3, 0.0, 0.0));
    // it reads kick's clock and moves with it
    let kick_tempo = cp.engine_state.voices[&kick].tempo;
    assert_eq!(args.tempo, TempoUse::Shared(kick_tempo));
    let proc = &cp.engine_state.procs[&args.proc];
    assert_eq!(proc.link, TempoLink::Voice);
    assert_eq!(proc.tempo, kick_tempo);
    // the short forms
    let args = lfo_args(lok(
        &mut cp,
        "kick { lfo w2 d=1 t=gain s=saw o=0.5 p=0.25 r=4 }",
    ));
    assert_eq!(args.shape, LfoShape::Saw);
    assert_eq!((args.depth, args.offset, args.phase), (1.0, 0.5, 0.25));
    assert_eq!(lfo_of(&cp, "kick", "w2").seed, Some(4));
}

#[test]
fn lfo_needs_to_and_depth() {
    let mut cp = session();
    for line in [
        "kick { lfo a }",
        "kick { lfo a to=pan }",
        "kick { lfo a depth=1 }",
    ] {
        assert_eq!(lerr(&mut cp, line)[0].code, Code::MissingItem, "{line}");
    }
    assert!(cp.engine_state.procs.is_empty());
}

#[test]
fn lfo_in_or_on_e402() {
    let mut cp = session();
    for line in [
        "kick { lfo a depth=1 to=pan in 4 }",
        "kick { lfo a depth=1 to=pan on 93b }",
    ] {
        let errs = lerr(&mut cp, line);
        assert_eq!(errs[0].code, Code::WrongType, "{line}");
        assert!(errs[0].message.contains("freq="), "{}", errs[0]);
    }
    assert!(cp.engine_state.procs.is_empty());
}

#[test]
fn lfo_default_one_cycle_per_period() {
    let mut cp = session();
    lok(&mut cp, "voice pad from \"snare\" in 8");
    let args = lfo_args(lok(&mut cp, "kick { lfo a depth=1 to=gain }"));
    assert_eq!(args.rate, LfoRate::Clock { ticks: 4.0 });
    // the owner's period, and bare numbers are periods
    let args = lfo_args(lok(&mut cp, "pad { lfo a depth=1 to=gain }"));
    assert_eq!(args.rate, LfoRate::Clock { ticks: 8.0 });
    let args = lfo_args(lok(&mut cp, "pad { lfo b depth=1 to=gain freq=0.5 }"));
    assert_eq!(args.rate, LfoRate::Clock { ticks: 4.0 });
    let args = lfo_args(lok(&mut cp, "kick { lfo b depth=1 to=gain freq=2 }"));
    assert_eq!(args.rate, LfoRate::Clock { ticks: 8.0 });
    // and they follow the owner's period
    lok(&mut cp, "kick in 2");
    assert_eq!(lfo_of(&cp, "kick", "a").rate, LfoRate::Clock { ticks: 2.0 });
    assert_eq!(lfo_of(&cp, "kick", "b").rate, LfoRate::Clock { ticks: 4.0 });
}

#[test]
fn freq_spellings() {
    let mut cp = session();
    let rate = |cp: &mut CmdProcessor, name: &str, freq: &str| {
        lfo_args(lok(
            cp,
            &format!("kick {{ lfo {name} depth=1 to=gain freq={freq} }}"),
        ))
        .rate
    };
    assert_eq!(rate(&mut cp, "a", "5.5hz"), LfoRate::Hz(5.5));
    assert_eq!(rate(&mut cp, "b", "4b"), LfoRate::Clock { ticks: 4.0 });
    assert_eq!(rate(&mut cp, "c", "250ms"), LfoRate::Hz(4.0));
    assert_eq!(rate(&mut cp, "d", "2s"), LfoRate::Hz(0.5));
    sample_rate::set(48_000);
    assert_eq!(rate(&mut cp, "e", "4800f"), LfoRate::Hz(10.0));
    assert_eq!(lfo_of(&cp, "kick", "c").freq_text, "250ms");
    // out of range, as a rate or as a length
    for freq in ["0hz", "201hz", "1ms", "2000s"] {
        let line = format!("kick {{ lfo z depth=1 to=gain freq={freq} }}");
        let code = lerr(&mut cp, &line)[0].code;
        assert_eq!(code, Code::OutOfRange, "{freq}");
    }
}

#[test]
fn lfo_mode_switch_by_restating() {
    let mut cp = session();
    lok(&mut cp, "kick { lfo a depth=1 to=gain freq=2hz }");
    // a clock unit after a free one switches mode
    let edit = edit_args(lok(&mut cp, "kick.a freq=4b"));
    assert_eq!(edit.rate, Some(LfoRate::Clock { ticks: 4.0 }));
    assert!(edit.freq.is_none());
    assert_eq!(lfo_of(&cp, "kick", "a").rate, LfoRate::Clock { ticks: 4.0 });
    // and back
    let edit = edit_args(lok(&mut cp, "kick.a freq=125ms"));
    assert_eq!(edit.rate, Some(LfoRate::Hz(8.0)));
    assert_eq!(lfo_of(&cp, "kick", "a").freq_text, "125ms");
}

#[test]
fn glide_across_unit_kinds_e402() {
    let mut cp = session();
    lok(&mut cp, "kick { lfo a depth=1 to=gain freq=2hz }");
    let errs = lerr(&mut cp, "kick.a freq=4b over 2");
    assert_eq!(errs[0].code, Code::WrongType);
    // within a kind it glides: hz to ms, both free
    let edit = edit_args(lok(&mut cp, "kick.a freq=100ms over 1b"));
    assert!(edit.rate.is_none());
    let f = edit.freq.unwrap();
    assert_eq!(f.value, 10.0);
    assert!(f.frames > 0);
}

#[test]
fn hz_rejected_by_duration_settings_e402() {
    let mut cp = session();
    for line in [
        "voice x from \"snare\" on 5hz",
        "clock c 5hz",
        "kick { seq s 1 jitter=5hz }",
        "kick gain=0 over 5hz",
        "kick { lfo a depth=1 to=gain over 5hz }",
        "kick { lfo a depth=1 to=gain depth=2 over 5hz }",
    ] {
        let errs = lerr(&mut cp, line);
        assert_eq!(errs[0].code, Code::WrongType, "{line}");
    }
    let errs = lerr(&mut cp, "kick gain=0 over 5hz");
    assert!(errs[0].message.contains("hz is a rate"), "{}", errs[0]);
}

#[test]
fn lfo_unknown_target() {
    let mut cp = session();
    lok(&mut cp, "kick { filter lo }");
    let errs = lerr(&mut cp, "kick { lfo a depth=1 to=volume }");
    assert_eq!(errs[0].code, Code::UnknownSetting);
    let hint = errs[0].suggestion.clone().unwrap_or_default();
    assert!(hint.contains("pan") && hint.contains("lo.PARAM"), "{hint}");
    for line in [
        "kick { lfo a depth=1 to=hi.cutoff }",
        "kick { lfo a depth=1 to=kick.lo.cutoff }",
        "kick { lfo a depth=1 to=lo }",
    ] {
        assert_eq!(lerr(&mut cp, line)[0].code, Code::UnknownSetting, "{line}");
    }
    assert!(cp.engine_state.procs.is_empty());
}

#[test]
fn lfo_fx_param_not_for_type() {
    let mut cp = session();
    lok(&mut cp, "kick { filter lo and drive d and crush c }");
    let target = |cp: &mut CmdProcessor, name: &str, to: &str| {
        lfo_args(lok(
            cp,
            &format!("kick {{ lfo {name} depth=1 to={to} freq=1hz }}"),
        ))
        .target
    };
    assert_eq!(
        target(&mut cp, "a", "lo.cutoff"),
        LfoTarget::Fx { id: 0, param: 0 }
    );
    assert_eq!(
        target(&mut cp, "b", "d.amount"),
        LfoTarget::Fx { id: 1, param: 0 }
    );
    assert_eq!(
        target(&mut cp, "lc", "c.bits"),
        LfoTarget::Fx { id: 2, param: 0 }
    );
    // a setting the effect's type doesn't have
    let errs = lerr(&mut cp, "kick { lfo z depth=1 to=d.cutoff }");
    assert_eq!(errs[0].code, Code::UnknownSetting);
    assert!(
        errs[0]
            .suggestion
            .clone()
            .unwrap_or_default()
            .contains("amount"),
        "{}",
        errs[0]
    );
    assert_eq!(
        lerr(&mut cp, "kick { lfo z depth=1 to=lo.bits }")[0].code,
        Code::UnknownSetting
    );
}

#[test]
fn lfo_at_session_level() {
    let mut cp = session();
    let errs = lerr(&mut cp, "lfo wob depth=1 to=gain");
    assert_eq!(errs[0].code, Code::WrongType);
    assert!(errs[0].message.contains("owner"), "{}", errs[0]);
    // a clock can't hold one either
    assert_eq!(
        lerr(&mut cp, "lfo main.wob depth=1 to=gain")[0].code,
        Code::WrongType
    );
}

#[test]
fn lfo_restate_glides() {
    let mut cp = session();
    lok(&mut cp, "kick { lfo wob depth=0.2 to=pan }");
    let edit = edit_args(lok(&mut cp, "kick.wob depth=0.6 over 4"));
    let d = edit.depth.unwrap();
    assert_eq!(d.value, 0.6);
    assert!(d.frames > 0);
    assert_eq!(lfo_of(&cp, "kick", "wob").depth, 0.6);
    // without `over` it lands at once
    let edit = edit_args(lok(&mut cp, "kick.wob depth=0.7 offset=0.1"));
    assert_eq!(edit.depth.unwrap().frames, 0);
    assert_eq!(edit.offset.unwrap().value, 0.1);
    // shape, target, phase and seed don't glide
    let errs = lerr(&mut cp, "kick.wob shape=saw over 2");
    assert_eq!(errs[0].code, Code::WrongType);
    let edit = edit_args(lok(&mut cp, "kick.wob shape=saw to=gain phase=0.5 seed=3"));
    assert_eq!(edit.shape, Some(LfoShape::Saw));
    assert_eq!(edit.target, Some(LfoTarget::Voice(VoiceParam::Gain)));
    assert_eq!(edit.phase, Some(0.5));
    assert!(edit.rng.is_some());
    let r = lfo_of(&cp, "kick", "wob");
    assert_eq!(r.target_text, "gain");
    assert_eq!(r.seed, Some(3));
}

#[test]
fn lfo_same_restatement_noop() {
    let mut cp = session();
    lok(
        &mut cp,
        "kick { lfo wob depth=0.2 to=pan freq=2hz shape=tri }",
    );
    let out = local(&mut cp, "kick.wob depth=0.2 to=pan freq=2hz shape=tri");
    assert!(out.contains("W101"), "{out}");
    // the same speed written another way changes only what `state` prints
    let edit = edit_args(lok(&mut cp, "kick.wob freq=500ms"));
    assert!(edit.freq.is_none() && edit.rate.is_none());
    assert_eq!(lfo_of(&cp, "kick", "wob").freq_text, "500ms");
}

#[test]
fn lfo_actions_by_path() {
    let mut cp = session();
    lok(&mut cp, "kick { lfo wob depth=0.2 to=pan }");
    let ops = |cmd: Command| -> Vec<ProcOp> {
        let cmds = match cmd {
            Command::Batch(b) => b.cmds,
            c => vec![c],
        };
        cmds.into_iter()
            .filter_map(|c| match c {
                Command::ProcEdit(a) => Some(a.op),
                _ => None,
            })
            .collect()
    };
    assert_eq!(ops(lok(&mut cp, "pause kick.wob")), [ProcOp::Disable]);
    let r = lfo_of(&cp, "kick", "wob");
    assert!(r.paused && !r.stopped);
    assert_eq!(ops(lok(&mut cp, "resume kick.wob")), [ProcOp::Enable]);
    let r = lfo_of(&cp, "kick", "wob");
    assert!(!r.paused && !r.stopped);
    assert_eq!(
        ops(lok(&mut cp, "stop kick.wob")),
        [ProcOp::Disable, ProcOp::Reset]
    );
    let r = lfo_of(&cp, "kick", "wob");
    assert!(!r.paused && r.stopped);
    assert_eq!(
        ops(lok(&mut cp, "start kick.wob")),
        [ProcOp::Enable, ProcOp::Reset]
    );
    let r = lfo_of(&cp, "kick", "wob");
    assert!(!r.paused && !r.stopped);
    assert_eq!(ops(lok(&mut cp, "remove kick.wob")), [ProcOp::Remove]);
    assert!(cp.engine_state.procs.is_empty());
    assert!(cp.engine_state.voices.values().all(|v| v.procs.is_empty()));
}

#[test]
fn remove_fx_drops_its_lfos_with_w102() {
    let mut cp = session();
    lok(
        &mut cp,
        "kick { filter lo and lfo sweep depth=1 to=lo.cutoff and lfo wob depth=1 to=pan }",
    );
    let cmd = lok(&mut cp, "remove kick.lo");
    let out = std::mem::take(&mut cp.outbox);
    assert!(out.contains("W102") && out.contains("kick.sweep"), "{out}");
    let removed = match cmd {
        Command::Batch(b) => b
            .cmds
            .iter()
            .filter(|c| matches!(c, Command::ProcEdit(a) if a.op == ProcOp::Remove))
            .count(),
        _ => 0,
    };
    assert_eq!(removed, 1);
    let es = &cp.engine_state;
    let kick = &es.voices[&es.voice_id("kick").unwrap()];
    assert!(!kick.procs.contains_key("sweep"));
    // the one that didn't point at it stays
    assert!(kick.procs.contains_key("wob"));
    assert!(kick.fx.is_empty());
}

#[test]
fn removing_the_owner_removes_its_lfos() {
    let mut cp = session();
    lok(&mut cp, "kick { lfo wob depth=1 to=pan }");
    lok(&mut cp, "remove kick");
    assert!(cp.engine_state.procs.is_empty());
}

#[test]
fn lfo_name_clashes() {
    let mut cp = session();
    lok(
        &mut cp,
        "kick { seq s 1 and filter lo and lfo a depth=1 to=pan }",
    );
    for line in [
        "kick { lfo s depth=1 to=pan }",
        "kick { lfo lo depth=1 to=pan }",
        "kick { lfo all depth=1 to=pan }",
        "kick { seq a 1 }",
        "kick { filter a }",
    ] {
        assert!(
            matches!(cp.parse(line), Err(CmdErr::Lang(_)) | Err(_)),
            "{line}"
        );
    }
    // a seq can't be restated as an lfo
    assert_eq!(
        lerr(&mut cp, "lfo kick.s depth=1 to=pan")[0].code,
        Code::KindClash
    );
}

#[test]
fn group_lfo_lowering() {
    let mut cp = session();
    lok(
        &mut cp,
        "group drums { hat; filter lo; lfo pump depth=0.5 to=gain; lfo sweep depth=1 to=lo.cutoff }",
    );
    let g = cp.engine_state.group_id("drums").unwrap();
    let procs = cp.engine_state.groups[&g].procs.clone();
    assert_eq!(procs.len(), 2);
    let pump = cp.engine_state.procs[&procs["pump"]].lfo.clone().unwrap();
    assert_eq!(pump.target, LfoTarget::Voice(VoiceParam::Gain));
    let sweep = cp.engine_state.procs[&procs["sweep"]].lfo.clone().unwrap();
    assert_eq!(sweep.target, LfoTarget::Fx { id: 0, param: 0 });
    assert_eq!(
        cp.engine_state.procs[&procs["pump"]].owner,
        ProcOwner::Group(g)
    );
    // restated in the block, and by path
    lok(&mut cp, "drums { lfo pump depth=0.4 }");
    lok(&mut cp, "drums.pump depth=0.25");
    assert_eq!(
        cp.engine_state.procs[&procs["pump"]]
            .lfo
            .as_ref()
            .unwrap()
            .depth,
        0.25
    );
}

#[test]
fn dotted_value_parses() {
    let mut cp = session();
    lok(&mut cp, "kick { filter lo }");
    // the dotted word is one value, and `start` is a word after `=`
    lok(&mut cp, "kick { lfo a depth=1 to=lo.cutoff }");
    lok(&mut cp, "kick { lfo b depth=0.1 to=start }");
    assert_eq!(
        lfo_of(&cp, "kick", "b").target,
        LfoTarget::Voice(VoiceParam::Start)
    );
    // a space breaks it
    assert!(cp.parse("kick { lfo c depth=1 to=lo .cutoff }").is_err());
    // numbers are unaffected
    lok(&mut cp, "kick gain=1.5");
}

#[test]
fn state_round_trip_lfo() {
    let mut cp = session();
    lok(
        &mut cp,
        "kick { filter lo cutoff=800 and lfo wob shape=tri depth=0.3 to=pan freq=5.5hz offset=0.1 phase=0.25 seed=9 and lfo slow depth=2 to=lo.cutoff and lfo q depth=1 to=gain freq=4b }",
    );
    lok(
        &mut cp,
        "group drums { hat; lfo pump shape=saw depth=-0.5 offset=0.5 to=gain }",
    );
    lok(
        &mut cp,
        "pause kick.slow and stop kick.q and pause drums.pump",
    );
    let text = cp.state(None).unwrap();
    assert!(
        text.contains("lfo wob to=pan depth=0.3 freq=5.5hz shape=tri offset=0.1 phase=0.25 seed=9"),
        "{text}"
    );
    assert!(text.contains("lfo slow to=lo.cutoff depth=2"), "{text}");
    assert!(text.contains("lfo q to=gain depth=1 freq=4b"), "{text}");
    assert!(text.contains("pause kick.slow"), "{text}");
    assert!(text.contains("stop kick.q"), "{text}");
    assert!(text.contains("pause drums.pump"), "{text}");
    // and it reads back to the same thing
    let mut fresh = lang_processor();
    lok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
    assert!(lfo_of(&fresh, "kick", "slow").paused);
    assert!(lfo_of(&fresh, "kick", "q").stopped);
    // one lfo by itself
    let one = cp.state(Some("kick.wob")).unwrap();
    assert_eq!(
        one.lines().next().unwrap(),
        "kick { lfo wob to=pan depth=0.3 freq=5.5hz shape=tri offset=0.1 phase=0.25 seed=9 }"
    );
    assert!(cp.state(Some("kick.q")).unwrap().contains("stop kick.q"));
}

// ---- the engine, end to end ----

const SR_FRAMES: usize = 4800;

// a mono track of a constant, 0.5
fn dc() -> AudioFile {
    track("dc", 16384, 20_000)
}

fn rig(
    extra: Vec<AudioFile>,
    lines: &[&str],
) -> (CmdProcessor, crate::audio_processing::engine::Conductor) {
    sample_rate::set(48_000);
    let mut tracks = vec![dc()];
    tracks.extend(extra);
    let (mut cp, mut c) = setup(1, tracks);
    c.apply(cp.boot());
    for line in lines {
        run(&mut cp, &mut c, line);
    }
    (cp, c)
}

fn near(got: f32, want: f32, tol: f32, what: &str) {
    assert!(
        (got - want).abs() <= tol,
        "{what}: got {got}, wanted {want}"
    );
}

#[test]
fn lfo_gain_tremolo() {
    // 100 Hz at 48 kHz: 480 frames a cycle; the base gain 1 stays, the
    // track is 0.5
    for freq in ["100hz", "10ms", "480f"] {
        let (_cp, mut c) = rig(
            Vec::new(),
            &[
                "voice v from \"dc\" on 1000ms",
                &format!("v {{ lfo t depth=0.5 to=gain freq={freq} }}"),
                "start v",
            ],
        );
        let out = render(&mut c, 1, 1200);
        for (i, got) in out.iter().enumerate() {
            let want = 0.5 * (1.0 + 0.5 * (std::f32::consts::TAU * i as f32 / 480.0).sin());
            near(*got, want, 1e-3, &format!("{freq} frame {i}"));
        }
    }
}

#[test]
fn clock_locked_lfo_follows_the_clock() {
    // a tick is 100 frames; a period of 4 ticks; the default is one cycle
    // per period: 400 frames
    let (_cp, mut c) = rig(
        Vec::new(),
        &[
            "voice v from \"dc\" on 100f",
            "v { lfo t depth=0.5 to=gain }",
            "start v",
        ],
    );
    let out = render(&mut c, 1, 900);
    for (i, got) in out.iter().enumerate() {
        let want = 0.5 * (1.0 + 0.5 * (std::f32::consts::TAU * i as f32 / 400.0).sin());
        near(*got, want, 2e-3, &format!("frame {i}"));
    }
}

#[test]
fn pause_stop_by_name_end_to_end() {
    let (mut cp, mut c) = rig(
        Vec::new(),
        &[
            "voice v from \"dc\" on 1000ms",
            "v { lfo t depth=0.5 to=gain freq=100hz }",
            "start v",
        ],
    );
    render(&mut c, 1, 120);
    // frame 120 is a quarter cycle in: gain 1.5
    run(&mut cp, &mut c, "pause v.t");
    let held = render(&mut c, 1, 500);
    for got in &held {
        near(*got, 0.5 * 1.5, 1e-3, "paused");
    }
    // resume moves again, from the frozen phase
    run(&mut cp, &mut c, "resume v.t");
    let moving = render(&mut c, 1, 240);
    assert!(moving.iter().any(|v| (v - 0.75).abs() > 0.05));
    near(moving[0], 0.75, 5e-3, "resumed where it left off");
    // stop gives the base value back
    run(&mut cp, &mut c, "stop v.t");
    for got in render(&mut c, 1, 500) {
        near(got, 0.5, 1e-6, "stopped");
    }
    // start runs it again from phase 0
    run(&mut cp, &mut c, "start v.t");
    let again = render(&mut c, 1, 200);
    near(again[0], 0.5, 1e-3, "starts at phase");
    near(again[120], 0.75, 5e-3, "a quarter in");
    // removed: base again, and the process is gone
    run(&mut cp, &mut c, "remove v.t");
    for got in render(&mut c, 1, 100) {
        near(got, 0.5, 1e-6, "removed");
    }
}

#[test]
fn two_lfos_on_one_parameter_add() {
    let (_cp, mut c) = rig(
        Vec::new(),
        &[
            "voice v from \"dc\" on 1000ms",
            "v { lfo a depth=0.25 to=gain freq=100hz and lfo b depth=0.5 offset=0.25 to=gain freq=50hz shape=square }",
            "start v",
        ],
    );
    let out = render(&mut c, 1, 960);
    for (i, got) in out.iter().enumerate() {
        let a = 0.25 * (std::f32::consts::TAU * i as f32 / 480.0).sin();
        let b = 0.25 + if i % 960 < 480 { 0.5 } else { -0.5 };
        near(*got, 0.5 * (1.0 + a + b), 1e-3, &format!("frame {i}"));
    }
}

#[test]
fn a_voice_with_an_lfo_plays_at_once() {
    // the lfo is not a seq: the voice must not wait for it
    let (_cp, mut c) = rig(
        Vec::new(),
        &[
            "voice v from \"dc\" on 1000ms",
            "v { lfo t depth=0.5 to=pan }",
            "start v",
        ],
    );
    let out = render(&mut c, 1, 10);
    assert!(out.iter().all(|v| *v != 0.0), "{out:?}");
}

#[test]
fn group_owner_modulates_every_running_member_in_phase() {
    let (mut cp, mut c) = rig(
        vec![track("dc2", 8192, 20_000)],
        &[
            "voice a from \"dc\" on 1000ms",
            "voice b from \"dc2\" on 1000ms",
            "group g { a; b; lfo t depth=0.5 to=gain freq=100hz }",
            "start g",
        ],
    );
    let out = render(&mut c, 1, 960);
    for (i, got) in out.iter().enumerate() {
        let w = 1.0 + 0.5 * (std::f32::consts::TAU * i as f32 / 480.0).sin();
        near(*got, (0.5 + 0.25) * w, 1e-3, &format!("frame {i}"));
    }
    // a member that isn't running isn't touched
    run(&mut cp, &mut c, "stop g.b");
    let out = render(&mut c, 1, 480);
    for (i, got) in out.iter().enumerate() {
        let n = 960 + i;
        let w = 1.0 + 0.5 * (std::f32::consts::TAU * n as f32 / 480.0).sin();
        near(*got, 0.5 * w, 1e-3, &format!("frame {n}"));
    }
}

// a sine of `freq` Hz, as a long track
fn sine_track(name: &str, freq: f32) -> AudioFile {
    let samples: Vec<i16> = (0..30_000)
        .map(|i| (12_000.0 * (std::f32::consts::TAU * freq * i as f32 / 48_000.0).sin()) as i16)
        .collect();
    AudioFile::new(name, "wav", 48000, 1, 16, pcm(&samples))
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

#[test]
fn lfo_fx_cutoff_moves() {
    // a 3 kHz sine through a lowpass at 1 kHz: an lfo of one octave
    // (10 Hz: a cycle in 4800 frames) opens it to 2 kHz a quarter in and
    // closes it to 500 Hz three quarters in
    let (_cp, mut c) = rig(
        vec![sine_track("hi", 3000.0)],
        &[
            "voice v from \"hi\" on 1000ms",
            "v { filter lo type=lp cutoff=1000 and lfo sweep depth=1 to=lo.cutoff freq=10hz }",
            "start v",
        ],
    );
    let out = render(&mut c, 1, SR_FRAMES);
    let open = rms(&out[1100..1300]);
    let shut = rms(&out[3500..3700]);
    assert!(open > 5.0 * shut, "open {open}, shut {shut}");
    // without the lfo it sits between the two
    let (_cp2, mut c2) = rig(
        vec![sine_track("hi", 3000.0)],
        &[
            "voice v from \"hi\" on 1000ms",
            "v { filter lo type=lp cutoff=1000 }",
            "start v",
        ],
    );
    let plain = render(&mut c2, 1, SR_FRAMES);
    let mid = rms(&plain[1100..1300]);
    assert!(open > mid && mid > shut, "{open} {mid} {shut}");
}

#[test]
fn group_fx_target_moves() {
    let (_cp, mut c) = rig(
        vec![sine_track("hi", 3000.0)],
        &[
            "voice v from \"hi\" on 1000ms",
            "group g { v; filter lo type=lp cutoff=1000; lfo sweep depth=1 to=lo.cutoff freq=10hz }",
            "start g",
        ],
    );
    let out = render(&mut c, 1, SR_FRAMES);
    let open = rms(&out[1100..1300]);
    let shut = rms(&out[3500..3700]);
    assert!(open > 5.0 * shut, "open {open}, shut {shut}");
}

#[test]
fn owner_restart_keeps_a_paused_lfo_paused() {
    let (mut cp, mut c) = rig(
        Vec::new(),
        &[
            "voice v from \"dc\" on 1000ms",
            "v { lfo t depth=0.5 to=gain freq=100hz }",
            "start v",
        ],
    );
    render(&mut c, 1, 120);
    run(&mut cp, &mut c, "pause v.t");
    run(&mut cp, &mut c, "start v");
    // still holding the value it had (a quarter in), not stopped
    for got in render(&mut c, 1, 100) {
        near(got, 0.75, 1e-3, "paused through a restart");
    }
}

#[test]
fn depth_on_pitch_takes_semitones_and_cents() {
    let mut cp = session();
    // a bare depth on pitch is semitones; st and c say so
    for (n, (depth, want)) in [("7", 7.0), ("7st", 7.0), ("50c", 0.5), ("-12st", -12.0)]
        .into_iter()
        .enumerate()
    {
        let line = format!("kick {{ lfo p{n} depth={depth} to=pitch }}");
        let args = lfo_args(lok(&mut cp, &line));
        assert!((args.depth - want).abs() < 1e-6, "{depth}: {}", args.depth);
    }
    // an env too
    let args = match without_shifters(lok(&mut cp, "hat { env e1 depth=7st to=pitch }")) {
        Command::Env(a) => a,
        _ => panic!("not an Env"),
    };
    assert_eq!(args.depth, 7.0);
    // but not on another target, and a ratio is no depth
    assert_eq!(
        lerr(&mut cp, "kick { lfo q depth=7st to=pan }")[0].code,
        Code::WrongType
    );
    assert_eq!(
        lerr(&mut cp, "kick { lfo q depth=2x to=pitch }")[0].code,
        Code::WrongType
    );
}

#[test]
fn state_prints_a_pitch_depth_in_semitones() {
    let mut cp = session();
    lok(&mut cp, "kick pitch=0.5");
    lok(&mut cp, "kick { lfo v to=pitch depth=50c freq=4b }");
    lok(&mut cp, "kick { env e to=pitch depth=7 }");
    lok(&mut cp, "kick { lfo g to=gain depth=0.5 freq=4b }");
    let text = cp.state(None).unwrap();
    assert!(text.contains("lfo v to=pitch depth=0.5st"), "{text}");
    assert!(text.contains("env e to=pitch depth=7st"), "{text}");
    assert!(text.contains("lfo g to=gain depth=0.5 "), "{text}");
}
