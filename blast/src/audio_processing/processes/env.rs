//! The envelope (ENV): fires on every hit of its owner and shapes one
//! parameter through the modulation bank: attack, decay to a sustain level,
//! hold, release. On `gain` it multiplies; on any other target it adds.

use super::lfo::LfoTarget;
use crate::audio_processing::{
    blast_time::blast_time::TempoState,
    commands::{EnvArgs, EnvEditArgs, GroupId, ProcOwner, RecvSpec, SeqEditArgs, SeqVecs, VoiceId},
    engine::{
        Group, ModOp, ModTarget, Voice, VoiceParam, WaveParam, arena::Arena, voice_is_running,
    },
};

/// How decay and release fall (the attack is always linear).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvCurve {
    Lin,
    /// -60 dB at the end of the stage.
    Exp,
}

/// The stages of an envelope, in frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvShape {
    pub attack: u32,
    pub decay: u32,
    pub sustain: f32,
    /// Frames at the sustain level before the release; `None` = until the
    /// next hit.
    pub hold: Option<u32>,
    pub release: u32,
    pub curve: EnvCurve,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Hold,
    Release,
}

/// An envelope on one parameter. Plain data: nothing here allocates.
pub struct Env {
    /// As last written; a hit copies it to `run`.
    shape: EnvShape,
    /// What the running envelope uses (an edit waits for the next hit).
    run: EnvShape,
    target: LfoTarget,
    depth: f32,
    stage: Stage,
    level: f32,
    /// The level the stage started from.
    from: f32,
    /// Frames into the stage, and its length.
    t: u32,
    len: u32,
    /// The per-frame factor of an exponential stage.
    k: f32,
    /// The trigger count at the last frame (the sum over the members for a
    /// group); `None` until the first frame after a reset.
    seen: Option<u32>,
    /// `start` of the envelope itself: the first frame only takes the count,
    /// so the next hit fires it.
    quiet: bool,
    enabled: bool,
    /// `Some(level)` while paused; `None` while stopped.
    frozen: Option<f32>,
}

// -60 dB over a stage of `frames`, a step at a time
fn exp_factor(frames: u32) -> f32 {
    (0.001f32.ln() / frames.max(1) as f32).exp()
}

impl Env {
    pub fn new(a: EnvArgs) -> Self {
        Self {
            shape: a.shape,
            run: a.shape,
            target: a.target,
            depth: a.depth,
            stage: Stage::Idle,
            level: 0.0,
            from: 0.0,
            t: 0,
            len: 0,
            k: 1.0,
            seen: None,
            quiet: false,
            enabled: true,
            frozen: None,
        }
    }

    // starts `stage` at the current level
    fn enter(&mut self, stage: Stage) {
        self.stage = stage;
        self.from = self.level;
        self.t = 0;
        self.len = match stage {
            Stage::Attack => self.run.attack,
            Stage::Decay => self.run.decay,
            Stage::Hold => self.run.hold.unwrap_or(u32::MAX),
            Stage::Release => self.run.release,
            Stage::Idle => 0,
        };
        self.k = match stage {
            Stage::Decay | Stage::Release => exp_factor(self.len),
            _ => 1.0,
        };
    }

    // one frame: moves time on (unless the stage just began), and falls
    // through the stages that are over, zero-length ones included
    fn step(&mut self, fresh: bool) {
        if !fresh && self.stage != Stage::Idle {
            self.t = self.t.saturating_add(1);
        }
        loop {
            let (goal, next) = match self.stage {
                Stage::Idle => {
                    self.level = 0.0;
                    return;
                }
                Stage::Attack => (1.0, Stage::Decay),
                Stage::Decay => (self.run.sustain, Stage::Hold),
                Stage::Hold => {
                    if self.run.hold.is_none() {
                        // until the next hit
                        self.level = self.run.sustain;
                        return;
                    }
                    (self.run.sustain, Stage::Release)
                }
                Stage::Release => (0.0, Stage::Idle),
            };
            if self.t >= self.len {
                self.level = goal;
                self.enter(next);
                continue;
            }
            let expo = self.run.curve == EnvCurve::Exp
                && matches!(self.stage, Stage::Decay | Stage::Release);
            if expo {
                if self.t > 0 {
                    self.level = goal + (self.level - goal) * self.k;
                }
            } else {
                self.level = self.from + (goal - self.from) * self.t as f32 / self.len as f32;
            }
            return;
        }
    }

    /// One frame given the trigger `count` of the owner. `Some(level)` is
    /// what to write; `None` writes nothing (stopped).
    fn tick(&mut self, count: u32) -> Option<f32> {
        let hit = match self.seen {
            Some(c) => c != count,
            None => !self.quiet,
        };
        self.seen = Some(count);
        self.quiet = false;
        if !self.enabled {
            // paused keeps its level and ignores hits; stopped writes nothing
            return self.frozen;
        }
        if hit {
            self.run = self.shape;
            self.enter(Stage::Attack);
            self.step(true);
        } else {
            self.step(false);
        }
        Some(self.level)
    }

    // the sum of the trigger counts of the owner (its members, for a group)
    fn count(voices: &Arena<Voice>, groups: &Arena<Group>, owner: ProcOwner) -> Option<u32> {
        match owner {
            ProcOwner::Voice(v) => voices.get(v.index()).map(|v| v.state.trigger_count()),
            ProcOwner::Group(g) => groups.get(g.index()).map(|group| {
                group
                    .members
                    .iter()
                    .filter_map(|m| voices.get(m.index()))
                    .fold(0u32, |sum, v| sum.wrapping_add(v.state.trigger_count()))
            }),
            ProcOwner::Session => None,
        }
    }

    // gain multiplies, so that full depth is silence outside the envelope;
    // the rest add
    fn op(&self, level: f32) -> ModOp {
        match self.target {
            LfoTarget::Voice(VoiceParam::Gain) => ModOp::Mul(1.0 - self.depth + self.depth * level),
            // a wave's gain is shaped like the voice's
            LfoTarget::Wave { param, .. } if param == WaveParam::Gain.index() as u8 => {
                ModOp::Mul(1.0 - self.depth + self.depth * level)
            }
            _ => ModOp::Add(self.depth * level),
        }
    }

    // writes `level` to the target, on the owner (or every member of a group)
    fn write(
        &self,
        voices: &mut Arena<Voice>,
        groups: &mut Arena<Group>,
        owner: ProcOwner,
        level: f32,
    ) {
        let op = self.op(level);
        let target = match self.target {
            LfoTarget::Voice(p) => ModTarget::Voice(p),
            LfoTarget::Fx { id, param } => ModTarget::Fx { id, param },
            LfoTarget::Wave { id, param } => ModTarget::Wave { id, param },
        };
        match (owner, self.target) {
            (ProcOwner::Voice(id), _) => {
                if let Some(voice) = voices.get_mut(id.index()) {
                    voice.modulate(target, op);
                }
            }
            (ProcOwner::Group(g), LfoTarget::Voice(_)) => {
                let Some(group) = groups.get(g.index()) else {
                    return;
                };
                for &m in &group.members {
                    if let Some(voice) = voices.get_mut(m.index())
                        && voice_is_running(voice, groups)
                    {
                        voice.modulate(target, op);
                    }
                }
            }
            (ProcOwner::Group(g), LfoTarget::Fx { id, param }) => {
                if let Some(group) = groups.get_mut(g.index()) {
                    group.modulate_fx(id, param, op);
                }
            }
            // an env needs an owner (the language refuses it), and a group
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
        _tempo: &TempoState,
    ) {
        let Some(count) = Self::count(voices, groups, owner) else {
            return;
        };
        if let Some(level) = self.tick(count) {
            self.write(voices, groups, owner, level);
        }
    }

    /// Re-reads the owner's trigger count into `seen`, so a change of
    /// the group's members (which moves the sum) isn't taken as a hit.
    pub(crate) fn resync_count(
        &mut self,
        voices: &Arena<Voice>,
        groups: &Arena<Group>,
        owner: ProcOwner,
    ) {
        if self.seen.is_some()
            && let Some(count) = Self::count(voices, groups, owner)
        {
            self.seen = Some(count);
        }
    }

    /// The owner starts parked, waiting for its seq's first step: the env
    /// takes the owner's count now, so only the step's hit fires it (not
    /// the start too).
    pub(crate) fn wait_for_hit(
        &mut self,
        voices: &Arena<Voice>,
        groups: &Arena<Group>,
        owner: ProcOwner,
    ) {
        if let Some(count) = Self::count(voices, groups, owner) {
            self.seen = Some(count);
        }
    }

    /// Idle at level 0, as when its owner starts or stops. A paused env
    /// stays paused; `unfreeze` (stop, start) is for the command that asked.
    pub(super) fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.level = 0.0;
        self.from = 0.0;
        self.t = 0;
        self.seen = None;
        self.quiet = false;
    }

    /// `stop` and `start` of the envelope itself: it stops being paused, and
    /// waits for the next hit.
    pub fn unfreeze(&mut self) {
        self.frozen = None;
        self.quiet = true;
    }

    pub(super) fn resync(&mut self) {}

    pub(super) fn set_enabled(&mut self, on: bool) {
        if on {
            self.frozen = None;
        } else if self.enabled {
            self.frozen = Some(self.level);
        }
        self.enabled = on;
    }

    pub(super) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(super) fn fires(&self) -> bool {
        false
    }

    /// Applies an edit: a new shape waits for the next hit, a new target or
    /// depth is at once.
    pub fn edit_env(&mut self, a: EnvEditArgs) {
        if let Some(shape) = a.shape {
            self.shape = shape;
        }
        if let Some(target) = a.target {
            self.target = target;
        }
        if let Some(depth) = a.depth {
            self.depth = depth;
        }
    }

    /// Whether the envelope is at rest, waiting for a hit.
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        self.stage == Stage::Idle
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::blast_time::blast_time::TempoUnit;
    use crate::audio_processing::commands::{ProcId, TempoId, TempoInit, TempoUse};

    fn shape(attack: u32, decay: u32, sustain: f32, curve: EnvCurve) -> EnvShape {
        EnvShape {
            attack,
            decay,
            sustain,
            hold: None,
            release: 0,
            curve,
        }
    }

    fn env(shape: EnvShape, target: LfoTarget, depth: f32) -> Env {
        Env::new(EnvArgs {
            proc: ProcId(0),
            owner: ProcOwner::Voice(VoiceId(0)),
            tempo: TempoUse::Own(TempoInit {
                id: TempoId(0),
                unit: TempoUnit::Samples,
                interval: 1.0,
            }),
            shape,
            target,
            depth,
        })
    }

    fn gain() -> LfoTarget {
        LfoTarget::Voice(VoiceParam::Gain)
    }

    fn near(got: f32, want: f32) {
        assert!((got - want).abs() < 1e-5, "got {got}, wanted {want}");
    }

    // the levels of `n` frames after a hit at the first
    fn run(e: &mut Env, n: usize) -> Vec<f32> {
        let mut out = vec![e.tick(1).unwrap()];
        for _ in 1..n {
            out.push(e.tick(1).unwrap());
        }
        out
    }

    #[test]
    fn ad_linear_shape() {
        let mut e = env(shape(10, 20, 0.0, EnvCurve::Lin), gain(), 1.0);
        let l = run(&mut e, 40);
        near(l[0], 0.0);
        near(l[1], 0.1);
        near(l[10], 1.0);
        near(l[20], 0.5);
        near(l[30], 0.0);
        near(l[39], 0.0);
    }

    #[test]
    fn adsr_with_hold_then_release() {
        let s = EnvShape {
            attack: 2,
            decay: 2,
            sustain: 0.5,
            hold: Some(4),
            release: 4,
            curve: EnvCurve::Lin,
        };
        let mut e = env(s, gain(), 1.0);
        let l = run(&mut e, 16);
        near(l[2], 1.0);
        near(l[4], 0.5);
        // held for 4 frames
        for v in &l[4..=8] {
            near(*v, 0.5);
        }
        // then 4 frames of release
        near(l[10], 0.25);
        near(l[12], 0.0);
        near(l[15], 0.0);
    }

    #[test]
    fn hold_until_next_hit_by_default() {
        let mut e = env(
            EnvShape {
                release: 4,
                ..shape(1, 1, 0.7, EnvCurve::Lin)
            },
            gain(),
            1.0,
        );
        for l in run(&mut e, 5000).iter().skip(2) {
            near(*l, 0.7);
        }
    }

    #[test]
    fn exp_reaches_minus_60db_at_stage_end() {
        let mut e = env(shape(0, 100, 0.0, EnvCurve::Exp), gain(), 1.0);
        let l = run(&mut e, 101);
        near(l[0], 1.0);
        // the last step before the snap is within a hair of -60 dB
        assert!(l[99] > 0.001 && l[99] < 0.0012, "{}", l[99]);
        assert!(l[50] > 0.02 && l[50] < 0.05, "{}", l[50]);
        near(l[100], 0.0);
    }

    #[test]
    fn zero_length_stages_fall_through() {
        // no attack, no decay: sustain at once
        let mut e = env(shape(0, 0, 0.4, EnvCurve::Lin), gain(), 1.0);
        near(run(&mut e, 1)[0], 0.4);
        // nothing at all but a release of zero: back to idle in the same call
        let s = EnvShape {
            hold: Some(0),
            ..shape(0, 0, 0.4, EnvCurve::Lin)
        };
        let mut e = env(s, gain(), 1.0);
        near(run(&mut e, 1)[0], 0.0);
    }

    #[test]
    fn retrigger_restarts_from_current_level() {
        let mut e = env(shape(10, 20, 0.0, EnvCurve::Lin), gain(), 1.0);
        run(&mut e, 5);
        let l = e.level;
        near(l, 0.4);
        // a hit: the attack continues from 0.4, not from 0
        near(e.tick(2).unwrap(), 0.4);
        near(e.tick(2).unwrap(), 0.46);
        // the decay's end is relative to the new hit
        for _ in 0..9 {
            e.tick(2);
        }
        near(e.level, 1.0);
    }

    #[test]
    fn gain_is_mul_others_add() {
        // before its first hit an env is at level 0: gain depth 1 is silence
        let e = env(shape(1, 1, 1.0, EnvCurve::Lin), gain(), 1.0);
        assert_eq!(e.level, 0.0);
        assert_eq!(e.op(e.level), ModOp::Mul(0.0));
        // depth 0.5 leaves half
        let e = env(shape(1, 1, 1.0, EnvCurve::Lin), gain(), 0.5);
        assert_eq!(e.op(0.0), ModOp::Mul(0.5));
        assert_eq!(e.op(1.0), ModOp::Mul(1.0));
        // any other target adds depth * level
        let e = env(
            shape(1, 1, 1.0, EnvCurve::Lin),
            LfoTarget::Voice(VoiceParam::Pitch),
            12.0,
        );
        assert_eq!(e.op(0.5), ModOp::Add(6.0));
        let e = env(
            shape(1, 1, 1.0, EnvCurve::Lin),
            LfoTarget::Fx { id: 0, param: 0 },
            3.0,
        );
        assert_eq!(e.op(1.0), ModOp::Add(3.0));
    }

    #[test]
    fn pause_holds_level_and_ignores_hits() {
        let mut e = env(shape(10, 20, 0.0, EnvCurve::Lin), gain(), 1.0);
        run(&mut e, 5);
        e.set_enabled(false);
        assert_eq!(e.frozen, Some(e.level));
        for _ in 0..30 {
            // a hit while paused changes nothing
            near(e.tick(9).unwrap(), 0.4);
        }
        e.set_enabled(true);
        // resume continues the attack, and the stale hit doesn't fire it
        near(e.tick(9).unwrap(), 0.5);
        near(e.tick(9).unwrap(), 0.6);
    }

    #[test]
    fn stop_writes_nothing_start_waits_for_a_hit() {
        let mut e = env(shape(10, 20, 0.0, EnvCurve::Lin), gain(), 1.0);
        run(&mut e, 5);
        // stop = Disable + Reset + unfreeze
        e.set_enabled(false);
        e.reset();
        e.unfreeze();
        assert_eq!(e.tick(1), None);
        // start = Enable + Reset + unfreeze: idle until the next hit
        e.set_enabled(true);
        e.reset();
        e.unfreeze();
        near(e.tick(1).unwrap(), 0.0);
        near(e.tick(1).unwrap(), 0.0);
        near(e.tick(2).unwrap(), 0.0);
        near(e.tick(2).unwrap(), 0.1);
    }

    #[test]
    fn owner_start_fires_on_the_first_frame() {
        let mut e = env(shape(10, 20, 0.0, EnvCurve::Lin), gain(), 1.0);
        e.reset();
        near(e.tick(1).unwrap(), 0.0);
        near(e.tick(1).unwrap(), 0.1);
    }

    #[test]
    fn an_edited_shape_waits_for_the_next_hit() {
        let mut e = env(shape(10, 20, 0.0, EnvCurve::Lin), gain(), 1.0);
        run(&mut e, 3);
        e.edit_env(EnvEditArgs {
            id: ProcId(0),
            shape: Some(shape(0, 0, 0.9, EnvCurve::Lin)),
            target: None,
            depth: Some(0.5),
        });
        near(e.tick(1).unwrap(), 0.3);
        assert_eq!(e.depth, 0.5);
        near(e.tick(2).unwrap(), 0.9);
    }
}
