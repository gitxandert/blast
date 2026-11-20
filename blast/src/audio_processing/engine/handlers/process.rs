//! Process commands (PRC).

use crate::audio_processing::commands::*;
use crate::audio_processing::engine::{Conductor, Garbage, ProcSlot, group_running, voice_running};
use crate::audio_processing::processes::*;

impl Conductor {
    // Processes
    //
    // the tempo a new process reads: an own one is created (and started if
    // its owner is playing), a shared one is already there
    fn proc_tempo(&mut self, owner: ProcOwner, tempo: TempoUse) -> TempoId {
        match tempo {
            TempoUse::Own(init) => {
                self.add_tempo(init, TempoOwner::from(owner));
                // the voice's start only starts the tempos that exist at
                // the time, so start this one if the voice is already playing
                let playing = match owner {
                    ProcOwner::Voice(v) => {
                        self.voices.get(v.index()).is_some_and(|v| v.state.active)
                    }
                    ProcOwner::Group(g) => {
                        self.groups.get(g.index()).is_some_and(|g| g.state.active)
                    }
                    // nothing gates it
                    ProcOwner::Session => true,
                };
                if playing && let Some(t) = self.tempos.get_mut(init.id.index()) {
                    t.state.start();
                }
                init.id
            }
            TempoUse::Shared(id) => id,
        }
    }

    pub(crate) fn seq(&mut self, args: SeqArgs) {
        let (proc, owner) = (args.proc, args.owner);
        let tempo = self.proc_tempo(owner, args.tempo);
        let epoch = self.tempo_state(tempo).map_or(0, |t| t.epoch);

        let slot = ProcSlot {
            owner,
            tempo,
            // moves the step Vecs in; nothing is allocated here
            process: Process::Seq(Seq::new(args, epoch)),
            generation: 0,
        };
        if let Err(slot) = self.procs.insert(proc.index(), slot) {
            debug_assert!(false, "process slot {proc:?} taken");
            self.retire(Garbage::Process(slot.process));
        }
    }

    /// Adds an lfo: plain data, so nothing is allocated.
    pub(crate) fn lfo(&mut self, args: LfoArgs) {
        let (proc, owner) = (args.proc, args.owner);
        let tempo = self.proc_tempo(owner, args.tempo);
        let slot = ProcSlot {
            owner,
            tempo,
            process: Process::Lfo(Lfo::new(args)),
            generation: 0,
        };
        if let Err(slot) = self.procs.insert(proc.index(), slot) {
            debug_assert!(false, "process slot {proc:?} taken");
            self.retire(Garbage::Process(slot.process));
        }
    }

    /// Edits an lfo in place. A glide on an lfo nothing advances lands at
    /// once.
    pub(crate) fn lfo_edit(&mut self, args: LfoEditArgs) {
        let id = args.id.index();
        let Some(slot) = self.procs.get(id) else {
            debug_assert!(false, "edit of a missing process {:?}", args.id);
            return;
        };
        let running = match slot.owner {
            ProcOwner::Voice(v) => voice_running(&self.voices, &self.groups, v),
            ProcOwner::Group(g) => group_running(&self.groups, g),
            ProcOwner::Session => true,
        } && slot.process.enabled();
        // the glides this edit starts, if the lfo is playing
        let used = [args.depth, args.offset, args.freq]
            .into_iter()
            .flatten()
            .map(|e| if running { e.frames } else { 0 })
            .max()
            .unwrap_or(0);
        if let Some(slot) = self.procs.get_mut(id)
            && let Process::Lfo(l) = &mut slot.process
        {
            slot.generation = slot.generation.wrapping_add(1);
            l.edit_lfo(args, !running);
        }
        self.note_glide(used);
    }

    /// Adds an env: plain data, so nothing is allocated.
    pub(crate) fn env(&mut self, args: EnvArgs) {
        let (proc, owner) = (args.proc, args.owner);
        let tempo = self.proc_tempo(owner, args.tempo);
        let slot = ProcSlot {
            owner,
            tempo,
            process: Process::Env(Env::new(args)),
            generation: 0,
        };
        if let Err(slot) = self.procs.insert(proc.index(), slot) {
            debug_assert!(false, "process slot {proc:?} taken");
            self.retire(Garbage::Process(slot.process));
        }
    }

    /// Edits an env in place: a new shape waits for the next hit.
    pub(crate) fn env_edit(&mut self, args: EnvEditArgs) {
        match self.procs.get_mut(args.id.index()).map(|s| &mut s.process) {
            Some(Process::Env(e)) => e.edit_env(args),
            _ => debug_assert!(false, "edit of a missing env {:?}", args.id),
        }
    }

    /// Edits a seq in place: the vectors built on the command thread move
    /// in, and the ones they replace leave through the garbage queue. No
    /// allocation, free, lock or print.
    pub(crate) fn seq_edit(&mut self, args: SeqEditArgs) {
        let Some(slot) = self.procs.get(args.id.index()) else {
            debug_assert!(false, "edit of a missing process {:?}", args.id);
            // dropping the new vectors here would free on the audio thread
            self.retire(Garbage::SeqData(SeqVecs {
                steps: args.steps,
                chance: args.chance,
                jit: args.jit,
                pend: args.pend,
                mask: args.mask,
                recv: Vec::new(),
            }));
            return;
        };
        // a glide on a seq nothing advances lands at once
        let running = match slot.owner {
            ProcOwner::Voice(v) => voice_running(&self.voices, &self.groups, v),
            ProcOwner::Group(g) => group_running(&self.groups, g),
            ProcOwner::Session => true,
        } && slot.process.enabled();
        let id = args.id.index();
        let used = args
            .feel
            .iter()
            .flatten()
            .map(|e| if running { e.frames } else { 0 })
            .max()
            .unwrap_or(0);
        let Some(slot) = self.procs.get_mut(id) else {
            return;
        };
        slot.generation = slot.generation.wrapping_add(1);
        let old = slot.process.edit(args, !running);
        self.note_glide(used);
        if old.steps.capacity()
            + old.chance.capacity()
            + old.jit.capacity()
            + old.mask.capacity()
            + old.pend.capacity()
            + old.recv.capacity()
            > 0
        {
            self.retire(Garbage::SeqData(old));
        }
    }

    /// Replaces a seq's receivers. The `Vec` built on the command thread
    /// moves in, and the old one leaves through the garbage queue (it owns
    /// the melo notes). No allocation, free, lock or print.
    pub(crate) fn seq_recv(&mut self, args: SeqRecvArgs) {
        let old = match self.procs.get_mut(args.id.index()) {
            Some(slot) => slot.process.set_recv(args.recv),
            None => {
                debug_assert!(false, "receivers of a missing process {:?}", args.id);
                // dropping the new Vec here would free on the audio thread
                args.recv
            }
        };
        if old.capacity() > 0 {
            self.retire(Garbage::SeqData(SeqVecs {
                recv: old,
                ..SeqVecs::default()
            }));
        }
    }

    /// Pauses, resumes, resets or removes a process.
    pub(crate) fn proc_edit(&mut self, args: ProcEditArgs) {
        let id = args.id.index();
        match args.op {
            ProcOp::Remove => {
                // a Seq owns its step Vecs, so it leaves as garbage
                if let Some(slot) = self.procs.remove(id) {
                    self.retire(Garbage::Process(slot.process));
                } else {
                    debug_assert!(false, "removal of a missing process {:?}", args.id);
                }
            }
            op => {
                let Some(slot) = self.procs.get_mut(id) else {
                    debug_assert!(false, "edit of a missing process {:?}", args.id);
                    return;
                };
                match op {
                    ProcOp::Enable => slot.process.set_enabled(true),
                    ProcOp::Disable => slot.process.set_enabled(false),
                    ProcOp::Reset => {
                        slot.process.reset();
                        // `stop` and `start` of an lfo ask for this; its
                        // owner starting or stopping doesn't
                        match &mut slot.process {
                            Process::Lfo(l) => l.unfreeze(),
                            // and the env waits for its next hit
                            Process::Env(e) => e.unfreeze(),
                            Process::Seq(_) => {}
                        }
                    }
                    ProcOp::Remove => {}
                }
            }
        }
    }
}
