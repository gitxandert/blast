//! The per-frame mix (README §3.4).

use crate::audio_processing::blast_time::blast_time::clock;
use crate::audio_processing::commands::{ProcOwner, TempoOwner};
use crate::audio_processing::engine::buffer::{BufSource, BufferSlot};
use crate::audio_processing::engine::shift::ColdBudget;
use crate::audio_processing::engine::{
    Conductor, Event, Garbage, group_running, retire_into, voice_is_running, voice_running,
};

// writes one frame to the buffer in `slot`, if it is recording; a take that
// the frame filled is reported
fn tap(
    buffers: &mut [Option<BufferSlot>],
    (events, garbage, parked, leaked): (
        &mut rtrb::Producer<Event>,
        &mut rtrb::Producer<Garbage>,
        &mut Vec<Garbage>,
        &mut u32,
    ),
    slot: u32,
    frame: &[f32],
) {
    if let Some(Some(buffer)) = buffers.get_mut(slot as usize)
        && let Some(frames) = buffer.push(frame)
    {
        // a full queue means the REPL isn't reading; losing a notice is fine
        let take = buffer.take_no();
        let _ = events.push(Event::BufferEnded { slot, take, frames });
        // a take with `to=` hands its copy back
        if let Some((ticket, data, saved)) = buffer.take_save() {
            retire_into(
                garbage,
                parked,
                leaked,
                Garbage::BufferSave {
                    slot,
                    ticket,
                    data,
                    frames: saved,
                },
            );
        }
    }
}

// `tap` over the fields it needs, so the render loop can call it while
// other arenas are borrowed
macro_rules! tap {
    ($s:ident, $slot:expr, $frame:expr) => {
        tap(
            &mut $s.buffers,
            (
                &mut $s.events,
                &mut $s.garbage,
                &mut $s.parked,
                &mut $s.leaked,
            ),
            $slot,
            $frame,
        )
    };
}

impl Conductor {
    /// Renders `out.len() / channels` interleaved frames, unclamped,
    /// full scale ±1.0. Never allocates or frees. Retries parked garbage
    /// first, and reports leaked garbage at the end.
    ///
    /// Every frame runs, in order: (1) every process whose owner is
    /// playing, (2) every scheduled line that is due, (3) every tempo whose
    /// owner is playing (contexts always), (4) the pending shifter hits of every
    /// playing voice, then its mix, (5) their `advance`, (6) the clock.
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        self.flush_parked();
        if channels == 0 {
            self.report_leaks();
            return;
        }

        // the bus and the scratch frame are sized by `out_channels`; slice
        // by that, whatever `channels` says, so a mismatch can't index past
        // either (the mixes below zip)
        let width = self.out_channels;
        // fresh shifter seeks this callback may start (REQ-099 section 4)
        let mut budget = ColdBudget::for_callback(out.len() / channels);
        let mut frames = out.chunks_exact_mut(channels);
        for frame in &mut frames {
            frame.fill(0.0);
            self.run_processes();
            // before the tempos tick, so a scheduled `start` sounds on the
            // frame it is due
            self.fire_due();
            self.tick_tempos();

            // each running group's bus starts silent
            for (i, group) in self.groups.iter() {
                if group.state.active {
                    self.bus[i * width..(i + 1) * width].fill(0.0);
                }
            }

            for (_, voice) in self.voices.iter_mut() {
                if !voice_is_running(voice, &self.groups) {
                    // a buffer recording a voice that isn't playing records
                    // the silence: time passes in the take
                    if !voice.taps.is_empty() {
                        self.fx_scratch.fill(0.0);
                        for &slot in &voice.taps {
                            tap!(self, slot, &self.fx_scratch);
                        }
                    }
                    continue;
                }
                voice.sync_len();
                // hits since the last frame (seqs, `start`, a changed
                // setting) get their shifter state before the voice sounds
                voice.resolve_hits(&mut budget);
                // a grouped voice mixes into its group's bus; the group's
                // chain and gain come after all its members
                let dest: &mut [f32] = match voice.group {
                    Some(g) => &mut self.bus[g.index() * width..(g.index() + 1) * width],
                    None => &mut *frame,
                };
                if !voice.fx.is_empty() || !voice.taps.is_empty() {
                    // a voice with effects or a buffer recording it renders
                    // alone, runs its chain, is tapped (after its effects,
                    // pan and gain, before its group), and then mixes in
                    self.fx_scratch.fill(0.0);
                    voice.render_frame(&mut self.fx_scratch, &self.blep, self.frame);
                    if !voice.fx.is_empty() {
                        voice.fx.process(&mut self.fx_scratch);
                    }
                    for &slot in &voice.taps {
                        tap!(self, slot, &self.fx_scratch);
                    }
                    for (o, s) in dest.iter_mut().zip(&self.fx_scratch) {
                        *o += s;
                    }
                } else {
                    voice.render_frame(dest, &self.blep, self.frame);
                }
            }

            // then each group's bus: its chain, its gain, into the output
            for (i, group) in self.groups.iter_mut() {
                if !group.state.active {
                    // as for a voice: a stopped group records silence
                    if !group.taps.is_empty() {
                        self.fx_scratch.fill(0.0);
                        for &slot in &group.taps {
                            tap!(self, slot, &self.fx_scratch);
                        }
                    }
                    continue;
                }
                let bus = &mut self.bus[i * width..(i + 1) * width];
                if !group.fx.is_empty() {
                    group.fx.process(bus);
                }
                let gain = group.state.gain;
                if group.taps.is_empty() {
                    for (o, s) in frame.iter_mut().zip(bus.iter()) {
                        *o += s * gain;
                    }
                } else {
                    // the group's bus after its effects and gain
                    for (m, s) in self.fx_scratch.iter_mut().zip(bus.iter()) {
                        *m = s * gain;
                    }
                    for &slot in &group.taps {
                        tap!(self, slot, &self.fx_scratch);
                    }
                    for (o, m) in frame.iter_mut().zip(self.fx_scratch.iter()) {
                        *o += m;
                    }
                }
            }

            // the whole mix, as it stands before the output clamp
            for i in 0..self.out_taps.len() {
                let slot = self.out_taps[i];
                if self.buffers.get(slot as usize).is_some_and(|b| {
                    b.as_ref()
                        .is_some_and(|b| b.source == BufSource::Out && b.is_writing())
                }) {
                    tap!(self, slot, frame);
                }
            }

            // advance once per frame, after the frame is mixed
            let groups = &self.groups;
            for (_, voice) in self.voices.iter_mut() {
                if voice_is_running(voice, groups) {
                    voice.advance();
                }
            }

            for (_, group) in self.groups.iter_mut() {
                group.state.advance();
                group.fx.advance();
            }

            clock::advance(1);
            self.frame += 1;
        }

        // a trailing partial frame isn't rendered; keep it silent
        frames.into_remainder().fill(0.0);
        self.report_leaks();
    }

    // the arenas are borrowed field by field, so a process can act on the
    // voice arena while the process arena is borrowed
    //
    // Two passes: the processes that make hits (seqs) first, then the ones
    // that watch or modulate (lfo, env), so an env sees a hit from another
    // voice's seq on the frame it happens, whatever order they were made in.
    fn run_processes(&mut self) {
        for firing in [true, false] {
            for (_, slot) in self.procs.iter_mut() {
                if slot.process.fires() != firing {
                    continue;
                }
                let Some(tempo) = self.tempos.get(slot.tempo.index()) else {
                    continue;
                };
                let playing = match slot.owner {
                    ProcOwner::Voice(v) => voice_running(&self.voices, &self.groups, v),
                    ProcOwner::Group(g) => group_running(&self.groups, g),
                    // no voice to gate it: it runs while its clock does
                    ProcOwner::Session => true,
                };
                if playing {
                    slot.process.process(
                        &mut self.voices,
                        &mut self.groups,
                        slot.owner,
                        &tempo.state,
                    );
                }
            }
        }
    }

    fn tick_tempos(&mut self) {
        for (_, tempo) in self.tempos.iter_mut() {
            let running = match tempo.owner {
                TempoOwner::Voice(v) => voice_running(&self.voices, &self.groups, v),
                TempoOwner::Group(g) => group_running(&self.groups, g),
                TempoOwner::Context => true,
            };
            // update itself does nothing while the tempo is stopped
            if running {
                tempo.state.update(1.0);
            }
        }
    }
}
