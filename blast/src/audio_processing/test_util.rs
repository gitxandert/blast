// shared engine test helpers
//
// drives Conductor::coordinate without ALSA by pointing fake
// channel areas at an interleaved Vec<i16>

use std::{ffi::c_void, collections::HashMap};

use alsa_sys::snd_pcm_channel_area_t;

use crate::audio_processing::engine::Conductor;
use crate::audio_processing::commands::{CmdProcessor, EngineState};
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

// CmdProcessor and Conductor whose track indices agree, built the way
// run_blast builds them (EngineState gets a clone of the same HashMap,
// so both iterate the tracks in the same order)
pub fn setup(channels: usize, tracks: Vec<AudioFile>) -> (CmdProcessor, Conductor) {
    let map: HashMap<String, AudioFile> = tracks
        .into_iter()
        .map(|af| (af.file_name.clone(), af))
        .collect();

    let state = EngineState::new(map.clone().into_values().collect(), channels);

    (CmdProcessor::new(state), Conductor::prepare(channels, map))
}

// parse a command line and apply it to the Conductor;
// panics if the command doesn't parse
pub fn run(cp: &mut CmdProcessor, conductor: &mut Conductor, cmd: &str) {
    match cp.parse(cmd.to_string()) {
        Ok(command) => conductor.apply(command),
        Err(error) => panic!("'{}' failed: {}", cmd, error),
    }
}

// mono track whose samples are all `val`
pub fn track(name: &str, val: i16, frames: usize) -> AudioFile {
    AudioFile::new(name, "wav", 48000, 1, 16, vec![val; frames])
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
