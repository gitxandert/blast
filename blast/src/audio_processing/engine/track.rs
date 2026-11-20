//! Tracks: decoded audio shared by every Voice that plays it.

use std::sync::Arc;

use crate::file_parsing::decode_helpers::AudioFile;

/// A loaded track. Built on the main thread at `prepare`; Voices hold clones
/// of `samples`, so loading a Voice copies nothing.
pub struct Track {
    /// Interleaved, full scale -1.0..1.0.
    pub samples: Arc<[f32]>,
    /// Interleaved channels in `samples`.
    pub channels: u32,
    /// Frames in `samples`.
    pub frames: usize,
}

impl Track {
    /// Moves `af`'s samples into a shared buffer.
    pub fn from_file(af: AudioFile) -> Track {
        let frames = af.samples.len() / (af.num_channels as usize).max(1);
        Track {
            samples: af.samples.into(),
            channels: af.num_channels,
            frames,
        }
    }
}
