// shared engine test helpers
//
// drives Conductor::coordinate without ALSA by pointing fake
// channel areas at an interleaved Vec<i16>

use std::{ffi::c_void, collections::HashMap};

use alsa_sys::snd_pcm_channel_area_t;

use crate::audio_processing::engine::Conductor;
use crate::file_parsing::decode_helpers::AudioFile;

// interleaved i16 buffer of frames * channels, like ALSA's mmap areas
pub fn render(conductor: &mut Conductor, channels: usize, frames: usize) -> Vec<i16> {
    // fill with a sentinel so tests can tell if coordinate skipped a sample
    let mut buf = vec![i16::MIN; frames * channels];
    let addr = buf.as_mut_ptr() as *mut c_void;

    let areas: Vec<snd_pcm_channel_area_t> = (0..channels)
        .map(|ch| snd_pcm_channel_area_t {
            addr,
            first: (ch * 16) as u32,
            step: (channels * 16) as u32,
        })
        .collect();

    conductor.coordinate(areas.as_ptr(), 0, frames as _);

    buf
}

// Conductor with tracks keyed by file_name
pub fn conductor(channels: usize, tracks: Vec<AudioFile>) -> Conductor {
    let map: HashMap<String, AudioFile> = tracks
        .into_iter()
        .map(|af| (af.file_name.clone(), af))
        .collect();

    Conductor::prepare(channels, map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_conductor_renders_silence() {
        let mut c = conductor(2, Vec::new());
        let out = render(&mut c, 2, 64);

        assert_eq!(out.len(), 128);
        assert!(out.iter().all(|&s| s == 0));
    }
}
