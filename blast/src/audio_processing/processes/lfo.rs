//! The low-frequency oscillator (LFO): adds a periodic value to one
//! parameter of its owner every frame, through the modulation bank. The
//! base value the language set never changes.

use super::seq::Ramp;
use crate::audio_processing::{
    blast_rand::X128P,
    blast_time::blast_time::TempoState,
    commands::{GroupId, LfoArgs, LfoEditArgs, ProcOwner, RecvSpec, SeqEditArgs, SeqVecs, VoiceId},
    engine::{Group, ModOp, ModTarget, Voice, VoiceParam, arena::Arena, voice_is_running},
};

/// The wave an lfo draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfoShape {
    Sine,
    Tri,
    Square,
    Saw,
    /// Sample and hold: a random value, held for each cycle.
    SampleHold,
}

/// Where an LFO writes: a voice parameter or an fx parameter, of the owner
/// (a group's `Voice` target means every member).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LfoTarget {
    Voice(VoiceParam),
    Fx {
        id: u16,
        param: u8,
    },
    /// A parameter (a `WaveParam` index) of one of a voice's waves; groups
    /// have none.
    Wave {
        id: u8,
        param: u8,
    },
}

/// Free-running (`hz`, or a time length turned into Hz) or locked to the
/// slot's clock (bare periods, `b`: a cycle of `ticks` ticks).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LfoRate {
    Hz(f32),
    Clock { ticks: f32 },
}

impl LfoRate {
    // the number the lfo's `freq` ramp holds in this mode
    fn value(self) -> f32 {
        match self {
            LfoRate::Hz(hz) => hz,
            LfoRate::Clock { ticks } => ticks,
        }
    }
}

/// A periodic value added to one parameter. Plain data: nothing here
/// allocates.
pub struct Lfo {
    shape: LfoShape,
    target: LfoTarget,
    /// Where in the cycle it starts (0..1).
    phase0: f64,
    /// Locked to the slot's clock; `freq` is then ticks per cycle, else Hz.
    clocked: bool,
    freq: Ramp,
    /// Cycles run since the start, not counting `phase0`.
    pos: f64,
    /// Seconds per frame.
    frame: f64,
    depth: Ramp,
    offset: Ramp,
    /// The value `SampleHold` holds now.
    sh: f32,
    /// The cycle `sh` was drawn for.
    last_cycle: i64,
    rng: X128P,
    // clock mode: the clock's position at the last frame, and the clock's
    // restart count then. `anchored` is false until `pos` is taken from the
    // clock (at the start, after a pause, after a restart).
    last_steps: f64,
    epoch: u32,
    anchored: bool,
    enabled: bool,
    /// `Some(value)` while paused; `None` while stopped.
    frozen: Option<f32>,
    /// The value written at the last frame.
    last: f32,
}

// the wave at `p` in [0, 1): -1..1
fn wave(shape: LfoShape, p: f32, held: f32) -> f32 {
    match shape {
        LfoShape::Sine => (std::f32::consts::TAU * p).sin(),
        // 0 rising at p = 0
        LfoShape::Tri => 1.0 - 4.0 * ((p + 0.25).fract() - 0.5).abs(),
        LfoShape::Square => {
            if p < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        LfoShape::Saw => 2.0 * p - 1.0,
        LfoShape::SampleHold => held,
    }
}

impl Lfo {
    pub fn new(a: LfoArgs) -> Self {
        let rate = a.rate;
        Self {
            shape: a.shape,
            target: a.target,
            phase0: f64::from(a.phase),
            clocked: matches!(rate, LfoRate::Clock { .. }),
            freq: Ramp::at(rate.value()),
            pos: 0.0,
            frame: 1.0 / f64::from(a.sample_rate.max(1.0)),
            depth: Ramp::at(a.depth),
            offset: Ramp::at(a.offset),
            sh: 0.0,
            last_cycle: i64::MIN,
            rng: a.rng,
            last_steps: 0.0,
            epoch: 0,
            anchored: false,
            enabled: true,
            frozen: None,
            last: 0.0,
        }
    }

    // `offset + depth · w` for this frame; moves the position on
    fn value(&mut self, tempo: &TempoState) -> f32 {
        if self.clocked {
            let steps = tempo.steps();
            let ticks = f64::from(self.freq.value).max(1e-9);
            if !self.anchored || tempo.epoch != self.epoch || steps < self.last_steps {
                self.pos = steps / ticks;
                self.anchored = true;
                self.epoch = tempo.epoch;
            } else {
                self.pos += (steps - self.last_steps) / ticks;
            }
            self.last_steps = steps;
        }
        let x = self.pos + self.phase0;
        let cycle = x.floor();
        if self.shape == LfoShape::SampleHold && cycle as i64 != self.last_cycle {
            self.sh = self.rng.next_f32() * 2.0 - 1.0;
        }
        self.last_cycle = cycle as i64;
        let w = wave(self.shape, (x - cycle) as f32, self.sh);
        if !self.clocked {
            self.pos += f64::from(self.freq.value) * self.frame;
        }
        self.offset.value + self.depth.value * w
    }

    // adds `v` to the target, on the owner (or every member of a group)
    fn write(
        &self,
        voices: &mut Arena<Voice>,
        groups: &mut Arena<Group>,
        owner: ProcOwner,
        v: f32,
    ) {
        let target = match self.target {
            LfoTarget::Voice(p) => ModTarget::Voice(p),
            LfoTarget::Fx { id, param } => ModTarget::Fx { id, param },
            LfoTarget::Wave { id, param } => ModTarget::Wave { id, param },
        };
        match (owner, self.target) {
            (ProcOwner::Voice(id), _) => modulate(voices, id, target, v),
            (ProcOwner::Group(g), LfoTarget::Voice(_)) => {
                let Some(group) = groups.get(g.index()) else {
                    return;
                };
                for &m in &group.members {
                    if let Some(voice) = voices.get_mut(m.index())
                        && voice_is_running(voice, groups)
                    {
                        voice.modulate(target, ModOp::Add(v));
                    }
                }
            }
            (ProcOwner::Group(g), LfoTarget::Fx { id, param }) => {
                if let Some(group) = groups.get_mut(g.index()) {
                    group.modulate_fx(id, param, ModOp::Add(v));
                }
            }
            // an lfo needs an owner (the language refuses it), and a group
            // has no waves
            (ProcOwner::Session, _) | (ProcOwner::Group(_), LfoTarget::Wave { .. }) => {}
        }
    }

    // one frame; the caller has checked that the owner is playing
    pub(super) fn process(
        &mut self,
        voices: &mut Arena<Voice>,
        groups: &mut Arena<Group>,
        owner: ProcOwner,
        tempo: &TempoState,
    ) {
        let v = if self.enabled {
            let v = self.value(tempo);
            self.last = v;
            self.depth.advance();
            self.offset.advance();
            self.freq.advance();
            v
        } else if let Some(v) = self.frozen {
            // paused: the value it had stays
            v
        } else {
            // stopped: the target plays at its base value
            return;
        };
        self.write(voices, groups, owner, v);
    }

    /// Back to `phase`. A paused lfo stays paused; `unfreeze` (stop, start)
    /// is for the command that asked.
    pub(super) fn reset(&mut self) {
        self.pos = 0.0;
        self.last_cycle = i64::MIN;
        self.anchored = false;
    }

    /// An lfo that was stopped or paused writes nothing until it is enabled.
    pub fn unfreeze(&mut self) {
        self.frozen = None;
    }

    /// A clock-locked lfo rejoins its clock's position.
    pub(super) fn resync(&mut self) {
        self.anchored = false;
    }

    pub(super) fn set_enabled(&mut self, on: bool) {
        if on {
            if !self.enabled {
                self.anchored = false;
            }
            self.frozen = None;
        } else if self.enabled {
            self.frozen = Some(self.last);
        }
        self.enabled = on;
    }

    pub(super) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(super) fn fires(&self) -> bool {
        false
    }

    /// Applies an edit. `instant` (its owner isn't playing): glides land at
    /// once.
    pub fn edit_lfo(&mut self, a: LfoEditArgs, instant: bool) {
        let frames = |f: u32| if instant { 0 } else { f };
        if let Some(e) = a.depth {
            self.depth.set(e.value, frames(e.frames));
        }
        if let Some(e) = a.offset {
            self.offset.set(e.value, frames(e.frames));
        }
        if let Some(e) = a.freq {
            self.freq.set(e.value, frames(e.frames));
        }
        if let Some(shape) = a.shape {
            self.shape = shape;
            self.last_cycle = i64::MIN;
        }
        if let Some(target) = a.target {
            self.target = target;
        }
        if let Some(phase) = a.phase {
            self.phase0 = f64::from(phase);
        }
        if let Some(rng) = a.rng {
            self.rng = rng;
            self.last_cycle = i64::MIN;
        }
        // the other kind of speed: the cycle starts over at `phase`
        if let Some(rate) = a.rate {
            self.clocked = matches!(rate, LfoRate::Clock { .. });
            self.freq = Ramp::at(rate.value());
            self.reset();
        }
    }

    // the seq-only methods of `Process`, trivially
    pub(super) fn set_recv(&mut self, recv: Vec<RecvSpec>) -> Vec<RecvSpec> {
        recv
    }

    pub(super) fn targets(&self, _v: VoiceId, _group: Option<GroupId>) -> bool {
        false
    }

    pub(super) fn edit(&mut self, args: SeqEditArgs, _instant: bool) -> SeqVecs {
        SeqVecs {
            steps: args.steps,
            chance: args.chance,
            jit: args.jit,
            pend: args.pend,
            mask: args.mask,
            recv: Vec::new(),
        }
    }
}

fn modulate(voices: &mut Arena<Voice>, id: VoiceId, target: ModTarget, v: f32) {
    if let Some(voice) = voices.get_mut(id.index()) {
        voice.modulate(target, ModOp::Add(v));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::blast_time::blast_time::TempoUnit;
    use crate::audio_processing::commands::{FeelEdit, ProcId, TempoId, TempoInit, TempoUse};

    // an lfo of unit depth at `sr` frames a second
    fn lfo(shape: LfoShape, rate: LfoRate, phase: f32, sr: f32) -> Lfo {
        Lfo::new(LfoArgs {
            proc: ProcId(0),
            owner: ProcOwner::Voice(VoiceId(0)),
            tempo: TempoUse::Own(TempoInit {
                id: TempoId(0),
                unit: TempoUnit::Samples,
                interval: 1.0,
            }),
            shape,
            target: LfoTarget::Voice(VoiceParam::Gain),
            rate,
            phase,
            depth: 1.0,
            offset: 0.0,
            rng: X128P::new(7),
            sample_rate: sr,
        })
    }

    // a clock that ticks once a frame
    fn clock() -> TempoState {
        let mut t = TempoState::new(TempoUnit::Samples, 1.0);
        t.start();
        t
    }

    fn near(got: f32, want: f32) {
        assert!((got - want).abs() < 1e-5, "got {got}, wanted {want}");
    }

    // four frames a cycle: the values at p = 0, .25, .5, .75
    fn points(shape: LfoShape) -> Vec<f32> {
        let mut l = lfo(shape, LfoRate::Hz(1.0), 0.0, 4.0);
        let t = clock();
        (0..4).map(|_| l.value(&t)).collect()
    }

    #[test]
    fn sine_quarter_points() {
        let p = points(LfoShape::Sine);
        for (got, want) in p.iter().zip([0.0, 1.0, 0.0, -1.0]) {
            near(*got, want);
        }
    }

    #[test]
    fn tri_square_saw_points() {
        for (got, want) in points(LfoShape::Tri).iter().zip([0.0, 1.0, 0.0, -1.0]) {
            near(*got, want);
        }
        for (got, want) in points(LfoShape::Square).iter().zip([1.0, 1.0, -1.0, -1.0]) {
            near(*got, want);
        }
        for (got, want) in points(LfoShape::Saw).iter().zip([-1.0, -0.5, 0.0, 0.5]) {
            near(*got, want);
        }
    }

    #[test]
    fn hz_rate_ignores_clock() {
        // 10 Hz at 48 kHz: a cycle in 4800 frames, whatever the clock does
        let mut l = lfo(LfoShape::Sine, LfoRate::Hz(10.0), 0.0, 48_000.0);
        let mut t = clock();
        let mut out = Vec::new();
        for i in 0..4801 {
            // the clock stops and starts; the lfo doesn't care
            if i % 1000 < 500 {
                t.update(1.0);
            }
            out.push(l.value(&t));
        }
        near(out[0], 0.0);
        near(out[1200], 1.0);
        near(out[2400], 0.0);
        near(out[3600], -1.0);
        assert!(out[4800].abs() < 1e-3);
        // a quarter in, a sine starts at 1
        let mut l = lfo(LfoShape::Sine, LfoRate::Hz(10.0), 0.25, 48_000.0);
        near(l.value(&t), 1.0);
    }

    #[test]
    fn clock_mode_follows_clock_and_holds_when_clock_paused() {
        // a cycle of 4 ticks; a tick a frame
        let mut l = lfo(LfoShape::Saw, LfoRate::Clock { ticks: 4.0 }, 0.0, 48_000.0);
        let mut t = clock();
        let mut out = Vec::new();
        for _ in 0..4 {
            out.push(l.value(&t));
            t.update(1.0);
        }
        for (got, want) in out.iter().zip([-1.0, -0.5, 0.0, 0.5]) {
            near(*got, want);
        }
        // the clock pauses: the value holds
        t.pause();
        let held = l.value(&t);
        for _ in 0..10 {
            near(l.value(&t), held);
            t.update(1.0);
        }
        // 8 ticks a cycle (a bare freq=2 on a period of 4) is half as fast
        let mut l = lfo(LfoShape::Saw, LfoRate::Clock { ticks: 8.0 }, 0.0, 48_000.0);
        let mut t = clock();
        let mut out = Vec::new();
        for _ in 0..4 {
            out.push(l.value(&t));
            t.update(1.0);
        }
        for (got, want) in out.iter().zip([-1.0, -0.75, -0.5, -0.25]) {
            near(*got, want);
        }
    }

    #[test]
    fn pause_freezes_value_resume_continues() {
        let mut l = lfo(LfoShape::Sine, LfoRate::Hz(1.0), 0.0, 4.0);
        let t = clock();
        let mut voices = Arena::with_capacity(1);
        let mut groups = Arena::with_capacity(1);
        let owner = ProcOwner::Voice(VoiceId(0));
        // frames at p = 0 and p = 0.25
        l.process(&mut voices, &mut groups, owner, &t);
        l.process(&mut voices, &mut groups, owner, &t);
        near(l.last, 1.0);
        l.set_enabled(false);
        assert_eq!(l.frozen, Some(1.0));
        // paused: the phase stops
        for _ in 0..8 {
            l.process(&mut voices, &mut groups, owner, &t);
        }
        near(l.pos as f32, 0.5);
        l.set_enabled(true);
        assert_eq!(l.frozen, None);
        // it carries on from p = 0.5
        near(l.value(&t), 0.0);
        near(l.value(&t), -1.0);
    }

    #[test]
    fn stop_writes_nothing_start_restarts_at_phase() {
        let mut l = lfo(LfoShape::Sine, LfoRate::Hz(1.0), 0.25, 4.0);
        let t = clock();
        let mut voices = Arena::with_capacity(1);
        let mut groups = Arena::with_capacity(1);
        let owner = ProcOwner::Voice(VoiceId(0));
        for _ in 0..3 {
            l.process(&mut voices, &mut groups, owner, &t);
        }
        // stop = Disable + Reset (+ the unfreeze the handler adds)
        l.set_enabled(false);
        l.reset();
        l.unfreeze();
        assert_eq!(l.frozen, None);
        let (before, pos) = (l.last, l.pos);
        l.process(&mut voices, &mut groups, owner, &t);
        assert_eq!(
            (l.last, l.pos),
            (before, pos),
            "a stopped lfo computes nothing"
        );
        // start = Enable + Reset: runs from `phase` again (a sine a quarter in is 1)
        l.set_enabled(true);
        l.reset();
        l.unfreeze();
        near(l.value(&t), 1.0);
    }

    #[test]
    fn reset_returns_to_phase() {
        let mut l = lfo(LfoShape::Saw, LfoRate::Hz(1.0), 0.5, 4.0);
        let t = clock();
        let first = l.value(&t);
        l.value(&t);
        l.value(&t);
        l.reset();
        near(l.value(&t), first);
    }

    #[test]
    fn freq_and_depth_glides_reach_their_targets() {
        let mut l = lfo(LfoShape::Sine, LfoRate::Hz(1.0), 0.0, 48_000.0);
        l.edit_lfo(
            LfoEditArgs {
                id: ProcId(0),
                depth: Some(FeelEdit {
                    value: 3.0,
                    frames: 50,
                }),
                offset: None,
                freq: Some(FeelEdit {
                    value: 5.0,
                    frames: 100,
                }),
                shape: None,
                target: None,
                rate: None,
                phase: None,
                rng: None,
            },
            false,
        );
        let t = clock();
        let mut voices = Arena::with_capacity(1);
        let mut groups = Arena::with_capacity(1);
        let owner = ProcOwner::Voice(VoiceId(0));
        for _ in 0..49 {
            l.process(&mut voices, &mut groups, owner, &t);
        }
        assert!(l.freq.value > 1.0 && l.freq.value < 5.0);
        assert!(l.depth.value > 1.0 && l.depth.value < 3.0);
        for _ in 0..51 {
            l.process(&mut voices, &mut groups, owner, &t);
        }
        assert_eq!(l.freq.value, 5.0);
        assert_eq!(l.depth.value, 3.0);
    }

    #[test]
    fn sh_changes_once_per_cycle_and_repeats_with_seed() {
        let run = |seed: u64| {
            let mut l = lfo(LfoShape::SampleHold, LfoRate::Hz(1.0), 0.0, 4.0);
            l.rng = X128P::new(seed);
            let t = clock();
            (0..16).map(|_| l.value(&t)).collect::<Vec<_>>()
        };
        let a = run(11);
        for cycle in a.chunks(4) {
            assert!(cycle.iter().all(|v| *v == cycle[0]), "{a:?}");
            assert!((-1.0..=1.0).contains(&cycle[0]));
        }
        let firsts: Vec<f32> = a.chunks(4).map(|c| c[0]).collect();
        assert!(firsts.windows(2).all(|w| w[0] != w[1]), "{firsts:?}");
        assert_eq!(a, run(11));
        assert_ne!(a, run(12));
    }
}
