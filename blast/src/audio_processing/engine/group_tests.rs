use super::*;
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::test_util::{
    ctx, gid, pcm, render, render_i16, run, setup, setup_with, to_i16, track, vid,
};

fn voice<'a>(c: &'a Conductor, cp: &CmdProcessor, name: &str) -> &'a Voice {
    c.voices.get(vid(cp, name).index()).unwrap()
}

fn group<'a>(c: &'a Conductor, cp: &CmdProcessor, name: &str) -> &'a Group {
    c.groups.get(gid(cp, name).index()).unwrap()
}

fn tempo(c: &Conductor, id: TempoId) -> &TempoState {
    &c.tempos.get(id.index()).unwrap().state
}

// the tempos of the processes a voice owns, in process ID order
fn seq_tempos(c: &Conductor, cp: &CmdProcessor, name: &str) -> Vec<TempoId> {
    let owner = ProcOwner::Voice(vid(cp, name));
    c.procs
        .iter()
        .filter(|(_, p)| p.owner == owner)
        .map(|(_, p)| p.tempo)
        .collect()
}

// voices not in a group, in ID order
fn top_level(c: &Conductor) -> Vec<i16> {
    c.voices
        .iter()
        .filter(|(_, v)| v.group.is_none())
        .map(|(_, v)| id(v))
        .collect()
}

// tracks are constant-valued, so track[0] identifies a Voice
fn id(v: &Voice) -> i16 {
    to_i16(v.track()[0])
}

// `voice <n> from "<n>" on 1000ms`: a voice with a one-second clock of its own
fn own_voice(n: &str) -> String {
    format!("voice {n} from \"{n}\" on 1000ms")
}

// the session's `main` exists in the mirror and the engine
fn boot(cp: &mut CmdProcessor, c: &mut Conductor) {
    c.apply(cp.boot());
}

// tempo references (B15)

#[test]
fn seq_voice_tempo_shares_its_voice_id() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("kick", 1, 16), track("snare", 2, 16)]);
    run(&mut cp, &mut c, &own_voice("kick"));
    run(&mut cp, &mut c, "voice snare from \"snare\" on 120b");
    run(&mut cp, &mut c, "snare { seq s 1 }");

    let seq = seq_tempos(&c, &cp, "snare");
    assert_eq!(seq, vec![voice(&c, &cp, "snare").state.tempo]);
    assert_ne!(seq[0], voice(&c, &cp, "kick").state.tempo);
}

#[test]
fn seq_on_a_voice_that_follows_main_shares_main() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("kick", 1, 16), track("snare", 2, 16)]);
    boot(&mut cp, &mut c);
    run(&mut cp, &mut c, "voice kick from \"kick\"");
    run(&mut cp, &mut c, "voice snare from \"snare\"");
    run(&mut cp, &mut c, "snare { seq s 1 }");

    let seq = seq_tempos(&c, &cp, "snare");
    assert_eq!(seq, vec![voice(&c, &cp, "snare").state.tempo]);
    assert_eq!(seq, vec![ctx(&cp, "main")]);
}

#[test]
fn group_tempo_refs_share_the_right_group_id() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(
        1,
        vec![
            track("a", 1, 16),
            track("b", 2, 16),
            track("c", 3, 16),
            track("d", 4, 16),
        ],
    );
    run(&mut cp, &mut c, &own_voice("a"));
    run(&mut cp, &mut c, &own_voice("b"));
    run(&mut cp, &mut c, &own_voice("c"));
    run(&mut cp, &mut c, "group g1 on 240b { a }");
    run(&mut cp, &mut c, "group g2 on 240b { b }");
    run(&mut cp, &mut c, "c { seq s 1 on g2 }");
    run(&mut cp, &mut c, "voice d from \"d\" on g1");

    let g1 = group(&c, &cp, "g1").state.tempo;
    let g2 = group(&c, &cp, "g2").state.tempo;
    assert_ne!(g1, g2);
    assert_eq!(seq_tempos(&c, &cp, "c"), vec![g2]);
    assert_eq!(voice(&c, &cp, "d").state.tempo, g1);
}

#[test]
fn context_tempo_refs_share_the_context_id() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 1, 16)]);
    run(&mut cp, &mut c, "clock c1 120b");
    run(&mut cp, &mut c, "clock c2 90b");
    run(&mut cp, &mut c, "voice a from \"a\" on c2");
    run(&mut cp, &mut c, "a { seq s 1 on c1 }");

    assert_eq!(voice(&c, &cp, "a").state.tempo, ctx(&cp, "c2"));
    assert_eq!(seq_tempos(&c, &cp, "a"), vec![ctx(&cp, "c1")]);
    assert_eq!(
        c.tempos.get(ctx(&cp, "c2").index()).unwrap().owner,
        TempoOwner::Context
    );
}

// grouping

#[test]
fn group_voices_follow_mirror_order() {
    sample_rate::set(48000);
    for (cmd, want) in [
        ("group g on 240b { c; a }", [3, 1]),
        ("group g on 240b { a; c }", [1, 3]),
    ] {
        let (mut cp, mut c) = setup(
            1,
            vec![track("a", 1, 16), track("b", 2, 16), track("c", 3, 16)],
        );
        run(&mut cp, &mut c, &own_voice("a"));
        run(&mut cp, &mut c, &own_voice("b"));
        run(&mut cp, &mut c, &own_voice("c"));
        run(&mut cp, &mut c, cmd);

        let g = group(&c, &cp, "g");
        assert_eq!(g.members.len(), 2);
        let ids: Vec<i16> = g
            .members
            .iter()
            .map(|m| id(c.voices.get(m.index()).unwrap()))
            .collect();
        assert_eq!(ids, want);
        assert_eq!(top_level(&c), vec![2]);
    }
}

#[test]
fn grouped_voices_play_through_the_group() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 100, 16), track("b", 20, 16)]);
    run(&mut cp, &mut c, &own_voice("a"));
    run(&mut cp, &mut c, &own_voice("b"));
    run(&mut cp, &mut c, "group g on 240b { b; a }");
    run(&mut cp, &mut c, "start g");

    let out = render_i16(&mut c, 1, 4);
    assert_eq!(out[0], 120);
}

#[test]
fn grouped_voice_stays_in_voice_arena() {
    sample_rate::set(48000);
    let limits = Limits {
        voices: 4,
        group_members: 3,
        ..Limits::default()
    };
    let (mut cp, mut c) = setup_with(limits, 1, vec![track("a", 1, 16), track("b", 2, 16)]);
    run(&mut cp, &mut c, &own_voice("a"));
    run(&mut cp, &mut c, &own_voice("b"));
    run(&mut cp, &mut c, "group g on 240b { a; b }");

    let g = gid(&cp, "g");
    let (a, b) = (vid(&cp, "g.a"), vid(&cp, "g.b"));
    assert_eq!(c.voices.len(), 2);
    assert_eq!(c.voices.capacity(), 4);
    assert_eq!(c.voices.get(a.index()).unwrap().group, Some(g));
    assert_eq!(c.voices.get(b.index()).unwrap().group, Some(g));
    let members = &c.groups.get(g.index()).unwrap().members;
    assert_eq!(members, &vec![a, b]);
    // the Vec the mirror allocated, moved in whole
    assert_eq!(members.capacity(), 3);
}

// addressing grouped voices (B04)

fn grouped() -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(
        1,
        vec![track("a", 1, 16), track("b", 2, 16), track("c", 3, 16)],
    );
    run(&mut cp, &mut c, &own_voice("a"));
    run(&mut cp, &mut c, "voice b from \"b\" on 120b");
    run(&mut cp, &mut c, &own_voice("c"));
    run(&mut cp, &mut c, "group g on 240b { a; b }");
    (cp, c)
}

#[test]
fn rate_reaches_group_voice() {
    let (mut cp, mut c) = grouped();
    run(&mut cp, &mut c, "g.b rate=2");

    assert_eq!(id(voice(&c, &cp, "g.b")), 2);
    assert_eq!(voice(&c, &cp, "g.b").state.get_param(VoiceParam::Rate), 2.0);
    assert_eq!(voice(&c, &cp, "g.a").state.get_param(VoiceParam::Rate), 1.0);
    assert_eq!(voice(&c, &cp, "c").state.get_param(VoiceParam::Rate), 1.0);

    run(&mut cp, &mut c, "c rate=0.5");
    assert_eq!(voice(&c, &cp, "c").state.get_param(VoiceParam::Rate), 0.5);
}

#[test]
fn start_stop_reach_group_voice() {
    let (mut cp, mut c) = grouped();
    run(&mut cp, &mut c, "start g.a");
    assert!(voice(&c, &cp, "g.a").state.active);
    assert!(!voice(&c, &cp, "g.b").state.active);
    assert!(!voice(&c, &cp, "c").state.active);

    run(&mut cp, &mut c, "pause g.a");
    assert!(!voice(&c, &cp, "g.a").state.active);
    run(&mut cp, &mut c, "resume g.a");
    assert!(voice(&c, &cp, "g.a").state.active);
    run(&mut cp, &mut c, "stop g.a");
    assert!(!voice(&c, &cp, "g.a").state.active);
}

#[test]
fn seq_reaches_group_voice() {
    let (mut cp, mut c) = grouped();
    run(&mut cp, &mut c, "g.b { seq s 1 }");

    assert_eq!(
        seq_tempos(&c, &cp, "g.b"),
        vec![voice(&c, &cp, "g.b").state.tempo]
    );
    assert!(seq_tempos(&c, &cp, "g.a").is_empty());
    assert!(seq_tempos(&c, &cp, "c").is_empty());
}

// group tempo (B12)

#[test]
fn group_shares_context_id() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 1, 16)]);
    boot(&mut cp, &mut c);
    run(&mut cp, &mut c, "clock other 60b");
    run(&mut cp, &mut c, "clock clk 120b");
    // a voice that follows main follows the group it joins, i.e. the clock
    run(&mut cp, &mut c, "voice a from \"a\"");
    run(&mut cp, &mut c, "group g on clk { a }");

    let clk = ctx(&cp, "clk");
    assert_eq!(group(&c, &cp, "g").state.tempo, clk);
    assert_eq!(voice(&c, &cp, "g.a").state.tempo, clk);
    assert_eq!(
        c.tempos.get(clk.index()).unwrap().owner,
        TempoOwner::Context
    );
}

// flags / process ids stay with their voice (B13)

#[test]
fn group_tempo_goes_to_the_right_voices() {
    sample_rate::set(48000);
    for perm in ["a; b; c", "b; c; a", "c; a; b", "c; b; a"] {
        let (mut cp, mut c) = setup(
            1,
            vec![track("a", 1, 16), track("b", 2, 16), track("c", 3, 16)],
        );
        boot(&mut cp, &mut c);
        run(&mut cp, &mut c, "voice a from \"a\"");
        run(&mut cp, &mut c, "voice b from \"b\" on 90b");
        run(&mut cp, &mut c, "voice c from \"c\"");
        run(&mut cp, &mut c, "a { seq s 1 }");
        run(&mut cp, &mut c, &format!("group g on 240b {{ {perm} }}"));

        let g = group(&c, &cp, "g");
        for m in &g.members {
            let v = c.voices.get(m.index()).unwrap();
            let shares = v.state.tempo == g.state.tempo;
            assert_eq!(shares, id(v) != 2, "voice {} in {}", id(v), perm);
        }
        assert_eq!(seq_tempos(&c, &cp, "g.a"), vec![g.state.tempo]);
    }
}

// only voice-following processes move to the group tempo (B14)

#[test]
fn group_keeps_owned_seq_tempo() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 1, 16)]);
    boot(&mut cp, &mut c);
    run(&mut cp, &mut c, "voice a from \"a\"");
    run(&mut cp, &mut c, "a { seq own 1 on 140b }");
    run(&mut cp, &mut c, "a { seq follow 1 }");
    let own = seq_tempos(&c, &cp, "a")[0];
    run(&mut cp, &mut c, "group g on 240b { a }");

    let g = group(&c, &cp, "g");
    let a = voice(&c, &cp, "g.a");
    assert_eq!(a.state.tempo, g.state.tempo);
    let after = seq_tempos(&c, &cp, "g.a");
    assert_eq!(after, vec![own, g.state.tempo]);
    assert_ne!(own, g.state.tempo);
    assert_eq!(
        c.tempos.get(own.index()).unwrap().owner,
        TempoOwner::Voice(vid(&cp, "g.a"))
    );
}

#[test]
fn group_repoints_voice_following_seq() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 1, 16)]);
    boot(&mut cp, &mut c);
    run(&mut cp, &mut c, "voice a from \"a\"");
    run(&mut cp, &mut c, "a { seq s 1 }");
    run(&mut cp, &mut c, "group g on 240b { a }");

    assert_eq!(
        seq_tempos(&c, &cp, "g.a"),
        vec![group(&c, &cp, "g").state.tempo]
    );
}

#[test]
fn adoption_repoints_voice_following_seq() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 1, 16)]);
    boot(&mut cp, &mut c);
    let main = ctx(&cp, "main");
    run(&mut cp, &mut c, "voice a from \"a\"");
    run(&mut cp, &mut c, "a { seq s 1 }");
    let old = voice(&c, &cp, "a").state.tempo;
    assert_eq!(old, main);
    run(&mut cp, &mut c, "group g on 240b { a }");

    let gt = group(&c, &cp, "g").state.tempo;
    assert_ne!(old, gt);
    // the voice only shared main, so main stays, on both sides
    assert!(c.tempos.get(old.index()).is_some());
    assert!(cp.engine_state.tempos.contains_key(&old));
    assert_eq!(voice(&c, &cp, "g.a").state.tempo, gt);
    assert!(!voice(&c, &cp, "g.a").state.adopts);
    assert_eq!(seq_tempos(&c, &cp, "g.a"), vec![gt]);
}

// A seq with an explicit `on main` on a voice that follows main reads the
// tempo the voice reads, so when the voice joins a group the adopt moves the
// seq to the group's clock with it (LANGb review). A voice with a clock of
// its own does not adopt, and its `on main` seqs stay on main.
#[test]
fn seq_on_main_moves_with_a_voice_that_adopts() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("kick", 1, 16), track("hat", 2, 16)]);
    boot(&mut cp, &mut c);
    let main = ctx(&cp, "main");
    run(&mut cp, &mut c, "voice kick from \"kick\"");
    run(&mut cp, &mut c, "voice hat from \"hat\" on 90b");
    run(&mut cp, &mut c, "kick { seq pinned 1 3 on main }");
    run(&mut cp, &mut c, "hat { seq pinned 1 3 on main }");
    assert_eq!(seq_tempos(&c, &cp, "kick"), vec![main]);
    assert_eq!(seq_tempos(&c, &cp, "hat"), vec![main]);

    run(&mut cp, &mut c, "group drums on 93b { kick; hat }");
    let gt = group(&c, &cp, "drums").state.tempo;
    // kick adopted drums' clock, and its seq went along, in both models
    assert_eq!(seq_tempos(&c, &cp, "drums.kick"), vec![gt]);
    let es = &cp.engine_state;
    let kick_proc = es.voices[&vid(&cp, "drums.kick")].procs["pinned"];
    assert_eq!(es.procs[&kick_proc].tempo, gt);
    // hat keeps its own clock, and its seq stays on main
    assert_eq!(seq_tempos(&c, &cp, "drums.hat"), vec![main]);
    let hat_proc = es.voices[&vid(&cp, "drums.hat")].procs["pinned"];
    assert_eq!(es.procs[&hat_proc].tempo, main);
    // main itself is untouched and still running
    assert!(tempo(&c, main).active);
}

// failed commands don't touch the mirror (B16)

#[test]
fn failed_group_keeps_mirror_and_engine_in_sync() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 1, 16), track("b", 2, 16)]);
    run(&mut cp, &mut c, &own_voice("a"));
    run(&mut cp, &mut c, &own_voice("b"));
    assert!(cp.parse("group g on 240b { a; nope }").is_err());

    run(&mut cp, &mut c, "b rate=2");
    assert_eq!(c.voices.len(), 2);
    assert_eq!(voice(&c, &cp, "b").state.get_param(VoiceParam::Rate), 2.0);
    assert_eq!(voice(&c, &cp, "a").state.get_param(VoiceParam::Rate), 1.0);

    run(&mut cp, &mut c, "group g on 240b { a }");
    let g = group(&c, &cp, "g");
    assert_eq!(id(c.voices.get(g.members[0].index()).unwrap()), 1);
    assert_eq!(top_level(&c), vec![2]);
}

// duplicate names (B17)

#[test]
fn later_names_match_engine_ids_after_duplicates() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 1, 16), track("b", 2, 16)]);
    run(&mut cp, &mut c, "clock x 120b");
    assert!(cp.parse("voice x from \"a\"").is_err());
    run(&mut cp, &mut c, "clock y 60b");
    run(&mut cp, &mut c, &own_voice("a"));
    run(&mut cp, &mut c, &own_voice("b"));
    run(&mut cp, &mut c, "group g on 240b { a }");
    run(&mut cp, &mut c, "group h on y { b }");

    let contexts = c
        .tempos
        .iter()
        .filter(|(_, t)| t.owner == TempoOwner::Context)
        .count();
    assert_eq!(contexts, 2);
    assert_eq!(c.groups.len(), 2);
    let h = group(&c, &cp, "h");
    assert_eq!(h.state.tempo, ctx(&cp, "y"));
    assert_eq!(id(c.voices.get(h.members[0].index()).unwrap()), 2);
}

// leftovers

#[test]
fn empty_track_voice_doesnt_panic() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![AudioFile::new("e", "wav", 48000, 1, 16, vec![])]);
    run(&mut cp, &mut c, &own_voice("e"));
    run(&mut cp, &mut c, "start e");
    assert!(render_i16(&mut c, 1, 8).iter().all(|s| *s == 0));
}

#[test]
fn group_stop_resets_voices() {
    let (mut cp, mut c) = grouped();
    run(&mut cp, &mut c, "start g");
    render(&mut c, 1, 5);
    assert_eq!(voice(&c, &cp, "g.a").state.inst[0].position, 5.0);

    run(&mut cp, &mut c, "stop g");
    for m in &group(&c, &cp, "g").members {
        let v = c.voices.get(m.index()).unwrap();
        assert!(!v.state.active);
        assert_eq!(v.state.inst[0].position, 0.0);
    }
    // b's own tempo is stopped too
    let b = tempo(&c, voice(&c, &cp, "g.b").state.tempo);
    assert!(!b.active);
    assert_eq!(b.current, 0);
}

#[test]
fn unload_group_voice() {
    let (mut cp, mut c) = grouped();
    let b = vid(&cp, "g.b");
    run(&mut cp, &mut c, "remove g.a");
    let g = group(&c, &cp, "g");
    assert_eq!(g.members, vec![b]);
    assert_eq!(id(c.voices.get(b.index()).unwrap()), 2);
    assert_eq!(c.voices.len(), 2);

    // b kept its ID in the mirror and the engine
    run(&mut cp, &mut c, "g.b rate=2");
    assert_eq!(voice(&c, &cp, "g.b").state.get_param(VoiceParam::Rate), 2.0);

    assert!(cp.parse("remove g.a").is_err());
    run(&mut cp, &mut c, "remove c");
    assert!(top_level(&c).is_empty());
}

#[test]
fn unload_frees_procs_and_owned_tempos() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 1, 16), track("b", 2, 16)]);
    run(&mut cp, &mut c, "clock clk 100f");
    run(&mut cp, &mut c, &own_voice("a"));
    run(&mut cp, &mut c, &own_voice("b"));
    run(&mut cp, &mut c, "a { seq p1 1 on 90b }");
    run(&mut cp, &mut c, "a { seq p2 1 }");
    run(&mut cp, &mut c, "a { seq p3 1 on clk }");
    run(&mut cp, &mut c, "b { seq p4 1 }");
    let a = vid(&cp, "a");
    let owned: Vec<TempoId> = c
        .tempos
        .iter()
        .filter(|(_, t)| t.owner == TempoOwner::Voice(a))
        .map(|(id, _)| TempoId(id as u32))
        .collect();
    assert_eq!(owned.len(), 2);
    assert_eq!(c.procs.len(), 4);

    run(&mut cp, &mut c, "remove a");

    assert!(c.procs.iter().all(|(_, p)| p.owner != ProcOwner::Voice(a)));
    assert_eq!(c.procs.len(), 1);
    for t in owned {
        assert!(c.tempos.get(t.index()).is_none());
        assert!(!cp.engine_state.tempos.contains_key(&t));
    }
    // the context and b's tempo survive
    assert_eq!(c.tempos.len(), 2);
    assert!(c.tempos.get(ctx(&cp, "clk").index()).is_some());
    assert_eq!(cp.engine_state.ids.procs.live(), 1);
    assert_eq!(cp.engine_state.ids.tempos.live(), 2);
}

// the Conductor ticks tempos (REQ-027)

#[test]
fn voice_clock_m1000_is_one_second() {
    sample_rate::set(48000);
    // -1 only on the first frame, so a retrigger shows up as -1
    let mut samples = vec![1i16; 60_000];
    samples[0] = -1;
    let (mut cp, mut c) = setup(
        1,
        vec![AudioFile::new("a", "wav", 48000, 1, 16, pcm(&samples))],
    );
    for cmd in [&own_voice("a"), "a { seq s 2 in 4 }", "start a"] {
        run(&mut cp, &mut c, cmd);
    }
    let out = render_i16(&mut c, 1, 50_000);
    let fired: Vec<usize> = (0..out.len()).filter(|&f| out[f] == -1).collect();
    // the voice waits for step 2 (tick 1)
    assert_eq!(fired, vec![48_000]);
}

#[test]
fn tempos_tick_once_per_frame_when_owner_plays() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 1, 16), track("b", 2, 16)]);
    for cmd in [
        "clock clk 100f",
        "voice a from \"a\" on 100f",
        "a { seq s 1 on 50f }",
        "voice b from \"b\" on 100f",
        "b { seq s 1 on 50f }",
        "group g on 240b { b }",
        "start clk",
        "start a",
        "start g",
    ] {
        run(&mut cp, &mut c, cmd);
    }
    let clk = ctx(&cp, "clk");
    let a = voice(&c, &cp, "a").state.tempo;
    let a_seq = seq_tempos(&c, &cp, "a")[0];
    let b = voice(&c, &cp, "g.b").state.tempo;
    let b_seq = seq_tempos(&c, &cp, "g.b")[0];
    let g = group(&c, &cp, "g").state.tempo;
    let current = |c: &Conductor| [clk, a, a_seq, b, b_seq, g].map(|t| tempo(c, t).current);

    render(&mut c, 1, 10);
    assert_eq!(current(&c), [10; 6]);

    // a context ticks while every voice is paused
    run(&mut cp, &mut c, "pause a");
    run(&mut cp, &mut c, "pause g");
    render(&mut c, 1, 10);
    assert_eq!(current(&c), [20, 10, 10, 10, 10, 10]);

    run(&mut cp, &mut c, "resume a");
    render(&mut c, 1, 5);
    assert_eq!(current(&c), [25, 15, 15, 10, 10, 10]);

    // a grouped voice's tempos stop with the voice, the group's keep going
    run(&mut cp, &mut c, "resume g");
    run(&mut cp, &mut c, "pause g.b");
    render(&mut c, 1, 5);
    assert_eq!(current(&c), [30, 20, 20, 10, 10, 15]);
}

#[test]
fn process_then_tempo_order() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(
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
    for cmd in [&own_voice("t"), "t { seq s 1 3 on 100f in 4 }", "start t"] {
        run(&mut cp, &mut c, cmd);
    }
    let out = render_i16(&mut c, 1, 450);
    let fired: Vec<usize> = (1..out.len()).filter(|&f| out[f] == 0).collect();
    // frame 0 is the start (and step 0)
    assert_eq!(out[0], 0);
    assert_eq!(fired, vec![200, 400]);
    assert_eq!(out[199], 199);
    assert_eq!(out[201], 1);
}

// the language: a voice that follows `main` adopts its group's clock

// parse a line of the language and apply it
fn lang(cp: &mut CmdProcessor, c: &mut Conductor, line: &str) {
    match cp.parse(line) {
        Ok(Parsed::Engine(command)) => c.apply(command),
        Ok(Parsed::Local(text)) => panic!("'{line}' is local: {text}"),
        Err(error) => panic!("'{line}' failed: {error}"),
    }
}

#[test]
fn following_voice_adopts_group_tempo() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("kick", 1, 16)]);
    c.apply(cp.boot());
    let main = cp.engine_state.context_id("main").unwrap();

    lang(&mut cp, &mut c, "voice kick from \"kick\"");
    assert_eq!(voice(&c, &cp, "kick").state.tempo, main);
    assert!(voice(&c, &cp, "kick").state.adopts);

    lang(&mut cp, &mut c, "group drums on 93b { kick }");
    let gt = group(&c, &cp, "drums").state.tempo;
    assert_ne!(gt, main);
    assert_eq!(voice(&c, &cp, "drums.kick").state.tempo, gt);
    assert!(!voice(&c, &cp, "drums.kick").state.adopts);
    assert_eq!(cp.engine_state.voices[&vid(&cp, "drums.kick")].tempo, gt);

    // main is still live, and nothing else took its slot
    assert!(c.tempos.get(main.index()).is_some());
    assert!(cp.engine_state.tempos.contains_key(&main));
    assert!(tempo(&c, main).active);
}

// GRP: editing groups

use crate::audio_processing::commands::lang::Code;
use crate::audio_processing::test_util::rx;

// a booted session over three long tracks a, b, c (1000, 2000, 4000)
fn edit_rig() -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(
        1,
        vec![
            track("a", 1000, 100_000),
            track("b", 2000, 100_000),
            track("c", 4000, 100_000),
        ],
    );
    boot(&mut cp, &mut c);
    (cp, c)
}

fn run_all(cp: &mut CmdProcessor, c: &mut Conductor, lines: &[&str]) {
    for l in lines {
        run(cp, c, l);
    }
}

fn mlink(cp: &CmdProcessor, v: &str) -> TempoLink {
    cp.engine_state.voices[&vid(cp, v)].link
}

fn mtempo(cp: &CmdProcessor, v: &str) -> TempoId {
    cp.engine_state.voices[&vid(cp, v)].tempo
}

fn lcode(cp: &mut CmdProcessor, line: &str) -> Code {
    match cp.parse(line) {
        Err(CmdErr::Lang(errs)) => errs[0].code,
        Err(e) => panic!("'{line}': a family error: {e}"),
        Ok(_) => panic!("'{line}' should have failed"),
    }
}

// the mirror and the engine agree on groups, voices and tempos
fn assert_agree(cp: &CmdProcessor, c: &Conductor) {
    let es = &cp.engine_state;
    for (g, repr) in &es.groups {
        let eng = c.groups.get(g.index()).expect("group in the engine");
        assert_eq!(eng.members, repr.members);
        assert_eq!(eng.state.tempo, repr.tempo);
        assert_eq!(eng.state.gain, repr.gain);
    }
    assert_eq!(c.groups.iter().count(), es.groups.len());
    for (v, repr) in &es.voices {
        let eng = c.voices.get(v.index()).expect("voice in the engine");
        assert_eq!(eng.group, repr.group, "{}", repr.name);
        assert_eq!(eng.state.tempo, repr.tempo, "{}", repr.name);
    }
    assert_eq!(c.voices.iter().count(), es.voices.len());
    assert_eq!(c.tempos.iter().count(), es.tempos.len());
    for (t, repr) in &es.tempos {
        assert_eq!(c.tempos.get(t.index()).unwrap().owner, repr.owner);
    }
}

#[test]
fn add_to_existing_group() {
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "b { seq s 1 }",
            "group g on 240b { a }",
            "group g { b }",
        ],
    );
    let g = group(&c, &cp, "g");
    let (a, b) = (vid(&cp, "g.a"), vid(&cp, "g.b"));
    assert_eq!(g.members, vec![a, b]);
    assert!(!cp.engine_state.voice_names.contains_key("b"));
    assert_eq!(voice(&c, &cp, "g.b").group, Some(gid(&cp, "g")));
    // b followed main, so it follows the group now, with its seq
    assert_eq!(mlink(&cp, "g.b"), TempoLink::Group(gid(&cp, "g")));
    assert_eq!(voice(&c, &cp, "g.b").state.tempo, g.state.tempo);
    assert_eq!(seq_tempos(&c, &cp, "g.b"), vec![g.state.tempo]);
    assert_agree(&cp, &c);
}

#[test]
fn add_keeps_own_clock() {
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\" on 93b",
            "group g on 240b { a }",
        ],
    );
    let own = mtempo(&cp, "b");
    run(&mut cp, &mut c, "group g { b }");
    assert_eq!(mlink(&cp, "g.b"), TempoLink::Own);
    assert_eq!(mtempo(&cp, "g.b"), own);
    assert_ne!(own, group(&c, &cp, "g").state.tempo);
    assert_eq!(voice(&c, &cp, "g.b").state.tempo, own);
    assert_agree(&cp, &c);

    // a named clock is kept too
    run_all(
        &mut cp,
        &mut c,
        &[
            "clock clk 90b",
            "voice c from \"c\" on clk",
            "group g { c }",
        ],
    );
    assert_eq!(mlink(&cp, "g.c"), TempoLink::Context);
    assert_eq!(mtempo(&cp, "g.c"), ctx(&cp, "clk"));
    assert_agree(&cp, &c);
}

#[test]
fn add_over_limit_errors_atomically() {
    sample_rate::set(48000);
    let limits = Limits {
        group_members: 2,
        ..Limits::default()
    };
    let (mut cp, mut c) = setup_with(
        limits,
        1,
        vec![track("a", 1, 16), track("b", 2, 16), track("c", 3, 16)],
    );
    boot(&mut cp, &mut c);
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "voice c from \"c\"",
            "group g on 240b { a }",
        ],
    );
    // b fits, c doesn't: neither is added
    assert_eq!(lcode(&mut cp, "group g { b; c }"), Code::LimitReached);
    assert_eq!(cp.engine_state.groups[&gid(&cp, "g")].members.len(), 1);
    assert_eq!(cp.engine_state.voices[&vid(&cp, "b")].group, None);
    assert_eq!(mlink(&cp, "b"), TempoLink::Parent);
    assert_eq!(group(&c, &cp, "g").members.len(), 1);
    assert_agree(&cp, &c);

    // the engine's list never grew past what it was made with
    run(&mut cp, &mut c, "group g { b }");
    assert_eq!(group(&c, &cp, "g").members.len(), 2);
    assert_eq!(lcode(&mut cp, "group g { c }"), Code::LimitReached);
    assert_eq!(group(&c, &cp, "g").members.len(), 2);
    assert!(group(&c, &cp, "g").members.capacity() >= 2);
}

#[test]
fn move_between_groups() {
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "voice c from \"c\"",
            "a { seq s 1 }",
            "group kit on 240b { a; b }",
            "group drums on 90b { c }",
            "group drums { a }",
        ],
    );
    let (kit, drums) = (gid(&cp, "kit"), gid(&cp, "drums"));
    assert_eq!(group(&c, &cp, "kit").members, vec![vid(&cp, "kit.b")]);
    assert_eq!(group(&c, &cp, "drums").members.len(), 2);
    assert_eq!(vid(&cp, "drums.a"), VoiceId(0));
    assert!(cp.engine_state.voice_id("kit.a").is_err());
    // a followed kit, so it follows drums now, with its seq
    assert_eq!(mlink(&cp, "drums.a"), TempoLink::Group(drums));
    let dt = group(&c, &cp, "drums").state.tempo;
    assert_eq!(voice(&c, &cp, "drums.a").state.tempo, dt);
    assert_eq!(seq_tempos(&c, &cp, "drums.a"), vec![dt]);
    assert_eq!(mlink(&cp, "kit.b"), TempoLink::Group(kit));
    assert_agree(&cp, &c);

    // into a new group too
    run(&mut cp, &mut c, "group solo on 60b { b }");
    let solo = group(&c, &cp, "solo");
    assert_eq!(voice(&c, &cp, "solo.b").state.tempo, solo.state.tempo);
    assert!(group(&c, &cp, "kit").members.is_empty());
    assert_agree(&cp, &c);
}

#[test]
fn join_stopped_group_silences() {
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "group g { a }",
            "start b",
        ],
    );
    assert!(render_i16(&mut c, 1, 8).iter().all(|s| *s == 2000));
    run(&mut cp, &mut c, "group g { b }");
    assert!(render_i16(&mut c, 1, 8).iter().all(|s| *s == 0));
    // the group plays: both members do
    run(&mut cp, &mut c, "start g");
    assert!(render_i16(&mut c, 1, 8).iter().all(|s| *s == 3000));
}

#[test]
fn ungroup_member_keeps_clock() {
    // the group follows main: the member follows main again
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "group g { a; b }",
        ],
    );
    run(&mut cp, &mut c, "ungroup g.a");
    assert_eq!(mlink(&cp, "a"), TempoLink::Parent);
    assert_eq!(mtempo(&cp, "a"), ctx(&cp, "main"));
    assert!(voice(&c, &cp, "a").group.is_none());
    assert!(voice(&c, &cp, "a").state.adopts);
    assert_eq!(vid(&cp, "a"), VoiceId(0));
    assert_eq!(group(&c, &cp, "g").members, vec![vid(&cp, "g.b")]);
    assert_agree(&cp, &c);
    // it adopts a group's clock again, as any voice following main does
    run(&mut cp, &mut c, "group h on 120b { a }");
    assert_eq!(
        voice(&c, &cp, "h.a").state.tempo,
        group(&c, &cp, "h").state.tempo
    );
    assert_agree(&cp, &c);

    // the group is on a named clock: the member gets `on clk`
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "clock clk 90b",
            "voice a from \"a\"",
            "group g on clk { a }",
            "ungroup g.a",
        ],
    );
    assert_eq!(mlink(&cp, "a"), TempoLink::Context);
    assert_eq!(mtempo(&cp, "a"), ctx(&cp, "clk"));
    assert_eq!(voice(&c, &cp, "a").state.tempo, ctx(&cp, "clk"));
    assert!(!voice(&c, &cp, "a").state.adopts);
    assert_eq!(cp.state(Some("a")).unwrap(), "voice a from \"a\" on clk");
    assert_agree(&cp, &c);

    // the group has a clock of its own: the member stays on it
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "group g on 240b { a; b }",
            "ungroup g.a",
        ],
    );
    let gt = group(&c, &cp, "g").state.tempo;
    assert_eq!(mtempo(&cp, "a"), gt);
    assert_eq!(voice(&c, &cp, "a").state.tempo, gt);
    assert_eq!(mlink(&cp, "a"), TempoLink::Group(gid(&cp, "g")));
    assert_agree(&cp, &c);
    // when the group goes, that clock is promoted and a is on it
    run(&mut cp, &mut c, "ungroup g");
    assert_eq!(ctx(&cp, "g"), gt);
    assert_eq!(mlink(&cp, "a"), TempoLink::Context);
    assert_eq!(mlink(&cp, "b"), TempoLink::Context);
    assert_eq!(cp.engine_state.tempos[&gt].owner, TempoOwner::Context);
    assert_agree(&cp, &c);
}

#[test]
fn ungroup_keeps_what_you_hear() {
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "group g { a; b }",
            "start g",
        ],
    );
    assert!(render_i16(&mut c, 1, 4).iter().all(|s| *s == 3000));
    run(&mut cp, &mut c, "pause g and ungroup g.a");
    assert!(render_i16(&mut c, 1, 4).iter().all(|s| *s == 0));
    // a was paused by its group; it doesn't start by itself
    assert!(!voice(&c, &cp, "a").state.active);
    run(&mut cp, &mut c, "start a");
    assert!(render_i16(&mut c, 1, 4).iter().all(|s| *s == 1000));

    // a playing group's member keeps playing
    run_all(&mut cp, &mut c, &["resume g", "ungroup g.b"]);
    assert!(render_i16(&mut c, 1, 4).iter().all(|s| *s == 3000));
}

#[test]
fn dissolve_group() {
    // on main: nothing to promote, the name is free
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "group g { a; b }",
        ],
    );
    run(&mut cp, &mut c, "ungroup g");
    assert!(cp.engine_state.groups.is_empty());
    assert_eq!(cp.engine_state.ids.groups.live(), 0);
    assert!(!cp.engine_state.context_names.contains_key("g"));
    assert!(c.groups.get(0).is_none());
    assert_eq!(cp.engine_state.voice_names.len(), 2);
    assert_agree(&cp, &c);
    run(&mut cp, &mut c, "clock g 60b");

    // with a clock of its own that something still reads: promoted
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "voice d from \"c\" on 100b",
            "group g on 240b { a; b }",
            "voice e from \"c\" on g",
        ],
    );
    let gt = group(&c, &cp, "g").state.tempo;
    run(&mut cp, &mut c, "ungroup g");
    assert_eq!(ctx(&cp, "g"), gt);
    assert_eq!(cp.engine_state.tempos[&gt].interval, 240.0);
    for v in ["a", "b", "e"] {
        assert_eq!(mtempo(&cp, v), gt, "{v}");
        assert_eq!(mlink(&cp, v), TempoLink::Context, "{v}");
    }
    assert_agree(&cp, &c);
    assert_eq!(cp.state(Some("e")).unwrap(), "voice e from \"c\" on g");
}

#[test]
fn remove_group_removes_members_and_seqs() {
    let (mut cp, mut c) = edit_rig();
    let mut garbage = rx(&mut c);
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "voice c from \"c\"",
            "a { seq s 1 }",
            "group g on 240b { a; b }",
            "g.a { seq t 2 }",
        ],
    );
    let gt = group(&c, &cp, "g").state.tempo;
    garbage.drain_garbage();
    run(&mut cp, &mut c, "remove g");
    let es = &cp.engine_state;
    assert_eq!(es.voices.len(), 1);
    assert!(es.groups.is_empty());
    assert!(es.group_names.is_empty());
    assert!(es.procs.is_empty());
    assert!(!es.tempos.contains_key(&gt));
    assert_eq!(es.ids.groups.live(), 0);
    assert_eq!(c.voices.iter().count(), 1);
    assert_eq!(c.procs.iter().count(), 0);
    assert_eq!(c.groups.iter().count(), 0);
    assert!(c.tempos.get(gt.index()).is_none());
    assert_agree(&cp, &c);
    // the group, two voices and two seqs went to the garbage queue
    assert!(garbage.drain_garbage() >= 5);
    // the name is free
    run(&mut cp, &mut c, "group g { c }");

    // something outside following a member stops it
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\" on a",
            "group g { a }",
        ],
    );
    assert_eq!(lcode(&mut cp, "remove g"), Code::InUse);
    assert_eq!(cp.engine_state.groups.len(), 1);
}

#[test]
fn remove_all_leaves_main_and_tracks() {
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "clock clk 90b",
            "voice a from \"a\"",
            "voice b from \"b\" on clk",
            "voice c from \"c\" on 93b",
            "a { seq s 1 }",
            "group g on 240b { a }",
            "voice d from \"c\" on g",
            "start all",
        ],
    );
    run(&mut cp, &mut c, "remove all");
    let es = &cp.engine_state;
    assert!(es.voices.is_empty());
    assert!(es.groups.is_empty());
    assert!(es.procs.is_empty());
    assert_eq!(es.context_names.len(), 1);
    assert_eq!(es.tempos.len(), 1);
    assert!(es.context_names.contains_key("main"));
    assert_eq!(es.tracks.len(), 3);
    assert_eq!(c.voices.iter().count(), 0);
    assert_eq!(c.groups.iter().count(), 0);
    assert_eq!(c.tempos.iter().count(), 1);
    assert_agree(&cp, &c);
    // and the session is usable
    run(&mut cp, &mut c, "voice a from \"a\"");
}

#[test]
fn group_gain_scales_members() {
    let (mut cp, mut c) = edit_rig();
    run_all(
        &mut cp,
        &mut c,
        &[
            "voice a from \"a\"",
            "voice b from \"b\"",
            "group g { a }",
            "start g and start b",
        ],
    );
    assert!(render_i16(&mut c, 1, 4).iter().all(|s| *s == 3000));
    // instant: the group's member is halved, the voice outside isn't
    run(&mut cp, &mut c, "g gain=0.5");
    assert!(render_i16(&mut c, 1, 4).iter().all(|s| *s == 500 + 2000));
    assert_eq!(group(&c, &cp, "g").state.gain, 0.5);
    run(&mut cp, &mut c, "g g=1");
    assert!(render_i16(&mut c, 1, 4).iter().all(|s| *s == 3000));

    // a glide is a straight line to the target
    run(&mut cp, &mut c, "g gain=0 over 100f");
    let out = render_i16(&mut c, 1, 120);
    for (k, s) in out.iter().enumerate().take(100) {
        let want = 2000 + (1000.0 * (1.0 - k as f32 / 100.0)) as i16;
        assert!((s - want).abs() <= 1, "frame {k}: {s} vs {want}");
    }
    assert!(out[100..].iter().all(|s| *s == 2000));
    assert_eq!(group(&c, &cp, "g").state.gain, 0.0);

    // a group that isn't playing jumps
    run_all(&mut cp, &mut c, &["stop g", "g gain=0.25 over 100f"]);
    assert_eq!(group(&c, &cp, "g").state.gain, 0.25);
    // and a new group takes `gain=` at creation
    run(
        &mut cp,
        &mut c,
        "voice c from \"c\" and group h gain=2 { c }",
    );
    assert_eq!(group(&c, &cp, "h").state.gain, 2.0);
    assert_agree(&cp, &c);
}

#[test]
fn group_removal_retires_through_garbage() {
    let (mut cp, mut c) = edit_rig();
    let mut garbage = rx(&mut c);
    run_all(&mut cp, &mut c, &["voice a from \"a\"", "group g { a }"]);
    garbage.drain_garbage();
    run(&mut cp, &mut c, "ungroup g");
    // the group's member list arrived as garbage, not freed in place
    assert!(garbage.drain_garbage() >= 1);
    assert_eq!(c.groups.iter().count(), 0);
    assert_eq!(c.leaked, 0);
}

#[test]
fn bus_mix_with_more_channels_doesnt_panic() {
    // the conductor is mono; the caller asks for stereo frames. The bus is
    // sliced by the conductor's own width, so nothing indexes past it.
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![track("a", 8192, 64), track("b", 8192, 64)]);
    c.apply(cp.boot());
    for line in [
        "voice a from \"a\" on 1000ms",
        "voice b from \"b\" on 1000ms",
        "group g on 1000ms { a; b }",
        "start g",
    ] {
        run(&mut cp, &mut c, line);
    }
    let mut out = vec![0.0f32; 2 * 16];
    c.render(&mut out, 2);
    // the members mixed into the group's bus, which reached the first channel
    assert!(out.chunks(2).all(|f| f[0] > 0.0));
}
