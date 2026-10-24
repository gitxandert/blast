use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// sample_rate
// (mainly used by TempoState and TempoGroup)
//
pub mod sample_rate {
    use super::*;

    pub static SAMPLE_RATE: AtomicU32 = AtomicU32::new(0);

    pub fn set(sample_rate: u32) {
        SAMPLE_RATE.store(sample_rate, Ordering::Relaxed);
    }

    pub fn get() -> u32 {
        SAMPLE_RATE.load(Ordering::Relaxed)
    }
}

pub mod blast_time {
    use super::*;

    // global clock
    pub mod clock {
        use super::*;

        pub static SAMPLE_COUNTER: AtomicU64 = AtomicU64::new(0);

        pub fn advance(n: u64) {
            SAMPLE_COUNTER.fetch_add(n, Ordering::Relaxed);
        }

        pub fn current() -> u64 {
            SAMPLE_COUNTER.load(Ordering::Relaxed)
        }
    }
    // tempo control
    // 
    // a Process that relies on temporal parameters can be assigned
    // a TempoSolo to function at its own pace,
    // a TempoContext to synchronize with other separate Processes,
    // or a TempoGroup to default to the TempoState of the Group to which it
    //// would be assigned
    //
    // a TempoContext is created by a special command (tempocon/tc);
    // a TempoSolo is created along with the Process that instantiates it;
    // a TempoGroup is created along with the Group that instantiates it
    //
    // a TempoContext has a name that can be assigned to a Process or Group
    //
    // all TempoContexts are updated by the Conductor;
    // TempoSolos are updated by Voices, TempoGroups updated by Groups
    //
    // interval is stored as samples, but converted from
    // samples, milliseconds, or BPM, depending on initialization
    //
    #[derive(Debug)]
    pub struct TempoState {
        pub mode: TempoMode,
        pub unit: TempoUnit,
        pub interval: f32,
        pub active: bool,
        pub current: u64,
        // bumped on every reset, so anything tracking this
        // TempoState's position can tell a restart from a wrap
        pub epoch: u32,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum TempoMode {
        Process,
        Voice,
        Group,
        Context,
        TBD, // used by Voices as a way to refer to a Group TempoState;
             // could extend this for Groups too
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum TempoUnit {
        Samples,
        Millis,
        Bpm,
    }

    impl TempoState {
        pub fn new(opt: Option<TempoMode>) -> Self {
            let mode = match opt {
                Some(tm) => tm,
                None => TempoMode::TBD, // subject to change
            };

            Self {
                mode,
                unit: TempoUnit::Samples,
                interval: sample_rate::get() as f32,
                active: false,
                current: 0,
                epoch: 0,
            }
        }

        pub fn init(&mut self, mode: TempoMode, unit: TempoUnit, interval: f32) {
            let interval_in_samps = convert_interval(&unit, interval);
            self.mode = mode;
            self.unit = unit; 
            self.interval = interval_in_samps;
        }

        pub fn clone(&self) -> TempoState {
            // interval is already in samples, so copy it rather
            // than running it through init() (which converts again)
            let mut clone = TempoState::new(Some(self.mode));
            clone.unit = self.unit;
            clone.interval = self.interval;
            clone
        }

        //
        // paused/stopped TempoStates don't advance
        pub fn update(&mut self, delta_in_samples: f64) {
            if !self.active { return; }
            self.current += delta_in_samples as u64;
        }

        // return current in steps (of interval) as f32;
        // divide in f64, since f32 can't count samples exactly
        // past 2^24 (~6 minutes at 48k)
        pub fn current(&self) -> f32 {
            (self.current as f64 / self.interval as f64) as f32
        }

        // current wrapped into 0..period steps; wrap in f64 so
        // precision doesn't degrade the longer the tempo runs
        pub fn current_in(&self, period: f32) -> f32 {
            ((self.current as f64 / self.interval as f64) % period as f64) as f32
        }

        pub fn start(&mut self) {
            self.reset();
            self.active = true;
        }

        pub fn pause(&mut self) {
            self.active = false;
        }

        pub fn resume(&mut self) {
            self.active = true;
        }

        pub fn stop(&mut self) {
            self.active = false;
            self.reset();
        }

        pub fn reset(&mut self) {
            self.current = 0;
            self.epoch = self.epoch.wrapping_add(1);
        }

        pub fn set_interval(&mut self, new_interval: f32) {
            let new_interval_in_samps = convert_interval(&self.unit, new_interval);
            self.interval = new_interval_in_samps;
        }
    }

    fn convert_interval(unit: &TempoUnit, interval: f32) -> f32 {
        let frac = match unit {
            TempoUnit::Samples => return interval,
            TempoUnit::Millis => interval / 1000.0,
            TempoUnit::Bpm => 60.0 / interval,
        };
        
        let interval_in_samples = sample_rate::get() as f32 * frac;
       
        interval_in_samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::blast_time::*;

    fn tempo(unit: TempoUnit, interval: f32) -> TempoState {
        sample_rate::set(48000);
        let mut ts = TempoState::new(None);
        ts.init(TempoMode::Context, unit, interval);
        ts
    }

    #[test]
    fn new_defaults_to_one_second_tbd() {
        sample_rate::set(48000);
        let ts = TempoState::new(None);
        assert_eq!(ts.mode, TempoMode::TBD);
        assert_eq!(ts.interval, 48000.0);
        assert!(!ts.active);
        assert_eq!(ts.current, 0);
    }

    #[test]
    fn init_converts_interval_to_samples() {
        assert_eq!(tempo(TempoUnit::Samples, 100.0).interval, 100.0);
        assert_eq!(tempo(TempoUnit::Millis, 500.0).interval, 24000.0);
        assert_eq!(tempo(TempoUnit::Bpm, 120.0).interval, 24000.0);
    }

    #[test]
    fn set_interval_keeps_unit() {
        let mut ts = tempo(TempoUnit::Bpm, 120.0);
        ts.set_interval(60.0);
        assert_eq!(ts.interval, 48000.0);
    }

    #[test]
    fn current_is_steps_of_interval() {
        let mut ts = tempo(TempoUnit::Samples, 100.0);
        ts.start();
        for _ in 0..250 {
            ts.update(1.0);
        }
        assert_eq!(ts.current, 250);
        assert_eq!(ts.current(), 2.5);
    }

    #[test]
    fn update_only_advances_while_active() {
        let mut ts = tempo(TempoUnit::Samples, 100.0);
        ts.update(1.0);
        assert_eq!(ts.current, 0);

        ts.start();
        ts.update(1.0);
        assert_eq!(ts.current, 1);

        ts.pause();
        ts.update(1.0);
        assert_eq!(ts.current, 1);

        ts.resume();
        ts.update(1.0);
        assert_eq!(ts.current, 2);
    }

    #[test]
    fn start_stop_reset() {
        let mut ts = tempo(TempoUnit::Samples, 100.0);
        ts.start();
        ts.update(5.0);
        assert_eq!(ts.current, 5);

        ts.reset();
        assert_eq!(ts.current, 0);
        assert!(ts.active);

        ts.update(5.0);
        ts.stop();
        assert_eq!(ts.current, 0);
        assert!(!ts.active);

        ts.update(5.0);
        ts.start();
        assert_eq!(ts.current, 0);
        assert!(ts.active);
    }

    #[test]
    fn current_in_stays_exact_after_hours() {
        let mut ts = tempo(TempoUnit::Bpm, 120.0); // 24000 samples per step
        ts.start();
        // ~3 hours at 48k (21600 steps, a multiple of the period),
        // past where f32 can count single samples
        ts.current = 24000 * 21600 + 12001;
        assert!((ts.current_in(4.0) - 12001.0 / 24000.0).abs() < 1e-6);
        // past where a u32 counter would have overflowed (~25h)
        ts.current = u32::MAX as u64 + 1;
        let want = ((u32::MAX as f64 + 1.0) / 24000.0 % 4.0) as f32;
        assert!((ts.current_in(4.0) - want).abs() < 1e-6);
    }

    #[test]
    fn reset_bumps_epoch() {
        let mut ts = tempo(TempoUnit::Samples, 100.0);
        let e = ts.epoch;
        ts.start();
        assert_eq!(ts.epoch, e + 1);
        ts.stop();
        assert_eq!(ts.epoch, e + 2);
        ts.pause();
        ts.resume();
        ts.update(5.0);
        assert_eq!(ts.epoch, e + 2);
    }

    #[test]
    fn clone_keeps_interval_in_samples() {
        let ts = tempo(TempoUnit::Bpm, 120.0);
        let clone = ts.clone();
        assert_eq!(clone.interval, 24000.0);
        assert_eq!(clone.unit, TempoUnit::Bpm);
        assert_eq!(clone.mode, TempoMode::Context);
    }
}
