//! Tempo commands (TMP). `retime_tempo`, `free_tempo` and `assign_tempo` run
//! on the audio thread: the tempo arena is preallocated and a `Tempo` is plain
//! data, so none of them allocates, frees or locks.

use crate::audio_processing::commands::*;
use crate::audio_processing::engine::{Conductor, Tempo, group_running, voice_running};

impl Conductor {
    pub(crate) fn tempo_context(&mut self, args: TcArgs) {
        self.add_tempo(args.tempo, TempoOwner::Context);
    }

    /// Changes a tempo's rate, at once or over `frames`; its position carries
    /// on. Lowering has already checked that a glide has a running clock.
    pub(crate) fn retime_tempo(&mut self, args: TempoSetArgs) {
        match self.tempos.get_mut(args.id.index()) {
            Some(tempo) => {
                tempo.glide_gen = tempo.glide_gen.wrapping_add(1);
                if args.frames > 0 {
                    tempo.state.glide(args.unit, args.interval, args.frames);
                } else {
                    tempo.state.retime(args.unit, args.interval);
                }
                self.note_glide(args.frames);
            }
            None => debug_assert!(false, "retime of a missing tempo {:?}", args.id),
        }
    }

    /// Drops a tempo nothing uses any more. It is plain data, so it drops in
    /// place instead of leaving as garbage.
    pub(crate) fn free_tempo(&mut self, args: TempoFreeArgs) {
        let removed: Option<Tempo> = self.tempos.remove(args.id.index());
        debug_assert!(removed.is_some(), "free of a missing tempo {:?}", args.id);
    }

    /// Points a voice, a group or a process at a tempo. An `Own` tempo is
    /// created first, owned by the target; if the target is playing, it
    /// starts at once (the owner's start only reaches tempos that exist).
    pub(crate) fn assign_tempo(&mut self, args: TempoAssignArgs) {
        let (owner, running) = match args.target {
            TempoTarget::Voice(v) => (
                TempoOwner::Voice(v),
                voice_running(&self.voices, &self.groups, v),
            ),
            TempoTarget::Group(g) => (TempoOwner::Group(g), group_running(&self.groups, g)),
            TempoTarget::Proc(p) => {
                let Some(slot) = self.procs.get(p.index()) else {
                    debug_assert!(false, "assign to a missing process {p:?}");
                    return;
                };
                let running = match slot.owner {
                    ProcOwner::Voice(v) => voice_running(&self.voices, &self.groups, v),
                    ProcOwner::Group(g) => group_running(&self.groups, g),
                    ProcOwner::Session => true,
                };
                (TempoOwner::from(slot.owner), running)
            }
        };

        let id = match args.tempo {
            TempoUse::Own(init) => {
                self.add_tempo(init, owner);
                if running && let Some(t) = self.tempos.get_mut(init.id.index()) {
                    t.state.start();
                }
                init.id
            }
            TempoUse::Shared(id) => id,
        };

        match args.target {
            TempoTarget::Voice(v) => {
                if let Some(voice) = self.voices.get_mut(v.index()) {
                    voice.state.tempo = id;
                    // an explicit clock: no longer adopts its group's
                    voice.state.adopts = false;
                }
            }
            TempoTarget::Group(g) => {
                if let Some(group) = self.groups.get_mut(g.index()) {
                    group.state.tempo = id;
                }
            }
            TempoTarget::Proc(p) => {
                if let Some(slot) = self.procs.get_mut(p.index()) {
                    slot.tempo = id;
                }
            }
        }
    }
}
