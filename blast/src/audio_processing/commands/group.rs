//! Groups of Voices.

use super::errors::{CmdErr, CmdResult};
use super::tempo::{TempoRepr, TempoSpec, parse_tempo_spec, rejected};
use super::voice::VoiceRepr;
use super::{CmdProcessor, Command, Parsed};
use crate::audio_processing::blast_time::blast_time::{TempoMode, TempoUnit};
use std::collections::HashMap;

/// One row per command: (name, usage and summary).
pub const HELP: &[(&str, &str)] = &[(
    "group",
    "group <name> -v <v1,v2,…> [-t s|m|b:<n> | c:<context>]: move voices into a new group",
)];

/// A Group in the mirror.
pub struct GroupRepr {
    pub(super) idx: usize,
    pub(super) tempo: TempoRepr,
    pub(super) voices: HashMap<String, VoiceRepr>,
}

impl GroupRepr {
    pub(super) fn new(idx: usize, tempo: TempoRepr, voices: HashMap<String, VoiceRepr>) -> Self {
        Self { idx, tempo, voices }
    }
}

/// Arguments for `Command::Group`.
pub struct GroupArgs {
    pub tempo: TempoRepr,
    /// In Group order (entry i becomes the Group's Voice i, matching the
    /// VoiceRepr idx in the GroupRepr): the Voice's engine idx, whether its
    /// TempoState refers to the Group's, and the ids of the Voice's
    /// Processes whose TempoStates refer to the Group's.
    pub vs_fs_ps: Vec<(usize, bool, Vec<usize>)>,
}

impl CmdProcessor {
    // reads only: every check happens here, so a failure can't touch the
    // mirror; returns the Group's tempo and its Voices in command-line order
    fn parse_group_flags<'a>(
        &self,
        mut args: impl Iterator<Item = &'a str>,
    ) -> CmdResult<(TempoRepr, Vec<String>)> {
        let mut tempo = TempoRepr::new(0);
        tempo.init(TempoMode::Group, TempoUnit::Bpm, 240.0);
        let mut members = Vec::<String>::new();

        while let Some(arg) = args.next() {
            match arg {
                "-t" | "--tempo" => {
                    let spec = args
                        .next()
                        .ok_or_else(|| CmdErr::missing("arguments", "group -t"))?;
                    // a Group can't follow another Group or a Voice
                    if spec.split(':').next() == Some("g") {
                        return Err(rejected(spec, "group"));
                    }
                    tempo = match parse_tempo_spec(spec, "group")? {
                        TempoSpec::Context(tc) => TempoRepr::shared(self.engine_state.context(tc)?),
                        TempoSpec::New { unit, interval } => {
                            let mut new_tempo = TempoRepr::new(0);
                            new_tempo.init(TempoMode::Group, unit, interval);
                            new_tempo
                        }
                        TempoSpec::Group(_) | TempoSpec::Voice => {
                            return Err(rejected(spec, "group"));
                        }
                    };
                }
                "-v" | "--voices" => {
                    let list = args
                        .next()
                        .ok_or_else(|| CmdErr::missing("arguments", "group -v"))?;
                    for name in list.split(',') {
                        if !self.engine_state.voices.contains_key(name) {
                            return Err(CmdErr::NoVoice {
                                name: name.to_string(),
                                group: None,
                            });
                        }
                        if members.iter().any(|m| m == name) {
                            return Err(CmdErr::Formatting {
                                err: format!("Voice '{name}' listed more than once"),
                            });
                        }
                        members.push(name.to_string());
                    }
                }
                _ => return Err(CmdErr::invalid(arg, "group")),
            }
        }

        Ok((tempo, members))
    }

    pub(super) fn try_group(&mut self, args: &str) -> CmdResult<Parsed> {
        let mut args = args.split_whitespace();
        let name = args
            .next()
            .ok_or_else(|| CmdErr::missing("name", "group"))?;

        // a second Group by the same name would replace the first in the
        // mirror but not in the engine, shifting every later idx
        if self.engine_state.groups.contains_key(name) {
            return Err(CmdErr::AlreadyIs {
                ty: "Group".to_string(),
                name: name.to_string(),
            });
        }

        let (tempo, members) = self.parse_group_flags(args)?;

        // all args are valid; move the Voices out of the top level, in
        // command-line (Group) order so the flags and Process ids below line
        // up with v_ids
        let mut voices = Vec::<(String, VoiceRepr)>::new();
        // the Voices' engine indices, saved before they change
        let mut v_ids = Vec::<usize>::new();

        for member in members {
            let Some(mut voice) = self.engine_state.voices.remove(&member) else {
                // parse_group_flags checked every member
                continue;
            };
            v_ids.push(voice.idx);
            voice.idx = voices.len(); // its index in the Group
            voices.push((member, voice));
        }

        // remove from the highest index down, so the remaining
        // voice.idx are decremented correctly
        let mut sorted = v_ids.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        for removed_id in sorted {
            for v in self.engine_state.voices.values_mut() {
                if v.idx > removed_id {
                    v.idx -= 1;
                }
            }
        }

        // per Voice: whether its TempoState will refer to the Group's, and
        // the Processes whose TempoStates move to the Group's
        let mut v_flags: Vec<bool> = Vec::new();
        let mut p_ids: Vec<Vec<usize>> = Vec::new();

        let g_idx = self.engine_state.groups.len();

        for (_, voice) in &mut voices {
            let mut p_i: Vec<usize> = Vec::new();

            // a Voice without a TempoState of its own takes the Group's
            // (which is how a Voice's Process syncs with a Group, by proxy,
            // if the Process follows its Voice)
            if voice.tempo.mode == TempoMode::TBD {
                voice.tempo = TempoRepr::following(&tempo, g_idx);
                v_flags.push(true);
                for process in voice.processes.values_mut() {
                    // only re-point Processes that follow the Voice (TBD, in
                    // anticipation of a Group); ones with their own (or a
                    // shared) TempoState keep it
                    if let Some(t) = &process.tempo
                        && t.mode == TempoMode::TBD
                    {
                        process.tempo = Some(TempoRepr::following(&tempo, g_idx));
                        p_i.push(process.idx);
                    }
                }
            } else {
                v_flags.push(false);
            }

            p_ids.push(p_i);
        }

        let group = GroupRepr::new(g_idx, tempo.clone(), voices.into_iter().collect());
        self.engine_state.groups.insert(name.to_string(), group);

        // keep Group order; the engine removes Voices from Conductor.voices
        // in reverse itself
        let vs_fs_ps = v_ids
            .into_iter()
            .zip(v_flags)
            .zip(p_ids)
            .map(|((a, b), c)| (a, b, c))
            .collect();

        Ok(Parsed::Engine(Command::Group(GroupArgs {
            tempo,
            vs_fs_ps,
        })))
    }
}

#[cfg(test)]
mod tests {
    use crate::audio_processing::blast_time::blast_time::{TempoMode, TempoUnit};
    use crate::audio_processing::commands::testing::*;
    use crate::audio_processing::commands::*;

    const PERMS: [&str; 6] = ["a,b,c", "a,c,b", "b,a,c", "b,c,a", "c,a,b", "c,b,a"];

    #[test]
    fn group_voice_order_matches_command_line() {
        let mut cp = processor(&["a", "b", "c"]);
        ok(&mut cp, "load a");
        ok(&mut cp, "load b");
        ok(&mut cp, "load c");
        let args = group_args(ok(&mut cp, "group g -v c,a"));

        // in-group order is c, a
        assert_eq!(gv_idx(&cp, "g", "c"), 0);
        assert_eq!(gv_idx(&cp, "g", "a"), 1);
        // GroupArgs entries are in that same order, holding original idx
        let ids: Vec<usize> = args.vs_fs_ps.iter().map(|(i, _, _)| *i).collect();
        assert_eq!(ids, vec![2, 0]);
        // remaining top-level voice shifted down
        assert_eq!(v_idx(&cp, "b"), 0);
        assert!(!cp.engine_state.voices.contains_key("a"));
        assert_eq!(cp.engine_state.groups.get("g").unwrap().idx, 0);
    }

    #[test]
    fn group_default_tempo_is_owned() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "load a");
        let args = group_args(ok(&mut cp, "group g -v a"));
        assert!(args.tempo.owned);
        assert_eq!(args.tempo.mode, TempoMode::Group);
        assert_eq!(args.tempo.unit, TempoUnit::Bpm);
        assert_eq!(args.tempo.interval, 240.0);

        let mut cp = processor(&["a"]);
        ok(&mut cp, "load a");
        let args = group_args(ok(&mut cp, "group g -t m:250 -v a"));
        assert!(args.tempo.owned);
        assert_eq!(args.tempo.mode, TempoMode::Group);
        assert_eq!(args.tempo.unit, TempoUnit::Millis);
        assert_eq!(args.tempo.interval, 250.0);
    }

    #[test]
    fn group_shares_named_context() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "tc other b:60");
        ok(&mut cp, "tc clk b:120");
        ok(&mut cp, "load a");
        let args = group_args(ok(&mut cp, "group g -t c:clk -v a"));
        assert!(!args.tempo.owned);
        assert_eq!(args.tempo.mode, TempoMode::Context);
        assert_eq!(args.tempo.idx, 1);
        // the TBD voice follows the group, i.e. the context
        let (_, follows, _) = args.vs_fs_ps[0];
        assert!(follows);
        let a = cp
            .engine_state
            .groups
            .get("g")
            .unwrap()
            .voices
            .get("a")
            .unwrap();
        assert_eq!(a.tempo.mode, TempoMode::Context);
        assert_eq!(a.tempo.idx, 1);
    }

    #[test]
    fn group_tempo_errors() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "tc clk b:120");
        ok(&mut cp, "load a");
        assert!(matches!(
            err(&mut cp, "group g -t c:nope -v a"),
            CmdErr::NoItem { .. }
        ));
        assert!(matches!(
            err(&mut cp, "group g -t c"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "group g -t x:1"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "group g -t b"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "group g -t b:fast"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "group g -t"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(err(&mut cp, "group"), CmdErr::MissingArg { .. }));
        assert!(matches!(
            err(&mut cp, "group g -x"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(cp.engine_state.groups.is_empty());
    }

    #[test]
    fn group_flags_follow_their_voice() {
        // HashMap order varies per map, so repeat with fresh processors
        for _ in 0..20 {
            for perm in PERMS {
                let mut cp = processor(&["a", "b", "c"]);
                ok(&mut cp, "load a");
                ok(&mut cp, "load b -t b:90");
                ok(&mut cp, "load c");
                ok(&mut cp, "seq a -t v -s 0");
                ok(&mut cp, "seq c -t v -s 0");
                let args = group_args(ok(&mut cp, &format!("group g -v {}", perm)));

                for (pos, (orig, follows, p_ids)) in args.vs_fs_ps.iter().enumerate() {
                    // a=0, b=1, c=2
                    let name = ["a", "b", "c"][*orig];
                    assert_eq!(*follows, name != "b", "{} in {}", name, perm);
                    match name {
                        "b" => assert!(p_ids.is_empty(), "{} in {}", name, perm),
                        _ => assert_eq!(p_ids, &vec![0], "{} in {}", name, perm),
                    }
                    assert_eq!(gv_idx(&cp, "g", name), pos);
                }

                let g = cp.engine_state.groups.get("g").unwrap();
                assert_eq!(g.voices.get("a").unwrap().tempo.mode, TempoMode::Group);
                assert_eq!(g.voices.get("b").unwrap().tempo.mode, TempoMode::Voice);
                assert_eq!(g.voices.get("c").unwrap().tempo.mode, TempoMode::Group);
            }
        }
    }

    #[test]
    fn group_keeps_owned_process_tempos() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "load a");
        ok(&mut cp, "seq a -t b:140 -s 0");
        let args = group_args(ok(&mut cp, "group g -v a"));
        let (_, follows, p_ids) = &args.vs_fs_ps[0];
        assert!(*follows);
        assert!(p_ids.is_empty());
        let a = cp
            .engine_state
            .groups
            .get("g")
            .unwrap()
            .voices
            .get("a")
            .unwrap();
        let t = a.processes.get("seq").unwrap().tempo.as_ref().unwrap();
        assert_eq!(t.mode, TempoMode::Process);
    }

    #[test]
    fn group_moves_voice_following_processes() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "load a");
        ok(&mut cp, "seq a -t v -s 0");
        let args = group_args(ok(&mut cp, "group g -v a"));
        let (_, _, p_ids) = &args.vs_fs_ps[0];
        assert_eq!(p_ids, &vec![0]);
        let a = cp
            .engine_state
            .groups
            .get("g")
            .unwrap()
            .voices
            .get("a")
            .unwrap();
        let t = a.processes.get("seq").unwrap().tempo.as_ref().unwrap();
        assert_eq!(t.mode, TempoMode::Group);
        assert!(!t.owned);
        assert_eq!(t.idx, 0);
    }

    #[test]
    fn failed_group_leaves_mirror_unchanged() {
        let names = ["a", "b", "c"];
        for cmd in [
            "group g -v a,nope",
            "group g -v nope,b",
            "group g -v a -t x:1",
            "group g -v a -t c:nope",
            "group g -v c -v nope",
            "group g -v a,a",
            "group g -v a -v a",
            "group g -v b -x",
            "group g -v",
        ] {
            let mut cp = loaded(&names);
            err(&mut cp, cmd);
            assert_untouched(&cp, &names);
        }
    }

    #[test]
    fn group_unknown_voice_is_no_voice() {
        let mut cp = loaded(&["a"]);
        match err(&mut cp, "group g -v a,nope") {
            CmdErr::NoVoice { name, group } => {
                assert_eq!(name, "nope");
                assert_eq!(group, None);
            }
            other => panic!("wrong error {:?}", other),
        }
    }

    #[test]
    fn group_with_several_voice_flags() {
        let mut cp = loaded(&["a", "b", "c", "d"]);
        let args = group_args(ok(&mut cp, "group g -v c -v a"));
        let ids: Vec<usize> = args.vs_fs_ps.iter().map(|(i, _, _)| *i).collect();
        assert_eq!(ids, vec![2, 0]);
        assert_eq!(gv_idx(&cp, "g", "c"), 0);
        assert_eq!(gv_idx(&cp, "g", "a"), 1);
        assert_eq!(v_idx(&cp, "b"), 0);
        assert_eq!(v_idx(&cp, "d"), 1);
    }

    #[test]
    fn duplicate_group_is_rejected() {
        let mut cp = loaded(&["a", "b", "c"]);
        ok(&mut cp, "group g -v a");
        assert!(matches!(
            err(&mut cp, "group g -v b"),
            CmdErr::AlreadyIs { .. }
        ));
        // b wasn't consumed
        assert_eq!(v_idx(&cp, "b"), 0);
        assert_eq!(v_idx(&cp, "c"), 1);
        assert_eq!(cp.engine_state.groups.len(), 1);
        assert!(
            cp.engine_state
                .groups
                .get("g")
                .unwrap()
                .voices
                .contains_key("a")
        );

        ok(&mut cp, "group h -v b");
        assert_eq!(cp.engine_state.groups.get("h").unwrap().idx, 1);
    }
}
