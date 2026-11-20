//! RECc's tests: a buffer's `to=`. A take's private copy comes back when it
//! ends, and goes to a file or a track.

use super::Code;
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::testing::*;
use crate::audio_processing::commands::*;
use crate::audio_processing::engine::{Conductor, EngineRx, Event};
use crate::audio_processing::test_util::{pcm, render, run, rx, setup};
use crate::file_parsing::decode_helpers::AudioFile;
use std::path::PathBuf;
use std::time::{Duration, Instant};

// a mono track whose frame `i` is `r(i)`
fn ramp(name: &str, frames: usize) -> AudioFile {
    let values: Vec<i16> = (0..frames).map(|i| (i as i16 + 1) * 400).collect();
    AudioFile::new(name, "wav", 48_000, 1, 16, pcm(&values))
}

fn r(i: usize) -> f32 {
    f32::from((i as i16 + 1) * 400) / 32768.0
}

// a folder of our own, gone when the test is
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("blast-recc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    fn file(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rig() -> (CmdProcessor, Conductor, EngineRx) {
    sample_rate::set(48_000);
    let (cp, mut c) = setup(1, vec![ramp("src", 64)]);
    let q = rx(&mut c);
    (cp, c, q)
}

fn lines(cp: &mut CmdProcessor, c: &mut Conductor, lines: &[&str]) {
    for line in lines {
        run(cp, c, line);
    }
}

// what the REPL does each pass: events to the mirror, takes to their
// destination; the commands that makes go to the engine and come back
fn pump(cp: &mut CmdProcessor, c: &mut Conductor, q: &mut EngineRx) -> Vec<Command> {
    for event in q.events() {
        if let Event::BufferEnded { slot, take, .. } = event {
            cp.engine_state.note_buffer_ended(slot, take);
        }
    }
    let mut made = Vec::new();
    for (slot, ticket, data, frames) in q.drain_saves() {
        made.extend(cp.note_buffer_save(slot, ticket, data, frames));
    }
    for cmd in &made {
        // the engine takes its own copy of what it needs
        match cmd {
            Command::AddTrack(a) => c.apply(Command::AddTrack(AddTrackArgs {
                id: a.id,
                samples: a.samples.clone(),
                channels: a.channels,
            })),
            Command::SetTrack(a) => c.apply(Command::SetTrack(SetTrackArgs {
                voice: a.voice,
                track: a.track,
            })),
            _ => {}
        }
    }
    made
}

// waits for the file writers, then the notices
fn settle(cp: &mut CmdProcessor) -> Vec<String> {
    let started = Instant::now();
    let mut notices = Vec::new();
    loop {
        notices.extend(cp.buffer_notices());
        if !cp.engine_state.saves.writing() {
            return notices;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "writer hung");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn added(cmds: &[Command]) -> Vec<f32> {
    cmds.iter()
        .find_map(|c| match c {
            Command::AddTrack(a) => Some(a.samples.to_vec()),
            _ => None,
        })
        .expect("no AddTrack")
}

fn close(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() < 1e-6, "{a:?} vs {b:?}");
    }
}

fn read_ints(path: &str) -> Vec<i32> {
    let mut reader = hound::WavReader::open(path).unwrap();
    reader.samples::<i32>().map(Result::unwrap).collect()
}

#[test]
fn to_file_written_on_stop_only() {
    let dir = Dir::new("stop");
    let path = dir.file("t.wav");
    let (mut cp, mut c, mut q) = rig();
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            &format!("buffer b length=64f bits=24 to=\"{path}\""),
            "start src",
            "start b",
        ],
    );
    render(&mut c, 1, 10);
    pump(&mut cp, &mut c, &mut q);
    assert!(!std::path::Path::new(&path).exists(), "nothing before stop");
    assert!(settle(&mut cp).is_empty());

    run(&mut cp, &mut c, "stop b");
    pump(&mut cp, &mut c, &mut q);
    let notices = settle(&mut cp);
    assert!(
        notices.iter().any(|n| n.starts_with("buf b: wrote ")),
        "{notices:?}"
    );
    let got = read_ints(&path);
    assert_eq!(got.len(), 10);
    for (i, x) in got.iter().enumerate() {
        let want = (r(i) * 8_388_608.0).round() as i32;
        assert!((x - want).abs() <= 1, "frame {i}: {x} vs {want}");
    }
}

#[test]
fn to_file_overwrites() {
    let dir = Dir::new("over");
    let path = dir.file("t.wav");
    std::fs::write(&path, b"not a wav file, and longer than the take will be").unwrap();
    let (mut cp, mut c, mut q) = rig();
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            &format!("buffer b length=64f to=\"{path}\""),
            "start src",
            "start b",
        ],
    );
    render(&mut c, 1, 5);
    run(&mut cp, &mut c, "stop b");
    pump(&mut cp, &mut c, &mut q);
    settle(&mut cp);
    assert_eq!(hound::WavReader::open(&path).unwrap().len(), 5);

    // a second take replaces the file
    run(&mut cp, &mut c, "start b");
    render(&mut c, 1, 3);
    run(&mut cp, &mut c, "stop b");
    pump(&mut cp, &mut c, &mut q);
    settle(&mut cp);
    let mut reader = hound::WavReader::open(&path).unwrap();
    assert_eq!(reader.len(), 3);
    // float, unchanged: the voice had played on to frames 5, 6, 7
    let got: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
    close(&got, &[r(5), r(6), r(7)]);
}

#[test]
fn stop_start_same_line_then_stop_is_not_w101() {
    let (mut cp, mut c, mut q) = rig();
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer b length=64f",
            "start src",
            "start b",
        ],
    );
    render(&mut c, 1, 5);
    lines(&mut cp, &mut c, &["stop b", "start b"]);
    render(&mut c, 1, 3);
    // the first take's end reaches the mirror now
    for event in q.events() {
        if let Event::BufferEnded { slot, take, .. } = event {
            cp.engine_state.note_buffer_ended(slot, take);
        }
    }
    assert_eq!(cp.engine_state.buffers["b"].take, Take::Recording);
    std::mem::take(&mut cp.outbox);
    run(&mut cp, &mut c, "stop b");
    assert!(!cp.outbox.contains("W101"), "{}", cp.outbox);
    assert_eq!(cp.engine_state.buffers["b"].take, Take::Idle);
}

#[test]
fn restart_right_after_stop_saves_first_take() {
    let (mut cp, mut c, mut q) = rig();
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer b length=64f to=\"kt\"",
            "start src",
            "start b",
        ],
    );
    render(&mut c, 1, 5);
    // stop and start in one go: the engine sees both before it renders
    lines(&mut cp, &mut c, &["stop b", "start b"]);
    render(&mut c, 1, 3);
    let first = pump(&mut cp, &mut c, &mut q);
    close(&added(&first), &[r(0), r(1), r(2), r(3), r(4)]);
    // the first take's end didn't mark the second one idle
    std::mem::take(&mut cp.outbox);
    run(&mut cp, &mut c, "stop b");
    assert!(!cp.outbox.contains("W101"), "{}", cp.outbox);
    let second = pump(&mut cp, &mut c, &mut q);
    close(&added(&second), &[r(5), r(6), r(7)]);
}

#[test]
fn to_track_makes_track_voices_play_it() {
    let (mut cp, mut c, mut q) = rig();
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer b length=8f to=\"kicktake\"",
            "start src",
            "start b",
        ],
    );
    // the take fills after 8 frames and saves itself
    render(&mut c, 1, 12);
    let made = pump(&mut cp, &mut c, &mut q);
    close(&added(&made), &(0..8).map(r).collect::<Vec<_>>());
    assert!(cp.engine_state.tracks.contains_key("kicktake"));
    assert!(cp.engine_state.tracks["kicktake"].source.is_none());
    assert_eq!(cp.engine_state.buffers["b"].take, Take::Idle);
    let notices = cp.buffer_notices();
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0].starts_with("buf b: track kicktake ("),
        "{notices:?}"
    );

    lines(
        &mut cp,
        &mut c,
        &["stop src", "voice k from \"kicktake\" on 1000ms", "start k"],
    );
    let heard = render(&mut c, 1, 8);
    close(&heard, &(0..8).map(r).collect::<Vec<_>>());
}

#[test]
fn to_track_replaces_and_switches() {
    let (mut cp, mut c, mut q) = rig();
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer b length=4f to=\"kt\"",
            "start src",
            "start b",
        ],
    );
    render(&mut c, 1, 6);
    pump(&mut cp, &mut c, &mut q);
    lines(
        &mut cp,
        &mut c,
        &["voice k from \"kt\" on 1000ms", "start b"],
    );
    let tracks = cp.engine_state.tracks.len();
    render(&mut c, 1, 6);
    let made = pump(&mut cp, &mut c, &mut q);
    // the same track, new samples, and the voice playing it is told
    assert_eq!(cp.engine_state.tracks.len(), tracks);
    close(&added(&made), &[r(6), r(7), r(8), r(9)]);
    let k = vid(&cp, "k");
    assert!(
        made.iter()
            .any(|c| matches!(c, Command::SetTrack(a) if a.voice == k)),
        "no SetTrack for k"
    );
}

#[test]
fn loop_take_saved_in_buffer_order() {
    let (mut cp, mut c, mut q) = rig();
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            "buffer b length=4f loop=yes to=\"kt\"",
            "start src",
            "start b",
        ],
    );
    // six frames into four: frames 4 and 5 wrapped over 0 and 1
    render(&mut c, 1, 6);
    run(&mut cp, &mut c, "stop b");
    let made = pump(&mut cp, &mut c, &mut q);
    close(&added(&made), &[r(4), r(5), r(2), r(3)]);
}

#[test]
fn full_without_loop_saves() {
    let dir = Dir::new("full");
    let path = dir.file("full.wav");
    let (mut cp, mut c, mut q) = rig();
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            &format!("buffer b length=6f to=\"{path}\""),
            "start src",
            "start b",
        ],
    );
    render(&mut c, 1, 20);
    pump(&mut cp, &mut c, &mut q);
    settle(&mut cp);
    assert_eq!(hound::WavReader::open(&path).unwrap().len(), 6);
    assert_eq!(cp.engine_state.buffers["b"].take, Take::Idle);
}

#[test]
fn to_bad_folder_e708() {
    let mut cp = lang_processor();
    let e = lerr(&mut cp, "buffer b to=\"/no-such-folder-blast/x.wav\"");
    assert_eq!(e[0].code, Code::BadSaveFolder);
    assert_eq!(e[0].code.as_str(), "E708");
    assert!(e[0].message.contains("no-such-folder-blast"), "{}", e[0]);
    assert!(cp.engine_state.buffers.is_empty());
    // a track name has no folder
    lok(&mut cp, "buffer b to=\"kicktake\"");
    // and an empty string is no output
    assert_eq!(local(&mut cp, "buffer b to=\"\""), "");
    assert!(cp.engine_state.buffers["b"].to.is_none());
}

#[test]
fn quit_writes_running_takes() {
    let dir = Dir::new("quit");
    let path = dir.file("q.wav");
    let (mut cp, mut c, mut q) = rig();
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            &format!("buffer b length=64f to=\"{path}\""),
            "start src",
            "start b",
        ],
    );
    render(&mut c, 1, 10);
    // the audio callback, on its own thread
    let (mut tx, mut crx) = cmd_queue(16);
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    let audio = std::thread::spawn(move || {
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            while let Some(cmd) = crx.try_pop() {
                c.apply(cmd);
            }
            render(&mut c, 1, 8);
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    cp.finish_buffers(&mut tx, &mut q, Duration::from_secs(5));
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    audio.join().unwrap();
    let notices = cp.buffer_notices();
    assert!(
        notices.iter().any(|n| n.starts_with("buf b: wrote ")),
        "{notices:?}"
    );
    assert!(hound::WavReader::open(&path).unwrap().len() >= 10);
}

#[test]
fn notices_reach_repl() {
    let dir = Dir::new("notices");
    let path = dir.file("n.wav");
    let (mut cp, mut c, mut q) = rig();
    lines(
        &mut cp,
        &mut c,
        &[
            "voice src from \"src\" on 1000ms",
            &format!("buffer b length=4f to=\"{path}\""),
            "start src",
            "start b",
        ],
    );
    render(&mut c, 1, 6);
    pump(&mut cp, &mut c, &mut q);
    let notices = settle(&mut cp);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].starts_with("buf b: wrote "), "{notices:?}");
    assert!(notices[0].contains("n.wav"), "{notices:?}");
    // said once
    assert!(cp.buffer_notices().is_empty());
    // a take of nothing says so
    lines(&mut cp, &mut c, &["start b", "stop b"]);
    pump(&mut cp, &mut c, &mut q);
    assert_eq!(cp.buffer_notices(), vec!["buf b: nothing was recorded"]);
}

#[test]
fn state_round_trip_to() {
    let dir = Dir::new("state");
    let path = dir.file("s.wav");
    let mut cp = lang_processor();
    lok(
        &mut cp,
        &format!("buffer a length=2s bits=16 to=\"{path}\""),
    );
    lok(&mut cp, "buffer c to=\"kicktake\"");
    let text = cp.state(None).unwrap();
    for want in [
        format!("buffer a length=2s bits=16 to=\"{path}\""),
        "buffer c to=\"kicktake\"".to_string(),
    ] {
        assert!(text.contains(&want), "{want}\n{text}");
    }
    let mut fresh = lang_processor();
    lok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
    assert_eq!(
        fresh.engine_state.buffers["c"].to,
        cp.engine_state.buffers["c"].to
    );
}
