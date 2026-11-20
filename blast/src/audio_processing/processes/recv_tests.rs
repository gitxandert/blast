//! RCV's engine tests: receivers (`trig`, `melo`), fan-out and fan-in,
//! packets and session seqs. Each drives the language into a `CmdProcessor`
//! and a `Conductor`, as the REPL does.

use crate::audio_processing::{
    blast_time::sample_rate,
    commands::{CmdProcessor, VoiceId},
    engine::{Conductor, Garbage, VoiceParam},
    test_util::{pcm, render_i16, run, rx, setup, vid},
};
use crate::file_parsing::decode_helpers::AudioFile;

// mono track whose first sample is -1 and every other is 1, so a frame
// renders -1 only when a voice is (re)triggered
fn kick() -> AudioFile {
    let mut samples = vec![1i16; 400_000];
    samples[0] = -1;
    AudioFile::new("kick", "wav", 48000, 1, 16, pcm(&samples))
}

// a session with `main` running (120b: a step every 24000 frames); each line
// runs on both sides
fn session(lines: &[&str]) -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![kick()]);
    let boot = cp.boot();
    c.apply(boot);
    for l in lines {
        run(&mut cp, &mut c, l);
    }
    (cp, c)
}

const STEP: usize = 24_000;

fn hits_of(c: &Conductor, v: VoiceId) -> u32 {
    c.voices
        .get(v.index())
        .expect("no such voice")
        .state
        .trigger_count()
}

fn pitch_of(c: &Conductor, v: VoiceId) -> f32 {
    c.voices
        .get(v.index())
        .expect("no such voice")
        .state
        .get_param(VoiceParam::Pitch)
}

// frames that rendered the trigger of one voice (-1)
fn triggers(out: &[i16]) -> Vec<usize> {
    out.iter()
        .enumerate()
        .filter(|(_, s)| **s == -1)
        .map(|(f, _)| f)
        .collect()
}

#[test]
fn trig_other_voice() {
    let (cp, mut c) = session(&[
        "voice a from \"kick\"",
        "voice b from \"kick\"",
        "a { seq s 1 2 | trig b }",
        "start a b",
    ]);
    let out = render_i16(&mut c, 1, STEP + 100);
    // a is sequenced and never hit, so only b is heard
    assert_eq!(triggers(&out), vec![0, STEP]);
    assert_eq!(hits_of(&c, vid(&cp, "a")), 0);
    assert_eq!(hits_of(&c, vid(&cp, "b")), 2);
}

#[test]
fn trig_group_members() {
    let (cp, mut c) = session(&[
        "voice a from \"kick\"",
        "voice b from \"kick\"",
        "voice d from \"kick\"",
        "group g { b; d }",
        "a { seq s 1 2 | trig g }",
        "start a g",
    ]);
    render_i16(&mut c, 1, STEP + 100);
    assert_eq!(hits_of(&c, vid(&cp, "g.b")), 2);
    assert_eq!(hits_of(&c, vid(&cp, "g.d")), 2);
    assert_eq!(hits_of(&c, vid(&cp, "a")), 0);
}

#[test]
fn fan_out_trig_and_melo() {
    let (cp, mut c) = session(&[
        "voice kick from \"kick\"",
        "voice bass from \"kick\"",
        "kick { seq beat 1 2 | trig, melo bass 36 43 }",
        "start kick bass",
    ]);
    let (k, b) = (vid(&cp, "kick"), vid(&cp, "bass"));
    render_i16(&mut c, 1, 100);
    assert_eq!((hits_of(&c, k), hits_of(&c, b)), (1, 1));
    assert_eq!(pitch_of(&c, b), 36.0 - 60.0);
    render_i16(&mut c, 1, STEP);
    assert_eq!((hits_of(&c, k), hits_of(&c, b)), (2, 2));
    assert_eq!(pitch_of(&c, b), 43.0 - 60.0);
}

#[test]
fn melo_wraps_notes() {
    let (cp, mut c) = session(&[
        "voice kick from \"kick\"",
        "voice bass from \"kick\"",
        "kick { seq beat 1 2 3 | melo bass 36 48 }",
        "start kick bass",
    ]);
    let b = vid(&cp, "bass");
    let mut seen = Vec::new();
    for n in 0..3 {
        render_i16(&mut c, 1, if n == 0 { 100 } else { STEP });
        seen.push(pitch_of(&c, b));
    }
    assert_eq!(seen, vec![-24.0, -12.0, -24.0]);
    // melo alone does not hit the owner
    assert_eq!(hits_of(&c, vid(&cp, "kick")), 0);
}

#[test]
fn melo_note_60_is_unshifted() {
    let (cp, mut c) = session(&[
        "voice kick from \"kick\"",
        "voice bass from \"kick\" pitch=5st",
        "kick { seq beat 1 | melo bass 60 }",
        "start kick bass",
    ]);
    let b = vid(&cp, "bass");
    render_i16(&mut c, 1, 100);
    assert_eq!(pitch_of(&c, b), 0.0);
    assert_eq!(hits_of(&c, b), 1);
}

#[test]
fn fan_in_two_seqs_one_target() {
    let (cp, mut c) = session(&[
        "voice a from \"kick\"",
        "voice b from \"kick\"",
        "voice snare from \"kick\"",
        "a { seq s 1 | trig snare }",
        "b { seq s 2 | trig snare }",
        "start a b snare",
    ]);
    render_i16(&mut c, 1, STEP + 100);
    assert_eq!(hits_of(&c, vid(&cp, "snare")), 2);
}

#[test]
fn packet_fills_trig_snare() {
    let (cp, mut c) = session(&[
        "voice snare from \"kick\"",
        "packet fills {\n  seq a 1 in 4\n  seq b 2 in 4\n} | trig snare",
        "start snare",
    ]);
    // the seqs run on `main`, which the session started
    let out = render_i16(&mut c, 1, STEP + 100);
    assert_eq!(triggers(&out), vec![0, STEP]);
    assert_eq!(hits_of(&c, vid(&cp, "snare")), 2);
}

#[test]
fn packet_actions_and_remove() {
    let (mut cp, mut c) = session(&[
        "voice snare from \"kick\"",
        "packet fills {\n  seq a 1 2 3 4 on 24000f\n  seq b 1 2 3 4 on 24000f\n} | trig snare",
        "start snare",
        "pause fills",
    ]);
    let snare = vid(&cp, "snare");
    render_i16(&mut c, 1, 10 * STEP);
    assert_eq!(hits_of(&c, snare), 0);
    // resume: both seqs fire again, two hits per step
    run(&mut cp, &mut c, "resume fills");
    render_i16(&mut c, 1, STEP / 2);
    let after_resume = hits_of(&c, snare);
    render_i16(&mut c, 1, 3 * STEP);
    assert!(hits_of(&c, snare) >= after_resume + 4);
    // stop sends no bangs
    run(&mut cp, &mut c, "stop fills");
    let stopped = hits_of(&c, snare);
    render_i16(&mut c, 1, 3 * STEP);
    assert_eq!(hits_of(&c, snare), stopped);
    // remove takes the seqs with it
    assert_eq!(c.procs.len(), 2);
    run(&mut cp, &mut c, "remove fills");
    assert_eq!(c.procs.len(), 0);
    assert!(cp.engine_state.packets.is_empty());
    assert!(cp.engine_state.procs.is_empty());
    assert!(cp.engine_state.session_procs.is_empty());
}

#[test]
fn session_seq_with_pipe() {
    let (cp, mut c) = session(&[
        "voice kick from \"kick\"",
        "seq x 1 3 | trig kick",
        "start kick",
    ]);
    // nothing started the seq: the session runs it with main
    let out = render_i16(&mut c, 1, 2 * STEP + 100);
    assert_eq!(triggers(&out), vec![0, 2 * STEP]);
    assert_eq!(hits_of(&c, vid(&cp, "kick")), 2);
}

#[test]
fn restate_pipe_replaces_receivers() {
    let (cp, mut c) = session(&[
        "voice kick from \"kick\"",
        "voice bass from \"kick\"",
        "voice hat from \"kick\"",
        "kick { seq s 1 2 | melo bass 36 }",
        "start kick bass hat",
    ]);
    let mut garbage = rx(&mut c);
    let (b, h) = (vid(&cp, "bass"), vid(&cp, "hat"));
    // hat has no seq, so its start played it once
    let h0 = hits_of(&c, h);
    render_i16(&mut c, 1, 100);
    assert_eq!((hits_of(&c, b), hits_of(&c, h)), (1, h0));
    garbage.drain_garbage();

    let mut cp = cp;
    run(&mut cp, &mut c, "kick.s | trig hat");
    // the old Vec (it owns the melo notes) left through the garbage queue
    let Ok(Garbage::SeqData(old)) = garbage.garbage.pop() else {
        panic!("no SeqData in the garbage queue");
    };
    assert_eq!(old.recv.len(), 1);
    assert!(garbage.garbage.pop().is_err());

    render_i16(&mut c, 1, STEP);
    assert_eq!((hits_of(&c, b), hits_of(&c, h)), (1, h0 + 1));
}

#[test]
fn receiver_target_parks_on_start() {
    let (cp, mut c) = session(&[
        "voice kick from \"kick\"",
        "voice hat from \"kick\"",
        "kick { seq s 2 | trig hat }",
        "start hat",
    ]);
    // hat is a target, so it waits, silent, for its seq's step
    let out = render_i16(&mut c, 1, STEP / 2);
    assert!(out.iter().all(|&s| s == 0 || s == 1), "{:?}", &out[..4]);
    assert_eq!(hits_of(&c, vid(&cp, "hat")), 0);
    // the seq isn't running (kick is stopped), so a start of kick gets there
    let mut cp = cp;
    run(&mut cp, &mut c, "start kick");
    render_i16(&mut c, 1, STEP + 100);
    assert_eq!(hits_of(&c, vid(&cp, "hat")), 1);
}

#[test]
fn removing_a_target_drops_its_receiver() {
    let (mut cp, mut c) = session(&[
        "voice kick from \"kick\"",
        "voice bass from \"kick\"",
        "kick { seq s 1 | trig, melo bass 36 }",
        "start kick bass",
    ]);
    run(&mut cp, &mut c, "remove bass");
    // a new voice takes the freed ID; the seq must not hit it
    run(&mut cp, &mut c, "voice other from \"kick\"");
    run(&mut cp, &mut c, "start other");
    render_i16(&mut c, 1, 100);
    assert_eq!(hits_of(&c, vid(&cp, "other")), 1, "only its own start");
    assert_eq!(hits_of(&c, vid(&cp, "kick")), 1);
}
