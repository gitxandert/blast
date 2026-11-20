//! Real-time safety as a test (README §1.2): nothing reachable from
//! `Conductor::apply` or `Conductor::render` allocates or frees.
//!
//! A counting global allocator is armed only on this test's thread, and only
//! around the calls the audio callback makes (`apply` for each queued
//! command, then `render`). Parsing, garbage draining and track building run
//! unarmed, as they do on the REPL thread. This file is its own crate, so the
//! `unsafe impl GlobalAlloc` doesn't touch `#![forbid(unsafe_code)]` in blast.
//!
//! When a feature adds commands, add lines that exercise them to `SCENARIO`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;

use blast::audio_processing::commands::queue::cmd_queue;
use blast::audio_processing::commands::{CmdProcessor, EngineState};
use blast::audio_processing::engine::Conductor;
use blast::file_parsing::AudioFile;

struct Counting;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static FREES: Cell<usize> = const { Cell::new(0) };
}

fn count(which: &'static std::thread::LocalKey<Cell<usize>>) {
    // try_with: the allocator also runs during thread teardown
    if ARMED.try_with(Cell::get).unwrap_or(false) {
        let _ = which.try_with(|c| c.set(c.get() + 1));
    }
}

// SAFETY: forwards every call unchanged to the system allocator; the
// bookkeeping only touches const-initialised thread-locals, which don't
// allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(&ALLOCS);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(&ALLOCS);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(&FREES);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(&ALLOCS);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Runs `f` armed; returns (allocations, frees) it made on this thread.
fn armed<R>(f: impl FnOnce() -> R) -> (R, usize, usize) {
    ALLOCS.with(|c| c.set(0));
    FREES.with(|c| c.set(0));
    ARMED.with(|a| a.set(true));
    let r = f();
    ARMED.with(|a| a.set(false));
    (r, ALLOCS.with(Cell::get), FREES.with(Cell::get))
}

const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
/// One callback's worth of frames, as with a 128-frame device buffer.
const BLOCK: usize = 128;
/// Callbacks rendered after each line: about 0.34 s, so steps, lfos and
/// envelopes on a 120b clock get to fire.
const BLOCKS_PER_LINE: usize = 128;

/// Mono test tracks: a decaying sine, long enough to loop and shift.
fn tracks() -> Vec<AudioFile> {
    ["kick", "rim1", "shaker", "snare"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let freq = 60.0 * (i + 1) as f32;
            let samples = (0..RATE as usize / 2)
                .map(|n| {
                    let t = n as f32 / RATE as f32;
                    (std::f32::consts::TAU * freq * t).sin() * (-4.0 * t).exp() * 0.5
                })
                .collect();
            AudioFile::new(name, "wav", RATE, 1, 16, samples)
        })
        .collect()
}

/// Lines in the order a session would type them, covering voices, seqs,
/// receivers, pitch/stretch through the shifter and the grains, choirs,
/// ping-pong loops, lfos, envelopes, voice and group effects, buffers, and
/// removal of each.
const SCENARIO: &[&str] = &[
    "voice kick from \"kick\" on main",
    "kick { seq four 1 2 3 4 }",
    "start kick",
    "voice snare from \"snare\" gain=0.6",
    "snare { seq back 2 4 }",
    "start snare",
    "voice rim from \"rim1\" gain=0.5 shift=spectral",
    "rim { seq q 1 2 3 4 }",
    "start rim",
    "rim pitch=0.5",
    "rim pitch=2 over 4",
    "rim stretch=0.5",
    "rim shift=grains",
    "rim shift=spectral window=250ms",
    "rim window=90ms",
    "rim choir=4 choirmode=sim intonation=0.8",
    "rim { seq f steps=16 grid=16 in 4 }",
    "rim choir=1 stretch=1 pitch=1",
    "rim { lfo vib to=pitch depth=50c freq=6hz }",
    "rim.vib depth=7st freq=4b shape=tri",
    "remove rim.vib",
    "rim { env drop to=pitch depth=-12st decay=200ms sustain=0 }",
    "remove rim.drop",
    "rim.q | melo rim 60 63 67 72",
    "voice pp from \"shaker\" loop=pingpong loopstart=0.1 loopend=0.2",
    "start pp",
    "pp bounce=5ms pitch=0.5",
    "snare { filter lo type=lp cutoff=800 q=4 }",
    "snare { lfo sweep to=lo.cutoff depth=2 freq=4b }",
    "snare { drive dir amount=24 mix=0.1 }",
    "snare { crush bit bits=4 down=6 }",
    "snare { env amp decay=60ms sustain=0 }",
    "snare.amp attack=40ms decay=100ms sustain=0.5 hold=80ms release=200ms",
    "pause snare.sweep",
    "resume snare.sweep",
    "remove snare.sweep snare.lo snare.dir snare.bit snare.amp",
    "group drums { kick; snare }",
    "start drums",
    "drums { filter dark type=lp cutoff=500 }",
    "drums.dark cutoff=6000 over 8",
    "buffer loop from=drums length=4b on main",
    "voice re from loop rate=0.5 loop=forward",
    "start loop re",
    "re pitch=0.5",
    "buffer loop loop=yes live=yes",
    "re pitch=2",
    "remove drums.dark",
    "stop all",
    "start main",
    "start drums",
    // a script's spectral voice gets its shifters like a typed one (SHFIX).
    // The placeholders stand for files the test writes: `source` at the
    // prompt, `run_script` (`--script`: apply_lines), and a timed script
    // whose later segment moves the pitch of an existing spectral voice
    SOURCE_LINE,
    "voice vs from \"kick\" shift=spectral",
    RUN_LINE,
    TIMED_LINE,
    // waves (WAVa): oscillators, glides, choir, lfo and env targets, melo
    // notes, stop/pause, a source added later, and removal
    "voice organ { wave s1 freq=a3; wave s2 shape=saw freq=220 pw=0.3 }",
    "start organ",
    "organ.s2 freq=330 over 2",
    "organ { wave s3 shape=square pw=0.25 }",
    "organ { wave hat shape=noise gain=-12db }",
    "organ.s3 shape=tri retrig=no",
    "organ choir=3 intonation=0.7",
    "organ { lfo wob to=s2.freq depth=0.5 freq=4hz }",
    "organ { env pluck to=s1.gain decay=100ms sustain=0 }",
    "organ { seq t 1 2 3 4 }",
    "organ.t | melo organ 60 63 67",
    "pause organ.s3",
    "resume organ.s3",
    "stop organ.s1",
    "start organ.s1",
    "voice organ from \"kick\"",
    // waiting lines that name a wave: one that runs, a wave made in a staged
    // part, and one dropped when its wave is removed
    "organ.s2 freq=300 at 1",
    "organ.s2 gain=0.5 over 1b then organ { wave late }",
    "organ.s3 gain=0.4 at 4",
    "organ.s2 freq=200 over 1b thenif organ.s2 gain=0.3",
    "remove organ.wob organ.pluck organ.s3",
    // table waves (WAVb): a table cut from a track, a glide of pos, a new
    // source and a new cut (each builds on the command thread and retires
    // the old table), lfo and env on pos, a switch away and back, removal
    "voice pad { wave t shape=table from \"kick\" slices=4 }",
    "start pad",
    "pad.t pos=1 over 2",
    "pad.t from=\"snare\"",
    "pad { lfo sweep to=t.pos depth=0.5 freq=2hz }",
    "pad { env swell to=t.pos depth=0.5 decay=100ms sustain=0 }",
    "pad.t frames=512",
    "pad.t shape=saw",
    "pad.t shape=table",
    "pad { wave u shape=table from \"shaker\" slices=2 pos=0.5 }",
    "remove pad.sweep pad.swell",
    "remove pad.u",
    "remove pad",
    // FM (WAVc): a modulator that isn't heard, a carrier that follows it, an
    // index glide, an lfo on the ratio, the link cut and made again, the
    // modulator heard, the mode switched both ways, removal
    "voice bell { wave m ratio=3.5 out=no }",
    "bell { wave c fm=m index=4 }",
    "start bell",
    "bell.c index=0.5 over 2",
    "bell { lfo wob to=m.ratio depth=0.1 freq=1hz }",
    "bell.c fm=none",
    "bell.c fm=m",
    "bell.m out=yes",
    "bell.m ratio=none",
    "bell.m ratio=2",
    "remove bell.wob",
    "remove bell.c",
    "remove bell",
    "voice ep { wave a ratio=1 out=no; wave b ratio=14 out=no gain=0.3; wave c fm=a+b index=2 fb=0.5 }",
    "start ep",
    "ep.c fb=1.5 over 2",
    "ep.c fm=b",
    "ep.c fm=a+b",
    "ep { lfo w to=c.fb depth=0.3 freq=1hz }",
    "remove ep.w",
    "remove ep.b",
    "ep.a ratio=3/2",
    "remove ep",
    // the Elliptic-BLEP resampler (HPR): a loop at speed, a glide through
    // 1x, the method switched, and a choir of them
    "voice fast from \"shaker\" resample=iir rate=2.5 loop=forward",
    "start fast",
    "fast rate=0.6 over 2",
    "fast resample=cubic",
    "fast resample=iir choir=3 intonation=0.6",
    "fast rate=-1.7 loop=pingpong",
    "start fast",
    "remove fast",
    "remove organ",
    "remove re pp rim",
    "remove drums",
    "remove sp rs vs",
];

const SOURCE_LINE: &str = "source (the test's script)";
const RUN_LINE: &str = "run_script (the test's script)";
const TIMED_LINE: &str = "run_script (the test's timed script)";
const SOURCE_SCRIPT: &str =
    "voice sp from \"kick\" shift=spectral pitch=3st\nstart sp\nsp stretch=0.8\n";
const RUN_SCRIPT: &str =
    "voice rs from \"kick\" shift=spectral pitch=2st\nstart rs\nrs pitch=3st\n";
const TIMED_SCRIPT: &str = "wait 1\nvs pitch=1.5\nwait 1\nvs stretch=0.8\n";

#[test]
fn apply_and_render_never_allocate() {
    let tracks = tracks();
    let map: HashMap<String, AudioFile> = tracks
        .iter()
        .map(|af| (af.file_name.clone(), af.clone()))
        .collect();
    let mut cp = CmdProcessor::new(EngineState::new(tracks, CHANNELS));
    let mut conductor = Conductor::prepare(CHANNELS, map);
    let mut rx = conductor
        .take_rx()
        .expect("fresh conductor keeps its queue ends");
    let (mut tx, mut commands) = cmd_queue(1024);
    let mut out = vec![0.0f32; BLOCK * CHANNELS];

    let boot = cp.boot();
    tx.try_push(boot).ok().expect("empty queue has room");

    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let write = |name: &str, text: &str| {
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write the script");
        path
    };
    let source = write("rt_alloc_source.blast", SOURCE_SCRIPT);
    let run = write("rt_alloc_run.blast", RUN_SCRIPT);
    let timed = write("rt_alloc_timed.blast", TIMED_SCRIPT);

    let mut failures = Vec::new();
    let mut peak = 0.0f32;
    for line in std::iter::once("(boot)").chain(SCENARIO.iter().copied()) {
        let parsed = if line == "(boot)" {
            Ok(None)
        } else if line == SOURCE_LINE {
            cp.submit(&format!("source \"{}\"", source.display()), &mut tx)
        } else if line == RUN_LINE || line == TIMED_LINE {
            let path = if line == RUN_LINE { &run } else { &timed };
            cp.run_script(path, &mut tx).map(|()| None)
        } else {
            cp.submit(line, &mut tx)
        };
        if let Err(e) = parsed {
            panic!("scenario line '{line}' doesn't parse: {e}");
        }
        // the callback: drain the command queue, then render
        let ((), a, f) = armed(|| {
            while let Some(cmd) = commands.try_pop() {
                conductor.apply(cmd);
            }
        });
        if a + f > 0 {
            failures.push(format!("apply  '{line}': {a} allocs, {f} frees"));
        }
        let (mut ra, mut rf) = (0, 0);
        for _ in 0..BLOCKS_PER_LINE {
            let ((), a, f) = armed(|| conductor.render(&mut out, CHANNELS));
            ra += a;
            rf += f;
            peak = out.iter().fold(peak, |m, s| m.max(s.abs()));
            // the REPL thread's side, unarmed
            rx.drain_garbage();
            while rx.events.pop().is_ok() {}
        }
        if ra + rf > 0 {
            failures.push(format!("render after '{line}': {ra} allocs, {rf} frees"));
        }
    }

    // a scenario that renders silence would pass without testing anything
    assert!(peak > 0.01, "the scenario rendered silence (peak {peak})");
    assert!(
        failures.is_empty(),
        "the audio thread allocated or freed (README §1.2):\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn the_counter_sees_allocations_and_frees() {
    let (v, a, f) = armed(|| vec![0u8; 64]);
    assert_eq!((a, f), (1, 0));
    let ((), a, f) = armed(|| drop(v));
    assert_eq!((a, f), (0, 1));
}
