use std::rc::Rc;
use std::cell::RefCell;

use crate::audio_processing::{
    blast_rand::X128P,
    engine::VoiceState,
    blast_time::blast_time::{TempoState, TempoMode},
};

// Processes
//
macro_rules! processes {
    ( $( $variant:ident ),* $(,)? ) => {
        pub enum Process {
            $(
                $variant($variant),
            )*
        }

        impl Process {
            pub fn process(&mut self, voice: &mut VoiceState) {
                match self {
                    $(
                        Process::$variant(inner) => inner.process(voice),
                    )*
                }
            }

            pub fn reset(&mut self) {
                match self {
                    $(
                        Process::$variant(inner) => inner.reset(),
                    )*
                }
            }

            pub fn update_tempo(&mut self, ts: Rc<RefCell<TempoState>>) {
                match self {
                    $(
                        Process::$variant(inner) => inner.update_tempo(ts),
                    )*
                }
            }
        }
    };
}

processes! {
    Seq,
}

pub struct Seq {
    pub state: SeqState,
}

pub struct SeqState {
    pub active: bool, // TODO: impl activation methods
    pub tempo: Rc<RefCell<TempoState>>,
    pub period: usize,
    pub steps: Vec<f32>,
    pub chance: Vec<f32>,
    pub jit: Vec<f32>,
    pub rng: X128P, // TODO: impl user-defined seed
    pub idx: usize,
}

impl Seq {
    // right now only retriggers samples
    fn process(&mut self, voice: &mut VoiceState) {
        if !self.state.active { return; }

        let state = &mut self.state;
        let tempo = state.tempo.borrow();

        if !tempo.active { return; }

        let current = tempo.current() % state.period as f32;

        if current == state.steps[state.idx] {
            let rand = state.rng.next_i64_range(0, 100);
            if rand < state.chance[state.idx] as i64 {
                voice.position = match voice.velocity >= 0.0 {
                    true => 0.0,
                    false => voice.end as f32,
                };
            }
            state.idx += 1;
            state.idx %= state.steps.len();
        }
    }

    fn reset(&mut self) {
        self.state.idx = 0;
    }

    fn update_tempo(&mut self, ts: Rc<RefCell<TempoState>>) {
        self.state.tempo = ts;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::audio_processing::{
        commands::{CmdProcessor, EngineState},
        blast_time::sample_rate,
        engine::Conductor,
        test_util::{conductor, render},
    };
    use crate::file_parsing::decode_helpers::AudioFile;

    // mono track whose first sample is -1 and every other is 1,
    // so a frame renders -1 only when the Voice is (re)triggered
    const LEN: usize = 400_000;

    fn kick() -> AudioFile {
        let mut samples = vec![1i16; LEN];
        samples[0] = -1;
        AudioFile::new("kick", "wav", 48000, 1, 16, samples)
    }

    // runs each command through the parser and into the engine;
    // render at 1 channel so tempos tick once per frame
    fn setup(cmds: &[&str]) -> Conductor {
        sample_rate::set(48000);
        let mut cp = CmdProcessor::new(EngineState::new(vec![kick()], 1));
        let mut c = conductor(1, vec![kick()]);
        for cmd in cmds {
            match cp.parse(cmd.to_string()) {
                Ok(command) => c.apply(command),
                Err(e) => panic!("'{}' failed: {}", cmd, e),
            }
        }
        c
    }

    fn triggers(out: &[i16]) -> Vec<usize> {
        out.iter()
            .enumerate()
            .filter(|(_, s)| **s == -1)
            .map(|(f, _)| f)
            .collect()
    }

    #[test]
    fn seq_without_tempo_runs() {
        // default tempo is 120 BPM: one step every 24000 frames
        let mut c = setup(&["load kick", "seq kick -s 0,1", "start -v kick"]);
        let out = render(&mut c, 1, 30000);
        assert_eq!(triggers(&out), vec![0, 24000]);
    }

    #[test]
    fn seq_added_to_playing_voice_runs() {
        let mut c = setup(&["load kick", "start -v kick", "seq kick -s 0,1"]);
        let out = render(&mut c, 1, 30000);
        assert_eq!(triggers(&out), vec![0, 24000]);
    }

    #[test]
    fn seq_with_own_tempo() {
        // s:1000 -> one step every 1000 frames, period 4
        let mut c = setup(&["load kick", "seq kick -t s:1000 -s 0,2", "start -v kick"]);
        let out = render(&mut c, 1, 8500);
        assert_eq!(triggers(&out), vec![0, 2000, 4000, 6000, 8000]);
    }

    #[test]
    fn seq_with_voice_tempo() {
        let mut c = setup(&["load kick -t s:1000", "seq kick -t v -p 2 -s 1", "start -v kick"]);
        let out = render(&mut c, 1, 5500);
        // frame 0 is the start itself
        assert_eq!(triggers(&out), vec![0, 1000, 3000, 5000]);
    }
}
