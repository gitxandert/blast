//! Groups of Voices.

use super::errors::{CmdErr, CmdResult};
use super::reprs::{
    EngineState, GroupId, ProcId, ProcOwner, TempoId, TempoLink, TempoOwner, VoiceId, alloc,
};
use super::tempo::{
    ClockRun, TempoChoice, TempoTarget, TempoUse, TempoUser, tempo_users, voice_followers,
};
use super::{AnyId, BatchArgs, CmdProcessor, Command, TempoAssignArgs, TempoFreeArgs};
use crate::audio_processing::engine::BufSource;
use std::collections::{BTreeMap, HashMap};

/// A Group in the mirror.
#[derive(Clone)]
pub struct GroupRepr {
    #[allow(dead_code)] // the map key; kept so a repr names itself
    pub(crate) id: GroupId,
    pub(crate) name: String,
    pub(crate) tempo: TempoId,
    #[allow(dead_code)] // for TMP's tempo listing (wave 3)
    pub(crate) link: TempoLink,
    /// Its period in ticks of its clock (`in N`, G5).
    pub(crate) period: u32,
    /// What its members are scaled by (`gain=`).
    pub(crate) gain: f32,
    /// In the order the block lists them.
    pub(crate) members: Vec<VoiceId>,
    pub(crate) member_names: HashMap<String, VoiceId>,
    /// Its seqs by name (`group.name`).
    pub(crate) procs: BTreeMap<String, ProcId>,
    /// The engine's `GroupState.active`; kept for CLKG's clock run states.
    pub(crate) active: bool,
    /// Effects on its mix, in chain order (FXb).
    pub(crate) fx: Vec<super::FxRepr>,
    /// Room the engine's chain has (see `VoiceRepr::fx_cap`).
    pub(crate) fx_cap: usize,
}

/// Arguments for `Command::Group`.
pub struct GroupArgs {
    pub group: GroupId,
    pub tempo: TempoUse,
    /// Allocated with capacity `Limits::group_members` on the REPL
    /// thread; the engine keeps this Vec as the group's member list.
    pub members: Vec<VoiceId>,
}

/// What a `Command::GroupEdit` does to a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupOp {
    /// A top-level voice becomes a member.
    Join(VoiceId),
    /// A member returns to the top level.
    Leave(VoiceId),
    /// The group itself goes, after its members left or were removed.
    Remove,
}

/// Arguments for `Command::GroupEdit`.
pub struct GroupEditArgs {
    pub group: GroupId,
    pub op: GroupOp,
}

/// Arguments for `Command::GroupGain`: the members are scaled by `gain`,
/// at once or gliding over `frames` (REQ-074).
pub struct GroupGainArgs {
    pub group: GroupId,
    pub gain: f32,
    pub frames: u32,
}

fn edit(group: GroupId, op: GroupOp) -> Command {
    Command::GroupEdit(GroupEditArgs { group, op })
}

fn no_cmds() -> Command {
    Command::Batch(BatchArgs { cmds: Vec::new() })
}

/// Removes `group` with its members from the mirror, and answers the commands
/// that do it in the engine.
pub(crate) fn remove_group_in(es: &mut EngineState, group: GroupId) -> Command {
    let members = es
        .groups
        .get(&group)
        .map(|g| g.members.clone())
        .unwrap_or_default();
    let mut cmds: Vec<Command> = Vec::new();
    for m in members {
        if let Ok(cmd) = es.unload_voice(m) {
            cmds.push(cmd);
        }
    }
    cmds.push(drop_group_in(es, group));
    Command::Batch(BatchArgs { cmds })
}

// Takes an empty group out of the mirror. Its clock is promoted to a
// clock named after the group if anything still reads it (what was on
// the group goes on the clock), or freed.
fn drop_group_in(es: &mut EngineState, group: GroupId) -> Command {
    let mut cmds = vec![edit(group, GroupOp::Remove)];
    let Some(repr) = es.groups.remove(&group) else {
        return Command::Batch(BatchArgs { cmds });
    };
    if es.group_names.get(&repr.name) == Some(&group) {
        es.group_names.remove(&repr.name);
    }
    es.limbo.retain(|l| l.id != AnyId::Group(group));
    es.ids.groups.free(group);

    // its seqs go with it (the engine retires them in GroupOp::Remove)
    for p in repr.procs.values() {
        es.procs.remove(p);
        es.ids.procs.free(*p);
    }

    // its packets go with its seqs, and a pipe that named the group (or
    // a member it removed) stops naming it
    es.drop_packets_of(ProcOwner::Group(group));
    cmds.extend(es.strip_receivers(ProcOwner::Group(group), &repr.name));
    // and a buffer that recorded the group records the mix instead
    cmds.extend(es.orphan_buffers(BufSource::Group(group), &repr.name));

    // the tempos the group owned: its clock, and any of its seqs'
    let owned: Vec<TempoId> = es
        .tempos
        .values()
        .filter(|t| t.owner == TempoOwner::Group(group))
        .map(|t| t.id)
        .collect();
    for t in owned {
        if t == repr.tempo && !tempo_users(es, t).is_empty() {
            if let Some(r) = es.tempos.get_mut(&t) {
                r.owner = TempoOwner::Context;
            }
            es.context_names.insert(repr.name.clone(), t);
        } else {
            es.tempos.remove(&t);
            es.ids.tempos.free(t);
            cmds.push(Command::TempoFree(TempoFreeArgs { id: t }));
        }
    }

    let linked = TempoLink::Group(group);
    for v in es.voices.values_mut().filter(|v| v.link == linked) {
        v.link = TempoLink::Context;
    }
    for p in es.procs.values_mut().filter(|p| p.link == linked) {
        p.link = TempoLink::Context;
    }
    for g in es.groups.values_mut().filter(|g| g.link == linked) {
        g.link = TempoLink::Context;
    }
    Command::Batch(BatchArgs { cmds })
}

/// Dissolves `group` in the mirror: every member leaves, then the group goes.
pub(crate) fn dissolve_group_in(es: &mut EngineState, group: GroupId) -> Command {
    let members = es
        .groups
        .get(&group)
        .map(|g| g.members.clone())
        .unwrap_or_default();
    let mut cmds: Vec<Command> = members
        .into_iter()
        .map(|m| leave_group_in(es, m, false))
        .collect();
    cmds.push(drop_group_in(es, group));
    Command::Batch(BatchArgs { cmds })
}

fn leave_group_in(es: &mut EngineState, voice: VoiceId, moving: bool) -> Command {
    let Some(group) = es.voices.get(&voice).and_then(|v| v.group) else {
        return no_cmds();
    };
    let group_link = es.groups.get(&group).map_or(TempoLink::Own, |g| g.link);
    let Some(v) = es.voices.get_mut(&voice) else {
        return no_cmds();
    };

    if v.link == TempoLink::Group(group) && moving {
        v.link = TempoLink::Parent;
    } else if v.link == TempoLink::Group(group) {
        v.link = match group_link {
            TempoLink::Parent => TempoLink::Parent,
            TempoLink::Context => TempoLink::Context,
            follow @ (TempoLink::Follow(_) | TempoLink::Group(_)) => follow,
            // its own clock, which stays the group's
            TempoLink::Own | TempoLink::Voice => TempoLink::Group(group),
        };
    }
    let (name, tempo, link) = (v.name.clone(), v.tempo, v.link);
    v.group = None;
    // what you hear stays: a member of a paused group was silent, and is
    // silent on its own too (the engine's `Leave`)
    v.active &= es.groups.get(&group).is_some_and(|g| g.active);

    if let Some(g) = es.groups.get_mut(&group) {
        g.members.retain(|m| *m != voice);
        g.member_names.remove(&name);
    }
    es.voice_names.insert(name, voice);

    let mut cmds = vec![edit(group, GroupOp::Leave(voice))];
    // the engine makes the voice follow its parent again; any other link
    // says so with an assignment to the clock it already has
    if !link.follows_parent() {
        cmds.push(Command::TempoAssign(TempoAssignArgs {
            target: TempoTarget::Voice(voice),
            tempo: TempoUse::Shared(tempo),
        }));
    }
    Command::Batch(BatchArgs { cmds })
}

impl CmdProcessor {
    /// Sets a group's gain in the mirror (the glide's target, for
    /// `frames > 0`) and returns the command.
    pub(crate) fn set_group_gain(&mut self, group: GroupId, gain: f32, frames: u32) -> Command {
        if let Some(repr) = self.engine_state.groups.get_mut(&group) {
            repr.gain = gain;
        }
        Command::GroupGain(GroupGainArgs {
            group,
            gain,
            frames,
        })
    }

    /// Makes top-level `voice` a member of `group`, last in its order. A
    /// voice that follows its parent moves to the group's clock (TMP's move,
    /// with its seqs); any other clock is kept. Nothing changes if the group
    /// is full.
    pub(crate) fn join_group(&mut self, voice: VoiceId, group: GroupId) -> CmdResult<Command> {
        let limit = self.engine_state.limits.group_members;
        let (Some(g), Some(v)) = (
            self.engine_state.groups.get(&group),
            self.engine_state.voices.get(&voice),
        ) else {
            return Ok(no_cmds());
        };
        if v.group.is_some() {
            return Err(CmdErr::Formatting {
                err: format!("Voice '{}' is already in a group", v.name),
            });
        }
        if g.members.len() >= limit as usize {
            return Err(CmdErr::LimitReached {
                kind: "group members",
                limit,
            });
        }
        let (group_tempo, follows_parent, name) =
            (g.tempo, v.link.follows_parent(), v.name.clone());

        let mut cmds: Vec<Command> = Vec::new();
        if follows_parent {
            cmds.push(self.move_to_clock(
                TempoTarget::Voice(voice),
                TempoChoice::Shared(group_tempo),
                TempoLink::Group(group),
            )?);
        }

        let es = &mut self.engine_state;
        es.voice_names.remove(&name);
        if let Some(g) = es.groups.get_mut(&group) {
            g.members.push(voice);
            g.member_names.insert(name, voice);
        }
        if let Some(v) = es.voices.get_mut(&voice) {
            v.group = Some(group);
        }
        cmds.push(edit(group, GroupOp::Join(voice)));
        Ok(Command::Batch(BatchArgs { cmds }))
    }

    /// Takes `voice` out of its group, back to the top level, on the clock it
    /// hears now. A member that followed the group follows what the group
    /// follows: `main` again if the group did, else the clock the group is
    /// on. If the group has a clock of its own, the voice stays on it
    /// (linked to the group; `drop_group` promotes it when the group goes).
    /// With `moving` (on its way into another group), a member that followed
    /// its group follows its parent again whatever the group was on, so the
    /// next join or `group` takes it to the new group's clock.
    pub(crate) fn leave_group(&mut self, voice: VoiceId, moving: bool) -> Command {
        leave_group_in(&mut self.engine_state, voice, moving)
    }

    /// Dissolves `group`: every member leaves (`leave_group`), then the
    /// group goes.
    pub(crate) fn dissolve_group(&mut self, group: GroupId) -> Command {
        let es = &mut self.engine_state;
        // a line that waits only marks the group (PKb3)
        if es.staging {
            es.staged_removal(AnyId::Group(group), true, |es| {
                Ok(dissolve_group_in(es, group))
            })
            .unwrap_or_else(|_| no_cmds())
        } else {
            dissolve_group_in(es, group)
        }
    }

    /// Removes `group` with its members and their seqs. A member that
    /// something outside the group follows (`on member`) stops it: the
    /// followers are returned for the error.
    pub(crate) fn remove_group(&mut self, group: GroupId) -> Result<Command, Vec<TempoUser>> {
        let members = self
            .engine_state
            .groups
            .get(&group)
            .map(|g| g.members.clone())
            .unwrap_or_default();
        let es = &self.engine_state;
        let inside = |u: &TempoUser| match *u {
            TempoUser::Voice(v) => members.contains(&v),
            TempoUser::Proc(p) => es
                .procs
                .get(&p)
                .is_some_and(|r| matches!(r.owner, ProcOwner::Voice(v) if members.contains(&v))),
            TempoUser::Group(_) => false,
        };
        let followers: Vec<TempoUser> = members
            .iter()
            .flat_map(|&m| voice_followers(es, m))
            .filter(|u| !inside(u))
            .collect();
        if !followers.is_empty() {
            return Err(followers);
        }
        Ok(self.remove_group_unchecked(group))
    }

    /// `remove_group` for a caller that removes whatever follows the members
    /// too (`remove all`).
    pub(crate) fn remove_group_unchecked(&mut self, group: GroupId) -> Command {
        let es = &mut self.engine_state;
        // a line that waits only marks the group: it and its members stay
        // until the line runs (PKb3)
        if es.staging {
            es.staged_removal(AnyId::Group(group), false, |es| {
                Ok(remove_group_in(es, group))
            })
            .unwrap_or_else(|_| no_cmds())
        } else {
            remove_group_in(es, group)
        }
    }

    /// Creates a group called `name` holding `members` (the language's
    /// `group`). Members that follow their parent (`TempoLink::follows_parent`)
    /// adopt the group's tempo, with the processes that follow them; a tempo
    /// the voice owned is freed. Nothing changes if it fails.
    pub(crate) fn make_group(
        &mut self,
        name: &str,
        choice: TempoChoice,
        link: TempoLink,
        members: &[VoiceId],
        period: u32,
    ) -> CmdResult<Command> {
        let limit = self.engine_state.limits.group_members;
        if members.len() > limit as usize {
            return Err(CmdErr::LimitReached {
                kind: "group members",
                limit,
            });
        }
        for m in members {
            match self.engine_state.voices.get(m) {
                Some(v) if v.group.is_none() => {}
                Some(v) => {
                    return Err(CmdErr::Formatting {
                        err: format!("Voice '{}' is already in a group", v.name),
                    });
                }
                None => {
                    return Err(CmdErr::Formatting {
                        err: format!("No voice with ID {}", m.0),
                    });
                }
            }
        }

        // everything is valid; allocate, undoing on failure
        let group = alloc(&mut self.engine_state.ids.groups, "groups")?;
        let tempo = match self.alloc_tempo(choice) {
            Ok(tempo) => tempo,
            Err(e) => {
                self.engine_state.ids.groups.free(group);
                return Err(e);
            }
        };
        self.insert_tempo(tempo, TempoOwner::Group(group), ClockRun::Stopped);
        let group_tempo = tempo.id();

        // the engine keeps this Vec, so it never grows on the audio thread
        let mut members_vec = Vec::with_capacity(limit as usize);
        members_vec.extend_from_slice(members);
        let members = members_vec;

        let es = &mut self.engine_state;
        let mut member_names = HashMap::with_capacity(members.len());
        for &m in &members {
            let Some(voice) = es.voices.get_mut(&m) else {
                continue; // checked above
            };
            es.voice_names.remove(&voice.name);
            member_names.insert(voice.name.clone(), m);
            voice.group = Some(group);

            // a voice that follows its parent adopts the group's tempo, and
            // so do its processes that read the voice's old one (a seq
            // without `on`, or with `on main` while the voice follows main)
            if voice.link.follows_parent() {
                let old = voice.tempo;
                // a voice that shared its parent's clock doesn't own it
                if es
                    .tempos
                    .get(&old)
                    .is_some_and(|t| t.owner == TempoOwner::Voice(m))
                {
                    es.tempos.remove(&old);
                    es.ids.tempos.free(old);
                }
                voice.tempo = group_tempo;
                voice.link = TempoLink::Group(group);
                for proc in voice.procs.values() {
                    if let Some(p) = es.procs.get_mut(proc)
                        && p.tempo == old
                    {
                        p.tempo = group_tempo;
                    }
                }
            }
        }

        es.groups.insert(
            group,
            super::GroupRepr {
                id: group,
                name: name.to_string(),
                tempo: group_tempo,
                link,
                period,
                gain: 1.0,
                members: members.clone(),
                member_names,
                procs: BTreeMap::new(),
                active: false,
                fx: Vec::new(),
                fx_cap: 0,
            },
        );
        es.group_names.insert(name.to_string(), group);

        Ok(Command::Group(GroupArgs {
            group,
            tempo,
            members,
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::audio_processing::blast_time::blast_time::TempoUnit;
    use crate::audio_processing::commands::lang::Code;
    use crate::audio_processing::commands::testing::*;
    use crate::audio_processing::commands::*;

    const PERMS: [&str; 6] = [
        "a; b; c", "a; c; b", "b; a; c", "b; c; a", "c; a; b", "c; b; a",
    ];

    fn members(cp: &CmdProcessor, g: &str) -> Vec<VoiceId> {
        cp.engine_state.groups[&gid(cp, g)].members.clone()
    }

    fn link(cp: &CmdProcessor, v: &str) -> TempoLink {
        cp.engine_state.voices[&vid(cp, v)].link
    }

    fn tempo_of(cp: &CmdProcessor, v: &str) -> TempoId {
        cp.engine_state.voices[&vid(cp, v)].tempo
    }

    fn proc_tempo(cp: &CmdProcessor, v: &str, p: &str) -> TempoId {
        let es = &cp.engine_state;
        es.procs[&es.voices[&vid(cp, v)].procs[p]].tempo
    }

    #[test]
    fn group_voice_order_matches_block() {
        let mut cp = loaded(&["a", "b", "c"]);
        let args = group_args(ok(&mut cp, "group g on 240b { c; a }"));

        // in-group order is c, a, and nobody's ID moved
        assert_eq!(args.members, vec![VoiceId(2), VoiceId(0)]);
        assert_eq!(members(&cp, "g"), args.members);
        assert_eq!(vid(&cp, "g.c"), VoiceId(2));
        assert_eq!(vid(&cp, "g.a"), VoiceId(0));
        assert_eq!(vid(&cp, "b"), VoiceId(1));
        assert!(!cp.engine_state.voice_names.contains_key("a"));
        assert_eq!(gid(&cp, "g"), GroupId(0));
        assert_eq!(args.group, GroupId(0));
        assert!(args.members.capacity() >= cp.engine_state.limits.group_members as usize);
    }

    #[test]
    fn group_own_tempo_is_owned() {
        let mut cp = loaded(&["a"]);
        let args = group_args(ok(&mut cp, "group g on 240b { a }"));
        let TempoUse::Own(t) = args.tempo else {
            panic!("not an own tempo")
        };
        assert_eq!(t.unit, TempoUnit::Bpm);
        assert_eq!(t.interval, 240.0);
        assert_eq!(
            cp.engine_state.tempos[&t.id].owner,
            TempoOwner::Group(args.group)
        );

        let mut cp = loaded(&["a"]);
        let args = group_args(ok(&mut cp, "group g on 250ms { a }"));
        let TempoUse::Own(t) = args.tempo else {
            panic!("not an own tempo")
        };
        assert_eq!(t.unit, TempoUnit::Millis);
        assert_eq!(t.interval, 250.0);
    }

    #[test]
    fn group_shares_named_context() {
        let mut cp = booted(&["a"]);
        ok(&mut cp, "clock other 60b");
        ok(&mut cp, "clock clk 120b");
        ok(&mut cp, "voice a from \"a\"");
        let args = group_args(ok(&mut cp, "group g on clk { a }"));
        assert_eq!(args.tempo, TempoUse::Shared(ctx(&cp, "clk")));
        // main is clock 0
        assert_eq!(ctx(&cp, "clk"), TempoId(2));
        // the voice that followed main follows the group, i.e. the context
        assert_eq!(link(&cp, "g.a"), TempoLink::Group(args.group));
        assert_eq!(tempo_of(&cp, "g.a"), TempoId(2));
    }

    #[test]
    fn group_without_on_follows_main() {
        let mut cp = booted(&["a"]);
        ok(&mut cp, "voice a from \"a\"");
        let args = group_args(ok(&mut cp, "group g { a }"));
        assert_eq!(args.tempo, TempoUse::Shared(TempoId(0)));
        assert_eq!(cp.engine_state.groups[&args.group].link, TempoLink::Parent);
        assert_eq!(cp.engine_state.groups[&args.group].period, 4);
        assert_eq!(tempo_of(&cp, "g.a"), TempoId(0));
    }

    #[test]
    fn group_tempo_errors() {
        let mut cp = loaded(&["a"]);
        ok(&mut cp, "clock clk 120b");
        let code = |cp: &mut CmdProcessor, line: &str| lerr(cp, line)[0].code;
        assert_eq!(code(&mut cp, "group g on nope { a }"), Code::UnknownName);
        assert_eq!(code(&mut cp, "group g on x1 { a }"), Code::UnknownName);
        assert_eq!(code(&mut cp, "group g on 0b { a }"), Code::OutOfRange);
        // a group can't be put on a seq's clock
        ok(&mut cp, "a { seq s 1 }");
        assert_eq!(code(&mut cp, "group g on a.s { a }"), Code::WrongType);
        assert!(cp.engine_state.groups.is_empty());
    }

    #[test]
    fn group_follows_its_voices() {
        for perm in PERMS {
            let mut cp = booted(&["a", "b", "c"]);
            ok(&mut cp, "voice a from \"a\"");
            ok(&mut cp, "voice b from \"b\" on 90b");
            ok(&mut cp, "voice c from \"c\"");
            ok(&mut cp, "a { seq s 1 }");
            ok(&mut cp, "c { seq s 1 }");
            // a and c follow main
            let main = TempoId(0);
            assert_eq!(tempo_of(&cp, "a"), main);
            assert_eq!(tempo_of(&cp, "c"), main);
            let args = group_args(ok(&mut cp, &format!("group g on 240b {{ {perm} }}")));
            let g = args.group;
            let gt = args.tempo.id();

            let order: Vec<VoiceId> = perm
                .split("; ")
                .map(|n| vid(&cp, &format!("g.{n}")))
                .collect();
            assert_eq!(args.members, order, "{perm}");

            // a and c adopted the group's tempo, with their seqs; b kept its own
            assert_eq!(link(&cp, "g.a"), TempoLink::Group(g), "{perm}");
            assert_eq!(link(&cp, "g.c"), TempoLink::Group(g), "{perm}");
            assert_eq!(link(&cp, "g.b"), TempoLink::Own, "{perm}");
            assert_eq!(tempo_of(&cp, "g.a"), gt);
            assert_eq!(tempo_of(&cp, "g.c"), gt);
            assert_ne!(tempo_of(&cp, "g.b"), gt);
            assert_eq!(proc_tempo(&cp, "g.a", "s"), gt, "{perm}");
            assert_eq!(proc_tempo(&cp, "g.c", "s"), gt, "{perm}");
            // main is shared, not owned by a voice, so it stays
            assert!(cp.engine_state.tempos.contains_key(&main));
        }
    }

    #[test]
    fn group_keeps_owned_process_tempos() {
        let mut cp = booted(&["a"]);
        ok(&mut cp, "voice a from \"a\"");
        ok(&mut cp, "a { seq s 1 on 140b }");
        let own = proc_tempo(&cp, "a", "s");
        let args = group_args(ok(&mut cp, "group g on 240b { a }"));
        assert_eq!(link(&cp, "g.a"), TempoLink::Group(args.group));
        assert_eq!(proc_tempo(&cp, "g.a", "s"), own);
        assert_ne!(own, args.tempo.id());
        assert_eq!(
            cp.engine_state.tempos[&own].owner,
            TempoOwner::Voice(vid(&cp, "g.a"))
        );
    }

    #[test]
    fn group_moves_voice_following_processes() {
        let mut cp = booted(&["a"]);
        ok(&mut cp, "voice a from \"a\"");
        ok(&mut cp, "a { seq s 1 }");
        let args = group_args(ok(&mut cp, "group g on 240b { a }"));
        assert_eq!(proc_tempo(&cp, "g.a", "s"), args.tempo.id());
        let es = &cp.engine_state;
        let p = &es.procs[&es.voices[&vid(&cp, "g.a")].procs["s"]];
        assert_eq!(p.link, TempoLink::Voice);
    }

    #[test]
    fn failed_group_leaves_mirror_unchanged() {
        let names = ["a", "b", "c"];
        for cmd in [
            "group g on 240b { a; nope }",
            "group g on 240b { nope; b }",
            "group g on x1 { a }",
            "group g on nope { a }",
            "group g on 240b { c; nope }",
            // a name in use
            "group a on 240b { b }",
            "group g on 240b { voice b from \"nope\" }",
        ] {
            let mut cp = loaded(&names);
            err(&mut cp, cmd);
            assert_untouched(&cp, &names);
        }
    }

    #[test]
    fn group_unknown_voice_is_unknown_name() {
        let mut cp = loaded(&["a"]);
        let errs = lerr(&mut cp, "group g on 240b { a; nope }");
        assert_eq!(errs[0].code, Code::UnknownName);
        assert!(errs[0].message.contains("nope"), "{}", errs[0]);
    }

    #[test]
    fn group_block_over_several_lines() {
        let mut cp = loaded(&["a", "b", "c", "d"]);
        let args = group_args(ok(&mut cp, "group g on 240b {\n  c\n  a\n}"));
        assert_eq!(args.members, vec![VoiceId(2), VoiceId(0)]);
        assert_eq!(vid(&cp, "g.c"), VoiceId(2));
        assert_eq!(vid(&cp, "g.a"), VoiceId(0));
        assert_eq!(vid(&cp, "b"), VoiceId(1));
        assert_eq!(vid(&cp, "d"), VoiceId(3));
    }

    #[test]
    fn restated_group_is_not_remade() {
        let mut cp = loaded(&["a", "b", "c"]);
        ok(&mut cp, "group g on 240b { a }");
        assert_eq!(cp.engine_state.groups.len(), 1);
        assert_eq!(members(&cp, "g"), vec![VoiceId(0)]);
        assert!(local(&mut cp, "group g on 240b { a }").contains("W101"));

        ok(&mut cp, "group h on 240b { b }");
        assert_eq!(gid(&cp, "h"), GroupId(1));
    }

    #[test]
    fn grouped_voice_stays_in_voice_map() {
        let mut cp = loaded(&["a", "b"]);
        let args = group_args(ok(&mut cp, "group g on 240b { a; b }"));
        for v in &args.members {
            assert_eq!(cp.engine_state.voices[v].group, Some(args.group));
        }
        assert!(cp.engine_state.voice_names.is_empty());
    }

    // the state text recreates the session from nothing
    fn round_trips(cp: &CmdProcessor, names: &[&str]) -> String {
        let text = cp.state(None).unwrap();
        let mut fresh = booted(names);
        ok(&mut fresh, &text);
        assert_eq!(fresh.state(None).unwrap(), text);
        text
    }

    #[test]
    fn group_gain_state_round_trip() {
        let mut cp = booted(&["a", "b"]);
        ok(&mut cp, "voice a from \"a\"");
        ok(&mut cp, "voice b from \"b\"");
        ok(&mut cp, "group g on 240b { a }");
        ok(&mut cp, "g gain=0.5 over 2");
        assert_eq!(cp.engine_state.groups[&gid(&cp, "g")].gain, 0.5);
        let text = round_trips(&cp, &["a", "b"]);
        assert!(text.contains("group g on 240b gain=0.5 {"), "{text}");
        // the same gain again changes nothing
        assert!(local(&mut cp, "g gain=0.5").contains("W101"));
        // decibels convert
        ok(&mut cp, "g gain=-6db");
        assert!((cp.engine_state.groups[&gid(&cp, "g")].gain - 0.501).abs() < 0.01);
        ok(&mut cp, "g gain=1");
        assert!(!cp.state(None).unwrap().contains("gain="));

        // a new group can state it, and a bad value is refused
        ok(&mut cp, "group h gain=2 { b }");
        round_trips(&cp, &["a", "b"]);
        assert_eq!(lerr(&mut cp, "g gain=9")[0].code, Code::OutOfRange);
        assert!(cp.parse("g gain=1 over -1").is_err());
    }

    #[test]
    fn group_period_state_round_trip() {
        let mut cp = booted(&["a"]);
        ok(&mut cp, "voice a from \"a\"");
        ok(&mut cp, "group g on 240b { a }");
        assert!(!cp.state(None).unwrap().contains(" in "));
        // the members don't read it: only the group changes
        let before = cp.engine_state.voices[&vid(&cp, "g.a")].period;
        // mirror only: nothing goes to the engine
        assert_eq!(local(&mut cp, "g in 8"), "");
        assert_eq!(cp.engine_state.groups[&gid(&cp, "g")].period, 8);
        assert_eq!(cp.engine_state.voices[&vid(&cp, "g.a")].period, before);
        let text = round_trips(&cp, &["a"]);
        assert!(text.contains("group g on 240b in 8 {"), "{text}");
        assert!(local(&mut cp, "g in 8").contains("W101"));
    }

    #[test]
    fn edits_round_trip_through_state() {
        let names = ["a", "b", "c"];
        let mut cp = booted(&names);
        for l in [
            "voice a from \"a\"",
            "voice b from \"b\" on 93b",
            "voice c from \"c\"",
            "group kit on 240b { a; b }",
            "group drums { c }",
            "drums { a }",
            "kit { c }",
            "ungroup kit.b",
        ] {
            ok(&mut cp, l);
        }
        let text = round_trips(&cp, &names);
        // b is back on the top level, with its own clock
        assert!(text.contains("voice b from \"b\" on 93b"), "{text}");
        assert_eq!(members(&cp, "kit"), vec![VoiceId(2)]);
        assert_eq!(members(&cp, "drums"), vec![VoiceId(0)]);
    }

    #[test]
    fn ungroup_errors() {
        let mut cp = booted(&["a", "b"]);
        ok(&mut cp, "voice a from \"a\"");
        ok(&mut cp, "voice b from \"b\"");
        ok(&mut cp, "group g { a }");
        ok(&mut cp, "clock clk 60b");
        assert_eq!(lerr(&mut cp, "ungroup b")[0].code, Code::WrongType);
        assert_eq!(lerr(&mut cp, "ungroup clk")[0].code, Code::WrongType);
        assert_eq!(lerr(&mut cp, "ungroup nope")[0].code, Code::UnknownName);
        assert_eq!(lerr(&mut cp, "ungroup g.b")[0].code, Code::AmbiguousPath);
        // one bad target undoes the whole line
        assert!(cp.parse("ungroup g.a b").is_err());
        assert_eq!(members(&cp, "g"), vec![VoiceId(0)]);
    }
}
