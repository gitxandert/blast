//! Buffer tests (RECb): taps, takes, halves and what voices hear, driven
//! through the language so the mirror and the engine agree.

use super::*;
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::test_util::{pcm, render, run, rx, setup, track};

// a mono track with these i16 values
fn samples(name: &str, values: &[i16]) -> AudioFile {
    AudioFile::new(name, "wav", 48000, 1, 16, pcm(values))
}

fn ramp(name: &str, frames: usize) -> AudioFile {
    let values: Vec<i16> = (0..frames).map(|i| (i as i16 + 1) * 400).collect();
    samples(name, &values)
}

// the i16 value `ramp` has at frame `i`, as f32
fn r(i: usize) -> f32 {
    f32::from((i as i16 + 1) * 400) / 32768.0
}

fn rig(tracks: Vec<AudioFile>) -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    setup(1, tracks)
}

// what the REPL does with the engine's notices
fn feed(cp: &mut CmdProcessor, queues: &mut EngineRx) {
    for event in queues.events() {
        if let Event::BufferEnded { slot, take, .. } = event {
            cp.engine_state.note_buffer_ended(slot, take);
        }
    }
}

fn lines(cp: &mut CmdProcessor, c: &mut Conductor, lines: &[&str]) {
    for line in lines {
        run(cp, c, line);
    }
}

// the front half of the buffer in `slot`, frame by frame
fn heard(c: &Conductor, slot: usize) -> Vec<f32> {
    let data = &c.buffers[slot].as_ref().unwrap().data;
    (0..data.frames_heard()).map(|i| data.read(i, 0)).collect()
}

fn slot(c: &Conductor, i: usize) -> &BufferSlot {
    c.buffers[i].as_ref().unwrap()
}

fn close(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() < 1e-6, "{a:?} vs {b:?}");
    }
}

#[test]
fn out_buffer_copies_unclamped_block() {
    // two voices of 0.75 sum to 1.5, which the buffer keeps
    let (mut cp, mut c) = rig(vec![samples("a", &[24576; 16]), samples("b", &[24576; 16])]);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\" on 1000ms",
            "voice b from \"b\" on 1000ms",
            "buffer rec length=4f live=yes",
            "start a",
            "start b",
            "start rec",
        ],
    );
    let out = render(&mut c, 1, 4);
    close(&out, &[1.5; 4]);
    close(&heard(&c, 0), &[1.5; 4]);
}

#[test]
fn voice_tap_is_post_fx_pre_group() {
    // gain 0.5, a crush with gain 0.5, then a group at 0.5: the buffer hears
    // the voice after the first two and before the third
    let (mut cp, mut c) = rig(vec![samples("a", &[16384; 16])]);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\" on 1000ms gain=0.5",
            "a { crush q bits=16 gain=0.5 }",
            "group g on 1000ms gain=0.5 { a }",
            "buffer rec from=g.a length=4f live=yes",
            "start g",
            "start rec",
        ],
    );
    let out = render(&mut c, 1, 4);
    // 0.5 (track) * 0.5 (gain) * 0.5 (crush gain) = 0.125, then the group
    // halves it
    close(&heard(&c, 0), &[0.125; 4]);
    close(&out, &[0.0625; 4]);
}

#[test]
fn out_taps_list_follows_start_stop_remove() {
    let (mut cp, mut c) = rig(vec![samples("a", &[24576; 16])]);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\" on 1000ms",
            "buffer one length=4f live=yes",
            "buffer two from=a length=4f live=yes",
            "buffer three length=4f live=yes",
        ],
    );
    // the buffers that take the mix, by slot; not the one on a voice
    assert_eq!(c.out_taps, [0, 2]);
    // takes starting and stopping don't change which buffers take the mix
    lines(&mut cp, &mut c, &["start one", "stop one"]);
    assert_eq!(c.out_taps, [0, 2]);
    lines(&mut cp, &mut c, &["remove one"]);
    assert_eq!(c.out_taps, [2]);
    // a buffer whose voice goes records the output instead
    lines(&mut cp, &mut c, &["remove a"]);
    assert_eq!(c.out_taps, [1, 2]);
    // and one that is recording still gets the frames
    lines(&mut cp, &mut c, &["start three"]);
    render(&mut c, 1, 2);
    assert_eq!(c.buffers[2].as_ref().unwrap().position(), 2);
}

#[test]
fn group_tap_is_bus_after_fx() {
    let (mut cp, mut c) = rig(vec![samples("a", &[8192; 16]), samples("b", &[16384; 16])]);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\" on 1000ms",
            "voice b from \"b\" on 1000ms",
            "group g on 1000ms gain=0.5 { a; b }",
            "g { crush q bits=16 gain=0.5 }",
            "buffer rec from=g length=4f live=yes",
            "start g",
            "start rec",
        ],
    );
    let out = render(&mut c, 1, 4);
    // (0.25 + 0.5) = 0.75, the chain halves it (0.375), the group gain
    // halves it again (0.1875): the tap is what the group adds to the output
    close(&heard(&c, 0), &[0.1875; 4]);
    close(&out, &[0.1875; 4]);
}

#[test]
fn voice_hears_frames_as_recorded_live() {
    // src is recorded into `rec`, which re plays at the same moment
    let (mut cp, mut c) = rig(vec![ramp("src", 32), ramp("src2", 32)]);
    let mut queues = rx(&mut c);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer rec from=src length=8f live=yes",
            "voice re from rec on 1000ms",
            "start src",
            "start rec",
            "start re",
        ],
    );
    // re comes after src in the frame, so it plays what src just wrote
    let out = render(&mut c, 1, 8);
    let want: Vec<f32> = (0..8).map(|i| 2.0 * r(i)).collect();
    close(&out, &want);
    feed(&mut cp, &mut queues);

    // the second take records src2 from the start; beyond the position the
    // buffer still holds the first take
    lines(
        &mut cp,
        &mut c,
        &["stop re", "voice src from \"src2\"", "start rec"],
    );
    // 2 frames: src2 is playing from where src was (frames 8 and 9)
    render(&mut c, 1, 2);
    let mixed = heard(&c, 0);
    assert_eq!(mixed.len(), 8, "the first take filled the buffer");
    close(&mixed[..2], &[r(8), r(9)]);
    close(&mixed[2..], &[r(2), r(3), r(4), r(5), r(6), r(7)]);
}

#[test]
fn live_no_swaps_at_end() {
    // constants make the positions irrelevant: A, then B
    let (mut cp, mut c) = rig(vec![samples("a", &[8192; 64]), samples("b", &[16384; 64])]);
    let mut queues = rx(&mut c);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"a\" on 1000ms",
            "buffer rec from=src length=4f",
            "voice re from rec on 1000ms loop=forward",
            "start src",
            "start rec",
            "start re",
        ],
    );
    // the first take is silent to re until it ends
    let out = render(&mut c, 1, 3);
    close(&out, &[0.25; 3]);
    assert_eq!(slot(&c, 0).data.frames_heard(), 0);
    render(&mut c, 1, 1);
    assert!(!slot(&c, 0).is_writing());
    let out = render(&mut c, 1, 3);
    close(&out, &[0.5; 3]);
    feed(&mut cp, &mut queues);

    // the second take records B while re keeps hearing A
    lines(&mut cp, &mut c, &["voice src from \"b\"", "start rec"]);
    let out = render(&mut c, 1, 3);
    close(&out, &[0.5 + 0.25; 3]);
    render(&mut c, 1, 1);
    let out = render(&mut c, 1, 3);
    close(&out, &[1.0; 3]);
}

// a ramp through `out`, recorded with these settings
fn record_ramp(settings: &str, frames: usize) -> (CmdProcessor, Conductor) {
    let (mut cp, mut c) = rig(vec![ramp("src", 64)]);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            &format!("buffer rec {settings}"),
            "start src",
            "start rec",
        ],
    );
    render(&mut c, 1, frames);
    (cp, c)
}

#[test]
fn loop_wraps_and_keeps_recording() {
    let (_, c) = record_ramp("length=4f live=yes loop=yes", 6);
    // frames 4 and 5 replaced 0 and 1
    close(&heard(&c, 0), &[r(4), r(5), r(2), r(3)]);
    assert!(slot(&c, 0).is_writing());
}

#[test]
fn loop_live_no_swaps_each_pass() {
    let (_, c) = record_ramp("length=4f loop=yes", 4);
    close(&heard(&c, 0), &[r(0), r(1), r(2), r(3)]);
    let (_, c) = record_ramp("length=4f loop=yes", 6);
    // the second pass is not over: voices still hear the first
    close(&heard(&c, 0), &[r(0), r(1), r(2), r(3)]);
    let (_, c) = record_ramp("length=4f loop=yes", 8);
    close(&heard(&c, 0), &[r(4), r(5), r(6), r(7)]);
    assert!(slot(&c, 0).is_writing());
}

#[test]
fn full_without_loop_ends_take() {
    let (mut cp, mut c) = rig(vec![ramp("src", 64)]);
    let mut queues = rx(&mut c);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer rec length=4f live=yes",
            "start src",
            "start rec",
        ],
    );
    render(&mut c, 1, 3);
    assert!(queues.events().next().is_none());
    render(&mut c, 1, 3);
    let ended: Vec<Event> = queues.events().collect();
    assert_eq!(
        ended,
        vec![Event::BufferEnded {
            slot: 0,
            take: 1,
            frames: 4
        }]
    );
    assert!(!slot(&c, 0).is_writing());
    // it stays as it was
    close(&heard(&c, 0), &[r(0), r(1), r(2), r(3)]);
    cp.engine_state.note_buffer_ended(0, 1);
    assert_eq!(cp.engine_state.buffers["rec"].take, Take::Idle);
}

#[test]
fn pause_resume_same_position() {
    let (mut cp, mut c) = rig(vec![ramp("src", 64)]);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer rec length=4f live=yes",
            "start src",
            "start rec",
        ],
    );
    render(&mut c, 1, 2);
    run(&mut cp, &mut c, "pause rec");
    render(&mut c, 1, 3);
    assert_eq!(slot(&c, 0).position(), 2);
    run(&mut cp, &mut c, "resume rec");
    render(&mut c, 1, 2);
    // the source played on while the buffer waited
    close(&heard(&c, 0), &[r(0), r(1), r(5), r(6)]);
}

#[test]
fn start_restarts_at_zero() {
    let data = BufData::alloc(4, 1, 1).unwrap();
    let mut b = BufferSlot::new(data, BufSource::Out, false, 32, 1);
    b.start(None, 1);
    for x in [1.0, 2.0, 3.0] {
        assert_eq!(b.push(&[x]), None);
    }
    assert_eq!(b.position(), 3);
    assert_eq!(b.stop(), Some(3));
    b.start(None, 1);
    assert_eq!(b.position(), 0);
    b.push(&[9.0]);
    // live: the first take's later frames are still there
    assert_eq!(b.data.read(0, 0), 9.0);
    assert_eq!(b.data.read(1, 0), 2.0);
    assert_eq!(b.data.frames_heard(), 3);
}

#[test]
fn table_grows_with_room_without_allocating() {
    let (mut cp, mut c) = rig(vec![ramp("src", 64)]);
    let mut queues = rx(&mut c);
    run(&mut cp, &mut c, "voice src from \"src\" on 1000ms");
    for i in 0..10 {
        run(&mut cp, &mut c, &format!("buffer b{i} length=4f"));
    }
    assert!(c.buffers.len() >= 10);
    assert!((0..10).all(|i| c.buffers[i].is_some()));
    // the outgrown tables came back as garbage
    let mut tables = 0;
    while let Ok(g) = queues.garbage.pop() {
        tables += usize::from(matches!(g, Garbage::BufferTable(_)));
    }
    assert!(tables >= 1);

    // nothing else grows it
    let (ptr, cap) = (c.buffers.as_ptr(), c.buffers.capacity());
    for _ in 0..200 {
        lines(
            &mut cp,
            &mut c,
            &["start b3", "pause b3", "resume b3", "stop b3"],
        );
        lines(
            &mut cp,
            &mut c,
            &["buffer b3 loop=yes", "buffer b3 loop=no"],
        );
    }
    assert_eq!((c.buffers.as_ptr(), c.buffers.capacity()), (ptr, cap));
}

#[test]
fn many_buffers_one_source() {
    let (mut cp, mut c) = rig(vec![ramp("src", 64)]);
    run(&mut cp, &mut c, "voice src from \"src\" on 1000ms");
    for i in 0..6 {
        run(
            &mut cp,
            &mut c,
            &format!("buffer b{i} from=src length=4f live=yes"),
        );
    }
    let v = c.voices.get(0).unwrap();
    assert_eq!(v.taps, vec![0, 1, 2, 3, 4, 5]);
    lines(&mut cp, &mut c, &["start src", "start b0", "start b5"]);
    render(&mut c, 1, 4);
    for i in [0, 5] {
        close(&heard(&c, i), &[r(0), r(1), r(2), r(3)]);
    }
    assert!(heard(&c, 2).is_empty());
}

#[test]
fn buffer_voice_renders_like_a_track_voice() {
    // the same samples through a track and through a buffer play alike,
    // here with rate 1.3 so the interpolation runs
    let (mut cp, mut c) = rig(vec![ramp("src", 32), ramp("same", 32)]);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer rec from=src length=32f",
            "start src",
            "start rec",
        ],
    );
    render(&mut c, 1, 32);
    assert!(!slot(&c, 0).is_writing());
    lines(
        &mut cp,
        &mut c,
        &[
            "remove src",
            "voice t from \"same\" on 1000ms rate=1.3",
            "voice b from rec on 1000ms rate=1.3",
        ],
    );
    // t's the only sound until b starts: record it, then play b alone
    lines(&mut cp, &mut c, &["start t"]);
    let track_out = render(&mut c, 1, 24);
    lines(&mut cp, &mut c, &["stop t", "start b"]);
    let buffer_out = render(&mut c, 1, 24);
    assert_eq!(track_out, buffer_out);
}

#[test]
fn bits_8_quantises() {
    let (mut cp, mut c) = rig(vec![samples("src", &[9830; 16])]);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer rec from=src length=8f bits=8 live=yes",
            "start src",
            "start rec",
        ],
    );
    render(&mut c, 1, 8);
    let got = heard(&c, 0);
    assert_eq!(got.len(), 8);
    let want = 9830.0 / 32768.0;
    for x in got {
        let steps = x * 128.0;
        assert!(
            (steps - steps.round()).abs() < 1e-4,
            "{x} is not on an 8-bit step"
        );
        // the dither is up to one step either way, on top of rounding
        assert!((x - want).abs() <= 2.0 / 128.0 + 1e-6);
    }
}

#[test]
fn default_is_live_no() {
    let (mut cp, mut c) = rig(vec![track("src", 8192, 16)]);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer rec from=src length=8f",
            "start src",
            "start rec",
        ],
    );
    assert_eq!(slot(&c, 0).data.halves(), 2);
    render(&mut c, 1, 5);
    // mid-take, nothing for a voice to hear yet
    assert_eq!(slot(&c, 0).data.frames_heard(), 0);
    render(&mut c, 1, 3);
    assert_eq!(slot(&c, 0).data.frames_heard(), 8);

    // live=yes is one half
    run(&mut cp, &mut c, "buffer fast length=8f live=yes");
    assert_eq!(slot(&c, 1).data.halves(), 1);
}

#[test]
fn removed_voice_stops_garbage_and_taps() {
    let (mut cp, mut c) = rig(vec![ramp("src", 64)]);
    let mut queues = rx(&mut c);
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer rec from=src length=4f",
            "voice re from rec on 1000ms",
            "start src",
            "start rec",
            "remove rec",
        ],
    );
    assert!(c.buffers[0].is_none());
    // re keeps running, silent; the memory goes back as garbage
    let out = render(&mut c, 1, 4);
    close(&out, &[r(0), r(1), r(2), r(3)]);
    let mut buffers = 0;
    while let Ok(g) = queues.garbage.pop() {
        buffers += usize::from(matches!(g, Garbage::Buffer(_)));
    }
    assert!(buffers >= 2, "the slot's and the voice's hold");
}

// ---- the command thread's copy of a buffer (copy_heard) ----

// `n` frames of `v` into a looping two-half buffer of 8 frames
fn push_n(slot: &mut BufferSlot, n: usize, v: f32) {
    for _ in 0..n {
        let _ = slot.push(&[v]);
    }
}

#[test]
fn copy_heard_is_the_front_take() {
    let data = BufData::alloc(8, 1, 2).unwrap();
    let mut slot = BufferSlot::new(std::sync::Arc::clone(&data), BufSource::Out, true, 32, 0);
    assert!(data.copy_heard().is_empty());
    let _ = slot.start(None, 1);
    push_n(&mut slot, 8, 0.1);
    push_n(&mut slot, 3, 0.2);
    // the first pass is heard; the second is still being written
    assert_eq!(data.copy_heard(), vec![0.1; 8]);
}

#[test]
fn copy_heard_never_mixes_two_takes() {
    let data = BufData::alloc(8, 1, 2).unwrap();
    let mut slot = BufferSlot::new(std::sync::Arc::clone(&data), BufSource::Out, true, 32, 0);
    let _ = slot.start(None, 1);
    push_n(&mut slot, 8, 0.1);
    push_n(&mut slot, 8, 0.2);
    // halfway through copying the 0.2 take, the engine finishes the 0.3
    // pass (the halves swap) and the next one writes 6 frames of 0.4 into
    // the half being copied
    let mut once = false;
    let copy = data.copy_heard_while(|f| {
        if f == 4 && !once {
            once = true;
            push_n(&mut slot, 8, 0.3);
            push_n(&mut slot, 6, 0.4);
        }
    });
    assert!(once);
    // not [0.2, 0.2, 0.2, 0.2, 0.4, 0.4, 0.2, 0.2]: copied again, from the
    // half voices hear now
    assert_eq!(copy, vec![0.3; 8]);
}

#[test]
fn copy_heard_of_a_two_channel_buffer_interleaves() {
    let data = BufData::alloc(4, 2, 2).unwrap();
    let mut slot = BufferSlot::new(std::sync::Arc::clone(&data), BufSource::Out, false, 32, 0);
    let _ = slot.start(None, 1);
    for i in 0..3 {
        let _ = slot.push(&[i as f32, -(i as f32)]);
    }
    let _ = slot.stop();
    assert_eq!(data.copy_heard(), vec![0.0, -0.0, 1.0, -1.0, 2.0, -2.0]);
}
