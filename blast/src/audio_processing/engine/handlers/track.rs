//! Track commands (TRK): the engine inserts or removes the `Arc` a command
//! carries. It never builds, copies or frees sample memory.

use crate::audio_processing::commands::{AddTrackArgs, DropTrackArgs};
use crate::audio_processing::engine::track::Track;
use crate::audio_processing::engine::{Conductor, Garbage};

impl Conductor {
    /// Puts a track's samples in its slot. If the slot is taken (a
    /// re-import), the new samples take its place and the old `Arc` is
    /// retired: voices that still hold it let go on their `SetTrack`.
    pub(crate) fn add_track(&mut self, args: AddTrackArgs) {
        let frames = args.samples.len() / (args.channels as usize).max(1);
        let track = Track {
            samples: args.samples,
            channels: args.channels,
            frames,
        };
        if let Some(slot) = self.tracks.get_mut(args.id.index()) {
            let old = std::mem::replace(slot, track);
            self.retire(Garbage::Track(old.samples));
            return;
        }
        if let Err(track) = self.tracks.insert(args.id.index(), track) {
            // out of range: a mirror bug; the samples must not drop here
            debug_assert!(false, "add_track to slot {:?} failed", args.id);
            self.retire(Garbage::Track(track.samples));
        }
    }

    /// Removes a track. Voices playing it hold their own `Arc`, so the last
    /// reference is dropped on the REPL side, through the garbage queue.
    pub(crate) fn drop_track(&mut self, args: DropTrackArgs) {
        if let Some(track) = self.tracks.remove(args.id.index()) {
            self.retire(Garbage::Track(track.samples));
        } else {
            debug_assert!(false, "drop of a missing track {:?}", args.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::audio_processing::blast_time::sample_rate;
    use crate::audio_processing::commands::{AddTrackArgs, Command, DropTrackArgs, TrackId};
    use crate::audio_processing::engine::Garbage;
    use crate::audio_processing::test_util::{conductor, render_i16, run, rx, setup, track};

    fn add(id: u32, samples: &[f32]) -> Command {
        Command::AddTrack(AddTrackArgs {
            id: TrackId(id),
            samples: Arc::from(samples),
            channels: 1,
        })
    }

    #[test]
    fn add_track_fills_slot() {
        let mut c = conductor(1, Vec::new());
        c.apply(add(3, &[0.5; 8]));
        let t = c.tracks.get(3).expect("slot 3 is filled");
        assert_eq!((t.channels, t.frames, t.samples.len()), (1, 8, 8));
        assert!(c.tracks.get(0).is_none());
    }

    #[test]
    fn drop_track_sends_samples_to_garbage() {
        let mut c = conductor(1, Vec::new());
        let mut queues = rx(&mut c);
        c.apply(add(0, &[0.25; 4]));
        c.apply(Command::DropTrack(DropTrackArgs { id: TrackId(0) }));
        assert!(c.tracks.get(0).is_none());
        let Ok(Garbage::Track(samples)) = queues.garbage.pop() else {
            panic!("the samples weren't retired");
        };
        // the last reference: it frees here, on the REPL side
        assert_eq!(Arc::strong_count(&samples), 1);
        assert!(queues.garbage.pop().is_err());
    }

    #[test]
    fn drop_with_playing_voice_keeps_voice_audio() {
        // the mirror refuses this, but the engine must stay safe
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![track("kick", 1000, 16)]);
        let mut queues = rx(&mut c);
        run(&mut cp, &mut c, "voice v from \"kick\" on 1000ms");
        run(&mut cp, &mut c, "start v");
        assert_eq!(render_i16(&mut c, 1, 2), [1000, 1000]);
        c.apply(Command::DropTrack(DropTrackArgs { id: TrackId(0) }));
        assert_eq!(render_i16(&mut c, 1, 2), [1000, 1000]);
        // the voice still holds the samples, so nothing was freed
        let Ok(Garbage::Track(samples)) = queues.garbage.pop() else {
            panic!("the samples weren't retired");
        };
        assert_eq!(Arc::strong_count(&samples), 2);
    }

    #[test]
    fn add_into_occupied_slot_garbages_old() {
        let mut c = conductor(1, Vec::new());
        let mut queues = rx(&mut c);
        c.apply(add(1, &[0.5; 4]));
        c.apply(add(1, &[0.75; 6]));
        assert_eq!(c.tracks.get(1).unwrap().frames, 6);
        let Ok(Garbage::Track(old)) = queues.garbage.pop() else {
            panic!("the old samples weren't retired");
        };
        assert_eq!((old.len(), Arc::strong_count(&old)), (4, 1));
        assert!(queues.garbage.pop().is_err());
    }
}
