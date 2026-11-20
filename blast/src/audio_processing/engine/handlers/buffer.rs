//! Buffer commands (RECb). Everything is built on the command thread; the
//! engine installs it, and what it lets go of leaves as garbage.

use crate::audio_processing::commands::{
    BufOp, BufferArgs, BufferEditArgs, SetSourceArgs, SetTapsArgs, TapOwner,
};
use crate::audio_processing::engine::{BufferSlot, Conductor, Event, Garbage, VoiceSource};

impl Conductor {
    /// Retires what a voice stopped playing.
    pub(crate) fn retire_source(&mut self, old: VoiceSource) {
        match old {
            // nothing to free
            VoiceSource::None => {}
            VoiceSource::Track(samples) => self.retire(Garbage::Track(samples)),
            VoiceSource::Buffer(data) => self.retire(Garbage::Buffer(data)),
        }
    }

    /// Puts a buffer in its slot (replacing the memory of one that is
    /// there). With `room`, the table moves into the bigger one first.
    pub(crate) fn buffer(&mut self, args: BufferArgs) {
        if let Some(mut room) = args.room {
            for (i, entry) in self.buffers.iter_mut().enumerate() {
                match room.get_mut(i) {
                    Some(cell) => *cell = entry.take(),
                    // the mirror sends a table that fits; don't free here
                    None => {
                        debug_assert!(false, "buffer room too small");
                        if let Some(lost) = entry.take() {
                            retire_slot_into(
                                &mut self.garbage,
                                &mut self.parked,
                                &mut self.leaked,
                                i as u32,
                                lost,
                            );
                        }
                    }
                }
            }
            let old = std::mem::replace(&mut self.buffers, room);
            self.retire(Garbage::BufferTable(old));
        }
        let slot = BufferSlot::new(args.data, args.source, args.looping, args.bits, args.seed);
        match self.buffers.get_mut(args.slot as usize) {
            Some(cell) => {
                if let Some(old) = cell.replace(slot) {
                    self.retire_slot(args.slot, old);
                }
            }
            None => {
                debug_assert!(false, "buffer slot {} is outside the table", args.slot);
                self.retire_slot(args.slot, slot);
            }
        }
    }

    /// Retires a buffer's memory, and its take's private copy if it has one.
    pub(crate) fn retire_slot(&mut self, index: u32, slot: BufferSlot) {
        retire_slot_into(
            &mut self.garbage,
            &mut self.parked,
            &mut self.leaked,
            index,
            slot,
        );
    }

    /// Start, pause, resume or stop a take, or change a buffer's settings.
    /// A slot that is gone (a waiting line outlived the buffer) is ignored.
    pub(crate) fn buffer_edit(&mut self, args: BufferEditArgs) {
        let Some(cell) = self.buffers.get_mut(args.slot as usize) else {
            return;
        };
        let mut ended = None;
        let mut removed = None;
        let mut leftover = None;
        let mut save_back = false;
        match (args.op, cell.as_mut()) {
            (BufOp::Remove, _) => {
                if let Some(old) = cell.take() {
                    removed = Some(old);
                }
            }
            (BufOp::Start, Some(b)) => leftover = b.start(args.save, args.take),
            (BufOp::Pause, Some(b)) => b.pause(),
            (BufOp::Resume, Some(b)) => b.resume(),
            (BufOp::Stop, Some(b)) => {
                ended = b.stop().map(|frames| (b.take_no(), frames));
                save_back = true;
            }
            (BufOp::Looping(on), Some(b)) => b.set_looping(on),
            (BufOp::Source(source), Some(b)) => b.source = source,
            (_, None) => {}
        }
        // a Stop hands the take's copy back, if it has one
        let stopped = match self.buffers.get_mut(args.slot as usize) {
            Some(Some(b)) if save_back => b.take_save(),
            _ => None,
        };
        if let Some(old) = removed {
            self.retire_slot(args.slot, old);
        }
        for (ticket, data, frames) in leftover.into_iter().chain(stopped) {
            self.retire(Garbage::BufferSave {
                slot: args.slot,
                ticket,
                data,
                frames,
            });
        }
        if let Some((take, frames)) = ended {
            self.emit(Event::BufferEnded {
                slot: args.slot,
                take,
                frames,
            });
        }
    }

    /// Replaces the list of buffers recording a voice or a group, or taking
    /// the output mix. The old list leaves as garbage; so does the new one
    /// if its owner is gone.
    pub(crate) fn set_taps(&mut self, args: SetTapsArgs) {
        let target = match args.owner {
            TapOwner::Voice(v) => self.voices.get_mut(v.index()).map(|o| &mut o.taps),
            TapOwner::Group(g) => self.groups.get_mut(g.index()).map(|o| &mut o.taps),
            TapOwner::Out => Some(&mut self.out_taps),
        };
        let old = match target {
            Some(taps) => std::mem::replace(taps, args.taps),
            // a waiting line outlived its owner
            None => args.taps,
        };
        self.retire(Garbage::Taps(old));
    }

    /// Points a voice at other memory (a buffer's, or an empty track).
    pub(crate) fn set_source(&mut self, args: SetSourceArgs) {
        let Some(voice) = self.voices.get_mut(args.voice.index()) else {
            // a waiting line outlived the voice; the new source must not
            // drop here
            self.retire_source(args.source);
            return;
        };
        let channels = match &args.source {
            VoiceSource::Buffer(data) => usize::from(data.channels),
            VoiceSource::Track(_) | VoiceSource::None => voice.channels,
        };
        let old = voice.swap_source(args.source, channels);
        self.retire_source(old);
    }
}

// `retire` over the fields it needs, for a caller that holds the table
// borrowed: the buffer's memory, and its take's copy if it has one
fn retire_slot_into(
    garbage: &mut rtrb::Producer<Garbage>,
    parked: &mut Vec<Garbage>,
    leaked: &mut u32,
    index: u32,
    lost: BufferSlot,
) {
    let (data, save) = lost.into_parts();
    if let Some((ticket, data, frames)) = save {
        super::super::retire_into(
            garbage,
            parked,
            leaked,
            Garbage::BufferSave {
                slot: index,
                ticket,
                data,
                frames,
            },
        );
    }
    super::super::retire_into(garbage, parked, leaked, Garbage::Buffer(data));
}
