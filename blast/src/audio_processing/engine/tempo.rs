//! Tempos: every TempoState lives in the Conductor's tempo arena.

use crate::audio_processing::blast_time::blast_time::TempoState;
use crate::audio_processing::commands::TempoOwner;

/// A TempoState and whose transport runs it. The Conductor ticks it once
/// per frame while its owner plays (a context always).
pub struct Tempo {
    pub state: TempoState,
    pub owner: TempoOwner,
    /// Changes whenever a glide starts or a plain retime replaces one
    /// (`thenif` compares it).
    pub glide_gen: u32,
}
