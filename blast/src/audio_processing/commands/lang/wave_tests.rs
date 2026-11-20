//! WAVa's tests: the `wave` kind in the language, the mirror, and the engine
//! end to end.

use super::Code;
use super::lower::note_hz;
use crate::audio_processing::blast_rand::{clear_session_seed, set_session_seed};
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::testing::*;
use crate::audio_processing::commands::*;
use crate::audio_processing::engine::{Conductor, VoiceParam, WaveParam, WaveShape};
use crate::audio_processing::processes::LfoTarget;
use crate::audio_processing::test_util::{render, run, setup, track};

fn session() -> CmdProcessor {
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\"");
    cp
}

fn waves(cp: &CmdProcessor, voice: &str) -> Vec<WaveRepr> {
    let v = cp.engine_state.voice_id(voice).unwrap();
    cp.engine_state.wave_list(v).to_vec()
}

fn batch_cmds(cmd: Command) -> Vec<Command> {
    match cmd {
        Command::Batch(b) => b.cmds,
        other => vec![other],
    }
}

fn wave_ops(cmds: Vec<Command>) -> Vec<WaveOp> {
    cmds.into_iter()
        .filter_map(|c| match c {
            Command::Wave(a) => Some(a.op),
            _ => None,
        })
        .collect()
}

fn codes(cp: &mut CmdProcessor, line: &str) -> Vec<Code> {
    lerr(cp, line).iter().map(|e| e.code).collect()
}

// ---- lowering ----

#[test]
fn voice_with_wave_needs_no_from() {
    let mut cp = lang_processor();
    let cmds = batch_cmds(lok(&mut cp, "voice organ { wave s1 shape=saw freq=110 }"));
    assert!(matches!(
        &cmds[0],
        Command::Load(a) if matches!(a.source, LoadSource::Waves)
    ));
    let v = cp.engine_state.voice_id("organ").unwrap();
    assert_eq!(cp.engine_state.voices[&v].source, VoiceSrc::Waves);
    let w = waves(&cp, "organ");
    assert_eq!(w.len(), 1);
    assert_eq!(
        (w[0].name.as_str(), w[0].id, w[0].shape),
        ("s1", 0, WaveShape::Saw)
    );
    assert_eq!(w[0].params[WaveParam::Freq.index()], 110.0);
    let ops = wave_ops(cmds);
    assert!(matches!(
        ops[0],
        WaveOp::Insert {
            shape: WaveShape::Saw,
            retrig: true,
            ..
        }
    ));
}

#[test]
fn voice_with_nothing_is_e405() {
    let mut cp = lang_processor();
    let errs = lerr(&mut cp, "voice v");
    assert_eq!(errs[0].code, Code::MissingItem);
    assert!(errs[0].message.contains("a track, a buffer or waves"));
    assert_eq!(codes(&mut cp, "voice v { seq s 1 2 }"), [Code::MissingItem]);
    assert!(cp.engine_state.voice_id("v").is_err());
}

#[test]
fn track_setting_on_wave_voice_is_w108() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice organ start=0.5 loop=forward { wave s1 }");
    let out = std::mem::take(&mut cp.outbox);
    assert!(out.contains("W108"), "{out}");
    assert!(
        out.contains("organ plays waves only: start does nothing"),
        "{out}"
    );
    assert!(out.contains("loop does nothing"), "{out}");
    // stored as usual
    let v = cp.engine_state.voice_id("organ").unwrap();
    assert_eq!(
        cp.engine_state.voices[&v].params[VoiceParam::Start.index()],
        0.5
    );
    // on an update too
    lok(&mut cp, "organ end=0.9");
    let out = std::mem::take(&mut cp.outbox);
    assert!(
        out.contains("W108") && out.contains("end does nothing"),
        "{out}"
    );
    // a track voice says nothing
    lok(&mut cp, "voice kick from \"kick\" start=0.5");
    assert!(!std::mem::take(&mut cp.outbox).contains("W108"));
}

#[test]
fn note_names_to_hz() {
    let near = |w: &str, hz: f64| {
        let got = note_hz(w).unwrap_or_else(|| panic!("{w} is a note"));
        assert!((got - hz).abs() < 0.01, "{w}: {got} not {hz}");
    };
    near("a4", 440.0);
    near("a3", 220.0);
    near("c4", 261.63);
    near("c#4", 277.18);
    near("eb2", 77.78);
    near("db4", 277.18);
    near("b3", 246.94);
    near("bb3", 233.08);
    near("c-1", 8.18);
    near("f#-1", 11.56);
    near("A4", 440.0);
    for bad in [
        "", "h4", "a", "a#", "4", "aa4", "a4x", "c##4", "c100", "e#", "ab",
    ] {
        assert!(note_hz(bad).is_none(), "{bad}");
    }
    // `e#4` is a sharp on e: f4
    near("e#4", 349.23);
}

#[test]
fn freq_takes_hz_hz_suffix_and_notes() {
    let mut cp = session();
    lok(&mut cp, "kick { wave a freq=a3 }");
    lok(&mut cp, "kick { wave b freq=330hz }");
    lok(&mut cp, "kick { wave c freq=c#4 }");
    let f = |n: usize| waves(&cp, "kick")[n].params[WaveParam::Freq.index()];
    assert_eq!(f(0), 220.0);
    assert_eq!(f(1), 330.0);
    assert!((f(2) - 277.18).abs() < 0.01);
    let e = lerr(&mut cp, "kick { wave d freq=zz }");
    assert_eq!(e[0].code, Code::WrongType);
    assert!(
        e[0].message.contains("a frequency in Hz"),
        "{}",
        e[0].message
    );
    assert_eq!(codes(&mut cp, "kick { wave d freq=0 }"), [Code::OutOfRange]);
    assert_eq!(
        codes(&mut cp, "kick { wave d freq=30000 }"),
        [Code::OutOfRange]
    );
    assert_eq!(
        codes(&mut cp, "kick { wave d shape=blip }"),
        [Code::WrongType]
    );
    assert_eq!(codes(&mut cp, "kick { wave d pw=1 }"), [Code::OutOfRange]);
    assert_eq!(codes(&mut cp, "kick { wave d gain=9 }"), [Code::OutOfRange]);
    // db gain, short forms
    lok(
        &mut cp,
        "kick { wave d s=square f=100 g=-6db w=0.25 r=no p=0.5 }",
    );
    let d = waves(&cp, "kick").pop().unwrap();
    assert_eq!(d.shape, WaveShape::Square);
    assert!((d.params[WaveParam::Gain.index()] - 0.501_187).abs() < 1e-4);
    assert_eq!(d.params[WaveParam::Pw.index()], 0.25);
    assert_eq!(d.params[WaveParam::Phase.index()], 0.5);
    assert!(!d.retrig);
}

#[test]
fn wave_restate_edits_in_place() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice organ { wave s1 shape=sine freq=220 }");
    // outside the block, by path, with a glide
    let cmds = batch_cmds(lok(&mut cp, "organ.s1 freq=330 over 2"));
    let ops = wave_ops(cmds);
    assert!(matches!(
        ops[0],
        WaveOp::Set { param: 0, value, frames } if value == 330.0 && frames > 0
    ));
    assert_eq!(waves(&cp, "organ")[0].params[0], 330.0);
    // the same statement again changes nothing
    let text = local(&mut cp, "organ.s1 freq=330");
    assert!(text.contains("W101"), "{text}");
    // `wave organ.s1 …` and the block form edit it too, shape and retrig included
    let ops = wave_ops(batch_cmds(lok(
        &mut cp,
        "organ { wave s1 shape=tri retrig=no }",
    )));
    assert!(matches!(ops[0], WaveOp::Shape(WaveShape::Tri)));
    assert!(matches!(ops[1], WaveOp::Retrig(false)));
    lok(&mut cp, "wave organ.s1 gain=0.5");
    assert_eq!(waves(&cp, "organ").len(), 1);
    assert_eq!(waves(&cp, "organ")[0].params[WaveParam::Gain.index()], 0.5);
    // a wave that isn't there needs the `wave` word
    assert_eq!(codes(&mut cp, "organ.nope freq=100"), [Code::UnknownName]);
    lok(&mut cp, "wave organ.s2 shape=saw");
    assert_eq!(waves(&cp, "organ").len(), 2);
}

#[test]
fn wave_limit_is_eight() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice organ { wave w0 }");
    for i in 1..8 {
        lok(&mut cp, &format!("organ {{ wave w{i} }}"));
    }
    assert_eq!(waves(&cp, "organ").len(), 8);
    assert_eq!(codes(&mut cp, "organ { wave w8 }"), [Code::LimitReached]);
    // a freed id is reused
    lok(&mut cp, "remove organ.w3");
    let ops = batch_cmds(lok(&mut cp, "organ { wave again }"));
    assert!(matches!(&ops[0], Command::Wave(a) if a.id == 3));
}

#[test]
fn wave_and_other_names_clash() {
    let mut cp = session();
    lok(&mut cp, "kick { filter lo }");
    assert_eq!(codes(&mut cp, "kick { wave lo }"), [Code::KindClash]);
    lok(&mut cp, "kick { wave w1 }");
    assert_eq!(codes(&mut cp, "kick { filter w1 }"), [Code::KindClash]);
    assert_eq!(codes(&mut cp, "kick { seq w1 1 2 }"), [Code::KindClash]);
    assert_eq!(
        codes(&mut cp, "kick { lfo w1 to=pan depth=1 }"),
        [Code::KindClash]
    );
    lok(&mut cp, "kick { seq beat 1 2 }");
    assert_eq!(codes(&mut cp, "kick { wave beat }"), [Code::KindClash]);
    assert_eq!(codes(&mut cp, "kick { wave all }"), [Code::ReservedWord]);
    // a group can't hold a wave
    lok(&mut cp, "group g { kick }");
    assert_eq!(codes(&mut cp, "g { wave x }"), [Code::WrongType]);
}

#[test]
fn from_added_later_keeps_the_waves() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice organ { wave s1 }");
    let cmds = batch_cmds(lok(&mut cp, "voice organ from \"kick\""));
    assert!(matches!(cmds[0], Command::SetTrack(_)));
    let v = cp.engine_state.voice_id("organ").unwrap();
    assert!(matches!(
        cp.engine_state.voices[&v].source,
        VoiceSrc::Track(_)
    ));
    assert_eq!(waves(&cp, "organ").len(), 1);
}

#[test]
fn wave_actions_and_remove() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice organ { wave s1; wave s2 }");
    let ops = wave_ops(batch_cmds(lok(&mut cp, "stop organ.s1")));
    assert!(matches!(
        ops[0],
        WaveOp::Bypass {
            on: true,
            clear: true
        }
    ));
    assert_eq!(waves(&cp, "organ")[0].bypass, Some(Bypass::Stopped));
    // already stopped: warns, nothing sent
    assert!(local(&mut cp, "stop organ.s1").contains("W101"));
    lok(&mut cp, "start organ.s1");
    let ops = wave_ops(batch_cmds(lok(&mut cp, "pause organ.s2")));
    assert!(matches!(
        ops[0],
        WaveOp::Bypass {
            on: true,
            clear: false
        }
    ));
    lok(&mut cp, "resume organ.s2");
    assert_eq!(waves(&cp, "organ")[1].bypass, None);
    // `all` doesn't touch waves, as it doesn't touch effects
    for line in ["stop all", "start all"] {
        let cmds = batch_cmds(lok(&mut cp, line));
        assert!(wave_ops(cmds).is_empty(), "{line}");
    }
    let ops = wave_ops(batch_cmds(lok(&mut cp, "remove organ.s1")));
    assert!(matches!(ops[0], WaveOp::Remove));
    assert_eq!(waves(&cp, "organ").len(), 1);
    // as for effects: the first part of an `at` line may not make a wave, and
    // a wave can't be removed in a part that waits
    assert_eq!(
        codes(&mut cp, "organ { wave late } at 4").first().copied(),
        Some(Code::CannotWait)
    );
    lok(&mut cp, "organ { wave extra }");
    assert_eq!(
        codes(&mut cp, "organ.s2 gain=0.5 over 1b then remove organ.extra")
            .first()
            .copied(),
        Some(Code::CannotWait)
    );
}

#[test]
fn lfo_targets_wave_freq() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice organ { wave s1; wave s2 shape=saw }");
    let cmds = batch_cmds(lok(
        &mut cp,
        "organ { lfo wob to=s2.freq depth=0.5 freq=4hz }",
    ));
    let Command::Lfo(a) = &cmds[0] else {
        panic!("not an Lfo")
    };
    assert_eq!(
        a.target,
        LfoTarget::Wave {
            id: 1,
            param: WaveParam::Freq.index() as u8
        }
    );
    lok(
        &mut cp,
        "organ { env pluck to=s1.gain decay=100ms sustain=0 }",
    );
    lok(&mut cp, "organ { lfo pw to=s2.pw depth=0.2 freq=1hz }");
    let e = lerr(&mut cp, "organ { lfo bad to=s1.phase depth=1 }");
    assert_eq!(e[0].code, Code::UnknownSetting);
    assert_eq!(
        codes(&mut cp, "organ { lfo bad to=s1.nope depth=1 }"),
        [Code::UnknownSetting]
    );
    // a name that isn't a wave or an effect still says so
    assert_eq!(
        codes(&mut cp, "organ { lfo bad to=zz.freq depth=1 }"),
        [Code::UnknownSetting]
    );
}

#[test]
fn remove_wave_drops_lfo_w102() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice organ { wave s1; wave s2 }");
    lok(
        &mut cp,
        "organ { lfo wob to=s1.freq depth=1 and env amp to=s1.gain decay=10ms }",
    );
    lok(&mut cp, "organ { lfo keep to=pan depth=0.3 }");
    let cmd = lok(&mut cp, "remove organ.s1");
    let out = std::mem::take(&mut cp.outbox);
    assert!(out.contains("W102") && out.contains("organ.wob"), "{out}");
    assert!(out.contains("organ.amp"), "{out}");
    let removed = batch_cmds(cmd)
        .iter()
        .filter(|c| matches!(c, Command::ProcEdit(a) if a.op == ProcOp::Remove))
        .count();
    assert_eq!(removed, 2);
    let v = cp.engine_state.voice_id("organ").unwrap();
    assert_eq!(cp.engine_state.voices[&v].procs.len(), 1);
}

#[test]
fn state_round_trips_waves() {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice organ { wave s1 shape=saw freq=110 gain=-6db; wave s2 shape=square pw=0.25 retrig=no; wave s3 freq=a4 phase=0.5 }",
    );
    lok(&mut cp, "organ { lfo wob to=s1.freq depth=0.5 freq=2hz }");
    lok(&mut cp, "stop organ.s2");
    lok(&mut cp, "pause organ.s3");
    lok(
        &mut cp,
        "voice kick from \"kick\" { wave hat shape=noise gain=0.5 }",
    );
    let text = cp.state(None).unwrap();
    assert!(
        text.contains("voice organ { wave s1 shape=saw freq=110 gain=-6db; wave s2 shape=square pw=0.25 retrig=no; wave s3 freq=440 phase=0.5 }"),
        "{text}"
    );
    assert!(text.contains("stop organ.s2"), "{text}");
    assert!(text.contains("pause organ.s3"), "{text}");
    assert!(text.contains("wave hat shape=noise gain=0.5"), "{text}");
    assert!(!text.contains("voice organ from"), "{text}");
    // the text recreates the session, and feeding it back changes nothing
    let mut fresh = lang_processor();
    lok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
    let answer = local(&mut fresh, &text);
    for line in answer.lines() {
        assert!(line.contains("W101"), "{line}");
    }
    // one wave by itself
    let one = cp.state(Some("organ.s2")).unwrap();
    assert!(
        one.contains("wave s2") && one.contains("stop organ.s2"),
        "{one}"
    );
}

// ---- end to end ----

fn rig(lines: &[&str]) -> (CmdProcessor, Conductor) {
    sample_rate::set(48_000);
    let (mut cp, mut c) = setup(1, vec![track("dc", 8192, 100_000)]);
    c.apply(cp.boot());
    for line in lines {
        run(&mut cp, &mut c, line);
    }
    (cp, c)
}

// rising zero crossings per second of `x` (48 kHz)
fn hz_of(x: &[f32]) -> f64 {
    let ups: Vec<f64> = x
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f64 + f64::from(-w[0] / (w[1] - w[0])))
        .collect();
    48_000.0 * (ups.len() - 1) as f64 / (ups[ups.len() - 1] - ups[0])
}

#[test]
fn a_wave_voice_plays_at_its_frequency() {
    let (_cp, mut c) = rig(&["voice organ { wave s1 freq=1000 }", "start organ"]);
    let x = render(&mut c, 1, 9600);
    assert!((hz_of(&x) - 1000.0).abs() < 1.0, "{}", hz_of(&x));
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!((peak - 1.0).abs() < 0.01, "{peak}");
}

#[test]
fn pitch_and_rate_multiply_the_frequency() {
    for (line, want) in [
        ("voice organ pitch=2 { wave s1 freq=500 }", 1000.0),
        ("voice organ pitch=12st { wave s1 freq=500 }", 1000.0),
        ("voice organ rate=2 { wave s1 freq=500 }", 1000.0),
        ("voice organ pitch=0.5 { wave s1 freq=500 }", 250.0),
        ("voice organ { wave s1 freq=500 }", 500.0),
    ] {
        let (_cp, mut c) = rig(&[line, "start organ"]);
        let x = render(&mut c, 1, 19_200);
        assert!(
            (hz_of(&x) - want).abs() < want * 0.002,
            "{line}: {}",
            hz_of(&x)
        );
    }
}

#[test]
fn lfo_on_wave_freq_moves_it() {
    // a saw lfo starts at -1: an octave down
    let (_cp, mut c) = rig(&[
        "voice organ { wave s1 freq=500 }",
        "organ { lfo down to=s1.freq depth=1 shape=saw offset=0 freq=0.001hz phase=0 }",
        "start organ",
    ]);
    let x = render(&mut c, 1, 4800);
    assert!((hz_of(&x) - 250.0).abs() < 5.0, "{}", hz_of(&x));
    let (_cp, mut c) = rig(&[
        "voice organ { wave s1 freq=500 }",
        "organ { lfo up to=s1.freq depth=0 offset=1 freq=1hz }",
        "start organ",
    ]);
    let x = render(&mut c, 1, 4800);
    assert!((hz_of(&x) - 1000.0).abs() < 5.0, "{}", hz_of(&x));
}

#[test]
fn env_shapes_a_wave() {
    let (_cp, mut c) = rig(&[
        "voice organ { wave s1 freq=1000; env pluck to=s1.gain attack=1ms decay=100ms sustain=0 }",
        "start organ",
    ]);
    let x = render(&mut c, 1, 12_000);
    let rms =
        |a: usize, b: usize| (x[a..b].iter().map(|v| v * v).sum::<f32>() / (b - a) as f32).sqrt();
    assert!(rms(100, 600) > 0.4, "{}", rms(100, 600));
    assert!(rms(10_000, 12_000) < 0.01, "{}", rms(10_000, 12_000));
}

#[test]
fn choir_instances_detune_and_sum() {
    // exact instances are the same wave three times over
    let (_cp, mut c) = rig(&[
        "voice organ choir=3 choirmode=sim { wave s1 freq=500 }",
        "start organ",
    ]);
    let exact = render(&mut c, 1, 9600);
    let peak = exact.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!((peak - 3.0).abs() < 0.01, "{peak}");
    // with intonation below 1 each instance draws its own detune, gain and
    // delay (a session seed makes the draws repeat): three different phases
    set_session_seed(3);
    let (_cp, mut c) = rig(&[
        "voice organ choir=3 choirmode=sim intonation=0.5 { wave s1 freq=500 }",
        "start organ",
    ]);
    clear_session_seed();
    let loose = render(&mut c, 1, 9600);
    assert_ne!(loose, exact);
    let peak = loose.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.5 && peak < 3.0, "{peak}");
}

#[test]
fn retrig_restarts_the_phase_on_a_trigger() {
    // 360 frames: not a whole cycle at 100 Hz, so the restart is a real jump
    let (mut cp, mut c) = rig(&["voice organ { wave s1 freq=100 }", "start organ"]);
    let first = render(&mut c, 1, 360);
    run(&mut cp, &mut c, "start organ");
    let again = render(&mut c, 1, 360);
    // the jump back is smoothed, and then the wave begins again
    assert!((first[0] - again[0]).abs() > 0.3);
    for i in 150..360 {
        assert!((first[i] - again[i]).abs() < 3e-2, "{i}");
    }
    let (mut cp, mut c) = rig(&["voice organ { wave s1 freq=100 retrig=no }", "start organ"]);
    let first = render(&mut c, 1, 100);
    run(&mut cp, &mut c, "start organ");
    let again = render(&mut c, 1, 100);
    assert_ne!(first, again);
}

#[test]
fn stop_silences_and_pause_holds_a_wave() {
    let (mut cp, mut c) = rig(&[
        "voice organ { wave s1 freq=100; wave s2 gain=0 }",
        "start organ",
    ]);
    let first = render(&mut c, 1, 50);
    run(&mut cp, &mut c, "pause organ.s1");
    assert!(render(&mut c, 1, 100).iter().all(|v| *v == 0.0));
    run(&mut cp, &mut c, "resume organ.s1");
    // it comes back as an onset, and then it is the phase it held
    let more = render(&mut c, 1, 400);
    let (_cp2, mut c2) = rig(&[
        "voice organ { wave s1 freq=100; wave s2 gain=0 }",
        "start organ",
    ]);
    let all = render(&mut c2, 1, 450);
    assert_eq!(first, all[..50]);
    for i in 150..400 {
        assert!((more[i] - all[50 + i]).abs() < 2e-2, "{i}");
    }
    run(&mut cp, &mut c, "stop organ.s1");
    assert!(render(&mut c, 1, 100).iter().all(|v| *v == 0.0));
    run(&mut cp, &mut c, "start organ.s1");
    assert_eq!(render(&mut c, 1, 50), first);
    // removing it silences the voice
    run(&mut cp, &mut c, "remove organ.s1");
    assert!(render(&mut c, 1, 100).iter().all(|v| *v == 0.0));
}

#[test]
fn the_last_wave_of_a_waves_only_voice_stays() {
    let mut cp = session();
    lok(&mut cp, "voice organ { wave s1; wave s2 }");
    lok(&mut cp, "remove organ.s1");
    let e = lerr(&mut cp, "remove organ.s2");
    assert_eq!(e[0].code, Code::InUse);
    assert!(
        e[0].message.contains("organ's last wave"),
        "{}",
        e[0].message
    );
    assert!(e[0].suggestion.as_deref().unwrap().contains("remove organ"));
    assert_eq!(waves(&cp, "organ").len(), 1);
    // add another first, then it goes
    lok(&mut cp, "organ { wave s3 }");
    lok(&mut cp, "remove organ.s2");
    assert_eq!(codes(&mut cp, "remove organ.s3"), [Code::InUse]);
    lok(&mut cp, "organ { wave s4 }");
    // one statement can't remove them all
    assert_eq!(codes(&mut cp, "remove organ.s3 organ.s4"), [Code::InUse]);
    assert_eq!(waves(&cp, "organ").len(), 2);
    lok(&mut cp, "remove organ.s3");
    // in a line that waits, and in a part that is staged
    assert_eq!(codes(&mut cp, "remove organ.s4 after 2"), [Code::InUse]);
    assert_eq!(
        codes(&mut cp, "organ.s4 gain=0.5 over 1b then remove organ.s4"),
        [Code::InUse]
    );
    // the voice itself can go, and a voice with a track may lose its last wave
    lok(&mut cp, "remove organ");
    lok(&mut cp, "voice kick2 from \"kick\" { wave w }");
    lok(&mut cp, "remove kick2.w");
    assert!(waves(&cp, "kick2").is_empty());
}

#[test]
fn a_voice_with_a_track_and_waves_plays_both() {
    let (mut cp, mut c) = rig(&[
        "voice v from \"dc\" on 1000ms { wave s1 freq=1000 gain=0.5 }",
        "start v",
    ]);
    let x = render(&mut c, 1, 960);
    // the track is 8192/32768 = 0.25 and the sine swings 0.5 around it
    let (lo, hi) = x
        .iter()
        .fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
    assert!(
        (hi - 0.75).abs() < 0.01 && (lo + 0.25).abs() < 0.01,
        "{lo} {hi}"
    );
    // waves keep sounding after the track's region has ended
    let _ = render(&mut c, 1, 100_000);
    let tail = render(&mut c, 1, 960);
    let peak = tail.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!((peak - 0.5).abs() < 0.01, "{peak}");
    // a wave voice that is given a track plays both too
    run(
        &mut cp,
        &mut c,
        "voice organ { wave s1 freq=1000 gain=0.5 }",
    );
    run(&mut cp, &mut c, "start organ");
    let _ = render(&mut c, 1, 10);
}

#[test]
fn waves_on_a_stereo_pan() {
    sample_rate::set(48_000);
    let (mut cp, mut c) = setup(2, vec![]);
    c.apply(cp.boot());
    run(&mut cp, &mut c, "voice organ pan=-1 { wave s1 freq=1000 }");
    run(&mut cp, &mut c, "start organ");
    let out = render(&mut c, 2, 480);
    let left: f32 = out.iter().step_by(2).map(|v| v.abs()).sum();
    let right: f32 = out.iter().skip(1).step_by(2).map(|v| v.abs()).sum();
    assert!(left > 100.0 && right < 1e-3, "{left} {right}");
}

#[test]
fn noise_is_repeatable_with_a_session_seed() {
    let run_once = || {
        set_session_seed(7);
        let (_cp, mut c) = rig(&["voice organ { wave hat shape=noise }", "start organ"]);
        let x = render(&mut c, 1, 512);
        clear_session_seed();
        x
    };
    let (a, b) = (run_once(), run_once());
    assert_eq!(a, b);
    assert!(a.iter().any(|v| v.abs() > 0.1));
}

#[test]
fn a_melo_note_sets_the_pitch_of_a_wave_voice() {
    // a seq on 1000ms ticks plays notes; melo sets the pitch (60 is the
    // voice's own note): 72 is an octave up
    let (_cp, mut c) = rig(&[
        "voice organ on 1000ms { wave s1 freq=500 }",
        "organ { seq t 1 }",
        "organ.t | melo organ 72",
        "start organ",
    ]);
    let x = render(&mut c, 1, 48_000 / 2);
    assert!(hz_of(&x) > 900.0, "{}", hz_of(&x));
}

#[test]
fn phase_one_is_phase_zero_and_more_is_out_of_range() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice organ { wave s1 shape=saw phase=1 }");
    assert_eq!(waves(&cp, "organ")[0].params[WaveParam::Phase.index()], 1.0);
    assert_eq!(
        codes(&mut cp, "organ { wave s2 phase=1.5 }"),
        [Code::OutOfRange]
    );
    assert_eq!(
        codes(&mut cp, "organ { wave s2 phase=-0.1 }"),
        [Code::OutOfRange]
    );
    // and the engine plays phase 1 as phase 0, started and retriggered
    let play = |phase: &str| {
        let (mut cp, mut c) = rig(&[
            &format!("voice organ {{ wave s1 shape=saw freq=500 phase={phase} }}"),
            "start organ",
        ]);
        let mut x = render(&mut c, 1, 311);
        run(&mut cp, &mut c, "start organ");
        x.extend(render(&mut c, 1, 311));
        x
    };
    assert_eq!(play("1"), play("0"));
}

// ---- table waves (WAVb) ----

// a mono track of `frames` samples holding sine cycles, `cycles` of them
fn tone(
    name: &str,
    frames: usize,
    cycles: usize,
) -> crate::file_parsing::decode_helpers::AudioFile {
    let samples = (0..frames)
        .map(|i| (std::f64::consts::TAU * (cycles * i) as f64 / frames as f64).sin() as f32)
        .collect();
    crate::file_parsing::decode_helpers::AudioFile::new(name, "wav", 48_000, 1, 16, samples)
}

fn table_session() -> CmdProcessor {
    sample_rate::set(48_000);
    let (mut cp, _) = setup(1, vec![tone("pad", 8192, 4), tone("lead", 2048, 1)]);
    cp.boot();
    lok(&mut cp, "voice kick from \"pad\"");
    cp
}

fn table_ops(cmds: Vec<Command>) -> usize {
    wave_ops(cmds)
        .iter()
        .filter(|op| matches!(op, WaveOp::Table(_)))
        .count()
}

#[test]
fn a_table_wave_is_built_on_creation() {
    let mut cp = table_session();
    let cmds = batch_cmds(lok(
        &mut cp,
        "kick { wave t shape=table from \"pad\" slices=4 frames=1024 pos=0.5 }",
    ));
    let ops = wave_ops(cmds);
    assert!(matches!(
        ops[0],
        WaveOp::Insert {
            shape: WaveShape::Table,
            ..
        }
    ));
    let WaveOp::Table(table) = &ops[1] else {
        panic!("no table: {}", ops.len());
    };
    assert_eq!(
        (table.slices(), table.frames(), table.levels()),
        (4, 1024, 9)
    );
    let w = &waves(&cp, "kick")[0];
    assert_eq!((w.slices, w.frames), (4, 1024));
    assert!(matches!(w.from, Some(WaveFrom::Track(_))));
    assert_eq!(w.params[WaveParam::Pos.index()], 0.5);
    // the same through `from=`, and a buffer by its bare name
    let mut cp = table_session();
    let cmds = batch_cmds(lok(&mut cp, "kick { wave t shape=table from=\"lead\" }"));
    assert_eq!(table_ops(cmds), 1);
    let w = &waves(&cp, "kick")[0];
    assert_eq!((w.slices, w.frames), (1, 2048));
}

#[test]
fn table_needs_from_e405() {
    let mut cp = table_session();
    let errs = lerr(&mut cp, "kick { wave t shape=table }");
    assert_eq!(errs[0].code, Code::MissingItem);
    assert!(
        errs[0].message.contains("needs from"),
        "{}",
        errs[0].message
    );
    // a wave that was never a table has none to keep
    lok(&mut cp, "kick { wave s }");
    let errs = lerr(&mut cp, "kick.s shape=table");
    assert_eq!(errs[0].code, Code::MissingItem);
    // naming one is enough
    let cmds = batch_cmds(lok(&mut cp, "kick.s shape=table from \"lead\""));
    assert_eq!(table_ops(cmds), 1);
    // an unknown track says so
    let errs = lerr(&mut cp, "kick { wave u shape=table from \"nope\" }");
    assert!(
        matches!(errs[0].code, Code::UnknownName | Code::WrongType),
        "{errs:?}"
    );
}

#[test]
fn from_on_non_table_e402() {
    let mut cp = table_session();
    let errs = lerr(&mut cp, "kick { wave t from \"lead\" }");
    assert_eq!(errs[0].code, Code::WrongType);
    assert!(errs[0].message.contains("only a table wave takes from"));
    let errs = lerr(&mut cp, "kick { wave t shape=saw from \"lead\" }");
    assert_eq!(errs[0].code, Code::WrongType);
    for setting in ["slices=4", "frames=512"] {
        let errs = lerr(&mut cp, &format!("kick {{ wave t {setting} }}"));
        assert_eq!(errs[0].code, Code::WrongType, "{setting}");
    }
    // no wave was made by any of them
    assert!(waves(&cp, "kick").is_empty());
}

#[test]
fn frames_not_power_of_two_e403() {
    let mut cp = table_session();
    for frames in ["1000", "300", "128", "8192", "4097"] {
        let errs = lerr(
            &mut cp,
            &format!("kick {{ wave t shape=table from \"lead\" frames={frames} }}"),
        );
        assert_eq!(errs[0].code, Code::OutOfRange, "frames={frames}");
    }
    assert!(waves(&cp, "kick").is_empty());
    for frames in ["256", "512", "1024", "2048", "4096"] {
        let name = format!("w{frames}");
        lok(
            &mut cp,
            &format!("kick {{ wave {name} shape=table from \"lead\" frames={frames} }}"),
        );
    }
}

#[test]
fn table_too_large_e403() {
    let mut cp = table_session();
    // 257 cycles is past the language's own range
    let errs = lerr(
        &mut cp,
        "kick { wave t shape=table from \"lead\" slices=257 }",
    );
    assert_eq!(errs[0].code, Code::OutOfRange);
    let errs = lerr(
        &mut cp,
        "kick { wave t shape=table from \"lead\" slices=0 }",
    );
    assert_eq!(errs[0].code, Code::OutOfRange);
    // the largest the language allows (256 cycles of 4096 samples, 11
    // levels: 11.5 M samples) is under the 16 M cap and builds
    let cmds = batch_cmds(lok(
        &mut cp,
        "kick { wave big shape=table from \"pad\" slices=256 frames=4096 }",
    ));
    let table = wave_ops(cmds)
        .into_iter()
        .find_map(|op| match op {
            WaveOp::Table(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (table.slices(), table.frames(), table.levels()),
        (256, 4096, 11)
    );
}

#[test]
fn an_empty_buffer_makes_a_silent_table_wave_with_a_warning() {
    sample_rate::set(48_000);
    let (mut cp, _) = setup(1, vec![tone("pad", 8192, 4), tone("none", 0, 1)]);
    cp.boot();
    lok(&mut cp, "voice kick from \"pad\"");
    lok(&mut cp, "buffer b length=2048f");
    let line = "kick { wave t shape=table from=b }";
    let warns = cp.check_text("t", line).unwrap();
    assert_eq!(warns[0].code, Code::EmptySource);
    assert_eq!(
        warns[0].message,
        "b is empty: t stays silent until from=b is restated"
    );
    assert_eq!(warns[0].code.as_str(), "W109");
    let ops = wave_ops(batch_cmds(lok(&mut cp, line)));
    assert_eq!(ops.len(), 1, "an insert and no table");
    let w = &waves(&cp, "kick")[0];
    assert!(matches!(w.from, Some(WaveFrom::Buffer(_))));
    // a track with no frames is still an error
    let errs = lerr(&mut cp, "kick { wave u shape=table from \"none\" }");
    assert_eq!(errs[0].code, Code::WrongType);
    assert!(
        errs[0]
            .message
            .contains("has no sound to make a table from")
    );
}

#[test]
fn a_table_is_rebuilt_when_its_source_or_cut_changes() {
    let mut cp = table_session();
    lok(&mut cp, "kick { wave t shape=table from \"pad\" slices=2 }");
    // nothing about the table changed: no table, and the line says so
    assert!(local(&mut cp, "kick.t pos=0").contains("W101"));
    // pos only moves
    let cmds = batch_cmds(lok(&mut cp, "kick.t pos=1 over 2"));
    assert_eq!(table_ops(cmds), 0);
    // the source restated, the cut changed, each builds
    for line in [
        "kick.t from \"pad\"",
        "kick.t slices=3",
        "kick.t frames=512",
        "kick.t from=\"lead\"",
    ] {
        let cmds = batch_cmds(lok(&mut cp, line));
        assert_eq!(table_ops(cmds), 1, "{line}");
    }
    let w = &waves(&cp, "kick")[0];
    assert_eq!((w.slices, w.frames), (3, 512));
    // a change of the same value is no change
    assert!(local(&mut cp, "kick.t slices=3").contains("W101"));
    // leaving table keeps what it was cut from, and so does coming back
    lok(&mut cp, "kick.t shape=saw");
    let cmds = batch_cmds(lok(&mut cp, "kick.t shape=table"));
    assert_eq!(table_ops(cmds), 0);
}

#[test]
fn state_round_trips_table_wave() {
    let mut cp = table_session();
    lok(
        &mut cp,
        "kick { wave t shape=table from \"lead\" slices=4 frames=1024 pos=0.25 gain=0.5 }",
    );
    lok(
        &mut cp,
        "voice organ { wave s1 shape=table from \"pad\" slices=8 }",
    );
    let text = cp.state(None).unwrap();
    assert!(
        text.contains("wave t shape=table from \"lead\" slices=4 frames=1024 gain=0.5 pos=0.25"),
        "{text}"
    );
    assert!(
        text.contains("voice organ { wave s1 shape=table from \"pad\" slices=8 }"),
        "{text}"
    );
    // the text recreates the session (the tracks are the same), and feeding
    // it back changes nothing
    sample_rate::set(48_000);
    let (mut fresh, _) = setup(1, vec![tone("pad", 8192, 4), tone("lead", 2048, 1)]);
    fresh.boot();
    lok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
    let w = &waves(&fresh, "kick")[0];
    assert_eq!((w.shape, w.slices, w.frames), (WaveShape::Table, 4, 1024));
    // one wave by itself
    let one = cp.state(Some("kick.t")).unwrap();
    assert!(one.contains("shape=table from \"lead\""), "{one}");
}

#[test]
fn state_says_when_a_table_lost_its_source() {
    let mut cp = table_session();
    lok(&mut cp, "kick { wave t shape=table from \"lead\" }");
    lok(&mut cp, "kick { wave s freq=110 }");
    lok(
        &mut cp,
        "voice organ { wave only shape=table from \"lead\" }",
    );
    lok(&mut cp, "remove \"lead\"");
    let w = &waves(&cp, "kick")[0];
    assert_eq!(w.from, Some(WaveFrom::Gone));
    let text = cp.state(None).unwrap();
    assert!(!text.contains("from \"lead\""), "{text}");
    assert!(
        text.contains("# wave kick.t was cut from a track or buffer that was removed"),
        "{text}"
    );
    assert!(text.contains("wave s freq=110"), "{text}");
    assert!(
        text.contains("# voice organ plays a table cut from a track or buffer that was removed"),
        "{text}"
    );
    // the text is a script
    sample_rate::set(48_000);
    let (mut fresh, _) = setup(1, vec![tone("pad", 8192, 4)]);
    fresh.boot();
    lok(&mut fresh, &text);
    // the cut can't be redone from nothing, but restating it can
    let errs = lerr(&mut cp, "kick.t slices=2");
    assert_eq!(errs[0].code, Code::UnknownName);
}

#[test]
fn lfo_targets_pos() {
    let mut cp = table_session();
    lok(&mut cp, "kick { wave t shape=table from \"pad\" slices=4 }");
    lok(&mut cp, "kick { lfo sweep to=t.pos depth=0.5 freq=2hz }");
    let target = cp
        .engine_state
        .procs
        .values()
        .find_map(|p| p.lfo.as_ref().map(|l| l.target))
        .unwrap();
    assert_eq!(
        target,
        LfoTarget::Wave {
            id: waves(&cp, "kick")[0].id,
            param: WaveParam::Pos.index() as u8
        }
    );
    lok(&mut cp, "kick { env sw to=t.pos depth=0.5 }");
}

// a rig with tracks that make tables, and the garbage queue
fn table_rig(
    lines: &[&str],
) -> (
    CmdProcessor,
    Conductor,
    crate::audio_processing::engine::EngineRx,
) {
    sample_rate::set(48_000);
    let (mut cp, mut c) = setup(
        1,
        vec![
            tone("pad", 8192, 4),
            tone("lead", 2048, 1),
            tone("octave", 2048, 2),
        ],
    );
    c.apply(cp.boot());
    let queues = crate::audio_processing::test_util::rx(&mut c);
    for line in lines {
        run(&mut cp, &mut c, line);
    }
    (cp, c, queues)
}

fn table_garbage(queues: &mut crate::audio_processing::engine::EngineRx) -> usize {
    let mut n = 0;
    while let Ok(g) = queues.garbage.pop() {
        n += usize::from(matches!(
            g,
            crate::audio_processing::engine::Garbage::WaveTable(_)
        ));
    }
    n
}

#[test]
fn a_table_wave_plays_at_its_frequency() {
    let (_cp, mut c, _q) = table_rig(&[
        "voice organ { wave t shape=table from \"lead\" freq=1000 }",
        "start organ",
    ]);
    let x = render(&mut c, 1, 9600);
    assert!((hz_of(&x) - 1000.0).abs() < 1.0, "{}", hz_of(&x));
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!((peak - 1.0).abs() < 0.01, "{peak}");
}

#[test]
fn table_swap_retires_old_arc() {
    let (_cp, mut c, mut q) = table_rig(&[
        "voice organ { wave t shape=table from \"lead\" }",
        "start organ",
    ]);
    render(&mut c, 1, 100);
    assert_eq!(table_garbage(&mut q), 0);
    // a new source: the old table leaves through the garbage queue
    let (_cp2, mut c2, mut q2) = table_rig(&[
        "voice organ { wave t shape=table from \"lead\" }",
        "start organ",
        "organ.t from \"octave\"",
    ]);
    assert_eq!(table_garbage(&mut q2), 1);
    // the sound is the new table's: twice the pitch of the old one
    let x = render(&mut c2, 1, 9600);
    assert!((hz_of(&x) - 440.0).abs() < 1.0, "{}", hz_of(&x));
    // a frames change and a removal retire one each
    let (_cp3, _c3, mut q3) = table_rig(&[
        "voice organ { wave t shape=table from \"lead\"; wave u freq=100 }",
        "organ.t frames=512",
        "organ.u shape=saw",
        "remove organ.t",
    ]);
    assert_eq!(table_garbage(&mut q3), 2);
}

#[test]
fn buffer_table_reads_current_take() {
    sample_rate::set(48_000);
    let (mut cp, mut c) = setup(1, vec![tone("src", 2048, 2)]);
    c.apply(cp.boot());
    let mut q = crate::audio_processing::test_util::rx(&mut c);
    for line in [
        "voice src from \"src\" on 1000ms",
        "buffer b from=src length=2048f",
    ] {
        run(&mut cp, &mut c, line);
    }
    // nothing recorded yet: nothing to cut
    assert_eq!(
        cp.check_text("t", "voice organ { wave t shape=table from=b }")
            .unwrap()[0]
            .code,
        Code::EmptySource
    );
    run(&mut cp, &mut c, "start src");
    run(&mut cp, &mut c, "start b");
    render(&mut c, 1, 2048);
    for event in q.events() {
        if let crate::audio_processing::engine::Event::BufferEnded { slot, take, .. } = event {
            cp.engine_state.note_buffer_ended(slot, take);
        }
    }
    // the take is two cycles: cut in two, the table has two cycles of one
    // sine each; played at 440 Hz it is a 440 Hz sine
    for line in [
        "stop src",
        "voice organ { wave t shape=table from=b slices=2 freq=440 }",
        "start organ",
    ] {
        run(&mut cp, &mut c, line);
    }
    let x = render(&mut c, 1, 9600);
    assert!((hz_of(&x) - 440.0).abs() < 1.0, "{}", hz_of(&x));
    // restating from=b reads the buffer again, and says it built one
    let cmds = batch_cmds(lok(&mut cp, "organ.t from=b"));
    assert_eq!(table_ops(cmds), 1);
    // removing the buffer leaves the table playing
    run(&mut cp, &mut c, "remove b");
    let x = render(&mut c, 1, 9600);
    assert!((hz_of(&x) - 440.0).abs() < 1.0, "{}", hz_of(&x));
    assert_eq!(waves(&cp, "organ")[0].from, Some(WaveFrom::Gone));
}

#[test]
fn pos_glides_and_moves_a_table_wave_between_cycles() {
    // the track's first half is one sine cycle a slice, the second two:
    // pos=0 is 440 Hz at this frequency, pos=1 is 880
    sample_rate::set(48_000);
    let mut samples = tone("a", 2048, 1).samples;
    samples.extend(tone("b", 2048, 2).samples);
    let both =
        crate::file_parsing::decode_helpers::AudioFile::new("both", "wav", 48_000, 1, 16, samples);
    let (mut cp, mut c) = setup(1, vec![both]);
    c.apply(cp.boot());
    for line in [
        "voice organ { wave t shape=table from \"both\" slices=2 freq=440 }",
        "start organ",
    ] {
        run(&mut cp, &mut c, line);
    }
    let x = render(&mut c, 1, 9600);
    assert!((hz_of(&x) - 440.0).abs() < 1.0, "{}", hz_of(&x));
    run(&mut cp, &mut c, "organ.t pos=1");
    let x = render(&mut c, 1, 9600);
    assert!((hz_of(&x) - 880.0).abs() < 2.0, "{}", hz_of(&x));
}

#[test]
fn state_replays_after_a_table_lost_its_source_with_dependents() {
    let mut cp = table_session();
    lok(&mut cp, "kick { wave t shape=table from \"lead\" }");
    lok(&mut cp, "kick { wave s freq=110 }");
    lok(&mut cp, "kick { lfo sw to=t.pos depth=0.5 freq=2hz }");
    lok(&mut cp, "kick { env en to=t.gain decay=100ms }");
    lok(&mut cp, "kick { lfo keep to=s.freq depth=0.5 freq=2hz }");
    lok(&mut cp, "stop kick.t");
    lok(&mut cp, "stop kick.sw");
    lok(&mut cp, "remove \"lead\"");
    let text = cp.state(None).unwrap();
    // nothing that names the hidden wave is a command; what stays is
    for gone in ["lfo sw", "env en", "stop kick.t", "stop kick.sw", "to=t."] {
        assert!(
            text.lines()
                .filter(|l| l.contains(gone))
                .all(|l| l.trim_start().starts_with('#')),
            "{gone}: {text}"
        );
    }
    assert!(text.contains("# stop kick.t was dropped"), "{text}");
    assert!(text.contains("# sw was dropped"), "{text}");
    assert!(text.contains("# en was dropped"), "{text}");
    assert!(text.contains("lfo keep to=s.freq"), "{text}");
    sample_rate::set(48_000);
    let (mut fresh, _) = setup(1, vec![tone("pad", 8192, 4)]);
    fresh.boot();
    lok(&mut fresh, &text);
    // one wave by itself says its own note only
    let one = cp.state(Some("kick.t")).unwrap();
    assert_eq!(
        one,
        "# wave kick.t was cut from a track or buffer that was removed"
    );
}

#[test]
fn state_replays_a_waves_only_voice_whose_waves_are_all_gone() {
    let mut cp = table_session();
    lok(
        &mut cp,
        "voice organ { wave only shape=table from \"lead\" }",
    );
    lok(&mut cp, "organ { lfo w to=only.gain depth=0.5 freq=2hz }");
    lok(&mut cp, "organ { seq q 1 2 3 4 }");
    lok(&mut cp, "stop organ.only");
    lok(&mut cp, "remove \"lead\"");
    let text = cp.state(None).unwrap();
    assert!(
        text.contains("# voice organ plays a table cut from a track or buffer that was removed"),
        "{text}"
    );
    assert!(
        text.contains("lfo w") && text.contains("seq q") && text.contains("dropped"),
        "{text}"
    );
    for l in text
        .lines()
        .filter(|l| l.contains("organ") || l.contains("lfo w"))
    {
        assert!(l.trim_start().starts_with('#'), "{l}\n{text}");
    }
    sample_rate::set(48_000);
    let (mut fresh, _) = setup(1, vec![tone("pad", 8192, 4)]);
    fresh.boot();
    lok(&mut fresh, &text);
    // its wave by itself is its note, not an empty first part
    let one = cp.state(Some("organ.only")).unwrap();
    assert_eq!(
        one,
        "# wave organ.only was cut from a track or buffer that was removed"
    );
}

#[test]
fn a_table_from_a_buffer_replays_from_state_with_a_warning() {
    sample_rate::set(48_000);
    let (mut cp, mut c) = setup(1, vec![tone("src", 2048, 2)]);
    c.apply(cp.boot());
    let mut q = crate::audio_processing::test_util::rx(&mut c);
    for line in [
        "voice src from \"src\" on 1000ms",
        "buffer b from=src length=2048f",
        "start src",
        "start b",
    ] {
        run(&mut cp, &mut c, line);
    }
    render(&mut c, 1, 2048);
    for event in q.events() {
        if let crate::audio_processing::engine::Event::BufferEnded { slot, take, .. } = event {
            cp.engine_state.note_buffer_ended(slot, take);
        }
    }
    run(&mut cp, &mut c, "stop src");
    run(
        &mut cp,
        &mut c,
        "voice organ { wave t shape=table from=b slices=2 freq=440 }",
    );
    let text = cp.state(None).unwrap();
    assert!(text.contains("shape=table from b slices=2"), "{text}");
    // the new session has the same buffer, empty: the script replays, with
    // the warning and no error
    sample_rate::set(48_000);
    let (mut fresh, mut fc) = setup(1, vec![tone("src", 2048, 2)]);
    fc.apply(fresh.boot());
    let mut fq = crate::audio_processing::test_util::rx(&mut fc);
    let warns = fresh.check_text("replay", &text).unwrap();
    assert!(
        warns.iter().any(|w| w.code == Code::EmptySource
            && w.message == "b is empty: t stays silent until from=b is restated"),
        "{warns:?}"
    );
    let cmd = lok(&mut fresh, &text);
    fc.apply(cmd);
    let w = &waves(&fresh, "organ")[0];
    assert!(matches!(w.from, Some(WaveFrom::Buffer(_))));
    // silent until it is restated after a recording
    run(&mut fresh, &mut fc, "start organ");
    assert!(render(&mut fc, 1, 2000).iter().all(|v| *v == 0.0));
    for line in ["start src", "start b"] {
        run(&mut fresh, &mut fc, line);
    }
    render(&mut fc, 1, 2048);
    for event in fq.events() {
        if let crate::audio_processing::engine::Event::BufferEnded { slot, take, .. } = event {
            fresh.engine_state.note_buffer_ended(slot, take);
        }
    }
    let cmds = batch_cmds(lok(&mut fresh, "organ.t from=b"));
    let built = cmds
        .iter()
        .filter(|c| matches!(c, Command::Wave(a) if matches!(a.op, WaveOp::Table(_))))
        .count();
    assert_eq!(built, 1);
    for cmd in cmds {
        fc.apply(cmd);
    }
    let x = render(&mut fc, 1, 9600);
    assert!((hz_of(&x) - 440.0).abs() < 1.0, "{}", hz_of(&x));
}

#[test]
fn startup_tracks_are_shared_between_the_mirror_and_the_engine() {
    let es = EngineState::new(vec![tone("zed", 64, 1), tone("abc", 32, 1)], 1);
    let engine = Conductor::prepare_shared(1, es.shared_tracks());
    for (name, id) in [("abc", 0usize), ("zed", 1)] {
        let mirror = &es.tracks[name];
        assert_eq!(mirror.id.0 as usize, id);
        let held = engine.tracks.get(id).expect("the engine has the track");
        // one allocation, not two copies
        assert!(std::sync::Arc::ptr_eq(&held.samples, &mirror.samples));
        assert_eq!(
            (held.channels, held.frames),
            (mirror.channels, mirror.frames)
        );
    }
}

// ---- WAVc: FM ----

fn fm_session() -> CmdProcessor {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice pad { wave a; wave b ratio=2 out=no; wave c }",
    );
    cp
}

#[test]
fn fm_settings_lower_to_ops() {
    let mut cp = lang_processor();
    let cmds = batch_cmds(lok(
        &mut cp,
        "voice pad { wave m ratio=3.5 out=no; wave c fm=m index=4 }",
    ));
    let ops = wave_ops(cmds);
    // m: insert, ratio mode, out=no; c: insert (index 4 in its params), fm
    assert!(matches!(ops[1], WaveOp::RatioMode(true)), "{}", ops.len());
    assert!(matches!(ops[2], WaveOp::Out(false)));
    assert!(matches!(ops[4], WaveOp::Fm(1)));
    let w = waves(&cp, "pad");
    assert_eq!(w[0].params[WaveParam::Ratio.index()], 3.5);
    assert!(w[0].ratio_mode && !w[0].out);
    assert_eq!(w[1].fm, 1);
    assert_eq!(w[1].params[WaveParam::Index.index()], 4.0);
    // restating: fm=none, out=yes, index with a glide
    let cmds = batch_cmds(lok(&mut cp, "pad.c fm=none"));
    assert!(matches!(wave_ops(cmds)[0], WaveOp::Fm(0)));
    lok(&mut cp, "pad.c fm=m index=1 over 2");
    lok(&mut cp, "pad.m out=yes");
    let w = waves(&cp, "pad");
    assert!(w[0].out && w[1].fm == 1);
    // the same again says so
    assert!(
        local(&mut cp, "pad.m out=yes").contains("W101"),
        "nothing changed"
    );
    // out of range
    assert_eq!(codes(&mut cp, "pad.c index=40"), [Code::OutOfRange]);
    assert_eq!(codes(&mut cp, "pad.c ratio=100"), [Code::OutOfRange]);
    assert_eq!(codes(&mut cp, "pad.c ratio=zero"), [Code::WrongType]);
}

#[test]
fn ratio_and_freq_are_one_or_the_other() {
    let mut cp = fm_session();
    // both in one statement
    let e = lerr(&mut cp, "pad.a freq=300 ratio=2");
    assert_eq!(e[0].code, Code::WrongType);
    assert!(e[0].message.contains("freq or ratio, not both"), "{e:?}");
    // ratio puts the wave in ratio mode, freq puts it back, ratio=none too
    lok(&mut cp, "pad.a ratio=2");
    assert!(waves(&cp, "pad")[0].ratio_mode);
    lok(&mut cp, "pad.a freq=330");
    assert!(!waves(&cp, "pad")[0].ratio_mode);
    lok(&mut cp, "pad.a ratio=3");
    lok(&mut cp, "pad.a ratio=none");
    assert!(!waves(&cp, "pad")[0].ratio_mode);
    // `ratio=1` is the default value but still the mode
    lok(&mut cp, "pad.a ratio=1");
    assert!(waves(&cp, "pad")[0].ratio_mode);
}

#[test]
fn fm_cycle_is_e412() {
    let mut cp = fm_session();
    lok(&mut cp, "pad.a fm=b");
    let e = lerr(&mut cp, "pad.b fm=a");
    assert_eq!(e[0].code, Code::Cycle);
    assert_eq!(e[0].code.as_str(), "E412");
    assert!(e[0].message.contains("b.fm=a would make a loop"), "{e:?}");
    // itself, and a longer loop
    assert_eq!(codes(&mut cp, "pad.c fm=c"), [Code::Cycle]);
    lok(&mut cp, "pad.b fm=c");
    assert_eq!(codes(&mut cp, "pad.c fm=a"), [Code::Cycle]);
    // a new wave naming itself
    assert_eq!(codes(&mut cp, "pad { wave d fm=d }"), [Code::Cycle]);
    // nothing changed for the refusals
    let w = waves(&cp, "pad");
    assert_eq!((w[0].fm, w[1].fm, w[2].fm), (2, 4, 0));
}

#[test]
fn fm_unknown_wave_is_e405() {
    let mut cp = fm_session();
    let e = lerr(&mut cp, "pad.a fm=nope");
    assert_eq!(e[0].code, Code::MissingItem);
    assert!(e[0].message.contains("pad has no wave nope"), "{e:?}");
    // a wave written later in the block isn't there yet
    assert_eq!(
        codes(&mut cp, "voice q { wave x fm=y; wave y }"),
        [Code::MissingItem]
    );
    assert!(cp.engine_state.voice_id("q").is_err());
}

#[test]
fn state_round_trips_fm_voice() {
    let mut cp = lang_processor();
    // ids 0, 1, 2: a follows b follows c, so the evaluation order is the
    // reverse of the ids
    lok(
        &mut cp,
        "voice pad { wave a shape=saw; wave b ratio=2.5 out=no; wave c freq=110 gain=0.5 out=no }",
    );
    lok(&mut cp, "pad.a fm=b index=3");
    lok(&mut cp, "pad.b fm=c index=0.5");
    let text = cp.state(None).unwrap();
    let line = text.lines().find(|l| l.starts_with("voice pad")).unwrap();
    assert_eq!(
        line,
        "voice pad { wave c freq=110 gain=0.5 out=no; wave b ratio=2.5 fm=c index=0.5 out=no; wave a shape=saw fm=b index=3 }",
        "{text}"
    );
    // the text recreates the session, and feeding it back changes nothing
    let mut fresh = lang_processor();
    lok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
    for line in local(&mut fresh, &text).lines() {
        assert!(line.contains("W101"), "{line}");
    }
    // and the replayed voice has the same links (by name)
    let names = |cp: &CmdProcessor| {
        waves(cp, "pad")
            .iter()
            .map(|w| {
                let m = waves(cp, "pad")
                    .iter()
                    .filter(|x| w.fm >> x.id & 1 == 1)
                    .map(|x| x.name.clone())
                    .collect::<Vec<_>>();
                (w.name.clone(), m, w.out, w.ratio_mode)
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(names(&fresh), names(&cp));
    // one wave by itself
    let one = cp.state(Some("pad.b")).unwrap();
    assert!(one.contains("wave b ratio=2.5 fm=c"), "{one}");
}

#[test]
fn lfo_targets_index_and_ratio() {
    let mut cp = fm_session();
    for (line, id, param) in [
        (
            "pad { lfo wob to=a.index depth=1 freq=1hz }",
            0,
            WaveParam::Index,
        ),
        (
            "pad { lfo wib to=b.ratio depth=0.1 freq=1hz }",
            1,
            WaveParam::Ratio,
        ),
        (
            "pad { env sweep to=a.index decay=100ms sustain=0 }",
            0,
            WaveParam::Index,
        ),
    ] {
        let cmds = batch_cmds(lok(&mut cp, line));
        let target = match &cmds[0] {
            Command::Lfo(a) => a.target,
            Command::Env(a) => a.target,
            _ => panic!("not an lfo or env"),
        };
        assert_eq!(
            target,
            LfoTarget::Wave {
                id: id as u8,
                param: param.index() as u8
            }
        );
    }
}

// ---- WAVc: end to end ----

#[test]
fn ratio_follows_pitch() {
    // ratio=2 of the voice's note: 440 Hz, and an octave above a pitch of 12st
    for (line, want) in [
        ("voice w { wave s1 ratio=2 }", 440.0),
        ("voice w pitch=12st { wave s1 ratio=2 }", 880.0),
        ("voice w pitch=12st { wave s1 ratio=2 freq=300 }", 0.0),
    ] {
        if want == 0.0 {
            let mut cp = lang_processor();
            assert_eq!(codes(&mut cp, line), [Code::WrongType]);
            continue;
        }
        let (_cp, mut c) = rig(&[line, "start w"]);
        let x = render(&mut c, 1, 19_200);
        assert!(
            (hz_of(&x) - want).abs() < want * 0.003,
            "{line}: {}",
            hz_of(&x)
        );
    }
    // freq= after ratio= goes back to an absolute frequency
    let (_cp, mut c) = rig(&["voice w { wave s1 ratio=2 }", "w.s1 freq=1000", "start w"]);
    let x = render(&mut c, 1, 9600);
    assert!((hz_of(&x) - 1000.0).abs() < 2.0, "{}", hz_of(&x));
}

#[test]
fn an_fm_voice_plays_and_a_modulator_is_not_heard() {
    let (_cp, mut c) = rig(&[
        "voice bell { wave m ratio=3.5 out=no; wave c fm=m index=4 }",
        "start bell",
    ]);
    let x = render(&mut c, 1, 9600);
    assert!(x.iter().all(|v| v.is_finite() && v.abs() <= 1.0 + 1e-4));
    assert!(x.iter().fold(0.0f32, |m, v| m.max(v.abs())) > 0.9);
    // the modulator alone is silence
    let (_cp, mut c) = rig(&[
        "voice bell { wave m ratio=3.5 out=no; wave c fm=m index=4 }",
        "stop bell.c",
        "start bell",
    ]);
    assert!(render(&mut c, 1, 2400).iter().all(|v| *v == 0.0));
}

#[test]
fn stop_modulator_plays_plain_carrier() {
    let line = "voice bell { wave m ratio=3.5 out=no; wave c freq=500 fm=m index=4 }";
    let (_cp, mut c) = rig(&[line, "stop bell.m", "start bell"]);
    // plain: a sine at 500 Hz, once the step the modulator's going left is by
    render(&mut c, 1, 100);
    let x = render(&mut c, 1, 9600);
    assert!((hz_of(&x) - 500.0).abs() < 1.0, "{}", hz_of(&x));
    assert!(x.iter().fold(0.0f32, |m, v| m.max(v.abs())) > 0.99);
}

#[test]
fn index_glide_and_lfo_on_ratio_play() {
    let (_cp, mut c) = rig(&[
        "voice bell { wave m ratio=3.5 out=no; wave c fm=m index=4 }",
        "bell.c index=0 over 0.1",
        "bell { lfo wob to=m.ratio depth=0.1 freq=1hz }",
        "start bell",
    ]);
    let x = render(&mut c, 1, 48_000);
    assert!(x.iter().all(|v| v.is_finite()));
    // the index has glided to nothing: a plain sine at 220 Hz at the end
    let tail = &x[40_000..];
    assert!((hz_of(tail) - 220.0).abs() < 2.0, "{}", hz_of(tail));
}

// ---- WAVc round 2 ----

#[test]
fn a_pending_modulator_is_e411_and_a_dropped_one_leaves_no_link() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice organ { wave s0 }");
    lok(&mut cp, "start organ");
    lok(
        &mut cp,
        "organ.s0 gain=0.5 over 4b thenif organ { wave late }",
    );
    assert_eq!(codes(&mut cp, "organ.s0 fm=late"), [Code::NotCreatedYet]);
}

#[test]
fn forgetting_a_staged_wave_clears_the_links_to_it() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice organ { wave s0; wave s1 }");
    lok(&mut cp, "organ.s0 fm=s1");
    let v = cp.engine_state.voice_id("organ").unwrap();
    let s1 = cp.engine_state.wave_list(v)[1].id;
    cp.engine_state
        .forget_object(AnyId::Wave { voice: v, id: s1 });
    let w = cp.engine_state.wave_list(v);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].fm, 0);
}

#[test]
fn a_loop_through_a_waiting_line_never_reaches_the_engine() {
    let (mut cp, mut c) = rig(&[
        "voice pad { wave a; wave b }",
        "start pad",
        "pad.a fm=b at 1",
    ]);
    run(&mut cp, &mut c, "pad.a fm=none");
    run(&mut cp, &mut c, "pad.b fm=a");
    // the waiting line runs at beat 1: no panic, no loop
    let x = render(&mut c, 1, 120_000);
    assert!(x.iter().all(|v| v.is_finite()));
    let v = cp.engine_state.voice_id("pad").unwrap();
    let engine = &c.voices.get(v.index()).unwrap().waves;
    assert_eq!(engine.modulators_of(0), 0);
    assert_eq!(engine.modulators_of(1), 1);
    let w = waves(&cp, "pad");
    assert_eq!((w[0].fm, w[1].fm), (0, 1));
}

// ---- WAVd: removing modulators, ratio text, feedback, fm sets ----

fn wave_state_line(cp: &CmdProcessor, wave: &str) -> String {
    let text = cp.state(None).unwrap();
    let key = format!("wave {wave} ");
    let alone = format!("wave {wave}");
    let from = text
        .find(&key)
        .or_else(|| text.find(&format!("{alone};")))
        .or_else(|| text.find(&format!("{alone} }}")))
        .unwrap_or_else(|| panic!("{text}"));
    text[from..]
        .split([';', '}'])
        .next()
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn remove_modulator_is_allowed() {
    let mut cp = fm_session();
    lok(&mut cp, "pad.a fm=b");
    // no E407 any more: the carrier plays plain
    lok(&mut cp, "remove pad.b");
    let w = waves(&cp, "pad");
    assert_eq!(w.len(), 2);
    assert_eq!(w[0].fm, 0);
    let text = cp.state(None).unwrap();
    assert!(!text.contains("fm="), "{text}");
    let mut fresh = lang_processor();
    lok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
}

#[test]
fn remove_one_of_two_modulators_keeps_the_other() {
    let mut cp = fm_session();
    lok(&mut cp, "pad.a fm=b+c");
    assert_eq!(waves(&cp, "pad")[0].fm, 0b110);
    lok(&mut cp, "remove pad.b");
    assert_eq!(waves(&cp, "pad")[0].fm, 0b100);
    assert!(wave_state_line(&cp, "a").contains("fm=c"));
    // and the only one left: plain
    lok(&mut cp, "remove pad.c");
    assert_eq!(waves(&cp, "pad")[0].fm, 0);
}

#[test]
fn remove_only_modulator_plays_plain() {
    let (mut cp, mut c) = rig(&["voice pad { wave a; wave b ratio=2 out=no }", "start pad"]);
    run(&mut cp, &mut c, "pad.a fm=b index=3");
    run(&mut cp, &mut c, "remove pad.b");
    let v = cp.engine_state.voice_id("pad").unwrap();
    assert_eq!(c.voices.get(v.index()).unwrap().waves.modulators_of(0), 0);
    let x = render(&mut c, 1, 24_000);
    assert!(x.iter().all(|s| s.is_finite()));
    assert!(x.iter().any(|s| s.abs() > 0.01));
}

#[test]
fn fm_set_cycle_is_e412() {
    let mut cp = fm_session();
    lok(&mut cp, "pad.c fm=a");
    assert_eq!(codes(&mut cp, "pad.a fm=b+c"), [Code::Cycle]);
    assert_eq!(waves(&cp, "pad")[0].fm, 0);
}

#[test]
fn fm_self_hint_names_fb() {
    let mut cp = fm_session();
    let e = lerr(&mut cp, "pad.a fm=a");
    assert_eq!(e[0].code, Code::Cycle);
    assert!(
        e[0].suggestion
            .as_deref()
            .is_some_and(|s| s.contains("fb=")),
        "{e:?}"
    );
}

#[test]
fn fm_duplicate_is_e402() {
    let mut cp = fm_session();
    assert_eq!(codes(&mut cp, "pad.a fm=b+b"), [Code::WrongType]);
}

#[test]
fn words_value_refused_outside_fm() {
    let mut cp = fm_session();
    assert_eq!(codes(&mut cp, "pad.a shape=sine+saw"), [Code::WrongType]);
    assert_eq!(codes(&mut cp, "pad.a freq=b+c"), [Code::WrongType]);
}

#[test]
fn fm_words_need_touching() {
    let mut cp = fm_session();
    assert!(!lerr(&mut cp, "pad.a fm=b + c").is_empty());
    assert!(!lerr(&mut cp, "pad.a fm=b +c").is_empty());
    assert!(!lerr(&mut cp, "pad.a fm=b+").is_empty());
    assert_eq!(waves(&cp, "pad")[0].fm, 0);
    lok(&mut cp, "pad.a fm=b+c");
}

#[test]
fn ratio_prints_as_typed() {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice pad { wave a ratio=3/2; wave b ratio=1.5; wave c ratio=(7/4) }",
    );
    assert_eq!(wave_state_line(&cp, "a"), "wave a ratio=3/2");
    assert_eq!(wave_state_line(&cp, "b"), "wave b ratio=1.5");
    assert_eq!(wave_state_line(&cp, "c"), "wave c ratio=(7/4)");
    assert_eq!(waves(&cp, "pad")[0].params[WaveParam::Ratio.index()], 1.5);
    // and it replays
    let text = cp.state(None).unwrap();
    let mut fresh = lang_processor();
    lok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
    // restated, the new text is kept
    lok(&mut cp, "pad.a ratio=2*3");
    assert_eq!(wave_state_line(&cp, "a"), "wave a ratio=2*3");
}

#[test]
fn ratio_with_var_prints_the_number() {
    let mut cp = lang_processor();
    local(&mut cp, "let r = 2");
    lok(&mut cp, "voice pad { wave a ratio=$r/4 }");
    assert_eq!(wave_state_line(&cp, "a"), "wave a ratio=0.5");
}

#[test]
fn ratio_glide_keeps_target_text() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice pad { wave a ratio=1 }");
    lok(&mut cp, "pad.a ratio=5/4 over 2");
    assert_eq!(wave_state_line(&cp, "a"), "wave a ratio=5/4");
}

#[test]
fn freq_clears_ratio_text() {
    let mut cp = lang_processor();
    lok(&mut cp, "voice pad { wave a ratio=3/2 }");
    assert_eq!(waves(&cp, "pad")[0].ratio_text.as_deref(), Some("3/2"));
    lok(&mut cp, "pad.a freq=440");
    assert_eq!(waves(&cp, "pad")[0].ratio_text, None);
    lok(&mut cp, "pad.a ratio=3/2");
    lok(&mut cp, "pad.a ratio=none");
    assert_eq!(waves(&cp, "pad")[0].ratio_text, None);
    assert!(!wave_state_line(&cp, "a").contains("ratio"));
}

#[test]
fn state_round_trips_fm_set_and_fb() {
    let mut cp = lang_processor();
    lok(
        &mut cp,
        "voice ep { wave a ratio=1 out=no; wave b ratio=14 out=no gain=0.3; wave c fm=a+b index=2 fb=0.5 }",
    );
    let text = cp.state(None).unwrap();
    let line = text.lines().find(|l| l.starts_with("voice ep")).unwrap();
    assert!(line.contains("wave c fm=a+b index=2 fb=0.5"), "{text}");
    let mut fresh = lang_processor();
    lok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
    assert_eq!(waves(&fresh, "ep")[2].fm, 0b011);
    assert_eq!(waves(&fresh, "ep")[2].params[WaveParam::Fb.index()], 0.5);
}

#[test]
fn lfo_targets_fb() {
    let mut cp = fm_session();
    let cmds = batch_cmds(lok(&mut cp, "pad { lfo w to=c.fb depth=0.3 freq=1hz }"));
    let Command::Lfo(a) = &cmds[0] else {
        panic!("not an lfo")
    };
    assert_eq!(
        a.target,
        LfoTarget::Wave {
            id: 2,
            param: WaveParam::Fb.index() as u8
        }
    );
}

// ---- WAVd round 2: a waiting fm= line holds its modulators ----

// the engine's report that the one waiting line ran
fn line_ran(cp: &mut CmdProcessor) {
    let seq = cp.engine_state.waiting[0].seq_no;
    cp.engine_state.note_schedule_fired(seq, 1, true);
}

fn engine_fm(cp: &CmdProcessor, c: &Conductor, voice: &str, id: u8) -> u8 {
    let v = cp.engine_state.voice_id(voice).unwrap();
    c.voices.get(v.index()).unwrap().waves.modulators_of(id)
}

#[test]
fn a_modulator_named_by_a_waiting_fm_line_cannot_go() {
    // repro A: a set with a waiting line
    let (mut cp, mut c) = rig(&[
        "voice pad { wave a; wave b; wave c }",
        "start pad",
        "pad.a fm=b+c at 1",
    ]);
    let e = lerr(&mut cp, "remove pad.c");
    assert_eq!(e[0].code, Code::InUse);
    assert!(
        e[0].message.contains("pad.c is named by a waiting fm="),
        "{e:?}"
    );
    assert_eq!(waves(&cp, "pad").len(), 3);
    // the line runs: engine and mirror agree
    let x = render(&mut c, 1, 120_000);
    assert!(x.iter().all(|v| v.is_finite()));
    line_ran(&mut cp);
    assert_eq!(engine_fm(&cp, &c, "pad", 0), 0b110);
    assert_eq!(waves(&cp, "pad")[0].fm, 0b110);
    // now it may go, and both drop the bit
    run(&mut cp, &mut c, "remove pad.c");
    assert_eq!(engine_fm(&cp, &c, "pad", 0), 0b010);
    assert_eq!(waves(&cp, "pad")[0].fm, 0b010);
}

#[test]
fn a_waiting_fm_line_that_replaces_a_link_agrees_after_it_runs() {
    // repro B
    let (mut cp, mut c) = rig(&[
        "voice pad { wave a; wave b; wave x }",
        "start pad",
        "pad.a fm=x",
        "pad.a fm=b at 1",
    ]);
    assert_eq!(codes(&mut cp, "remove pad.b"), [Code::InUse]);
    // (a wave the line does not name can go: x)
    render(&mut c, 1, 120_000);
    line_ran(&mut cp);
    assert_eq!(engine_fm(&cp, &c, "pad", 0), 0b010);
    assert_eq!(waves(&cp, "pad")[0].fm, 0b010);
    // the mirror and the engine see the same graph: x following a is fine
    // in both
    run(&mut cp, &mut c, "pad.x fm=a");
    assert_eq!(engine_fm(&cp, &c, "pad", 2), 0b001);
    assert_eq!(waves(&cp, "pad")[2].fm, 0b001);
}

#[test]
fn a_waiting_batch_with_an_fm_holds_its_modulators() {
    let (mut cp, mut c) = rig(&[
        "voice pad { wave a; wave b; wave c }",
        "start pad",
        "pad.a fm=b+c and pad.b gain=0.5 at 1",
    ]);
    let e = lerr(&mut cp, "remove pad.c");
    assert_eq!(e[0].code, Code::InUse);
    assert!(
        e[0].message.contains("pad.c is named by a waiting fm="),
        "{e:?}"
    );
    render(&mut c, 1, 120_000);
    line_ran(&mut cp);
    run(&mut cp, &mut c, "remove pad.c");
    assert_eq!(engine_fm(&cp, &c, "pad", 0), 0b010);
    assert_eq!(waves(&cp, "pad")[0].fm, 0b010);
}
