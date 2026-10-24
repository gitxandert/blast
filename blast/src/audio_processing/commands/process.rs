//! Processes in general (PRC adds the generic process commands here).

use super::reprs::Idx;
use super::tempo::TempoRepr;

/// One row per command: (name, usage and summary).
pub const HELP: &[(&str, &str)] = &[];

/// What every Process has in common; each kind differs otherwise.
pub struct ProcRepr {
    /// Index of the Process in its owner's `Vec<Process>`.
    pub(super) idx: usize,
    /// Index of the Process's owner in the engine's `Vec<$owner>`.
    #[allow(dead_code)] // not read yet; kept for targeting Processes
    pub(super) owner_idx: Idx,
    pub(super) tempo: Option<TempoRepr>,
}

impl ProcRepr {
    pub(super) fn new(idx: usize, owner_idx: Idx, tempo: Option<TempoRepr>) -> Self {
        Self {
            idx,
            owner_idx,
            tempo,
        }
    }
}
