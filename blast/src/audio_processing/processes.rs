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
    pub idx: usize, // index of the step after the last one fired
    pub last: Option<f32>, // position at the previous process()
}

impl Seq {
    // right now only retriggers samples
    fn process(&mut self, voice: &mut VoiceState) {
        if !self.state.active { return; }

        let state = &mut self.state;
        let tempo = state.tempo.borrow();

        if !tempo.active { return; }

        // nothing to sequence (the parser rejects these anyway)
        if state.steps.is_empty() || state.period == 0 { return; }

        let period = state.period as f32;
        let current = tempo.current() % period;

        // steps fire when the position crosses them, since
        // step * interval is rarely a whole number of samples;
        // without a previous position (after reset), act as if
        // the previous sample was one sample earlier
        let last = match state.last {
            Some(l) => l,
            None => current - 1.0 / tempo.interval,
        };
        state.last = Some(current);

        // tempo didn't move (e.g. called again in the same frame)
        if current == last { return; }

        // the position wrapped past the end of the period
        let wrapped = current < last;

        let mut retrigger = false;
        for i in 0..state.steps.len() {
            let step = state.steps[i];
            let crossed = match wrapped {
                false => last < step && step <= current,
                true => (last < step && step < period) || step <= current,
            };

            if crossed {
                let rand = state.rng.next_i64_range(0, 100);
                if rand < state.chance[i] as i64 {
                    retrigger = true;
                }
                state.idx = (i + 1) % state.steps.len();
            }
        }

        if retrigger {
            voice.position = match voice.velocity >= 0.0 {
                true => 0.0,
                false => voice.end as f32,
            };
        }
    }

    fn reset(&mut self) {
        self.state.idx = 0;
        self.state.last = None;
    }

    fn update_tempo(&mut self, ts: Rc<RefCell<TempoState>>) {
        self.state.tempo = ts;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::audio_processing::{
        commands::{CmdProcessor, EngineState, Command, SeqArgs, StartArgs, TempoRepr, Idx},
        blast_rand::fast_seed,
        blast_time::{sample_rate, blast_time::TempoUnit},
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
    #[test]
    fn seq_with_no_steps_or_period_doesnt_panic() {
        for (period, steps) in [(4, Vec::new()), (0, vec![0.0])] {
            let mut c = setup(&["load kick"]);
            let tempo = TempoRepr {
                idx: 0,
                owned: true,
                mode: TempoMode::Process,
                unit: TempoUnit::Samples,
                interval: 100.0,
            };
            c.apply(Command::Seq(SeqArgs {
                idx: Idx::Voice(0),
                tempo,
                period,
                chance: vec![100.0; steps.len()],
                jit: vec![0.0; steps.len()],
                steps,
                rng: X128P::new(fast_seed()),
            }));
            c.apply(Command::Start(StartArgs { idx: Idx::Voice(0) }));
            let out = render(&mut c, 1, 1000);
            assert_eq!(triggers(&out), vec![0]);
        }
    }
    fn assert_near(got: &[usize], want: &[f64]) {
        assert_eq!(got.len(), want.len(), "got {:?}, want {:?}", got, want);
        for (g, w) in got.iter().zip(want) {
            assert!((*g as f64 - w.ceil()).abs() <= 1.0, "got {:?}, want {:?}", got, want);
        }
    }

    #[test]
    fn seq_fires_on_non_integer_intervals() {
        // b:140 at 48k is 20571.43 samples per step
        let interval = 48000.0 * 60.0 / 140.0;
        let mut c = setup(&["load kick", "seq kick -t b:140 -s 0,1,2,3", "start -v kick"]);
        // 3 periods (12 steps) minus a little, so step 12 isn't reached
        let out = render(&mut c, 1, 246000);
        let want: Vec<f64> = (0..12).map(|k| k as f64 * interval).collect();
        assert_near(&triggers(&out), &want);
    }

    #[test]
    fn seq_fires_on_fractional_steps() {
        let mut c = setup(&["load kick", "seq kick -t b:120 -s 0,0.5,1", "start -v kick"]);
        let out = render(&mut c, 1, 3 * 96000);
        let mut want = Vec::new();
        for p in 0..3 {
            for f in [0, 12000, 24000] {
                want.push(p * 96000 + f);
            }
        }
        assert_eq!(triggers(&out), want);
    }

    #[test]
    fn seq_fires_on_millis_interval() {
        // m:3.33 -> 159.84 samples per step
        let interval = 48000.0 * 3.33 / 1000.0;
        let mut c = setup(&["load kick", "seq kick -t m:3.33 -p 2 -s 0,1.5", "start -v kick"]);
        let out = render(&mut c, 1, 1500);
        let mut want = Vec::new();
        let mut p = 0.0;
        while (p + 1.5) * interval < 1499.0 {
            want.push(p * interval);
            want.push((p + 1.5) * interval);
            p += 2.0;
        }
        if p * interval < 1499.0 { want.push(p * interval); }
        assert_near(&triggers(&out), &want);
    }

    #[test]
    fn seq_steps_out_of_order_and_past_period() {
        let mut c = setup(&["load kick", "seq kick -t s:1000 -s 2,0", "start -v kick"]);
        let out = render(&mut c, 1, 8500);
        assert_eq!(triggers(&out), vec![0, 2000, 4000, 6000, 8000]);

        // step 3 is outside period 2, so it never fires
        let mut c = setup(&["load kick", "seq kick -t s:1000 -p 2 -s 0,3", "start -v kick"]);
        let out = render(&mut c, 1, 4500);
        assert_eq!(triggers(&out), vec![0, 2000, 4000]);
    }

    #[test]
    fn seq_restarts_with_voice() {
        let mut c = setup(&["load kick", "seq kick -t s:1000 -s 1", "start -v kick"]);
        let out = render(&mut c, 1, 1500);
        assert_eq!(triggers(&out), vec![0, 1000]);

        // restarting mid-period resets the seq and its tempo
        c.apply(Command::Start(StartArgs { idx: Idx::Voice(0) }));
        let out = render(&mut c, 1, 1500);
        assert_eq!(triggers(&out), vec![0, 1000]);
    }
}
