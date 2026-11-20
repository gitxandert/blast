//! The engine's outgoing queues (README §3.4): events (engine → REPL
//! notices) and garbage (objects leaving the audio thread to be freed on the
//! REPL side).

use crate::audio_processing::commands::{Command, EngineState, SeqVecs, TempoOwner};
use crate::audio_processing::engine::buffer::{BufData, BufferSlot};
use crate::audio_processing::engine::{FxSlot, Group, Voice};
use crate::audio_processing::processes::Process;

/// Engine → REPL notices. Small and `Copy`; dropped when the queue is full.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// A voice or group started or resumed on a tempo that isn't running.
    TempoInactive { owner: TempoOwner },
    /// A scheduled line was retired: applied, or dropped because something
    /// it names was removed. `seq_no` is the line's number (the order lines
    /// arrived in, from 0), so the mirror forgets the right one. `total`
    /// counts all retired lines since the engine began, so a lost event is
    /// made good by the next. `applied` is false for a line that was dropped
    /// unapplied (an object it names was removed, a `thenif` was cut short, its
    /// script was cancelled): the mirror then undoes what the line would have
    /// created or removed (PKb2).
    ScheduleFired {
        seq_no: u64,
        total: u64,
        applied: bool,
    },
    /// A `thenif` entry was dropped: a glide it watched was cut short (the
    /// index into its watch list), or an object it names is gone
    /// (`u16::MAX`). `seq_no` is the entry's number; the REPL says why.
    ChainDropped { seq_no: u64, watch: u16 },
    /// The garbage queue and the park were both full, so objects were leaked
    /// instead of freed on the audio thread.
    GarbageLeaked { count: u32 },
    /// A buffer's take ended: `stop` was applied, or it filled without
    /// `loop`. `take` is the number the `start` carried; `frames` were recorded. The REPL feeds it to the mirror.
    BufferEnded { slot: u32, take: u32, frames: u32 },
    /// `quit` was applied; the REPL ends the program.
    Quit,
}

/// Objects leaving the audio thread, to be dropped on the REPL side.
// a Voice is large (its parameter and modulation banks); boxing it here would
// allocate on the audio thread
#[allow(clippy::large_enum_variant)]
pub enum Garbage {
    Voice(Voice),
    Process(Process),
    /// A removed group, which owns its member list.
    Group(Group),
    /// The samples a voice stopped playing (`SetTrack`).
    Track(std::sync::Arc<[f32]>),
    /// A buffer's memory: the engine let go of it (removed, or replaced).
    Buffer(std::sync::Arc<BufData>),
    /// The buffer table an insert outgrew (empty; it owns the memory).
    BufferTable(Vec<Option<BufferSlot>>),
    /// The private copy of a take with `to=`, handed back when the take
    /// ended (or its buffer went). `frames` is the furthest frame written.
    BufferSave {
        slot: u32,
        ticket: u64,
        data: Box<[f32]>,
        frames: u32,
    },
    /// A voice's shifters, replaced or stubbed (REQ-099).
    Shifters(Box<crate::audio_processing::engine::shift::VoiceShifters>),
    /// The tap list a voice or group replaced.
    Taps(Vec<u32>),
    /// The vectors a seq edit replaced.
    SeqData(SeqVecs),
    /// The effect chain an insert outgrew (empty; it owns the buffer).
    FxChain(Vec<FxSlot>),
    /// A command's emptied `Vec`s (the `Batch` list).
    Commands(Vec<Command>),
    /// The box of a scheduled command that fired (its contents were moved
    /// out, so it holds an empty `Batch`), or of one dropped unapplied
    /// because something it names was removed.
    Scheduled(Box<Command>),
    /// A table wave's table, replaced or removed with its wave (WAVb); the
    /// last `Arc` never drops on the audio thread.
    WaveTable(std::sync::Arc<crate::audio_processing::engine::WaveTable>),
    /// The glides a `thenif` entry watched (its list of `GlideWatch`).
    Watch(Box<[crate::audio_processing::commands::GlideWatch]>),
}

/// The REPL-side ends of the engine's queues.
pub struct EngineRx {
    pub events: rtrb::Consumer<Event>,
    pub garbage: rtrb::Consumer<Garbage>,
}

impl EngineRx {
    /// Drops everything in the garbage queue; returns how many.
    pub fn drain_garbage(&mut self) -> usize {
        let mut n = 0;
        while self.garbage.pop().is_ok() {
            n += 1;
        }
        n
    }

    /// Takes every pending garbage object, drops them, and returns the
    /// takes' copies (`Garbage::BufferSave`) as `(slot, ticket, data,
    /// frames)` for the mirror.
    pub fn drain_saves(&mut self) -> Vec<(u32, u64, Box<[f32]>, u32)> {
        let mut saves = Vec::new();
        while let Ok(g) = self.garbage.pop() {
            if let Garbage::BufferSave {
                slot,
                ticket,
                data,
                frames,
            } = g
            {
                saves.push((slot, ticket, data, frames));
            }
        }
        saves
    }

    /// Pops every pending event.
    pub fn events(&mut self) -> impl Iterator<Item = Event> + '_ {
        std::iter::from_fn(|| self.events.pop().ok())
    }
}

/// One line for the terminal, with names from the mirror when the object still exists.
pub fn describe(event: &Event, names: &EngineState) -> String {
    match *event {
        Event::TempoInactive { owner } => {
            let what = match owner {
                TempoOwner::Voice(v) => match names.display_name(v) {
                    Some(name) => format!("voice '{name}'"),
                    None => format!("voice #{}", v.0),
                },
                TempoOwner::Group(g) => match names.groups.get(&g) {
                    Some(group) => format!("group '{}'", group.name),
                    None => format!("group #{}", g.0),
                },
                TempoOwner::Context => "tempo context".to_string(),
            };
            format!("Warn: tempo not active for {what}")
        }
        // the REPL feeds it to the mirror; it prints nothing
        Event::ScheduleFired { .. } => String::new(),
        // the mirror knows the line and the names of its glides
        Event::ChainDropped { seq_no, watch } => names.chain_dropped_text(seq_no, watch),
        Event::GarbageLeaked { count } => {
            format!("Warn: leaked {count} removed objects (garbage queue full)")
        }
        // the REPL feeds it to the mirror; it prints nothing
        Event::BufferEnded { .. } => String::new(),
        Event::Quit => "Quitting".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::commands::{CmdProcessor, Parsed};
    use crate::audio_processing::commands::{GroupId, VoiceId};
    use crate::audio_processing::test_util::track;

    fn parse(cp: &mut CmdProcessor, line: &str) {
        assert!(matches!(cp.parse(line), Ok(Parsed::Engine(_))), "{line}");
    }

    #[test]
    fn describe_names_live_objects_and_falls_back() {
        let files = vec![track("kick", 1, 4), track("snare", 1, 4)];
        let mut cp = CmdProcessor::new(EngineState::new(files, 2));
        parse(&mut cp, "voice kick from \"kick\" on 1000ms");
        parse(&mut cp, "voice snare from \"snare\" on 1000ms");
        parse(&mut cp, "group g on 240b { snare }");
        let es = &cp.engine_state;
        let kick = es.voice_id("kick").unwrap();
        let snare = es.voice_id("g.snare").unwrap();
        let g = es.group_id("g").unwrap();

        let text = |owner| describe(&Event::TempoInactive { owner }, es);
        assert_eq!(
            text(TempoOwner::Voice(kick)),
            "Warn: tempo not active for voice 'kick'"
        );
        assert_eq!(
            text(TempoOwner::Voice(snare)),
            "Warn: tempo not active for voice 'g.snare'"
        );
        assert_eq!(
            text(TempoOwner::Group(g)),
            "Warn: tempo not active for group 'g'"
        );
        assert_eq!(
            text(TempoOwner::Voice(VoiceId(3))),
            "Warn: tempo not active for voice #3"
        );
        assert_eq!(
            text(TempoOwner::Group(GroupId(7))),
            "Warn: tempo not active for group #7"
        );
        assert_eq!(
            describe(&Event::GarbageLeaked { count: 2 }, es),
            "Warn: leaked 2 removed objects (garbage queue full)"
        );
    }
}
