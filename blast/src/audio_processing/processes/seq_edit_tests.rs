//! PRC's engine tests: editing, removing, seeding and pausing seqs, and seqs
//! on groups. Each drives the language into a `CmdProcessor` and a
//! `Conductor`, as the REPL does.

use crate::audio_processing::{
    blast_time::sample_rate,
    commands::{CmdProcessor, ProcId},
    engine::{Conductor, Garbage},
    processes::Process,
    test_util::{render_i16, run, rx, setup},
};
use crate::file_parsing::decode_helpers::AudioFile;

use crate::audio_processing::test_util::pcm;

// mono track whose first sample is -1 and every other is 1, so a frame
// renders -1 only when the Voice is (re)triggered
fn kick() -> AudioFile {
    let mut samples = vec![1i16; 400_000];
    samples[0] = -1;
    AudioFile::new("kick", "wav", 48000, 1, 16, pcm(&samples))
}

// a session; each line runs on both sides
fn session(lines: &[&str]) -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![kick()]);
    for l in lines {
        run(&mut cp, &mut c, l);
    }
    (cp, c)
}

// frames that rendered the trigger (-1 per voice)
fn hits(out: &[i16]) -> Vec<usize> {
    out.iter()
        .enumerate()
        .filter(|(_, s)| **s < 0)
        .map(|(f, _)| f)
        .collect()
}

fn state_of(c: &Conductor, proc: usize) -> &super::SeqState {
    match &c.procs.get(proc).expect("no such process").process {
        Process::Seq(s) => &s.state,
        Process::Lfo(_) | Process::Env(_) => panic!("process {proc} is not a seq"),
    }
}

#[test]
fn seed_repeats() {
    let line = "kick { seq s 1 2 3 4 chance=50 seed=42 on 100f }";
    let run_once = |line: &str| {
        let (_, mut c) = session(&["voice kick from \"kick\" on 1000ms", line, "start kick"]);
        hits(&render_i16(&mut c, 1, 6400))
    };
    let a = run_once(line);
    let b = run_once(line);
    assert_eq!(a, b);
    // 64 steps at 50%: some fired, some didn't
    assert!(a.len() > 10 && a.len() < 54, "{}", a.len());
    // another seed gives another pattern
    assert_ne!(a, run_once(&line.replace("seed=42", "seed=43")));
}

#[test]
fn restating_seed_reseeds() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 2 3 4 chance=50 seed=5 on 100f }",
        "start kick",
    ]);
    let first = hits(&render_i16(&mut c, 1, 3200));
    // the same seed again starts the same stream over
    run(&mut cp, &mut c, "stop kick and start kick");
    run(&mut cp, &mut c, "kick.s seed=5");
    let second = hits(&render_i16(&mut c, 1, 3200));
    assert_eq!(first, second);
    assert_eq!(cp.engine_state.procs[&ProcId(0)].seq.seed, Some(5));
}

#[test]
fn edit_steps_keeps_position() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 3 on 1000f }",
        "start kick",
    ]);
    // position 2.5: steps 1 and 3 have fired
    assert_eq!(hits(&render_i16(&mut c, 1, 2500)), vec![0, 2000]);
    run(&mut cp, &mut c, "kick.s 1 4");
    // step 4 is next (frame 3000); 3 doesn't fire again; the cycle wraps
    // to step 1 at frame 4000
    assert_eq!(hits(&render_i16(&mut c, 1, 1600)), vec![500, 1500]);
}

#[test]
fn edit_step_at_current_position_doesnt_double() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 3 on 1000f }",
        "start kick",
    ]);
    // step 3 fired at frame 2000, this very frame range
    assert_eq!(hits(&render_i16(&mut c, 1, 2001)), vec![0, 2000]);
    run(&mut cp, &mut c, "kick.s 1 3 4");
    // 3 is kept: it counts as crossed. 4 comes at 3000.
    assert_eq!(hits(&render_i16(&mut c, 1, 1500)), vec![999]);
}

#[test]
fn edit_period_and_feel() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 on 1000f }",
        "start kick",
    ]);
    assert_eq!(hits(&render_i16(&mut c, 1, 500)), vec![0]);
    // a cycle of 2 ticks: the next hit is at tick 2
    run(&mut cp, &mut c, "kick.s in 2");
    assert_eq!(state_of(&c, 0).period, 2);
    assert_eq!(cp.engine_state.procs[&ProcId(0)].seq.period, 2);
    assert_eq!(hits(&render_i16(&mut c, 1, 4000)), vec![1500, 3500]);
    // a seq-wide chance of 0 silences every step without its own `%`
    run(&mut cp, &mut c, "kick.s chance=0");
    assert_eq!(state_of(&c, 0).feel[0].value, 0.0);
    assert!(hits(&render_i16(&mut c, 1, 4000)).is_empty());
    assert_eq!(cp.engine_state.procs[&ProcId(0)].seq.steps[0].chance, 0.0);
}

#[test]
fn edit_retires_old_vectors() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 2 3 on 1000f }",
    ]);
    let mut rx = rx(&mut c);
    assert_eq!(rx.drain_garbage(), 0);
    run(&mut cp, &mut c, "kick.s 1 4");
    // the old three-step vectors went to the garbage queue, not the heap
    let Ok(Garbage::SeqData(old)) = rx.garbage.pop() else {
        panic!("no SeqData in the garbage queue");
    };
    assert_eq!(old.steps, vec![0.0, 1.0, 2.0]);
    assert_eq!(old.mask.len(), 3);
    assert!(rx.garbage.pop().is_err());
    assert_eq!(state_of(&c, 0).steps, vec![0.0, 3.0]);
}

#[test]
fn remove_seq() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 2 on 1000f }",
    ]);
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "remove kick.s");
    assert!(cp.engine_state.procs.is_empty());
    assert_eq!(cp.engine_state.ids.procs.live(), 0);
    assert!(c.procs.get(0).is_none());
    // the process left as garbage, with its vectors
    assert!(matches!(rx.garbage.pop(), Ok(Garbage::Process(_))));
    // no seq left, so `start` plays at once again
    run(&mut cp, &mut c, "start kick");
    assert_eq!(hits(&render_i16(&mut c, 1, 100)), vec![0]);
}

#[test]
fn remove_seq_frees_own_clock() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 on 93b and seq t 1 on 100f }",
    ]);
    assert_eq!(cp.engine_state.tempos.len(), 3);
    assert!(c.tempos.get(1).is_some());
    run(&mut cp, &mut c, "remove kick.s");
    assert_eq!(cp.engine_state.tempos.len(), 2);
    assert!(c.tempos.get(1).is_none());
    // the other seq still has its clock
    assert!(c.tempos.get(2).is_some());
    // a clock the seq only shared stays
    run(&mut cp, &mut c, "kick { seq u 1 }");
    run(&mut cp, &mut c, "remove kick.u");
    assert_eq!(cp.engine_state.tempos.len(), 2);
}

#[test]
fn group_seq_retriggers_members() {
    let (cp, mut c) = session(&[
        "voice a from \"kick\" on 1000ms",
        "voice b from \"kick\" on 1000ms",
        "group drums on 1000f { a; b; seq hits 1 3 }",
        "start drums",
    ]);
    assert_eq!(cp.engine_state.groups.len(), 1);
    // both members hit on the group's steps: -1 + -1
    let out = render_i16(&mut c, 1, 4500);
    let both: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, s)| **s == -2)
        .map(|(f, _)| f)
        .collect();
    assert_eq!(both, vec![0, 2000, 4000]);
}

#[test]
fn group_seq_parks_members_on_start() {
    let (mut cp, mut c) = session(&[
        "voice a from \"kick\" on 1000ms",
        "voice b from \"kick\" on 1000ms",
        "group drums on 1000f { a; b }",
        "drums { seq hits 2 }",
        "start drums",
    ]);
    // silent until step 2 (tick 1), then both come in together
    let out = render_i16(&mut c, 1, 1500);
    assert!(out[..1000].iter().all(|s| *s == 0));
    assert_eq!(out[1000], -2);
    // a seq-less group plays at once
    run(&mut cp, &mut c, "remove drums.hits");
    run(&mut cp, &mut c, "start drums");
    assert_eq!(render_i16(&mut c, 1, 10)[0], -2);
}

#[test]
fn group_seq_resume_waits_for_the_next_step() {
    let (mut cp, mut c) = session(&[
        "clock clk 1000f",
        "voice a from \"kick\" on 1000ms",
        "group drums on clk { a }",
        "drums { seq hits 1 3 }",
        "start clk and start drums",
    ]);
    render_i16(&mut c, 1, 2500);
    run(&mut cp, &mut c, "pause drums");
    render_i16(&mut c, 1, 700); // the clock ticks on, now at tick 3.2
    run(&mut cp, &mut c, "resume drums");
    // the member waits, silent, for the next step (tick 4 = frame 800 on)
    let out = render_i16(&mut c, 1, 1000);
    assert!(out[..790].iter().all(|s| *s == 0), "{:?}", &out[..5]);
    assert_eq!(hits(&out), vec![800]);
}

#[test]
fn pause_seq_voice_keeps_playing() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 2 3 4 on 1000f }",
        "start kick",
    ]);
    assert_eq!(hits(&render_i16(&mut c, 1, 1500)), vec![0, 1000]);
    run(&mut cp, &mut c, "pause kick.s");
    assert!(!cp.engine_state.procs[&ProcId(0)].enabled);
    // the hit plays out: no new triggers and no silence
    let out = render_i16(&mut c, 1, 2000);
    assert!(out.iter().all(|s| *s == 1), "{:?}", &out[..4]);
    run(&mut cp, &mut c, "resume kick.s");
    // position 3.5: the next step is tick 4 at frame 500 on, with no
    // catch-up of the steps it missed
    assert_eq!(hits(&render_i16(&mut c, 1, 1000)), vec![500]);
}

#[test]
fn stop_start_seq_resets() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 2 3 4 on 1000f }",
        "start kick",
    ]);
    render_i16(&mut c, 1, 1500);
    assert_eq!(state_of(&c, 0).idx, 2);
    run(&mut cp, &mut c, "stop kick.s");
    assert!(!state_of(&c, 0).active);
    assert_eq!(state_of(&c, 0).idx, 0);
    assert!(hits(&render_i16(&mut c, 1, 2000)).is_empty());
    run(&mut cp, &mut c, "start kick.s");
    assert!(state_of(&c, 0).active);
    assert!(cp.engine_state.procs[&ProcId(0)].enabled);
    // `start all` brings a paused seq back too
    run(&mut cp, &mut c, "pause kick.s");
    run(&mut cp, &mut c, "start all");
    assert!(state_of(&c, 0).active);
}

#[test]
fn chance_glides() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 2%100 on 1000f }",
        "start kick",
    ]);
    // step 1 follows the seq-wide chance, step 2 wrote its own
    render_i16(&mut c, 1, 10);
    run(&mut cp, &mut c, "kick.s chance=0 over 1000f");
    assert_eq!(state_of(&c, 0).feel[0].value, 100.0);
    render_i16(&mut c, 1, 500);
    let mid = state_of(&c, 0).feel[0].value;
    assert!((mid - 50.0).abs() < 1.0, "{mid}");
    render_i16(&mut c, 1, 600);
    assert_eq!(state_of(&c, 0).feel[0].value, 0.0);
    // the mirror holds the target at once
    assert_eq!(cp.engine_state.procs[&ProcId(0)].seq.feel[0], 0.0);
    // from here only step 2 fires: ticks 1, 5, 9 of the 4-tick cycle
    let out = hits(&render_i16(&mut c, 1, 8000));
    assert!(!out.is_empty());
    for f in out {
        // frames since the start: 1110 + f; step 2 is at 1000 mod 4000
        assert_eq!((1110 + f) % 4000, 1000, "{f}");
    }
}

#[test]
fn glide_on_a_stopped_seq_lands_at_once() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 on 1000f }",
    ]);
    run(&mut cp, &mut c, "kick.s chance=20 over 5000f");
    assert_eq!(state_of(&c, 0).feel[0].value, 20.0);
}

// JIT: the early/late feel moves hits

// hits of `kick { seq s 1 2 3 4 <feel> on 1000f }` over `frames`
fn feel_hits(feel: &str, frames: usize) -> Vec<usize> {
    let line = format!("kick {{ seq s 1 2 3 4 {feel} on 1000f }}");
    let (_, mut c) = session(&["voice kick from \"kick\" on 1000ms", &line, "start kick"]);
    hits(&render_i16(&mut c, 1, frames))
}

// each hit's step (a multiple of 1000 frames) and its distance from it
fn offsets(hits: &[usize]) -> Vec<(i64, i64)> {
    hits.iter()
        .map(|f| {
            let step = (*f as i64 + 500) / 1000;
            (step, *f as i64 - step * 1000)
        })
        .collect()
}

#[test]
fn jitter_moves_hits_within_bounds() {
    let h = feel_hits("jitter=0.1", 20_000);
    let o = offsets(&h);
    assert!(o.len() >= 20, "{o:?}");
    assert!(o.iter().all(|(_, d)| d.abs() <= 101), "{o:?}");
    assert!(o.iter().any(|(_, d)| *d != 0), "{o:?}");
    assert!(o.iter().any(|(_, d)| *d < 0) && o.iter().any(|(_, d)| *d > 0));
}

#[test]
fn late_only_never_early() {
    let o = offsets(&feel_hits("late=0.1", 20_000));
    assert!(o.iter().all(|(_, d)| (0..=101).contains(d)), "{o:?}");
    assert!(o.iter().any(|(_, d)| *d > 0));
}

#[test]
fn early_only_never_late() {
    let o = offsets(&feel_hits("early=0.1", 20_000));
    assert!(o.iter().all(|(_, d)| (-101..=0).contains(d)), "{o:?}");
    assert!(o.iter().any(|(_, d)| *d < 0));
}

#[test]
fn step_tilde_overrides_seq_feel() {
    let line = "kick { seq s 1~0 2 3 4 jitter=0.1 on 1000f }";
    let (_, mut c) = session(&["voice kick from \"kick\" on 1000ms", line, "start kick"]);
    let o = offsets(&hits(&render_i16(&mut c, 1, 20_000)));
    for (step, d) in &o {
        if step % 4 == 0 {
            assert_eq!(*d, 0, "{o:?}");
        }
    }
    assert!(o.iter().any(|(s, d)| s % 4 != 0 && *d != 0));
}

#[test]
fn seed_repeats_jitter() {
    let a = feel_hits("jitter=0.2 chance=70 seed=7", 20_000);
    let b = feel_hits("jitter=0.2 chance=70 seed=7", 20_000);
    let other = feel_hits("jitter=0.2 chance=70 seed=8", 20_000);
    assert_eq!(a, b);
    assert_ne!(a, other);
}

#[test]
fn early_first_step_fires_once_per_cycle() {
    for feel in ["early=0.4", "jitter=0.4", "late=0.4"] {
        let o = offsets(&feel_hits(feel, 40_000));
        let steps: Vec<i64> = o.iter().map(|(s, _)| *s).collect();
        // every step 0..=39 once (40 may or may not have come early)
        assert!((40..=41).contains(&steps.len()), "{feel}: {o:?}");
        assert_eq!(steps[..40], (0..40).collect::<Vec<i64>>()[..], "{feel}");
        // nothing before the start
        assert!(o[0].1 >= 0, "{feel}: {o:?}");
    }
}

#[test]
fn feel_glide_widens_spread() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\" on 1000ms",
        "kick { seq s 1 2 3 4 on 1000f }",
        "start kick",
    ]);
    let before = offsets(&hits(&render_i16(&mut c, 1, 8000)));
    assert!(before.iter().all(|(_, d)| *d == 0));
    run(&mut cp, &mut c, "kick.s jitter=0.3 over 8000f");
    let spread = |c: &mut Conductor| {
        let o = offsets(&hits(&render_i16(c, 1, 8000)));
        o.iter().map(|(_, d)| d.abs()).max().unwrap()
    };
    let (first, second) = (spread(&mut c), spread(&mut c));
    assert!(first < second, "{first} {second}");
    assert!(second <= 301);
}

#[test]
fn zero_feel_is_exact() {
    // a seq with offset state and one without (the old path) agree
    let line = "kick { seq s 1 1.5 3 4 chance=60 seed=3 in 4 on 1000f }";
    let play = |keep: bool| {
        let (_, mut c) = session(&["voice kick from \"kick\" on 1000ms", line, "start kick"]);
        if !keep
            && let Some(slot) = c.procs.get_mut(0)
            && let Process::Seq(s) = &mut slot.process
        {
            s.state.pend = Vec::new();
        }
        hits(&render_i16(&mut c, 1, 30_000))
    };
    let (with, without) = (play(true), play(false));
    assert_eq!(with, without);
    assert!(with.len() > 10);
}
