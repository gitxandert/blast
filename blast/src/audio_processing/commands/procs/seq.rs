//! `seq`: retrigger a voice (or every member of a group) on steps.

use crate::audio_processing::blast_rand::{X128P, fast_seed};
use crate::audio_processing::commands::errors::{CmdErr, CmdResult};
use crate::audio_processing::commands::process::{ProcRepr, SeqRepr};
use crate::audio_processing::commands::reprs::{
    GroupId, PacketId, ProcId, ProcOwner, TempoLink, TempoOwner, VoiceId, alloc,
};
use crate::audio_processing::commands::tempo::{ClockRun, TempoChoice, TempoUse};
use crate::audio_processing::commands::{BatchArgs, CmdProcessor, Command};

/// `SeqArgs::mask` bit: the step's chance follows the seq-wide `chance=`.
pub const FOLLOWS_CHANCE: u8 = 1;
/// `SeqArgs::mask` bit: the step's early and late follow the seq-wide ones.
pub const FOLLOWS_JITTER: u8 = 2;

/// Where a step's next hit is due, once its early/late offset is drawn. The
/// engine fills these in; the command thread only sizes the Vec (one per
/// step) so the audio thread never allocates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pending {
    /// Position in the cycle, `[0, period)`.
    pub pos: f32,
    /// Cycles from the current one to the one `pos` is in; `Pending::NEVER`
    /// for a step that can't fire.
    pub wait: i8,
    /// Cycles the offset moved the hit from its nominal cycle (-1 for an
    /// early first step, 1 for a late last one).
    pub lag: i8,
}

impl Pending {
    pub const NEVER: i8 = i8::MAX;
}

/// Who a `trig` restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrigTarget {
    /// The seq's owner: a voice, or every member of a group.
    Owner,
    Voice(VoiceId),
    /// Every member of the group.
    Group(GroupId),
}

/// What a seq does on each step that fires (a receiver of its pipe). The
/// `Vec`s are built on the command thread; the audio thread only reads them.
#[derive(Clone, Debug, PartialEq)]
pub enum RecvSpec {
    Trig(TrigTarget),
    /// Sets `target`'s pitch to the next note (`notes[idx] - 60` semitones)
    /// and hits it.
    Melo {
        target: VoiceId,
        notes: Vec<f32>,
        /// The next note; advanced in place by the engine, 0 when built.
        idx: usize,
    },
}

impl RecvSpec {
    /// A `melo` receiver that starts at its first note.
    pub fn melo(target: VoiceId, notes: Vec<f32>) -> Self {
        RecvSpec::Melo {
            target,
            notes,
            idx: 0,
        }
    }

    /// The pipe of a seq written without one: it triggers its owner (G6).
    pub fn owner_trig() -> Vec<RecvSpec> {
        vec![RecvSpec::Trig(TrigTarget::Owner)]
    }

    /// The voice or group this receiver acts on, if it names one.
    pub(crate) fn names(&self) -> Option<ProcOwner> {
        match self {
            RecvSpec::Trig(TrigTarget::Voice(v)) | RecvSpec::Melo { target: v, .. } => {
                Some(ProcOwner::Voice(*v))
            }
            RecvSpec::Trig(TrigTarget::Group(g)) => Some(ProcOwner::Group(*g)),
            RecvSpec::Trig(TrigTarget::Owner) => None,
        }
    }
}

/// Arguments for `Command::SeqRecv`: a seq's receivers are replaced (a
/// pipe restated). The old `Vec` leaves through the garbage queue.
pub struct SeqRecvArgs {
    pub id: ProcId,
    pub recv: Vec<RecvSpec>,
}

/// Arguments for `Command::Seq`.
pub struct SeqArgs {
    pub proc: ProcId,
    pub owner: ProcOwner,
    pub tempo: TempoUse,
    pub period: usize,
    pub steps: Vec<f32>,
    pub chance: Vec<f32>,
    /// Four values per step, at `jit[step * 4..]`:
    /// `[early_min, early_max, late_min, late_max]` (the engine draws a hit's
    /// offset in `[-early_max, late_max]`).
    pub jit: Vec<f32>,
    /// One per step, empty values; the engine's per-step offset state. If its
    /// length isn't the number of steps, the seq's hits stay on their steps.
    pub pend: Vec<Pending>,
    /// One byte per step: which of its values follow the seq-wide ones
    /// (`FOLLOWS_*`), and so glide with them.
    pub mask: Vec<u8>,
    /// The seq-wide `[chance, early, late]` (chance a percentage, the
    /// others ticks).
    pub feel: [f32; 3],
    pub rng: X128P,
    /// What each firing step does, in pipe order.
    pub recv: Vec<RecvSpec>,
}

/// A seq-wide value on its way to a new one (REQ-074).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeelEdit {
    pub value: f32,
    /// 0 = at once.
    pub frames: u32,
}

/// Arguments for `Command::SeqEdit`: what to change in a running seq. The
/// Vecs are built on the command thread; the engine swaps them in and the
/// old ones go to the garbage queue.
pub struct SeqEditArgs {
    pub id: ProcId,
    /// Empty = the steps stay as they are (and so do `chance`, `jit` and
    /// `mask`, which are then empty too).
    pub steps: Vec<f32>,
    pub chance: Vec<f32>,
    pub jit: Vec<f32>,
    pub pend: Vec<Pending>,
    pub mask: Vec<u8>,
    /// 0 = unchanged.
    pub period: usize,
    /// Re-seeds the seq's RNG.
    pub rng: Option<X128P>,
    /// New seq-wide `[chance, early, late]`.
    pub feel: [Option<FeelEdit>; 3],
}

/// The vectors a seq edit replaced, on their way out through the garbage
/// queue.
#[derive(Default)]
pub struct SeqVecs {
    pub steps: Vec<f32>,
    pub chance: Vec<f32>,
    pub jit: Vec<f32>,
    pub pend: Vec<Pending>,
    pub mask: Vec<u8>,
    /// Receivers a pipe restatement replaced.
    pub recv: Vec<RecvSpec>,
}

/// One step of a seq as the language gives it. `at` is 0-based, in ticks of
/// the seq's clock (the language counts from 1 and subtracts); `chance` is a
/// percentage; `early` and `late` are how far the step may drift (ticks).
/// `own_chance` and `own_jit` say the step wrote its own `%` or `~`, so the
/// seq-wide settings (and their glides) don't move it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeqStep {
    pub at: f32,
    pub chance: f32,
    pub early: f32,
    pub late: f32,
    pub own_chance: bool,
    pub own_jit: bool,
}

/// A seq's step generator (EUC); the mirror keeps it so `state` prints it
/// and restating `rot=`/`grid=`/`in` regenerates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Euclid {
    pub hits: u32,
    /// Slots per cycle; `None` = one per tick of the cycle.
    pub grid: Option<u32>,
    pub rot: i32,
}

/// Written steps that were given a `grid=` or a `rot=`: the steps as written
/// (0-based slots, fractions allowed) with the grid and rotation they were
/// written with. The mirror keeps them so `state` prints them back and
/// restating `grid=`, `rot=` or `in` moves the steps.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotGen {
    /// Slots per cycle; `None` = one per beat of the cycle.
    pub grid: Option<u32>,
    pub rot: i32,
    /// The steps as written, in slot order; `at` is the slot, 0-based.
    pub steps: Vec<SeqStep>,
}

impl SlotGen {
    /// The first slot at or past the grid, if one is (a step 17 of 16 is
    /// `at` 16), for a cycle of `period` beats.
    pub fn first_outside(&self, period: u32) -> Option<f32> {
        let grid = self.grid.unwrap_or(period) as f32;
        self.steps.iter().map(|s| s.at).find(|at| *at >= grid)
    }

    /// The written steps with the seq-wide `[chance, early, late]` applied to
    /// those that wrote no mark of their own.
    pub fn slot_steps(&self, feel: [f32; 3]) -> Vec<SeqStep> {
        self.steps
            .iter()
            .map(|s| {
                let mut s = *s;
                if !s.own_chance {
                    s.chance = feel[0];
                }
                if !s.own_jit {
                    s.early = feel[1];
                    s.late = feel[2];
                }
                s
            })
            .collect()
    }

    /// The steps on the cycle of `period` beats: each slot moved `rot` slots
    /// later (wrapping within the grid), then placed at `slot * period /
    /// grid`. Sorted by position. The slots must be inside the grid
    /// (`first_outside`).
    pub fn beats(&self, period: u32, feel: [f32; 3]) -> Vec<SeqStep> {
        let grid = f64::from(self.grid.unwrap_or(period).max(1));
        let mut out: Vec<SeqStep> = self
            .slot_steps(feel)
            .into_iter()
            .map(|s| {
                let slot = (f64::from(s.at) + f64::from(self.rot)).rem_euclid(grid);
                SeqStep {
                    at: (slot * f64::from(period) / grid) as f32,
                    ..s
                }
            })
            .collect();
        out.sort_by(|a, b| a.at.total_cmp(&b.at));
        out
    }
}

/// Bjorklund: `hits` onsets spread as evenly as possible over `grid` slots,
/// the first on slot 0, then moved `rot` slots later (wrapping). Sorted
/// slot indices in `0..grid`. `hits <= grid`, both >= 1 (checked earlier).
pub fn euclid(hits: u32, grid: u32, rot: i32) -> Vec<u32> {
    let hits = hits.min(grid);
    // the pairing form: groups of onsets, groups of rests; each round glues
    // one rest group onto each onset group until at most one rest group is left
    let mut onsets: Vec<Vec<bool>> = vec![vec![true]; hits as usize];
    let mut rests: Vec<Vec<bool>> = vec![vec![false]; (grid - hits) as usize];
    while rests.len() > 1 {
        let n = onsets.len().min(rests.len());
        let leftover = if onsets.len() > n {
            onsets.split_off(n)
        } else {
            rests.split_off(n)
        };
        for (o, r) in onsets.iter_mut().zip(&rests) {
            o.extend(r);
        }
        rests = leftover;
    }
    let pattern: Vec<bool> = onsets.into_iter().chain(rests).flatten().collect();
    let mut slots: Vec<u32> = pattern
        .iter()
        .enumerate()
        .filter(|(_, on)| **on)
        .map(|(i, _)| (i as i64 + i64::from(rot)).rem_euclid(i64::from(grid)) as u32)
        .collect();
    slots.sort_unstable();
    slots
}

impl Euclid {
    /// The steps for a cycle of `period` ticks, 0-based, in order, each
    /// following the seq-wide chance and feel (`own_chance`/`own_jit` false).
    pub fn steps(self, period: u32) -> Vec<SeqStep> {
        let grid = self.grid.unwrap_or(period).max(1);
        euclid(self.hits, grid, self.rot)
            .into_iter()
            .map(|i| SeqStep {
                at: i as f32 * period as f32 / grid as f32,
                chance: 100.0,
                early: 0.0,
                late: 0.0,
                own_chance: false,
                own_jit: false,
            })
            .collect()
    }
}

/// A seq to make: the language's `seq` statement, lowered.
pub(crate) struct NewSeq {
    pub steps: Vec<SeqStep>,
    pub period: u32,
    pub explicit_period: bool,
    /// The generator the steps came from, if any.
    pub r#gen: Option<Euclid>,
    /// The written steps' grid and rotation, if they have any.
    pub slots: Option<SlotGen>,
    /// `[chance, early, late]`.
    pub feel: [f32; 3],
    pub seed: Option<u64>,
    pub recv: Vec<RecvSpec>,
    /// The packet it belongs to, if any.
    pub packet: Option<PacketId>,
}

/// What a restated seq changes (`edit_seq`).
#[derive(Default)]
pub(crate) struct SeqChange {
    pub steps: Option<Vec<SeqStep>>,
    /// The generator after the change: `Some(None)` drops it (written
    /// steps), `None` leaves it.
    pub r#gen: Option<Option<Euclid>>,
    /// Likewise for the written steps' grid and rotation.
    pub slots: Option<Option<SlotGen>>,
    pub period: Option<u32>,
    /// `in N` was written.
    pub explicit_period: bool,
    pub seed: Option<u64>,
    /// New seq-wide `[chance, early, late]` with the frames of their glides.
    pub feel: [Option<(f32, u32)>; 3],
}

/// The per-step vectors the engine holds, from the steps.
type Vectors = (Vec<f32>, Vec<f32>, Vec<f32>, Vec<Pending>, Vec<u8>);

fn vectors(steps: &[SeqStep]) -> Vectors {
    // the four-value jitter model: [early_min, early_max, late_min, late_max]
    let mut jit = Vec::with_capacity(steps.len() * 4);
    let mut mask = Vec::with_capacity(steps.len());
    for s in steps {
        jit.extend_from_slice(&[0.0, s.early, 0.0, s.late]);
        let mut bits = 0;
        if !s.own_chance {
            bits |= FOLLOWS_CHANCE;
        }
        if !s.own_jit {
            bits |= FOLLOWS_JITTER;
        }
        mask.push(bits);
    }
    (
        steps.iter().map(|s| s.at).collect(),
        steps.iter().map(|s| s.chance).collect(),
        jit,
        vec![Pending::default(); steps.len()],
        mask,
    )
}

impl CmdProcessor {
    /// Creates a seq called `name` on `owner`, a voice or a group (the
    /// language's `seq`): allocates its ID and, for an own clock, a tempo, and
    /// records it in the mirror. Nothing changes if it fails.
    pub(in crate::audio_processing::commands) fn make_seq(
        &mut self,
        owner: ProcOwner,
        name: &str,
        new: &NewSeq,
        choice: TempoChoice,
        link: TempoLink,
    ) -> CmdResult<Command> {
        let es = &self.engine_state;
        let Some(procs) = es.owner_procs(owner) else {
            return Err(CmdErr::NoItem {
                ty: match owner {
                    ProcOwner::Voice(_) => "Voice",
                    ProcOwner::Group(_) => "Group",
                    ProcOwner::Session => "Session",
                }
                .to_string(),
                name: match owner {
                    ProcOwner::Voice(v) => v.0,
                    ProcOwner::Group(g) => g.0,
                    ProcOwner::Session => 0,
                }
                .to_string(),
            });
        };
        // names are unique per owner (and a group's seq can't share a
        // member's name, since `group.name` has to mean one of them)
        let member = match owner {
            ProcOwner::Group(g) => es
                .groups
                .get(&g)
                .is_some_and(|g| g.member_names.contains_key(name)),
            ProcOwner::Voice(_) | ProcOwner::Session => false,
        };
        if procs
            .get(name)
            .is_some_and(|p| !es.hidden(crate::audio_processing::commands::AnyId::Proc(*p)))
            || member
        {
            return Err(CmdErr::AlreadyIs {
                ty: "Process".to_string(),
                name: name.to_string(),
            });
        }

        // everything is valid; allocate, undoing on failure
        let proc = alloc(&mut self.engine_state.ids.procs, "processes")?;
        let tempo = match self.alloc_tempo(choice) {
            Ok(tempo) => tempo,
            Err(e) => {
                self.engine_state.ids.procs.free(proc);
                return Err(e);
            }
        };

        // the engine starts a new own clock at once if the owner is playing
        let run = match self.engine_state.seq_starts(owner) {
            true => ClockRun::Running,
            false => ClockRun::Stopped,
        };
        self.insert_tempo(tempo, TempoOwner::from(owner), run);
        let es = &mut self.engine_state;
        es.procs.insert(
            proc,
            ProcRepr {
                id: proc,
                name: name.to_string(),
                owner,
                tempo: tempo.id(),
                link,
                enabled: true,
                packet: new.packet,
                seq: SeqRepr {
                    steps: new.steps.clone(),
                    period: new.period,
                    explicit_period: new.explicit_period,
                    seed: new.seed,
                    feel: new.feel,
                    recv: new.recv.clone(),
                    r#gen: new.r#gen,
                    slots: new.slots.clone(),
                },
                lfo: None,
                env: None,
            },
        );
        if let Some(procs) = es.owner_procs_mut(owner) {
            procs.insert(name.to_string(), proc);
        }

        let (steps, chance, jit, pend, mask) = vectors(&new.steps);
        Ok(Command::Seq(SeqArgs {
            proc,
            owner,
            tempo,
            period: new.period as usize,
            steps,
            chance,
            jit,
            pend,
            mask,
            feel: new.feel,
            rng: X128P::new(new.seed.unwrap_or_else(fast_seed)),
            recv: new.recv.clone(),
        }))
    }

    /// Replaces a seq's receivers in the mirror and returns the `SeqRecv`
    /// that does the same in the engine. The engine gets its own copy (built
    /// here, on the command thread).
    pub(in crate::audio_processing::commands) fn set_recv(
        &mut self,
        id: ProcId,
        recv: Vec<RecvSpec>,
    ) -> Command {
        let Some(repr) = self.engine_state.procs.get_mut(&id) else {
            return Command::Batch(BatchArgs { cmds: Vec::new() });
        };
        repr.seq.recv = recv.clone();
        Command::SeqRecv(SeqRecvArgs { id, recv })
    }

    /// Changes a seq in the mirror and returns the `SeqEdit` that does the
    /// same in the engine, without restarting it. A seq-wide value that
    /// changes moves the steps that follow it (the mirror holds the glide's
    /// target).
    pub(in crate::audio_processing::commands) fn edit_seq(
        &mut self,
        id: ProcId,
        change: SeqChange,
    ) -> Command {
        let Some(repr) = self.engine_state.procs.get_mut(&id) else {
            return Command::Batch(BatchArgs { cmds: Vec::new() });
        };
        let seq = &mut repr.seq;

        let (mut steps, mut chance, mut jit, mut pend, mut mask) = Default::default();
        if let Some(new) = change.steps {
            (steps, chance, jit, pend, mask) = vectors(&new);
            seq.steps = new;
        }
        if let Some(g) = change.r#gen {
            seq.r#gen = g;
        }
        if let Some(s) = change.slots {
            seq.slots = s;
        }
        let mut period = 0;
        if let Some(p) = change.period {
            seq.period = p;
            period = p as usize;
        }
        seq.explicit_period |= change.explicit_period;

        let mut rng = None;
        if let Some(s) = change.seed {
            seq.seed = Some(s);
            rng = Some(X128P::new(s));
        }

        let mut feel = [None; 3];
        for (i, f) in change.feel.iter().enumerate() {
            if let Some((value, frames)) = *f {
                seq.feel[i] = value;
                feel[i] = Some(FeelEdit { value, frames });
            }
        }
        // the mirror's steps hold the values their seq-wide setting gives
        let [c, e, l] = seq.feel;
        for s in &mut seq.steps {
            if !s.own_chance {
                s.chance = c;
            }
            if !s.own_jit {
                s.early = e;
                s.late = l;
            }
        }

        Command::SeqEdit(SeqEditArgs {
            id,
            steps,
            chance,
            jit,
            pend,
            mask,
            period,
            rng,
            feel,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::audio_processing::blast_time::blast_time::TempoUnit;
    use crate::audio_processing::commands::lang::Code;
    use crate::audio_processing::commands::testing::*;
    use crate::audio_processing::commands::*;

    fn proc_link(cp: &CmdProcessor, v: &str, p: &str) -> TempoLink {
        let es = &cp.engine_state;
        es.procs[&es.voices[&vid(cp, v)].procs[p]].link
    }

    #[test]
    fn seq_voice_tempo_points_at_its_voice() {
        let mut cp = processor(&["kick", "snare"]);
        ok(&mut cp, "voice kick from \"kick\" on 1000ms");
        ok(&mut cp, "voice snare from \"snare\" on 120b");
        let t = seq_tempo(ok(&mut cp, "snare { seq s 1 }"));
        let snare = &cp.engine_state.voices[&vid(&cp, "snare")];
        assert_eq!(t, TempoUse::Shared(snare.tempo));
        assert_eq!(t, TempoUse::Shared(TempoId(1)));
        assert_eq!(proc_link(&cp, "snare", "s"), TempoLink::Voice);
    }

    #[test]
    fn seq_voice_tempo_survives_unload_of_others() {
        let mut cp = processor(&["kick", "snare"]);
        ok(&mut cp, "voice kick from \"kick\" on 1000ms");
        ok(&mut cp, "voice snare from \"snare\" on 120b");
        ok(&mut cp, "remove kick");
        let t = seq_tempo(ok(&mut cp, "snare { seq s 1 }"));
        assert_eq!(t, TempoUse::Shared(TempoId(1)));
        assert_eq!(
            cp.engine_state.tempos[&TempoId(1)].owner,
            TempoOwner::Voice(vid(&cp, "snare"))
        );
    }

    #[test]
    fn seq_on_a_voice_that_follows_main_reads_main() {
        let mut cp = booted(&["kick", "snare"]);
        ok(&mut cp, "voice kick from \"kick\"");
        ok(&mut cp, "voice snare from \"snare\"");
        let t = seq_tempo(ok(&mut cp, "snare { seq s 1 }"));
        assert_eq!(t, TempoUse::Shared(TempoId(0)));
        assert_eq!(proc_link(&cp, "snare", "s"), TempoLink::Voice);
    }

    #[test]
    fn seq_context_tempo_points_at_context() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "clock c1 120b");
        ok(&mut cp, "clock c2 90b");
        ok(&mut cp, "voice a from \"a\" on 1000ms");
        let t = seq_tempo(ok(&mut cp, "a { seq s 1 on c2 }"));
        assert_eq!(t, TempoUse::Shared(ctx(&cp, "c2")));
        assert_eq!(t, TempoUse::Shared(TempoId(1)));
        assert_eq!(proc_link(&cp, "a", "s"), TempoLink::Context);
    }

    #[test]
    fn group_tempo_refs_point_at_their_group() {
        let mut cp = processor(&["a", "b", "c", "d"]);
        for n in ["a", "b", "c"] {
            ok(&mut cp, &format!("voice {n} from \"{n}\" on 1000ms"));
        }
        ok(&mut cp, "group g1 on 240b { a }");
        ok(&mut cp, "group g2 on 240b { b }");
        let g1 = cp.engine_state.groups[&gid(&cp, "g1")].tempo;
        let g2 = cp.engine_state.groups[&gid(&cp, "g2")].tempo;
        assert_ne!(g1, g2);

        let t = seq_tempo(ok(&mut cp, "c { seq s 1 on g2 }"));
        assert_eq!(t, TempoUse::Shared(g2));
        assert_eq!(proc_link(&cp, "c", "s"), TempoLink::Group(gid(&cp, "g2")));

        let args = load_args(ok(&mut cp, "voice d from \"d\" on g1"));
        assert_eq!(args.tempo, TempoUse::Shared(g1));
        assert!(!args.adopts);
        assert_eq!(
            cp.engine_state.voices[&vid(&cp, "d")].link,
            TempoLink::Group(gid(&cp, "g1"))
        );
    }

    #[test]
    fn seq_targets_group_voice() {
        let mut cp = grouped(&["a", "b"], "group g on 240b { a; b }");
        match ok(&mut cp, "g.b { seq s 1 }") {
            Command::Seq(args) => {
                assert_eq!(args.owner, ProcOwner::Voice(vid(&cp, "g.b")));
                assert_eq!(args.owner, ProcOwner::Voice(VoiceId(1)));
            }
            _ => panic!("not a Seq"),
        }
        let gv = &cp.engine_state.voices[&vid(&cp, "g.b")];
        assert_eq!(gv.procs.len(), 1);
    }

    #[test]
    fn euclid_known_patterns() {
        use super::euclid;
        assert_eq!(euclid(1, 4, 0), [0]);
        assert_eq!(euclid(3, 8, 0), [0, 3, 6]);
        assert_eq!(euclid(5, 8, 0), [0, 2, 3, 5, 6]);
        assert_eq!(euclid(4, 4, 0), [0, 1, 2, 3]);
        assert_eq!(euclid(2, 5, 0), [0, 2]);
        assert_eq!(euclid(3, 7, 0), [0, 2, 4]);
        for n in 1..=32u32 {
            for k in 1..=n {
                let slots = euclid(k, n, 0);
                assert_eq!(slots.len(), k as usize, "E({k},{n})");
                assert_eq!(slots[0], 0, "E({k},{n})");
                // gaps, counting the wrap to the next cycle
                let gaps: Vec<u32> = (0..slots.len())
                    .map(|i| match slots.get(i + 1) {
                        Some(next) => next - slots[i],
                        None => n - slots[i],
                    })
                    .collect();
                let (lo, hi) = (gaps.iter().min().unwrap(), gaps.iter().max().unwrap());
                assert!(hi - lo <= 1, "E({k},{n}) gaps {gaps:?}");
            }
        }
    }

    #[test]
    fn euclid_rotates_later_and_wraps() {
        use super::euclid;
        assert_eq!(euclid(3, 8, 1), [1, 4, 7]);
        assert_eq!(euclid(3, 8, -1), [2, 5, 7]);
        assert_eq!(euclid(3, 8, 8), euclid(3, 8, 0));
        assert_eq!(euclid(3, 8, -8), euclid(3, 8, 0));
    }

    #[test]
    fn steps_scale_to_period() {
        use super::Euclid;
        let steps = Euclid {
            hits: 5,
            grid: Some(16),
            rot: 0,
        }
        .steps(4);
        let at: Vec<f32> = steps.iter().map(|s| s.at).collect();
        assert_eq!(at, [0.0, 0.75, 1.5, 2.25, 3.0]);
        assert!(steps.iter().all(|s| s.chance == 100.0 && !s.own_chance));
    }

    // a mirror with one voice, kick, that has a clock of its own (tempo 0)
    fn kick_session() -> CmdProcessor {
        let mut cp = processor(&["kick"]);
        ok(&mut cp, "voice kick from \"kick\" on 1000ms");
        cp
    }

    fn seq(cp: &mut CmdProcessor, line: &str) -> CmdResult<SeqArgs> {
        match cp.parse(line)? {
            Parsed::Engine(Command::Seq(args)) => Ok(args),
            _ => panic!("'{}' didn't parse to Command::Seq", line),
        }
    }

    fn seq_ok(line: &str) -> SeqArgs {
        match seq(&mut kick_session(), line) {
            Ok(args) => args,
            Err(e) => panic!("'{}' failed: {}", line, e),
        }
    }

    fn seq_code(line: &str) -> Code {
        match seq(&mut kick_session(), line) {
            Ok(_) => panic!("'{}' should have failed", line),
            Err(CmdErr::Lang(errs)) => errs[0].code,
            Err(e) => panic!("'{}' failed with a family error: {}", line, e),
        }
    }

    fn kick(cp: &CmdProcessor) -> &VoiceRepr {
        &cp.engine_state.voices[&VoiceId(0)]
    }

    // tempos owned by kick's processes (all but kick's own)
    fn proc_tempos(cp: &CmdProcessor) -> usize {
        cp.engine_state.tempos.len() - 1
    }

    #[test]
    fn seq_default_tempo_is_the_voices() {
        let args = seq_ok("kick { seq s 1 2 }");
        assert_eq!(args.owner, ProcOwner::Voice(VoiceId(0)));
        assert_eq!(args.tempo, TempoUse::Shared(TempoId(0)));
    }

    #[test]
    fn seq_own_tempo() {
        let args = seq_ok("kick { seq s 1 2 on 120b }");
        assert_eq!(
            args.tempo,
            TempoUse::Own(TempoInit {
                id: TempoId(1),
                unit: TempoUnit::Bpm,
                interval: 120.0
            })
        );
    }

    #[test]
    fn seq_clock_forms() {
        let args = seq_ok("kick { seq s 1 on 250ms }");
        let TempoUse::Own(t) = args.tempo else {
            panic!("not an own tempo")
        };
        assert_eq!(t.unit, TempoUnit::Millis);
        assert_eq!(t.interval, 250.0);

        assert_eq!(seq_ok("kick { seq s 1 on 48000f }").steps, vec![0.0]);
        assert_eq!(seq_code("kick { seq s 1 on x1 }"), Code::UnknownName);
        assert_eq!(seq_code("kick { seq s 1 on 0b }"), Code::OutOfRange);
    }

    #[test]
    fn seq_period_and_steps() {
        let args = seq_ok("kick { seq s 1 2.5 4 in 8 }");
        assert_eq!(args.period, 8);
        assert_eq!(args.steps, vec![0.0, 1.5, 3.0]);
        assert_eq!(args.chance.len(), 3);
        assert_eq!(args.jit.len(), 3 * 4);

        // a seq takes its voice's period (4 unless it states one)
        assert_eq!(seq_ok("kick { seq s 1 }").period, 4);

        // these are not numbers or not steps at all
        for line in ["kick { seq s 1 in x }", "kick { seq s 1 x }"] {
            assert!(seq(&mut kick_session(), line).is_err(), "{line}");
        }
    }

    #[test]
    fn seq_unknown_owner() {
        assert_eq!(seq_code("seq snare.s 1"), Code::UnknownName);
        assert_eq!(seq_code("snare { seq s 1 }"), Code::UnknownName);
        assert!(seq(&mut kick_session(), "seq").is_err());
    }

    #[test]
    fn seq_requires_steps() {
        assert_eq!(seq_code("kick { seq s on 120b }"), Code::MissingItem);
        assert_eq!(seq_code("kick { seq s }"), Code::MissingItem);

        // a rejected seq leaves the mirror untouched
        let mut cp = kick_session();
        assert!(seq(&mut cp, "kick { seq s on 120b }").is_err());
        assert_eq!(kick(&cp).procs.len(), 0);
        assert_eq!(proc_tempos(&cp), 0);
        assert_eq!(cp.engine_state.ids.procs.live(), 0);
    }

    #[test]
    fn seq_rejects_zero_period() {
        assert_eq!(seq_code("kick { seq s 1 in 0 }"), Code::OutOfRange);
        assert_eq!(seq_ok("kick { seq s 1 in 1 }").period, 1);
    }

    #[test]
    fn seq_chance_marks() {
        assert_eq!(seq_ok("kick { seq s 1 2 3 }").chance, vec![100.0; 3]);
        assert_eq!(
            seq_ok("kick { seq s 1 2 3%50 }").chance,
            vec![100.0, 100.0, 50.0]
        );
        assert_eq!(
            seq_ok("kick { seq s 1%50 2 3 }").chance,
            vec![50.0, 100.0, 100.0]
        );
        assert_eq!(
            seq_ok("kick { seq s 1%10 2%20 3 }").chance,
            vec![10.0, 20.0, 100.0]
        );
        assert_eq!(seq_ok("kick { seq s 1 1.5%25 }").chance, vec![100.0, 25.0]);
        assert_eq!(seq_ok("kick { seq s 1%0 2%100 }").chance, vec![0.0, 100.0]);
        assert_eq!(seq_code("kick { seq s 1 2 3%101 }"), Code::OutOfRange);
        assert_eq!(seq_code("kick { seq s 1%-1 }"), Code::OutOfRange);
        assert_eq!(seq_code("kick { seq s 5%50 }"), Code::StepOutsideCycle);
        assert!(seq(&mut kick_session(), "kick { seq s 1%x }").is_err());
    }

    #[test]
    fn seq_chance_range() {
        let s = "kick { seq s";
        assert_eq!(
            seq_ok(&format!("{s} 1 2..4%20 }}")).chance,
            vec![100.0, 20.0, 20.0, 20.0]
        );
        assert_eq!(
            seq_ok(&format!("{s} 1..2%20 3 4 }}")).chance,
            vec![20.0, 20.0, 100.0, 100.0]
        );
        assert_eq!(
            seq_ok(&format!("{s} 3..4%20 1 2 }}")).chance,
            vec![20.0, 20.0, 100.0, 100.0]
        );
        assert_eq!(
            seq_ok(&format!("{s} 1 2..2%20 3 4 }}")).chance,
            vec![100.0, 20.0, 100.0, 100.0]
        );
        assert_eq!(seq_code(&format!("{s} 5..9%20 }}")), Code::StepOutsideCycle);
        assert_eq!(seq_code(&format!("{s} 3..1%20 }}")), Code::WrongType);
    }

    #[test]
    fn seq_jitter_default_and_none() {
        assert_eq!(seq_ok("kick { seq s 1 2 }").jit, vec![0.0; 8]);
        assert_eq!(seq_ok("kick { seq s 1~0 2~0 }").jit, vec![0.0; 8]);
    }

    #[test]
    fn seq_jitter_values() {
        // jit[step * 4..] = [early_min, early_max, late_min, late_max]
        // a one-sided feel is a seq setting
        assert_eq!(
            seq_ok("kick { seq s 1 2 early=5 late=10 }").jit,
            vec![0.0, 5.0, 0.0, 10.0, 0.0, 5.0, 0.0, 10.0]
        );
        assert_eq!(
            seq_ok("kick { seq s 1~5 2 }").jit,
            vec![0.0, 5.0, 0.0, 5.0, 0.0, 0.0, 0.0, 0.0]
        );
        assert_eq!(
            seq_ok("kick { seq s 1~3 2~2 }").jit,
            vec![0.0, 3.0, 0.0, 3.0, 0.0, 2.0, 0.0, 2.0]
        );
        // both marks, in either order
        let args = seq_ok("kick { seq s 1~5 2%50~5 }");
        assert_eq!(args.chance, vec![100.0, 50.0]);
        assert_eq!(args.jit, vec![0.0, 5.0, 0.0, 5.0, 0.0, 5.0, 0.0, 5.0]);
    }

    #[test]
    fn seq_jitter_on_ranges() {
        let v = [0.0f32, 5.0, 0.0, 5.0];
        let none = [0.0f32; 4];
        let cat = |a: [[f32; 4]; 3]| a.concat();
        let j = |s: &str| seq_ok(&format!("kick {{ seq s {s} }}")).jit;

        assert_eq!(j("1 2~5 3"), cat([none, v, none]));
        assert_eq!(j("1..3~5"), cat([v, v, v]));
        assert_eq!(j("1 2..3~5"), cat([none, v, v]));
        assert_eq!(j("1~5 2 3~5"), cat([v, none, v]));

        assert_eq!(seq_code("kick { seq s 7~5 }"), Code::StepOutsideCycle);
        assert_eq!(seq_code("kick { seq s 5..9~5 }"), Code::StepOutsideCycle);
    }

    #[test]
    fn seq_jitter_errors() {
        assert_eq!(seq_code("kick { seq s 1~-1 }"), Code::OutOfRange);
        // the range form is gone
        assert_eq!(seq_code("kick { seq s 1~0..1 }"), Code::Unexpected);
        assert!(seq(&mut kick_session(), "kick { seq s 1~x }").is_err());
    }

    #[test]
    fn seq_names_are_per_owner() {
        let mut cp = kick_session();
        for n in ["a", "b", "c"] {
            ok(&mut cp, &format!("kick {{ seq {n} 1 on 120b }}"));
        }
        assert_eq!(kick(&cp).procs.len(), 3);
        assert_eq!(kick(&cp).procs["a"].index(), 0);
        assert_eq!(kick(&cp).procs["b"].index(), 1);
        assert_eq!(kick(&cp).procs["c"].index(), 2);
        // each owns a tempo
        assert_eq!(proc_tempos(&cp), 3);
        // another voice may use the same names
        ok(&mut cp, "voice other from \"kick\" on 1000ms");
        ok(&mut cp, "other { seq a 1 }");
        assert_eq!(cp.engine_state.voices[&vid(&cp, "other")].procs.len(), 1);
    }

    #[test]
    fn seq_named_twice() {
        let mut cp = kick_session();
        ok(&mut cp, "kick { seq hats 1 }");
        ok(&mut cp, "kick { seq beat 2 }");
        assert_eq!(kick(&cp).procs["hats"].index(), 0);
        assert_eq!(kick(&cp).procs["beat"].index(), 1);

        // the same statement again changes nothing; another edits it
        assert!(local(&mut cp, "kick { seq hats 1 }").contains("W101"));
        assert!(matches!(
            ok(&mut cp, "kick { seq hats 2 }"),
            Command::SeqEdit(_)
        ));
        assert_eq!(
            cp.engine_state.procs[&kick(&cp).procs["hats"]].seq.steps[0].at,
            1.0
        );
        // nothing was added to the mirror
        assert_eq!(kick(&cp).procs.len(), 2);
        assert_eq!(cp.engine_state.procs.len(), 2);
        assert_eq!(proc_tempos(&cp), 0);
    }

    #[test]
    fn seq_steps_keep_the_written_order() {
        let args = seq_ok("kick { seq s 4 1%40 }");
        assert_eq!(args.steps, vec![3.0, 0.0]);
        assert_eq!(args.chance, vec![100.0, 40.0]);
    }
}
