//! Voice commands (VOX): load, start, pause, resume, stop, unload, and their settings.
//! Transport on groups and tempos is here too, since `start` & co. take
//! any `Target`.

use crate::audio_processing::commands::*;
use crate::audio_processing::engine::{Conductor, Event, Garbage, ProcSlot, Voice, VoiceParam};
use crate::audio_processing::processes::Process;

/// Sets a voice's base value for `param`, at once or as a glide of `frames`.
/// For GRP's group-wide commands too.
pub(crate) fn set_voice_param(
    c: &mut Conductor,
    v: VoiceId,
    param: VoiceParam,
    value: f32,
    frames: u32,
) {
    if let Some(voice) = c.voices.get_mut(v.index()) {
        let used = voice.state.set_param(param, value, frames);
        c.note_glide(used);
    } else {
        debug_assert!(false, "missing voice {v:?}");
    }
}

impl Conductor {
    pub(crate) fn load(&mut self, args: LoadArgs) {
        let tempo = match args.tempo {
            TempoUse::Own(init) => {
                self.add_tempo(init, TempoOwner::Voice(args.voice));
                init.id
            }
            TempoUse::Shared(id) => id,
        };
        let voice = match args.source {
            LoadSource::Track(id) => {
                let Some(track) = self.tracks.get(id.index()) else {
                    debug_assert!(false, "load of a missing track {id:?}");
                    return;
                };
                Voice::new(track, tempo, args.adopts)
            }
            LoadSource::Buffer(data) => Voice::from_buffer(data, tempo, args.adopts),
            LoadSource::Waves => Voice::from_waves(tempo, args.adopts),
        };
        if let Err(voice) = self.voices.insert(args.voice.index(), voice) {
            debug_assert!(false, "voice slot {:?} taken", args.voice);
            self.retire(Garbage::Voice(voice));
        }
    }

    pub(crate) fn start(&mut self, args: StartArgs) {
        match args.target {
            Target::Voice(v) => self.start_voice(v),
            Target::Group(g) => self.start_group(g),
            Target::Tempo(t) => self.with_tempo(t, |ts| ts.start()),
        }
    }

    pub(crate) fn pause(&mut self, args: PauseArgs) {
        match args.target {
            Target::Voice(v) => {
                if let Some(voice) = self.voice_mut(v) {
                    voice.state.active = false;
                }
            }
            Target::Group(g) => {
                // its voices stay active, but don't run while it's paused
                if let Some(group) = self.groups.get_mut(g.index()) {
                    group.state.active = false;
                }
            }
            Target::Tempo(t) => self.with_tempo(t, |ts| ts.pause()),
        }
    }

    pub(crate) fn resume(&mut self, args: ResumeArgs) {
        match args.target {
            Target::Voice(v) => {
                let Some(voice) = self.voice_mut(v) else {
                    return;
                };
                voice.state.active = true;
                let tempo = voice.state.tempo;
                self.rejoin_grid(v, [TempoOwner::Voice(v); 2]);
                self.warn_if_shared_tempo_inactive(tempo, TempoOwner::Voice(v));
            }
            Target::Group(g) => {
                let Some(group) = self.groups.get_mut(g.index()) else {
                    debug_assert!(false, "missing group {g:?}");
                    return;
                };
                group.state.active = true;
                let tempo = group.state.tempo;
                // the group's own seqs on a clock that kept ticking while it
                // was paused are resynced, and its members wait for the next
                // step; a seq on the group's own clock froze with it
                let mut ticked = false;
                for (_, slot) in self.procs.iter_mut() {
                    if slot.owner != ProcOwner::Group(g) || !slot.process.fires() {
                        continue;
                    }
                    let owner = self.tempos.get(slot.tempo.index()).map(|t| t.owner);
                    if owner == Some(TempoOwner::Group(g)) {
                        continue;
                    }
                    slot.process.resync();
                    ticked = true;
                }
                // by index, since rejoin_grid needs the whole Conductor
                let mut i = 0;
                while let Some(&m) = self
                    .groups
                    .get(g.index())
                    .and_then(|group| group.members.get(i))
                {
                    self.rejoin_grid(m, [TempoOwner::Voice(m), TempoOwner::Group(g)]);
                    if ticked && let Some(voice) = self.voices.get_mut(m.index()) {
                        voice.park();
                    }
                    i += 1;
                }
                self.warn_if_shared_tempo_inactive(tempo, TempoOwner::Group(g));
            }
            Target::Tempo(t) => self.with_tempo(t, |ts| ts.resume()),
        }
    }

    pub(crate) fn stop(&mut self, args: StopArgs) {
        match args.target {
            Target::Voice(v) => self.stop_voice(v),
            Target::Group(g) => self.stop_group(g),
            Target::Tempo(t) => self.with_tempo(t, |ts| ts.stop()),
        }
    }

    pub(crate) fn unload(&mut self, args: UnloadArgs) {
        self.remove_voice(args.voice);
    }

    /// Sets a voice's base value for one parameter (clamped), at once or as a
    /// glide of `frames`.
    pub(crate) fn set_param(&mut self, args: SetParamArgs) {
        set_voice_param(self, args.voice, args.param, args.value, args.frames);
    }

    /// Sets a voice's loop mode or slice count.
    pub(crate) fn set_mode(&mut self, args: SetModeArgs) {
        let Some(voice) = self.voice_mut(args.voice) else {
            return;
        };
        match args.mode {
            VoiceMode::Loop(mode) => voice.state.set_loop_mode(mode),
            VoiceMode::Slices(n) => voice.state.set_slices(n),
            VoiceMode::Choir(n) => voice.set_choir(n),
            VoiceMode::ChoirMode(m) => voice.state.set_choir_mode(m),
            VoiceMode::Shift(m) => voice.set_shift_mode(m),
            VoiceMode::Resample(m) => voice.state.set_resample(m),
            VoiceMode::Bounce(n) => voice.state.set_bounce(n),
        }
    }

    /// Gives a voice its shifters (REQ-099). The old set leaves through the
    /// garbage queue, since dropping it frees memory. A voice that isn't
    /// there (the mirror prevents that) hands the new set straight back.
    pub(crate) fn set_shifters(&mut self, args: SetShiftersArgs) {
        let Some(voice) = self.voices.get_mut(args.voice.index()) else {
            debug_assert!(false, "shifters for a missing voice {:?}", args.voice);
            self.retire(Garbage::Shifters(args.shifters));
            return;
        };
        if let Some(old) = voice.set_shifters(args.shifters) {
            self.retire(Garbage::Shifters(old));
        }
    }

    /// Points a voice at another track. The old samples leave through the
    /// garbage queue: this may hold the last reference.
    pub(crate) fn set_track(&mut self, args: SetTrackArgs) {
        let Some(track) = self.tracks.get(args.track.index()) else {
            debug_assert!(false, "set_track to a missing track {:?}", args.track);
            return;
        };
        let Some(voice) = self.voices.get_mut(args.voice.index()) else {
            debug_assert!(false, "missing voice {:?}", args.voice);
            return;
        };
        let old = voice.swap_track(track);
        self.retire_source(old);
    }

    /// Plays a voice from the top (or parks it, if it has a seq): resets its processes, starts the tempos
    /// it owns (its own and its processes').
    pub(crate) fn start_voice(&mut self, v: VoiceId) {
        // a voice with a seq waits, silent, for the seq's next step, so it
        // comes in on the grid; otherwise it plays at once
        let has_seq = self.sequenced(v);
        let Some(voice) = self.voice_mut(v) else {
            return;
        };
        voice.state.active = true;
        // an effect starts from silence, as the voice does
        voice.fx.reset();
        match has_seq {
            true => voice.park(),
            false => voice.hit(),
        }
        let tempo = voice.state.tempo;

        self.reset_procs(ProcOwner::Voice(v));
        if has_seq {
            // a parked voice's first hit is its seq's first step: its envs
            // wait for that instead of firing now and again at the step
            for (_, slot) in self.procs.iter_mut() {
                if slot.owner == ProcOwner::Voice(v)
                    && let Process::Env(e) = &mut slot.process
                {
                    e.wait_for_hit(&self.voices, &self.groups, slot.owner);
                }
            }
        }
        self.for_owned_tempos(TempoOwner::Voice(v), |ts| ts.start());
        self.warn_if_shared_tempo_inactive(tempo, TempoOwner::Voice(v));
    }

    /// Stops and rewinds a voice, its processes and the tempos it owns.
    pub(crate) fn stop_voice(&mut self, v: VoiceId) {
        let Some(voice) = self.voice_mut(v) else {
            return;
        };
        voice.state.active = false;
        voice.rewind();

        self.reset_procs(ProcOwner::Voice(v));
        self.for_owned_tempos(TempoOwner::Voice(v), |ts| ts.stop());
    }

    /// Whether a seq will trigger voice `v`: one of its own, its group's, or
    /// any whose receivers name it (or its group), that isn't paused. Such a
    /// voice waits, silent, for the seq's step.
    pub(super) fn sequenced(&self, v: VoiceId) -> bool {
        let group = self.voices.get(v.index()).and_then(|x| x.group);
        self.procs
            .iter()
            .any(|(_, slot)| slot.process.fires() && slot_hits(slot, v, group))
    }

    // a missing ID is a mirror bug
    fn voice_mut(&mut self, v: VoiceId) -> Option<&mut Voice> {
        let voice = self.voices.get_mut(v.index());
        debug_assert!(voice.is_some(), "missing voice {v:?}");
        voice
    }

    fn with_tempo(
        &mut self,
        t: TempoId,
        f: impl FnOnce(&mut crate::audio_processing::blast_time::blast_time::TempoState),
    ) {
        match self.tempos.get_mut(t.index()) {
            Some(tempo) => f(&mut tempo.state),
            None => debug_assert!(false, "missing tempo {t:?}"),
        }
    }

    /// Brings a resumed voice's seqs back on the grid: the seqs that read a
    /// tempo which kept ticking while the voice was paused are resynced, and
    /// then the voice is parked, so it waits, silent, for the next step.
    /// Seqs on a tempo in `frozen` (the voice's own, or its group's) froze
    /// with it and pick up exactly where they left off.
    pub(super) fn rejoin_grid(&mut self, v: VoiceId, frozen: [TempoOwner; 2]) {
        let group = self.voices.get(v.index()).and_then(|x| x.group);
        let mut ticked = false;
        for (_, slot) in self.procs.iter_mut() {
            let own = slot.owner == ProcOwner::Voice(v);
            // its group's seqs (and any that name it) keep firing for the
            // others, so they aren't resynced here; the voice just waits for
            // their next step
            let shared = !own && slot_hits(slot, v, group);
            if !(own || shared) || !slot.process.fires() {
                continue;
            }
            let owner = self.tempos.get(slot.tempo.index()).map(|t| t.owner);
            if owner.is_some_and(|o| frozen.contains(&o)) {
                continue;
            }
            if own {
                slot.process.resync();
            }
            ticked = true;
        }
        if ticked && let Some(voice) = self.voices.get_mut(v.index()) {
            voice.park();
        }
    }

    pub(super) fn reset_procs(&mut self, owner: ProcOwner) {
        for (_, slot) in self.procs.iter_mut() {
            if slot.owner == owner {
                slot.process.reset();
            }
        }
    }

    pub(super) fn for_owned_tempos(
        &mut self,
        owner: TempoOwner,
        mut f: impl FnMut(&mut crate::audio_processing::blast_time::blast_time::TempoState),
    ) {
        for (_, tempo) in self.tempos.iter_mut() {
            if tempo.owner == owner {
                f(&mut tempo.state);
            }
        }
    }

    // a tempo someone else runs (a context, or a group's) may not be playing
    pub(super) fn warn_if_shared_tempo_inactive(&mut self, tempo: TempoId, owner: TempoOwner) {
        let Some(t) = self.tempos.get(tempo.index()) else {
            return;
        };
        if t.owner != owner && !t.state.active {
            self.emit(Event::TempoInactive { owner });
        }
    }
}

// Whether the process in `slot` hits voice `v` (in `group`): it is the
// voice's seq, its group's, or one whose receivers name the voice or the group.
fn slot_hits(slot: &ProcSlot, v: VoiceId, group: Option<GroupId>) -> bool {
    slot.owner == ProcOwner::Voice(v)
        || group.is_some_and(|g| slot.owner == ProcOwner::Group(g))
        || slot.process.targets(v, group)
}
