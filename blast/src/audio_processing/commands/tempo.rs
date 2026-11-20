//! Tempo states and clocks.

use super::errors::CmdResult;
use super::lang::ast::ActionWord;
use super::reprs::Target;
use super::reprs::{
    EngineState, GroupId, ProcId, ProcOwner, TempoId, TempoLink, TempoOwner, VoiceId, alloc,
};
use super::{BatchArgs, CmdProcessor, Command};
use crate::audio_processing::blast_time::blast_time::TempoUnit;

/// Whether a clock runs, as the engine's `TempoState.active` and the
/// start/stop history say (CLKG). `Running` is exactly `active`; `Paused`
/// and `Stopped` are both inactive, but only a stopped clock refuses a
/// glide (E409).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockRun {
    Stopped,
    Paused,
    Running,
}

/// A live TempoState in the mirror.
#[derive(Clone, Debug, PartialEq)]
pub struct TempoRepr {
    pub(crate) id: TempoId,
    pub(crate) owner: TempoOwner,
    pub(crate) unit: TempoUnit,
    /// In `unit`s.
    pub(crate) interval: f32,
    /// Kept by the same rules the engine follows (`EngineState::run_*`):
    /// nothing in the engine changes it except a command.
    pub(crate) run: ClockRun,
}

/// A tempo a command creates. The engine converts `interval` to samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoInit {
    pub id: TempoId,
    pub unit: TempoUnit,
    pub interval: f32,
}

/// How a new object gets its tempo. An `Own` tempo's owner follows from the
/// command (voice: the voice; group: the group; seq: the process's owner).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TempoUse {
    Own(TempoInit),
    Shared(TempoId),
}

impl TempoUse {
    /// The tempo's ID, own or shared.
    pub fn id(self) -> TempoId {
        match self {
            TempoUse::Own(init) => init.id,
            TempoUse::Shared(id) => id,
        }
    }
}

/// Arguments for `Command::Tc`.
pub struct TcArgs {
    pub tempo: TempoInit,
}

/// Arguments for `Command::TempoSet`: a tempo changes its rate, keeping its
/// position (`unit` and `interval` as written, as in `TempoInit`).
pub struct TempoSetArgs {
    pub id: TempoId,
    pub unit: TempoUnit,
    pub interval: f32,
    /// 0: at once. Otherwise the rate glides to the new one over this many
    /// frames of the clock running (REQ-074).
    pub frames: u32,
}

/// Arguments for `Command::TempoFree`: a tempo nothing uses is dropped.
pub struct TempoFreeArgs {
    pub id: TempoId,
}

/// Arguments for `Command::TempoAssign`: an existing object takes another
/// tempo. An `Own` tempo is created for it, owned by the target.
pub struct TempoAssignArgs {
    pub target: TempoTarget,
    pub tempo: TempoUse,
}

/// What a `TempoAssign` points at another tempo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TempoTarget {
    Voice(VoiceId),
    Group(GroupId),
    Proc(ProcId),
}

/// A tempo still to be created: its unit and interval, before it has an ID.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NewTempo {
    pub(crate) unit: TempoUnit,
    pub(crate) interval: f32,
}

/// How a command chose its tempo, before any ID is allocated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TempoChoice {
    Own(NewTempo),
    Shared(TempoId),
}

impl CmdProcessor {
    /// Turns a validated choice into a `TempoUse`, allocating a tempo ID for
    /// an own tempo. The `TempoRepr` is inserted by `insert_tempo` once every
    /// allocation of the command has succeeded.
    pub(super) fn alloc_tempo(&mut self, choice: TempoChoice) -> CmdResult<TempoUse> {
        Ok(match choice {
            TempoChoice::Own(t) => TempoUse::Own(TempoInit {
                id: alloc(&mut self.engine_state.ids.tempos, "tempos")?,
                unit: t.unit,
                interval: t.interval,
            }),
            TempoChoice::Shared(id) => TempoUse::Shared(id),
        })
    }

    /// Records an own tempo in the mirror; a shared one is already there.
    pub(super) fn insert_tempo(&mut self, tempo: TempoUse, owner: TempoOwner, run: ClockRun) {
        if let TempoUse::Own(init) = tempo {
            self.engine_state.tempos.insert(
                init.id,
                TempoRepr {
                    id: init.id,
                    owner,
                    unit: init.unit,
                    interval: init.interval,
                    run,
                },
            );
        }
    }

    /// Creates a clock called `name` (the language's `clock`).
    pub(crate) fn make_clock(
        &mut self,
        name: &str,
        unit: TempoUnit,
        interval: f32,
    ) -> CmdResult<Command> {
        let tempo = TempoInit {
            id: alloc(&mut self.engine_state.ids.tempos, "tempos")?,
            unit,
            interval,
        };
        self.insert_tempo(TempoUse::Own(tempo), TempoOwner::Context, ClockRun::Stopped);
        self.engine_state
            .context_names
            .insert(name.to_string(), tempo.id);
        Ok(Command::Tc(TcArgs { tempo }))
    }
}

/// Something that uses a tempo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TempoUser {
    Voice(VoiceId),
    Group(GroupId),
    Proc(ProcId),
}

impl From<TempoTarget> for TempoUser {
    fn from(t: TempoTarget) -> Self {
        match t {
            TempoTarget::Voice(v) => TempoUser::Voice(v),
            TempoTarget::Group(g) => TempoUser::Group(g),
            TempoTarget::Proc(p) => TempoUser::Proc(p),
        }
    }
}

/// The name a user is listed by: `kick`, `drums.hat`, `kick.hats`.
fn user_name(state: &EngineState, user: TempoUser) -> String {
    match user {
        TempoUser::Voice(v) => state.display_name(v).unwrap_or_default(),
        TempoUser::Group(g) => state
            .groups
            .get(&g)
            .map(|g| g.name.clone())
            .unwrap_or_default(),
        TempoUser::Proc(p) => state
            .procs
            .get(&p)
            .map(|r| match r.owner {
                ProcOwner::Voice(v) => {
                    format!("{}.{}", state.display_name(v).unwrap_or_default(), r.name)
                }
                ProcOwner::Group(g) => format!(
                    "{}.{}",
                    state
                        .groups
                        .get(&g)
                        .map(|g| g.name.as_str())
                        .unwrap_or_default(),
                    r.name
                ),
                ProcOwner::Session => r.name.clone(),
            })
            .unwrap_or_default(),
    }
}

/// Everything in the mirror that reads tempo `id`, voices first, then seqs,
/// then groups, each by name.
pub(crate) fn tempo_users(state: &EngineState, id: TempoId) -> Vec<TempoUser> {
    let mut users: Vec<TempoUser> = Vec::new();
    users.extend(
        state
            .voices
            .values()
            .filter(|v| v.tempo == id)
            .map(|v| TempoUser::Voice(v.id)),
    );
    users.extend(
        state
            .procs
            .values()
            .filter(|p| p.tempo == id)
            .map(|p| TempoUser::Proc(p.id)),
    );
    users.extend(
        state
            .groups
            .values()
            .filter(|g| g.tempo == id)
            .map(|g| TempoUser::Group(g.id)),
    );
    sort_users(state, &mut users);
    users
}

fn sort_users(state: &EngineState, users: &mut [TempoUser]) {
    let rank = |u: &TempoUser| match u {
        TempoUser::Voice(_) => 0,
        TempoUser::Proc(_) => 1,
        TempoUser::Group(_) => 2,
    };
    users.sort_by_cached_key(|u| (rank(u), user_name(state, *u)));
}

/// `voice kick, seq kick.hats, group drums`.
pub(crate) fn describe_users(state: &EngineState, users: &[TempoUser]) -> String {
    users
        .iter()
        .map(|&u| {
            let kind = match u {
                TempoUser::Voice(_) => "voice",
                TempoUser::Proc(p) if state.procs.get(&p).is_some_and(|r| r.lfo.is_some()) => "lfo",
                TempoUser::Proc(p) if state.procs.get(&p).is_some_and(|r| r.env.is_some()) => "env",
                TempoUser::Proc(_) => "seq",
                TempoUser::Group(_) => "group",
            };
            format!("{kind} {}", user_name(state, u))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What follows a voice's clock: voices, groups and seqs linked `Follow`.
pub(crate) fn voice_followers(state: &EngineState, leader: VoiceId) -> Vec<TempoUser> {
    let link = TempoLink::Follow(leader);
    let mut users: Vec<TempoUser> = Vec::new();
    users.extend(
        state
            .voices
            .values()
            .filter(|v| v.link == link)
            .map(|v| TempoUser::Voice(v.id)),
    );
    users.extend(
        state
            .procs
            .values()
            .filter(|p| p.link == link)
            .map(|p| TempoUser::Proc(p.id)),
    );
    users.extend(
        state
            .groups
            .values()
            .filter(|g| g.link == link)
            .map(|g| TempoUser::Group(g.id)),
    );
    sort_users(state, &mut users);
    users
}

fn tempo_of(state: &EngineState, t: TempoTarget) -> Option<TempoId> {
    match t {
        TempoTarget::Voice(v) => state.voices.get(&v).map(|r| r.tempo),
        TempoTarget::Group(g) => state.groups.get(&g).map(|r| r.tempo),
        TempoTarget::Proc(p) => state.procs.get(&p).map(|r| r.tempo),
    }
}

fn link_of(state: &EngineState, t: TempoTarget) -> Option<TempoLink> {
    match t {
        TempoTarget::Voice(v) => state.voices.get(&v).map(|r| r.link),
        TempoTarget::Group(g) => state.groups.get(&g).map(|r| r.link),
        TempoTarget::Proc(p) => state.procs.get(&p).map(|r| r.link),
    }
}

fn set_tempo_link(
    state: &mut EngineState,
    t: TempoTarget,
    tempo: Option<TempoId>,
    link: Option<TempoLink>,
) {
    let (t_ref, l_ref) = match t {
        TempoTarget::Voice(v) => match state.voices.get_mut(&v) {
            Some(r) => (&mut r.tempo, &mut r.link),
            None => return,
        },
        TempoTarget::Group(g) => match state.groups.get_mut(&g) {
            Some(r) => (&mut r.tempo, &mut r.link),
            None => return,
        },
        TempoTarget::Proc(p) => match state.procs.get_mut(&p) {
            Some(r) => (&mut r.tempo, &mut r.link),
            None => return,
        },
    };
    if let Some(tempo) = tempo {
        *t_ref = tempo;
    }
    if let Some(link) = link {
        *l_ref = link;
    }
}

fn owner_of(state: &EngineState, t: TempoTarget) -> Option<TempoOwner> {
    match t {
        TempoTarget::Voice(v) => Some(TempoOwner::Voice(v)),
        TempoTarget::Group(g) => Some(TempoOwner::Group(g)),
        TempoTarget::Proc(p) => state.procs.get(&p).map(|r| TempoOwner::from(r.owner)),
    }
}

/// The objects that read `leader`'s clock because of how they were made, so
/// they move when it moves: voices, groups and seqs linked to it.
fn followers(state: &EngineState, leader: TempoTarget) -> Vec<TempoTarget> {
    let follows = |link: TempoLink| match leader {
        TempoTarget::Voice(v) => link == TempoLink::Follow(v),
        TempoTarget::Group(g) => link == TempoLink::Group(g),
        TempoTarget::Proc(_) => false,
    };
    let mut out: Vec<TempoTarget> = Vec::new();
    for v in state.voices.values() {
        // a member that follows its parent adopted the group's clock
        let adopted = match leader {
            TempoTarget::Group(g) => {
                v.group == Some(g)
                    && v.link == TempoLink::Parent
                    && state.groups.get(&g).is_some_and(|r| r.tempo == v.tempo)
            }
            _ => false,
        };
        if follows(v.link) || adopted {
            out.push(TempoTarget::Voice(v.id));
        }
    }
    out.extend(
        state
            .groups
            .values()
            .filter(|g| follows(g.link))
            .map(|g| TempoTarget::Group(g.id)),
    );
    out.extend(
        state
            .procs
            .values()
            .filter(|p| follows(p.link))
            .map(|p| TempoTarget::Proc(p.id)),
    );
    out
}

/// `target` and everything that moves with it (see TMP "Who moves"): a
/// voice takes its seqs that follow it and whatever follows its clock; a
/// group takes whatever follows its clock (adopted members); a seq goes
/// alone. `target` comes first.
pub(crate) fn movers(state: &EngineState, target: TempoTarget) -> Vec<TempoTarget> {
    let mut out = vec![target];
    let mut i = 0;
    while let Some(&t) = out.get(i) {
        i += 1;
        let mut more: Vec<TempoTarget> = Vec::new();
        match t {
            TempoTarget::Voice(v) => {
                more.extend(
                    state
                        .procs
                        .values()
                        .filter(|p| p.owner == ProcOwner::Voice(v) && p.link == TempoLink::Voice)
                        .map(|p| TempoTarget::Proc(p.id)),
                );
                more.extend(followers(state, t));
            }
            TempoTarget::Group(_) => more.extend(followers(state, t)),
            TempoTarget::Proc(_) => {}
        }
        for m in more {
            if !out.contains(&m) {
                out.push(m);
            }
        }
    }
    out
}

/// The tempo, link and tempo owner of a voice, group or seq in the mirror.
pub(crate) fn clock_of(
    state: &EngineState,
    t: TempoTarget,
) -> Option<(TempoId, TempoLink, TempoOwner)> {
    Some((tempo_of(state, t)?, link_of(state, t)?, owner_of(state, t)?))
}

impl EngineState {
    fn set_owned_run(&mut self, owner: TempoOwner, run: ClockRun) {
        for t in self.tempos.values_mut().filter(|t| t.owner == owner) {
            t.run = run;
        }
    }

    /// `voice_running` in the engine: active, and its group too.
    fn voice_plays(&self, v: VoiceId) -> bool {
        self.voices
            .get(&v)
            .is_some_and(|r| r.active && r.group.is_none_or(|g| self.group_plays(g)))
    }

    fn group_plays(&self, g: GroupId) -> bool {
        self.groups.get(&g).is_some_and(|r| r.active)
    }

    /// Whether `assign_tempo` starts the own clock it creates for `target`.
    pub(crate) fn assign_starts(&self, target: TempoTarget) -> bool {
        match target {
            TempoTarget::Voice(v) => self.voice_plays(v),
            TempoTarget::Group(g) => self.group_plays(g),
            TempoTarget::Proc(p) => match self.procs.get(&p).map(|r| r.owner) {
                Some(ProcOwner::Voice(v)) => self.voice_plays(v),
                Some(ProcOwner::Group(g)) => self.group_plays(g),
                Some(ProcOwner::Session) => true,
                None => false,
            },
        }
    }

    /// Whether the engine's `seq` starts the own clock it creates for a seq
    /// owned by `owner`: the owner's own `active` flag (a voice's group is
    /// not asked), or always for a session seq.
    pub(crate) fn seq_starts(&self, owner: ProcOwner) -> bool {
        match owner {
            ProcOwner::Voice(v) => self.voices.get(&v).is_some_and(|r| r.active),
            ProcOwner::Group(g) => self.group_plays(g),
            ProcOwner::Session => true,
        }
    }

    /// `start_voice` in the engine.
    fn run_start_voice(&mut self, v: VoiceId) {
        if let Some(r) = self.voices.get_mut(&v) {
            r.active = true;
        }
        self.set_owned_run(TempoOwner::Voice(v), ClockRun::Running);
    }

    /// `stop_voice` in the engine.
    fn run_stop_voice(&mut self, v: VoiceId) {
        if let Some(r) = self.voices.get_mut(&v) {
            r.active = false;
        }
        self.set_owned_run(TempoOwner::Voice(v), ClockRun::Stopped);
    }

    /// Follows the engine's `start`, `pause`, `resume` and `stop` handlers
    /// for one target, so the mirror knows which clocks run.
    pub(crate) fn run_transport(&mut self, word: ActionWord, target: Target) {
        match (word, target) {
            (ActionWord::Start, Target::Voice(v)) => self.run_start_voice(v),
            (ActionWord::Start, Target::Group(g)) => {
                if let Some(r) = self.groups.get_mut(&g) {
                    r.active = true;
                }
                self.set_owned_run(TempoOwner::Group(g), ClockRun::Running);
                for m in self
                    .groups
                    .get(&g)
                    .map(|r| r.members.clone())
                    .unwrap_or_default()
                {
                    self.run_start_voice(m);
                }
            }
            (ActionWord::Start | ActionWord::Resume, Target::Tempo(t)) => {
                self.set_run(t, ClockRun::Running)
            }
            (ActionWord::Pause, Target::Voice(v)) => {
                if let Some(r) = self.voices.get_mut(&v) {
                    r.active = false;
                }
            }
            (ActionWord::Pause, Target::Group(g)) => {
                if let Some(r) = self.groups.get_mut(&g) {
                    r.active = false;
                }
            }
            // pausing a stopped clock leaves it stopped (its position is 0)
            (ActionWord::Pause, Target::Tempo(t)) => {
                if let Some(r) = self.tempos.get_mut(&t)
                    && r.run == ClockRun::Running
                {
                    r.run = ClockRun::Paused;
                }
            }
            (ActionWord::Resume, Target::Voice(v)) => {
                if let Some(r) = self.voices.get_mut(&v) {
                    r.active = true;
                }
            }
            (ActionWord::Resume, Target::Group(g)) => {
                if let Some(r) = self.groups.get_mut(&g) {
                    r.active = true;
                }
            }
            (ActionWord::Stop, Target::Voice(v)) => self.run_stop_voice(v),
            (ActionWord::Stop, Target::Group(g)) => {
                if let Some(r) = self.groups.get_mut(&g) {
                    r.active = false;
                }
                for m in self
                    .groups
                    .get(&g)
                    .map(|r| r.members.clone())
                    .unwrap_or_default()
                {
                    self.run_stop_voice(m);
                }
                self.set_owned_run(TempoOwner::Group(g), ClockRun::Stopped);
            }
            (ActionWord::Stop, Target::Tempo(t)) => self.set_run(t, ClockRun::Stopped),
            _ => {}
        }
    }

    fn set_run(&mut self, t: TempoId, run: ClockRun) {
        if let Some(r) = self.tempos.get_mut(&t) {
            r.run = run;
        }
    }
}

/// A command that does nothing (the mirror changed, the engine need not).
fn empty_batch() -> Command {
    Command::Batch(BatchArgs { cmds: Vec::new() })
}

impl CmdProcessor {
    /// Puts `target` on the clock `choice` names, with `link`, and whatever
    /// moves with it (`movers`). An own clock for a target that already owns
    /// its clock retimes that one instead. An old own clock nobody uses any
    /// more is freed. The only failure is a full tempo pool, which changes
    /// nothing. Returns one command (a Batch for several).
    pub(crate) fn move_to_clock(
        &mut self,
        target: TempoTarget,
        choice: TempoChoice,
        link: TempoLink,
    ) -> CmdResult<Command> {
        let es = &self.engine_state;
        let (Some(current), Some(old_link), Some(owner)) = (
            tempo_of(es, target),
            link_of(es, target),
            owner_of(es, target),
        ) else {
            return Ok(empty_batch());
        };

        // an own clock on a target that has one: change its rate, so the
        // phase carries on and the followers hear it
        if let TempoChoice::Own(n) = choice
            && old_link == TempoLink::Own
            && es.tempos.get(&current).is_some_and(|t| t.owner == owner)
        {
            if let Some(repr) = self.engine_state.tempos.get_mut(&current) {
                repr.unit = n.unit;
                repr.interval = n.interval;
            }
            return Ok(Command::TempoSet(TempoSetArgs {
                id: current,
                unit: n.unit,
                interval: n.interval,
                frames: 0,
            }));
        }

        // the same clock reached another way: only the link changes
        if choice == TempoChoice::Shared(current) {
            set_tempo_link(&mut self.engine_state, target, None, Some(link));
            return Ok(empty_batch());
        }

        let movers = movers(es, target);
        // the only step that can fail; nothing has changed yet
        let tempo = self.alloc_tempo(choice)?;
        // the engine starts an own clock at once if its target is playing
        let run = match self.engine_state.assign_starts(target) {
            true => ClockRun::Running,
            false => ClockRun::Stopped,
        };
        self.insert_tempo(tempo, owner, run);
        let id = tempo.id();

        let es = &mut self.engine_state;
        let mut cmds: Vec<Command> = Vec::new();
        let mut olds: Vec<TempoId> = Vec::new();
        for (i, &m) in movers.iter().enumerate() {
            if let Some(old) = tempo_of(es, m)
                && !olds.contains(&old)
            {
                olds.push(old);
            }
            set_tempo_link(es, m, Some(id), (i == 0).then_some(link));
            cmds.push(Command::TempoAssign(TempoAssignArgs {
                target: m,
                tempo: if i == 0 { tempo } else { TempoUse::Shared(id) },
            }));
        }

        // a clock of its own that nobody reads now is let go; a named
        // clock never is
        for old in olds {
            let ownable = es
                .tempos
                .get(&old)
                .is_some_and(|t| t.owner != TempoOwner::Context);
            if ownable && tempo_users(es, old).is_empty() {
                es.tempos.remove(&old);
                es.ids.tempos.free(old);
                cmds.push(Command::TempoFree(TempoFreeArgs { id: old }));
            }
        }
        Ok(Command::Batch(BatchArgs { cmds }))
    }

    /// Removes the clock `id` (and its name) if nothing uses it; otherwise
    /// the users, for the error. Returns the `TempoFree`.
    pub(crate) fn remove_clock(&mut self, id: TempoId) -> Result<Command, Vec<TempoUser>> {
        let users = tempo_users(&self.engine_state, id);
        if !users.is_empty() {
            return Err(users);
        }
        let es = &mut self.engine_state;
        let remove = |es: &mut EngineState| {
            es.tempos.remove(&id);
            es.context_names.retain(|_, t| *t != id);
            es.ids.tempos.free(id);
            Ok(Command::TempoFree(TempoFreeArgs { id }))
        };
        // a line that waits only marks the clock (PKb3)
        if es.staging {
            return es
                .staged_removal(
                    crate::audio_processing::commands::AnyId::Tempo(id),
                    false,
                    remove,
                )
                .map_err(|_| Vec::new());
        }
        remove(es).map_err(|_: super::errors::CmdErr| Vec::new())
    }

    /// Changes a named clock's rate in the mirror (to the target, if it
    /// glides) and returns the command. `frames` is 0 for at once.
    pub(crate) fn retime_clock(
        &mut self,
        id: TempoId,
        unit: TempoUnit,
        interval: f32,
        frames: u32,
    ) -> Command {
        if let Some(repr) = self.engine_state.tempos.get_mut(&id) {
            repr.unit = unit;
            repr.interval = interval;
        }
        Command::TempoSet(TempoSetArgs {
            id,
            unit,
            interval,
            frames,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::blast_time::sample_rate;
    use crate::audio_processing::commands::lang::Code;
    use crate::audio_processing::commands::testing::*;

    #[test]
    fn clock_makes_a_tempo() {
        let mut cp = processor(&[]);
        let t = tc_tempo(ok(&mut cp, "clock a 120b"));
        assert_eq!(
            t,
            TempoInit {
                id: TempoId(0),
                unit: TempoUnit::Bpm,
                interval: 120.0
            }
        );
        assert_eq!(
            cp.engine_state.tempos[&TempoId(0)].owner,
            TempoOwner::Context
        );
        let t = tc_tempo(ok(&mut cp, "clock b 500ms"));
        assert_eq!(t.unit, TempoUnit::Millis);
        assert_eq!(t.id, TempoId(1));
        let t = tc_tempo(ok(&mut cp, "clock c 100f"));
        assert_eq!(t.unit, TempoUnit::Samples);

        let code = |cp: &mut CmdProcessor, line: &str| lerr(cp, line)[0].code;
        assert_eq!(code(&mut cp, "clock d"), Code::MissingItem);
        assert_eq!(code(&mut cp, "clock d 0b"), Code::OutOfRange);
        assert!(cp.parse("clock d b").is_err());
        assert!(cp.parse("clock d x:1").is_err());
        assert_eq!(cp.engine_state.context_names.len(), 3);
        assert_eq!(cp.engine_state.ids.tempos.live(), 3);
    }

    #[test]
    fn clock_glides_over() {
        sample_rate::set(48000);
        let mut cp = processor(&[]);
        ok(&mut cp, "clock a 120b");
        ok(&mut cp, "start a");
        let a = ctx(&cp, "a");
        let frames = |c: Command| match c {
            Command::TempoSet(s) => (s.frames, s.interval),
            _ => panic!("not a TempoSet"),
        };
        // a bare number is periods of 4 ticks; a tick is 0.5 s at 120b
        assert_eq!(
            frames(ok(&mut cp, "clock a 140b over 8")),
            (8 * 4 * 24000, 140.0)
        );
        // the mirror holds the target
        assert_eq!(cp.engine_state.tempos[&a].interval, 140.0);
        // ticks are at the rate the clock has now (140b)
        let tick = (60.0_f64 / 140.0 * 48000.0).round() as u32;
        let (f, _) = frames(ok(&mut cp, "clock a 120b over 2b"));
        assert!(f.abs_diff(2 * tick) <= 1, "{f}");
        assert_eq!(frames(ok(&mut cp, "clock a 100b over 2s")).0, 96000);
        assert_eq!(frames(ok(&mut cp, "clock a 90b over 4800f")).0, 4800);
        assert_eq!(frames(ok(&mut cp, "clock a 80b")).0, 0);
        // variables work in the rate and the duration
        local(&mut cp, "let n = 2s");
        assert_eq!(frames(ok(&mut cp, "clock a 70b over $n")).0, 96000);
        assert!(cp.parse("clock a 90b over").is_err());
        assert!(cp.parse("clock a 90b over -1").is_err());
    }

    #[test]
    fn glide_on_new_clock_e409() {
        let mut cp = processor(&[]);
        // a new clock has nothing to glide from: refused, nothing created
        assert_eq!(lerr(&mut cp, "clock b 100b over 4")[0].code, Code::ClockOff);
        assert!(!cp.engine_state.context_names.contains_key("b"));
        assert_eq!(Code::ClockOff.as_str(), "E409");
        // without over, it's made as ever
        tc_tempo(ok(&mut cp, "clock b 100b"));
    }

    #[test]
    fn glide_on_stopped_clock_e409() {
        sample_rate::set(48000);
        let mut cp = processor(&[]);
        ok(&mut cp, "clock a 120b");
        // never started
        assert_eq!(lerr(&mut cp, "clock a 140b over 4")[0].code, Code::ClockOff);
        // and the mirror kept the rate
        assert_eq!(cp.engine_state.tempos[&ctx(&cp, "a")].interval, 120.0);
        // stopped again
        ok(&mut cp, "start a");
        ok(&mut cp, "stop a");
        assert_eq!(lerr(&mut cp, "clock a 140b over 4")[0].code, Code::ClockOff);
        // a plain retime is fine on a stopped clock
        ok(&mut cp, "clock a 140b");
    }

    #[test]
    fn glide_on_paused_clock_waits_for_resume() {
        sample_rate::set(48000);
        let mut cp = processor(&[]);
        ok(&mut cp, "clock a 120b");
        ok(&mut cp, "start a");
        ok(&mut cp, "pause a");
        // a paused clock accepts the glide
        match ok(&mut cp, "clock a 140b over 4") {
            Command::TempoSet(s) => assert!(s.frames > 0),
            _ => panic!("not a TempoSet"),
        }
        ok(&mut cp, "resume a");
        ok(&mut cp, "clock a 150b over 4");
    }

    #[test]
    fn glide_on_running_clock_works() {
        let mut cp = booted(&[]);
        // `main` starts with the session
        match ok(&mut cp, "clock main 140b over 8") {
            Command::TempoSet(s) => assert!(s.frames > 0),
            _ => panic!("not a TempoSet"),
        }
    }

    #[test]
    fn restated_clock_is_not_remade() {
        let mut cp = processor(&[]);
        ok(&mut cp, "clock a 120b");
        // the same rate changes nothing; another retimes the clock
        assert!(local(&mut cp, "clock a 120b").contains("W101"));
        let Command::TempoSet(set) = ok(&mut cp, "clock a 90b") else {
            panic!("not a TempoSet")
        };
        let a = ctx(&cp, "a");
        assert_eq!((set.id, set.unit, set.interval), (a, TempoUnit::Bpm, 90.0));
        assert_eq!(cp.engine_state.tempos[&a].interval, 90.0);
        assert_eq!(cp.engine_state.context_names.len(), 1);
        assert_eq!(cp.engine_state.ids.tempos.live(), 1);

        // the next clock gets the next ID
        let t = tc_tempo(ok(&mut cp, "clock b 60b"));
        assert_eq!(
            t,
            TempoInit {
                id: TempoId(1),
                unit: TempoUnit::Bpm,
                interval: 60.0
            }
        );
        assert_eq!(ctx(&cp, "b"), TempoId(1));
    }
}
