//! Processes: per-Voice or per-Group behaviour run once per frame.

use crate::audio_processing::{
    blast_time::blast_time::TempoState,
    commands::{GroupId, ProcOwner, RecvSpec, SeqEditArgs, SeqVecs, VoiceId},
    engine::{Group, Voice, arena::Arena},
};

pub mod env;
pub mod lfo;
pub mod seq;
pub use env::*;
pub use lfo::*;
pub use seq::*;

// Processes
//
macro_rules! processes {
    ( $( $variant:ident ),* $(,)? ) => {
        /// A per-frame behaviour owned by a Voice or a Group.
        pub enum Process {
            $(
                $variant($variant),
            )*
        }

        impl Process {
            /// Runs one frame, on the tempo it reads. A process can reach any
            /// voice (its receivers name them), so it gets the arenas; the
            /// caller has checked that `owner` is playing.
            pub fn process(
                &mut self,
                voices: &mut Arena<Voice>,
                groups: &mut Arena<Group>,
                owner: ProcOwner,
                tempo: &TempoState,
            ) {
                match self {
                    $(
                        Process::$variant(inner) => inner.process(voices, groups, owner, tempo),
                    )*
                }
            }

            /// Replaces its receivers; the old ones come back, to leave
            /// through the garbage queue.
            pub fn set_recv(&mut self, recv: Vec<RecvSpec>) -> Vec<RecvSpec> {
                match self {
                    $(
                        Process::$variant(inner) => inner.set_recv(recv),
                    )*
                }
            }

            /// Whether it hits voice `v` (in group `group`) other than as
            /// its owner.
            pub fn targets(&self, v: VoiceId, group: Option<GroupId>) -> bool {
                match self {
                    $(
                        Process::$variant(inner) => inner.targets(v, group),
                    )*
                }
            }

            /// Skips what passed while its owner was paused.
            pub fn resync(&mut self) {
                match self {
                    $(
                        Process::$variant(inner) => inner.resync(),
                    )*
                }
            }

            /// Back to the start, as when its owner starts or stops.
            pub fn reset(&mut self) {
                match self {
                    $(
                        Process::$variant(inner) => inner.reset(),
                    )*
                }
            }

            /// Pauses (`false`) or resumes (`true`) its firing. Resuming
            /// skips what passed while it was paused.
            pub fn set_enabled(&mut self, on: bool) {
                match self {
                    $(
                        Process::$variant(inner) => inner.set_enabled(on),
                    )*
                }
            }

            /// Whether it triggers voices and isn't paused: such a process
            /// makes a voice wait for its steps. A modulating process never
            /// does.
            pub fn fires(&self) -> bool {
                match self {
                    $(
                        Process::$variant(inner) => inner.fires(),
                    )*
                }
            }

            /// Whether it fires (it isn't paused).
            pub fn enabled(&self) -> bool {
                match self {
                    $(
                        Process::$variant(inner) => inner.enabled(),
                    )*
                }
            }

            /// Swaps in new data without a restart; the replaced vectors
            /// come back, to leave through the garbage queue. `instant`:
            /// glides land at once.
            pub fn edit(&mut self, args: SeqEditArgs, instant: bool) -> SeqVecs {
                match self {
                    $(
                        Process::$variant(inner) => inner.edit(args, instant),
                    )*
                }
            }
        }
    };
}

processes! {
    Seq,
    Lfo,
    Env,
}

#[cfg(test)]
mod recv_tests;
#[cfg(test)]
mod seq_edit_tests;
