use std::rc::Rc;
use std::cell::RefCell;

use crate::audio_processing::{
    blast_rand::X128P,
    engine::{Voice, VoiceState},
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

            // a Process owned by a Group acts on all of its Voices
            pub fn process_group(&mut self, voices: &mut [Voice]) {
                match self {
                    $(
                        Process::$variant(inner) => inner.process_group(voices),
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
        if self.fire() {
            retrigger(voice);
        }
    }

    // a Group Seq retriggers every Voice in the Group
    // on the same step
    fn process_group(&mut self, voices: &mut [Voice]) {
        if self.fire() {
            for v in voices {
                retrigger(&mut v.state);
            }
        }
    }

    // advance through the steps; returns whether this step
    // should retrigger
    fn fire(&mut self) -> bool {
        if !self.state.active { return false; }

        let state = &mut self.state;
        let tempo = state.tempo.borrow();

        if !tempo.active { return false; }

        let current = tempo.current() % state.period as f32;

        let mut fired = false;
        if current == state.steps[state.idx] {
            let rand = state.rng.next_i64_range(0, 100);
            if rand < state.chance[state.idx] as i64 {
                fired = true;
            }
            state.idx += 1;
            state.idx %= state.steps.len();
        }

        fired
    }

    fn reset(&mut self) {
        self.state.idx = 0;
    }

    fn update_tempo(&mut self, ts: Rc<RefCell<TempoState>>) {
        self.state.tempo = ts;
    }
}

fn retrigger(voice: &mut VoiceState) {
    voice.position = match voice.velocity >= 0.0 {
        true => 0.0,
        false => voice.end as f32,
    };
}
