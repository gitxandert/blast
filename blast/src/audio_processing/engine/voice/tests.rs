//! VOXa's tests: the parameter model and playback geometry, driven through
//! source text (`CmdProcessor` + `Conductor`).

use super::*;
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::lang::Code;
use crate::audio_processing::commands::{
    BatchArgs, CmdErr, CmdProcessor, Command, Parsed, ProcId, VoiceId,
};
use crate::audio_processing::engine::shift::{ColdBudget, VoiceShifters, stats};
use crate::audio_processing::engine::{Conductor, Garbage};
use crate::audio_processing::test_util::{pcm, render, render_i16, run, rx, setup};
use crate::file_parsing::decode_helpers::AudioFile;

// mono track whose sample at frame i is i
fn ramp(name: &str, frames: usize) -> AudioFile {
    let samples: Vec<i16> = (0..frames as i16).collect();
    AudioFile::new(name, "wav", 48000, 1, 16, pcm(&samples))
}

// stereo track: left is i, right is -i
fn stereo_ramp(name: &str, frames: usize) -> AudioFile {
    let samples: Vec<i16> = (0..frames as i16).flat_map(|i| [i, -i]).collect();
    AudioFile::new(name, "wav", 48000, 2, 16, pcm(&samples))
}

// one voice `v` on a ramp track "t" with `settings`, not started
fn rig(out_channels: usize, frames: usize, settings: &str) -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(out_channels, vec![ramp("t", frames)]);
    run(
        &mut cp,
        &mut c,
        &format!("voice v from \"t\" on 1000ms {settings}"),
    );
    (cp, c)
}

// the first `n` mono frames of `v` playing a `frames`-frame ramp
fn play(frames: usize, settings: &str, n: usize) -> Vec<i16> {
    let (mut cp, mut c) = rig(1, frames, settings);
    run(&mut cp, &mut c, "start v");
    render_i16(&mut c, 1, n)
}

fn state(c: &mut Conductor) -> &mut VoiceState {
    &mut c.voices.get_mut(0).unwrap().state
}

fn lang_code(cp: &mut CmdProcessor, line: &str) -> Code {
    match cp.parse(line) {
        Err(CmdErr::Lang(errs)) => errs[0].code,
        Err(e) => panic!("'{line}': a family error: {e}"),
        Ok(_) => panic!("'{line}' should have failed"),
    }
}

fn warns(cp: &mut CmdProcessor, line: &str) -> bool {
    match cp.parse(line) {
        Ok(Parsed::Local(text)) => text.contains("W101"),
        Ok(Parsed::Engine(_)) => false,
        Err(e) => panic!("'{line}' failed: {e}"),
    }
}

fn voice_state() -> VoiceState {
    VoiceState::new(10, crate::audio_processing::commands::TempoId(0), false)
}

#[test]
fn param_defaults_and_roundtrip() {
    let mut s = voice_state();
    let defaults = [1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
    for (p, want) in VoiceParam::ALL.into_iter().zip(defaults) {
        assert_eq!(s.get_param(p), want, "{}", p.name());
        assert_eq!(VoiceParam::from_name(p.name()), Some(p));
    }
    s.set_param(VoiceParam::Pan, 0.25, 0);
    assert_eq!(s.get_param(VoiceParam::Pan), 0.25);
    // clamped to the range; NaN ignored
    s.set_param(VoiceParam::Gain, 100.0, 0);
    assert_eq!(s.get_param(VoiceParam::Gain), 8.0);
    s.set_param(VoiceParam::Pitch, f32::NAN, 0);
    assert_eq!(s.get_param(VoiceParam::Pitch), 0.0);
}

#[test]
fn modulation_does_not_touch_base() {
    let (mut cp, mut c) = rig(1, 100, "gain=0.5");
    run(&mut cp, &mut c, "start v");
    state(&mut c).modulate(ModTarget::Voice(VoiceParam::Gain), ModOp::Add(0.25));
    assert_eq!(state(&mut c).get_param(VoiceParam::Gain), 0.5);
    assert_eq!(state(&mut c).effective(VoiceParam::Gain), 0.75);
    // gone after one rendered frame
    render(&mut c, 1, 1);
    assert_eq!(state(&mut c).effective(VoiceParam::Gain), 0.5);
}

#[test]
fn mul_and_add_combine() {
    let mut s = voice_state();
    s.set_param(VoiceParam::Gain, 1.0, 0);
    let g = ModTarget::Voice(VoiceParam::Gain);
    for op in [
        ModOp::Add(1.0),
        ModOp::Mul(2.0),
        ModOp::Add(0.5),
        ModOp::Mul(0.5),
    ] {
        s.modulate(g, op);
    }
    // (1 + 1.5) * 1
    assert_eq!(s.effective(VoiceParam::Gain), 2.5);
    // clamped to the range
    s.modulate(g, ModOp::Mul(100.0));
    assert_eq!(s.effective(VoiceParam::Gain), 8.0);
    // an effect's parameters belong to the chain: the state ignores them
    s.modulate(ModTarget::Fx { id: 1, param: 2 }, ModOp::Add(3.0));
    assert_eq!(s.effective(VoiceParam::Gain), 8.0);
}

#[test]
fn gain_scales_output() {
    assert_eq!(play(100, "gain=0.5", 4), [0, 0, 1, 1]);
    assert_eq!(play(100, "gain=2", 4), [0, 2, 4, 6]);
}

#[test]
fn mono_pan_is_constant_power() {
    let power = |pan: f32| {
        let (mut cp, mut c) = rig(2, 100, &format!("pan={pan}"));
        run(&mut cp, &mut c, "start v");
        render(&mut c, 2, 1);
        // frame 1 holds sample 1
        let out = render(&mut c, 2, 1);
        (out[0] * 32768.0, out[1] * 32768.0)
    };
    for pan in [-1.0, -0.5, 0.0, 0.3, 1.0] {
        let (l, r) = power(pan);
        assert!((l * l + r * r - 1.0).abs() < 1e-4, "pan {pan}: {l} {r}");
    }
    let (l, r) = power(0.0);
    assert!((l - std::f32::consts::FRAC_PI_4.cos()).abs() < 1e-4 && (l - r).abs() < 1e-4);
    // hard left and hard right
    let (l, r) = power(-1.0);
    assert!((l - 1.0).abs() < 1e-4 && r.abs() < 1e-4);
    let (l, r) = power(1.0);
    assert!(l.abs() < 1e-4 && (r - 1.0).abs() < 1e-4);
}

#[test]
fn mono_on_one_channel_ignores_pan() {
    assert_eq!(play(100, "pan=1", 3), [0, 1, 2]);
}

#[test]
fn stereo_balance_keeps_unity_at_center() {
    sample_rate::set(48000);
    let render_pan = |pan: &str| {
        let (mut cp, mut c) = setup(2, vec![stereo_ramp("s", 100)]);
        run(
            &mut cp,
            &mut c,
            &format!("voice v from \"s\" on 1000ms {pan}"),
        );
        run(&mut cp, &mut c, "start v");
        render_i16(&mut c, 2, 4)
    };
    assert_eq!(render_pan(""), [0, 0, 1, -1, 2, -2, 3, -3]);
    // pan right: the left channel falls, the right stays
    assert_eq!(render_pan("pan=1"), [0, 0, 0, -1, 0, -2, 0, -3]);
    assert_eq!(render_pan("pan=-0.5"), [0, 0, 1, 0, 2, -1, 3, -1]);
}

#[test]
fn multichannel_source_on_one_output_averages() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![stereo_ramp("s", 100)]);
    run(&mut cp, &mut c, "voice v from \"s\" on 1000ms");
    run(&mut cp, &mut c, "start v");
    // left i and right -i average to zero
    assert_eq!(render_i16(&mut c, 1, 3), [0, 0, 0]);
}

#[test]
fn region_plays_only_its_frames() {
    let out = play(100, "start=0.1 end=0.2", 14);
    assert_eq!(out, [10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 0, 0, 0, 0]);
}

#[test]
fn region_negative_rate_plays_backwards() {
    let out = play(100, "start=0.1 end=0.2 rate=-1", 12);
    assert_eq!(out, [19, 18, 17, 16, 15, 14, 13, 12, 11, 10, 0, 0]);
}

#[test]
fn negative_rate_plays_backwards() {
    assert_eq!(play(5, "rate=-1", 6), [4, 3, 2, 1, 0, 0]);
}

#[test]
fn loop_forward_wraps() {
    assert_eq!(
        play(5, "loop=forward", 12),
        [0, 1, 2, 3, 4, 0, 1, 2, 3, 4, 0, 1]
    );
}

#[test]
fn loop_points_with_intro() {
    assert_eq!(
        play(10, "loop=forward loopstart=0.5", 18),
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 5, 6, 7, 8, 9, 5, 6, 7]
    );
}

#[test]
fn loop_reverse_wraps() {
    assert_eq!(
        play(5, "loop=forward rate=-1", 12),
        [4, 3, 2, 1, 0, 4, 3, 2, 1, 0, 4, 3]
    );
    // the tail after the loop's end plays once, backwards
    assert_eq!(
        play(10, "loop=forward loopend=0.5 rate=-1", 12),
        [9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 4, 3]
    );
}

#[test]
fn pingpong_reflects() {
    assert_eq!(
        play(5, "loop=pingpong bounce=0", 14),
        [0, 1, 2, 3, 4, 3, 2, 1, 0, 1, 2, 3, 4, 3]
    );
}

#[test]
fn pingpong_fast_rate_stays_in_bounds() {
    // values are frame + 1 (none is silent), so a read outside would show
    sample_rate::set(48000);
    let samples: Vec<i16> = (1..=4).collect();
    let (mut cp, mut c) = setup(
        1,
        vec![AudioFile::new("t", "wav", 48000, 1, 16, pcm(&samples))],
    );
    run(
        &mut cp,
        &mut c,
        "voice v from \"t\" on 1000ms loop=pingpong bounce=0 rate=7",
    );
    run(&mut cp, &mut c, "start v");
    for s in render_i16(&mut c, 1, 60) {
        assert!((1..=4).contains(&s), "read outside the loop: {s}");
    }
}

#[test]
fn slices_select_the_region() {
    // slice counts from 1: the first quarter, then the third
    let first = play(100, "slices=4 slice=1", 26);
    assert_eq!(first[0], 0);
    assert_eq!(first[24], 24);
    assert_eq!(first[25], 0);
    let third = play(100, "slices=4 slice=3", 27);
    assert_eq!(third[0], 50);
    assert_eq!(third[24], 74);
    assert_eq!(third[25], 0);
}

#[test]
fn slice_mod_scans() {
    let (mut cp, mut c) = rig(1, 100, "slices=4 slice=1 loop=forward");
    run(&mut cp, &mut c, "start v");
    state(&mut c).modulate(ModTarget::Voice(VoiceParam::Slice), ModOp::Add(1.0));
    // the voice sits at frame 0, outside slice 2 (25..=49): it's silent
    // until a retrigger puts it at the slice's start
    state(&mut c).hit();
    let out = render_i16(&mut c, 1, 1);
    assert_eq!(out, [25]);
}

#[test]
fn rate_still_varispeed() {
    // rate changes speed and pitch together, without the granular engine
    assert_eq!(play(100, "rate=2", 5), [0, 2, 4, 6, 8]);
    let (_cp, mut c) = rig(1, 100, "rate=2");
    assert!(!state(&mut c).inst[0].granular);
}

#[test]
fn start_mod_out_of_region_ends_oneshot() {
    let (mut cp, mut c) = rig(1, 100, "");
    run(&mut cp, &mut c, "start v");
    assert_eq!(render_i16(&mut c, 1, 3), [0, 1, 2]);
    // a start modulation (an LFO writes one every frame) that passes the
    // playhead silences a one-shot, and the playhead stops moving
    for _ in 0..3 {
        state(&mut c).modulate(ModTarget::Voice(VoiceParam::Start), ModOp::Add(0.5));
        assert_eq!(render_i16(&mut c, 1, 1), [0]);
    }
    assert_eq!(state(&mut c).inst[0].position, 3.0);
}

#[test]
fn retrigger_bumps_trigger_count() {
    let (mut cp, mut c) = rig(1, 100, "");
    assert_eq!(state(&mut c).trigger_count(), 0);
    run(&mut cp, &mut c, "start v");
    assert_eq!(state(&mut c).trigger_count(), 1);
    render(&mut c, 1, 5);
    state(&mut c).hit();
    assert_eq!(state(&mut c).trigger_count(), 2);
    assert_eq!(state(&mut c).inst[0].position, 0.0);
    // stop rewinds without counting
    run(&mut cp, &mut c, "stop v");
    assert_eq!(state(&mut c).trigger_count(), 2);
}

#[test]
fn position_is_f64_past_2_24() {
    let mut s = voice_state();
    s.len = 40_000_000;
    s.active = true;
    s.inst[0].position = 16_777_216.5;
    s.step(None, None);
    assert_eq!(s.inst[0].position, 16_777_217.5);
}

#[test]
fn render_and_advance_leave_params_alone() {
    let (mut cp, mut c) = rig(2, 100, "gain=0.5 pan=0.2 slices=4 slice=2 loop=pingpong");
    run(&mut cp, &mut c, "start v");
    render(&mut c, 2, 50);
    let s = state(&mut c);
    assert_eq!(s.get_param(VoiceParam::Gain), 0.5);
    assert_eq!(s.get_param(VoiceParam::Slice), 1.0);
    assert_eq!(s.slices(), 4);
    assert_eq!(s.loop_mode(), LoopMode::PingPong);
}

// ---- through the language ----

#[test]
fn voice_settings_lower() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(2, vec![ramp("t", 100)]);
    let Ok(Parsed::Engine(Command::Batch(BatchArgs { cmds: batch }))) =
        cp.parse("voice v from \"t\" on 1000ms shift=spectral gain=0.5 pan=-0.3 pitch=2st loop=pingpong rate=-1")
    else {
        panic!("not one batch");
    };
    // the load, its shifters, then the settings in schema order
    let kinds: Vec<&str> = batch
        .iter()
        .map(|cmd| match cmd {
            Command::Load(_) => "Load",
            // pitch makes the voice want shifters; they follow the load
            Command::SetShifters(_) => "SetShifters",
            Command::SetParam(_) => "SetParam",
            Command::SetMode(_) => "SetMode",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "Load",
            "SetShifters",
            "SetParam",
            "SetParam",
            "SetParam",
            "SetParam",
            "SetMode",
            "SetMode"
        ]
    );
    for cmd in batch {
        c.apply(cmd);
    }
    let s = state(&mut c);
    assert_eq!(s.get_param(VoiceParam::Gain), 0.5);
    assert_eq!(s.get_param(VoiceParam::Pan), -0.3);
    assert_eq!(s.get_param(VoiceParam::Pitch), 2.0);
    assert_eq!(s.loop_mode(), LoopMode::PingPong);
    assert_eq!(s.get_param(VoiceParam::Rate), -1.0);
}

#[test]
fn restating_changes_and_applies() {
    let (mut cp, mut c) = rig(1, 100, "");
    run(&mut cp, &mut c, "v gain=0.25 loop=forward slices=2 slice=2");
    let s = state(&mut c);
    assert_eq!(s.get_param(VoiceParam::Gain), 0.25);
    assert_eq!(s.get_param(VoiceParam::Slice), 1.0);
    assert_eq!(s.slices(), 2);
    assert_eq!(s.loop_mode(), LoopMode::Forward);
}

#[test]
fn restate_same_settings_warns() {
    let (mut cp, _c) = rig(1, 100, "gain=0.5 loop=forward");
    assert!(warns(&mut cp, "v gain=0.5"));
    assert!(warns(&mut cp, "v loop=forward"));
    assert!(warns(
        &mut cp,
        "voice v from \"t\" on 1000ms gain=0.5 loop=forward"
    ));
    assert!(!warns(&mut cp, "v gain=0.6"));
}

#[test]
fn gain_db() {
    // on creation
    let (mut cp, mut c) = rig(1, 100, "gain=-6db");
    let g = state(&mut c).get_param(VoiceParam::Gain);
    assert!((g - 0.501).abs() < 1e-3, "{g}");
    // and on a restatement
    run(&mut cp, &mut c, "v gain=-20db");
    let g = state(&mut c).get_param(VoiceParam::Gain);
    assert!((g - 0.1).abs() < 1e-5, "{g}");
    // decibels belong to gain only; 40db is out of range
    assert_eq!(lang_code(&mut cp, "v pan=1db"), Code::WrongType);
    assert_eq!(lang_code(&mut cp, "v gain=40db"), Code::OutOfRange);
    // a variable holds the linear value
    run(&mut cp, &mut c, "let q = -6db");
    run(&mut cp, &mut c, "v gain=$q");
    let g = state(&mut c).get_param(VoiceParam::Gain);
    assert!((g - 0.501).abs() < 1e-3, "{g}");
}

#[test]
fn slice_counts_from_one() {
    // `slices=4 slice=1` is the first quarter, and so is a bare `slices=4`
    let first: Vec<i16> = (0..3).collect();
    assert_eq!(play(100, "slices=4 slice=1", 3), first);
    assert_eq!(play(100, "slices=4", 3), first);
    assert_eq!(play(100, "slices=4 slice=4", 1), [75]);
}

#[test]
fn slice_above_slices_errors() {
    let (mut cp, _c) = rig(1, 100, "");
    assert_eq!(lang_code(&mut cp, "v slices=4 slice=5"), Code::OutOfRange);
    assert_eq!(
        lang_code(&mut cp, "voice w from \"t\" on 1000ms slices=2 slice=3"),
        Code::OutOfRange
    );
    // against the stored slice count
    assert!(!warns(&mut cp, "v slices=4 slice=4"));
    assert_eq!(lang_code(&mut cp, "v slices=2"), Code::OutOfRange);
    // nothing changed
    assert!(warns(&mut cp, "v slices=4 slice=4"));
}

#[test]
fn loop_points_order_errors() {
    let (mut cp, _c) = rig(1, 100, "");
    assert_eq!(
        lang_code(&mut cp, "v loopstart=0.8 loopend=0.2"),
        Code::OutOfRange
    );
    assert_eq!(lang_code(&mut cp, "v start=0.5 end=0.5"), Code::OutOfRange);
    assert_eq!(
        lang_code(&mut cp, "voice w from \"t\" on 1000ms start=0.9 end=0.1"),
        Code::OutOfRange
    );
    // one side against the stored other
    assert!(!warns(&mut cp, "v end=0.5"));
    assert_eq!(lang_code(&mut cp, "v start=0.6"), Code::OutOfRange);
}

#[test]
fn change_track_swaps_samples() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![ramp("a", 10), stereo_ramp("b", 20)]);
    let mut queues = rx(&mut c);
    run(&mut cp, &mut c, "voice v from \"a\" on 1000ms");
    run(&mut cp, &mut c, "start v");
    render(&mut c, 1, 3);
    run(&mut cp, &mut c, "v from \"b\"");
    let voice = c.voices.get(0).unwrap();
    assert_eq!(voice.channels, 2);
    assert_eq!(voice.track().len(), 40);
    assert_eq!(voice.state.len, 20);
    // the old samples left through the garbage queue
    let Ok(Garbage::Track(old)) = queues.garbage.pop() else {
        panic!("the old track wasn't retired");
    };
    assert_eq!(old.len(), 10);
    assert!(queues.garbage.pop().is_err());
    // the playhead kept its place in the (whole-track) region
    assert!(state(&mut c).inst[0].position >= 6.0);
    // restating the same track changes nothing
    assert!(warns(&mut cp, "v from \"b\""));
}

#[test]
fn change_track_when_the_queue_is_full_parks_or_leaks() {
    use crate::audio_processing::engine::arena::Limits;
    use crate::audio_processing::test_util::setup_with;
    sample_rate::set(48000);
    let limits = Limits {
        garbage: 1,
        parked: 1,
        ..Limits::default()
    };
    let (mut cp, mut c) = setup_with(limits, 1, vec![ramp("a", 10), ramp("b", 20)]);
    let mut queues = rx(&mut c);
    run(&mut cp, &mut c, "voice v from \"a\" on 1000ms");
    run(&mut cp, &mut c, "v from \"b\"");
    run(&mut cp, &mut c, "v from \"a\"");
    // one in the queue, one parked: nothing was freed here
    assert_eq!(c.parked.len(), 1);
    assert_eq!(queues.drain_garbage(), 1);
}

#[test]
fn change_track_of_a_parked_voice_stays_parked() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![ramp("a", 10), ramp("b", 20)]);
    run(&mut cp, &mut c, "voice v from \"a\" on 1000ms");
    run(&mut cp, &mut c, "v { seq s 2 }");
    run(&mut cp, &mut c, "start v");
    run(&mut cp, &mut c, "v from \"b\"");
    assert_eq!(render_i16(&mut c, 1, 5), [0, 0, 0, 0, 0]);
}

#[test]
fn change_period_without_seqs() {
    let (mut cp, mut c) = rig(1, 100, "in 8");
    run(&mut cp, &mut c, "v in 4");
    assert_eq!(cp.engine_state.voices[&VoiceId(0)].period, 4);
    assert!(warns(&mut cp, "v in 4"));
}

#[test]
fn change_period_with_seqs_moves_the_followers() {
    let (mut cp, mut c) = rig(1, 100, "in 8");
    run(&mut cp, &mut c, "v { seq s 1 }");
    run(&mut cp, &mut c, "v in 4");
    assert_eq!(cp.engine_state.voices[&VoiceId(0)].period, 4);
    // the seq follows the voice's period and took the new one
    assert_eq!(cp.engine_state.procs[&ProcId(0)].seq.period, 4);
    assert_eq!(
        c.procs.get(0).map(|p| match &p.process {
            crate::audio_processing::processes::Process::Seq(s) => s.state.period,
            crate::audio_processing::processes::Process::Lfo(_)
            | crate::audio_processing::processes::Process::Env(_) => 0,
        }),
        Some(4)
    );
    // a step the new cycle can't hold is refused, and nothing changed
    let (mut cp, mut c) = rig(1, 100, "in 8");
    run(&mut cp, &mut c, "v { seq s 6 }");
    let err = match cp.parse("v in 4") {
        Err(CmdErr::Lang(errs)) => errs,
        _ => panic!("should have failed"),
    };
    assert_eq!(err[0].code, Code::StepOutsideCycle);
    assert_eq!(cp.engine_state.voices[&VoiceId(0)].period, 8);
    // a seq with a period of its own doesn't follow the voice's
    let (mut cp, mut c) = rig(1, 100, "in 8");
    run(&mut cp, &mut c, "v { seq s 1 in 2 }");
    run(&mut cp, &mut c, "v in 4");
    assert_eq!(cp.engine_state.voices[&VoiceId(0)].period, 4);
}

#[test]
fn state_round_trips_voice_settings() {
    let (mut cp, mut c) = rig(
        1,
        100,
        "gain=-6db pan=-0.3 rate=-0.5 pitch=3st start=0.1 end=0.9 loop=pingpong loopstart=0.2 loopend=0.8 slices=8 slice=3",
    );
    run(&mut cp, &mut c, "voice w from \"t\" on 1000ms gain=0.5");
    let text = cp.state(None).unwrap();
    let line = text.lines().find(|l| l.starts_with("voice w")).unwrap();
    assert_eq!(line, "voice w from \"t\" on 1000ms gain=0.5");
    let line = text.lines().find(|l| l.starts_with("voice v")).unwrap();
    assert!(
        line.contains(" pan=-0.3 rate=-0.5 pitch=3st start=0.1 end=0.9 loop=pingpong loopstart=0.2 loopend=0.8 slices=8 slice=3"),
        "{line}"
    );
    // gain prints in decibels as written, before pan
    assert!(line.contains(" gain=-6db pan="), "{line}");

    // feeding it back changes nothing
    match cp.parse(&text) {
        Ok(Parsed::Local(t)) => assert!(t.lines().all(|l| l.contains("W101")), "{t}"),
        _ => panic!("the state should only warn"),
    }
    // and it recreates the session from nothing
    sample_rate::set(48000);
    let (mut fresh, _c) = setup(1, vec![ramp("t", 100)]);
    assert!(fresh.parse(&text).is_ok());
    assert_eq!(fresh.state(None).unwrap(), text);
}

#[test]
fn seq_retrigger_respects_region() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![ramp("t", 5000)]);
    for line in [
        "voice v from \"t\" on 1000f start=0.5",
        "v { seq s 1 in 2 }",
        "start v",
    ] {
        run(&mut cp, &mut c, line);
    }
    let out = render_i16(&mut c, 1, 4);
    // the hit restarts at start=, not at frame 0
    assert_eq!(out, [2500, 2501, 2502, 2503]);
}

#[test]
fn rate_still_works_through_set_param() {
    assert_eq!(play(100, "rate=2", 4), [0, 2, 4, 6]);
    assert_eq!(play(5, "rate=-1", 6), [4, 3, 2, 1, 0, 0]);
}

// ---- VOXb: `over` glides, Hermite reads ----

// a voice on a long ramp track and a 1000-frame tick, playing
fn gliding_rig(settings: &str) -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![ramp("t", 30000)]);
    run(
        &mut cp,
        &mut c,
        &format!("voice v from \"t\" on 1000f {settings}"),
    );
    run(&mut cp, &mut c, "start v");
    (cp, c)
}

fn param(c: &mut Conductor, p: VoiceParam) -> f32 {
    state(c).get_param(p)
}

// the frames of every SetParam a line lowers to
fn glide_frames(cp: &mut CmdProcessor, line: &str) -> Vec<u32> {
    let cmd = match cp.parse(line) {
        Ok(Parsed::Engine(c)) => c,
        other => panic!("'{line}': {:?}", other.map(|_| ())),
    };
    let cmds = match cmd {
        Command::Batch(b) => b.cmds,
        c => vec![c],
    };
    cmds.into_iter()
        .filter_map(|c| match c {
            Command::SetParam(a) => Some(a.frames),
            _ => None,
        })
        .collect()
}

#[test]
fn over_glides_rate() {
    let (mut cp, mut c) = gliding_rig("");
    run(&mut cp, &mut c, "v r=2 over 1b");
    assert_eq!(param(&mut c, VoiceParam::Rate), 1.0);
    render(&mut c, 1, 500);
    assert!((param(&mut c, VoiceParam::Rate) - 1.5).abs() < 1e-4);
    render(&mut c, 1, 500);
    assert_eq!(param(&mut c, VoiceParam::Rate), 2.0);
    render(&mut c, 1, 10);
    assert_eq!(param(&mut c, VoiceParam::Rate), 2.0);
}

#[test]
fn over_several_in_one_statement() {
    let (mut cp, mut c) = gliding_rig("");
    run(&mut cp, &mut c, "v pan=1 over 200f gain=0.5 over 100f");
    render(&mut c, 1, 100);
    assert_eq!(param(&mut c, VoiceParam::Gain), 0.5);
    assert!((param(&mut c, VoiceParam::Pan) - 0.5).abs() < 1e-4);
    render(&mut c, 1, 100);
    assert_eq!(param(&mut c, VoiceParam::Pan), 1.0);
}

#[test]
fn over_from_current_value() {
    let (mut cp, mut c) = gliding_rig("");
    run(&mut cp, &mut c, "v r=3 over 1000f");
    render(&mut c, 1, 500);
    let mid = param(&mut c, VoiceParam::Rate);
    assert!((mid - 2.0).abs() < 1e-4);
    run(&mut cp, &mut c, "v r=1 over 1000f");
    // no jump when the second glide starts
    assert_eq!(param(&mut c, VoiceParam::Rate), mid);
    render(&mut c, 1, 500);
    assert!((param(&mut c, VoiceParam::Rate) - 1.5).abs() < 1e-3);
    render(&mut c, 1, 500);
    assert_eq!(param(&mut c, VoiceParam::Rate), 1.0);
}

#[test]
fn over_bare_is_periods() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![ramp("t", 100)]);
    run(&mut cp, &mut c, "voice v from \"t\" on 1000f in 8");
    // 8 ticks of 1000 frames
    assert_eq!(glide_frames(&mut cp, "v r=2 over 1"), [8000]);
    assert_eq!(glide_frames(&mut cp, "v r=3 over 0.5"), [4000]);
    // the default period is 4 ticks
    run(&mut cp, &mut c, "voice w from \"t\" on 1000f");
    assert_eq!(glide_frames(&mut cp, "w r=2 over 1"), [4000]);
}

#[test]
fn over_time_units() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![ramp("t", 100)]);
    run(&mut cp, &mut c, "voice v from \"t\" on 1000f");
    assert_eq!(glide_frames(&mut cp, "v r=2 over 250ms"), [12000]);
    assert_eq!(glide_frames(&mut cp, "v r=3 over 480f"), [480]);
    assert_eq!(glide_frames(&mut cp, "v r=4 over 2b"), [2000]);
    assert_eq!(glide_frames(&mut cp, "v r=5 over 0.5s"), [24000]);
}

#[test]
fn over_on_loop_errors() {
    let (mut cp, _c) = rig(1, 100, "");
    let msg = match cp.parse("v loop=forward over 1") {
        Err(CmdErr::Lang(errs)) => {
            assert_eq!(errs[0].code, Code::WrongType);
            errs[0].message.clone()
        }
        _ => panic!("should fail"),
    };
    assert!(msg.contains("changes at once"), "{msg}");
    assert_eq!(lang_code(&mut cp, "v slices=4 over 1"), Code::WrongType);
    // `over` needs a setting in front of it
    assert_eq!(lang_code(&mut cp, "v over 1"), Code::Unexpected);
    assert_eq!(
        lang_code(&mut cp, "v gain=0.5 over 1 over 2"),
        Code::Unexpected
    );
    match cp.parse("v over 1") {
        Err(CmdErr::Lang(errs)) => {
            assert_eq!(
                errs[0].suggestion.as_deref(),
                Some("'over' follows a setting")
            )
        }
        _ => panic!("should fail"),
    }
    // and a duration after it
    assert_eq!(lang_code(&mut cp, "v gain=0.5 over"), Code::Unexpected);
}

#[test]
fn instant_without_over() {
    let (mut cp, mut c) = gliding_rig("");
    assert_eq!(glide_frames(&mut cp, "v r=2"), [0]);
    run(&mut cp, &mut c, "v r=3");
    assert_eq!(param(&mut c, VoiceParam::Rate), 3.0);
    // a voice that isn't running can't glide, so it jumps
    let (mut cp, mut c) = rig(1, 100, "");
    run(&mut cp, &mut c, "v gain=0.25 over 1b");
    assert_eq!(param(&mut c, VoiceParam::Gain), 0.25);
}

#[test]
fn reverse_setting_is_gone() {
    let (mut cp, _c) = rig(1, 100, "");
    match cp.parse("v reverse=yes") {
        Err(CmdErr::Lang(errs)) => {
            assert_eq!(errs[0].code, Code::UnknownSetting);
            assert!(errs[0].suggestion.as_deref().unwrap().contains("rate"));
        }
        _ => panic!("should fail"),
    }
}

#[test]
fn fadein_is_gone() {
    let (mut cp, _c) = rig(1, 100, "");
    assert_eq!(lang_code(&mut cp, "v fadein=1b"), Code::UnknownSetting);
    assert_eq!(lang_code(&mut cp, "v fadeout=1b"), Code::UnknownSetting);
    assert_eq!(VoiceParam::COUNT, 11);
}

#[test]
fn state_prints_target_not_glide() {
    let (mut cp, mut c) = gliding_rig("");
    run(&mut cp, &mut c, "v gain=0.5 over 1000f");
    render(&mut c, 1, 100);
    let text = cp.state(None).unwrap();
    let line = text.lines().find(|l| l.starts_with("voice v")).unwrap();
    assert!(line.contains(" gain=0.5"), "{line}");
    assert!(!line.contains("over"), "{line}");
    // restating the target, glide or not, changes nothing
    assert!(warns(&mut cp, "v gain=0.5 over 2b"));
}

// ---- Hermite ----

// mono sine of `amp`, `period` frames per cycle
fn sine(i: usize, period: f64, amp: f64) -> i16 {
    (amp * (std::f64::consts::TAU * i as f64 / period).sin()).round() as i16
}

fn sine_track(frames: usize, period: f64, amp: f64) -> AudioFile {
    let samples: Vec<i16> = (0..frames).map(|i| sine(i, period, amp)).collect();
    AudioFile::new("t", "wav", 48000, 1, 16, pcm(&samples))
}

fn play_track(af: AudioFile, settings: &str, n: usize) -> Vec<i16> {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![af]);
    run(
        &mut cp,
        &mut c,
        &format!("voice v from \"t\" on 1000ms {settings}"),
    );
    run(&mut cp, &mut c, "start v");
    render_i16(&mut c, 1, n)
}

#[test]
fn rate_one_is_bit_exact() {
    let want: Vec<i16> = (0..300).map(|i| sine(i, 48.0, 20000.0)).collect();
    let out = play_track(sine_track(300, 48.0, 20000.0), "", 300);
    assert_eq!(out, want);
}

#[test]
fn hermite_reproduces_linear_ramp() {
    // a ramp stays a ramp at every position, edges included
    for (frames, rate) in [(100usize, 0.37f64), (20, 0.5)] {
        let out = play(frames, &format!("rate={rate}"), 3 * frames);
        for (n, &v) in out.iter().enumerate() {
            let pos = rate * n as f64;
            if pos <= (frames - 1) as f64 {
                assert!((f64::from(v) - pos).abs() <= 1.0, "frame {n}: {v} vs {pos}");
            }
        }
    }
}

#[test]
fn hermite_beats_linear_on_sine() {
    let (frames, period, amp, rate) = (400usize, 48.0f64, 20000.0f64, 0.37f64);
    let at = |x: f64| amp * (std::f64::consts::TAU * x / period).sin();
    let out = play_track(sine_track(frames, period, amp), "rate=0.37", 1000);
    let (mut herm, mut lin, mut used) = (0.0f64, 0.0f64, 0);
    for (n, &v) in out.iter().enumerate() {
        let pos = rate * n as f64;
        if !(5.0..frames as f64 - 5.0).contains(&pos) {
            continue;
        }
        let i = pos.floor() as usize;
        let t = pos - i as f64;
        let linear =
            f64::from(sine(i, period, amp)) * (1.0 - t) + f64::from(sine(i + 1, period, amp)) * t;
        herm += (f64::from(v) - at(pos)).powi(2);
        lin += (linear - at(pos)).powi(2);
        used += 1;
    }
    let (herm, lin) = ((herm / used as f64).sqrt(), (lin / used as f64).sqrt());
    assert!(herm * 10.0 < lin, "hermite {herm} vs linear {lin}");
}

#[test]
fn hermite_wraps_at_loop_end() {
    // exactly one period: the seam is invisible
    let (period, amp) = (48.0f64, 20000.0f64);
    let out = play_track(sine_track(48, period, amp), "loop=forward rate=0.37", 400);
    for (n, &v) in out.iter().enumerate() {
        let want = amp * (std::f64::consts::TAU * 0.37 * n as f64 / period).sin();
        assert!(
            (f64::from(v) - want).abs() < 100.0,
            "frame {n}: {v} vs {want}"
        );
    }
}

// ---- granular playback (VOXc) ----

// the first `n` mono frames of `v` on `af`, as f32
fn play_f32(af: AudioFile, settings: &str, n: usize) -> Vec<f32> {
    let (mut cp, mut c) = granular_rig(af, settings);
    run(&mut cp, &mut c, "start v");
    render(&mut c, 1, n)
}

// pinned to `shift=grains`: these tests are about the WSOLA grains (REQ-134
// made them the default again)
fn granular_rig(af: AudioFile, settings: &str) -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![af]);
    run(
        &mut cp,
        &mut c,
        &format!("voice v from \"t\" on 1000f {settings} shift=grains"),
    );
    (cp, c)
}

fn dc_track(frames: usize) -> AudioFile {
    AudioFile::new("t", "wav", 48000, 1, 16, pcm(&vec![16384; frames]))
}

// upward zero crossings in `out[from..to]`
fn crossings(out: &[f32], from: usize, to: usize) -> usize {
    out[from..to]
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count()
}

#[test]
fn engine_off_is_bit_exact() {
    let af = || sine_track(3000, 48.0, 20000.0);
    for rate in ["", "rate=0.37", "rate=-1.5 loop=forward"] {
        let plain = play_f32(af(), rate, 2500);
        let off = play_f32(af(), &format!("{rate} pitch=1 stretch=1"), 2500);
        assert_eq!(plain, off, "{rate}");
    }
    // and rate 1 still reproduces the source exactly
    let out = play_track(af(), "pitch=1 stretch=1", 300);
    let want: Vec<i16> = (0..300).map(|i| sine(i, 48.0, 20000.0)).collect();
    assert_eq!(out, want);
}

#[test]
fn stretch_half_doubles_duration() {
    // a constant source lasts as long as the read pointer does
    let out = play_f32(dc_track(1000), "stretch=0.5", 2500);
    let bad: Vec<_> = (0..1990).filter(|&i| (out[i] - 0.5).abs() > 1e-3).collect();
    assert!(
        bad.is_empty(),
        "{:?} {:?}",
        &bad[..bad.len().min(5)],
        bad.last()
    );
    assert!(out[2000..].iter().all(|v| *v == 0.0));
    // impulses 100 frames apart keep their spacing (the pitch)
    let samples: Vec<i16> = (0..1000)
        .map(|i| if i % 100 == 0 { 16000 } else { 0 })
        .collect();
    let af = AudioFile::new("t", "wav", 48000, 1, 16, pcm(&samples));
    let out = play_f32(af, "stretch=0.5", 900);
    let peaks: Vec<usize> = (0..900).filter(|&i| out[i].abs() > 0.01).collect();
    assert!(peaks.len() >= 8, "{peaks:?}");
    assert!(peaks.windows(2).all(|w| w[1] - w[0] == 100), "{peaks:?}");
}

#[test]
fn pitch_octave_keeps_duration() {
    // a 1 kHz sine measures 2 kHz: 10000 frames hold 208 cycles
    let out = play_f32(sine_track(30000, 48.0, 20000.0), "pitch=12st", 14000);
    let n = crossings(&out, 2000, 12000);
    assert!((400..=432).contains(&n), "{n} crossings");
    // the voice ends where it would at pitch 0
    let out = play_f32(dc_track(5000), "pitch=12st", 5500);
    assert!(out[..5000].iter().all(|v| (v - 0.5).abs() < 1e-3));
    assert!(out[5000..].iter().all(|v| *v == 0.0));
}

#[test]
fn pitch_and_stretch_combine() {
    // an octave up at half speed: 2 kHz, for twice as long
    let out = play_f32(
        sine_track(10000, 48.0, 20000.0),
        "pitch=12st stretch=0.5",
        22000,
    );
    let n = crossings(&out, 2000, 12000);
    assert!((400..=432).contains(&n), "{n} crossings");
    let out = play_f32(dc_track(4000), "pitch=12st stretch=0.5", 9000);
    assert!(out[..7990].iter().all(|v| (v - 0.5).abs() < 1e-3));
    assert!(out[8000..].iter().all(|v| *v == 0.0));
    // a fifth down, twice as fast
    let out = play_f32(dc_track(4000), "pitch=-7st stretch=2", 3000);
    assert!(out[..1990].iter().all(|v| (v - 0.5).abs() < 1e-3));
    assert!(out[2000..].iter().all(|v| *v == 0.0));
}

#[test]
fn windows_sum_to_one() {
    for settings in [
        "pitch=7st",
        "stretch=1.3",
        "pitch=-5st stretch=0.7 loop=forward",
    ] {
        let out = play_f32(dc_track(20000), settings, 6000);
        for (n, v) in out.iter().enumerate() {
            assert!((v - 0.5).abs() < 1e-3, "{settings}, frame {n}: {v}");
        }
    }
}

#[test]
fn grains_respect_loop_and_region() {
    // region 100..=299 of a ramp: every output stays inside it, and the
    // voice ends with the read pointer
    let (mut cp, mut c) = rig(1, 400, "start=0.25 end=0.75 pitch=12st shift=grains");
    run(&mut cp, &mut c, "start v");
    let out = render_i16(&mut c, 1, 400);
    assert!(
        out[..200].iter().all(|v| (99..=300).contains(v)),
        "{:?}",
        &out[..200]
    );
    assert!(out[200..].iter().all(|v| *v == 0));
    // a loop wraps the grains too: they never leave it, and never stop
    let (mut cp, mut c) = rig(1, 100, "loop=forward pitch=7st stretch=0.8 shift=grains");
    run(&mut cp, &mut c, "start v");
    let out = render_i16(&mut c, 1, 3000);
    assert!(out.iter().all(|v| (-30..=130).contains(v)));
    assert!(out[2900..].iter().any(|v| *v > 10));
    let (mut cp, mut c) = rig(1, 100, "loop=pingpong pitch=-5st stretch=1.5 shift=grains");
    run(&mut cp, &mut c, "start v");
    let out = render_i16(&mut c, 1, 3000);
    assert!(out.iter().all(|v| (-30..=130).contains(v)));
    assert!(out[2900..].iter().any(|v| *v > 10));
}

#[test]
fn engine_switches_without_a_jump() {
    let (mut cp, mut c) = granular_rig(sine_track(40000, 480.0, 20000.0), "");
    run(&mut cp, &mut c, "start v");
    let mut out = render(&mut c, 1, 500);
    assert!(!state(&mut c).inst[0].granular);
    run(&mut cp, &mut c, "v stretch=2");
    out.extend(render(&mut c, 1, 100));
    assert!(state(&mut c).inst[0].granular);
    run(&mut cp, &mut c, "v stretch=1");
    out.extend(render(&mut c, 1, 6000));
    // back on the plain head once the crossfade is over
    assert!(!state(&mut c).inst[0].granular);
    // a slow sine (480 frames a cycle, amplitude 0.6): no step bigger than
    // its own slope allows at twice the speed
    let worst = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst < 0.6 * std::f32::consts::TAU / 480.0 * 2.5,
        "step {worst}"
    );
}

#[test]
fn retrigger_starts_open() {
    let (mut cp, mut c) = granular_rig(dc_track(20000), "pitch=3st");
    run(&mut cp, &mut c, "start v");
    render(&mut c, 1, 700);
    state(&mut c).hit();
    let out = render(&mut c, 1, 3);
    assert!(out.iter().all(|v| (v - 0.5).abs() < 1e-3), "{out:?}");
}

#[test]
fn stretch_glides() {
    let (mut cp, mut c) = gliding_rig("shift=grains");
    run(&mut cp, &mut c, "v stretch=0.5 over 4800f");
    render(&mut c, 1, 2400);
    let mid = param(&mut c, VoiceParam::Stretch);
    assert!((mid - 0.75).abs() < 0.01, "{mid}");
    assert!(state(&mut c).inst[0].granular);
    render(&mut c, 1, 2500);
    assert_eq!(param(&mut c, VoiceParam::Stretch), 0.5);
    // it glides with the others
    assert_eq!(glide_frames(&mut cp, "v x=2 over 100f"), [100]);
}

#[test]
fn stretch_setting() {
    let (mut cp, mut c) = rig(1, 100, "stretch=2");
    assert_eq!(param(&mut c, VoiceParam::Stretch), 2.0);
    run(&mut cp, &mut c, "v x=0.5");
    assert_eq!(param(&mut c, VoiceParam::Stretch), 0.5);
    assert_eq!(lang_code(&mut cp, "v stretch=0"), Code::OutOfRange);
    assert_eq!(lang_code(&mut cp, "v stretch=17"), Code::OutOfRange);
    let text = cp.state(None).unwrap();
    assert!(text.contains(" stretch=0.5"), "{text}");
}

#[test]
fn pitch_units() {
    let measure = |settings: &str| {
        let out = play_f32(sine_track(30000, 48.0, 20000.0), settings, 8000);
        (crossings(&out, 1000, 7000), out)
    };
    let (n, base) = measure("pitch=12st");
    for form in ["pitch=12st", "pitch=1200c", "pitch=2x", "pitch=(4/2)x"] {
        let (m, out) = measure(form);
        assert_eq!(m, n, "{form}");
        assert_eq!(out, base, "{form}");
    }
    // and the stored value is in semitones
    let (_cp, mut c) = rig(1, 100, "pitch=7st");
    assert_eq!(param(&mut c, VoiceParam::Pitch), 7.0);
    for (form, want) in [
        ("7st", 7.0),
        ("-7st", -7.0),
        ("50c", 0.5),
        ("-150c", -1.5),
        ("1.5x", 7.019_55),
        ("0.5x", -12.0),
        ("(3/2)x", 7.019_55),
    ] {
        let (_cp, mut c) = rig(1, 100, &format!("pitch={form}"));
        let got = param(&mut c, VoiceParam::Pitch);
        assert!((got - want).abs() < 1e-4, "{form}: {got}");
    }
}

#[test]
fn pitch_ratio_must_be_positive() {
    let (mut cp, _c) = rig(1, 100, "");
    assert_eq!(lang_code(&mut cp, "v pitch=0x"), Code::OutOfRange);
    assert_eq!(lang_code(&mut cp, "v pitch=(0-1)x"), Code::OutOfRange);
    // only pitch takes a pitch value
    assert_eq!(lang_code(&mut cp, "v rate=2x"), Code::WrongType);
    assert_eq!(lang_code(&mut cp, "v gain=7st"), Code::WrongType);
    // 100 octaves is out of range
    assert_eq!(lang_code(&mut cp, "v pitch=100x"), Code::OutOfRange);
}

#[test]
fn pitch_prints_semitones() {
    let (cp, _c) = rig(1, 100, "pitch=50c");
    let text = cp.state(None).unwrap();
    assert!(text.contains(" pitch=0.5st"), "{text}");
    let (cp, _c) = rig(1, 100, "pitch=2x");
    let text = cp.state(None).unwrap();
    assert!(text.contains(" pitch=2"), "{text}");
    assert!(!text.contains(" pitch=2st"), "{text}");
    // a clean ratio is printed as one, anything else as semitones
    for (given, want) in [
        ("0.5", " pitch=0.5"),
        ("1.25", " pitch=1.25"),
        ("7st", " pitch=7st"),
        ("-12st", " pitch=0.5"),
        ("3c", " pitch=0.03st"),
    ] {
        let (cp, _c) = rig(1, 100, &format!("pitch={given}"));
        let text = cp.state(None).unwrap();
        assert!(text.contains(want), "{given}: {text}");
    }
}

#[test]
fn bare_pitch_is_a_ratio() {
    // 1 is normal; 0.5 an octave down; 2 an octave up
    for (given, want) in [("1", 0.0), ("0.5", -12.0), ("2", 12.0), ("1.5", 7.019_55)] {
        let (_cp, mut c) = rig(1, 100, &format!("pitch={given}"));
        let got = param(&mut c, VoiceParam::Pitch);
        assert!((got - want).abs() < 1e-4, "{given}: {got}");
    }
    let (mut cp, _c) = rig(1, 100, "");
    assert_eq!(lang_code(&mut cp, "v pitch=0"), Code::OutOfRange);
    assert_eq!(lang_code(&mut cp, "v pitch=(0-1)"), Code::OutOfRange);
    // ct is gone
    assert_eq!(lang_code(&mut cp, "v pitch=50ct"), Code::BadNumber);
}

// ---- VOXd: choirs ----

// the level of a DC track (16384 / 32768) one instance plays
const DC: f32 = 0.5;

// a started voice on a long DC track
fn choir_rig(settings: &str) -> (CmdProcessor, Conductor) {
    let (mut cp, mut c) = granular_rig(dc_track(20000), settings);
    run(&mut cp, &mut c, "start v");
    (cp, c)
}

// the voice's repr in the mirror
fn repr(cp: &CmdProcessor) -> &crate::audio_processing::commands::VoiceRepr {
    cp.engine_state.voices.values().next().unwrap()
}

#[test]
fn choir_one_is_today() {
    let plain = play(100, "", 150);
    assert_eq!(play(100, "choir=1", 150), plain);
    assert_eq!(play(100, "choir=1 choirmode=sim", 150), plain);
    assert_eq!(play(100, "choir=1 intonation=1", 150), plain);
}

#[test]
fn choir_seq_overlaps() {
    let (_cp, mut c) = choir_rig("choir=2");
    assert!((render(&mut c, 1, 10)[9] - DC).abs() < 1e-4);
    // the second hit takes the second instance; the first rings on
    state(&mut c).hit();
    let out = render(&mut c, 1, 10);
    assert!((out[9] - 2.0 * DC).abs() < 1e-4, "{out:?}");
    // the third hit comes back to the first and cuts it
    let before1 = state(&mut c).inst[1].position;
    assert!(state(&mut c).inst[0].position > 0.0);
    state(&mut c).hit();
    assert_eq!(state(&mut c).inst[0].position, 0.0);
    assert_eq!(state(&mut c).inst[1].position, before1);
    assert!((render(&mut c, 1, 10)[9] - 2.0 * DC).abs() < 1e-4);
    // a one-voice choir never overlaps
    let (_cp1, mut c1) = choir_rig("");
    render(&mut c1, 1, 10);
    state(&mut c1).hit();
    assert!((render(&mut c1, 1, 10)[9] - DC).abs() < 1e-4);
}

#[test]
fn choir_seq_cuts_the_oldest() {
    // on a ramp, positions tell which instance was restarted
    let (mut cp, mut c) = rig(1, 1000, "choir=3");
    run(&mut cp, &mut c, "start v");
    for _ in 0..2 {
        render(&mut c, 1, 100);
        state(&mut c).hit();
    }
    render(&mut c, 1, 100);
    let pos: Vec<f64> = (0..3).map(|i| state(&mut c).inst[i].position).collect();
    assert_eq!(pos, [300.0, 200.0, 100.0]);
    state(&mut c).hit();
    assert_eq!(state(&mut c).inst[0].position, 0.0);
}

#[test]
fn choir_sim_starts_all() {
    let (_cp, mut c) = choir_rig("choir=3 choirmode=sim");
    let out = render(&mut c, 1, 10);
    assert!(out.iter().all(|v| (v - 3.0 * DC).abs() < 1e-4), "{out:?}");
    // a hit restarts them together
    render(&mut c, 1, 50);
    state(&mut c).hit();
    assert!((0..3).all(|i| state(&mut c).inst[i].position == 0.0));
}

#[test]
fn intonation_one_exact() {
    let (_cp, mut c) = choir_rig("choir=3 choirmode=sim");
    for _ in 0..5 {
        state(&mut c).hit();
        render(&mut c, 1, 7);
    }
    assert_eq!(state(&mut c).draws, 0);
    for inst in &state(&mut c).inst[..3] {
        assert_eq!(
            (inst.rate_mul, inst.pan_add, inst.gain_mul, inst.delay_left),
            (1.0, 0.0, 1.0, 0)
        );
    }
}

#[test]
fn intonation_spreads() {
    let offsets = |seed: u64| {
        let (_cp, mut c) = choir_rig("choir=8 choirmode=sim intonation=0");
        state(&mut c).seed(seed);
        state(&mut c).hit();
        state(&mut c).inst[..8]
            .iter()
            .map(|i| (i.rate_mul, i.pan_add, i.gain_mul, i.delay_left))
            .collect::<Vec<_>>()
    };
    let a = offsets(7);
    for &(rate, pan, gain, delay) in &a {
        assert!((0.97..=1.03).contains(&rate), "rate {rate}");
        assert!((-0.5..=0.5).contains(&pan), "pan {pan}");
        assert!((0.5..=1.0).contains(&gain), "gain {gain}");
        assert!(delay <= 960, "delay {delay}");
    }
    // they differ, and the same seed gives the same choir
    assert!(a.windows(2).any(|w| w[0] != w[1]));
    assert_eq!(a, offsets(7));
    assert_ne!(a, offsets(8));
    // half the intonation, half the spread
    let (_cp, mut c) = choir_rig("choir=8 choirmode=sim intonation=0.5");
    for _ in 0..20 {
        state(&mut c).hit();
        for i in &state(&mut c).inst[..8] {
            assert!((0.985..=1.015).contains(&i.rate_mul));
            assert!(i.pan_add.abs() <= 0.25 && i.delay_left <= 480);
        }
    }
}

#[test]
fn intonation_delays_the_onset() {
    let (_cp, mut c) = choir_rig("choir=2 choirmode=sim intonation=0");
    state(&mut c).seed(3);
    state(&mut c).hit();
    let left = state(&mut c).inst[0].delay_left;
    // silent and still until the delay has run out
    assert!(left > 0);
    render(&mut c, 1, 1);
    assert_eq!(state(&mut c).inst[0].position, 0.0);
    render(&mut c, 1, left as usize + 5);
    assert!(state(&mut c).inst[0].position > 0.0);
}

#[test]
fn choirmode_words() {
    let (mut cp, _c) = rig(1, 100, "");
    for (w, want) in [
        ("seq", ChoirMode::Sequential),
        ("sequential", ChoirMode::Sequential),
        ("sim", ChoirMode::Simultaneous),
        ("simultaneous", ChoirMode::Simultaneous),
    ] {
        let line = format!("v choirmode={w}");
        // a restated mode is a no-op, so only check the mirror
        run_ok(&mut cp, &line);
        assert_eq!(repr(&cp).choir_mode, want, "{w}");
    }
    assert_eq!(lang_code(&mut cp, "v choirmode=round"), Code::WrongType);
    assert_eq!(lang_code(&mut cp, "v choirmode=3"), Code::WrongType);
    assert_eq!(lang_code(&mut cp, "v choir=9"), Code::OutOfRange);
    assert_eq!(lang_code(&mut cp, "v choir=0"), Code::OutOfRange);
    assert_eq!(lang_code(&mut cp, "v choir=2.5"), Code::WrongType);
    assert_eq!(lang_code(&mut cp, "v intonation=1.5"), Code::OutOfRange);
    assert_eq!(
        lang_code(&mut cp, "v intonation=0.5 over 1"),
        Code::WrongType
    );
    // the short forms
    run_ok(&mut cp, "v h=4 m=sim i=0.25");
    let r = repr(&cp);
    assert_eq!((r.choir, r.choir_mode), (4, ChoirMode::Simultaneous));
    assert_eq!(r.params[VoiceParam::Intonation.index()], 0.25);
}

fn run_ok(cp: &mut CmdProcessor, line: &str) {
    if let Err(e) = cp.parse(line) {
        panic!("'{line}': {e}");
    }
}

#[test]
fn lower_choir_cuts_extras() {
    let (mut cp, mut c) = choir_rig("choir=3 choirmode=sim");
    let out = render(&mut c, 1, 10);
    assert!((out[9] - 3.0 * DC).abs() < 1e-4);
    run(&mut cp, &mut c, "v choir=1");
    assert!(!state(&mut c).inst[1].live && !state(&mut c).inst[2].live);
    let out = render(&mut c, 1, 10);
    assert!((out[0] - DC).abs() < 1e-4, "{out:?}");
    // raising it again does not wake them up
    run(&mut cp, &mut c, "v choir=3");
    assert!((render(&mut c, 1, 1)[0] - DC).abs() < 1e-4);
}

#[test]
fn state_round_trips_choir() {
    let (cp, _c) = rig(1, 100, "choir=4 choirmode=sim intonation=0.5");
    let text = cp.state(None).unwrap();
    let line = text.lines().find(|l| l.starts_with("voice v")).unwrap();
    assert!(
        line.ends_with(" choir=4 choirmode=sim intonation=0.5"),
        "{line}"
    );
    // the defaults print nothing
    let (cp2, _c2) = rig(1, 100, "choir=1 choirmode=seq intonation=1");
    let plain = cp2.state(None).unwrap();
    assert!(
        !plain.contains("choir") && !plain.contains("intonation"),
        "{plain}"
    );
    // and the printed text builds the same voice again
    let (mut cp3, mut c3) = setup(1, vec![ramp("t", 100)]);
    run(&mut cp3, &mut c3, line);
    let r = repr(&cp3);
    assert_eq!((r.choir, r.choir_mode), (4, ChoirMode::Simultaneous));
    assert_eq!(r.params[VoiceParam::Intonation.index()], 0.5);
}

// ---- pitch behaviour on percussive material (W6DEMO) ----
//
// A bare `pitch=` number is semitones, and any non-zero pitch runs the
// granular engine (two 40 ms Hann grains, 20 ms hop, each new grain starting
// at the playhead). These tests pin down what that does to clicks and tones.

// mono track of `frames` zeros with a 1 ms (48 frame) burst at `at`
fn click_track(frames: usize, at: usize) -> AudioFile {
    let mut samples = vec![0i16; frames];
    samples[at..at + 48].fill(16000);
    AudioFile::new("t", "wav", 48000, 1, 16, pcm(&samples))
}

// distinct transients in `out`: clusters of |x| > 0.05 apart by more than
// 2 ms of quiet, as their first frames
fn transients(out: &[f32]) -> Vec<usize> {
    let mut found: Vec<usize> = vec![];
    let mut last = None::<usize>;
    for (i, v) in out.iter().enumerate() {
        if v.abs() > 0.05 {
            if last.is_none_or(|l| i - l > 96) {
                found.push(i);
            }
            last = Some(i);
        }
    }
    found
}

// plays a click at `at`, hit again every 6000 frames; the transients of each
// of `hits` windows and the first window's energy
fn click_hits(at: usize, settings: &str, hits: usize) -> (Vec<Vec<usize>>, f64) {
    let (mut cp, mut c) = granular_rig(click_track(4800, at), settings);
    run(&mut cp, &mut c, "start v");
    let (mut per, mut energy) = (vec![], 0.0);
    for h in 0..hits {
        if h > 0 {
            state(&mut c).hit();
        }
        let out = render(&mut c, 1, 6000);
        if h == 0 {
            energy = out.iter().map(|v| f64::from(*v).powi(2)).sum();
        }
        per.push(transients(&out));
    }
    (per, energy)
}

// a rim-like hit: a click on top of a 1.7 kHz ring that decays over ~60 ms
fn rim_track() -> AudioFile {
    let samples: Vec<i16> = (0..6000)
        .map(|i| {
            let t = i as f64;
            let ring = (-t / 3000.0).exp() * (std::f64::consts::TAU * t / 28.3).sin();
            let click = if i < 48 { 0.5 } else { 0.0 };
            (14000.0 * (ring + click)) as i16
        })
        .collect();
    AudioFile::new("t", "wav", 48000, 1, 16, pcm(&samples))
}

// the source's frequency ratio, from the interpolated upward zero crossings
// of a steady sine of period 47.3 frames
fn tone_ratio(out: &[f32]) -> f64 {
    let ups: Vec<f64> = out
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f64 + f64::from(-w[0] / (w[1] - w[0])))
        .collect();
    47.3 * (ups.len() - 1) as f64 / (ups[ups.len() - 1] - ups[0])
}

// the level spread, in dB, of 473-frame (10 ms, 10 cycles) block RMS
fn ripple_db(out: &[f32]) -> f64 {
    let rms: Vec<f64> = out
        .as_chunks::<473>()
        .0
        .iter()
        .map(|b| (b.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / 473.0).sqrt())
        .collect();
    let lo = rms.iter().cloned().fold(f64::MAX, f64::min);
    let hi = rms.iter().cloned().fold(0.0, f64::max);
    20.0 * (hi / lo).log10()
}

fn steady_sine(settings: &str) -> Vec<f32> {
    let out = play_f32(sine_track(60000, 47.3, 20000.0), settings, 22000);
    out[2000..20000 - 1].to_vec()
}

#[test]
fn pitch_shifts_a_tone_with_the_same_duration() {
    let want = |semi: f64| 2f64.powf(semi / 12.0);
    for (s, semi) in [
        ("pitch=0.5st", 0.5),
        ("pitch=12st", 12.0),
        ("pitch=3st", 3.0),
        ("pitch=7st", 7.0),
        ("pitch=-5st", -5.0),
    ] {
        let r = tone_ratio(&steady_sine(s));
        assert!(
            (r / want(semi) - 1.0).abs() < 0.003,
            "{s}: {r} vs {}",
            want(semi)
        );
    }
    // stretch changes the length, not the pitch
    for s in ["stretch=0.5", "stretch=2"] {
        let r = tone_ratio(&steady_sine(s));
        assert!((r - 1.0).abs() < 0.003, "{s}: {r}");
    }
    // the voice still ends where it would at pitch 0
    let out = play_f32(sine_track(6000, 47.3, 20000.0), "pitch=0.5st", 9000);
    assert!(out[6000..].iter().all(|v| *v == 0.0));
    assert!(out[5900..6000].iter().any(|v| v.abs() > 0.1));
}

#[test]
fn small_pitch_leaves_the_clicks_alone() {
    // one transient per hit, at the same place on every hit
    for at in [0, 500, 1000, 2000, 3000] {
        let (plain, e0) = click_hits(at, "", 3);
        assert!(plain.iter().all(|t| t == &[at]), "{plain:?}");
        let (per, e) = click_hits(at, "pitch=0.5st", 3);
        assert!(
            per.iter().all(|t| t.len() == 1 && t == &per[0]),
            "{at}: {per:?}"
        );
        // a touch early (the head runs 2.9% slower), and nearly as loud
        let early = at - per[0][0];
        assert!(early <= at / 30 + 1, "{at}: {per:?}");
        assert!((10.0 * (e / e0).log10()).abs() < 0.5, "{at}: {e} vs {e0}");
    }
}

#[test]
fn big_pitch_moves_clicks_across_the_grains() {
    // a click at the top of the sample is always a single click per hit
    for s in ["pitch=12st", "pitch=-12st", "pitch=7st"] {
        let (per, _) = click_hits(0, s, 3);
        assert!(per.iter().all(|t| t == &[0]), "{s}: {per:?}");
    }
    // later in the sample it is read by whichever grains cover it: an
    // octave up gives it several times (3 copies 10 ms apart at 3000), an
    // octave down all but drops it when it falls at a grain's edge
    let (up, _) = click_hits(3000, "pitch=12st", 2);
    assert!(up[0].len() >= 2, "{up:?}");
    let (plain, e0) = click_hits(1000, "", 1);
    assert_eq!(plain[0], [1000]);
    let (_, e) = click_hits(1000, "pitch=-12st", 1);
    assert!(10.0 * (e / e0).log10() < -15.0, "{e} vs {e0}");
}

#[test]
fn shifted_and_stretched_tones_hold_their_level() {
    // grains are lined up where they overlap, so a steady sine has no
    // 20 ms dips: its blocks stay within 1.5 dB
    let plain = ripple_db(&steady_sine(""));
    assert!(plain < 0.1, "{plain}");
    for s in [
        "pitch=0.5st",
        "pitch=3st",
        "pitch=7st",
        "pitch=-5st",
        "stretch=0.5",
        "stretch=2",
    ] {
        let r = ripple_db(&steady_sine(s));
        assert!(r < 1.5, "{s}: {r} dB");
    }
}

#[test]
fn shifted_ring_keeps_its_level() {
    // the rim-like hit, per 240 frames against the plain playback, after the
    // click (the first block)
    let blocks = |o: &[f32]| -> Vec<f64> {
        o.chunks(240)
            .map(|b| b.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>())
            .collect()
    };
    let p = blocks(&play_f32(rim_track(), "", 4800));
    for s in ["pitch=0.5st", "pitch=3st", "pitch=7st", "pitch=-5st"] {
        let o = blocks(&play_f32(rim_track(), s, 4800));
        for (n, (a, b)) in o.iter().zip(&p).enumerate().skip(1).take(14) {
            let db = 10.0 * (a / b).log10();
            assert!(db.abs() < 3.0, "{s} block {n}: {db} dB");
        }
    }
}

#[test]
fn pitch_back_to_zero_is_the_plain_voice() {
    let af = || sine_track(30000, 48.0, 20000.0);
    let (mut cp, mut c) = granular_rig(af(), "");
    let (mut cp2, mut c2) = granular_rig(af(), "");
    run(&mut cp, &mut c, "start v");
    run(&mut cp2, &mut c2, "start v");
    render(&mut c, 1, 100);
    run(&mut cp, &mut c, "v pitch=0.5st");
    render(&mut c, 1, 3000);
    assert!(state(&mut c).inst[0].granular);
    run(&mut cp, &mut c, "v pitch=1");
    render(&mut c, 1, 3000);
    assert!(!state(&mut c).inst[0].granular);
    // after a hit it renders exactly what a never-pitched voice does
    state(&mut c).hit();
    state(&mut c2).hit();
    assert_eq!(render(&mut c, 1, 2000), render(&mut c2, 1, 2000));
}

#[test]
fn granular_one_shot_ends_and_holds_little() {
    // grains running ahead of the head hold the region's last frame, for
    // up to a grain's lead (7.5 ms here at an octave up); then silence
    let n = 6000;
    for (s, longest) in [
        ("pitch=0.5st", 80),
        ("pitch=12st", 400),
        ("pitch=-12st", 80),
    ] {
        let out = play_f32(sine_track(n, 48.0, 20000.0), s, n + 3000);
        let (best, _) = out[..n].windows(2).fold((0usize, 0usize), |(b, r), w| {
            let r = if (w[1] - w[0]).abs() < 1e-7 { r + 1 } else { 0 };
            (b.max(r), r)
        });
        assert!(best <= longest, "{s}: held {best} frames");
        assert!(out[n..].iter().all(|v| *v == 0.0), "{s}");
    }
}

#[test]
fn retrigger_in_the_granular_engine_replays_the_hit() {
    // hits while the grains are mid-flight, and mid-exit, restart cleanly
    let (mut cp, mut c) = granular_rig(click_track(4800, 0), "pitch=0.5st");
    run(&mut cp, &mut c, "start v");
    let first = render(&mut c, 1, 1500);
    for gap in [1, 7, 700, 959, 961, 1500] {
        state(&mut c).hit();
        let again = render(&mut c, 1, 1500);
        assert_eq!(first, again, "gap {gap}");
        render(&mut c, 1, gap);
    }
    run(&mut cp, &mut c, "v pitch=1");
    render(&mut c, 1, 10);
    state(&mut c).hit();
    let plain = render(&mut c, 1, 100);
    assert_eq!(transients(&plain), [0]);
}

// ---- the spectral shifter (PSHa) ----

// a mono saw of `hz`, `frames` long, peaking at 0.5
fn saw_track(frames: usize, hz: f64) -> AudioFile {
    let samples: Vec<i16> = (0..frames)
        .map(|i| {
            let phase = (i as f64 * hz / 48000.0).fract();
            ((phase * 2.0 - 1.0) * 16000.0) as i16
        })
        .collect();
    AudioFile::new("t", "wav", 48000, 1, 16, pcm(&samples))
}

// the pitch of `x` in Hz, from the peak of its normalised autocorrelation
// between `lo` and `hi` Hz, refined by a parabola through the peak
fn autocorr_hz(x: &[f32], lo: f64, hi: f64) -> f64 {
    let corr = |lag: usize| -> f64 {
        let (mut sum, mut a, mut b) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..x.len() - lag {
            let (p, q) = (f64::from(x[i]), f64::from(x[i + lag]));
            sum += p * q;
            a += p * p;
            b += q * q;
        }
        sum / (a * b).sqrt().max(1e-12)
    };
    let (min_lag, max_lag) = ((48000.0 / hi) as usize, (48000.0 / lo) as usize + 1);
    let best = (min_lag..=max_lag)
        .max_by(|&p, &q| corr(p).total_cmp(&corr(q)))
        .unwrap();
    let (l, m, r) = (corr(best - 1), corr(best), corr(best + 1));
    let shift = 0.5 * (l - r) / (l - 2.0 * m + r);
    48000.0 / (best as f64 + shift)
}

// the cents from `want` to `got`
fn cents(got: f64, want: f64) -> f64 {
    1200.0 * (got / want).log2()
}

// a rig like `granular_rig`, with the spectral shifter (`shift=spectral`)
fn spectral_rig(af: AudioFile, settings: &str) -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![af]);
    let shift = if settings.contains("shift=") {
        ""
    } else {
        "shift=spectral"
    };
    run(
        &mut cp,
        &mut c,
        &format!("voice v from \"t\" on 1000f {shift} {settings}"),
    );
    (cp, c)
}

#[test]
fn spectral_pitch_up_7st() {
    let (mut cp, mut c) = spectral_rig(saw_track(96000, 220.0), "pitch=7st");
    run(&mut cp, &mut c, "start v");
    assert!(state(&mut c).inst[0].spectral);
    let out = render(&mut c, 1, 30000);
    let want = 220.0 * 2f64.powf(7.0 / 12.0);
    let got = autocorr_hz(&out[10000..18000], 250.0, 450.0);
    assert!(
        cents(got, want).abs() < 5.0,
        "{got} Hz vs {want} Hz ({} cents)",
        cents(got, want)
    );
}

#[test]
fn spectral_stretch_half_doubles_duration() {
    // a 1 kHz sine of 4000 frames at half speed lasts 8000, at the same pitch
    let (mut cp, mut c) = spectral_rig(sine_track(4000, 48.0, 20000.0), "stretch=0.5");
    run(&mut cp, &mut c, "start v");
    let out = render(&mut c, 1, 9000);
    assert!(out[8000..].iter().all(|v| *v == 0.0));
    let rms =
        |x: &[f32]| (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
    assert!(rms(&out[3000..6000]) > 0.3, "{}", rms(&out[3000..6000]));
    // the pitch is unchanged: 4000 frames hold 83.3 cycles
    let n = crossings(&out, 2000, 6000);
    assert!((81..=86).contains(&n), "{n} crossings");
    // and the same source at stretch 1 plays the plain head for 4000
    let (mut cp, mut c) = spectral_rig(sine_track(4000, 48.0, 20000.0), "");
    run(&mut cp, &mut c, "start v");
    let plain = render(&mut c, 1, 5000);
    assert!(plain[4000..].iter().all(|v| *v == 0.0));
}

#[test]
fn shift_grains_matches_old_engine() {
    let af = || sine_track(20000, 123.0, 18000.0);
    // a voice that never had shifters
    let (mut cp, mut c) = spectral_rig(af(), "shift=grains pitch=2st stretch=0.9");
    assert!(c.voices.get(0).unwrap().shifters.is_none());
    // and one that was built with them, then switched to the grains
    let (mut cp2, mut c2) = spectral_rig(af(), "pitch=2st stretch=0.9");
    assert!(c2.voices.get(0).unwrap().shifters.is_some());
    run(&mut cp2, &mut c2, "v shift=grains");
    for (cp, c) in [(&mut cp, &mut c), (&mut cp2, &mut c2)] {
        run(cp, c, "start v");
    }
    assert_eq!(render(&mut c, 1, 8000), render(&mut c2, 1, 8000));
    assert!(state(&mut c2).inst[0].granular);
}

// the SetShifters commands in `cmd`, in order
fn shifter_shapes(cmd: &Command) -> Vec<(VoiceId, usize, usize)> {
    match cmd {
        Command::SetShifters(a) => vec![(a.voice, a.shifters.channels(), a.shifters.instances())],
        Command::Batch(b) => b.cmds.iter().flat_map(shifter_shapes).collect(),
        _ => vec![],
    }
}

fn parsed(cp: &mut CmdProcessor, line: &str) -> Command {
    match cp.parse(line) {
        Ok(Parsed::Engine(cmd)) => cmd,
        Ok(Parsed::Local(_)) => Command::Batch(BatchArgs { cmds: vec![] }),
        Err(e) => panic!("'{line}' failed: {e}"),
    }
}

#[test]
fn no_shifters_until_wanted() {
    sample_rate::set(48000);
    let (mut cp, _c) = setup(1, vec![ramp("t", 100)]);
    for line in [
        "voice v from \"t\" on 1000ms shift=spectral",
        "v gain=0.5 rate=2 loop=forward",
        "v pitch=1 stretch=1",
        // grains asked for: none are built
        "v shift=grains pitch=3st",
        "v shift=spectral pitch=1",
    ] {
        assert!(shifter_shapes(&parsed(&mut cp, line)).is_empty(), "{line}");
    }
    // pitch=2st sends one, once
    let cmd = parsed(&mut cp, "v pitch=2st");
    assert_eq!(shifter_shapes(&cmd), [(VoiceId(0), 1, 1)]);
    // it comes before the pitch that wants it
    let Command::Batch(b) = cmd else {
        panic!("not a batch");
    };
    assert!(matches!(b.cmds[0], Command::SetShifters(_)));
    assert!(shifter_shapes(&parsed(&mut cp, "v pitch=5st")).is_empty());
    // and a voice that stops wanting them keeps them
    assert!(shifter_shapes(&parsed(&mut cp, "v pitch=1")).is_empty());
    assert_eq!(
        cp.engine_state.voices[&VoiceId(0)].shifters_built,
        Some((1, 1, 4320))
    );
    // a voice made with pitch gets them right after its load
    let cmd = parsed(
        &mut cp,
        "voice w from \"t\" on 1000ms shift=spectral stretch=2",
    );
    let Command::Batch(b) = cmd else {
        panic!("not a batch");
    };
    assert!(matches!(
        b.cmds.as_slice(),
        [
            Command::Load(_),
            Command::SetShifters(_),
            Command::SetParam(_),
            Command::SetMode(_)
        ]
    ));
}

#[test]
fn shifters_follow_choir_and_channels() {
    sample_rate::set(48000);
    let (mut cp, _c) = setup(2, vec![ramp("t", 100), stereo_ramp("s", 100)]);
    let v = VoiceId(0);
    assert_eq!(
        shifter_shapes(&parsed(
            &mut cp,
            "voice v from \"t\" on 1000ms shift=spectral pitch=2st"
        )),
        [(v, 1, 1)]
    );
    assert_eq!(shifter_shapes(&parsed(&mut cp, "v choir=4")), [(v, 1, 4)]);
    assert_eq!(
        shifter_shapes(&parsed(&mut cp, "v from \"s\"")),
        [(v, 2, 4)]
    );
    // fewer instances is a new shape too; the same shape is not sent again
    assert_eq!(shifter_shapes(&parsed(&mut cp, "v choir=2")), [(v, 2, 2)]);
    assert!(shifter_shapes(&parsed(&mut cp, "v choir=2 gain=0.5")).is_empty());
}

fn shifter_windows(cmd: &Command) -> Vec<usize> {
    match cmd {
        Command::SetShifters(a) => vec![a.shifters.window()],
        Command::Batch(b) => b.cmds.iter().flat_map(shifter_windows).collect(),
        _ => vec![],
    }
}

#[test]
fn window_sets_the_shifter_window() {
    sample_rate::set(48000);
    let (mut cp, _c) = setup(1, vec![ramp("t", 100)]);
    // the default is 90 ms
    let cmd = parsed(
        &mut cp,
        "voice d from \"t\" on 1000ms shift=spectral pitch=2st",
    );
    assert_eq!(shifter_windows(&cmd), [4320]);
    // `w` is the short form
    let cmd = parsed(
        &mut cp,
        "voice v from \"t\" on 1000ms shift=spectral pitch=2st window=60ms",
    );
    assert_eq!(shifter_windows(&cmd), [2880]);
    let cmd = parsed(
        &mut cp,
        "voice u from \"t\" on 1000ms shift=spectral pitch=2st w=60ms",
    );
    assert_eq!(shifter_windows(&cmd), [2880]);
    // frames work too
    let cmd = parsed(
        &mut cp,
        "voice f from \"t\" on 1000ms shift=spectral pitch=2st window=1000f",
    );
    assert_eq!(shifter_windows(&cmd), [1000]);
}

#[test]
fn changing_the_window_rebuilds_the_shifters() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![ramp("t", 100)]);
    let mut queues = rx(&mut c);
    run(
        &mut cp,
        &mut c,
        "voice v from \"t\" on 1000ms shift=spectral pitch=2st",
    );
    while queues.garbage.pop().is_ok() {}
    // the same window sends nothing
    assert!(shifter_windows(&parsed(&mut cp, "v window=90ms")).is_empty());
    let cmd = parsed(&mut cp, "v window=60ms");
    assert_eq!(shifter_windows(&cmd), [2880]);
    c.apply(cmd);
    let Ok(Garbage::Shifters(old)) = queues.garbage.pop() else {
        panic!("the old shifters weren't retired");
    };
    assert_eq!(old.window(), 4320);
    assert!(queues.garbage.pop().is_err());
    // the window alone rebuilds, with the other settings as they were
    assert_eq!(
        cp.engine_state.voices[&VoiceId(0)].shifters_built,
        Some((1, 1, 2880))
    );
}

#[test]
fn window_builds_nothing_for_grains() {
    sample_rate::set(48000);
    let (mut cp, _c) = setup(1, vec![ramp("t", 100)]);
    for line in [
        "voice v from \"t\" on 1000ms shift=grains pitch=3st window=60ms",
        "v window=120ms",
    ] {
        assert!(shifter_windows(&parsed(&mut cp, line)).is_empty(), "{line}");
    }
    // back to spectral: the shifters come with the window set meanwhile
    let cmd = parsed(&mut cp, "v shift=spectral");
    assert_eq!(shifter_windows(&cmd), [5760]);
}

#[test]
fn melo_receiver_wants_shifters() {
    sample_rate::set(48000);
    let (mut cp, _c) = setup(1, vec![ramp("t", 100), ramp("u", 100)]);
    for line in [
        "voice a from \"t\" on 1000ms shift=spectral",
        "voice b from \"u\" on 1000ms shift=spectral",
    ] {
        assert!(shifter_shapes(&parsed(&mut cp, line)).is_empty());
    }
    let cmd = parsed(&mut cp, "a { seq s 1 | melo b 60 62 }");
    assert_eq!(shifter_shapes(&cmd), [(VoiceId(1), 1, 1)]);
}

#[test]
fn group_lfo_on_pitch_wants_shifters() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![ramp("t", 100), ramp("u", 100)]);
    c.apply(cp.boot());
    for line in [
        "voice a from \"t\" on 1000ms shift=spectral",
        "voice b from \"u\" on 1000ms shift=spectral",
        "voice c from \"u\" on 1000ms shift=spectral",
    ] {
        assert!(shifter_shapes(&parsed(&mut cp, line)).is_empty());
    }
    let cmd = parsed(
        &mut cp,
        "group g { a; b; lfo vib depth=1st to=pitch freq=5hz }",
    );
    let mut shapes = shifter_shapes(&cmd);
    shapes.sort_by_key(|s| s.0.0);
    // the members, and not the voice outside
    assert_eq!(shapes, [(VoiceId(0), 1, 1), (VoiceId(1), 1, 1)]);
    // an lfo on something else wants none
    assert!(shifter_shapes(&parsed(&mut cp, "c { lfo pan depth=0.5 to=pan }")).is_empty());
}

#[test]
fn live_buffer_uses_grains() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![ramp("src", 64)]);
    c.apply(cp.boot());
    for line in [
        "buffer live length=4000f live=yes",
        "buffer once length=4000f",
        "voice a from live on 1000f shift=spectral pitch=3st",
        "voice b from once on 1000f shift=spectral pitch=3st",
    ] {
        run(&mut cp, &mut c, line);
    }
    // a live buffer grows while it is read: the grains play it
    assert_eq!(c.voices.get(0).unwrap().state.spectral_avail, 0);
    assert_eq!(c.voices.get(1).unwrap().state.spectral_avail, 1);
}

#[test]
fn reverse_and_pingpong_spectral_are_finite() {
    for settings in [
        "rate=-1 pitch=3st",
        "rate=-1 pitch=3st loop=forward",
        "loop=pingpong pitch=-5st stretch=1.5",
        "loop=pingpong rate=-1 stretch=0.6",
        "loop=forward start=0.2 end=0.7 pitch=7st",
        "rate=0 pitch=3st",
        "rate=-4 pitch=12st stretch=2",
        "rate=0.02 pitch=-12st",
    ] {
        let (mut cp, mut c) = spectral_rig(saw_track(24000, 220.0), settings);
        run(&mut cp, &mut c, "start v");
        let out = render(&mut c, 1, 20000);
        assert!(out.iter().all(|v| v.is_finite()), "{settings}");
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        // rate 0 holds one frame, which is a fixed value: only the others
        // have to be audible
        if !settings.starts_with("rate=0 ") {
            assert!(peak > 0.05, "{settings}: silent ({peak})");
        }
        assert!(peak < 4.0, "{settings}: {peak}");
    }
}

#[test]
fn retrigger_onset_on_time() {
    // a kick-like burst at the top of the track, a seq step every 2 s at
    // +7 st; the onset after each step lands within 0.5 ms (24 frames)
    let samples: Vec<i16> = (0..20000)
        .map(|i| {
            let t = i as f64;
            (20000.0 * (-t / 2000.0).exp() * (std::f64::consts::TAU * t / 60.0).sin()) as i16
        })
        .collect();
    let af = AudioFile::new("t", "wav", 48000, 1, 16, pcm(&samples));
    let (mut cp, mut c) = spectral_rig(af, "pitch=7st");
    run(&mut cp, &mut c, "v { seq s 1 3 }");
    run(&mut cp, &mut c, "start v");
    let out = render(&mut c, 1, 96000 * 2 + 4000);
    for step in [0usize, 96000, 192000] {
        let win = &out[step..step + 4000];
        let peak = win.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.1, "step {step}: no sound");
        let onset = win.iter().position(|v| v.abs() > 0.1 * peak).unwrap();
        assert!(onset <= 24, "step {step}: onset {onset} frames late");
    }
}

// ---- hit snapshot and cold-seek budget (PSHb) ----

// `n` frames in callbacks of `chunk` frames
fn render_chunks(c: &mut Conductor, n: usize, chunk: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n / chunk {
        out.extend(render(c, 1, chunk));
    }
    out
}

// forgets the voice's snapshot, so its next hit seeks afresh
fn clear_snapshot(c: &mut Conductor) {
    for (_, v) in c.voices.iter_mut() {
        if let Some(s) = v.shifters.as_deref_mut() {
            s.invalidate();
        }
    }
}

#[test]
fn snapshot_hit_bit_identical() {
    // the same seq (a step every 1000 frames) and pitch; one voice has its
    // snapshot cleared before every callback, so each of its hits is a
    // fresh `output_seek`
    let build = || {
        let (mut cp, mut c) = spectral_rig(saw_track(20000, 220.0), "pitch=7st");
        run(&mut cp, &mut c, "v { seq s 1 2 3 4 }");
        run(&mut cp, &mut c, "start v");
        (cp, c)
    };
    stats::reset();
    let (_cp_a, mut a) = build();
    let warm = render_chunks(&mut a, 96000, 128);
    let (cold_a, copies_a, _) = stats::get();
    stats::reset();
    let (_cp_b, mut b) = build();
    let mut fresh = Vec::new();
    for _ in 0..96000 / 128 {
        clear_snapshot(&mut b);
        fresh.extend(render(&mut b, 1, 128));
    }
    let (cold_b, copies_b, _) = stats::get();
    // 96 hits: one seek then copies, against a seek for every hit
    assert_eq!((cold_a, copies_a), (1, 95));
    assert_eq!((cold_b, copies_b), (96, 0));
    assert!(warm.iter().any(|v| v.abs() > 0.1));
    for (n, (x, y)) in warm.iter().zip(&fresh).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "frame {n}");
    }
}

#[test]
fn choir_hit_one_cold_seek() {
    // eight instances with the same offsets (intonation 1) share a key
    let (mut cp, mut c) = spectral_rig(
        saw_track(20000, 220.0),
        "pitch=7st choir=8 choirmode=sim intonation=1",
    );
    stats::reset();
    run(&mut cp, &mut c, "start v");
    render(&mut c, 1, 128);
    assert_eq!(stats::get(), (1, 7, 0));
    let v = &c.voices.get(0).unwrap().state;
    assert!(v.inst[..8].iter().all(|i| i.spectral && !i.granular));
    // the next hit finds the snapshot: eight copies and no seek
    stats::reset();
    run(&mut cp, &mut c, "start v");
    render(&mut c, 1, 128);
    assert_eq!(stats::get(), (0, 8, 0));
}

// a stereo saw track named "t"
fn stereo_saw(frames: usize) -> AudioFile {
    let samples: Vec<i16> = (0..frames)
        .flat_map(|i| {
            let phase = (i as f64 * 220.0 / 48000.0).fract();
            let l = ((phase * 2.0 - 1.0) * 16000.0) as i16;
            [l, -l / 2]
        })
        .collect();
    AudioFile::new("t", "wav", 48000, 2, 16, pcm(&samples))
}

// `channels` channels of the stereo saw's pattern, for the cost benches
fn multi_saw(frames: usize, channels: usize) -> AudioFile {
    let samples: Vec<i16> = (0..frames)
        .flat_map(|i| {
            let phase = (i as f64 * 220.0 / 48000.0).fract();
            let l = ((phase * 2.0 - 1.0) * 16000.0) as i16;
            (0..channels).map(move |c| if c % 2 == 0 { l } else { -l / 2 })
        })
        .collect();
    AudioFile::new("t", "wav", 48000, channels as u32, 16, pcm(&samples))
}

#[test]
fn budget_falls_back_to_grains() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(2, vec![stereo_saw(20000)]);
    for k in 0..6 {
        run(
            &mut cp,
            &mut c,
            &format!(
                "voice v{k} from \"t\" on 1000f shift=spectral pitch={}st",
                k + 1
            ),
        );
    }
    let kinds = |c: &Conductor| -> Vec<bool> {
        c.voices
            .iter()
            .map(|(_, v)| v.state.inst[0].spectral)
            .collect()
    };
    let hit_all = |cp: &mut CmdProcessor, c: &mut Conductor| {
        for k in 0..6 {
            run(cp, c, &format!("start v{k}"));
        }
        stats::reset();
        render(c, 2, 128);
    };
    // 6 different pitches in one 128-frame callback: 4 units, 4 a stereo
    // seek, so one is spectral and five play on the grains
    hit_all(&mut cp, &mut c);
    assert_eq!(stats::get(), (1, 0, 5));
    assert_eq!(kinds(&c).iter().filter(|s| **s).count(), 1);
    let grains = c
        .voices
        .iter()
        .filter(|(_, v)| v.state.inst[0].granular)
        .count();
    assert_eq!(grains, 5);
    // each step warms one more, in the voices' order (a refused hit builds
    // no snapshot); all six are spectral from the sixth step on
    for (warm, cold, fell) in [(1, 1, 4), (2, 1, 3), (3, 1, 2), (4, 1, 1), (5, 1, 0)] {
        hit_all(&mut cp, &mut c);
        assert_eq!(stats::get(), (cold, warm, fell));
    }
    assert!(kinds(&c).iter().all(|s| *s));
    hit_all(&mut cp, &mut c);
    assert_eq!(stats::get(), (0, 6, 0));
    assert!(kinds(&c).iter().all(|s| *s));
}

#[test]
fn budget_scales_with_callback() {
    let units = |frames| ColdBudget::for_callback(frames).units();
    assert_eq!([units(64), units(128), units(512)], [2, 4, 16]);
    // never fewer than the minimum, however short the callback
    assert_eq!(units(1), 2);
    // a seek that costs more than the whole budget still starts, once
    let mut b = ColdBudget::for_callback(128);
    assert!(b.take(10));
    assert!(!b.take(3));
    // a stereo seek fills a 128-frame budget: one, not two
    let mut b = ColdBudget::for_callback(128);
    assert_eq!([b.take(4), b.take(4)], [true, false]);
    // a larger callback buys more
    let mut b = ColdBudget::for_callback(512);
    assert_eq!(
        [b.take(4), b.take(4), b.take(4), b.take(4), b.take(4)],
        [true, true, true, true, false]
    );
}

// a cold seek costs its units in proportion to the window: 2 + channels at
// 90 ms
#[test]
fn cold_cost_scales_with_window() {
    let cost = |ch, ms: usize| ColdBudget::cost(ch, ms * 48, 48000.0);
    // (60, 90, 180 ms) for 1, 2 and 8 channels
    assert_eq!([cost(1, 60), cost(1, 90), cost(1, 180)], [2, 3, 6]);
    assert_eq!([cost(2, 60), cost(2, 90), cost(2, 180)], [3, 4, 8]);
    assert_eq!([cost(8, 60), cost(8, 90), cost(8, 180)], [7, 10, 20]);
    // never free, however short the window
    assert_eq!(ColdBudget::cost(1, 960, 48000.0), 1);
    // the shifters carry the cost for their own window
    let v = VoiceShifters::new(2, 1, 48000.0, 60 * 48);
    assert_eq!((v.window(), v.cold_cost), (2880, 3));
}

#[test]
fn buffer_flip_invalidates_snapshot() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![saw_track(3000, 220.0)]);
    let mut queues = rx(&mut c);
    let feed = |cp: &mut CmdProcessor, q: &mut crate::audio_processing::engine::EngineRx| {
        for e in q.events() {
            if let crate::audio_processing::engine::Event::BufferEnded { slot, take, .. } = e {
                cp.engine_state.note_buffer_ended(slot, take);
            }
        }
    };
    for line in [
        "voice src from \"t\" on 1000ms shift=spectral",
        "buffer rec length=3000f",
        "start src",
        "start rec",
    ] {
        run(&mut cp, &mut c, line);
    }
    render(&mut c, 1, 3100);
    feed(&mut cp, &mut queues);
    run(
        &mut cp,
        &mut c,
        "voice a from rec on 1000f shift=spectral pitch=3st",
    );
    assert_eq!(c.voices.get(1).unwrap().state.spectral_avail, 1);
    let hit = |cp: &mut CmdProcessor, c: &mut Conductor| {
        run(cp, c, "start a");
        stats::reset();
        render(c, 1, 128);
        stats::get()
    };
    assert_eq!(hit(&mut cp, &mut c), (1, 0, 0));
    assert_eq!(hit(&mut cp, &mut c), (0, 1, 0));
    // a second take ends and the other half is heard: the next hit seeks
    for line in ["start src", "start rec"] {
        run(&mut cp, &mut c, line);
    }
    render(&mut c, 1, 3100);
    feed(&mut cp, &mut queues);
    assert_eq!(hit(&mut cp, &mut c), (1, 0, 0));
    assert_eq!(hit(&mut cp, &mut c), (0, 1, 0));
}

// Run by hand in release: the worst 128-frame callback of a stereo choir of
// 8 retriggered on 16ths (a step every 6000 frames) at +7 st, over 2 s,
// against the 2667 us a callback lasts.
//   cargo test --release -- --ignored hit_cost_worst_callback --nocapture
#[test]
#[ignore = "timing; run by hand in release"]
fn hit_cost_worst_callback() {
    for (name, channels, settings) in [
        ("sim, intonation 0.5", 2, "choirmode=sim intonation=0.5"),
        ("sim, intonation 1", 2, "choirmode=sim intonation=1"),
        (
            "sequential, intonation 0.5",
            2,
            "choirmode=seq intonation=0.5",
        ),
        (
            "stereo, sim, intonation 0.5, window=60ms",
            2,
            "choirmode=sim intonation=0.5 window=60ms",
        ),
        (
            "8 channels, sim, intonation 0.5, window=60ms",
            8,
            "choirmode=sim intonation=0.5 window=60ms",
        ),
        (
            "8 channels, sim, intonation 0.5, window=90ms",
            8,
            "choirmode=sim intonation=0.5",
        ),
    ] {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(channels, vec![multi_saw(48000, channels)]);
        run(
            &mut cp,
            &mut c,
            &format!("voice v from \"t\" on 6000f pitch=7st choir=8 {settings}"),
        );
        run(&mut cp, &mut c, "v { seq s 1 2 3 4 }");
        run(&mut cp, &mut c, "start v");
        stats::reset();
        let mut buf = vec![0.0f32; 128 * channels];
        let mut worst = std::time::Duration::ZERO;
        let mut total = std::time::Duration::ZERO;
        let mut worst_at = 0;
        let mut steady = std::time::Duration::ZERO;
        for n in 0..96000 / 128 {
            let t = std::time::Instant::now();
            c.render(&mut buf, channels);
            let dt = t.elapsed();
            if dt > worst {
                (worst, worst_at) = (dt, n);
            }
            total += dt;
            if n > 0 {
                steady = steady.max(dt);
            }
        }
        let (cold, copies, fallbacks) = stats::get();
        eprintln!(
            "{name}: worst {} us (callback {worst_at}), \
             worst after the first {} us, mean {} us per callback (budget 2667 us); \
             cold {cold}, copies {copies}, fallbacks {fallbacks}",
            worst.as_micros(),
            steady.as_micros(),
            total.as_micros() / 750
        );
    }
}

// ---- bounce= (PPF) ----

fn bounds(ls: i64, le: i64) -> Bounds {
    Bounds {
        rs: 0,
        re: le,
        ls,
        le,
        looping: true,
        front: 0,
    }
}

#[test]
fn bounce_gain_dips_at_the_edges() {
    let b = bounds(100, 1100);
    let pp = LoopMode::PingPong;
    let w = 96;
    // zero at the edge frames, one from `w` in
    assert_eq!(bounce_gain(&b, pp, 100, w), 0.0);
    assert_eq!(bounce_gain(&b, pp, 1100, w), 0.0);
    assert_eq!(bounce_gain(&b, pp, 100 + w as i64, w), 1.0);
    assert_eq!(bounce_gain(&b, pp, 1100 - w as i64, w), 1.0);
    assert_eq!(bounce_gain(&b, pp, 600, w), 1.0);
    // monotone up to w, and mirror-symmetric
    let mut last = 0.0;
    for d in 0..=w as i64 {
        let g = bounce_gain(&b, pp, 100 + d, w);
        assert!(g >= last, "{d}");
        last = g;
        assert_eq!(g, bounce_gain(&b, pp, 1100 - d, w), "{d}");
    }
    // exactly 1 when it can't apply
    assert_eq!(bounce_gain(&b, pp, 100, 0), 1.0);
    assert_eq!(bounce_gain(&b, LoopMode::Forward, 100, w), 1.0);
    assert_eq!(bounce_gain(&b, LoopMode::Off, 100, w), 1.0);
    // outside the loop the fade mirrors, and is 1 from `w` away
    assert!(bounce_gain(&b, pp, 99, w) < 1.0);
    assert!(bounce_gain(&b, pp, 1101, w) < 1.0);
    assert_eq!(bounce_gain(&b, pp, 100 - w as i64, w), 1.0);
    assert_eq!(bounce_gain(&b, pp, 1100 + w as i64, w), 1.0);
    assert_eq!(bounce_gain(&b, pp, 0, w), 1.0);
    let off = Bounds {
        looping: false,
        ..b
    };
    assert_eq!(bounce_gain(&off, pp, 100, w), 1.0);
}

#[test]
fn bounce_gain_is_continuous_across_the_edges_and_squared() {
    let b = bounds(100, 1100);
    let pp = LoopMode::PingPong;
    let w = 96;
    let max_step = std::f32::consts::PI / (2.0 * w as f32) + 1e-6;
    for edge in [100i64, 1100] {
        let mut last = bounce_gain(&b, pp, edge - 2 * w as i64, w);
        for f in edge - 2 * w as i64 + 1..=edge + 2 * w as i64 {
            let g = bounce_gain(&b, pp, f, w);
            assert!((g - last).abs() <= max_step, "frame {f}: {last} -> {g}");
            last = g;
        }
        // mirror-symmetric across the edge, zero at it
        assert_eq!(bounce_gain(&b, pp, edge, w), 0.0);
        for d in 1..=w as i64 {
            assert_eq!(
                bounce_gain(&b, pp, edge - d, w),
                bounce_gain(&b, pp, edge + d, w)
            );
        }
    }
    // sin^2: half way through the fade is 0.5
    let g = bounce_gain(&b, pp, 100 + w as i64 / 2, w);
    assert!((g - 0.5).abs() < 1e-6, "{g}");
    // w == 0 is exactly 1 everywhere
    for f in [0, 99, 100, 101, 600, 1099, 1100, 1101, 2000] {
        assert_eq!(bounce_gain(&b, pp, f, 0), 1.0);
    }
}

#[test]
fn bounce_gain_short_loop_only_dips_at_its_edges() {
    // a span below 2w: w' is half of it, so only the edges are 0
    for (ls, le) in [(0, 10), (5, 16), (0, 3), (0, 2), (7, 9)] {
        let b = bounds(ls, le);
        for f in ls..=le {
            let g = bounce_gain(&b, LoopMode::PingPong, f, 96);
            assert!((0.0..=1.0).contains(&g));
            match f == ls || f == le {
                true => assert_eq!(g, 0.0, "edge {f} of {ls}..{le}"),
                false => assert!(g > 0.0, "frame {f} of {ls}..{le}"),
            }
        }
    }
    // a loop of two frames has no room to fade
    let b = bounds(0, 1);
    assert_eq!(bounce_gain(&b, LoopMode::PingPong, 0, 96), 1.0);
}

#[test]
fn bounce_zero_matches_no_fade_and_forward_is_unaffected() {
    // forward loops ignore bounce
    assert_eq!(
        play(5, "loop=forward bounce=5ms", 12),
        play(5, "loop=forward bounce=0", 12)
    );
    // on a ramp, bounce=0 is the plain reflection; the default dips the edge
    assert_eq!(
        play(5, "loop=pingpong bounce=0", 14),
        [0, 1, 2, 3, 4, 3, 2, 1, 0, 1, 2, 3, 4, 3]
    );
    let faded = play(5000, "loop=pingpong", 10000);
    let plain = play(5000, "loop=pingpong bounce=0", 10000);
    assert_ne!(faded, plain);
    // away from the edges they agree
    assert_eq!(faded[300..4700], plain[300..4700]);
    // and the edge frame itself is silent
    assert_eq!(faded[4999], 0);
    assert_eq!(faded[0], 0);
}

// energy above 6 kHz of `out[from..]`, in 1024-frame Hann segments
fn hf_energy(out: &[f32], from: usize) -> f64 {
    let n = 1024;
    let win: Vec<f64> = (0..n)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos())
        .collect();
    let k0 = (6000.0 / 48000.0 * n as f64).ceil() as usize;
    let mut total = 0.0;
    let mut at = from;
    while at + n <= out.len() {
        for k in (k0..n / 2).step_by(2) {
            let w = std::f64::consts::TAU * k as f64 / n as f64;
            let (mut re, mut im) = (0.0, 0.0);
            for i in 0..n {
                let x = f64::from(out[at + i]) * win[i];
                re += x * (w * i as f64).cos();
                im -= x * (w * i as f64).sin();
            }
            total += re * re + im * im;
        }
        at += n / 2;
    }
    total
}

// the drop in dB above 6 kHz from bounce=0 to the default 2 ms, for a 1 kHz
// sine ping-pong loop that bounces at an edge off a zero crossing
fn bounce_drop_db(settings: &str, spectral: bool) -> f64 {
    let hf = |bounce: &str| {
        // 125 cycles and an eighth: both edges are off a peak
        let (mut cp, mut c) = spectral_rig(
            sine_track(6006, 48.0, 20000.0),
            &format!("loop=pingpong bounce={bounce} {settings}"),
        );
        run(&mut cp, &mut c, "start v");
        assert_eq!(c.voices.get(0).unwrap().state.inst[0].spectral, spectral);
        let out = render(&mut c, 1, 32000);
        hf_energy(&out, 12000)
    };
    10.0 * (hf("0") / hf("2ms")).log10()
}

#[test]
fn bounce_cuts_the_click_on_plain_playback() {
    let db = bounce_drop_db("", false);
    eprintln!("plain: {db:.1} dB");
    assert!(db >= 15.0, "{db} dB");
}

#[test]
fn bounce_cuts_the_click_on_the_grains() {
    let db = bounce_drop_db("shift=grains pitch=-5st", false);
    eprintln!("grains: {db:.1} dB");
    assert!(db >= 15.0, "{db} dB");
}

#[test]
fn bounce_cuts_the_click_on_the_spectral_shifter() {
    let db = bounce_drop_db("pitch=-5st", true);
    eprintln!("spectral: {db:.1} dB");
    assert!(db >= 15.0, "{db} dB");
}

// the case of tests/scenarios/shift-refused-hits.blast, with the counters
// the scenario can't read: three spectral voices at different pitches hit
// on one frame of a mono source. A block has 2 units and a mono cold seek
// costs 3, and the first seek of a block is always allowed, so one hit is
// served and two are refused (they play on the grains).
#[test]
fn same_frame_hits_are_refused() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![saw_track(20000, 440.0)]);
    for (name, st) in [("a", 3), ("b", 7), ("c", -5)] {
        run(
            &mut cp,
            &mut c,
            &format!("voice {name} from \"t\" on 1000f shift=spectral window=60ms pitch={st}st"),
        );
    }
    for name in ["a", "b", "c"] {
        run(&mut cp, &mut c, &format!("start {name}"));
    }
    stats::reset();
    render(&mut c, 1, 64);
    // (cold seeks, snapshot copies, fallbacks to the grains)
    assert_eq!(stats::get(), (1, 0, 2));
    let spectral = c
        .voices
        .iter()
        .filter(|(_, v)| v.state.inst[0].spectral)
        .count();
    assert_eq!(spectral, 1);
}

// ---- waves (WAVa) ----

fn wave_voice(pitch: f32) -> Voice {
    use crate::audio_processing::engine::{WaveShape, wave_defaults};
    let mut v = Voice::from_waves(crate::audio_processing::commands::TempoId(0), false);
    v.waves.insert(0, WaveShape::Sine, wave_defaults(), true);
    v.state.active = true;
    v.state.set_param(VoiceParam::Pitch, pitch, 0);
    v.hit();
    v
}

#[test]
fn wave_only_voice_no_grains() {
    sample_rate::set(48000);
    let mut v = wave_voice(12.0);
    assert!(!v.state.engine_wanted());
    for inst in &v.state.inst {
        assert!(!inst.granular && !inst.spectral);
    }
    let mut frame = [0.0f32; 1];
    for now in 0..100 {
        v.render_frame(
            &mut frame,
            crate::audio_processing::engine::wave::test_table(),
            now,
        );
        v.advance();
    }
    for inst in &v.state.inst {
        assert!(!inst.granular && !inst.spectral && !inst.shift_hit);
    }
    // a pitch that moves later doesn't wake the engine either
    v.state.set_param(VoiceParam::Pitch, -7.0, 0);
    v.hit();
    assert!(!v.state.inst[0].granular);
}

#[test]
fn wave_only_voice_never_reads_source() {
    sample_rate::set(48000);
    let mut v = wave_voice(0.0);
    assert_eq!(v.source.frames(v.channels), 0);
    let mut peak = 0.0f32;
    for i in 0..10_000 {
        let mut frame = [0.0f32; 2];
        v.render_frame(
            &mut frame,
            crate::audio_processing::engine::wave::test_table(),
            i as u64,
        );
        peak = peak.max(frame[0].abs());
        v.advance();
        if i % 1000 == 0 {
            v.hit();
        }
    }
    // a mono voice on a stereo bus: heard on both sides
    assert!(peak > 0.5);
    v.rewind();
    v.park();
    let mut frame = [0.0f32; 2];
    v.render_frame(
        &mut frame,
        crate::audio_processing::engine::wave::test_table(),
        20_000,
    );
    assert_eq!(frame, [0.0, 0.0]);
}

#[test]
fn a_hit_retriggers_only_the_instance_it_starts() {
    sample_rate::set(48000);
    let mut v = wave_voice(0.0);
    v.state.set_choir(2);
    // sequential: the hit above took instance 0, so these take 0, then 1
    v.hit();
    assert_eq!(v.state.started, 0b01);
    v.hit();
    assert_eq!(v.state.started, 0b10);
    v.state.set_choir_mode(ChoirMode::Simultaneous);
    v.hit();
    assert_eq!(v.state.started, 0b11);
}

#[test]
fn choir_instances_play_their_own_frequencies() {
    use crate::audio_processing::engine::{WaveShape, wave_defaults};
    sample_rate::set(48000);
    let mut v = Voice::from_waves(crate::audio_processing::commands::TempoId(0), false);
    let mut p = wave_defaults();
    p[0] = 300.0;
    v.waves.insert(0, WaveShape::Sine, p, true);
    v.state.active = true;
    v.state.set_choir(3);
    v.state.set_choir_mode(ChoirMode::Simultaneous);
    v.state.set_param(VoiceParam::Intonation, 0.0, 0);
    v.state.seed(5);
    v.hit();
    let mul: Vec<f32> = v.state.inst[..3].iter().map(|i| i.rate_mul).collect();
    assert!(
        mul[0] != mul[1] && mul[1] != mul[2] && mul[0] != mul[2],
        "{mul:?}"
    );
    // only the frequency differs between the instances
    for inst in &mut v.state.inst[..3] {
        inst.delay_left = 0;
        inst.gain_mul = 1.0;
    }
    // the same three waves by hand, each at its instance's ratio
    let mut by_hand = v.waves.clone();
    let mut got = Vec::new();
    for now in 0..2000 {
        let mut f = [0.0f32; 1];
        v.render_frame(
            &mut f,
            crate::audio_processing::engine::wave::test_table(),
            now,
        );
        v.advance();
        got.push(f[0]);
        by_hand.set_now(now);
        let want: f32 = (0..3)
            .map(|i| {
                by_hand.render(
                    i,
                    f64::from(mul[i]),
                    48_000.0,
                    crate::audio_processing::engine::wave::test_table(),
                )
            })
            .sum();
        by_hand.advance_ramps();
        assert!((f[0] - want).abs() < 1e-5);
    }
    // and it isn't what equal ratios would give
    let mut flat = Waves::new();
    flat.insert(0, WaveShape::Sine, p, true);
    let same: Vec<f32> = (0..2000)
        .map(|now| {
            flat.set_now(now);
            (0..3)
                .map(|i| {
                    flat.render(
                        i,
                        1.0,
                        48_000.0,
                        crate::audio_processing::engine::wave::test_table(),
                    )
                })
                .sum::<f32>()
        })
        .collect();
    assert!(got.iter().zip(&same).any(|(g, s)| (g - s).abs() > 0.1));
}

#[test]
fn a_waves_voice_given_a_track_plays_it_from_the_top() {
    sample_rate::set(48000);
    for rate in [1.0f32, -1.0] {
        let (mut cp, mut c) = setup(1, vec![ramp("t", 10)]);
        run(&mut cp, &mut c, "voice v on 1000ms { wave w gain=0 }");
        run(&mut cp, &mut c, &format!("voice v rate={rate}"));
        run(&mut cp, &mut c, "start v");
        render(&mut c, 1, 3);
        run(&mut cp, &mut c, "voice v from \"t\"");
        let out = render_i16(&mut c, 1, 3);
        // the ramp is i, so it starts at its first frame (backwards, its last)
        let want = if rate > 0.0 { [0, 1, 2] } else { [9, 8, 7] };
        assert_eq!(out, want, "rate {rate}");
    }
}

// PERF-IIR2: the integer floor and ceil are `floor() as i64` and
// `ceil() as i64`, for every kind of value
#[test]
fn floor_and_ceil_i64_match_the_float_ones() {
    let mut xs = vec![
        0.0,
        -0.0,
        0.5,
        -0.5,
        1.0,
        -1.0,
        2.999_999_999_999_999_6,
        -2.999_999_999_999_999_6,
        1e-300,
        -1e-300,
        4_503_599_627_370_495.5,
        -4_503_599_627_370_495.5,
        9_007_199_254_740_993.0,
        9.223_372_036_854_775e18,
        -9.223_372_036_854_775e18,
        1e30,
        -1e30,
        f64::MAX,
        f64::MIN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        f64::EPSILON,
        -f64::EPSILON,
    ];
    // and a sweep across the integers, both signs, with fractions and the
    // floats next to them
    for k in -2000..2000 {
        let x = f64::from(k) * 0.37;
        xs.extend([
            x,
            x.next_up(),
            x.next_down(),
            f64::from(k),
            f64::from(k).next_down(),
        ]);
    }
    for &x in &xs {
        // and `bounds`' slice index: `as usize` is `floor() as usize`
        let y = x as f32;
        assert_eq!(y as usize, y.floor() as usize, "usize {y:e}");
    }
    for x in xs {
        assert_eq!(floor_i64(x), x.floor() as i64, "floor {x:e}");
        assert_eq!(ceil_i64(x), x.ceil() as i64, "ceil {x:e}");
    }
}
