//! RECb's tests: the `buffer` statement, its actions, and what the mirror
//! decides (the engine side is in `engine/buffer_tests.rs`).

use super::Code;
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::testing::*;
use crate::audio_processing::commands::*;
use crate::audio_processing::engine::{BufSource, Conductor, EngineRx, Event};
use crate::audio_processing::test_util::{pcm, render, run, rx, setup};
use crate::file_parsing::decode_helpers::AudioFile;

fn session() -> CmdProcessor {
    sample_rate::set(48_000);
    let mut cp = lang_processor();
    lok(&mut cp, "voice kick from \"kick\" on 100f");
    cp
}

fn buffer<'a>(cp: &'a CmdProcessor, name: &str) -> &'a BufferRepr {
    &cp.engine_state.buffers[name]
}

fn cmds(cmd: Command) -> Vec<Command> {
    match cmd {
        Command::Batch(args) => args.cmds,
        other => vec![other],
    }
}

#[test]
fn buffer_defaults() {
    let mut cp = session();
    let made = cmds(lok(&mut cp, "buffer b"));
    let Command::Buffer(args) = &made[0] else {
        panic!("not a Buffer");
    };
    assert_eq!(args.source, BufSource::Out);
    assert!(!args.looping);
    assert_eq!(args.bits, 32);
    // the first buffer brings the table it lives in
    assert!(args.room.is_some());
    // 30 s, two halves, two channels
    assert_eq!(args.data.capacity(), 30 * 48_000);
    assert_eq!(args.data.halves(), 2);
    assert_eq!(args.data.channels, 2);
    let b = buffer(&cp, "b");
    assert!(!b.live && !b.looping);
    assert_eq!(b.take, Take::Idle);
}

#[test]
fn length_beats_need_clock_e402() {
    let mut cp = session();
    let e = lerr(&mut cp, "buffer b length=4b");
    assert_eq!(e[0].code, Code::WrongType);
    assert!(e[0].message.contains("needs a clock"), "{}", e[0]);
    assert!(cp.engine_state.buffers.is_empty());
    // a bare number counts ticks too
    assert_eq!(lerr(&mut cp, "buffer b length=4")[0].code, Code::WrongType);

    // on main (120b): 4 ticks are 2 s
    let made = cmds(lok(&mut cp, "buffer b length=4b on main"));
    let Command::Buffer(args) = &made[0] else {
        panic!("not a Buffer");
    };
    assert_eq!(args.data.capacity(), 96_000);
    assert_eq!(buffer(&cp, "b").clock, cp.engine_state.main_id());
    // seconds, milliseconds and frames need none
    for (text, frames) in [("2s", 96_000), ("250ms", 12_000), ("77f", 77)] {
        let made = cmds(lok(&mut cp, &format!("buffer t{frames} length={text}")));
        let Command::Buffer(args) = &made[0] else {
            panic!("not a Buffer");
        };
        assert_eq!(args.data.capacity(), frames, "{text}");
    }
}

#[test]
fn huge_length_e709_mirror_unchanged() {
    let mut cp = session();
    let e = lerr(&mut cp, "buffer b length=100000000000s");
    assert_eq!(e[0].code, Code::NoMemory);
    assert_eq!(e[0].code.as_str(), "E709");
    assert!(
        e[0].message.starts_with("not enough memory for buffer b ("),
        "{}",
        e[0]
    );
    let es = &cp.engine_state;
    assert!(es.buffers.is_empty());
    assert_eq!(es.ids.buffers.live(), 0);
    assert_eq!(es.buffer_cap, 0);
    // and a restated length too big is refused at the start
    lok(&mut cp, "buffer b length=4f");
    assert_eq!(local(&mut cp, "buffer b length=100000000000s"), "");
    let e = lerr(&mut cp, "start b");
    assert_eq!(e[0].code, Code::NoMemory);
    assert_eq!(buffer(&cp, "b").take, Take::Idle);
}

#[test]
fn from_clock_e402() {
    let mut cp = session();
    for line in ["buffer b from=main", "buffer b from=kick.nothing"] {
        assert!(!lerr(&mut cp, line).is_empty(), "{line}");
    }
    assert_eq!(lerr(&mut cp, "buffer b from=main")[0].code, Code::WrongType);
    assert_eq!(
        lerr(&mut cp, "buffer b from=nope")[0].code,
        Code::UnknownName
    );
    assert_eq!(
        lerr(&mut cp, "buffer b length=4f on 93b")[0].code,
        Code::WrongType
    );
    assert_eq!(lerr(&mut cp, "buffer b bits=12")[0].code, Code::OutOfRange);
    assert_eq!(
        lerr(&mut cp, "buffer b length=0f")[0].code,
        Code::OutOfRange
    );
    assert_eq!(lerr(&mut cp, "buffer out")[0].code, Code::ReservedWord);
    assert!(cp.engine_state.buffers.is_empty());
    // to= takes a quoted string
    assert_eq!(lerr(&mut cp, "buffer b to=x")[0].code, Code::WrongType);
}

#[test]
fn start_recording_w101() {
    let mut cp = session();
    lok(&mut cp, "buffer b length=4f");
    assert!(local(&mut cp, "stop b").contains("W101"));
    assert!(local(&mut cp, "pause b").contains("W101"));
    assert!(local(&mut cp, "resume b").contains("W101"));
    lok(&mut cp, "start b");
    assert_eq!(buffer(&cp, "b").take, Take::Recording);
    assert!(local(&mut cp, "start b").contains("W101"));
    assert!(local(&mut cp, "resume b").contains("W101"));
    lok(&mut cp, "pause b");
    assert_eq!(buffer(&cp, "b").take, Take::Paused);
    assert!(local(&mut cp, "pause b").contains("W101"));
    lok(&mut cp, "resume b");
    lok(&mut cp, "stop b");
    assert_eq!(buffer(&cp, "b").take, Take::Idle);
}

#[test]
fn all_leaves_buffers_alone_but_remove_all_does_not() {
    let mut cp = session();
    lok(&mut cp, "buffer b length=4f from=kick");
    // `start all` has nothing to say to a buffer
    lok(&mut cp, "start all");
    assert_eq!(buffer(&cp, "b").take, Take::Idle);
    lok(&mut cp, "remove all");
    assert!(cp.engine_state.buffers.is_empty());
    // the buffer went first, so nothing is warned about
    assert!(cp.engine_state.warnings.is_empty());
}

#[test]
fn remove_source_stops_take_w102() {
    let mut cp = session();
    lok(&mut cp, "buffer b length=4f from=kick");
    lok(&mut cp, "start b");
    assert_eq!(
        cp.engine_state.taps_of(BufSource::Voice(vid(&cp, "kick"))),
        [0]
    );
    let out = cmds(lok(&mut cp, "remove kick"));
    let b = buffer(&cp, "b");
    assert_eq!(b.source, BufSource::Out);
    assert_eq!(b.take, Take::Idle);
    assert!(out.iter().any(|c| matches!(
        c,
        Command::BufferEdit(a) if a.op == BufOp::Stop
    )));
    let text = std::mem::take(&mut cp.outbox);
    assert!(text.contains("W102"), "{text}");
    assert!(text.contains("buffer 'b'"), "{text}");
}

#[test]
fn remove_buffer_silences_voices() {
    let mut cp = session();
    lok(&mut cp, "buffer b length=4f");
    lok(&mut cp, "voice re from b on 100f");
    assert_eq!(
        cp.engine_state.voices[&vid(&cp, "re")].source,
        VoiceSrc::Buffer(buffer(&cp, "b").id)
    );
    let out = cmds(lok(&mut cp, "remove b"));
    assert!(matches!(out[0], Command::SetSource(_)));
    assert!(out.iter().any(|c| matches!(
        c,
        Command::BufferEdit(a) if a.op == BufOp::Remove
    )));
    assert_eq!(
        cp.engine_state.voices[&vid(&cp, "re")].source,
        VoiceSrc::Gone
    );
    assert!(cp.engine_state.buffers.is_empty());
    // the slot is free again
    assert_eq!(cp.engine_state.ids.buffers.live(), 0);
    // a quoted name is a track, a bare one a buffer
    assert_eq!(
        lerr(&mut cp, "voice x from \"b\" on 100f")[0].code,
        Code::UnknownName
    );
    assert_eq!(
        lerr(&mut cp, "voice x from b on 100f")[0].code,
        Code::UnknownName
    );
}

#[test]
fn state_round_trip_buffer() {
    let mut cp = session();
    lok(
        &mut cp,
        "buffer a length=4b on main loop=yes live=yes bits=16 and buffer c length=250ms from=kick",
    );
    lok(&mut cp, "voice re from a on 100f");
    lok(&mut cp, "group g on 100f { re }");
    lok(&mut cp, "buffer d from=g");
    let text = cp.state(None).unwrap();
    for want in [
        "buffer a length=4b loop=yes live=yes bits=16 on main",
        "buffer c length=0.25s",
        "buffer c from=kick",
        "buffer d from=g",
        "voice re from a",
    ] {
        assert!(text.contains(want), "{want}\n{text}");
    }
    let mut fresh = lang_processor();
    sample_rate::set(48_000);
    lok(&mut fresh, "voice kick from \"kick\" on 100f");
    lok(&mut fresh, &text);
    // the fresh voice and group read back as they were
    assert_eq!(fresh.state(None).unwrap(), cp.state(None).unwrap());
    assert_eq!(
        buffer(&fresh, "c").source,
        BufSource::Voice(vid(&fresh, "kick"))
    );
    assert_eq!(
        buffer(&cp, "d").frames,
        buffer(&fresh, "d").frames,
        "30 s either way"
    );
    // one buffer by itself
    assert!(cp.state(Some("c")).unwrap().contains("buffer c from=kick"));
}

#[test]
fn restated_settings_apply_when_they_do() {
    let mut cp = session();
    lok(&mut cp, "buffer b length=4f");
    lok(&mut cp, "voice re from b on 100f");
    lok(&mut cp, "start b");
    // loop and from apply at once
    let c = cmds(lok(&mut cp, "buffer b loop=yes from=kick"));
    assert!(c.iter().any(|c| matches!(
        c,
        Command::BufferEdit(a) if a.op == BufOp::Looping(true)
    )));
    assert!(c.iter().any(|c| matches!(c, Command::SetTaps(_))));
    assert!(buffer(&cp, "b").looping);
    // length waits for the next start
    assert_eq!(
        local(&mut cp, "buffer b length=8f live=yes"),
        "",
        "restating memory sends nothing yet"
    );
    assert!(buffer(&cp, "b").stale());
    assert!(local(&mut cp, "buffer b length=8f live=yes").contains("W101"));
    lok(&mut cp, "stop b");
    let c = cmds(lok(&mut cp, "start b"));
    let names: Vec<&str> = c
        .iter()
        .map(|c| match c {
            Command::Buffer(_) => "Buffer",
            Command::SetSource(_) => "SetSource",
            Command::BufferEdit(_) => "BufferEdit",
            _ => "other",
        })
        .collect();
    assert_eq!(names, ["Buffer", "SetSource", "BufferEdit"]);
    let Command::Buffer(args) = &c[0] else {
        panic!("not a Buffer");
    };
    assert_eq!((args.data.capacity(), args.data.halves()), (8, 1));
    assert!(!buffer(&cp, "b").stale());
}

fn ramp(name: &str, frames: usize) -> AudioFile {
    let values: Vec<i16> = (0..frames).map(|i| (i as i16 + 1) * 400).collect();
    AudioFile::new(name, "wav", 48000, 1, 16, pcm(&values))
}

fn r(i: usize) -> f32 {
    f32::from((i as i16 + 1) * 400) / 32768.0
}

fn rig() -> (CmdProcessor, Conductor, EngineRx) {
    sample_rate::set(48_000);
    let (mut cp, mut c) = setup(1, vec![ramp("src", 64)]);
    c.apply(cp.boot());
    let queues = rx(&mut c);
    (cp, c, queues)
}

#[test]
fn voice_from_empty_buffer_is_silent() {
    let (mut cp, mut c, _) = rig();
    for line in [
        "buffer b length=4f",
        "voice re from b on 1000ms",
        "start re",
    ] {
        run(&mut cp, &mut c, line);
    }
    assert!(render(&mut c, 1, 8).iter().all(|&x| x == 0.0));
}

#[test]
fn record_then_play_from_buffer() {
    let (mut cp, mut c, mut queues) = rig();
    for line in [
        "voice src from \"src\" on 1000ms",
        "buffer b from=src length=4f",
        "start src",
        "start b",
    ] {
        run(&mut cp, &mut c, line);
    }
    render(&mut c, 1, 4);
    for event in queues.events() {
        if let Event::BufferEnded { slot, take, frames } = event {
            assert_eq!(frames, 4);
            cp.engine_state.note_buffer_ended(slot, take);
        }
    }
    for line in ["stop src", "voice re from b on 1000ms", "start re"] {
        run(&mut cp, &mut c, line);
    }
    let out = render(&mut c, 1, 5);
    let want = [r(0), r(1), r(2), r(3), 0.0];
    for (a, b) in out.iter().zip(want) {
        assert!((a - b).abs() < 1e-6, "{out:?}");
    }
}

#[test]
fn scheduled_start_on_the_beat() {
    let (mut cp, mut c, mut queues) = rig();
    for line in [
        "clock beat 4f",
        "start beat",
        "voice src from \"src\" on 1000ms",
        "buffer b from=src length=4f live=yes on beat",
        "start src",
        "start b at 4 by beat",
    ] {
        run(&mut cp, &mut c, line);
    }
    // 16 frames to the next multiple of 4 ticks
    render(&mut c, 1, 12);
    assert!(
        queues
            .events()
            .all(|e| !matches!(e, Event::BufferEnded { .. }))
    );
    assert_eq!(c.buffers[0].as_ref().unwrap().position(), 0);
    render(&mut c, 1, 12);
    let ended: Vec<Event> = queues
        .events()
        .filter(|e| matches!(e, Event::BufferEnded { .. }))
        .collect();
    assert_eq!(
        ended,
        vec![Event::BufferEnded {
            slot: 0,
            take: 1,
            frames: 4
        }]
    );
    let data = &c.buffers[0].as_ref().unwrap().data;
    let first = data.read(0, 0);
    // src has played on since frame 0; the take began on the grid
    let start = (0..60).find(|&i| (r(i) - first).abs() < 1e-6).unwrap();
    assert_eq!(start % 4, 0, "the take began at frame {start}");
    assert!(start >= 4);
}

#[test]
fn scheduled_start_with_new_memory_cannot_wait() {
    let (mut cp, mut c, _) = rig();
    for line in ["clock beat 4f", "start beat", "buffer b length=4f"] {
        run(&mut cp, &mut c, line);
    }
    run(&mut cp, &mut c, "buffer b length=8f");
    let e = lerr(&mut cp, "start b at 4 by beat");
    assert_eq!(e[0].code, Code::CannotWait);
    // started once without waiting, it can be scheduled
    run(&mut cp, &mut c, "start b");
    run(&mut cp, &mut c, "stop b");
    run(&mut cp, &mut c, "start b at 4 by beat");
}
