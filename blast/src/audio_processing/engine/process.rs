//! Process slots: every Process lives in the Conductor's process arena.

use crate::audio_processing::commands::{ProcOwner, TempoId};
use crate::audio_processing::processes::Process;

/// A Process, who owns it (and so whom it acts on), and the tempo it reads.
pub struct ProcSlot {
    pub owner: ProcOwner,
    pub tempo: TempoId,
    pub process: Process,
    /// Changes whenever an edit starts a glide on the process (`thenif`
    /// compares it).
    pub generation: u32,
}
