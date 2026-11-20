//! PKb1's tests: timed scripts, against a real `Conductor` driven the way
//! the runtime drives it (commands in, blocks out, events to the mirror,
//! `pump`).

use super::{Code, LangErr};
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::*;
use crate::audio_processing::engine::arena::Limits;
use crate::audio_processing::engine::{Conductor, EngineRx, Event};
use crate::audio_processing::test_util::{pcm, render_i16, setup_with};
use crate::file_parsing::decode_helpers::AudioFile;

const BLOCK: usize = 64;

// a track of silence with one sample `val` first, so a frame is not zero
// only when the voice is (re)triggered
fn spike(name: &str, val: i16) -> AudioFile {
    let mut samples = vec![0i16; 100_000];
    samples[0] = val;
    AudioFile::new(name, "wav", 48000, 1, 16, pcm(&samples))
}

// a track that stays loud: whether a voice is playing shows in the output
fn tone(name: &str) -> AudioFile {
    AudioFile::new(name, "wav", 48000, 1, 16, pcm(&vec![1000i16; 100_000]))
}

struct Rig {
    cp: CmdProcessor,
    c: Conductor,
    tx: CmdSender,
    rx: CmdReceiver,
    erx: EngineRx,
    /// What `pump` printed.
    log: Vec<String>,
    /// Frames rendered so far.
    frames: usize,
}

// `kick` (-1) and `hat` (-2) on a clock `c` of 1000 frames a tick, which
// runs; `a`, `b` and `cc` are loud tones on `c`
fn rig_with(limits: Limits) -> Rig {
    sample_rate::set(48000);
    let tracks = vec![
        spike("kick", -1),
        spike("hat", -2),
        tone("ta"),
        tone("tb"),
        tone("tc"),
    ];
    let (mut cp, mut c) = setup_with(limits, 1, tracks);
    c.apply(cp.boot());
    let erx = c.take_rx().unwrap();
    let (tx, rx) = cmd_queue(256);
    let mut rig = Rig {
        cp,
        c,
        tx,
        rx,
        erx,
        log: Vec::new(),
        frames: 0,
    };
    for line in [
        "clock c 1000f",
        "start c",
        "voice kick from \"kick\" on c",
        "voice hat from \"hat\" on c",
        "voice a from \"ta\" on c",
        "voice b from \"tb\" on c",
        "voice cc from \"tc\" on c",
    ] {
        rig.line(line);
    }
    rig
}

fn rig() -> Rig {
    rig_with(Limits::default())
}

impl Rig {
    fn line(&mut self, line: &str) {
        self.try_line(line)
            .unwrap_or_else(|e| panic!("'{line}' failed: {e}"));
    }

    fn try_line(&mut self, line: &str) -> Result<Option<String>, CmdErr> {
        let text = self.cp.submit(line, &mut self.tx)?;
        self.apply_queue();
        Ok(text)
    }

    fn apply_queue(&mut self) {
        while let Some(cmd) = self.rx.try_pop() {
            self.c.apply(cmd);
        }
    }

    /// Runs a script text as `--script` would.
    fn script(&mut self, text: &str) -> Result<(), Vec<LangErr>> {
        let done = self.cp.run_text("t", text, &mut self.tx);
        self.apply_queue();
        match done {
            Ok(_) => Ok(()),
            Err(CmdErr::Lang(errs)) => Err(errs),
            Err(e) => panic!("{e}"),
        }
    }

    /// Renders `frames`, a block at a time: commands in, the block, events
    /// to the mirror, `pump`. Returns the output.
    fn run(&mut self, frames: usize) -> Vec<i16> {
        let mut out = Vec::new();
        let mut left = frames;
        while left > 0 {
            let n = left.min(BLOCK);
            self.apply_queue();
            out.extend(render_i16(&mut self.c, 1, n));
            self.frames += n;
            left -= n;
            for event in self.erx.events() {
                if let Event::ScheduleFired {
                    seq_no,
                    total,
                    applied,
                } = event
                {
                    self.cp
                        .engine_state
                        .note_schedule_fired(seq_no, total, applied);
                }
            }
            let texts = self.cp.pump(&mut self.tx);
            self.log.extend(texts);
        }
        out
    }

    fn running(&self, name: &str) -> bool {
        let id = self.cp.engine_state.voice_id(name).unwrap();
        self.c.voices.get(id.index()).unwrap().state.active
    }

    fn state(&mut self) -> String {
        self.cp
            .submit("state", &mut self.tx)
            .unwrap()
            .unwrap_or_default()
    }
}

// frames where the output is `val`
fn at(out: &[i16], val: i16) -> Vec<usize> {
    out.iter()
        .enumerate()
        .filter(|(_, s)| **s == val)
        .map(|(f, _)| f)
        .collect()
}

fn near(got: &[usize], want: &[usize]) {
    assert_eq!(got.len(), want.len(), "got {got:?}, want {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!(g.abs_diff(*w) <= 1, "got {got:?}, want {want:?}");
    }
}

fn codes(errs: &[LangErr]) -> Vec<Code> {
    errs.iter().map(|e| e.code).collect()
}

fn lang(e: CmdErr) -> Vec<LangErr> {
    match e {
        CmdErr::Lang(errs) => errs,
        other => panic!("not a language error: {other}"),
    }
}

#[test]
fn waits_add_up() {
    let mut r = rig();
    r.script("wait 4 by c\nstart kick\nwait 4 by c\nstart hat\n")
        .unwrap();
    let out = r.run(10_000);
    // x at tick 4, y at tick 8: relative to when the script was applied,
    // which is a whole number of ticks past a tick edge
    let kick = at(&out, -1);
    let hat = at(&out, -2);
    assert_eq!(kick.len(), 1, "{kick:?}");
    assert_eq!(hat.len(), 1, "{hat:?}");
    assert_eq!(hat[0] - kick[0], 4000);
    assert!(kick[0].abs_diff(4000) <= 1, "{kick:?}");
}

#[test]
fn wait_by_clock() {
    let mut r = rig();
    // main runs at 120 bpm: a beat is 24000 frames at 48 kHz
    r.script("wait 1\nstart kick\nwait 2 by c\nstart hat\n")
        .unwrap();
    let out = r.run(30_000);
    near(&at(&out, -1), &[24_000]);
    near(&at(&out, -2), &[26_000]);
}

#[test]
fn wait_seconds() {
    let mut r = rig();
    // 100 ms is 4800 frames: 4.8 ticks of c
    r.script("wait 100ms by c\nstart kick\n").unwrap();
    let out = r.run(6000);
    near(&at(&out, -1), &[4800]);
}

#[test]
fn segment_counts_from_real_fire() {
    let mut r = rig();
    r.script("wait 4 by c\nstart kick\nwait 4 by c\nstart hat\n")
        .unwrap();
    let mut out = r.run(1500);
    // the clock stops counting: segment 1 is held
    r.line("pause c");
    out.extend(r.run(5000));
    assert!(at(&out, -1).is_empty());
    r.line("resume c");
    out.extend(r.run(8000));
    let (kick, hat) = (at(&out, -1), at(&out, -2));
    assert_eq!((kick.len(), hat.len()), (1, 1));
    // 2500 frames of the clock were left when it resumed
    assert!(kick[0].abs_diff(1500 + 5000 + 2500) <= 1, "{kick:?}");
    // segment 2 counts from where segment 1 really fired
    assert_eq!(hat[0] - kick[0], 4000);
}

#[test]
fn runner_two_segments_ahead() {
    let mut r = rig();
    let mut script = String::from("start a\n");
    for _ in 0..6 {
        script.push_str("wait 2 by c\npause a\nresume a\n");
    }
    r.script(&script).unwrap();
    let mut most = 0;
    for _ in 0..200 {
        r.run(BLOCK);
        for n in r.cp.runner_ahead() {
            most = most.max(n);
            assert!(n <= 2, "{n} segments are sent beyond the last fired");
        }
    }
    assert_eq!(most, 2);
    // all of it ran: the script is gone and nothing waits
    assert!(r.cp.runner_ahead().is_empty());
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
}

#[test]
fn runner_lookahead_is_a_setting() {
    // export has no audio thread to keep up with: it sets the lookahead high
    let mut r = rig();
    r.cp.set_runner_ahead(5);
    let mut script = String::from("start a\n");
    for _ in 0..8 {
        script.push_str("wait 2 by c\npause a\nresume a\n");
    }
    r.script(&script).unwrap();
    assert_eq!(r.cp.runner_ahead(), [5]);
    let mut most = 0;
    for _ in 0..400 {
        r.run(BLOCK);
        for n in r.cp.runner_ahead() {
            most = most.max(n);
            assert!(n <= 5);
        }
    }
    assert_eq!(most, 5);
    assert!(r.cp.scripts_idle());
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
}

#[test]
fn long_lookahead_stops_at_the_table_and_goes_on() {
    // the table of waiting lines is the bound, not an error
    let mut r = rig_with(Limits {
        scheduled: 16,
        ..Limits::default()
    });
    r.cp.set_runner_ahead(1000);
    let mut script = String::from("start a\n");
    for _ in 0..30 {
        script.push_str("wait 1 by c\npause a\nresume a\n");
    }
    r.script(&script).unwrap();
    assert!(!r.cp.runner_ahead().is_empty());
    for _ in 0..800 {
        r.run(BLOCK);
    }
    assert!(r.cp.scripts_idle(), "{:?}", r.log);
    assert!(
        r.log.iter().all(|l| !l.contains("cancelled")),
        "{:?}",
        r.log
    );
}

#[test]
fn segment_line_skipped_w107() {
    let mut r = rig();
    r.script(
        "kick gain=1\n\
         wait 2 by c\nkick gain=0.9\n\
         wait 2 by c\nkick gain=1\n\
         wait 2 by c\nhat gain=0.5\nstart kick\n",
    )
    .unwrap();
    // the fourth segment is not lowered yet: its voice goes
    r.line("remove hat");
    let out = r.run(8000);
    assert!(
        r.log
            .iter()
            .any(|l| l.contains("W107") && l.contains("hat")),
        "{:?}",
        r.log
    );
    // the rest of the segment still ran
    near(&at(&out, -1), &[6000]);
}

#[test]
fn end_stops_script_objects_only() {
    let mut r = rig();
    r.line("start cc");
    r.script("start a\nstart b\nwait 4 by c\nend\nwait 4 by c\nstart kick\n")
        .unwrap();
    r.run(2000);
    assert!(r.running("a") && r.running("b") && r.running("cc"));
    let out = r.run(8000);
    assert!(!r.running("a") && !r.running("b"));
    // started at the prompt, not by the script
    assert!(r.running("cc"));
    // later segments never fire
    assert!(at(&out, -1).is_empty());
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
    assert!(!r.state().contains("scripts:"));
}

#[test]
fn end_after_is_wait_then_end() {
    let mut r = rig();
    r.script("start a\nend after 4 by c\n").unwrap();
    r.run(3900);
    assert!(r.running("a"));
    r.run(300);
    assert!(!r.running("a"));
}

#[test]
fn end_inside_a_then_chain() {
    let mut r = rig();
    r.line("a gain=1");
    r.script("start a\nwait 1 by c\na gain=0 over 3b then end\n")
        .unwrap();
    r.run(3000);
    // `over 3b` of a clock of a 1000-frame tick: the end is due at tick 4
    assert!(r.running("a"));
    r.run(2500);
    assert!(!r.running("a"));
}

#[test]
fn implicit_end_stops_nothing() {
    let mut r = rig();
    r.script("start a\nwait 2 by c\nstart b\n").unwrap();
    assert!(r.state().contains("# scripts: t"));
    r.run(3000);
    assert!(r.running("a") && r.running("b"));
    assert!(!r.state().contains("scripts:"));
    assert!(r.cp.runner_ahead().is_empty());
}

fn with_sources(r: &mut Rig, name: &str, text: &str) -> std::path::PathBuf {
    // one folder per call: the tests run in parallel
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("blast-pkb1-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.blast")), text).unwrap();
    r.cp.set_sources_dir(dir.clone());
    dir
}

#[test]
fn source_stop_cancels() {
    let mut r = rig();
    let dir = with_sources(
        &mut r,
        "demo",
        "start a\nwait 4 by c\nstart kick\nwait 4 by c\nstart hat\n",
    );
    r.line("source demo");
    assert!(r.state().contains("# scripts: demo"));
    r.run(1000);
    r.line("source demo stop");
    let out = r.run(10_000);
    // entries retired, nothing stopped, nothing more fired
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
    assert!(r.running("a"));
    assert!(at(&out, -1).is_empty() && at(&out, -2).is_empty());
    assert!(!r.state().contains("scripts:"));
    // not running any more
    let errs = lang(r.try_line("source demo stop").unwrap_err());
    assert_eq!(codes(&errs), [Code::UnknownName]);
    assert!(errs[0].message.contains("no script 'demo' is running"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn end_at_prompt_e206() {
    let mut r = rig();
    let errs = lang(r.try_line("end").unwrap_err());
    assert_eq!(codes(&errs), [Code::NoScript]);
    assert!(errs[0].message.contains("end: no script is running"));

    let dir = with_sources(&mut r, "demo", "start a\nwait 4 by c\nstart kick\n");
    r.line("source demo");
    let errs = lang(r.try_line("end").unwrap_err());
    assert_eq!(codes(&errs), [Code::NoScript]);
    assert!(errs[0].message.contains("name the script (end demo)"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn end_name_ends() {
    let mut r = rig();
    let dir = with_sources(
        &mut r,
        "demo",
        "start a\nwait 4 by c\nstart kick\nwait 4 by c\nstart hat\n",
    );
    r.line("start cc");
    r.line("source demo");
    r.run(1000);
    assert!(r.running("a"));
    r.line("end demo");
    let out = r.run(10_000);
    assert!(!r.running("a"));
    assert!(r.running("cc"));
    assert!(at(&out, -1).is_empty() && at(&out, -2).is_empty());
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
    let errs = lang(r.try_line("end demo").unwrap_err());
    assert_eq!(codes(&errs), [Code::UnknownName]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn end_name_is_prompt_only() {
    let mut r = rig();
    let errs = r.script("end other\n").unwrap_err();
    assert_eq!(codes(&errs), [Code::Unexpected]);
}

#[test]
fn state_lists_running_scripts() {
    let mut r = rig();
    r.script("start a\nwait 4 by c\nstart kick\nwait 6 by c\nstart hat\n")
        .unwrap();
    let s = r.state();
    assert!(s.contains("# scripts: t  line 2, next wait 4 by c"), "{s}");
    r.run(4500);
    let s = r.state();
    assert!(s.contains("line 4, next wait 6 by c"), "{s}");
    // the output is still valid input: the list is a comment
    assert!(s.lines().all(|l| !l.starts_with("scripts")));
}

#[test]
fn script_without_waits_unchanged() {
    let mut r = rig();
    r.script("start a\nstart b\n").unwrap();
    // one batch, no marks, nothing running
    assert!(r.cp.runner_ahead().is_empty());
    assert!(!r.state().contains("scripts:"));
    r.run(BLOCK);
    assert!(r.running("a") && r.running("b"));
}

#[test]
fn segment_cannot_stage_buffers_e410() {
    let mut r = rig();
    let errs = r.script("wait 1 by c\nbuffer bf length=4f\n").unwrap_err();
    assert_eq!(codes(&errs), [Code::CannotWait]);
    assert!(errs[0].message.contains("can't wait yet"));
    // before the first wait it is fine
    r.script("buffer bf length=4f\nwait 1 by c\nstart a\n")
        .unwrap();
}

#[test]
fn scripts_limit_e601() {
    let mut r = rig_with(Limits {
        scripts: 1,
        ..Limits::default()
    });
    r.script("wait 4 by c\nstart kick\n").unwrap();
    let errs = r.script("wait 4 by c\nstart hat\n").unwrap_err();
    assert_eq!(codes(&errs), [Code::LimitReached]);
    // the slot is free again once the first has run
    r.run(5000);
    r.script("wait 4 by c\nstart hat\n").unwrap();
}

#[test]
fn removed_clock_cancels_script() {
    let mut r = rig();
    r.line("clock d 1000f");
    r.line("start d");
    r.script("wait 4 by d\nstart kick\nwait 4 by d\nstart hat\nwait 4 by d\nstart a\n")
        .unwrap();
    r.line("remove d");
    r.run(2000);
    assert!(
        r.log.iter().any(|l| l.contains("is cancelled")),
        "{:?}",
        r.log
    );
    assert!(!r.state().contains("scripts:"));
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
}

#[test]
fn lone_wait_stays_e201_at_the_prompt() {
    let mut r = rig();
    let errs = lang(r.try_line("wait 4").unwrap_err());
    assert_eq!(codes(&errs), [Code::Unexpected]);
}

#[test]
fn end_is_reserved() {
    let mut r = rig();
    let errs = lang(r.try_line("voice end from \"kick\"").unwrap_err());
    assert_eq!(codes(&errs), [Code::ReservedWord]);
}

#[test]
fn check_finds_bad_waits_and_creation_after_them() {
    let mut r = rig();
    // a wait on a clock that doesn't exist, and a duration that is zero
    let errs = r.script("wait 4 by nope\nwait 0\n").unwrap_err();
    assert_eq!(codes(&errs), [Code::UnknownName, Code::OutOfRange]);
}

#[test]
fn end_and_source_stop_parse() {
    use super::ast::Stmt;
    use super::{Parse, Source, parse};
    let first = |text: &str| match parse(Source { name: "t", text }) {
        Ok(Parse::Complete(script)) => script.lines[0].stmts().next().unwrap().clone(),
        _ => panic!("'{text}' did not parse"),
    };
    assert!(matches!(first("end"), Stmt::End(e) if e.name.is_none() && e.after.is_none()));
    assert!(
        matches!(first("end demo"), Stmt::End(e) if e.name.as_ref().is_some_and(|n| n.name == "demo"))
    );
    assert!(matches!(first("end after 16b"), Stmt::End(e) if e.after.is_some()));
    assert!(
        matches!(first("end after 2s by c"), Stmt::End(e) if e.after.as_ref().is_some_and(|w| w.by.is_some()))
    );
    assert!(matches!(first("source demo stop"), Stmt::Source(s) if s.stop && !s.read));
    // `end` joins like any statement
    let line = match parse(Source {
        name: "t",
        text: "kick gain=0 over 8 then end",
    }) {
        Ok(Parse::Complete(script)) => script.lines[0].clone(),
        _ => panic!(),
    };
    assert_eq!(line.parts.len(), 2);
}

#[test]
fn failed_line_leaves_the_runner_as_it_was() {
    let mut r = rig();
    let dir = with_sources(&mut r, "demo", "start a\nwait 4 by c\nstart kick\n");
    assert!(r.try_line("source demo and bogus").is_err());
    assert!(!r.state().contains("scripts:"));
    r.line("source demo");
    assert!(r.try_line("end demo and bogus").is_err());
    assert!(r.state().contains("# scripts: demo"));
    r.line("end demo");
    assert!(!r.state().contains("scripts:"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn drained_slot_is_not_reused_while_its_tail_waits() {
    let mut r = rig();
    r.line("start a");
    r.line("start b");
    // A ends with its last segment; its `then` tail (stop a) is due 8 ticks on
    r.script("wait 1 by c\nb gain=0.5 over 8b then stop a\n")
        .unwrap();
    r.run(1500);
    assert!(!r.state().contains("scripts:"));
    // B takes another slot, and its `end` leaves A's tail alone
    r.script("start kick\nend\n").unwrap();
    r.run(5000);
    assert!(r.running("a"));
    r.run(5000);
    assert!(!r.running("a"));
}

#[test]
fn draining_slots_count_against_the_limit() {
    let mut r = rig();
    r.line("start b");
    for _ in 0..8 {
        r.script("wait 1 by c\nb gain=0.5 over 3b then b gain=1\n")
            .unwrap();
    }
    r.run(1500);
    assert!(!r.state().contains("scripts:"));
    let errs = r.script("wait 1 by c\nstart kick\n").unwrap_err();
    assert_eq!(codes(&errs), [Code::LimitReached]);
    r.run(4000);
    r.script("wait 1 by c\nstart kick\n").unwrap();
}

// ---- staged creation (PKb2) ----

fn in_engine(r: &Rig, name: &str) -> bool {
    r.cp.engine_state
        .voice_id(name)
        .is_ok_and(|id| r.c.voices.get(id.index()).is_some())
}

#[test]
fn segment_creates_voice_on_due_frame() {
    let mut r = rig();
    r.script("wait 4 by c\nvoice x from \"kick\" on c\nstart x\n")
        .unwrap();
    // the mirror has it at once; the engine only from the fourth tick
    assert!(r.cp.engine_state.voice_id("x").is_ok());
    assert!(!in_engine(&r, "x"));
    let mut out = r.run(3900);
    assert!(!in_engine(&r, "x"));
    out.extend(r.run(300));
    assert!(in_engine(&r, "x"));
    // started in the same instant: the spike sounds
    let kicks = at(&out, -1);
    assert_eq!(kicks.len(), 1, "{kicks:?}");
    assert!(kicks[0].abs_diff(4000) <= 64, "{kicks:?}");
    r.run(1000);
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
    assert!(r.log.is_empty(), "{:?}", r.log);
}

#[test]
fn later_segment_names_what_an_earlier_one_creates() {
    let mut r = rig();
    r.script("wait 2 by c\nvoice x from \"kick\" on c\nwait 2 by c\nstart x\nx gain=1\n")
        .unwrap();
    // a line typed now can't name it
    let errs = lang(r.try_line("start x").unwrap_err());
    assert_eq!(codes(&errs), [Code::NotCreatedYet]);
    assert!(
        errs[0]
            .message
            .contains("'x' doesn't exist yet (a waiting line creates it)")
    );
    let out = r.run(5000);
    near(&at(&out, -1), &[4000]);
    assert!(r.log.is_empty(), "{:?}", r.log);
    // once it exists it can
    r.line("x gain=1");
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
}

#[test]
fn dropped_segment_frees_ids() {
    let mut r = rig();
    let live = r.cp.engine_state.ids.voices.live();
    r.script("wait 4 by c\nvoice x from \"kick\" on c\nvoice y from \"hat\" on c\n")
        .unwrap();
    assert_eq!(r.cp.engine_state.ids.voices.live(), live + 2);
    r.run(1000);
    r.line("source t stop");
    r.run(2000);
    // neither was made: the mirror forgot them and said so
    assert_eq!(r.cp.engine_state.ids.voices.live(), live);
    assert!(r.cp.engine_state.voice_id("x").is_err());
    assert!(r.cp.engine_state.voice_id("y").is_err());
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
    let said = r.log.join("\n");
    assert!(said.contains("x was not created"), "{said}");
    assert!(said.contains("y was not created"), "{said}");
    // the IDs are free to take again
    r.line("voice x from \"kick\" on c");
    assert_eq!(r.cp.engine_state.ids.voices.live(), live + 1);
}

#[test]
fn cancel_restores_removed() {
    let mut r = rig();
    let dir = with_sources(&mut r, "demo", "wait 4 by c\nremove a\n");
    r.line("source demo");
    // gone for the mirror, still in the engine, its ID held
    assert!(r.cp.engine_state.voice_id("a").is_err());
    assert!(in_engine_id(&r, 2));
    r.run(1000);
    r.line("source demo stop");
    r.run(10_000);
    assert!(r.cp.engine_state.voice_id("a").is_ok());
    assert!(in_engine(&r, "a"));
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
    let said = r.log.join("\n");
    assert!(said.contains("a stays"), "{said}");
    // it is the same object: named lines reach it again
    r.line("a gain=0.5");
    let _ = std::fs::remove_dir_all(dir);
}

// the engine has a voice in slot `n`
fn in_engine_id(r: &Rig, n: usize) -> bool {
    r.c.voices.get(n).is_some()
}

#[test]
fn segment_removal_runs_and_frees_its_id() {
    let mut r = rig();
    let a = r.cp.engine_state.voice_id("a").unwrap();
    r.script("wait 2 by c\nremove a\n").unwrap();
    assert!(r.cp.engine_state.voice_id("a").is_err());
    // a new voice must not take a's ID while the engine still has a
    r.line("voice n from \"ta\" on c");
    assert_ne!(r.cp.engine_state.voice_id("n").unwrap(), a);
    r.run(3000);
    assert!(r.c.voices.get(a.index()).is_none());
    r.line("voice m from \"ta\" on c");
    assert_eq!(r.cp.engine_state.voice_id("m").unwrap(), a);
    assert!(r.log.is_empty(), "{:?}", r.log);
}

#[test]
fn removing_the_voice_a_waiting_line_creates_cancels_nothing_else() {
    // a later segment removes what an earlier one made
    let mut r = rig();
    r.script("wait 2 by c\nvoice x from \"kick\" on c\nwait 2 by c\nremove x\n")
        .unwrap();
    r.run(5000);
    assert!(r.cp.engine_state.voice_id("x").is_err());
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
    assert!(r.log.is_empty(), "{:?}", r.log);
    // cancelled before either ran: both are undone, the ID is free
    let live = r.cp.engine_state.ids.voices.live();
    r.script("wait 9 by c\nvoice y from \"kick\" on c\nwait 2 by c\nremove y\n")
        .unwrap();
    r.run(1000);
    r.line("source t stop");
    r.run(3000);
    assert_eq!(r.cp.engine_state.ids.voices.live(), live);
    assert!(r.cp.engine_state.voice_id("y").is_err());
    assert_eq!(r.cp.engine_state.scheduled_pending(), 0);
}

#[test]
fn check_allows_creating_and_naming_after_a_wait() {
    let mut r = rig();
    // the check lowers everything against a copy: x is made after the wait and
    // named after it
    r.script("wait 1 by c\nvoice x from \"kick\" on c\nx gain=0.5\nwait 1 by c\nstart x\n")
        .unwrap();
    // a group can be, a buffer can't
    r.script("wait 1 by c\ngroup g { a }\n").unwrap();
    let errs = r.script("wait 1 by c\nbuffer bf length=4f\n").unwrap_err();
    assert_eq!(codes(&errs), [Code::CannotWait]);
}

// ---- shifters in a timed script (SHFIX) ----

#[test]
fn later_segments_send_shifters() {
    for (shift, want) in [("spectral", true), ("grains", false)] {
        let mut r = rig();
        let id = r.cp.engine_state.voice_id("a").unwrap();
        r.script(&format!(
            "start kick\nwait 2 by c\na shift={shift} pitch=3st\n"
        ))
        .unwrap();
        let has = |r: &Rig| r.c.voices.get(id.index()).unwrap().shifters.is_some();
        // installed when the segment is lowered, ahead of its wait
        assert_eq!(has(&r), want, "{shift}");
        r.run(4000);
        assert_eq!(has(&r), want, "{shift}");
    }
}

#[test]
fn first_segment_sends_shifters() {
    let mut r = rig();
    let id = r.cp.engine_state.voice_id("a").unwrap();
    r.script("a shift=spectral pitch=3st\nwait 2 by c\nstart kick\n")
        .unwrap();
    assert!(r.c.voices.get(id.index()).unwrap().shifters.is_some());
}

// A waiting line's shifters are installed when it is lowered, not carried by
// its entry: if the engine drops the entry (`source NAME stop`), the mirror
// still says they are built, and they are (REQ-134 review). Chosen over
// clearing `shifters_built` on drop: an early install is inert and costs
// nothing the line wouldn't.
#[test]
fn stopped_script_leaves_shifters_installed() {
    fn owes(c: &Command) -> bool {
        match c {
            Command::SetShifters(_) => true,
            Command::Batch(b) => b.cmds.iter().any(owes),
            _ => false,
        }
    }
    let mut r = rig();
    let id = r.cp.engine_state.voice_id("a").unwrap();
    r.line("a shift=spectral");
    r.line("start a");
    r.script("wait 4 by c\na pitch=3st\n").unwrap();
    let has = |r: &Rig| r.c.voices.get(id.index()).unwrap().shifters.is_some();
    // installed now, before the wait is over
    assert!(has(&r));
    r.line("source t stop");
    r.run(500);
    assert!(has(&r));
    let Ok(Parsed::Engine(cmd)) = r.cp.parse("a pitch=5st") else {
        panic!("not an engine command")
    };
    assert!(!owes(&cmd));
}
