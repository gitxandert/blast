//! Tracks (TRK adds `import`, `rescan` and `drop` here).

/// One row per command: (name, usage and summary).
pub const HELP: &[(&str, &str)] = &[];

/// A loaded track: its index in the engine's track list.
pub struct TrackRepr {
    pub(super) idx: usize,
}

impl TrackRepr {
    pub(super) fn new(idx: usize) -> Self {
        Self { idx }
    }
}
