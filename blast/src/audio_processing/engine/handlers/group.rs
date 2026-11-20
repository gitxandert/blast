//! Group commands (GRP).

use crate::audio_processing::commands::*;
use crate::audio_processing::engine::{Conductor, Garbage, Group, retire_into};
use crate::audio_processing::processes::Process;

impl Conductor {
    pub(crate) fn group(&mut self, args: GroupArgs) {
        let g = args.group;
        let tempo = match args.tempo {
            TempoUse::Own(init) => {
                self.add_tempo(init, TempoOwner::Group(g));
                init.id
            }
            TempoUse::Shared(id) => id,
        };

        for &m in &args.members {
            let Some(voice) = self.voices.get_mut(m.index()) else {
                debug_assert!(false, "missing member {m:?}");
                continue;
            };
            voice.group = Some(g);

            // a voice that follows its parent (loaded without -t, or without
            // `on`) takes the group's tempo, and so do its processes that
            // read its old one (seq -t v). It frees the old tempo only if
            // it owned it; a voice that followed `main` leaves `main` alone.
            let old = voice.state.tempo;
            let owns_old = self
                .tempos
                .get(old.index())
                .is_some_and(|t| t.owner == TempoOwner::Voice(m));
            if voice.state.adopts {
                voice.state.tempo = tempo;
                voice.state.adopts = false;
                if owns_old {
                    // tempos are plain data, dropped in place
                    self.tempos.remove(old.index());
                }
                for (_, slot) in self.procs.iter_mut() {
                    if slot.owner == ProcOwner::Voice(m) && slot.tempo == old {
                        slot.tempo = tempo;
                    }
                }
            }
        }

        // the members Vec moves in; nothing is allocated here
        if let Err(group) = self
            .groups
            .insert(g.index(), Group::new(tempo, args.members))
        {
            debug_assert!(false, "group slot {g:?} taken");
            // a mirror bug; freeing the members Vec here would free on the
            // audio thread
            self.retire(Garbage::Group(group));
        }
    }

    /// Adds a voice to a group, takes one out, or removes the group itself.
    /// Runs on the audio thread: the member list never grows past the
    /// capacity it was created with, and a removed group leaves as garbage.
    pub(crate) fn group_edit(&mut self, args: GroupEditArgs) {
        let g = args.group;
        match args.op {
            GroupOp::Join(v) => {
                let Some(group) = self.groups.get_mut(g.index()) else {
                    debug_assert!(false, "join of a missing group {g:?}");
                    return;
                };
                // the mirror checks `Limits::group_members`; the check here
                // keeps the push from reallocating if it ever disagrees
                if group.members.len() >= group.members.capacity() {
                    debug_assert!(false, "group {g:?} is full");
                    return;
                }
                let Some(voice) = self.voices.get_mut(v.index()) else {
                    debug_assert!(false, "join of a missing voice {v:?}");
                    return;
                };
                voice.group = Some(g);
                group.members.push(v);
                self.resync_group_envs(g);
            }
            GroupOp::Leave(v) => {
                let active = self.groups.get(g.index()).is_some_and(|g| g.state.active);
                if let Some(group) = self.groups.get_mut(g.index())
                    && let Some(pos) = group.members.iter().position(|m| *m == v)
                {
                    // keeps member order; never allocates
                    group.members.remove(pos);
                }
                if let Some(voice) = self.voices.get_mut(v.index()) {
                    voice.group = None;
                    // what you hear stays: a member of a paused group was
                    // silent, and is silent on its own too
                    voice.state.active &= active;
                    // it follows its parent again, unless the mirror says
                    // otherwise with a TempoAssign right after
                    voice.state.adopts = true;
                }
                self.resync_group_envs(g);
            }
            GroupOp::Remove => {
                let Some(group) = self.groups.remove(g.index()) else {
                    debug_assert!(false, "removal of a missing group {g:?}");
                    return;
                };
                let (procs, parked, garbage, leaked) = (
                    &mut self.procs,
                    &mut self.parked,
                    &mut self.garbage,
                    &mut self.leaked,
                );
                procs.remove_where(
                    |_, p| p.owner == ProcOwner::Group(g),
                    |_, slot| retire_into(garbage, parked, leaked, Garbage::Process(slot.process)),
                );
                // its tempos stay for whoever still reads them; the mirror
                // frees the ones nothing uses (TempoFree)
                for (_, tempo) in self.tempos.iter_mut() {
                    if tempo.owner == TempoOwner::Group(g) {
                        tempo.owner = TempoOwner::Context;
                    }
                }
                // the member list owns a buffer
                self.retire(Garbage::Group(group));
            }
        }
    }

    /// Re-reads the hit count into each env the group owns. A group env
    /// watches the sum over its members, so a member joining or leaving moves
    /// it; only real hits should fire the env.
    pub(crate) fn resync_group_envs(&mut self, g: GroupId) {
        let owner = ProcOwner::Group(g);
        for (_, slot) in self.procs.iter_mut() {
            if slot.owner == owner
                && let Process::Env(e) = &mut slot.process
            {
                e.resync_count(&self.voices, &self.groups, owner);
            }
        }
    }

    /// Scales a group's members, at once or over `frames`.
    pub(crate) fn group_gain(&mut self, args: GroupGainArgs) {
        match self.groups.get_mut(args.group.index()) {
            Some(group) => {
                let used = group.state.set_gain(args.gain, args.frames);
                self.note_glide(used);
            }
            None => debug_assert!(false, "gain of a missing group {:?}", args.group),
        }
    }

    /// Plays a group from the top: starts the tempos it owns (its own and
    /// its processes'), resets its processes and starts every member.
    pub(crate) fn start_group(&mut self, g: GroupId) {
        let Some(group) = self.groups.get_mut(g.index()) else {
            debug_assert!(false, "missing group {g:?}");
            return;
        };
        // a group that starts over starts its effects clean
        if !group.state.active {
            group.fx.reset();
        }
        group.state.active = true;
        let tempo = group.state.tempo;

        // before the members, so a member on the group's tempo sees it running
        self.for_owned_tempos(TempoOwner::Group(g), |ts| ts.start());
        self.warn_if_shared_tempo_inactive(tempo, TempoOwner::Group(g));
        self.reset_procs(ProcOwner::Group(g));
        self.for_members(g, Self::start_voice);
    }

    /// Stops and rewinds a group, its members, processes and tempos.
    pub(crate) fn stop_group(&mut self, g: GroupId) {
        let Some(group) = self.groups.get_mut(g.index()) else {
            debug_assert!(false, "missing group {g:?}");
            return;
        };
        group.state.active = false;

        self.for_members(g, Self::stop_voice);
        self.reset_procs(ProcOwner::Group(g));
        self.for_owned_tempos(TempoOwner::Group(g), |ts| ts.stop());
    }

    // by index, since `f` needs the whole Conductor
    fn for_members(&mut self, g: GroupId, f: fn(&mut Self, VoiceId)) {
        let mut i = 0;
        while let Some(&m) = self
            .groups
            .get(g.index())
            .and_then(|group| group.members.get(i))
        {
            f(self, m);
            i += 1;
        }
    }
}
