//! Processes in general (PRC): what the mirror keeps of them, and the
//! command that pauses, resumes, resets or removes one.

use super::procs::env::EnvRepr;
use super::procs::lfo::LfoRepr;
use super::procs::seq::{Euclid, RecvSpec, SeqStep, SlotGen};
use super::reprs::{DEFAULT_PERIOD, PacketId, ProcId, ProcOwner, TempoId, TempoLink, TempoOwner};
use super::tempo::tempo_users;
use super::{BatchArgs, CmdProcessor, Command, EngineState, TempoFreeArgs};
use std::collections::BTreeMap;

/// What the mirror keeps of a seq, for `state` and the no-op check: its
/// steps (0-based, as the engine has them) and its cycle in ticks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeqRepr {
    pub(crate) steps: Vec<SeqStep>,
    pub(crate) period: u32,
    /// True once `in N` was written for this seq; until then it follows
    /// its owner's period.
    pub(crate) explicit_period: bool,
    /// The seed it was given (`seed=`), if any; `state` prints only that.
    pub(crate) seed: Option<u64>,
    /// The seq-wide `[chance, early, late]` the steps without a `%` or `~`
    /// of their own follow (a glide's target while it glides).
    pub(crate) feel: [f32; 3],
    /// What each firing step does (its pipe); `[trig]` for a seq written
    /// without one.
    pub(crate) recv: Vec<RecvSpec>,
    /// The generator `steps` came from (`steps=`), if it has one.
    pub(crate) r#gen: Option<Euclid>,
    /// The grid and rotation the written steps came with, if any; then
    /// `steps` is where they land.
    pub(crate) slots: Option<SlotGen>,
}

/// What every Process has in common; each kind differs otherwise.
#[derive(Clone)]
pub struct ProcRepr {
    pub(crate) id: ProcId,
    pub(crate) name: String,
    pub(crate) owner: ProcOwner,
    pub(crate) tempo: TempoId,
    pub(crate) link: TempoLink,
    /// False while `pause`d; a paused process doesn't fire.
    pub(crate) enabled: bool,
    /// The packet this seq belongs to, which gives it its receivers.
    pub(crate) packet: Option<PacketId>,
    /// A seq's data (empty for other kinds).
    pub(crate) seq: SeqRepr,
    /// An lfo's data (`None` for a seq).
    pub(crate) lfo: Option<LfoRepr>,
    /// An env's data (`None` for a seq or an lfo).
    pub(crate) env: Option<EnvRepr>,
}

/// What a `Command::ProcEdit` does to a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcOp {
    /// Fires again, from the next step (no catch-up).
    Enable,
    /// Stops firing; what its owner is playing keeps playing.
    Disable,
    /// Back to the first step.
    Reset,
    /// The process goes, as garbage.
    Remove,
}

/// Arguments for `Command::ProcEdit`.
pub struct ProcEditArgs {
    pub id: ProcId,
    pub op: ProcOp,
}

impl EngineState {
    /// The processes `owner` holds, by name.
    pub(crate) fn owner_procs(&self, owner: ProcOwner) -> Option<&BTreeMap<String, ProcId>> {
        match owner {
            ProcOwner::Voice(v) => self.voices.get(&v).map(|v| &v.procs),
            ProcOwner::Group(g) => self.groups.get(&g).map(|g| &g.procs),
            ProcOwner::Session => Some(&self.session_procs),
        }
    }

    pub(crate) fn owner_procs_mut(
        &mut self,
        owner: ProcOwner,
    ) -> Option<&mut BTreeMap<String, ProcId>> {
        match owner {
            ProcOwner::Voice(v) => self.voices.get_mut(&v).map(|v| &mut v.procs),
            ProcOwner::Group(g) => self.groups.get_mut(&g).map(|g| &mut g.procs),
            ProcOwner::Session => Some(&mut self.session_procs),
        }
    }

    /// The period of a voice or a group, in ticks of its clock.
    pub(crate) fn owner_period(&self, owner: ProcOwner) -> Option<u32> {
        match owner {
            ProcOwner::Voice(v) => self.voices.get(&v).map(|v| v.period),
            ProcOwner::Group(g) => self.groups.get(&g).map(|g| g.period),
            ProcOwner::Session => Some(DEFAULT_PERIOD),
        }
    }

    /// The tempo a voice or a group reads.
    pub(crate) fn owner_tempo(&self, owner: ProcOwner) -> Option<TempoId> {
        match owner {
            ProcOwner::Voice(v) => self.voices.get(&v).map(|v| v.tempo),
            ProcOwner::Group(g) => self.groups.get(&g).map(|g| g.tempo),
            ProcOwner::Session => self.main_id(),
        }
    }
}

fn empty() -> Command {
    Command::Batch(BatchArgs { cmds: Vec::new() })
}

impl EngineState {
    /// `owner.name`, or just `name` for a seq at the top level.
    pub(crate) fn owner_path(&self, owner: ProcOwner, name: &str) -> String {
        let owner = match owner {
            ProcOwner::Voice(v) => self.display_name(v),
            ProcOwner::Group(g) => self.groups.get(&g).map(|g| g.name.clone()),
            ProcOwner::Session => None,
        };
        match owner {
            Some(owner) => format!("{owner}.{name}"),
            None => name.to_string(),
        }
    }

    /// The path a seq is written with: `kick.beat`.
    pub(crate) fn proc_path(&self, id: ProcId) -> Option<String> {
        let p = self.procs.get(&id)?;
        Some(self.owner_path(p.owner, &p.name))
    }

    /// Removes a process from the mirror and returns the commands that
    /// remove it from the engine, and its own clock if that is left unused.
    pub(crate) fn remove_proc(&mut self, id: ProcId) -> Command {
        let es = self;
        let Some(repr) = es.procs.remove(&id) else {
            return empty();
        };
        if let Some(procs) = es.owner_procs_mut(repr.owner)
            && procs.get(&repr.name) == Some(&id)
        {
            procs.remove(&repr.name);
        }
        es.ids.procs.free(id);
        if let Some(packet) = repr.packet.and_then(|p| es.packets.get_mut(&p)) {
            packet.seqs.retain(|s| *s != id);
        }

        let mut cmds = vec![Command::ProcEdit(ProcEditArgs {
            id,
            op: ProcOp::Remove,
        })];
        let ownable = es
            .tempos
            .get(&repr.tempo)
            .is_some_and(|t| t.owner != TempoOwner::Context);
        if ownable && tempo_users(es, repr.tempo).is_empty() {
            es.tempos.remove(&repr.tempo);
            es.ids.tempos.free(repr.tempo);
            cmds.push(Command::TempoFree(TempoFreeArgs { id: repr.tempo }));
        }
        Command::Batch(BatchArgs { cmds })
    }
}

impl CmdProcessor {
    /// Pauses, resumes or resets a process: `Enable` and `Disable` are kept
    /// in the mirror. A process that is gone gives an empty Batch.
    pub(crate) fn proc_op(&mut self, id: ProcId, op: ProcOp) -> Command {
        let Some(repr) = self.engine_state.procs.get_mut(&id) else {
            return empty();
        };
        let was = repr.enabled;
        match op {
            ProcOp::Enable => repr.enabled = true,
            ProcOp::Disable => repr.enabled = false,
            ProcOp::Reset | ProcOp::Remove => {}
        }
        // an lfo tells pause from stop by the Reset that follows the Disable
        if let Some(lfo) = &mut repr.lfo {
            match op {
                ProcOp::Enable => (lfo.paused, lfo.stopped) = (false, false),
                ProcOp::Disable if was => (lfo.paused, lfo.stopped) = (true, false),
                ProcOp::Reset if !repr.enabled => (lfo.paused, lfo.stopped) = (false, true),
                _ => {}
            }
        }
        if let Some(env) = &mut repr.env {
            match op {
                ProcOp::Enable => (env.paused, env.stopped) = (false, false),
                ProcOp::Disable if was => (env.paused, env.stopped) = (true, false),
                ProcOp::Reset if !repr.enabled => (env.paused, env.stopped) = (false, true),
                _ => {}
            }
        }
        Command::ProcEdit(ProcEditArgs { id, op })
    }

    /// Removes a process from the mirror and the engine. Its own clock goes
    /// with it if nothing else reads it (a named clock never goes). The
    /// owner keeps playing what it is playing.
    pub(crate) fn remove_proc(&mut self, id: ProcId) -> Command {
        self.engine_state.remove_proc_view(id)
    }
}
