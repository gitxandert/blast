use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// epochs are handed out from one counter, so no two tempos (or two resets)
// share one; a process that moves to another tempo sees a new epoch, not a
// wrap
static NEXT_EPOCH: AtomicU32 = AtomicU32::new(1);

fn next_epoch() -> u32 {
    NEXT_EPOCH.fetch_add(1, Ordering::Relaxed)
}

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

// renaming it would change the `blast_time::blast_time::…` path that every
// engine, command and plan file uses
#[allow(clippy::module_inception)]
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
    // interval is stored as samples, converted from samples, milliseconds
    // or BPM when the TempoState is built; who ticks it (and when) is the
    // Conductor's business (its owner's transport, README §3.4)
    #[derive(Debug)]
    pub struct TempoState {
        pub unit: TempoUnit,
        pub interval: f32,
        pub active: bool,
        pub current: u64,
        // `current` at the last retime
        anchor_samples: u64,
        // the position in steps at the last retime
        anchor_steps: f64,
        // new on every reset (and unique across tempos), so anything
        // tracking this TempoState's position can tell a restart from a wrap
        pub epoch: u32,
        // a rate glide in progress (plain data; see `glide`)
        glide: Option<Glide>,
    }

    /// A glide of the rate, linear in the unit it was written in.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Glide {
        from: f64,
        to: f64,
        unit: TempoUnit,
        // frames of the clock running still to go, and in all
        left: u64,
        total: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum TempoUnit {
        Samples,
        Millis,
        Bpm,
    }

    impl TempoState {
        /// A stopped TempoState of `interval` `unit`s, converted to samples.
        pub fn new(unit: TempoUnit, interval: f32) -> Self {
            Self {
                unit,
                interval: convert_interval(&unit, interval),
                active: false,
                current: 0,
                anchor_samples: 0,
                anchor_steps: 0.0,
                epoch: next_epoch(),
                glide: None,
            }
        }

        //
        // paused/stopped TempoStates don't advance
        pub fn update(&mut self, delta_in_samples: f64) {
            if !self.active {
                return;
            }
            let delta = delta_in_samples as u64;
            self.current += delta;
            if let Some(g) = self.glide {
                self.step_glide(g, delta);
            }
        }

        // Moves the rate along its glide. The position is re-anchored first,
        // at the rate it had, so a step is never doubled or skipped.
        fn step_glide(&mut self, mut g: Glide, delta: u64) {
            self.anchor_steps = self.steps();
            self.anchor_samples = self.current;
            g.left = g.left.saturating_sub(delta);
            let value = if g.left == 0 {
                self.glide = None;
                g.to
            } else {
                self.glide = Some(g);
                let done = (g.total - g.left) as f64 / g.total as f64;
                g.from + (g.to - g.from) * done
            };
            self.interval = convert_interval(&g.unit, value as f32);
        }

        /// The position in steps of the interval. A retime moves the anchor
        /// instead of rescaling the elapsed time, so the position never
        /// jumps. Divides in f64, since f32 can't count samples exactly past
        /// 2^24 (~6 minutes at 48k).
        pub fn steps(&self) -> f64 {
            self.anchor_steps + (self.current - self.anchor_samples) as f64 / self.interval as f64
        }

        /// `steps` as f32.
        pub fn current(&self) -> f32 {
            self.steps() as f32
        }

        /// `steps` wrapped into 0..period; wrapped in f64 so precision
        /// doesn't degrade the longer the tempo runs.
        pub fn current_in(&self, period: f32) -> f32 {
            (self.steps() % period as f64) as f32
        }

        /// Changes the rate without moving the position: from here on the
        /// position advances by the new interval. It isn't a restart (the
        /// epoch stays) and works while paused.
        pub fn retime(&mut self, unit: TempoUnit, interval: f32) {
            self.anchor_steps = self.steps();
            self.anchor_samples = self.current;
            self.glide = None;
            self.unit = unit;
            self.interval = convert_interval(&unit, interval);
        }

        /// Like `retime`, but the rate moves to the new one over `frames`
        /// frames of the clock running, linearly in `unit` (bpm for `Bpm`).
        /// It starts from the rate the clock has now; a glide already going
        /// is replaced. It waits while the clock is stopped or paused, and a
        /// `retime` cancels it. Plain data: no allocation.
        pub fn glide(&mut self, unit: TempoUnit, interval: f32, frames: u32) {
            if frames == 0 {
                return self.retime(unit, interval);
            }
            self.anchor_steps = self.steps();
            self.anchor_samples = self.current;
            self.glide = Some(Glide {
                from: rate_in(unit, self.interval),
                to: f64::from(interval),
                unit,
                left: u64::from(frames),
                total: u64::from(frames),
            });
            self.unit = unit;
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
            self.anchor_samples = 0;
            self.anchor_steps = 0.0;
            self.epoch = next_epoch();
        }
    }

    // a rate of `samples` per step, written in `unit`
    fn rate_in(unit: TempoUnit, samples: f32) -> f64 {
        let sr = f64::from(sample_rate::get());
        let samples = f64::from(samples);
        match unit {
            TempoUnit::Samples => samples,
            TempoUnit::Millis => samples * 1000.0 / sr,
            TempoUnit::Bpm => 60.0 * sr / samples,
        }
    }

    fn convert_interval(unit: &TempoUnit, interval: f32) -> f32 {
        let frac = match unit {
            TempoUnit::Samples => return interval,
            TempoUnit::Millis => interval / 1000.0,
            TempoUnit::Bpm => 60.0 / interval,
        };

        sample_rate::get() as f32 * frac
    }
}

#[cfg(test)]
mod tests {
    use super::blast_time::*;
    use super::*;

    fn tempo(unit: TempoUnit, interval: f32) -> TempoState {
        sample_rate::set(48000);
        TempoState::new(unit, interval)
    }

    #[test]
    fn new_converts_and_starts_inactive() {
        sample_rate::set(48000);
        let ts = TempoState::new(TempoUnit::Millis, 1000.0);
        assert_eq!(ts.unit, TempoUnit::Millis);
        assert_eq!(ts.interval, 48000.0);
        assert!(!ts.active);
        assert_eq!(ts.current, 0);
        assert_eq!(ts.steps(), 0.0);
    }

    #[test]
    fn new_converts_interval_to_samples() {
        assert_eq!(tempo(TempoUnit::Samples, 100.0).interval, 100.0);
        assert_eq!(tempo(TempoUnit::Millis, 500.0).interval, 24000.0);
        assert_eq!(tempo(TempoUnit::Bpm, 120.0).interval, 24000.0);
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
    fn reset_takes_a_new_epoch() {
        let mut ts = tempo(TempoUnit::Samples, 100.0);
        let e = ts.epoch;
        ts.start();
        let started = ts.epoch;
        assert_ne!(started, e);
        ts.stop();
        let stopped = ts.epoch;
        assert_ne!(stopped, started);
        ts.pause();
        ts.resume();
        ts.update(5.0);
        assert_eq!(ts.epoch, stopped);
    }

    #[test]
    fn epochs_are_unique_across_tempos() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            let mut ts = tempo(TempoUnit::Samples, 100.0);
            assert!(seen.insert(ts.epoch));
            ts.reset();
            assert!(seen.insert(ts.epoch));
        }
    }

    // 100 samples per step, running for `n` samples
    fn running(n: u64) -> TempoState {
        let mut ts = tempo(TempoUnit::Samples, 100.0);
        ts.start();
        for _ in 0..n {
            ts.update(1.0);
        }
        ts
    }

    #[test]
    fn retime_keeps_position() {
        let mut ts = running(250);
        assert_eq!(ts.steps(), 2.5);
        let epoch = ts.epoch;
        ts.retime(TempoUnit::Samples, 50.0);
        assert_eq!(ts.steps(), 2.5);
        assert_eq!(ts.current_in(4.0), 2.5);
        assert_eq!(ts.epoch, epoch);
        // from here a step is 50 samples
        for _ in 0..50 {
            ts.update(1.0);
        }
        assert_eq!(ts.steps(), 3.5);
        // and slowing down
        ts.retime(TempoUnit::Samples, 200.0);
        assert_eq!(ts.steps(), 3.5);
        for _ in 0..100 {
            ts.update(1.0);
        }
        assert_eq!(ts.steps(), 4.0);
    }

    #[test]
    fn retime_changes_unit() {
        let mut ts = running(250);
        ts.retime(TempoUnit::Bpm, 120.0);
        assert_eq!(ts.unit, TempoUnit::Bpm);
        assert_eq!(ts.interval, 24000.0);
        assert_eq!(ts.steps(), 2.5);
        for _ in 0..12000 {
            ts.update(1.0);
        }
        assert_eq!(ts.steps(), 3.0);
    }

    #[test]
    fn retime_while_paused_then_resume() {
        let mut ts = running(250);
        ts.pause();
        ts.retime(TempoUnit::Samples, 50.0);
        ts.update(1.0);
        assert_eq!(ts.steps(), 2.5);
        ts.resume();
        for _ in 0..25 {
            ts.update(1.0);
        }
        assert_eq!(ts.steps(), 3.0);
    }

    #[test]
    fn reset_clears_anchors() {
        let mut ts = running(250);
        ts.retime(TempoUnit::Samples, 50.0);
        ts.update(1.0);
        ts.reset();
        assert_eq!(ts.steps(), 0.0);
        for _ in 0..50 {
            ts.update(1.0);
        }
        assert_eq!(ts.steps(), 1.0);
    }

    // 120 bpm at 48k is 24000 samples a step
    #[test]
    fn accelerando_reaches_target() {
        let mut ts = tempo(TempoUnit::Bpm, 120.0);
        ts.start();
        ts.glide(TempoUnit::Bpm, 240.0, 4 * 24000);
        // halfway it is 180 bpm: linear in bpm
        for _ in 0..2 * 24000 {
            ts.update(1.0);
        }
        assert_eq!(ts.interval, 60.0 * 48000.0 / 180.0);
        for _ in 0..2 * 24000 {
            ts.update(1.0);
        }
        assert_eq!(ts.interval, tempo(TempoUnit::Bpm, 240.0).interval);
        assert_eq!(ts.unit, TempoUnit::Bpm);
        // it stays there
        ts.update(1.0);
        assert_eq!(ts.interval, 12000.0);
    }

    #[test]
    fn glide_keeps_seq_phase() {
        let mut ts = tempo(TempoUnit::Bpm, 120.0);
        ts.start();
        ts.glide(TempoUnit::Bpm, 240.0, 48000);
        // a seq reads the step each frame: it must see each step once
        let mut last = ts.steps().floor();
        let mut prev = ts.steps();
        let mut fired = vec![];
        for _ in 0..3 * 48000 + 1000 {
            ts.update(1.0);
            let s = ts.steps();
            assert!(
                s >= prev && s - prev < 0.01,
                "position jumped: {prev} -> {s}"
            );
            prev = s;
            if s.floor() > last {
                assert_eq!(s.floor(), last + 1.0, "a step was skipped");
                last = s.floor();
                fired.push(last);
            }
        }
        let want: Vec<f64> = (1..=fired.len()).map(|n| n as f64).collect();
        assert_eq!(fired, want);
        // the glide takes 1 s from 2 to 4 steps/s (3 steps), then 4/s for 2 s
        assert_eq!(fired.len(), 3 + 8);
    }

    #[test]
    fn glide_then_retime_cancels() {
        let mut ts = tempo(TempoUnit::Bpm, 120.0);
        ts.start();
        ts.glide(TempoUnit::Bpm, 240.0, 48000);
        for _ in 0..12000 {
            ts.update(1.0);
        }
        let at = ts.steps();
        ts.retime(TempoUnit::Bpm, 60.0);
        assert_eq!(ts.steps(), at);
        for _ in 0..48000 {
            ts.update(1.0);
        }
        assert_eq!(ts.interval, 48000.0);
        assert_eq!(ts.steps(), at + 1.0);
        // a new glide starts from the rate it has now
        ts.glide(TempoUnit::Bpm, 120.0, 100);
        for _ in 0..50 {
            ts.update(1.0);
        }
        assert_eq!(ts.interval, 60.0 * 48000.0 / 90.0);
    }

    #[test]
    fn glide_on_paused_clock() {
        let mut ts = tempo(TempoUnit::Bpm, 120.0);
        ts.start();
        ts.glide(TempoUnit::Bpm, 240.0, 100);
        for _ in 0..50 {
            ts.update(1.0);
        }
        ts.pause();
        let (iv, at) = (ts.interval, ts.steps());
        for _ in 0..1000 {
            ts.update(1.0);
        }
        assert_eq!((ts.interval, ts.steps()), (iv, at));
        ts.resume();
        for _ in 0..50 {
            ts.update(1.0);
        }
        assert_eq!(ts.interval, 12000.0);
    }

    #[test]
    fn current_in_wraps_after_retime() {
        let mut ts = running(350);
        ts.retime(TempoUnit::Samples, 50.0);
        // 3.5 + 1.0 = 4.5, wrapped into a cycle of 4
        for _ in 0..50 {
            ts.update(1.0);
        }
        assert_eq!(ts.current_in(4.0), 0.5);
    }
}
