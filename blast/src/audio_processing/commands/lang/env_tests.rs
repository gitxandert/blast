//! ENV's tests: the language, the mirror, and the engine end to end.

use super::Code;
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::testing::*;
use crate::audio_processing::commands::*;
use crate::audio_processing::engine::{Conductor, VoiceParam};
use crate::audio_processing::processes::{EnvCurve, EnvShape, LfoTarget, Process};
use crate::audio_processing::test_util::{pcm, render, run, setup, track};
use crate::file_parsing::decode_helpers::AudioFile;

// ---- lowering ----

fn session() -> CmdProcessor {
    let mut cp = lang_processor();
    // a tick is 100 frames at 48 kHz, so 0.5 of a 4-tick period is 200 frames
    sample_rate::set(48_000);
    lok(&mut cp, "voice kick from \"kick\" on 100f");
    lok(&mut cp, "voice hat from \"hat\"");
    cp
}

fn env_of(cp: &CmdProcessor, voice: &str, name: &str) -> EnvRepr {
    let es = &cp.engine_state;
    es.procs[&es.voices[&es.voice_id(voice).unwrap()].procs[name]]
        .env
        .clone()
        .unwrap()
}

fn env_args(cmd: Command) -> EnvArgs {
    match cmd {
        Command::Env(args) => args,
        _ => panic!("expected an Env"),
    }
}

fn edit_args(cmd: Command) -> EnvEditArgs {
    match cmd {
        Command::EnvEdit(args) => args,
        _ => panic!("expected an EnvEdit"),
    }
}

#[test]
fn env_defaults() {
    let mut cp = session();
    let args = env_args(lok(&mut cp, "kick { env amp decay=20f sustain=0 }"));
    let kick = cp.engine_state.voice_id("kick").unwrap();
    assert_eq!(args.owner, ProcOwner::Voice(kick));
    assert_eq!(args.target, LfoTarget::Voice(VoiceParam::Gain));
    assert_eq!(args.depth, 1.0);
    assert_eq!(
        args.shape,
        EnvShape {
            attack: 0,
            decay: 20,
            sustain: 0.0,
            hold: None,
            release: 0,
            curve: EnvCurve::Exp,
        }
    );
    // it reads kick's clock and moves with it
    let kick_tempo = cp.engine_state.voices[&kick].tempo;
    assert_eq!(args.tempo, TempoUse::Shared(kick_tempo));
    let proc = &cp.engine_state.procs[&args.proc];
    assert_eq!(proc.link, TempoLink::Voice);
    // the short forms
    let args = env_args(lok(
        &mut cp,
        "kick { env e2 a=2f d=4f s=0.5 h=6f r=8f t=pan p=0.25 c=lin }",
    ));
    assert_eq!(args.target, LfoTarget::Voice(VoiceParam::Pan));
    assert_eq!(args.depth, 0.25);
    assert_eq!(
        args.shape,
        EnvShape {
            attack: 2,
            decay: 4,
            sustain: 0.5,
            hold: Some(6),
            release: 8,
            curve: EnvCurve::Lin,
        }
    );
    // a bare env sits at full level: it says so
    lok(&mut cp, "kick { env flat }");
    let out = std::mem::take(&mut cp.outbox);
    assert!(
        out.contains("W101") && out.contains("does nothing"),
        "{out}"
    );
}

#[test]
fn env_durations_fraction_of_period() {
    let mut cp = session();
    // a period of 4 ticks of 100 frames: a bare 0.5 is 200 frames
    let args = env_args(lok(
        &mut cp,
        "kick { env a attack=0.5 decay=1b release=5ms }",
    ));
    assert_eq!(args.shape.attack, 200);
    assert_eq!(args.shape.decay, 100);
    assert_eq!(args.shape.release, 240);
    assert_eq!(env_of(&cp, "kick", "a").text.attack, "0.5");
    // a longer period stretches the fractions, not the other units
    lok(&mut cp, "voice pad from \"hat\" on 100f in 8");
    let args = env_args(lok(&mut cp, "pad { env a attack=0.5 decay=1b }"));
    assert_eq!(args.shape.attack, 400);
    assert_eq!(args.shape.decay, 100);
    // a later clock change doesn't stretch what was written
    local(&mut cp, "kick in 8");
    assert_eq!(env_of(&cp, "kick", "a").shape.attack, 200);
}

#[test]
fn env_hold_retrigger_word() {
    let mut cp = session();
    let args = env_args(lok(
        &mut cp,
        "kick { env a decay=4f hold=retrigger release=4f }",
    ));
    assert_eq!(args.shape.hold, None);
    // a duration, and back
    let edit = edit_args(lok(&mut cp, "kick.a hold=10f"));
    assert_eq!(edit.shape.unwrap().hold, Some(10));
    let edit = edit_args(lok(&mut cp, "kick.a hold=retrigger"));
    assert_eq!(edit.shape.unwrap().hold, None);
    let errs = lerr(&mut cp, "kick.a hold=now");
    assert_eq!(errs[0].code, Code::WrongType);
    // hit is gone, not an alias
    let errs = lerr(&mut cp, "kick.a hold=hit");
    assert_eq!(errs[0].code, Code::WrongType);
    assert_eq!(errs[0].suggestion.as_deref(), Some("hold=retrigger"));
    assert_eq!(lerr(&mut cp, "kick.a hold=5hz")[0].code, Code::WrongType);
}

#[test]
fn env_at_session_level_e402() {
    let mut cp = session();
    let errs = lerr(&mut cp, "env amp decay=4f");
    assert_eq!(errs[0].code, Code::WrongType, "{}", errs[0]);
    assert!(cp.engine_state.procs.is_empty());
}

#[test]
fn env_unknown_target_e401() {
    let mut cp = session();
    let errs = lerr(&mut cp, "kick { env a decay=4f to=nope }");
    assert_eq!(errs[0].code, Code::UnknownSetting);
    assert!(
        errs[0].suggestion.as_deref().unwrap_or("").contains("env"),
        "{}",
        errs[0]
    );
    let errs = lerr(&mut cp, "kick { env a decay=4f to=lo.cutoff }");
    assert_eq!(errs[0].code, Code::UnknownSetting);
    assert!(cp.engine_state.procs.is_empty());
}

#[test]
fn env_over_is_e402() {
    let mut cp = session();
    lok(&mut cp, "kick { env a decay=4f }");
    for line in [
        "kick.a decay=8f over 4",
        "kick { env b decay=4f in 4 }",
        "kick { env b decay=4f on 93b }",
    ] {
        assert_eq!(lerr(&mut cp, line)[0].code, Code::WrongType, "{line}");
    }
    assert_eq!(env_of(&cp, "kick", "a").shape.decay, 4);
}

#[test]
fn env_gain_depth_range() {
    let mut cp = session();
    assert_eq!(
        lerr(&mut cp, "kick { env a decay=4f depth=2 }")[0].code,
        Code::OutOfRange
    );
    // elsewhere it adds
    lok(&mut cp, "kick { env a decay=4f depth=12 to=pitch }");
    // and moving back to gain is checked against the depth it has
    assert_eq!(lerr(&mut cp, "kick.a to=gain")[0].code, Code::OutOfRange);
}

#[test]
fn env_same_restatement_noop() {
    let mut cp = session();
    lok(
        &mut cp,
        "kick { env a decay=4f sustain=0.2 to=pan depth=0.5 }",
    );
    let out = local(&mut cp, "kick.a decay=4f sustain=0.2 to=pan depth=0.5");
    assert!(out.contains("W101"), "{out}");
    // the same length written another way changes only what `state` prints
    let edit = edit_args(lok(&mut cp, "kick.a decay=0.04b"));
    assert!(edit.shape.is_none());
    // restating one setting leaves the others
    let edit = edit_args(lok(&mut cp, "kick.a sustain=0.4"));
    let shape = edit.shape.unwrap();
    assert_eq!((shape.sustain, shape.decay), (0.4, 4));
    assert!(edit.target.is_none() && edit.depth.is_none());
    let edit = edit_args(lok(&mut cp, "kick.a depth=0.25 to=gain"));
    assert_eq!(edit.depth, Some(0.25));
    assert_eq!(edit.target, Some(LfoTarget::Voice(VoiceParam::Gain)));
}

#[test]
fn remove_fx_drops_env_w102() {
    let mut cp = session();
    lok(
        &mut cp,
        "kick { filter lo and env sweep decay=4f to=lo.cutoff depth=2 and env amp decay=4f }",
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
    assert!(kick.procs.contains_key("amp"));
}

#[test]
fn env_actions_and_name_clashes() {
    let mut cp = session();
    lok(
        &mut cp,
        "kick { env a decay=4f and lfo l depth=1 to=pan and seq s 1 }",
    );
    lok(&mut cp, "pause kick.a");
    assert!(env_of(&cp, "kick", "a").paused);
    lok(&mut cp, "stop kick.a");
    assert!(env_of(&cp, "kick", "a").stopped);
    lok(&mut cp, "start kick.a");
    let e = env_of(&cp, "kick", "a");
    assert!(!e.paused && !e.stopped);
    // the names of other processes are taken
    assert_eq!(
        lerr(&mut cp, "kick { env l decay=4f }")[0].code,
        Code::KindClash
    );
    assert_eq!(
        lerr(&mut cp, "kick { env s decay=4f }")[0].code,
        Code::KindClash
    );
    assert_eq!(
        lerr(&mut cp, "kick { lfo a depth=1 to=pan }")[0].code,
        Code::KindClash
    );
    lok(&mut cp, "remove kick.a");
    assert!(
        !cp.engine_state.voices[&cp.engine_state.voice_id("kick").unwrap()]
            .procs
            .contains_key("a")
    );
}

#[test]
fn state_round_trip_env() {
    let mut cp = session();
    lok(
        &mut cp,
        "kick { filter lo cutoff=800 and env amp attack=2f decay=0.5 sustain=0.3 hold=4b release=20ms curve=lin and env sweep decay=40f sustain=0 to=lo.cutoff depth=3 }",
    );
    lok(
        &mut cp,
        "group drums { hat; env pump decay=0.25 sustain=0.5 depth=0.5 }",
    );
    lok(&mut cp, "pause kick.sweep and stop drums.pump");
    let text = cp.state(None).unwrap();
    assert!(
        text.contains("env amp attack=2f decay=0.5 sustain=0.3 hold=4b release=20ms curve=lin"),
        "{text}"
    );
    assert!(
        text.contains("env sweep decay=40f sustain=0 to=lo.cutoff depth=3"),
        "{text}"
    );
    assert!(text.contains("pause kick.sweep"), "{text}");
    assert!(text.contains("stop drums.pump"), "{text}");
    // and it reads back to the same thing
    let mut fresh = lang_processor();
    sample_rate::set(48_000);
    lok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
    assert!(env_of(&fresh, "kick", "sweep").paused);
    let es = &fresh.engine_state;
    let pump = es.groups[&es.group_names["drums"]].procs["pump"];
    assert!(es.procs[&pump].env.as_ref().unwrap().stopped);
    // one env by itself
    let one = cp.state(Some("kick.sweep")).unwrap();
    assert_eq!(
        one.lines().next().unwrap(),
        "kick { env sweep decay=40f sustain=0 to=lo.cutoff depth=3 }"
    );
}

// ---- the engine, end to end ----

// a mono track of a constant, 0.5
fn dc() -> AudioFile {
    track("dc", 16384, 20_000)
}

fn rig(extra: Vec<AudioFile>, lines: &[&str]) -> (CmdProcessor, Conductor) {
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

// the index of the largest value
fn peak(x: &[f32]) -> usize {
    let mut at = 0;
    for (i, v) in x.iter().enumerate() {
        if *v > x[at] {
            at = i;
        }
    }
    at
}

#[test]
fn env_on_seq_steps() {
    // ticks of 1000ms: the steps 1 and 3 are 96 000 frames apart
    let (_cp, mut c) = rig(
        Vec::new(),
        &[
            "voice v from \"dc\" on 1000ms",
            "v { seq s 1 3 and env amp attack=1ms decay=5ms sustain=0 }",
            "start v",
        ],
    );
    let out = render(&mut c, 1, 96_400);
    // a linear attack to 48 frames, then a fall to nothing in 240
    assert_eq!(peak(&out[..1000]), 48);
    near(out[48], 0.5, 1e-3, "first peak");
    near(out[0], 0.0, 1e-3, "starts from nothing");
    assert!(out[49] < out[48]);
    for (i, got) in out.iter().enumerate().take(16_000).skip(300) {
        near(
            *got,
            0.0,
            1e-6,
            &format!("silent after the decay, frame {i}"),
        );
    }
    // the second step fires it again
    assert_eq!(peak(&out[96_000..96_400]), 48);
    near(out[96_048], 0.5, 1e-3, "second peak");
}

#[test]
fn env_on_trig_and_melo_hits() {
    // the hits come from other voices' seqs
    let (_cp, mut c) = rig(
        vec![track("dc2", 16384, 20_000)],
        &[
            "voice v from \"dc\" on 1000ms",
            "voice w from \"dc2\" on 1000ms",
            "v { env amp attack=1ms decay=5ms sustain=0 }",
            "w { seq s 1 3 | trig v }",
            "start v and start w",
        ],
    );
    let out = render(&mut c, 1, 96_400);
    assert_eq!(peak(&out[..1000]), 48);
    // the seqs run before the env, so it sees that hit on the same frame
    assert_eq!(peak(&out[96_000..96_400]), 48);
    near(out[96_048], 0.5, 1e-3, "fired by a trig");

    let (_cp, mut c) = rig(
        vec![track("dc2", 16384, 20_000)],
        &[
            "voice v from \"dc\" on 1000ms shift=grains",
            "voice w from \"dc2\" on 1000ms",
            "v { env amp attack=1ms decay=5ms sustain=0 }",
            "w { seq s 1 3 | melo v 60 62 }",
            "start v and start w",
        ],
    );
    let out = render(&mut c, 1, 96_400);
    let first = peak(&out[..1000]);
    assert_eq!(first, 48);
    near(out[first], 0.5, 1e-3, "fired by a melo note");
    assert_eq!(peak(&out[96_000..96_400]), 48);
    near(out[96_048], 0.5, 1e-3, "and the next");
}

#[test]
fn env_sees_trig_hit_same_frame() {
    // the env is made before the seq that hits its owner: same-frame anyway
    let (_cp, mut c) = rig(
        vec![track("dc2", 16384, 20_000)],
        &[
            "voice v from \"dc\" on 1000ms",
            "voice w from \"dc2\" on 1000ms",
            "v { env amp attack=1ms decay=5ms sustain=0 }",
            "w { seq s 1 3 | trig v }",
            "start v and start w",
        ],
    );
    let out = render(&mut c, 1, 96_400);
    assert_eq!(peak(&out[96_000..96_400]), 48);
}

// a sine of `freq` Hz, as a long track
fn sine_track(name: &str, freq: f32) -> AudioFile {
    let samples: Vec<i16> = (0..30_000)
        .map(|i| (12_000.0 * (std::f32::consts::TAU * freq * i as f32 / 48_000.0).sin()) as i16)
        .collect();
    AudioFile::new(name, "wav", 48000, 1, 16, pcm(&samples))
}

#[test]
fn env_adds_to_the_rate() {
    // a ramp reads back the rate: each frame advances by `rate` samples.
    // Twice as fast at the start, falling back to normal over 100 frames.
    let ramp: Vec<i16> = (0..20_000).map(|i| i as i16).collect();
    let ramp = AudioFile::new("ramp", "wav", 48000, 1, 16, pcm(&ramp));
    let (_cp, mut c) = rig(
        vec![ramp],
        &[
            "voice v from \"ramp\" on 1000ms",
            "v { env r to=rate depth=1 decay=100f sustain=0 curve=lin }",
            "start v",
        ],
    );
    let out = render(&mut c, 1, 300);
    let step = |i: usize| (out[i + 1] - out[i]) * 32768.0;
    near(step(0), 2.0, 1e-2, "double speed at the hit");
    near(step(50), 1.5, 2e-2, "halfway down");
    near(step(150), 1.0, 1e-2, "back to normal");
}

#[test]
fn env_adds_to_pan() {
    // a mono track on two channels: pan 1 is all right, 0 is centred
    sample_rate::set(48_000);
    let (mut cp, mut c) = setup(2, vec![dc()]);
    c.apply(cp.boot());
    for line in [
        "voice v from \"dc\" on 1000ms",
        "v { env p to=pan depth=1 decay=100f sustain=0 curve=lin }",
        "start v",
    ] {
        run(&mut cp, &mut c, line);
    }
    let out = render(&mut c, 2, 200);
    near(out[0], 0.0, 1e-3, "all right at the hit: left");
    near(out[1], 0.5, 1e-3, "all right at the hit: right");
    let centre = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    near(out[2 * 150], centre, 1e-3, "centred after the decay: left");
    near(
        out[2 * 150 + 1],
        centre,
        1e-3,
        "centred after the decay: right",
    );
}

#[test]
fn group_env_fires_once_per_group_hit() {
    let (_cp, mut c) = rig(
        vec![track("dc2", 16384, 20_000)],
        &[
            "voice a from \"dc\" on 1000ms",
            "voice b from \"dc2\" on 1000ms",
            "group g { a; b; seq s 1 3 and env amp attack=1ms decay=5ms sustain=0 }",
            "start g",
        ],
    );
    let out = render(&mut c, 1, 96_400);
    // both members shaped by the one envelope: 0.5 + 0.5 at the peak
    assert_eq!(peak(&out[..1000]), 48);
    near(out[48], 1.0, 1e-3, "both members at the peak");
    near(out[24], 0.5, 1e-3, "halfway up, as one envelope");
    near(out[96_048], 1.0, 1e-3, "second step");
}

#[test]
fn group_env_ignores_member_join_and_leave() {
    let (mut cp, mut c) = rig(
        vec![track("dc2", 16384, 20_000)],
        &[
            "voice a from \"dc\" on 1000ms",
            "voice b from \"dc2\" on 1000ms",
            "group g { a; seq s 1 3 and env amp attack=1ms decay=5ms sustain=0 }",
            "start g",
            "start b",
        ],
    );
    // past the first hit and its decay
    render(&mut c, 1, 5_000);
    // b has been hit once: joining moves the sum the env watches
    run(&mut cp, &mut c, "group g { b }");
    for got in render(&mut c, 1, 2_000) {
        near(got, 0.0, 1e-6, "no hit from a join");
    }
    run(&mut cp, &mut c, "ungroup g.b");
    // b is on its own now, at 0.5; a firing env would add a's sound
    for got in render(&mut c, 1, 2_000) {
        near(got, 0.5, 1e-6, "no hit from a leave");
    }
}

#[test]
fn parked_voice_env_fires_once_at_first_step() {
    let (_cp, mut c) = rig(
        Vec::new(),
        &[
            "voice v from \"dc\" on 1000ms",
            "v { seq s 2 4 and env amp attack=1ms decay=5ms sustain=0 }",
            "start v",
        ],
    );
    let env_idle = |c: &Conductor| {
        c.procs
            .iter()
            .any(|(_, s)| matches!(&s.process, Process::Env(e) if e.is_idle()))
    };
    // the voice waits for the step at 48 000; its env doesn't fire before
    render(&mut c, 1, 100);
    assert!(env_idle(&c), "fired at start");
    render(&mut c, 1, 47_900);
    render(&mut c, 1, 10);
    assert!(!env_idle(&c), "didn't fire at the step");
}

#[test]
fn pause_and_stop_by_name_end_to_end() {
    let (mut cp, mut c) = rig(
        Vec::new(),
        &[
            "voice v from \"dc\" on 1000ms",
            "v { env amp attack=1000f depth=1 }",
            "start v",
        ],
    );
    // a quarter of the way up the attack
    render(&mut c, 1, 251);
    run(&mut cp, &mut c, "pause v.amp");
    let held = render(&mut c, 1, 400);
    for got in &held {
        near(*got, 0.5 * 0.25, 2e-3, "paused level");
    }
    // resume continues the attack
    run(&mut cp, &mut c, "resume v.amp");
    let moving = render(&mut c, 1, 100);
    assert!(moving[99] > moving[0]);
    // stop gives the base value back
    run(&mut cp, &mut c, "stop v.amp");
    for got in render(&mut c, 1, 200) {
        near(got, 0.5, 1e-6, "stopped");
    }
    // start waits for the next hit: still the base value
    run(&mut cp, &mut c, "start v.amp");
    for got in render(&mut c, 1, 200) {
        near(got, 0.0, 1e-6, "started, waiting for a hit");
    }
    // removed: the base again
    run(&mut cp, &mut c, "remove v.amp");
    for got in render(&mut c, 1, 100) {
        near(got, 0.5, 1e-6, "removed");
    }
}

#[test]
fn a_voice_with_an_env_plays_at_once() {
    // an env is not a seq: the voice must not wait for it, and it fires on
    // the start
    let (_cp, mut c) = rig(
        Vec::new(),
        &[
            "voice v from \"dc\" on 1000ms",
            "v { env amp attack=100f }",
            "start v",
        ],
    );
    let out = render(&mut c, 1, 200);
    near(out[50], 0.25, 1e-3, "rising");
    near(out[150], 0.5, 1e-3, "sustaining at full level");
}

#[test]
fn env_depth_on_gain_leaves_the_rest() {
    // depth 0.5: between envelopes the voice plays at half
    let (_cp, mut c) = rig(
        Vec::new(),
        &[
            "voice v from \"dc\" on 1000ms",
            "v { env amp attack=100f depth=0.5 }",
            "start v",
        ],
    );
    let out = render(&mut c, 1, 200);
    near(out[0], 0.25, 1e-3, "half to start");
    near(out[150], 0.5, 1e-3, "full once risen");
}

#[test]
fn env_on_a_group_effect() {
    let (_cp, mut c) = rig(
        vec![sine_track("hi", 3000.0)],
        &[
            "voice v from \"hi\" on 1000ms",
            "group g { v; filter lo type=lp cutoff=500; env sweep decay=4800f sustain=0 to=lo.cutoff depth=2 curve=lin }",
            "start g",
        ],
    );
    let out = render(&mut c, 1, 9600);
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
    // two octaves up at the hit (2 kHz), back to 500 Hz after the decay
    assert!(rms(&out[100..300]) > 5.0 * rms(&out[8000..8200]));
}
