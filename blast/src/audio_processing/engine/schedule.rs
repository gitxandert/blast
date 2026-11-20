//! Lines that wait for the beat (SCHED): `start lead at 16`,
//! `kick r=2 over 1 after 4`. The command thread has already checked
//! everything, including that the table has room (the mirror counts the
//! pending entries against `Limits::scheduled`), so nothing here refuses:
//! a command that arrives is held, and applied when its clock reaches the
//! due position. Nothing allocates or frees on the audio thread: the table
//! is preallocated, and a retired command's box leaves as garbage.
//!
//! `then` and `thenif` (PKa) are entries too, but due on a frame of the engine
//! (`Due::Frame`): the frame they were applied on plus the longest glide
//! started since `MarkGlides`. A `thenif` also remembers the generation of
//! each glide it watches and is dropped, with `Event::ChainDropped`, if any
//! changed. An entry may hold more `Schedule`s; those arrive when it fires,
//! and are counted as retired if it is dropped instead.
//!
//! Timed scripts (PKb) add two things. A script slot has a **mark**: where
//! its counting clock was when the last segment fired (`SetMark`). A segment
//! is an entry due `AfterMark`: once the slot's mark has the segment's number,
//! `ticks` of the clock after the position the mark recorded. So waits add
//! up from where each segment really fired. Entries carry the slot as a tag;
//! `CancelScript` drops every entry with it.
//!
//! A line whose object is removed while it waits is dropped, not applied:
//! the ID could name another object by the time the line fires. Applying a
//! removal sweeps the table (`sweep_waiting`); the walk that finds what a
//! command names is `commands::visit_ids`.
//!
//! Staged creation (PKb2): an entry may make objects (the mirror gave the IDs).
//! A name that does not exist yet is not "gone" if an entry makes it: the
//! entry itself or the tree it holds, always; for the sweep, any entry in the
//! table. At fire time the rule is stricter (only the entry's own tree), so
//! a line due before the one that makes its object is dropped, not applied
//! to nothing. `ScheduleFired.applied` tells the mirror which entries ran.

use crate::audio_processing::commands::*;
use crate::audio_processing::engine::{Conductor, Event, Garbage};

/// When an entry is due: a position of a clock (in ticks), or a frame of the
/// engine (`then`, `thenif`).
#[derive(Clone, Copy, Debug)]
enum Due {
    Ticks {
        tempo: TempoId,
        due: f64,
    },
    Frame(u64),
    /// A segment: waits for mark `seq` of script `slot`, then `ticks` of
    /// `tempo` after the position that mark recorded.
    Mark {
        slot: u8,
        seq: u32,
        tempo: TempoId,
        ticks: f64,
    },
}

/// No mark set: what a slot holds before its script starts, and after it is
/// cancelled. An entry of a script whose slot has no mark is refused, so
/// segments that were already on their way when the script was cancelled
/// never run.
const NO_MARK: u32 = u32::MAX;

/// The IDs a tree of commands makes, kept on the stack (the audio thread
/// allocates nothing). Past its room it says everything is made, which only
/// makes the checks that use it more lenient.
struct Made {
    ids: [Option<AnyId>; 64],
    n: usize,
    overflow: bool,
}

impl Made {
    fn of(cmd: &Command) -> Made {
        let mut made = Made {
            ids: [None; 64],
            n: 0,
            overflow: false,
        };
        created_ids(cmd, &mut |id| {
            if made.n < made.ids.len() {
                made.ids[made.n] = Some(id);
                made.n += 1;
            } else {
                made.overflow = true;
            }
        });
        made
    }

    fn has(&self, id: AnyId) -> bool {
        self.overflow || self.ids[..self.n].contains(&Some(id))
    }
}

/// Where a script slot's counting clock was when its last segment fired.
#[derive(Clone, Copy, Debug)]
struct Mark {
    seq: u32,
    steps: f64,
}

/// A command waiting for its due position.
struct Pending {
    due: Due,
    /// Arrival number, so entries due together apply in the order they came.
    order: u64,
    /// The number the command thread gave it, repeated in `ScheduleFired`.
    seq_no: u64,
    /// For `thenif`: the glides that must run their course.
    watch: Option<Box<[GlideWatch]>>,
    /// The script slot the entry belongs to (for `CancelScript`).
    script: Option<u8>,
    cmd: Box<Command>,
}

/// The table of waiting commands: a fixed number of slots.
pub struct Schedule {
    slots: Vec<Option<Pending>>,
    live: usize,
    next_order: u64,
    /// Entries retired so far, applied or dropped (for `ScheduleFired`).
    retired: u64,
    /// One mark per script slot; allocated now, never grown.
    marks: Vec<Mark>,
}

impl Schedule {
    /// A table of `capacity` slots and `scripts` marks, allocated now.
    pub fn with_capacity(capacity: usize, scripts: usize) -> Self {
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, || None);
        let mark = Mark {
            seq: NO_MARK,
            steps: 0.0,
        };
        Schedule {
            slots,
            live: 0,
            next_order: 0,
            retired: 0,
            marks: vec![mark; scripts],
        }
    }

    /// How many entries are waiting.
    #[cfg(test)]
    pub fn waiting(&self) -> usize {
        self.live
    }

    // Puts `pending` in a free slot; hands it back if there is none.
    fn insert(&mut self, mut pending: Pending) -> Result<(), Pending> {
        let Some(slot) = self.slots.iter_mut().find(|s| s.is_none()) else {
            return Err(pending);
        };
        pending.order = self.next_order;
        self.next_order += 1;
        *slot = Some(pending);
        self.live += 1;
        Ok(())
    }
}

// what `mem::replace` leaves in a fired box: no allocation, applies as nothing
fn empty() -> Command {
    Command::Batch(BatchArgs { cmds: Vec::new() })
}

impl Conductor {
    /// Resolves the due position and holds the command. "At" is never
    /// "now": on an exact multiple it waits a whole `every`. After glides, it
    /// is the frame the longest of them started since `MarkGlides` is over.
    pub(crate) fn schedule_command(&mut self, args: ScheduleArgs) {
        let ScheduleArgs {
            when,
            cmd,
            seq_no,
            script,
        } = args;
        if let Some(slot) = script
            && self
                .schedule
                .marks
                .get(usize::from(slot))
                .is_none_or(|m| m.seq == NO_MARK)
        {
            return self.refuse(seq_no, cmd);
        }
        let (due, watch) = match when {
            WhenSpec::At { tempo, every } => {
                let Some(steps) = self.tempo_state(tempo).map(|t| t.steps()) else {
                    return self.missing_clock(tempo, seq_no, cmd);
                };
                let due = (steps / every).ceil() * every;
                let due = if due <= steps { due + every } else { due };
                (Due::Ticks { tempo, due }, None)
            }
            WhenSpec::After { tempo, ticks } => {
                let Some(steps) = self.tempo_state(tempo).map(|t| t.steps()) else {
                    return self.missing_clock(tempo, seq_no, cmd);
                };
                (
                    Due::Ticks {
                        tempo,
                        due: steps + ticks,
                    },
                    None,
                )
            }
            WhenSpec::AfterGlides { mut watch } => {
                // the generations the glides have now: a later one means the
                // glide was replaced
                if let Some(list) = watch.as_mut() {
                    for w in list.iter_mut() {
                        w.generation = self.glide_generation(w.glide).unwrap_or(0);
                    }
                }
                (Due::Frame(self.frame + u64::from(self.glide_mark)), watch)
            }
            WhenSpec::AfterMark {
                slot,
                seq,
                tempo,
                ticks,
            } => {
                if usize::from(slot) >= self.schedule.marks.len() {
                    return self.refuse(seq_no, cmd);
                }
                (
                    Due::Mark {
                        slot,
                        seq,
                        tempo,
                        ticks,
                    },
                    None,
                )
            }
        };
        let pending = Pending {
            due,
            order: 0,
            seq_no,
            watch,
            script,
            cmd,
        };
        if let Err(pending) = self.schedule.insert(pending) {
            // the mirror counts entries against the table, so it has room
            debug_assert!(false, "schedule table full");
            self.drop_scheduled(pending);
        }
    }

    // its clock is gone: nothing to wait on
    fn missing_clock(&mut self, tempo: TempoId, seq_no: u64, cmd: Box<Command>) {
        debug_assert!(false, "schedule on a missing tempo {tempo:?}");
        self.refuse(seq_no, cmd);
    }

    // an entry that can't wait: retired unapplied, with what it nested
    fn refuse(&mut self, seq_no: u64, cmd: Box<Command>) {
        let pending = Pending {
            due: Due::Frame(0),
            order: 0,
            seq_no,
            watch: None,
            script: None,
            cmd,
        };
        self.drop_scheduled(pending);
    }

    /// `SetMark`: script `slot` reached segment `seq`. The mark is where
    /// `tempo` is now. If the entry that is applying was itself counting on
    /// that clock and fired on time, it is the position it was due at, so
    /// that waits add up without losing the part of a frame the entry
    /// overshot by; one that fired late counts from now.
    pub(crate) fn set_mark(&mut self, args: SetMarkArgs) {
        let SetMarkArgs { slot, seq, tempo } = args;
        let Some(state) = self.tempo_state(tempo) else {
            // the clock is gone: nothing counts on it, and the next segment
            // is dropped when it finds that out
            if let Some(m) = self.schedule.marks.get_mut(usize::from(slot)) {
                *m = Mark { seq, steps: 0.0 };
            }
            return;
        };
        let now = state.steps();
        let slack = 1.5 / f64::from(state.interval).max(1.0);
        let steps = match self.firing {
            Some((t, due)) if t == tempo && now - due <= slack => due,
            _ => now,
        };
        if let Some(m) = self.schedule.marks.get_mut(usize::from(slot)) {
            *m = Mark { seq, steps };
        }
    }

    /// `CancelScript`: drops every waiting entry tagged with `slot`, clears
    /// its mark, and stops what the script started.
    pub(crate) fn cancel_script(&mut self, args: CancelScriptArgs) {
        let CancelScriptArgs { slot, mut stops } = args;
        if let Some(m) = self.schedule.marks.get_mut(usize::from(slot)) {
            m.seq = NO_MARK;
        }
        for i in 0..self.schedule.slots.len() {
            if self.schedule.slots[i]
                .as_ref()
                .is_some_and(|p| p.script == Some(slot))
                && let Some(pending) = self.schedule.slots[i].take()
            {
                self.schedule.live -= 1;
                self.drop_scheduled(pending);
            }
        }
        // what the script started and still exists; the rest is skipped
        // (the mirror checked when it lowered, but a removal may have come
        // since)
        for stop in stops.drain(..) {
            let alive = match &stop {
                Command::Stop(a) => match a.target {
                    Target::Voice(v) => self.exists(AnyId::Voice(v)),
                    Target::Group(g) => self.exists(AnyId::Group(g)),
                    Target::Tempo(t) => self.exists(AnyId::Tempo(t)),
                },
                _ => false,
            };
            if alive {
                self.apply(stop);
            }
        }
        // the emptied Vec still owns its buffer
        self.retire(Garbage::Commands(stops));
    }

    /// The generation of the slot a glide runs in, or `None` if its object
    /// is gone.
    fn glide_generation(&self, glide: GlideRef) -> Option<u32> {
        match glide {
            GlideRef::Voice { voice, param } => self
                .voices
                .get(voice.index())
                .map(|v| v.state.glide_generation(param)),
            GlideRef::Group(g) => self.groups.get(g.index()).map(|g| g.state.generation),
            GlideRef::Tempo(t) => self.tempos.get(t.index()).map(|t| t.glide_gen),
            GlideRef::Fx { owner, id, param } => match owner {
                FxOwner::Voice(v) => self
                    .voices
                    .get(v.index())
                    .and_then(|v| v.fx.generation(id, param)),
                FxOwner::Group(g) => self
                    .groups
                    .get(g.index())
                    .and_then(|g| g.fx.generation(id, param)),
            },
            GlideRef::Wave { voice, id, param } => self
                .voices
                .get(voice.index())
                .and_then(|v| v.waves.generation(id, param)),
            GlideRef::Proc(p) => self.procs.get(p.index()).map(|s| s.generation),
        }
    }

    // the index of the first watched glide that was replaced, or `u16::MAX`
    // if its object is gone; `None` if all ran their course
    fn cut_short(&self, watch: &[GlideWatch]) -> Option<u16> {
        watch
            .iter()
            .enumerate()
            .find_map(|(i, w)| match self.glide_generation(w.glide) {
                Some(g) if g == w.generation => None,
                Some(_) => Some(u16::try_from(i).unwrap_or(u16::MAX - 1)),
                None => Some(u16::MAX),
            })
    }

    /// Applies every entry that is due, in arrival order, and retires its
    /// box. Costs nothing while the table is empty.
    pub(crate) fn fire_due(&mut self) {
        if self.schedule.live == 0 {
            return;
        }
        // re-evaluated after each, since an applied `start` moves a clock,
        // and an applied entry may add more that are due now
        while let Some(i) = self.next_due() {
            let Some(mut pending) = self.schedule.slots[i].take() else {
                break;
            };
            self.schedule.live -= 1;
            // the sweep leaves nothing dangling; this is the last guard
            if pending.watch.is_none() && holds_no_statement(&pending.cmd) {
                let made = Made::of(&pending.cmd);
                self.neutralise(&mut pending.cmd, &made, false);
            }
            let stale = self.names_missing(&pending, false);
            let cut = match (&pending.watch, stale) {
                (Some(watch), false) => self.cut_short(watch),
                (Some(_), true) => Some(u16::MAX),
                (None, _) => None,
            };
            if let Some(watch) = cut {
                self.emit(Event::ChainDropped {
                    seq_no: pending.seq_no,
                    watch,
                });
            } else if !stale {
                let taken = std::mem::replace(&mut *pending.cmd, empty());
                self.firing = self.due_position(&pending.due);
                self.apply(taken);
                self.firing = None;
                self.drop_fired(pending);
                continue;
            }
            self.drop_scheduled(pending);
        }
    }

    /// Drops every waiting entry that names an object that no longer
    /// exists, unapplied. Runs after a command that removed something, so
    /// an ID the command freed hasn't been handed out again yet. Costs
    /// nothing while the table is empty; never allocates.
    pub(crate) fn sweep_waiting(&mut self) {
        if self.schedule.live == 0 {
            return;
        }
        for i in 0..self.schedule.slots.len() {
            let Some(mut pending) = self.schedule.slots[i].take() else {
                continue;
            };
            // an entry with no statement of its own (a head, a tail, a wait)
            // outlives a missing object: only the statements nested in it
            // that name it go
            if pending.watch.is_none() && holds_no_statement(&pending.cmd) {
                let made = Made::of(&pending.cmd);
                self.neutralise(&mut pending.cmd, &made, true);
            }
            if self.names_missing(&pending, true) {
                self.schedule.live -= 1;
                self.drop_scheduled(pending);
            } else {
                self.schedule.slots[i] = Some(pending);
            }
        }
    }

    // inside a statement-free box: replaces each nested entry that names a
    // missing object by an empty batch, retiring it and what it nested.
    // Nothing is freed here; the entry's box and watch leave as garbage.
    //
    // `made` is what the whole tree makes: a name it makes isn't gone yet,
    // even if a sibling entry that names it comes before the one that makes it
    // in the tree. With `lenient`, so is a name that any entry in the table
    // makes (the sweep runs while the maker still waits).
    fn neutralise(&mut self, cmd: &mut Command, made: &Made, lenient: bool) {
        match cmd {
            Command::Batch(a) => {
                for c in a.cmds.iter_mut() {
                    self.neutralise(c, made, lenient);
                }
            }
            Command::Schedule(a) => {
                let free = matches!(
                    a.when,
                    WhenSpec::AfterGlides { watch: None }
                        | WhenSpec::At { .. }
                        | WhenSpec::After { .. }
                        | WhenSpec::AfterMark { .. }
                ) && holds_no_statement(&a.cmd);
                if free {
                    let own_clock_gone = match a.when {
                        WhenSpec::At { tempo, .. }
                        | WhenSpec::After { tempo, .. }
                        | WhenSpec::AfterMark { tempo, .. } => self.tempo_state(tempo).is_none(),
                        WhenSpec::AfterGlides { .. } => false,
                    };
                    if !own_clock_gone {
                        self.neutralise(&mut a.cmd, made, lenient);
                        return;
                    }
                } else {
                    let mut missing = false;
                    visit_ids(cmd, &mut |id| missing |= self.gone(id, made, lenient));
                    if !missing {
                        return;
                    }
                }
                let Command::Schedule(args) = std::mem::replace(cmd, empty()) else {
                    return;
                };
                self.retire_nested(&args.cmd);
                self.schedule.retired += 1;
                self.last_retired = self.frame;
                self.emit(Event::ScheduleFired {
                    seq_no: args.seq_no,
                    total: self.schedule.retired,
                    applied: false,
                });
                if let WhenSpec::AfterGlides { watch: Some(w) } = args.when {
                    self.retire(Garbage::Watch(w));
                }
                self.retire(Garbage::Scheduled(args.cmd));
            }
            _ => {}
        }
    }

    // whether an object a command names is gone: it isn't there, and nothing
    // that waits will make it
    fn gone(&self, id: AnyId, made: &Made, lenient: bool) -> bool {
        !self.exists(id) && !made.has(id) && !(lenient && self.table_creates(id))
    }

    // whether an entry in the table makes `id`
    fn table_creates(&self, id: AnyId) -> bool {
        self.schedule
            .slots
            .iter()
            .flatten()
            .any(|p| creates_id(&p.cmd, id))
    }

    // whether the entry's clock or anything its command names is gone
    fn names_missing(&self, p: &Pending, lenient: bool) -> bool {
        let mut missing = match p.due {
            Due::Ticks { tempo, .. } | Due::Mark { tempo, .. } => self.tempo_state(tempo).is_none(),
            Due::Frame(_) => false,
        };
        // a statement-free entry names nothing itself (see `neutralise`)
        if p.watch.is_none() && holds_no_statement(&p.cmd) {
            return missing;
        }
        visit_ids(&p.cmd, &mut |id| {
            // the tree is only searched for a name that isn't there
            missing |=
                !self.exists(id) && !creates_id(&p.cmd, id) && !(lenient && self.table_creates(id));
        });
        if let Some(watch) = &p.watch {
            for w in watch.iter() {
                missing |= self.glide_generation(w.glide).is_none();
            }
        }
        missing
    }

    // whether the object `id` names is in its arena
    fn exists(&self, id: AnyId) -> bool {
        match id {
            AnyId::Voice(v) => self.voices.get(v.index()).is_some(),
            AnyId::Group(g) => self.groups.get(g.index()).is_some(),
            AnyId::Tempo(t) => self.tempos.get(t.index()).is_some(),
            AnyId::Proc(p) => self.procs.get(p.index()).is_some(),
            AnyId::Track(t) => self.tracks.get(t.index()).is_some(),
            AnyId::Buffer(b) => self.buffers.get(b.index()).is_some_and(Option::is_some),
            AnyId::Wave { voice, id } => self
                .voices
                .get(voice.index())
                .is_some_and(|v| v.waves.has(id)),
            AnyId::Fx { owner, id } => match owner {
                FxOwner::Voice(v) => self.voices.get(v.index()).is_some_and(|v| v.fx.has(id)),
                FxOwner::Group(g) => self.groups.get(g.index()).is_some_and(|g| g.fx.has(id)),
            },
        }
    }

    // the due entry that arrived first, if any; a missing clock counts as due
    // so the entry doesn't hold its slot for ever
    fn next_due(&self) -> Option<usize> {
        let mut best: Option<(u64, usize)> = None;
        for (i, slot) in self.schedule.slots.iter().enumerate() {
            let Some(p) = slot else { continue };
            let due = match p.due {
                Due::Ticks { tempo, due } => {
                    self.tempo_state(tempo).is_none_or(|t| t.steps() >= due)
                }
                Due::Frame(frame) => self.frame >= frame,
                Due::Mark {
                    slot,
                    seq,
                    tempo,
                    ticks,
                } => self.schedule.marks.get(usize::from(slot)).is_none_or(|m| {
                    m.seq == seq
                        && self
                            .tempo_state(tempo)
                            .is_none_or(|t| t.steps() >= m.steps + ticks)
                }),
            };
            if due && best.is_none_or(|(order, _)| p.order < order) {
                best = Some((p.order, i));
            }
        }
        best.map(|(_, i)| i)
    }

    // where a clock-counted entry was due, if it counts on one
    fn due_position(&self, due: &Due) -> Option<(TempoId, f64)> {
        match *due {
            Due::Ticks { tempo, due } => Some((tempo, due)),
            Due::Mark {
                slot, tempo, ticks, ..
            } => self
                .schedule
                .marks
                .get(usize::from(slot))
                .map(|m| (tempo, m.steps + ticks)),
            Due::Frame(_) => None,
        }
    }

    // counts a retired entry, sends its box out as garbage, and tells the
    // mirror which line it was
    fn retire_entry(
        &mut self,
        seq_no: u64,
        applied: bool,
        watch: Option<Box<[GlideWatch]>>,
        cmd: Box<Command>,
    ) {
        self.schedule.retired += 1;
        self.last_retired = self.frame;
        if let Some(watch) = watch {
            self.retire(Garbage::Watch(watch));
        }
        self.retire(Garbage::Scheduled(cmd));
        self.emit(Event::ScheduleFired {
            seq_no,
            total: self.schedule.retired,
            applied,
        });
    }

    // an entry that fired: its contents were moved out and applied, so what
    // it nested has arrived on its own
    fn drop_fired(&mut self, pending: Pending) {
        let Pending {
            seq_no, watch, cmd, ..
        } = pending;
        self.retire_entry(seq_no, true, watch, cmd);
    }

    // an entry that did not apply: whatever it nested never arrives, so each
    // of those counts as retired too (the mirror counted them when it sent)
    fn drop_scheduled(&mut self, pending: Pending) {
        let Pending {
            seq_no, watch, cmd, ..
        } = pending;
        self.retire_nested(&cmd);
        self.retire_entry(seq_no, false, watch, cmd);
    }

    // reports every `Schedule` inside `cmd` as retired
    fn retire_nested(&mut self, cmd: &Command) {
        match cmd {
            Command::Schedule(a) => {
                self.schedule.retired += 1;
                self.emit(Event::ScheduleFired {
                    seq_no: a.seq_no,
                    total: self.schedule.retired,
                    applied: false,
                });
                self.retire_nested(&a.cmd);
            }
            Command::Batch(a) => {
                for c in &a.cmds {
                    self.retire_nested(c);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
