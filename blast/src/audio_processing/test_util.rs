// shared engine test helpers

use std::collections::HashMap;

use crate::audio_processing::commands::{
    CmdProcessor, EngineState, GroupId, Parsed, TempoId, VoiceId,
};
use crate::audio_processing::engine::arena::Limits;
use crate::audio_processing::engine::{Conductor, EngineRx};
use crate::file_parsing::decode_helpers::AudioFile;

// interleaved f32 buffer of frames * channels, straight from render
pub fn render(conductor: &mut Conductor, channels: usize, frames: usize) -> Vec<f32> {
    // fill with a sentinel so tests can tell if render skipped a sample
    let mut buf = vec![f32::NAN; frames * channels];
    conductor.render(&mut buf, channels);
    buf
}

// a rendered sample as i16, clipped to the i16 range
pub fn to_i16(x: f32) -> i16 {
    (x * 32768.0).clamp(-32768.0, 32767.0) as i16
}

// render, converted to i16
pub fn render_i16(conductor: &mut Conductor, channels: usize, frames: usize) -> Vec<i16> {
    render(conductor, channels, frames)
        .into_iter()
        .map(to_i16)
        .collect()
}

// Voices made by these helpers (and so by the tests that use them) start with
// `resample=cubic`: most tests read exact source values and exact onsets,
// which the band-limited default (`iir`) doesn't give. The tests of `iir`
// say `resample=iir`, or call `ResampleMode::set_initial` themselves.
fn exact_reads() {
    crate::audio_processing::commands::ResampleMode::set_initial(
        crate::audio_processing::commands::ResampleMode::Cubic,
    );
}

// Conductor with tracks keyed by file_name
pub fn conductor(channels: usize, tracks: Vec<AudioFile>) -> Conductor {
    exact_reads();
    let map: HashMap<String, AudioFile> = tracks
        .into_iter()
        .map(|af| (af.file_name.clone(), af))
        .collect();

    Conductor::prepare(channels, map)
}

// CmdProcessor and Conductor whose track IDs agree, built the way
// run_blast builds them
pub fn setup(channels: usize, tracks: Vec<AudioFile>) -> (CmdProcessor, Conductor) {
    exact_reads();
    let map: HashMap<String, AudioFile> = tracks
        .into_iter()
        .map(|af| (af.file_name.clone(), af))
        .collect();

    let state = EngineState::new(map.clone().into_values().collect(), channels);

    (CmdProcessor::new(state), Conductor::prepare(channels, map))
}

// like setup, with both sides sized by the same `limits`
pub fn setup_with(
    limits: Limits,
    channels: usize,
    tracks: Vec<AudioFile>,
) -> (CmdProcessor, Conductor) {
    exact_reads();
    let state = EngineState::with_limits(tracks.clone(), channels, limits);
    (
        CmdProcessor::new(state),
        Conductor::keeping_rx(limits, channels, tracks),
    )
}

// the REPL ends of the Conductor's queues; panics if already taken
pub fn rx(conductor: &mut Conductor) -> EngineRx {
    conductor.take_rx().expect("queue ends already taken")
}

// the ID of the voice named `kick` or `g.kick`
pub fn vid(cp: &CmdProcessor, name: &str) -> VoiceId {
    cp.engine_state.voice_id(name).unwrap()
}

// the ID of the group named `name`
pub fn gid(cp: &CmdProcessor, name: &str) -> GroupId {
    cp.engine_state.group_id(name).unwrap()
}

// the tempo of the context named `name`
pub fn ctx(cp: &CmdProcessor, name: &str) -> TempoId {
    cp.engine_state.context_id(name).unwrap()
}

// parse a line of the language and apply it to the Conductor;
// panics if it doesn't parse
pub fn run(cp: &mut CmdProcessor, conductor: &mut Conductor, cmd: &str) {
    match cp.parse(cmd) {
        Ok(Parsed::Engine(command)) => conductor.apply(command),
        Ok(Parsed::Local(_)) => (),
        Err(error) => panic!("'{}' failed: {}", cmd, error),
    }
}

// mono track whose samples are all `val`
pub fn track(name: &str, val: i16, frames: usize) -> AudioFile {
    AudioFile::new(name, "wav", 48000, 1, 16, pcm(&vec![val; frames]))
}

// i16 test values as f32 track samples; render_i16 gives them back exactly
pub fn pcm(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| f32::from(s) / 32768.0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_conductor_renders_silence() {
        let mut c = conductor(2, Vec::new());
        let out = render(&mut c, 2, 64);

        assert_eq!(out.len(), 128);
        assert!(out.iter().all(|&s| s == 0.0));
    }
}
