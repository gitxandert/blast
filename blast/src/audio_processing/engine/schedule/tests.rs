//! SCHED: `at`, `after` and `by`, against a real `Conductor`. One channel,
//! so a clock ticks once per frame; each voice's track starts with a -1
//! sample, so a frame holds -1 only when a voice is (re)triggered.

use super::*;
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::lang::Code;
use crate::audio_processing::engine::arena::Limits;
use crate::audio_processing::test_util::{pcm, render, render_i16, run, rx, setup_with};
use crate::file_parsing::decode_helpers::AudioFile;

fn spike(name: &str) -> AudioFile {
    let mut samples = vec![1i16; 100_000];
    samples[0] = -1;
    AudioFile::new(name, "wav", 48000, 1, 16, pcm(&samples))
}

fn session_with(limits: Limits) -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup_with(limits, 1, vec![spike("kick"), spike("hat")]);
    c.apply(cp.boot());
    (cp, c)
}

fn session() -> (CmdProcessor, Conductor) {
    session_with(Limits::default())
}

// a slow clock `c` (1000 frames a tick), running, with `kick` and `hat` on it
// (not started)
fn on_c() -> (CmdProcessor, Conductor) {
    let (mut cp, mut c) = session();
    for line in [
        "clock c 1000f",
        "start c",
        "voice kick from \"kick\" on c",
        "voice hat from \"hat\" on c",
    ] {
        run(&mut cp, &mut c, line);
    }
    (cp, c)
}

fn triggers(out: &[i16]) -> Vec<usize> {
    out.iter()
        .enumerate()
        .filter(|(_, s)| **s == -1)
        .map(|(f, _)| f)
        .collect()
}

fn near(got: &[usize], want: &[usize]) {
    assert_eq!(got.len(), want.len(), "got {got:?}, want {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!(g.abs_diff(*w) <= 1, "got {got:?}, want {want:?}");
    }
}

fn code(cp: &mut CmdProcessor, line: &str) -> Code {
    match cp.parse(line) {
        Err(CmdErr::Lang(errs)) => errs[0].code,
        Err(e) => panic!("'{line}': {e}"),
        Ok(_) => panic!("'{line}' should have failed"),
    }
}

fn interval(cp: &CmdProcessor, c: &Conductor, name: &str) -> f32 {
    let id = cp.engine_state.context_names[name];
    c.tempos.get(id.index()).unwrap().state.interval
}

#[test]
fn at_waits_for_next_multiple() {
    let (mut cp, mut c) = on_c();
    render(&mut c, 1, 2300);
    run(&mut cp, &mut c, "start kick at 4");
    assert_eq!(c.schedule.waiting(), 1);
    let out = render_i16(&mut c, 1, 3000);
    // 2.3 ticks in; the clock reaches 4.0 at frame 4000 of its own count
    near(&triggers(&out), &[1700]);
    assert_eq!(c.schedule.waiting(), 0);
}

#[test]
fn at_on_exact_boundary_waits_a_full_period() {
    let (mut cp, mut c) = on_c();
    render(&mut c, 1, 4000);
    run(&mut cp, &mut c, "start kick at 4");
    let out = render_i16(&mut c, 1, 5000);
    // "at" is never "now": the next multiple is 8
    near(&triggers(&out), &[4000]);
}

#[test]
fn after_counts_from_now() {
    let (mut cp, mut c) = on_c();
    render(&mut c, 1, 2300);
    run(&mut cp, &mut c, "start kick after 4");
    let out = render_i16(&mut c, 1, 5000);
    near(&triggers(&out), &[4000]);
}

#[test]
fn by_other_clock() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "clock d 500f");
    run(&mut cp, &mut c, "start d");
    // kick is on c (1000f), but d (500f) counts: 4 ticks of d = 2000 frames
    run(&mut cp, &mut c, "start kick at 4 by d");
    let out = render_i16(&mut c, 1, 5000);
    near(&triggers(&out), &[2000]);
}

#[test]
fn counts_on_the_first_targets_clock() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "clock d 500f");
    run(&mut cp, &mut c, "start d");
    // hat is first, and on c
    run(&mut cp, &mut c, "start hat kick after 2");
    let out = render_i16(&mut c, 1, 5000);
    // both spikes sum to -2
    let at = out.iter().position(|s| *s < -1).expect("both trigger");
    assert!(at.abs_diff(2000) <= 1, "{at}");
}

#[test]
fn scheduled_batch_applies_together() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "start kick and start hat after 2");
    // both are one entry, and both sound on the due frame
    assert_eq!(c.schedule.waiting(), 1);
    let out = render_i16(&mut c, 1, 3000);
    // kick and hat have the same spike, so a mix of two is -2 there
    let at = out.iter().position(|s| *s < -1).expect("both trigger");
    assert!(at.abs_diff(2000) <= 1, "{at}");
}

#[test]
fn paused_clock_holds_schedule() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "start kick after 1");
    run(&mut cp, &mut c, "pause c");
    let out = render_i16(&mut c, 1, 5000);
    assert!(triggers(&out).is_empty());
    assert_eq!(c.schedule.waiting(), 1);
    run(&mut cp, &mut c, "resume c");
    let out = render_i16(&mut c, 1, 2000);
    near(&triggers(&out), &[1000]);
}

#[test]
fn cannot_schedule_creation() {
    let (mut cp, _c) = on_c();
    let before = (
        cp.engine_state.voices.len(),
        cp.engine_state.tempos.len(),
        cp.engine_state.scheduled_pending(),
    );
    for line in [
        "voice z from \"kick\" at 4 by c",
        "voice z from \"kick\" on 90b after 4",
        "clock q 90b at 4 by main",
        "seq top 1 2 | trig kick after 4",
        "group g { kick } at 4",
        "kick on 100b at 4",
    ] {
        assert_eq!(code(&mut cp, line), Code::CannotWait, "{line}");
    }
    assert_eq!(Code::CannotWait.as_str(), "E410");
    // nothing changed, and nothing waits
    let after = (
        cp.engine_state.voices.len(),
        cp.engine_state.tempos.len(),
        cp.engine_state.scheduled_pending(),
    );
    assert_eq!(before, after);
}

#[test]
fn cannot_schedule_remove() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "clock spare 90b");
    run(&mut cp, &mut c, "voice snare from \"hat\" on c");
    run(&mut cp, &mut c, "group g { snare }");
    for line in [
        "remove kick after 4",
        "remove spare at 4 by main",
        "ungroup g after 4",
        "remove all after 4 by main",
    ] {
        assert_eq!(code(&mut cp, line), Code::CannotWait, "{line}");
    }
    assert!(cp.engine_state.voice_id("kick").is_ok());
    assert!(cp.engine_state.group_id("g").is_ok());
}

#[test]
fn only_statements_about_objects_wait() {
    let (mut cp, _c) = on_c();
    assert_eq!(code(&mut cp, "let n = 3 at 4 by c"), Code::Unexpected);
    assert_eq!(code(&mut cp, "state at 4 by c"), Code::Unexpected);
    assert_eq!(code(&mut cp, "start kick at 0"), Code::OutOfRange);
}

#[test]
fn schedule_full_errors() {
    let limits = Limits {
        scheduled: 2,
        ..Limits::default()
    };
    let (mut cp, mut c) = session_with(limits);
    let mut rx = rx(&mut c);
    for line in [
        "clock c 1000f",
        "start c",
        "voice kick from \"kick\" on c",
        "start kick after 1",
        "kick r=1.5 after 2",
    ] {
        run(&mut cp, &mut c, line);
    }
    assert_eq!(c.schedule.waiting(), 2);
    let full = code(&mut cp, "kick r=2 after 3");
    assert_eq!(full, Code::LimitReached);
    // nothing changed for the refused line
    assert_eq!(cp.engine_state.scheduled_pending(), 2);
    // after one fires and the REPL hears of it, there is room again
    render(&mut c, 1, 1500);
    for e in rx.events() {
        if let Event::ScheduleFired {
            seq_no,
            total,
            applied,
        } = e
        {
            cp.engine_state.note_schedule_fired(seq_no, total, applied);
        }
    }
    assert_eq!(cp.engine_state.scheduled_pending(), 1);
    run(&mut cp, &mut c, "kick r=2 after 3");
    assert_eq!(c.schedule.waiting(), 2);
}

#[test]
fn a_lost_event_is_made_good_by_the_next() {
    let (mut cp, mut c) = on_c();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "kick r=2 after 1");
    render(&mut c, 1, 1500);
    run(&mut cp, &mut c, "kick r=1 after 1");
    render(&mut c, 1, 1500);
    // the first event never arrives
    let fired: Vec<(u64, u64)> = rx
        .events()
        .filter_map(|e| match e {
            Event::ScheduleFired { seq_no, total, .. } => Some((seq_no, total)),
            _ => None,
        })
        .collect();
    assert_eq!(fired, vec![(0, 1), (1, 2)]);
    cp.engine_state.note_schedule_fired(1, 2, true);
    assert_eq!(cp.engine_state.scheduled_pending(), 0);
}

#[test]
fn fired_box_retired_through_garbage() {
    let (mut cp, mut c) = on_c();
    let mut rx = rx(&mut c);
    // the batch's own Vec and the box both leave as garbage
    run(&mut cp, &mut c, "start kick and start hat after 1");
    render(&mut c, 1, 1500);
    let mut boxes = 0;
    while let Ok(g) = rx.garbage.pop() {
        if let Garbage::Scheduled(b) = g {
            // its contents were moved out and applied
            assert!(matches!(*b, Command::Batch(ref a) if a.cmds.is_empty()));
            boxes += 1;
        }
    }
    assert_eq!(boxes, 1);
}

#[test]
fn scheduled_retime_and_glide() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "clock x 120b");
    run(&mut cp, &mut c, "start x");
    run(&mut cp, &mut c, "clock x 240b after 1");
    // a tick of x is 24000 frames
    render(&mut c, 1, 20000);
    assert_eq!(interval(&cp, &c, "x"), 24000.0);
    render(&mut c, 1, 5000);
    assert_eq!(interval(&cp, &c, "x"), 12000.0);

    run(&mut cp, &mut c, "clock x 120b over 1000f after 1");
    // x is 1.08 ticks in, so the line fires 12000 frames on and the glide
    // takes 1000 more
    render(&mut c, 1, 11000);
    assert_eq!(interval(&cp, &c, "x"), 12000.0);
    render(&mut c, 1, 1500);
    let mid = interval(&cp, &c, "x");
    assert!(mid > 12000.0 && mid < 24000.0, "{mid}");
    render(&mut c, 1, 1000);
    assert_eq!(interval(&cp, &c, "x"), 24000.0);
}

#[test]
fn scheduled_start_counts_as_running_for_later_glides() {
    // the mirror records a scheduled line at once, so the clock counts as
    // running for the glide after it; the engine holds the glide's effect
    // until the scheduled start has run
    let (mut cp, mut c) = session();
    run(&mut cp, &mut c, "clock a 120b");
    assert_eq!(code(&mut cp, "clock a 140b over 4"), Code::ClockOff);
    run(&mut cp, &mut c, "start a at 1 by main");
    run(&mut cp, &mut c, "clock a 240b over 1000f");
    // not running yet: the rate still stands
    render(&mut c, 1, 2000);
    assert_eq!(interval(&cp, &c, "a"), 24000.0);
    // main's first tick is 24000 frames; the start fires there, and the
    // glide then plays
    render(&mut c, 1, 24000);
    render(&mut c, 1, 1500);
    assert_eq!(interval(&cp, &c, "a"), 12000.0);
}

#[test]
fn removed_clock_drops_the_waiting_line() {
    let (mut cp, mut c) = session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "clock a 120b");
    run(&mut cp, &mut c, "start a");
    run(&mut cp, &mut c, "clock a 90b after 1");
    run(&mut cp, &mut c, "remove a");
    render(&mut c, 1, 10);
    // it went without being applied, and its slot is free
    assert_eq!(c.schedule.waiting(), 0);
    assert!(rx.events().any(|e| e
        == Event::ScheduleFired {
            seq_no: 0,
            total: 1,
            applied: false
        }));
}

// like `run`, but returns what the line printed (warnings)
fn say(cp: &mut CmdProcessor, c: &mut Conductor, line: &str) -> String {
    let (mut tx, mut rx) = cmd_queue(4);
    let text = cp
        .submit(line, &mut tx)
        .unwrap_or_else(|e| panic!("{line}: {e}"));
    while let Some(cmd) = rx.try_pop() {
        c.apply(cmd);
    }
    text.unwrap_or_default()
}

#[test]
fn remove_drops_waiting_line_w103() {
    let (mut cp, mut c) = on_c();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "start kick and start hat at 4");
    run(&mut cp, &mut c, "hat gain=0.5 after 3");
    assert_eq!(c.schedule.waiting(), 2);
    let text = say(&mut cp, &mut c, "remove kick");
    assert!(
        text.contains(
            "W103 the waiting line 'start kick and start hat at 4' is dropped (it names kick)"
        ),
        "{text}"
    );
    // the engine dropped it at once, unapplied, and the other line stays
    assert_eq!(c.schedule.waiting(), 1);
    assert_eq!(cp.engine_state.waiting.len(), 1);
    // a new voice takes the freed ID; no line reaches it
    run(&mut cp, &mut c, "voice snare from \"hat\" on c");
    assert_eq!(
        cp.engine_state.voice_id("snare").unwrap(),
        VoiceId(0),
        "the ID is reused"
    );
    render(&mut c, 1, 4500);
    let out = render_i16(&mut c, 1, 4000);
    assert!(triggers(&out).is_empty());
    assert!(!c.voices.get(0).unwrap().state.active);
    // the dropped line's box left as garbage, and the mirror was told
    let mut boxes = 0;
    while let Ok(g) = rx.garbage.pop() {
        boxes += usize::from(matches!(g, Garbage::Scheduled(_)));
    }
    assert!(boxes >= 1);
    let fired: Vec<Event> = rx.events().collect();
    assert!(
        fired.contains(&Event::ScheduleFired {
            seq_no: 0,
            total: 1,
            applied: false
        }),
        "{fired:?}"
    );
}

#[test]
fn removed_effect_drops_waiting_line_w103() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "kick { filter lo }");
    run(&mut cp, &mut c, "kick.lo cutoff=2000 at 16");
    assert_eq!(c.schedule.waiting(), 1);
    let text = say(&mut cp, &mut c, "remove kick.lo");
    assert!(text.contains("W103") && text.contains("names lo"), "{text}");
    assert_eq!(c.schedule.waiting(), 0);
    assert!(cp.engine_state.waiting.is_empty());
    // a new filter takes the id; the line is gone, so it isn't touched
    run(&mut cp, &mut c, "kick { filter hi }");
    let v = cp.engine_state.voice_id("kick").unwrap();
    assert_eq!(cp.engine_state.fx_list(FxOwner::Voice(v))[0].id, 0);
    assert_eq!(c.schedule.waiting(), 0);
}

#[test]
fn remove_other_kinds_drops_waiting_lines() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "clock x 120b");
    run(&mut cp, &mut c, "kick { seq beat 1 }");
    run(&mut cp, &mut c, "group g { hat }");
    run(&mut cp, &mut c, "start x");
    run(&mut cp, &mut c, "clock x 90b after 2");
    run(&mut cp, &mut c, "pause kick.beat after 2 by c");
    run(&mut cp, &mut c, "g gain=0.5 after 2 by c");
    assert_eq!(c.schedule.waiting(), 3);
    let text = say(&mut cp, &mut c, "remove x");
    assert!(text.contains("W103") && text.contains("names x"), "{text}");
    assert_eq!(c.schedule.waiting(), 2);
    let text = say(&mut cp, &mut c, "remove kick.beat");
    assert!(text.contains("names kick.beat"), "{text}");
    assert_eq!(c.schedule.waiting(), 1);
    let text = say(&mut cp, &mut c, "remove g");
    assert!(text.contains("names g"), "{text}");
    assert_eq!(c.schedule.waiting(), 0);
    assert!(cp.engine_state.waiting.is_empty());
}

#[test]
fn waiting_line_names_ids_walk_is_exhaustive() {
    // `visit_ids` and `creates_or_removes` match every variant of `Command`
    // with no wildcard arm, so a new command fails to compile until it says
    // what it names. This checks that the walk looks inside a scheduled line.
    use crate::audio_processing::commands::{AnyId, names_id};
    let (mut cp, _c) = on_c();
    let kick = cp.engine_state.voice_id("kick").unwrap();
    let hat = cp.engine_state.voice_id("hat").unwrap();
    let c_clock = cp.engine_state.context_id("c").unwrap();
    let Ok(Parsed::Engine(Command::Schedule(s))) = cp.parse("start kick and start hat after 2")
    else {
        panic!("a scheduled line");
    };
    assert!(names_id(&s.cmd, AnyId::Voice(kick)));
    assert!(names_id(&s.cmd, AnyId::Voice(hat)));
    assert!(!names_id(&s.cmd, AnyId::Tempo(c_clock)));
    let whole = Command::Schedule(s);
    assert!(names_id(&whole, AnyId::Tempo(c_clock)));
    assert!(!names_id(&whole, AnyId::Group(GroupId(0))));
}

#[test]
fn lost_fired_event_keeps_list_until_pending_zero() {
    let (mut cp, mut c) = on_c();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "start kick after 1");
    run(&mut cp, &mut c, "start hat after 2");
    assert_eq!(cp.engine_state.waiting.len(), 2);
    render(&mut c, 1, 1500);
    let first: Vec<Event> = rx.events().collect();
    assert_eq!(first.len(), 1);
    // that event is lost; the mirror still lists both, and pending shows 2
    assert_eq!(cp.engine_state.waiting.len(), 2);
    assert_eq!(cp.engine_state.scheduled_pending(), 2);
    render(&mut c, 1, 1000);
    let Some(Event::ScheduleFired { seq_no, total, .. }) = rx.events().next() else {
        panic!("the second event");
    };
    assert_eq!((seq_no, total), (1, 2));
    cp.engine_state.note_schedule_fired(seq_no, total, true);
    // total says nothing is pending, so the stale first entry goes too
    assert_eq!(cp.engine_state.scheduled_pending(), 0);
    assert!(cp.engine_state.waiting.is_empty());
}

#[test]
fn counting_on_stopped_clock_e409() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "voice lead from \"kick\" on 120b");
    let Err(CmdErr::Lang(errs)) = cp.parse("start lead at 16") else {
        panic!("should fail");
    };
    assert_eq!(errs[0].code, Code::ClockOff);
    assert_eq!(
        errs[0].message,
        "clock 'lead' is off, so it can't count; add by main"
    );
    assert_eq!(cp.engine_state.scheduled_pending(), 0);
    // by a running clock it is fine
    run(&mut cp, &mut c, "start lead at 16 by main");
    // and a stopped `by` clock is no better
    run(&mut cp, &mut c, "clock idle 100f");
    assert_eq!(code(&mut cp, "start kick at 4 by idle"), Code::ClockOff);
}

#[test]
fn counting_on_paused_clock_waits() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "pause c");
    run(&mut cp, &mut c, "start kick after 1");
    assert_eq!(c.schedule.waiting(), 1);
    let out = render_i16(&mut c, 1, 3000);
    assert!(triggers(&out).is_empty());
    run(&mut cp, &mut c, "resume c");
    let out = render_i16(&mut c, 1, 3000);
    assert_eq!(triggers(&out).len(), 1);
}

#[test]
fn schedule_full_e601() {
    let limits = Limits {
        scheduled: 1,
        ..Limits::default()
    };
    let (mut cp, mut c) = session_with(limits);
    for line in [
        "clock c 1000f",
        "start c",
        "voice kick from \"kick\" on c",
        "start kick after 1",
    ] {
        run(&mut cp, &mut c, line);
    }
    assert_eq!(code(&mut cp, "kick r=2 after 3"), Code::LimitReached);
    assert_eq!(Code::LimitReached.as_str(), "E601");
}

// ---- then, thenif, wait (PKa) ----

use crate::audio_processing::engine::events::describe;
use crate::audio_processing::engine::{EngineRx, VoiceParam};
use crate::audio_processing::test_util::{ctx, gid, vid};

// `c` at 1000 frames a tick, four voices on it, all playing
fn chain_session() -> (CmdProcessor, Conductor) {
    let (mut cp, mut c) = on_c();
    for line in [
        "voice snare from \"kick\" on c",
        "voice tom from \"hat\" on c",
        "start kick",
        "start hat",
        "start snare",
        "start tom",
    ] {
        run(&mut cp, &mut c, line);
    }
    (cp, c)
}

fn gain_at(c: &Conductor, v: VoiceId) -> f32 {
    c.voices
        .get(v.index())
        .unwrap()
        .state
        .get_param(VoiceParam::Gain)
}

fn gain_of(cp: &CmdProcessor, c: &Conductor, name: &str) -> f32 {
    gain_at(c, vid(cp, name))
}

// renders one frame at a time until `done`; the number of frames rendered
fn frames_until(c: &mut Conductor, max: usize, mut done: impl FnMut(&Conductor) -> bool) -> usize {
    for n in 1..=max {
        render(c, 1, 1);
        if done(c) {
            return n;
        }
    }
    panic!("not done after {max} frames");
}

// what the REPL does with the engine's events: feed the mirror, collect the
// texts it would print
fn pump(cp: &mut CmdProcessor, rx: &mut EngineRx) -> Vec<String> {
    let mut printed = Vec::new();
    for e in rx.events() {
        match e {
            Event::ScheduleFired {
                seq_no,
                total,
                applied,
            } => cp.engine_state.note_schedule_fired(seq_no, total, applied),
            other => printed.push(describe(&other, &cp.engine_state)),
        }
    }
    printed
}

fn settled(cp: &mut CmdProcessor, c: &Conductor, rx: &mut EngineRx) {
    pump(cp, rx);
    assert_eq!(cp.engine_state.scheduled_pending(), 0);
    assert_eq!(c.schedule.waiting(), 0);
    assert!(cp.engine_state.waiting.is_empty());
}

#[test]
fn then_waits_for_longest_glide() {
    let (mut cp, mut c) = chain_session();
    let snare = vid(&cp, "snare");
    run(
        &mut cp,
        &mut c,
        "kick gain=0 over 2b and hat gain=0 over 3b then snare gain=0.5",
    );
    // the right side applies on exactly the frame the 3-tick glide is over
    let n = frames_until(&mut c, 5000, |c| gain_at(c, snare) == 0.5);
    assert_eq!(n, 3001);
    assert_eq!(gain_of(&cp, &c, "hat"), 0.0);
}

#[test]
fn then_after_action_is_immediate() {
    let (mut cp, mut c) = chain_session();
    run(
        &mut cp,
        &mut c,
        "snare gain=0.5 then snare gain=0.25 and tom gain=0.5",
    );
    // nothing glides, so all of it runs in the first frame, in order
    render(&mut c, 1, 1);
    assert_eq!(gain_of(&cp, &c, "snare"), 0.25);
    assert_eq!(gain_of(&cp, &c, "tom"), 0.5);
}

#[test]
fn then_chain_three() {
    let (mut cp, mut c) = chain_session();
    let snare = vid(&cp, "snare");
    let mut rx = rx(&mut c);
    run(
        &mut cp,
        &mut c,
        "kick gain=0 over 1b then hat gain=0 over 2b then snare gain=0.5",
    );
    let n = frames_until(&mut c, 5000, |c| gain_at(c, snare) == 0.5);
    assert_eq!(n, 3001);
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn then_runs_after_retarget() {
    let (mut cp, mut c) = chain_session();
    let snare = vid(&cp, "snare");
    run(&mut cp, &mut c, "kick gain=0 over 2b then snare gain=0.5");
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "kick gain=1 over 4b");
    // `then` runs whatever happened to the left side, at the original due
    let n = frames_until(&mut c, 5000, |c| gain_at(c, snare) == 0.5);
    assert_eq!(n, 1501);
}

#[test]
fn then_removed_object_drops_statement_w103() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(
        &mut cp,
        &mut c,
        "kick gain=0 over 4b then hat gain=0.5 and snare gain=0.5 then tom gain=0.5",
    );
    let text = say(&mut cp, &mut c, "remove hat");
    assert_eq!(text.matches("W103").count(), 1, "{text}");
    assert!(text.contains("names hat"), "{text}");
    // snare and tom (the part after the next `then`) still run
    let tom = vid(&cp, "tom");
    render(&mut c, 1, 4000);
    assert_eq!(gain_of(&cp, &c, "snare"), 1.0);
    render(&mut c, 1, 2);
    assert_eq!(gain_of(&cp, &c, "snare"), 0.5);
    assert_eq!(gain_at(&c, tom), 0.5);
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn then_removed_object_in_a_later_part_keeps_the_rest() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let tom = vid(&cp, "tom");
    run(
        &mut cp,
        &mut c,
        "kick gain=0 over 4b then hat gain=0.5 then snare gain=0.5 then tom gain=0.5",
    );
    let text = say(&mut cp, &mut c, "remove snare");
    assert_eq!(text.matches("W103").count(), 1, "{text}");
    assert!(text.contains("names snare"), "{text}");
    render(&mut c, 1, 4000);
    assert_eq!(gain_at(&c, tom), 1.0);
    // the part after the removed statement still runs on its due frame
    render(&mut c, 1, 1);
    assert_eq!(gain_at(&c, tom), 0.5);
    assert_eq!(gain_of(&cp, &c, "hat"), 0.5);
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn thenif_retarget_drops_rest_w105() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(
        &mut cp,
        &mut c,
        "kick gain=0 over 2b thenif snare gain=0.5 and tom gain=0.5",
    );
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "kick gain=1 over 4b");
    render(&mut c, 1, 4000);
    assert_eq!(gain_of(&cp, &c, "snare"), 1.0);
    assert_eq!(gain_of(&cp, &c, "tom"), 1.0);
    let printed = pump(&mut cp, &mut rx);
    assert_eq!(
        printed,
        ["W105 thenif: the glide on kick.gain was cut short; the rest of the line was dropped"]
    );
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn thenif_set_without_over_drops_rest() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "kick gain=0 over 2b thenif snare gain=0.5");
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "kick gain=1");
    render(&mut c, 1, 3000);
    assert_eq!(gain_of(&cp, &c, "snare"), 1.0);
    assert_eq!(pump(&mut cp, &mut rx).len(), 1);
}

#[test]
fn thenif_success_runs() {
    let (mut cp, mut c) = chain_session();
    let snare = vid(&cp, "snare");
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "kick gain=0 over 2b thenif snare gain=0.5");
    let n = frames_until(&mut c, 5000, |c| gain_at(c, snare) == 0.5);
    assert_eq!(n, 2001);
    assert!(pump(&mut cp, &mut rx).is_empty());
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn thenif_after_jump_succeeds() {
    let (mut cp, mut c) = chain_session();
    run(&mut cp, &mut c, "stop kick");
    // a voice that isn't playing jumps: nothing to cut short
    run(&mut cp, &mut c, "kick gain=0 over 2b thenif snare gain=0.5");
    render(&mut c, 1, 2);
    assert_eq!(gain_of(&cp, &c, "snare"), 0.5);
}

#[test]
fn then_thenif_chain() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let line =
        "kick gain=0 over 1b then hat gain=0 over 1b thenif snare gain=0.5 then tom gain=0.5";
    run(&mut cp, &mut c, line);
    // untouched: snare at 2 ticks, tom with it (the snare doesn't glide)
    let tom = vid(&cp, "tom");
    let n = frames_until(&mut c, 5000, |c| gain_at(c, tom) == 0.5);
    assert_eq!(n, 2001);
    assert_eq!(gain_of(&cp, &c, "snare"), 0.5);
    settled(&mut cp, &c, &mut rx);

    // the hat's glide (the thenif's left side) is cut short: the snare and
    // everything after the thenif are dropped
    run(
        &mut cp,
        &mut c,
        "snare gain=1 and tom gain=1 and kick gain=1 and hat gain=1",
    );
    render(&mut c, 1, 10);
    run(&mut cp, &mut c, line);
    render(&mut c, 1, 1500);
    run(&mut cp, &mut c, "hat gain=1 over 3b");
    render(&mut c, 1, 4000);
    assert_eq!(gain_of(&cp, &c, "snare"), 1.0);
    assert_eq!(gain_of(&cp, &c, "tom"), 1.0);
    assert_eq!(pump(&mut cp, &mut rx).len(), 1);
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn pending_returns_to_zero() {
    let abc = "kick gain=0 over 1b then hat gain=0 over 1b then snare gain=0.5";
    let thenif = "kick gain=0 over 1b thenif hat gain=0 over 1b then snare gain=0.5";
    for (line, after) in [
        (
            "kick gain=0 over 1b then snare gain=0.5 and tom gain=0.5",
            "",
        ),
        ("kick gain=0 over 1b thenif snare gain=0.5", "kick gain=1"),
        (abc, "remove tom"),
        (abc, "remove hat"),
        (abc, "remove snare"),
        (
            "kick gain=0 over 1b then hat gain=0.5 then snare gain=0.5 then tom gain=0.5",
            "remove snare",
        ),
        (
            "kick gain=0 over 1b then hat gain=0.5 then snare gain=0.5 then tom gain=0.5",
            "remove hat",
        ),
        (
            "wait 2 by c then hat gain=0.5 then snare gain=0.5 then tom gain=0.5",
            "remove hat",
        ),
        (thenif, "remove snare"),
        (thenif, "remove kick"),
        (thenif, "hat gain=1"),
        ("kick gain=0 over 1b then snare gain=0.5 at 4", ""),
        (
            "wait 2 by c then snare gain=0.5 and tom gain=0.5 then hat gain=0.5",
            "remove hat",
        ),
    ] {
        let (mut cp, mut c) = chain_session();
        let mut rx = rx(&mut c);
        run(&mut cp, &mut c, line);
        render(&mut c, 1, 300);
        if !after.is_empty() {
            run(&mut cp, &mut c, after);
        }
        render(&mut c, 1, 12000);
        pump(&mut cp, &mut rx);
        assert_eq!(c.schedule.waiting(), 0, "{line} / {after}");
        assert_eq!(cp.engine_state.scheduled_pending(), 0, "{line} / {after}");
        assert!(cp.engine_state.waiting.is_empty(), "{line} / {after}");
    }
}

#[test]
fn then_cannot_stage_buffers_e410() {
    let (mut cp, _c) = chain_session();
    let before = cp.engine_state.scheduled_pending();
    for line in [
        "kick gain=0 over 1b thenif buffer b length=4f",
        "kick gain=0 over 1b then snare gain=1 and kick on 100b",
        "wait 1 then buffer b length=4f",
    ] {
        assert_eq!(code(&mut cp, line), Code::CannotWait, "{line}");
    }
    assert_eq!(cp.engine_state.scheduled_pending(), before);
}

#[test]
fn only_statements_about_objects_follow_then() {
    let (mut cp, _c) = chain_session();
    for line in [
        "kick gain=0 over 1b then let x = 1",
        "kick gain=0 over 1b then state",
    ] {
        assert_eq!(code(&mut cp, line), Code::Unexpected, "{line}");
    }
}

#[test]
fn at_line_with_then() {
    let (mut cp, mut c) = chain_session();
    let snare = vid(&cp, "snare");
    run(
        &mut cp,
        &mut c,
        "kick gain=0 over 2b then snare gain=0.5 at 4",
    );
    // nothing happens before the beat: the left side starts at tick 4
    render(&mut c, 1, 3990);
    assert_eq!(gain_of(&cp, &c, "kick"), 1.0);
    let n = frames_until(&mut c, 8000, |c| gain_at(c, snare) == 0.5);
    // the beat, then 2 ticks
    assert!((2009..=2013).contains(&n), "{n}");
}

#[test]
fn wait_then_runs_after() {
    let (mut cp, mut c) = chain_session();
    run(&mut cp, &mut c, "clock main 1000f");
    let snare = vid(&cp, "snare");
    run(&mut cp, &mut c, "wait 4 then snare gain=0.5");
    let n = frames_until(&mut c, 8000, |c| gain_at(c, snare) == 0.5);
    assert!((4000..=4002).contains(&n), "{n}");

    // time units are clock time, turned into ticks at the clock's rate
    let parsed = cp.parse("wait 2s by c then tom gain=0.5");
    let Ok(Parsed::Engine(Command::Schedule(s))) = parsed else {
        panic!("a scheduled line: {:?}", parsed.err());
    };
    let WhenSpec::After { ticks, .. } = s.when else {
        panic!("a wait is an After");
    };
    assert!((ticks - 96.0).abs() < 1e-6, "{ticks}");
    let Ok(Parsed::Engine(Command::Schedule(s))) = cp.parse("wait 3b by c then tom gain=0.4")
    else {
        panic!("a scheduled line");
    };
    assert!(matches!(s.when, WhenSpec::After { ticks, .. } if ticks == 3.0));
}

#[test]
fn wait_after_glide_then_more() {
    let (mut cp, mut c) = chain_session();
    let tom = vid(&cp, "tom");
    let mut rx = rx(&mut c);
    run(
        &mut cp,
        &mut c,
        "kick gain=0 over 1b then wait 2 by c then tom gain=0.5",
    );
    let n = frames_until(&mut c, 8000, |c| gain_at(c, tom) == 0.5);
    assert!((3000..=3003).contains(&n), "{n}");
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn wait_and_warns_w106() {
    let (mut cp, mut c) = chain_session();
    let text = say(&mut cp, &mut c, "wait 1 by c and snare gain=0.5");
    assert!(text.contains("W106"), "{text}");
    render(&mut c, 1, 1100);
    assert_eq!(gain_of(&cp, &c, "snare"), 0.5);
}

#[test]
fn lone_wait_is_e201() {
    let (mut cp, _c) = chain_session();
    assert_eq!(code(&mut cp, "wait 4"), Code::Unexpected);
    assert_eq!(
        code(&mut cp, "snare gain=0.5 then wait 4"),
        Code::Unexpected
    );
    assert_eq!(cp.engine_state.scheduled_pending(), 0);
}

#[test]
fn chain_counts_every_schedule_against_the_table() {
    let limits = Limits {
        scheduled: 3,
        ..Limits::default()
    };
    let (mut cp, mut c) = session_with(limits);
    for line in [
        "clock c 1000f",
        "start c",
        "voice kick from \"kick\" on c",
        "voice hat from \"hat\" on c",
    ] {
        run(&mut cp, &mut c, line);
    }
    // head, two statements and the tail for the next part: more than 3
    assert_eq!(
        code(
            &mut cp,
            "kick gain=0 over 1b then hat gain=0.5 and kick gain=1 then kick gain=0"
        ),
        Code::LimitReached
    );
    assert_eq!(cp.engine_state.scheduled_pending(), 0);
    run(&mut cp, &mut c, "kick gain=0 over 1b then hat gain=0.5");
    assert_eq!(cp.engine_state.scheduled_pending(), 1);
}

// ---- glide marks and generations ----

fn mark(c: &mut Conductor) -> u32 {
    let m = c.glide_mark;
    c.apply(Command::MarkGlides(MarkGlidesArgs {}));
    assert_eq!(c.glide_mark, 0);
    m
}

#[test]
fn voice_glide_sets_the_mark() {
    let (mut cp, mut c) = chain_session();
    mark(&mut c);
    run(&mut cp, &mut c, "kick gain=0 over 2b");
    run(&mut cp, &mut c, "hat gain=0 over 1b");
    assert_eq!(mark(&mut c), 2000);
    // a voice that isn't playing jumps: it adds nothing
    run(&mut cp, &mut c, "stop snare");
    run(&mut cp, &mut c, "snare gain=0 over 2b");
    assert_eq!(mark(&mut c), 0);
    // so does a plain set
    run(&mut cp, &mut c, "kick gain=0.5");
    assert_eq!(mark(&mut c), 0);
}

#[test]
fn voice_generation_changes_on_retarget() {
    let (mut cp, mut c) = chain_session();
    let k = vid(&cp, "kick");
    let gen_of = |c: &Conductor| {
        c.voices
            .get(k.index())
            .unwrap()
            .state
            .glide_generation(VoiceParam::Gain)
    };
    let g0 = gen_of(&c);
    run(&mut cp, &mut c, "kick gain=0 over 2b");
    let g1 = gen_of(&c);
    run(&mut cp, &mut c, "kick gain=1 over 2b");
    let g2 = gen_of(&c);
    run(&mut cp, &mut c, "kick gain=0.7");
    let g3 = gen_of(&c);
    assert!(g0 != g1 && g1 != g2 && g2 != g3);
    // another parameter doesn't touch it
    run(&mut cp, &mut c, "kick pan=0.5 over 1b");
    assert_eq!(gen_of(&c), g3);
}

#[test]
fn group_glide_mark_and_generation() {
    let (mut cp, mut c) = chain_session();
    run(&mut cp, &mut c, "group g { tom }");
    run(&mut cp, &mut c, "start g");
    mark(&mut c);
    let g = gid(&cp, "g");
    let gen0 = c.groups.get(g.index()).unwrap().state.generation;
    run(&mut cp, &mut c, "g gain=0.5 over 3000f");
    assert_eq!(mark(&mut c), 3000);
    let gen1 = c.groups.get(g.index()).unwrap().state.generation;
    run(&mut cp, &mut c, "g gain=1 over 1000f");
    assert_ne!(gen0, gen1);
    assert_ne!(gen1, c.groups.get(g.index()).unwrap().state.generation);
    run(&mut cp, &mut c, "stop g");
    mark(&mut c);
    run(&mut cp, &mut c, "g gain=0.5 over 3000f");
    assert_eq!(mark(&mut c), 0);
}

#[test]
fn tempo_glide_mark_and_generation() {
    let (mut cp, mut c) = chain_session();
    run(&mut cp, &mut c, "clock x 120b");
    run(&mut cp, &mut c, "start x");
    mark(&mut c);
    let x = ctx(&cp, "x");
    let gen0 = c.tempos.get(x.index()).unwrap().glide_gen;
    run(&mut cp, &mut c, "clock x 140b over 2b");
    // 2 ticks of 120b is a second
    assert_eq!(mark(&mut c), 48000);
    let gen1 = c.tempos.get(x.index()).unwrap().glide_gen;
    run(&mut cp, &mut c, "clock x 100b");
    assert_ne!(gen0, gen1);
    assert_ne!(gen1, c.tempos.get(x.index()).unwrap().glide_gen);
    assert_eq!(mark(&mut c), 0);
}

#[test]
fn fx_glide_mark_and_generation() {
    let (mut cp, mut c) = chain_session();
    run(&mut cp, &mut c, "kick { filter lo }");
    mark(&mut c);
    let k = vid(&cp, "kick");
    let gen_of = |c: &Conductor| {
        c.voices
            .get(k.index())
            .unwrap()
            .fx
            .generation(0, 0)
            .unwrap()
    };
    let g0 = gen_of(&c);
    run(&mut cp, &mut c, "kick.lo cutoff=500 over 1b");
    assert_eq!(mark(&mut c), 1000);
    let g1 = gen_of(&c);
    run(&mut cp, &mut c, "kick.lo cutoff=800");
    assert!(g0 != g1 && g1 != gen_of(&c));
}

#[test]
fn lfo_glide_mark_and_generation() {
    let (mut cp, mut c) = chain_session();
    run(&mut cp, &mut c, "kick { lfo wob depth=1 to=pan }");
    mark(&mut c);
    let id = *cp.engine_state.voices[&vid(&cp, "kick")]
        .procs
        .values()
        .next()
        .expect("an lfo");
    let g0 = c.procs.get(id.index()).unwrap().generation;
    run(&mut cp, &mut c, "kick.wob depth=0 over 1b");
    assert_eq!(mark(&mut c), 1000);
    assert_ne!(g0, c.procs.get(id.index()).unwrap().generation);
}

// ---- timed scripts (PKb1) ----

fn mark_entry(slot: u8, seq: u32, tempo: TempoId, ticks: f64, cmd: Command) -> Command {
    Command::Schedule(ScheduleArgs {
        when: WhenSpec::AfterMark {
            slot,
            seq,
            tempo,
            ticks,
        },
        cmd: Box::new(cmd),
        seq_no: 0,
        script: Some(slot),
    })
}

fn set_mark(slot: u8, seq: u32, tempo: TempoId) -> Command {
    Command::SetMark(SetMarkArgs { slot, seq, tempo })
}

fn start_cmd(v: VoiceId) -> Command {
    Command::Start(StartArgs {
        target: Target::Voice(v),
    })
}

#[test]
fn mark_entry_waits_for_its_mark_then_counts_from_it() {
    let (cp, mut c) = on_c();
    let tempo = ctx(&cp, "c");
    let kick = vid(&cp, "kick");
    c.apply(set_mark(0, 0, tempo));
    // due 2 ticks after mark 1, which is not set yet
    c.apply(mark_entry(0, 1, tempo, 2.0, start_cmd(kick)));
    let out = render_i16(&mut c, 1, 5000);
    assert!(triggers(&out).is_empty());
    assert_eq!(c.schedule.waiting(), 1);
    // mark 1 is set when the clock is 5 ticks on
    c.apply(set_mark(0, 1, tempo));
    let out = render_i16(&mut c, 1, 3000);
    near(&triggers(&out), &[2000]);
    assert_eq!(c.schedule.waiting(), 0);
}

#[test]
fn cancel_script_drops_its_entries_and_refuses_late_ones() {
    let (cp, mut c) = on_c();
    let tempo = ctx(&cp, "c");
    let kick = vid(&cp, "kick");
    c.apply(set_mark(0, 0, tempo));
    c.apply(mark_entry(0, 0, tempo, 4.0, start_cmd(kick)));
    // another script's entry is not touched
    c.apply(set_mark(1, 0, tempo));
    c.apply(mark_entry(1, 0, tempo, 4.0, start_cmd(kick)));
    assert_eq!(c.schedule.waiting(), 2);
    c.apply(Command::CancelScript(CancelScriptArgs {
        slot: 0,
        stops: Vec::new(),
    }));
    assert_eq!(c.schedule.waiting(), 1);
    // a segment that was already on its way is refused
    c.apply(mark_entry(0, 0, tempo, 4.0, start_cmd(kick)));
    assert_eq!(c.schedule.waiting(), 1);
    let out = render_i16(&mut c, 1, 5000);
    near(&triggers(&out), &[4000]);
}

#[test]
fn cancel_script_stops_only_what_still_exists() {
    let (mut cp, mut c) = on_c();
    let tempo = ctx(&cp, "c");
    run(&mut cp, &mut c, "start kick hat");
    let (kick, hat) = (vid(&cp, "kick"), vid(&cp, "hat"));
    c.apply(set_mark(0, 0, tempo));
    let stop = |v| {
        Command::Stop(StopArgs {
            target: Target::Voice(v),
        })
    };
    run(&mut cp, &mut c, "remove hat");
    assert!(c.voices.get(kick.index()).unwrap().state.active);
    c.apply(Command::CancelScript(CancelScriptArgs {
        slot: 0,
        stops: vec![stop(kick), stop(hat)],
    }));
    assert!(!c.voices.get(kick.index()).unwrap().state.active);
}

// ---- staged creation (PKb2) ----

fn live_voice(c: &Conductor, v: VoiceId) -> bool {
    c.voices.get(v.index()).is_some()
}

#[test]
fn then_creates_voice_on_due_frame() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(
        &mut cp,
        &mut c,
        "kick gain=0 over 2b then voice z from \"kick\" on c",
    );
    // the mirror has it at once, the engine only when the glide is over
    let z = vid(&cp, "z");
    assert!(!live_voice(&c, z));
    render(&mut c, 1, 1999);
    assert!(!live_voice(&c, z));
    render(&mut c, 1, 3);
    assert!(live_voice(&c, z));
    assert!(pump(&mut cp, &mut rx).is_empty());
    settled(&mut cp, &c, &mut rx);
    assert_eq!(vid(&cp, "z"), z);
}

#[test]
fn then_remove_after_fade() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let kick = vid(&cp, "kick");
    run(&mut cp, &mut c, "hat gain=0 over 2b thenif remove kick");
    // a line that waits sees it gone; a line typed now still names it
    assert!(cp.engine_state.voice_id("kick").is_err());
    run(&mut cp, &mut c, "kick pan=0.2");
    assert!(live_voice(&c, kick));
    render(&mut c, 1, 1900);
    assert!(live_voice(&c, kick));
    render(&mut c, 1, 200);
    assert!(!live_voice(&c, kick));
    assert!(pump(&mut cp, &mut rx).is_empty());
    settled(&mut cp, &c, &mut rx);
    // its ID is free again
    run(&mut cp, &mut c, "voice n from \"hat\" on c");
    assert_eq!(vid(&cp, "n"), kick);
}

#[test]
fn thenif_retarget_keeps_the_removed_object() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let kick = vid(&cp, "kick");
    run(&mut cp, &mut c, "hat gain=0 over 2b thenif remove kick");
    assert!(cp.engine_state.voice_id("kick").is_err());
    render(&mut c, 1, 500);
    // the glide is cut short: the rest of the line is dropped
    run(&mut cp, &mut c, "hat gain=1 over 4b");
    render(&mut c, 1, 4000);
    assert!(live_voice(&c, kick));
    let mut printed = pump(&mut cp, &mut rx);
    printed.extend(cp.engine_state.take_notes());
    assert!(printed.iter().any(|t| t.starts_with("W105")), "{printed:?}");
    assert!(
        printed.iter().any(|t| t.contains("kick stays")),
        "{printed:?}"
    );
    // the mirror has it again, under its name and ID
    assert_eq!(vid(&cp, "kick"), kick);
    run(&mut cp, &mut c, "kick gain=0.5");
    render(&mut c, 1, 2);
    assert_eq!(gain_of(&cp, &c, "kick"), 0.5);
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn dropped_line_frees_what_it_would_have_made() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let live = cp.engine_state.ids.voices.live();
    run(
        &mut cp,
        &mut c,
        "hat gain=0 over 2b thenif voice z from \"kick\" on c",
    );
    let z = vid(&cp, "z");
    assert_eq!(cp.engine_state.ids.voices.live(), live + 1);
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "hat gain=1");
    render(&mut c, 1, 3000);
    assert!(!live_voice(&c, z));
    let mut printed = pump(&mut cp, &mut rx);
    printed.extend(cp.engine_state.take_notes());
    assert!(
        printed.iter().any(|t| t.contains("z was not created")),
        "{printed:?}"
    );
    assert!(cp.engine_state.voice_id("z").is_err());
    assert_eq!(cp.engine_state.ids.voices.live(), live);
    settled(&mut cp, &c, &mut rx);
    // the ID can be had again
    run(&mut cp, &mut c, "voice y from \"kick\" on c");
    assert_eq!(vid(&cp, "y"), z);
}

#[test]
fn held_id_not_reused() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let kick = vid(&cp, "kick");
    run(&mut cp, &mut c, "hat gain=0 over 2b thenif remove kick");
    run(&mut cp, &mut c, "voice n from \"hat\" on c");
    assert_ne!(vid(&cp, "n"), kick);
    render(&mut c, 1, 2100);
    pump(&mut cp, &mut rx);
    // the removal ran: the ID is free for the next
    run(&mut cp, &mut c, "voice m from \"hat\" on c");
    assert_eq!(vid(&cp, "m"), kick);
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn pending_object_e411() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(
        &mut cp,
        &mut c,
        "wait 2 by c then voice z from \"kick\" on c",
    );
    for line in ["start z", "z gain=0.5", "remove z"] {
        assert_eq!(code(&mut cp, line), Code::NotCreatedYet, "{line}");
    }
    assert_eq!(Code::NotCreatedYet.as_str(), "E411");
    // `all` doesn't reach it either
    run(&mut cp, &mut c, "start all");
    // a line that waits may name it
    run(&mut cp, &mut c, "start z after 3");
    render(&mut c, 1, 5100);
    assert!(pump(&mut cp, &mut rx).is_empty());
    let z = vid(&cp, "z");
    assert!(c.voices.get(z.index()).unwrap().state.active);
    // once it exists a line typed now can name it
    run(&mut cp, &mut c, "z gain=0.5");
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn line_due_before_its_object_exists_is_dropped() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(
        &mut cp,
        &mut c,
        "wait 3 by c then voice z from \"kick\" on c",
    );
    run(&mut cp, &mut c, "start z after 1");
    render(&mut c, 1, 1200);
    let z = vid(&cp, "z");
    assert!(!live_voice(&c, z));
    render(&mut c, 1, 2000);
    assert!(live_voice(&c, z));
    assert!(!c.voices.get(z.index()).unwrap().state.active);
    pump(&mut cp, &mut rx);
    let notes = cp.engine_state.take_notes();
    assert!(
        notes.iter().any(|t| t.contains("didn't exist yet")),
        "{notes:?}"
    );
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn at_line_still_e410() {
    let (mut cp, mut c) = chain_session();
    for line in [
        "voice z from \"kick\" on c at 4",
        "remove kick after 4",
        "group g { snare } at 4",
    ] {
        assert_eq!(code(&mut cp, line), Code::CannotWait, "{line}");
    }
    // the right side of a then after an at line may create
    run(
        &mut cp,
        &mut c,
        "start hat then voice z from \"kick\" on c at 4",
    );
    assert!(cp.engine_state.voice_id("z").is_ok());
}

#[test]
fn a_waiting_line_fires_before_the_removal_that_follows_it() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let kick = vid(&cp, "kick");
    run(&mut cp, &mut c, "kick gain=0.5 after 2");
    run(&mut cp, &mut c, "wait 3 by c then remove kick");
    render(&mut c, 1, 2100);
    assert_eq!(gain_at(&c, kick), 0.5);
    render(&mut c, 1, 1100);
    assert!(!live_voice(&c, kick));
    pump(&mut cp, &mut rx);
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn creating_in_a_chain_names_in_order() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    // each statement is its own entry: the voice exists when the next fires
    run(
        &mut cp,
        &mut c,
        "wait 1 by c then voice z from \"kick\" on c and z gain=0.25 then start z",
    );
    render(&mut c, 1, 1100);
    let z = vid(&cp, "z");
    assert!(live_voice(&c, z));
    assert_eq!(gain_at(&c, z), 0.25);
    assert!(c.voices.get(z.index()).unwrap().state.active);
    assert!(pump(&mut cp, &mut rx).is_empty());
    settled(&mut cp, &c, &mut rx);
}

fn state_of(cp: &mut CmdProcessor) -> String {
    match cp.parse("state") {
        Ok(Parsed::Local(text)) => text,
        _ => panic!("state"),
    }
}

#[test]
fn restore_brings_back_a_voice_with_its_seq_and_clock() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "voice w from \"kick\" on 90b");
    run(&mut cp, &mut c, "w { seq beat 1 3 }");
    let before = state_of(&mut cp);
    let (procs, tempos) = (cp.engine_state.procs.len(), cp.engine_state.tempos.len());
    run(&mut cp, &mut c, "hat gain=0 over 2b thenif remove w");
    // the voice with its seq and clock stays in the mirror, nameable now,
    // but a line that waits sees the name free
    let now = state_of(&mut cp);
    assert!(now.contains("voice w from") && now.contains("w { seq beat 1 3 }"));
    assert!(cp.engine_state.voice_id("w").is_err());
    assert_eq!(cp.engine_state.procs.len(), procs);
    assert_eq!(cp.engine_state.tempos.len(), tempos);
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "hat gain=1");
    render(&mut c, 1, 3000);
    pump(&mut cp, &mut rx);
    assert_eq!(state_of(&mut cp), before);
    assert_eq!(cp.engine_state.procs.len(), procs);
    assert_eq!(cp.engine_state.tempos.len(), tempos);
    // nothing is held any more, and the engine still has all of it
    let w = vid(&cp, "w");
    assert!(live_voice(&c, w));
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn removal_that_runs_takes_the_voice_with_its_seq_and_clock() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "voice w from \"kick\" on 90b");
    run(&mut cp, &mut c, "w { seq beat 1 3 }");
    let (voices, procs, tempos) = (
        cp.engine_state.ids.voices.live(),
        cp.engine_state.ids.procs.live(),
        cp.engine_state.ids.tempos.live(),
    );
    run(&mut cp, &mut c, "wait 2 by c then remove w");
    // held: still counted live, so nothing can take the IDs
    assert_eq!(cp.engine_state.ids.voices.live(), voices);
    render(&mut c, 1, 2100);
    pump(&mut cp, &mut rx);
    assert_eq!(cp.engine_state.ids.voices.live(), voices - 1);
    assert_eq!(cp.engine_state.ids.procs.live(), procs - 1);
    assert_eq!(cp.engine_state.ids.tempos.live(), tempos - 1);
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn staged_effect_is_pending_then_there() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "wait 1 by c then kick { filter lo }");
    let kick = vid(&cp, "kick");
    assert_eq!(cp.engine_state.fx_list(FxOwner::Voice(kick)).len(), 1);
    assert_eq!(code(&mut cp, "kick.lo cutoff=2000"), Code::NotCreatedYet);
    assert!(!c.voices.get(kick.index()).unwrap().fx.has(0));
    // a line that waits may name it
    run(&mut cp, &mut c, "kick.lo cutoff=2000 after 2 by c");
    render(&mut c, 1, 1100);
    assert!(c.voices.get(kick.index()).unwrap().fx.has(0));
    render(&mut c, 1, 1100);
    assert!(pump(&mut cp, &mut rx).is_empty());
    run(&mut cp, &mut c, "kick.lo cutoff=3000");
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn dropped_effect_line_forgets_the_effect() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(
        &mut cp,
        &mut c,
        "hat gain=0 over 2b thenif kick { filter lo }",
    );
    let kick = vid(&cp, "kick");
    assert_eq!(cp.engine_state.fx_list(FxOwner::Voice(kick)).len(), 1);
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "hat gain=1");
    render(&mut c, 1, 3000);
    pump(&mut cp, &mut rx);
    assert!(cp.engine_state.fx_list(FxOwner::Voice(kick)).is_empty());
    assert!(!c.voices.get(kick.index()).unwrap().fx.has(0));
    settled(&mut cp, &c, &mut rx);
    // the first free id again
    run(&mut cp, &mut c, "kick { filter hi }");
    assert_eq!(cp.engine_state.fx_list(FxOwner::Voice(kick))[0].id, 0);
}

#[test]
fn staged_clock_made_and_removed() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "wait 1 by c then clock q 90b");
    let q = cp.engine_state.context_id("q").unwrap();
    assert_eq!(code(&mut cp, "start q"), Code::NotCreatedYet);
    assert!(c.tempos.get(q.index()).is_none());
    render(&mut c, 1, 1100);
    assert!(c.tempos.get(q.index()).is_some());
    pump(&mut cp, &mut rx);
    run(&mut cp, &mut c, "wait 1 by c then remove q");
    // a line typed now still names it until the removal runs
    assert!(cp.engine_state.context_id("q").is_ok());
    assert!(c.tempos.get(q.index()).is_some());
    render(&mut c, 1, 1100);
    assert!(c.tempos.get(q.index()).is_none());
    pump(&mut cp, &mut rx);
    settled(&mut cp, &c, &mut rx);
    run(&mut cp, &mut c, "clock r 100b");
    assert_eq!(cp.engine_state.context_id("r").unwrap(), q);
}

#[test]
fn a_name_made_while_a_removal_waits_is_kept_on_restore_by_renaming() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let kick = vid(&cp, "kick");
    run(&mut cp, &mut c, "hat gain=0 over 2b thenif remove kick");
    // the name is free for a line that waits, and taken for one typed now
    assert_eq!(
        code(&mut cp, "voice kick from \"hat\" on c"),
        Code::KindClash
    );
    run(
        &mut cp,
        &mut c,
        "wait 1 by c then voice kick from \"hat\" on c",
    );
    let fresh = vid(&cp, "kick");
    assert_ne!(fresh, kick);
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "hat gain=1");
    render(&mut c, 1, 3000);
    pump(&mut cp, &mut rx);
    // the removed one is back, under a name that is free
    assert_eq!(vid(&cp, "kick"), fresh);
    assert_eq!(vid(&cp, "kick_"), kick);
    settled(&mut cp, &c, &mut rx);
}

// the mirror and the engine hold the same voices, clocks and processes
fn assert_agree(cp: &CmdProcessor, c: &Conductor, what: &str) {
    let es = &cp.engine_state;
    for i in 0..es.limits.voices as usize {
        let id = VoiceId(i as u32);
        assert_eq!(
            es.voices.contains_key(&id),
            c.voices.get(i).is_some(),
            "{what}: voice {i}"
        );
    }
    for i in 0..es.limits.tempos as usize {
        let id = TempoId(i as u32);
        assert_eq!(
            es.tempos.contains_key(&id),
            c.tempos.get(i).is_some(),
            "{what}: tempo {i}"
        );
    }
    for i in 0..es.limits.procs as usize {
        let id = ProcId(i as u32);
        assert_eq!(
            es.procs.contains_key(&id),
            c.procs.get(i).is_some(),
            "{what}: proc {i}"
        );
    }
    for i in 0..es.limits.groups as usize {
        let id = GroupId(i as u32);
        assert_eq!(
            es.groups.contains_key(&id),
            c.groups.get(i).is_some(),
            "{what}: group {i}"
        );
    }
    // every waiting removal has run or been dropped
    assert!(es.limbo.is_empty(), "{what}: {:?}", es.limbo);
}

#[test]
fn mirror_and_engine_agree_after_staged_lines_in_every_outcome() {
    for (i, lines) in [
        // ran
        vec![
            "wait 1 by c then voice z from \"kick\" on 90b",
            "wait 2 by c then z { seq s 1 3 } and start z",
        ],
        vec!["wait 1 by c then voice z from \"kick\" on c and remove snare"],
        // dropped by a retarget
        vec![
            "hat gain=0 over 2b thenif voice z from \"kick\" on 90b and z { seq s 1 }",
            "hat gain=1",
        ],
        vec!["hat gain=0 over 2b thenif remove tom", "hat gain=1"],
        // a prompt removal strands the line
        vec![
            "wait 2 by c then voice z from \"kick\" on c and start hat",
            "remove hat",
        ],
        vec![
            "wait 2 by c then tom gain=1 and remove tom",
            "snare gain=0.5",
        ],
        // created and removed by lines that wait
        vec![
            "wait 1 by c then voice z from \"kick\" on 90b and z { seq s 1 }",
            "wait 3 by c then remove z",
        ],
        // the removed one is named by an earlier waiting line
        vec!["tom gain=0.1 after 1 by c", "wait 2 by c then remove tom"],
        // groups: made, removed, dropped
        vec!["wait 1 by c then group g on c { snare and tom }"],
        vec![
            "group g on c { snare and tom }",
            "start g",
            "g gain=0 over 2b thenif remove g",
        ],
        vec![
            "group g on c { snare and tom }",
            "start g",
            "g gain=0 over 2b thenif remove g",
            "g gain=1",
        ],
        vec![
            "hat gain=0 over 2b thenif group g on c { snare and tom }",
            "hat gain=1",
        ],
        vec![
            "wait 1 by c then group g on c { snare and tom }",
            "wait 3 by c then remove g",
        ],
        vec![
            "group g on c { snare and tom }",
            "wait 2 by c then ungroup snare and remove tom",
        ],
        // seqs, clocks and lfos
        vec![
            "kick { seq beat 1 3 }",
            "hat gain=0 over 2b thenif remove kick.beat",
        ],
        vec![
            "kick { seq beat 1 3 }",
            "hat gain=0 over 2b thenif remove kick.beat",
            "hat gain=1",
        ],
        vec![
            "kick { seq beat 1 3 }",
            "hat gain=0 over 2b thenif remove kick.beat",
            "wait 3 by c then kick { seq beat 1 4 }",
        ],
        vec!["clock q 90b", "wait 1 by c then remove q"],
        vec![
            "clock q 90b",
            "hat gain=0 over 2b thenif remove q",
            "hat gain=1",
        ],
        vec![
            "clock q 90b",
            "hat gain=0 over 2b thenif remove q",
            "remove q",
        ],
        vec![
            "kick { lfo wob to=pan depth=0.3 freq=2hz }",
            "hat gain=0 over 2b thenif remove kick.wob",
        ],
        vec![
            "kick { lfo wob to=pan depth=0.3 freq=2hz }",
            "hat gain=0 over 2b thenif remove kick.wob",
            "hat gain=1",
        ],
        // the name views
        vec!["kick gain=0 over 2b thenif remove kick", "kick gain=1"],
        vec!["kick gain=0 over 2b thenif remove kick", "remove kick"],
        vec![
            "remove kick",
            "wait 2 by c then voice kick from \"hat\" on c",
        ],
        vec![
            "hat gain=0 over 2b thenif remove kick",
            "wait 1 by c then voice kick from \"hat\" on c",
            "hat gain=1",
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let (mut cp, mut c) = chain_session();
        let mut rx = rx(&mut c);
        for line in &lines {
            run(&mut cp, &mut c, line);
            render(&mut c, 1, 200);
            pump(&mut cp, &mut rx);
        }
        render(&mut c, 1, 10_000);
        pump(&mut cp, &mut rx);
        let what = format!("case {i}: {lines:?}");
        assert_agree(&cp, &c, &what);
        assert_eq!(cp.engine_state.scheduled_pending(), 0, "{what}");
        assert_eq!(c.schedule.waiting(), 0, "{what}");
        assert!(cp.engine_state.waiting.is_empty(), "{what}");
        // nothing is left held or pending: every live ID is a live object
        let es = &cp.engine_state;
        assert_eq!(es.ids.voices.live() as usize, es.voices.len(), "{what}");
        assert_eq!(es.ids.tempos.live() as usize, es.tempos.len(), "{what}");
        assert_eq!(es.ids.procs.live() as usize, es.procs.len(), "{what}");
    }
}

#[test]
fn thenif_remove_group_after_fade() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "group drums on c { snare and tom }");
    run(&mut cp, &mut c, "start drums");
    let (drums, snare, tom) = (
        gid(&cp, "drums"),
        vid(&cp, "drums.snare"),
        vid(&cp, "drums.tom"),
    );
    run(&mut cp, &mut c, "drums gain=0 over 2b thenif remove drums");
    // a line that waits sees it gone; the mirror and the engine still have it
    assert!(cp.engine_state.group_id("drums").is_err());
    assert!(cp.engine_state.groups.contains_key(&drums));
    assert!(c.groups.get(drums.index()).is_some());
    render(&mut c, 1, 1900);
    assert!(live_voice(&c, snare) && live_voice(&c, tom));
    render(&mut c, 1, 200);
    assert!(c.groups.get(drums.index()).is_none());
    assert!(!live_voice(&c, snare) && !live_voice(&c, tom));
    assert!(pump(&mut cp, &mut rx).is_empty());
    settled(&mut cp, &c, &mut rx);
    assert!(cp.engine_state.groups.is_empty());
    assert!(!cp.engine_state.voices.contains_key(&snare));
    assert!(!cp.engine_state.voices.contains_key(&tom));
    assert_agree(&cp, &c, "group removed");
    assert_eq!(cp.engine_state.ids.groups.live(), 0);
}

#[test]
fn dropped_group_removal_keeps_members_grouped() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "group drums on c { snare and tom }");
    run(&mut cp, &mut c, "start drums");
    let (drums, snare) = (gid(&cp, "drums"), vid(&cp, "drums.snare"));
    run(&mut cp, &mut c, "drums gain=0 over 2b thenif remove drums");
    render(&mut c, 1, 500);
    // the name works now, and cuts the removal
    run(&mut cp, &mut c, "drums gain=1");
    render(&mut c, 1, 3000);
    let mut printed = pump(&mut cp, &mut rx);
    printed.extend(cp.engine_state.take_notes());
    assert!(printed.iter().any(|t| t.starts_with("W105")), "{printed:?}");
    assert_eq!(gid(&cp, "drums"), drums);
    let es = &cp.engine_state;
    assert_eq!(es.voices[&snare].group, Some(drums));
    assert!(es.groups[&drums].member_names.contains_key("snare"));
    assert_eq!(vid(&cp, "drums.snare"), snare);
    assert!(c.groups.get(drums.index()).is_some());
    assert!(live_voice(&c, snare));
    settled(&mut cp, &c, &mut rx);
    assert_agree(&cp, &c, "group removal dropped");
}

#[test]
fn staged_group_creation_dropped_puts_members_back() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let before = state_of(&mut cp);
    run(
        &mut cp,
        &mut c,
        "hat gain=0 over 2b thenif group g on c { snare and tom }",
    );
    // the mirror has the group already; its members are in it
    let g = gid(&cp, "g");
    assert_eq!(cp.engine_state.voices[&vid(&cp, "g.tom")].group, Some(g));
    assert!(c.groups.get(g.index()).is_none());
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "hat gain=1");
    render(&mut c, 1, 3000);
    let printed = pump(&mut cp, &mut rx);
    assert!(printed.iter().any(|t| t.starts_with("W105")), "{printed:?}");
    assert!(cp.engine_state.group_id("g").is_err());
    assert!(cp.engine_state.groups.is_empty());
    assert!(cp.engine_state.voices[&vid(&cp, "tom")].group.is_none());
    let after = state_of(&mut cp);
    assert_eq!(after.replace("gain=0", ""), before.replace("gain=0", ""));
    settled(&mut cp, &c, &mut rx);
    assert_agree(&cp, &c, "group creation dropped");
}

#[test]
fn retarget_by_name_cuts_the_removal() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let kick = vid(&cp, "kick");
    run(&mut cp, &mut c, "kick gain=0 over 2b thenif remove kick");
    // a line that waits sees it gone; a line typed now names it
    assert!(cp.engine_state.voice_id("kick").is_err());
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "kick gain=1");
    render(&mut c, 1, 3000);
    let mut printed = pump(&mut cp, &mut rx);
    printed.extend(cp.engine_state.take_notes());
    assert!(printed.iter().any(|t| t.starts_with("W105")), "{printed:?}");
    assert!(live_voice(&c, kick));
    assert_eq!(vid(&cp, "kick"), kick);
    assert_eq!(gain_of(&cp, &c, "kick"), 1.0);
    settled(&mut cp, &c, &mut rx);
    assert_agree(&cp, &c, "retargeted by name");
}

#[test]
fn prompt_remove_while_removal_waits() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let kick = vid(&cp, "kick");
    run(&mut cp, &mut c, "kick gain=0 over 2b thenif remove kick");
    let text = say(&mut cp, &mut c, "remove kick");
    assert_eq!(text.matches("W103").count(), 1, "{text}");
    assert!(text.contains("names kick"), "{text}");
    // gone now
    assert!(cp.engine_state.voice_id("kick").is_err());
    assert!(!cp.engine_state.voices.contains_key(&kick));
    render(&mut c, 1, 10);
    assert!(!live_voice(&c, kick));
    render(&mut c, 1, 3000);
    pump(&mut cp, &mut rx);
    settled(&mut cp, &c, &mut rx);
    assert_agree(&cp, &c, "prompt removal");
}

#[test]
fn prompt_create_same_name_e302() {
    let (mut cp, mut c) = chain_session();
    run(&mut cp, &mut c, "kick gain=0 over 2b thenif remove kick");
    assert_eq!(
        code(&mut cp, "voice kick from \"hat\" on c"),
        Code::KindClash
    );
    // state shows it: it still sounds
    assert!(state_of(&mut cp).contains("voice kick from"));
}

#[test]
fn script_recreates_name_after_wait() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    let first = vid(&cp, "kick");
    run(&mut cp, &mut c, "wait 1 by c then remove kick");
    // staged: the old kick is still there; a later line may use the name
    run(
        &mut cp,
        &mut c,
        "wait 2 by c then voice kick from \"kick\" on c",
    );
    let count = |cp: &mut CmdProcessor| {
        state_of(cp)
            .lines()
            .filter(|l| l.starts_with("voice kick "))
            .count()
    };
    assert_eq!(count(&mut cp), 1);
    // the old kick goes at its due frame
    render(&mut c, 1, 900);
    assert!(live_voice(&c, first));
    render(&mut c, 1, 200);
    pump(&mut cp, &mut rx);
    assert!(!live_voice(&c, first));
    assert_eq!(count(&mut cp), 0);
    // the new one appears at its own
    render(&mut c, 1, 1000);
    pump(&mut cp, &mut rx);
    assert_eq!(count(&mut cp), 1);
    let new = vid(&cp, "kick");
    assert!(live_voice(&c, new));
    settled(&mut cp, &c, &mut rx);
    assert_agree(&cp, &c, "recreated name");
}

#[test]
fn dropped_member_edits_put_voices_back() {
    for edit in [
        "hat gain=0 over 2b thenif ungroup snare",
        "hat gain=0 over 2b thenif ungroup g",
        "hat gain=0 over 2b thenif g { kick }",
    ] {
        let (mut cp, mut c) = chain_session();
        let mut rx = rx(&mut c);
        run(&mut cp, &mut c, "group g on c { snare and tom }");
        let before = state_of(&mut cp);
        run(&mut cp, &mut c, edit);
        assert_ne!(state_of(&mut cp), before, "{edit}");
        render(&mut c, 1, 500);
        run(&mut cp, &mut c, "hat gain=1");
        render(&mut c, 1, 3000);
        let printed = pump(&mut cp, &mut rx);
        assert!(printed.iter().any(|t| t.starts_with("W105")), "{edit}");
        let g = gid(&cp, "g");
        let es = &cp.engine_state;
        assert_eq!(es.voices[&vid(&cp, "g.snare")].group, Some(g), "{edit}");
        assert_eq!(es.voices[&vid(&cp, "g.tom")].group, Some(g), "{edit}");
        assert!(es.voices[&vid(&cp, "kick")].group.is_none(), "{edit}");
        assert_eq!(state_of(&mut cp), before, "{edit}");
        settled(&mut cp, &c, &mut rx);
        assert_agree(&cp, &c, edit);
    }
}

#[test]
fn group_whose_removal_waits_and_its_members_stay_nameable() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "group drums on c { snare and tom }");
    run(&mut cp, &mut c, "start drums");
    run(&mut cp, &mut c, "drums gain=0 over 2b thenif remove drums");
    // a line typed now names the group and its members
    run(&mut cp, &mut c, "drums.snare gain=0.5");
    run(&mut cp, &mut c, "snare pan=0.1");
    // the name is free for a line that waits, and the members are gone
    run(
        &mut cp,
        &mut c,
        "wait 1 by c then group drums on c { kick }",
    );
    let text = say(&mut cp, &mut c, "remove drums");
    assert!(text.contains("W103"), "{text}");
    render(&mut c, 1, 3000);
    pump(&mut cp, &mut rx);
    settled(&mut cp, &c, &mut rx);
    assert_agree(&cp, &c, "group removed by hand");
}

#[test]
fn removing_an_effect_still_cannot_wait() {
    let (mut cp, mut c) = chain_session();
    run(&mut cp, &mut c, "kick { filter lo }");
    assert_eq!(
        code(&mut cp, "hat gain=0 over 2b then remove kick.lo"),
        Code::CannotWait
    );
}

#[test]
fn seq_whose_removal_waits_stays_nameable() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "kick { seq beat 1 3 }");
    let beat = cp.engine_state.voices[&vid(&cp, "kick")].procs["beat"];
    run(
        &mut cp,
        &mut c,
        "hat gain=0 over 2b thenif remove kick.beat",
    );
    // a line typed now still names it; a line that waits does not
    let state = state_of(&mut cp);
    assert!(state.contains("seq beat 1 3"), "{state}");
    run(&mut cp, &mut c, "kick.beat 1 4");
    assert_eq!(code(&mut cp, "kick { seq beat 1 2 }"), Code::KindClash);
    assert_eq!(
        code(&mut cp, "wait 1 by c then kick.beat 1 2"),
        Code::UnknownName
    );
    // a later part may make a new one of the same name
    run(&mut cp, &mut c, "wait 3 by c then kick { seq beat 1 4 }");
    // and a seq a waiting line will make is not in `state` yet
    run(&mut cp, &mut c, "wait 4 by c then kick { seq later 1 2 }");
    assert!(!state_of(&mut cp).contains("seq later"));
    run(&mut cp, &mut c, "wait 4 by c then clock zz 70b");
    assert!(!state_of(&mut cp).contains("clock zz"));
    // cutting the glide drops the removal
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "hat gain=1");
    render(&mut c, 1, 4000);
    let mut printed = pump(&mut cp, &mut rx);
    printed.extend(cp.engine_state.take_notes());
    assert!(printed.iter().any(|t| t.starts_with("W105")), "{printed:?}");
    assert!(c.procs.get(beat.index()).is_some());
    settled(&mut cp, &c, &mut rx);
    assert_agree(&cp, &c, "seq removal dropped");
}

#[test]
fn seq_removed_by_hand_while_its_removal_waits() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "kick { seq beat 1 3 }");
    let beat = cp.engine_state.voices[&vid(&cp, "kick")].procs["beat"];
    run(
        &mut cp,
        &mut c,
        "hat gain=0 over 2b thenif remove kick.beat",
    );
    let text = say(&mut cp, &mut c, "remove kick.beat");
    assert!(text.contains("W103"), "{text}");
    assert!(!cp.engine_state.procs.contains_key(&beat));
    render(&mut c, 1, 3000);
    pump(&mut cp, &mut rx);
    settled(&mut cp, &c, &mut rx);
    assert_agree(&cp, &c, "seq removed by hand");
}

#[test]
fn clock_whose_removal_waits_stays_nameable() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "clock q 90b");
    let q = cp.engine_state.context_id("q").unwrap();
    run(&mut cp, &mut c, "hat gain=0 over 2b thenif remove q");
    // typed now: it is there
    assert!(state_of(&mut cp).contains("clock q"));
    run(&mut cp, &mut c, "start q");
    assert_eq!(code(&mut cp, "clock q 100b"), Code::KindClash);
    // a line that waits sees the name free
    assert_eq!(code(&mut cp, "wait 1 by c then start q"), Code::UnknownName);
    run(&mut cp, &mut c, "wait 1 by c then clock q 80b");
    // cut: q stays, the new q takes the name over and the old one is q_
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "hat gain=1");
    render(&mut c, 1, 3000);
    let printed = pump(&mut cp, &mut rx);
    assert!(printed.iter().any(|t| t.starts_with("W105")), "{printed:?}");
    assert_eq!(cp.engine_state.context_id("q_").unwrap(), q);
    settled(&mut cp, &c, &mut rx);
    assert_agree(&cp, &c, "clock removal dropped");
}

#[test]
fn lfo_whose_removal_waits_stays_nameable() {
    let (mut cp, mut c) = chain_session();
    let mut rx = rx(&mut c);
    run(
        &mut cp,
        &mut c,
        "kick { lfo wob to=pan depth=0.3 freq=2hz }",
    );
    run(&mut cp, &mut c, "hat gain=0 over 2b thenif remove kick.wob");
    run(&mut cp, &mut c, "kick.wob depth=0.5");
    render(&mut c, 1, 3000);
    pump(&mut cp, &mut rx);
    settled(&mut cp, &c, &mut rx);
    assert_agree(&cp, &c, "lfo removed");
}

// ---- waves in waiting lines (WAVa) ----

// the base `freq` of wave `id` of voice `name`, as the engine has it
fn wave_freq(cp: &CmdProcessor, c: &Conductor, name: &str, id: u8) -> Option<f32> {
    c.voices.get(vid(cp, name).index())?.waves.base(id, 0)
}

fn wave_ids(cp: &CmdProcessor, name: &str) -> Vec<u8> {
    cp.engine_state
        .wave_list(vid(cp, name))
        .iter()
        .map(|w| w.id)
        .collect()
}

#[test]
fn removed_wave_drops_waiting_line_and_a_reused_slot_is_untouched() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "voice organ on c { wave s0; wave s1 }");
    run(&mut cp, &mut c, "start organ");
    run(&mut cp, &mut c, "organ.s1 freq=330 at 4");
    assert_eq!(c.schedule.waiting(), 1);
    let text = say(&mut cp, &mut c, "remove organ.s1");
    assert!(text.contains("W103") && text.contains("names s1"), "{text}");
    assert_eq!(c.schedule.waiting(), 0);
    assert!(cp.engine_state.waiting.is_empty());
    // a new wave takes the slot; the dropped line must not reach it
    run(&mut cp, &mut c, "organ { wave s2 }");
    assert_eq!(wave_ids(&cp, "organ"), [0, 1]);
    assert_eq!(wave_freq(&cp, &c, "organ", 1), Some(220.0));
    render(&mut c, 1, 6000);
    assert_eq!(wave_freq(&cp, &c, "organ", 1), Some(220.0));
    // mirror and engine agree on which slots are taken
    let v = &c.voices.get(vid(&cp, "organ").index()).unwrap().waves;
    assert!(v.has(0) && v.has(1) && !v.has(2));
}

#[test]
fn a_waiting_wave_set_runs_when_the_wave_stays() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "voice organ on c { wave s0 }");
    run(&mut cp, &mut c, "start organ");
    run(&mut cp, &mut c, "organ.s0 freq=330 at 2");
    render(&mut c, 1, 2500);
    assert_eq!(wave_freq(&cp, &c, "organ", 0), Some(330.0));
}

#[test]
fn a_wave_made_in_a_staged_part_exists_when_its_line_runs() {
    let (mut cp, mut c) = on_c();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "voice organ on c { wave s0 }");
    run(&mut cp, &mut c, "start organ");
    run(
        &mut cp,
        &mut c,
        "organ.s0 gain=0.5 over 1b then organ { wave late freq=500 }",
    );
    // the mirror has it now (the line is staged), the engine when it runs
    assert_eq!(wave_ids(&cp, "organ"), [0, 1]);
    let v = vid(&cp, "organ");
    assert!(!c.voices.get(v.index()).unwrap().waves.has(1));
    render(&mut c, 1, 1500);
    assert_eq!(wave_freq(&cp, &c, "organ", 1), Some(500.0));
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn a_staged_wave_goes_when_its_line_is_dropped() {
    let (mut cp, mut c) = on_c();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "voice organ on c { wave s0 }");
    run(&mut cp, &mut c, "start organ");
    run(
        &mut cp,
        &mut c,
        "organ.s0 gain=0.5 over 4b thenif organ { wave late }",
    );
    assert_eq!(wave_ids(&cp, "organ"), [0, 1]);
    // the voice stays: a set cuts the glide short, so the thenif is dropped
    run(&mut cp, &mut c, "organ.s0 gain=1 over 1b");
    render(&mut c, 1, 6000);
    let printed = pump(&mut cp, &mut rx);
    assert_eq!(printed.len(), 1, "{printed:?}");
    assert!(printed[0].starts_with("W105"), "{printed:?}");
    // the mirror forgot the staged wave, and the engine never had it
    assert_eq!(wave_ids(&cp, "organ"), [0]);
    let v = vid(&cp, "organ");
    assert!(!c.voices.get(v.index()).unwrap().waves.has(1));
    settled(&mut cp, &c, &mut rx);
    // and the slot is free again
    run(&mut cp, &mut c, "organ { wave again }");
    assert_eq!(wave_ids(&cp, "organ"), [0, 1]);
    assert!(c.voices.get(v.index()).unwrap().waves.has(1));
}

#[test]
fn a_staged_wave_goes_with_a_removed_voice() {
    let (mut cp, mut c) = on_c();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "voice organ on c { wave s0 }");
    run(&mut cp, &mut c, "start organ");
    run(
        &mut cp,
        &mut c,
        "organ.s0 gain=0.5 over 4b then organ { wave late }",
    );
    assert_eq!(wave_ids(&cp, "organ"), [0, 1]);
    // removing the voice drops the waiting line
    run(&mut cp, &mut c, "remove organ");
    pump(&mut cp, &mut rx);
    assert_eq!(c.schedule.waiting(), 0);
}

#[test]
fn a_pending_wave_does_not_keep_the_last_real_one() {
    // a waves-only voice must keep a wave: the one a waiting line will make
    // doesn't count, since that line may be dropped
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "voice organ on c { wave s0 }");
    run(&mut cp, &mut c, "start organ");
    run(
        &mut cp,
        &mut c,
        "organ.s0 gain=0.5 over 4b thenif organ { wave late }",
    );
    assert_eq!(code(&mut cp, "remove organ.s0"), Code::InUse);
    assert_eq!(wave_ids(&cp, "organ"), [0, 1]);
}

#[test]
fn thenif_is_cut_by_a_set_on_a_gliding_wave_w105() {
    let (mut cp, mut c) = on_c();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "voice organ on c { wave s0; wave s1 }");
    run(&mut cp, &mut c, "start organ");
    run(
        &mut cp,
        &mut c,
        "organ.s0 freq=300 over 2b thenif organ.s1 gain=0.5",
    );
    render(&mut c, 1, 500);
    run(&mut cp, &mut c, "organ.s0 freq=100 over 4b");
    render(&mut c, 1, 5000);
    let printed = pump(&mut cp, &mut rx);
    assert_eq!(
        printed,
        ["W105 thenif: the glide on organ.s0.freq was cut short; the rest of the line was dropped"]
    );
    let gain = c
        .voices
        .get(vid(&cp, "organ").index())
        .unwrap()
        .waves
        .base(1, 1);
    assert_eq!(gain, Some(1.0));
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn thenif_runs_when_a_wave_glide_finishes() {
    let (mut cp, mut c) = on_c();
    let mut rx = rx(&mut c);
    run(&mut cp, &mut c, "voice organ on c { wave s0; wave s1 }");
    run(&mut cp, &mut c, "start organ");
    run(
        &mut cp,
        &mut c,
        "organ.s0 freq=300 over 1b thenif organ.s1 gain=0.5",
    );
    render(&mut c, 1, 1500);
    let gain = c
        .voices
        .get(vid(&cp, "organ").index())
        .unwrap()
        .waves
        .base(1, 1);
    assert_eq!(gain, Some(0.5));
    settled(&mut cp, &c, &mut rx);
}

#[test]
fn a_pending_wave_can_be_removed_without_touching_the_real_count() {
    let (mut cp, mut c) = on_c();
    run(&mut cp, &mut c, "voice organ on c { wave s0 }");
    run(&mut cp, &mut c, "start organ");
    run(&mut cp, &mut c, "wait 1 by c then organ { wave late }");
    assert_eq!(wave_ids(&cp, "organ"), [0, 1]);
    // the real wave still can't go; the pending one is not what keeps it
    assert_eq!(code(&mut cp, "remove organ.s0"), Code::InUse);
    // removing the pending one gets past the last-wave check (a removal
    // can't wait, so a waiting line is refused for that, not for the count)
    assert_eq!(
        code(&mut cp, "wait 2 by c then remove organ.late"),
        Code::CannotWait
    );
    assert_eq!(wave_ids(&cp, "organ"), [0, 1]);
}
