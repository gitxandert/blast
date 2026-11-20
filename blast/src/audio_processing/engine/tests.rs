use std::sync::Arc;

use super::*;
use crate::audio_processing::test_util::{
    conductor, pcm, render, render_i16, run, rx, setup, setup_with, vid,
};
use crate::audio_processing::{
    blast_rand::X128P,
    blast_time::{blast_time::TempoUnit, sample_rate},
};

// interleaved track of `frames` frames where every sample is `val`
fn track(name: &str, channels: u32, frames: usize, val: i16) -> AudioFile {
    AudioFile::new(
        name,
        "wav",
        48000,
        channels,
        16,
        pcm(&vec![val; frames * channels as usize]),
    )
}

// a new tempo of `interval` samples in slot `id`
fn own(id: u32, interval: f32) -> TempoUse {
    TempoUse::Own(TempoInit {
        id: TempoId(id),
        unit: TempoUnit::Samples,
        interval,
    })
}

fn load(c: &mut Conductor, voice: u32, track: u32, tempo: TempoUse, adopts: bool) {
    c.apply(Command::Load(LoadArgs {
        voice: VoiceId(voice),
        source: LoadSource::Track(TrackId(track)),
        tempo,
        adopts,
    }));
}

// Conductor with one loaded (not started) Voice on its own tempo 0
fn one_voice(out_channels: usize, af: AudioFile) -> Conductor {
    sample_rate::set(48000);
    let mut c = conductor(out_channels, vec![af]);
    load(&mut c, 0, 0, own(0, 100.0), false);
    c
}

fn start_voice(c: &mut Conductor, id: u32) {
    c.apply(Command::Start(StartArgs {
        target: Target::Voice(VoiceId(id)),
    }));
}

fn voice(c: &Conductor, id: u32) -> &Voice {
    c.voices.get(id as usize).unwrap()
}

fn tempo(c: &Conductor, id: TempoId) -> &TempoState {
    &c.tempos.get(id.index()).unwrap().state
}

// the tempo voice `id` uses
fn voice_tempo(c: &Conductor, id: u32) -> &TempoState {
    tempo(c, voice(c, id).state.tempo)
}

fn voice_tempo_ticks_once_per_frame(channels: usize) {
    let mut c = one_voice(channels, track("t", channels as u32, 2000, 100));
    start_voice(&mut c, 0);

    render(&mut c, channels, 1000);

    assert_eq!(voice_tempo(&c, 0).current, 1000);
    assert_eq!(voice(&c, 0).state.inst[0].position, 1000.0);
}

#[test]
fn voice_tempo_ticks_once_per_frame_stereo() {
    voice_tempo_ticks_once_per_frame(2);
}

#[test]
fn voice_tempo_ticks_once_per_frame_mono() {
    voice_tempo_ticks_once_per_frame(1);
}

#[test]
fn group_tempo_ticks_once_per_frame() {
    sample_rate::set(48000);
    let mut c = conductor(2, vec![track("t", 2, 2000, 100)]);
    // a voice loaded without -t, so it adopts the group's tempo
    load(&mut c, 0, 0, own(0, 100.0), true);
    c.apply(Command::Group(GroupArgs {
        group: GroupId(0),
        tempo: own(1, 100.0),
        members: vec![VoiceId(0)],
    }));
    assert_eq!(voice(&c, 0).state.tempo, TempoId(1));
    assert!(c.tempos.get(0).is_none());
    c.apply(Command::Start(StartArgs {
        target: Target::Group(GroupId(0)),
    }));

    render(&mut c, 2, 1000);

    let g = c.groups.get(0).unwrap();
    assert_eq!(tempo(&c, g.state.tempo).current, 1000);
    assert_eq!(voice(&c, 0).state.inst[0].position, 1000.0);
}

#[test]
fn voice_start_pause_resume_stop() {
    let mut c = one_voice(2, track("t", 2, 2000, 100));
    assert!(!voice(&c, 0).state.active);

    start_voice(&mut c, 0);
    assert!(voice(&c, 0).state.active);
    assert!(voice_tempo(&c, 0).active);
    render(&mut c, 2, 10);
    assert_eq!(voice(&c, 0).state.inst[0].position, 10.0);

    c.apply(Command::Pause(PauseArgs {
        target: Target::Voice(VoiceId(0)),
    }));
    assert!(!voice(&c, 0).state.active);
    // paused Voices don't advance or output
    let out = render_i16(&mut c, 2, 10);
    assert_eq!(voice(&c, 0).state.inst[0].position, 10.0);
    assert!(out.iter().all(|&s| s == 0));

    c.apply(Command::Resume(ResumeArgs {
        target: Target::Voice(VoiceId(0)),
    }));
    assert!(voice(&c, 0).state.active);
    render(&mut c, 2, 10);
    assert_eq!(voice(&c, 0).state.inst[0].position, 20.0);

    c.apply(Command::Stop(StopArgs {
        target: Target::Voice(VoiceId(0)),
    }));
    assert!(!voice(&c, 0).state.active);
    assert_eq!(voice(&c, 0).state.inst[0].position, 0.0);
    assert_eq!(voice_tempo(&c, 0).current, 0);
    assert!(!voice_tempo(&c, 0).active);

    // start resets position and tempo
    start_voice(&mut c, 0);
    render(&mut c, 2, 10);
    start_voice(&mut c, 0);
    assert_eq!(voice(&c, 0).state.inst[0].position, 0.0);
    assert_eq!(voice_tempo(&c, 0).current, 0);
}

#[test]
fn mono_voice_on_stereo_advances_once_per_frame() {
    let af = AudioFile::new("m", "wav", 48000, 1, 16, pcm(&(0..100).collect::<Vec<_>>()));
    let mut c = one_voice(2, af);
    start_voice(&mut c, 0);

    let out = render(&mut c, 2, 10);

    // a centred mono sample goes to both channels at cos(pi/4)
    let centre = std::f32::consts::FRAC_PI_4.cos();
    for (frame, pair) in out.chunks(2).enumerate() {
        let want = f32::from(frame as i16) / 32768.0 * centre;
        assert!((pair[0] - want).abs() < 1e-6, "frame {frame}: {}", pair[0]);
        assert!((pair[1] - want).abs() < 1e-6, "frame {frame}: {}", pair[1]);
    }
    assert_eq!(voice(&c, 0).state.inst[0].position, 10.0);
}

#[test]
fn stereo_voice_on_stereo_keeps_channels() {
    // L = frame index, R = -frame index
    let samples: Vec<i16> = (0..100).flat_map(|s| [s, -s]).collect();
    let af = AudioFile::new("s", "wav", 48000, 2, 16, pcm(&samples));
    let mut c = one_voice(2, af);
    start_voice(&mut c, 0);

    let out = render_i16(&mut c, 2, 10);

    let expected: Vec<i16> = (0..10).flat_map(|s| [s, -s]).collect();
    assert_eq!(out, expected);
    assert_eq!(voice(&c, 0).state.inst[0].position, 10.0);
}

// mono [10,20,30,40,50] at 1 output channel with velocity `vel`
fn render_at_velocity(vel: f32, frames: usize) -> Vec<i16> {
    let af = AudioFile::new("v", "wav", 48000, 1, 16, pcm(&[10, 20, 30, 40, 50]));
    let mut c = one_voice(1, af);
    c.apply(Command::SetParam(SetParamArgs {
        voice: VoiceId(0),
        param: VoiceParam::Rate,
        value: vel,
        frames: 0,
    }));
    start_voice(&mut c, 0);
    render_i16(&mut c, 1, frames)
}

#[test]
fn forward_playback_plays_every_frame_then_stops() {
    assert_eq!(
        render_at_velocity(1.0, 8),
        vec![10, 20, 30, 40, 50, 0, 0, 0]
    );
}

#[test]
fn reverse_playback_plays_every_frame_then_stops() {
    assert_eq!(
        render_at_velocity(-1.0, 8),
        vec![50, 40, 30, 20, 10, 0, 0, 0]
    );
}

#[test]
fn half_speed_interpolates() {
    assert_eq!(
        render_at_velocity(0.5, 11),
        vec![10, 15, 20, 25, 30, 35, 40, 45, 50, 0, 0]
    );
}

#[test]
fn reverse_half_speed_interpolates() {
    assert_eq!(
        render_at_velocity(-0.5, 11),
        vec![50, 45, 40, 35, 30, 25, 20, 15, 10, 0, 0]
    );
}

// two started mono Voices of constant `a` and `b` on 1 output channel
fn render_two_voices(a: i16, b: i16) -> Vec<i16> {
    let mut c = two_voices(a, b);
    render_i16(&mut c, 1, 10)
}

fn two_voices(a: i16, b: i16) -> Conductor {
    sample_rate::set(48000);
    let mut c = conductor(1, vec![track("a", 1, 100, a), track("b", 1, 100, b)]);
    // tracks sort by name, so "a" is TrackId(0)
    load(&mut c, 0, 0, own(0, 100.0), false);
    load(&mut c, 1, 1, own(1, 100.0), false);
    start_voice(&mut c, 0);
    start_voice(&mut c, 1);
    c
}

#[test]
fn mix_clips_at_i16_max() {
    assert!(
        render_two_voices(30000, 30000)
            .iter()
            .all(|&s| s == i16::MAX)
    );
}

#[test]
fn mix_clips_at_i16_min() {
    assert!(
        render_two_voices(-30000, -30000)
            .iter()
            .all(|&s| s == i16::MIN)
    );
}

#[test]
fn mix_sums_small_values_exactly() {
    assert!(render_two_voices(1000, 2000).iter().all(|&s| s == 3000));
    assert!(render_two_voices(1000, -3000).iter().all(|&s| s == -2000));
}

#[test]
fn tempo_context_ticks_once_per_frame_while_active() {
    sample_rate::set(48000);
    let mut c = conductor(2, vec![track("t", 2, 2000, 100)]);
    c.apply(Command::Tc(TcArgs {
        tempo: TempoInit {
            id: TempoId(0),
            unit: TempoUnit::Samples,
            interval: 100.0,
        },
    }));
    // a Voice synced to the context shares its TempoState
    load(&mut c, 0, 0, TempoUse::Shared(TempoId(0)), false);

    // not started yet
    render(&mut c, 2, 10);
    assert_eq!(tempo(&c, TempoId(0)).current, 0);

    c.apply(Command::Start(StartArgs {
        target: Target::Tempo(TempoId(0)),
    }));
    start_voice(&mut c, 0);
    render(&mut c, 2, 100);
    assert_eq!(tempo(&c, TempoId(0)).current, 100);
    assert_eq!(voice_tempo(&c, 0).current, 100);

    c.apply(Command::Pause(PauseArgs {
        target: Target::Tempo(TempoId(0)),
    }));
    render(&mut c, 2, 50);
    assert_eq!(tempo(&c, TempoId(0)).current, 100);

    c.apply(Command::Resume(ResumeArgs {
        target: Target::Tempo(TempoId(0)),
    }));
    render(&mut c, 2, 50);
    assert_eq!(tempo(&c, TempoId(0)).current, 150);

    c.apply(Command::Stop(StopArgs {
        target: Target::Tempo(TempoId(0)),
    }));
    render(&mut c, 2, 50);
    assert_eq!(tempo(&c, TempoId(0)).current, 0);
}

// Group of two mono Voices (samples = frame index) with a Seq
// that retriggers on steps 0 and 2 of a 4-step period at s:100
fn group_with_seq(seq_tempo: TempoUse) -> Conductor {
    sample_rate::set(48000);
    let mut c = conductor(
        1,
        vec![AudioFile::new(
            "t",
            "wav",
            48000,
            1,
            16,
            pcm(&(0..1000).collect::<Vec<_>>()),
        )],
    );
    load(&mut c, 0, 0, own(0, 100.0), true);
    load(&mut c, 1, 0, own(1, 100.0), true);
    c.apply(Command::Group(GroupArgs {
        group: GroupId(0),
        tempo: own(2, 100.0),
        members: vec![VoiceId(1), VoiceId(0)],
    }));
    c.apply(Command::Seq(SeqArgs {
        proc: ProcId(0),
        owner: ProcOwner::Group(GroupId(0)),
        tempo: seq_tempo,
        period: 4,
        steps: vec![0.0, 2.0],
        chance: vec![100.0, 100.0],
        jit: vec![0.0, 0.0],
        pend: Vec::new(),
        mask: Vec::new(),
        feel: [100.0, 0.0, 0.0],
        rng: X128P::new(1),
        recv: RecvSpec::owner_trig(),
    }));
    c.apply(Command::Start(StartArgs {
        target: Target::Group(GroupId(0)),
    }));
    c
}

fn assert_group_seq_retriggers(mut c: Conductor) {
    let out = render_i16(&mut c, 1, 450);

    // both Voices restart on frames 200 and 400
    assert_eq!(out[199], 2 * 199);
    assert_eq!(out[200], 0);
    assert_eq!(out[201], 2);
    assert_eq!(out[399], 2 * 199);
    assert_eq!(out[400], 0);
    for (_, v) in c.voices.iter() {
        assert_eq!(v.state.inst[0].position, 50.0);
    }
}

#[test]
fn group_seq_with_own_tempo_retriggers_all_voices() {
    assert_group_seq_retriggers(group_with_seq(own(3, 100.0)));
}

#[test]
fn group_seq_on_group_tempo_retriggers_all_voices() {
    assert_group_seq_retriggers(group_with_seq(TempoUse::Shared(TempoId(2))));
}

#[test]
fn voice_seq_retriggers_voice() {
    let af = AudioFile::new(
        "t",
        "wav",
        48000,
        1,
        16,
        pcm(&(0..1000).collect::<Vec<_>>()),
    );
    let mut c = one_voice(1, af);
    c.apply(Command::Seq(SeqArgs {
        proc: ProcId(0),
        owner: ProcOwner::Voice(VoiceId(0)),
        tempo: own(1, 100.0),
        period: 4,
        steps: vec![0.0, 2.0],
        chance: vec![100.0, 100.0],
        jit: vec![0.0, 0.0],
        pend: Vec::new(),
        mask: Vec::new(),
        feel: [100.0, 0.0, 0.0],
        rng: X128P::new(1),
        recv: RecvSpec::owner_trig(),
    }));
    start_voice(&mut c, 0);

    let out = render_i16(&mut c, 1, 450);
    assert_eq!(out[199], 199);
    assert_eq!(out[200], 0);
    assert_eq!(out[400], 0);
    assert_eq!(voice(&c, 0).state.inst[0].position, 50.0);
}

#[test]
fn group_stop_stops_seq_tempo() {
    let mut c = group_with_seq(own(3, 100.0));
    render(&mut c, 1, 10);
    assert_eq!(tempo(&c, TempoId(3)).current, 10);

    c.apply(Command::Stop(StopArgs {
        target: Target::Group(GroupId(0)),
    }));
    assert!(!tempo(&c, TempoId(3)).active);
    assert_eq!(tempo(&c, TempoId(3)).current, 0);
}

#[test]
fn stereo_voice_mixes_its_samples() {
    let mut c = one_voice(2, track("t", 2, 100, 1234));
    start_voice(&mut c, 0);

    let out = render_i16(&mut c, 2, 50);
    assert!(out.iter().all(|&s| s == 1234));
}

// tracks and IDs

#[test]
fn tracks_are_sorted_by_name() {
    let c = conductor(
        1,
        vec![
            track("b", 1, 20, 0),
            track("a", 1, 10, 0),
            track("c", 1, 30, 0),
        ],
    );
    let frames: Vec<usize> = c.tracks.iter().map(|(_, t)| t.frames).collect();
    assert_eq!(frames, vec![10, 20, 30]);
    assert_eq!(c.tracks.get(TrackId(0).index()).unwrap().frames, 10);
}

#[test]
fn voices_share_track_samples() {
    let mut c = one_voice(1, track("t", 1, 100, 7));
    load(&mut c, 1, 0, own(1, 100.0), false);

    let shared = &c.tracks.get(0).unwrap().samples;
    assert_eq!(Arc::strong_count(shared), 3);
    assert!(Arc::ptr_eq(voice(&c, 0).track(), shared));
    assert!(Arc::ptr_eq(voice(&c, 1).track(), shared));
}

#[test]
fn mirror_and_engine_track_ids_agree() {
    // names whose HashMap order is unlikely to match their sorted order
    let tracks: Vec<AudioFile> = (0..20)
        .map(|i| track(&format!("t{}", (i * 7) % 20), 1, 10 + i, 0))
        .collect();
    let want: HashMap<String, usize> = tracks
        .iter()
        .map(|af| (af.file_name.clone(), af.samples.len()))
        .collect();
    let (mut cp, c) = setup(1, tracks);

    for (name, frames) in &want {
        let line = format!("voice {name} from \"{name}\" on 1000ms");
        let Ok(Parsed::Engine(Command::Load(args))) = cp.parse(&line) else {
            panic!("'{line}' didn't parse");
        };
        let LoadSource::Track(id) = args.source else {
            panic!("not a track");
        };
        let t = c.tracks.get(id.index()).unwrap();
        assert_eq!(t.frames, *frames, "{}", name);
    }
}

// render

#[test]
fn render_is_unclamped() {
    let mut c = two_voices(30000, 30000);
    let out = render(&mut c, 1, 10);
    let want = 2.0 * 30000.0 / 32768.0;
    assert!(out.iter().all(|&s| (s - want).abs() < 1e-6), "{:?}", out);
    assert!(out[0] > 1.83);
}

#[test]
fn render_fills_every_sample() {
    let mut empty = conductor(2, Vec::new());
    let mut buf = vec![f32::NAN; 2 * 64];
    empty.render(&mut buf, 2);
    assert!(buf.iter().all(|s| !s.is_nan()));

    let (_, mut busy) = busy_setup();
    let mut buf = vec![f32::NAN; 2 * 3000];
    busy.render(&mut buf, 2);
    assert!(buf.iter().all(|s| !s.is_nan()));
    assert!(buf.iter().any(|&s| s != 0.0));
}

// a stereo and a mono Voice, one reversed at an off-unit rate, one
// with a Seq, loud enough that their sum clips now and then
fn busy_setup() -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let stereo: Vec<i16> = (0..4000)
        .map(|i| ((i * 211) % 50000 - 25000) as i16)
        .collect();
    let mono: Vec<i16> = (0..3000)
        .map(|i| ((i * 97) % 40000 - 20000) as i16)
        .collect();
    let (mut cp, mut c) = setup(
        2,
        vec![
            AudioFile::new("a", "wav", 48000, 2, 16, pcm(&stereo)),
            AudioFile::new("b", "wav", 48000, 1, 16, pcm(&mono)),
        ],
    );
    for cmd in [
        "voice a from \"a\" on 1000ms",
        "voice b from \"b\" on 1000ms",
        "b rate=-0.75",
        "a { seq s 1 3 on 300f }",
        "start a",
        "start b",
    ] {
        run(&mut cp, &mut c, cmd);
    }
    (cp, c)
}

// E1c: events, garbage, batches

#[test]
fn quit_emits_event_after_earlier_commands() {
    let (mut cp, mut c) = setup(2, mono(&["a"]));
    let mut rx = rx(&mut c);
    let load = parsed(&mut cp, "voice a from \"a\" on 1000ms");
    let quit = parsed(&mut cp, "quit");
    c.apply(Command::Batch(BatchArgs {
        cmds: vec![load, quit],
    }));
    // the load applied before the quit's event
    assert!(c.voices.get(vid(&cp, "a").index()).is_some());
    assert_eq!(rx.events().collect::<Vec<_>>(), vec![Event::Quit]);
}

fn parsed(cp: &mut CmdProcessor, line: &str) -> Command {
    match cp.parse(line) {
        Ok(Parsed::Engine(cmd)) => cmd,
        _ => panic!("'{line}' isn't an engine command"),
    }
}

fn mono(names: &[&str]) -> Vec<AudioFile> {
    names.iter().map(|n| track(n, 1, 64, 1)).collect()
}

#[test]
fn start_on_inactive_tempo_emits_event() {
    let (mut cp, mut c) = setup(2, mono(&["a"]));
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "clock x 120b");
    run(&mut cp, &mut c, "voice a from \"a\" on x");
    let a = vid(&cp, "a");
    run(&mut cp, &mut c, "start a");
    let events: Vec<Event> = rx.events().collect();
    assert_eq!(
        events,
        vec![Event::TempoInactive {
            owner: TempoOwner::Voice(a)
        }]
    );

    run(&mut cp, &mut c, "stop a");
    run(&mut cp, &mut c, "start x");
    run(&mut cp, &mut c, "start a");
    assert_eq!(rx.events().count(), 0);
}

#[test]
fn full_event_queue_drops_without_blocking() {
    let limits = Limits {
        events: 2,
        ..Limits::default()
    };
    let (mut cp, mut c) = setup_with(limits, 2, mono(&["a"]));
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "clock x 120b");
    run(&mut cp, &mut c, "voice a from \"a\" on x");
    for _ in 0..5 {
        run(&mut cp, &mut c, "start a");
    }
    let out = render(&mut c, 2, 8);
    assert!(out.iter().all(|s| !s.is_nan()));
    assert_eq!(rx.events().count(), 2);
}

#[test]
fn unload_sends_voice_and_procs_to_garbage() {
    let (mut cp, mut c) = setup(2, mono(&["a"]));
    let mut rx = rx(&mut c);
    for cmd in [
        "voice a from \"a\" on 1000ms",
        "a { seq s 1 }",
        "a { seq t 2 }",
        "remove a",
    ] {
        run(&mut cp, &mut c, cmd);
    }
    assert_eq!(Arc::strong_count(&c.tracks.get(0).unwrap().samples), 2);
    assert_eq!(rx.drain_garbage(), 3);
    assert_eq!(Arc::strong_count(&c.tracks.get(0).unwrap().samples), 1);
}

fn unload_three(limits: Limits) -> (Conductor, EngineRx) {
    let (mut cp, mut c) = setup_with(limits, 2, mono(&["a", "b", "c"]));
    let rx = rx(&mut c);
    for cmd in [
        "voice a from \"a\" on 1000ms",
        "voice b from \"b\" on 1000ms",
        "voice c from \"c\" on 1000ms",
        "remove a",
        "remove b",
        "remove c",
    ] {
        run(&mut cp, &mut c, cmd);
    }
    (c, rx)
}

#[test]
fn garbage_parks_when_queue_full_then_flushes() {
    let limits = Limits {
        garbage: 1,
        parked: 4,
        ..Limits::default()
    };
    let (mut c, mut rx) = unload_three(limits);
    assert_eq!(c.parked.len(), 2);
    assert_eq!(rx.drain_garbage(), 1);

    // one slot in the queue, so one parked object moves per render
    render(&mut c, 2, 1);
    assert_eq!(c.parked.len(), 1);
    assert_eq!(rx.drain_garbage(), 1);
    render(&mut c, 2, 1);
    assert!(c.parked.is_empty());
    assert_eq!(rx.drain_garbage(), 1);
    assert_eq!(rx.events().count(), 0);
}

#[test]
fn garbage_leaks_instead_of_freeing_when_park_full() {
    let limits = Limits {
        garbage: 1,
        parked: 1,
        ..Limits::default()
    };
    let (mut c, mut rx) = unload_three(limits);
    render(&mut c, 2, 1);
    let events: Vec<Event> = rx.events().collect();
    assert_eq!(events, vec![Event::GarbageLeaked { count: 1 }]);
    assert_eq!(c.parked.len(), 1);
    assert_eq!(rx.drain_garbage(), 1);
}

#[test]
fn batch_applies_in_order_within_one_render() {
    let (mut cp, mut c) = setup(2, mono(&["a"]));
    let mut rx = rx(&mut c);
    let cmds = ["voice a from \"a\" on 1000ms", "start a", "a rate=0.5"]
        .map(|line| parsed(&mut cp, line))
        .into_iter()
        .collect();
    c.apply(Command::Batch(BatchArgs { cmds }));

    let v = voice(&c, 0);
    assert!(v.state.active);
    assert_eq!(v.state.get_param(VoiceParam::Rate), 0.5);
    render(&mut c, 2, 1);
    assert_eq!(voice(&c, 0).state.inst[0].position, 0.5);

    let Ok(Garbage::Commands(emptied)) = rx.garbage.pop() else {
        panic!("expected the batch's Vec");
    };
    assert!(emptied.is_empty());
    assert!(emptied.capacity() >= 3);
    assert!(rx.garbage.pop().is_err());
}

#[test]
fn queue_ends_are_taken_once() {
    let mut c = conductor(2, Vec::new());
    assert!(c.take_rx().is_some());
    assert!(c.take_rx().is_none());
    assert_eq!(c.out_channels(), 2);
}

// CLKG: the mirror tracks which clocks run, by the engine's own rules

// every mirror tempo is `Running` exactly when the engine's is active
fn assert_run_states_match(cp: &CmdProcessor, c: &Conductor, after: &str) {
    for (id, repr) in &cp.engine_state.tempos {
        let active = c.tempos.get(id.index()).unwrap().state.active;
        assert_eq!(
            repr.run == ClockRun::Running,
            active,
            "tempo {id:?} ({:?}) after '{after}'",
            repr.run
        );
    }
}

#[test]
fn mirror_run_state_matches_engine() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(2, mono(&["a"]));
    c.apply(cp.boot());
    assert_run_states_match(&cp, &c, "boot");
    let lines = [
        "clock x 120b",
        "clock y 90b",
        "start x",
        "pause x",
        "resume x",
        "stop x",
        "pause x",
        "resume x",
        "voice v from \"a\" on 93b",
        "voice w from \"a\"",
        "voice u from \"a\" on x",
        "start v",
        "pause v",
        "resume v",
        "stop v",
        "start w u",
        "v { seq s 1 2 on 70b }",
        "start v",
        "v { seq t 1 on 50b }",
        "w { seq r 1 2 | trig }",
        "stop v",
        "group g on 240b { v; w }",
        "g { seq q 1 on 60b }",
        "start g",
        "pause g",
        "resume g",
        "stop g",
        "start g",
        "v on 100b",
        "g on 200b",
        "start y",
        "seq top 1 2 on 80b | trig w",
        "ungroup v",
        "v on x",
        "v on 77b",
        "pause g",
        "start v",
        "stop all",
        "start all",
        "pause y",
        "stop all",
        "voice z from \"a\" on 66b",
        "start z",
        "group h on 120b { z }",
        "stop h",
        "remove g",
        "remove y",
        "start all",
        "remove all",
        "start all",
    ];
    for line in lines {
        run(&mut cp, &mut c, line);
        assert_run_states_match(&cp, &c, line);
    }
}

#[test]
fn glide_on_paused_clock_freezes_then_runs() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(2, mono(&["a"]));
    run(&mut cp, &mut c, "clock x 120b");
    run(&mut cp, &mut c, "start x");
    render(&mut c, 2, 100);
    run(&mut cp, &mut c, "pause x");
    run(&mut cp, &mut c, "clock x 240b over 1000f");
    render(&mut c, 2, 2000);
    assert_eq!(clock_interval(&cp, &c, "x"), 24000.0);
    run(&mut cp, &mut c, "resume x");
    render(&mut c, 2, 1100);
    assert_eq!(clock_interval(&cp, &c, "x"), 12000.0);
}

#[test]
fn glide_on_running_clock_works() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(2, mono(&["a"]));
    run(&mut cp, &mut c, "clock x 120b");
    run(&mut cp, &mut c, "start x");
    run(&mut cp, &mut c, "clock x 240b over 1000f");
    render(&mut c, 2, 1100);
    assert_eq!(clock_interval(&cp, &c, "x"), 12000.0);
}

#[test]
fn glide_already_going_survives_a_pause() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(2, mono(&["a"]));
    run(&mut cp, &mut c, "clock x 120b");
    run(&mut cp, &mut c, "start x");
    run(&mut cp, &mut c, "clock x 240b over 1000f");
    render(&mut c, 2, 400);
    run(&mut cp, &mut c, "pause x");
    render(&mut c, 2, 1000);
    run(&mut cp, &mut c, "resume x");
    render(&mut c, 2, 700);
    assert_eq!(clock_interval(&cp, &c, "x"), 12000.0);
}

#[test]
fn retime_without_over_on_stopped_clock_works() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(2, mono(&["a"]));
    run(&mut cp, &mut c, "clock x 120b");
    run(&mut cp, &mut c, "clock x 240b");
    assert_eq!(clock_interval(&cp, &c, "x"), 12000.0);
}

// the clock `name`'s interval in samples, in the engine
fn clock_interval(cp: &CmdProcessor, c: &Conductor, name: &str) -> f32 {
    let id = cp.engine_state.context_names[name];
    c.tempos.get(id.index()).unwrap().state.interval
}
