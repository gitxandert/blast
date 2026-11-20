//! The step sequencer.

use crate::audio_processing::{
    blast_rand::X128P,
    blast_time::blast_time::TempoState,
    commands::{
        FOLLOWS_CHANCE, FOLLOWS_JITTER, GroupId, Pending, ProcOwner, RecvSpec, SeqArgs,
        SeqEditArgs, SeqVecs, TrigTarget, VoiceId,
    },
    engine::{Group, Voice, VoiceParam, arena::Arena, voice_is_running},
};

/// Retriggers its Voice (or every Voice in its Group) on steps of a tempo.
pub struct Seq {
    pub state: SeqState,
}

/// A seq-wide value that may be on its way to a new one (`over`, REQ-074).
/// Plain data: a ramp never allocates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ramp {
    pub value: f32,
    from: f32,
    to: f32,
    pos: u32,
    len: u32,
}

impl Ramp {
    pub(super) fn at(value: f32) -> Self {
        Self {
            value,
            from: value,
            to: value,
            pos: 0,
            len: 0,
        }
    }

    /// Heads for `target`: at once for 0 frames, else over that many frames
    /// from where it is now.
    pub(super) fn set(&mut self, target: f32, frames: u32) {
        if frames == 0 {
            *self = Self::at(target);
        } else {
            self.from = self.value;
            self.to = target;
            self.pos = 0;
            self.len = frames;
        }
    }

    // one frame
    pub(super) fn advance(&mut self) {
        if self.pos < self.len {
            self.pos += 1;
            self.value = if self.pos == self.len {
                self.to
            } else {
                self.from + (self.to - self.from) * (self.pos as f32 / self.len as f32)
            };
        }
    }
}

/// A Seq's steps, chances and position. Its tempo is the `ProcSlot`'s.
pub struct SeqState {
    /// False while paused: the seq doesn't fire.
    pub active: bool,
    pub period: usize,
    pub steps: Vec<f32>,
    pub chance: Vec<f32>,
    pub jit: Vec<f32>,
    /// Per step, where its next hit is due (offset drawn); see `fire`.
    pub pend: Vec<Pending>,
    /// `pend` needs filling in (after a start, resync or edit).
    pub armed: bool,
    /// The seq (re)starts from its first cycle, so a first step drawn early
    /// fires at the start instead of before it.
    pub fresh: bool,
    /// Per step, which values follow the seq-wide `feel` (`FOLLOWS_*`).
    pub mask: Vec<u8>,
    pub rng: X128P,
    pub idx: usize,        // index of the step after the last one fired
    pub last: Option<f32>, // position at the previous process()
    pub epoch: u32,        // tempo.epoch when last was taken
    /// The seq-wide chance (a percentage), early and late (ticks).
    pub feel: [Ramp; 3],
    /// What a firing step does, in pipe order. Built on the command thread;
    /// only read here (a melo's note index moves in place).
    pub recv: Vec<RecvSpec>,
}

// hits a voice that is playing (a stopped or paused one stays quiet)
fn hit(voices: &mut Arena<Voice>, groups: &Arena<Group>, v: VoiceId) {
    if let Some(voice) = voices.get_mut(v.index())
        && voice_is_running(voice, groups)
    {
        voice.hit();
    }
}

// hits every member of group `g` that is playing
fn hit_members(voices: &mut Arena<Voice>, groups: &Arena<Group>, g: GroupId) {
    let Some(group) = groups.get(g.index()) else {
        return;
    };
    for &m in &group.members {
        hit(voices, groups, m);
    }
}

impl Seq {
    /// Moves the parsed steps in; `epoch` is its tempo's current epoch.
    pub fn new(args: SeqArgs, epoch: u32) -> Self {
        Self {
            state: SeqState {
                active: true,
                period: args.period,
                steps: args.steps,
                chance: args.chance,
                jit: args.jit,
                pend: args.pend,
                armed: false,
                fresh: true,
                mask: args.mask,
                rng: args.rng,
                idx: 0,
                last: None,
                epoch,
                feel: args.feel.map(Ramp::at),
                recv: args.recv,
            },
        }
    }

    // on each step that fires, runs the receivers in pipe order
    pub(super) fn process(
        &mut self,
        voices: &mut Arena<Voice>,
        groups: &Arena<Group>,
        owner: ProcOwner,
        tempo: &TempoState,
    ) {
        self.advance_feel();
        if !self.fire(tempo) {
            return;
        }
        for i in 0..self.state.recv.len() {
            match &mut self.state.recv[i] {
                RecvSpec::Trig(TrigTarget::Owner) => match owner {
                    ProcOwner::Voice(v) => hit(voices, groups, v),
                    ProcOwner::Group(g) => hit_members(voices, groups, g),
                    // nothing to trigger (the language refuses it)
                    ProcOwner::Session => {}
                },
                RecvSpec::Trig(TrigTarget::Voice(v)) => hit(voices, groups, *v),
                RecvSpec::Trig(TrigTarget::Group(g)) => hit_members(voices, groups, *g),
                RecvSpec::Melo { target, notes, idx } => {
                    if notes.is_empty() {
                        continue;
                    }
                    let note = notes[*idx % notes.len()];
                    *idx = (*idx + 1) % notes.len();
                    // pure pitch: 60 is no shift, and the note lands at once
                    if let Some(voice) = voices.get_mut(target.index()) {
                        voice.state.set_param(VoiceParam::Pitch, note - 60.0, 0);
                    }
                    hit(voices, groups, *target);
                }
            }
        }
    }

    /// Replaces the receivers; returns the old ones, for the garbage queue.
    pub(super) fn set_recv(&mut self, recv: Vec<RecvSpec>) -> Vec<RecvSpec> {
        std::mem::replace(&mut self.state.recv, recv)
    }

    /// Whether a receiver hits voice `v` (in `group`) as someone else's
    /// voice; its owner is for the caller to know.
    pub(super) fn targets(&self, v: VoiceId, group: Option<GroupId>) -> bool {
        self.state.recv.iter().any(|r| match r {
            RecvSpec::Trig(TrigTarget::Voice(t)) | RecvSpec::Melo { target: t, .. } => *t == v,
            RecvSpec::Trig(TrigTarget::Group(g)) => group == Some(*g),
            RecvSpec::Trig(TrigTarget::Owner) => false,
        })
    }

    fn advance_feel(&mut self) {
        for r in &mut self.state.feel {
            r.advance();
        }
    }

    // the chance of step `i` now: its own, or the seq-wide one (which may
    // be gliding)
    fn chance_of(&self, i: usize) -> f32 {
        let state = &self.state;
        let follows = state.mask.get(i).is_some_and(|m| m & FOLLOWS_CHANCE != 0);
        if follows {
            state.feel[0].value
        } else {
            state.chance.get(i).copied().unwrap_or(100.0)
        }
    }

    // how far step `i` may land before and after its step (ticks, now)
    fn spread_of(&self, i: usize) -> (f32, f32) {
        let state = &self.state;
        let (early, late) = if state.mask.get(i).is_some_and(|m| m & FOLLOWS_JITTER != 0) {
            (state.feel[1].value, state.feel[2].value)
        } else {
            let at = |k| state.jit.get(i * 4 + k).copied().unwrap_or(0.0);
            (at(1), at(3))
        };
        (early.max(0.0), late.max(0.0))
    }

    // the offset for step `i`'s next hit: uniform in [-early, late]. Nothing
    // is drawn without a spread, so a seq without feel uses the RNG for its
    // chances alone.
    fn draw_offset(&mut self, i: usize) -> f32 {
        let (early, late) = self.spread_of(i);
        if early + late <= 0.0 {
            return 0.0;
        }
        -early + self.state.rng.next_f32() * (early + late)
    }

    // step `s` moved by `off`: its position in a cycle, and how many cycles
    // that moved it
    fn locate(s: f32, off: f32, period: f32) -> (f32, i8) {
        if off == 0.0 {
            return (s, 0);
        }
        let t = s + off;
        let lag = (t / period).floor().clamp(-100.0, 100.0);
        let pos = t - lag * period;
        (
            if (0.0..period).contains(&pos) {
                pos
            } else {
                0.0
            },
            lag as i8,
        )
    }

    // draws every step's first offset; `last` is where the seq is now
    fn arm(&mut self, last: f32, period: f32) {
        for i in 0..self.state.steps.len() {
            let s = self.state.steps[i];
            self.state.pend[i] = if !(0.0..period).contains(&s) {
                Pending {
                    wait: Pending::NEVER,
                    ..Pending::default()
                }
            } else {
                let off = self.draw_offset(i);
                if self.state.fresh && s + off < 0.0 && last < 0.0 {
                    // early before the start: right at it
                    Pending::default()
                } else {
                    let (pos, lag) = Self::locate(s, off, period);
                    // this cycle's hit, unless it's past by now
                    let mut wait = lag as i32;
                    if wait < 0 || (wait == 0 && pos <= last) {
                        wait += 1;
                    }
                    Pending {
                        pos,
                        wait: wait as i8,
                        lag,
                    }
                }
            };
        }
        self.state.armed = true;
        self.state.fresh = false;
    }

    // step `i` fired in the cycle `rel` (0 = this one, -1 = the one that
    // just ended): draws its next offset and schedules the hit
    fn rearm(&mut self, i: usize, rel: i32, current: f32, period: f32, interval: f32) {
        let before = self.state.pend[i];
        let off = self.draw_offset(i);
        let (mut pos, lag) = Self::locate(self.state.steps[i], off, period);
        // the nominal cycle of the next hit, then where the offset puts it
        let mut wait = (rel - before.lag as i32 + 1 + lag as i32).clamp(0, 100);
        if wait == 0 && pos <= current {
            // an early offset can't reach back: next frame instead
            pos = current + 0.5 / interval;
            if pos >= period {
                wait = 1;
                pos = 0.0;
            }
        }
        self.state.pend[i] = Pending {
            pos,
            wait: wait as i8,
            lag,
        };
    }

    // advance through the steps; returns whether this step
    // should retrigger
    fn fire(&mut self, tempo: &TempoState) -> bool {
        if !self.state.active {
            return false;
        }

        if !tempo.active {
            return false;
        }

        // nothing to sequence (the parser rejects these anyway)
        if self.state.steps.is_empty() || self.state.period == 0 {
            return false;
        }

        let period = self.state.period as f32;
        let current = tempo.current_in(period);

        // a shared tempo was restarted (e.g. by its Group or by
        // start -t) since the last call; start over instead of
        // reading the drop in position as a wrap
        if tempo.epoch != self.state.epoch {
            self.state.epoch = tempo.epoch;
            self.state.last = None;
            self.state.armed = false;
            self.state.fresh = true;
        }

        // steps fire when the position crosses them, since
        // step * interval is rarely a whole number of samples;
        // without a previous position (after reset), act as if
        // the previous sample was one sample earlier
        let last = match self.state.last {
            Some(l) => l,
            None => current - 1.0 / tempo.interval,
        };
        self.state.last = Some(current);

        // tempo didn't move (e.g. called again in the same frame)
        if current == last {
            return false;
        }

        // the position wrapped past the end of the period
        let wrapped = current < last;

        let n = self.state.steps.len();
        if self.state.pend.len() != n {
            // no room for offsets (hand-built args): hits stay on their steps
            let mut fired = false;
            for i in 0..n {
                let step = self.state.steps[i];
                let crossed = match wrapped {
                    false => last < step && step <= current,
                    true => (last < step && step < period) || step <= current,
                };

                if crossed {
                    let rand = self.state.rng.next_i64_range(0, 100);
                    if (rand as f32) < self.chance_of(i) {
                        fired = true;
                    }
                    self.state.idx = (i + 1) % n;
                }
            }
            return fired;
        }

        if !self.state.armed {
            self.arm(last, period);
        }

        // Each step holds its next hit as a position in the cycle and the
        // cycles to wait for it. It's due when the position crosses it, as a
        // step without feel is (the offset is zero then, and nothing is drawn).
        let mut fired = false;
        for i in 0..n {
            let p = self.state.pend[i];
            if p.wait == Pending::NEVER {
                continue;
            }
            let due = match wrapped {
                false => (p.wait == 0 && last < p.pos && p.pos <= current).then_some(0),
                true if p.wait == 0 && last < p.pos && p.pos < period => Some(-1),
                true if p.wait == 1 && p.pos <= current => Some(0),
                true => None,
            };
            let Some(rel) = due else {
                if wrapped && p.wait > 0 {
                    self.state.pend[i].wait -= 1;
                }
                continue;
            };
            // the chance first, then the next offset: a fixed order
            let rand = self.state.rng.next_i64_range(0, 100);
            if (rand as f32) < self.chance_of(i) {
                fired = true;
            }
            self.state.idx = (i + 1) % n;
            self.rearm(i, rel, current, period, tempo.interval);
        }

        fired
    }

    /// Forgets the last position but keeps the step index: the next `fire`
    /// acts as if the previous sample was one sample earlier, so no step
    /// between a pause and now counts as crossed.
    pub(super) fn resync(&mut self) {
        self.state.last = None;
        self.state.armed = false;
    }

    pub(super) fn reset(&mut self) {
        self.state.idx = 0;
        // a melo starts over at its first note
        for r in &mut self.state.recv {
            if let RecvSpec::Melo { idx, .. } = r {
                *idx = 0;
            }
        }
        self.state.last = None;
        self.state.armed = false;
        self.state.fresh = true;
    }

    /// Pauses or resumes firing. Resuming resyncs, so nothing that passed
    /// while it was paused counts as crossed.
    pub(super) fn set_enabled(&mut self, on: bool) {
        self.state.active = on;
        if on {
            self.state.last = None;
            self.state.armed = false;
        }
    }

    pub(super) fn enabled(&self) -> bool {
        self.state.active
    }

    pub(super) fn fires(&self) -> bool {
        self.state.active
    }

    /// Swaps in what `args` changes, without a restart: the position
    /// (`last`) and the tempo epoch stay, so a step at or before the current
    /// position counts as crossed and nothing fires twice. `instant` (the seq
    /// isn't running) makes glides land at once. Returns the replaced
    /// vectors, for the garbage queue; nothing here allocates or frees.
    pub(super) fn edit(&mut self, args: SeqEditArgs, instant: bool) -> SeqVecs {
        let state = &mut self.state;
        let mut old = SeqVecs::default();
        if !args.steps.is_empty() {
            old.steps = std::mem::replace(&mut state.steps, args.steps);
            old.chance = std::mem::replace(&mut state.chance, args.chance);
            old.jit = std::mem::replace(&mut state.jit, args.jit);
            old.pend = std::mem::replace(&mut state.pend, args.pend);
            state.armed = false;
            old.mask = std::mem::replace(&mut state.mask, args.mask);
            // the first step after where the seq is now
            state.idx = match state.last {
                Some(last) => state.steps.iter().position(|s| *s > last).unwrap_or(0),
                None => 0,
            };
        }
        if args.period != 0 {
            state.period = args.period;
            state.armed = false;
            // the old position means nothing in the new cycle
            state.last = None;
        }
        if let Some(rng) = args.rng {
            state.rng = rng;
        }
        for (ramp, edit) in state.feel.iter_mut().zip(args.feel) {
            if let Some(e) = edit {
                ramp.set(e.value, if instant { 0 } else { e.frames });
            }
        }
        old
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::audio_processing::{
        blast_rand::fast_seed,
        blast_time::{blast_time::TempoUnit, sample_rate},
        commands::{
            CmdProcessor, Command, EngineState, GroupId, Parsed, PauseArgs, ProcId, ProcOwner,
            ResumeArgs, SeqArgs, SetParamArgs, StartArgs, Target, TempoId, TempoInit, TempoUse,
            VoiceId,
        },
        engine::Conductor,
        engine::VoiceParam,
        test_util::{conductor, pcm, render, render_i16},
    };
    use crate::file_parsing::decode_helpers::AudioFile;

    // mono track whose first sample is -1 and every other is 1,
    // so a frame renders -1 only when the Voice is (re)triggered
    const LEN: usize = 400_000;

    fn kick() -> AudioFile {
        let mut samples = vec![1i16; LEN];
        samples[0] = -1;
        AudioFile::new("kick", "wav", 48000, 1, 16, pcm(&samples))
    }

    // runs each command through the parser and into the engine;
    // render at 1 channel so tempos tick once per frame
    fn setup(cmds: &[&str]) -> Conductor {
        sample_rate::set(48000);
        let mut cp = CmdProcessor::new(EngineState::new(vec![kick()], 1));
        let mut c = conductor(1, vec![kick()]);
        for cmd in cmds {
            match cp.parse(cmd) {
                Ok(Parsed::Engine(command)) => c.apply(command),
                Ok(Parsed::Local(_)) => (),
                Err(e) => panic!("'{}' failed: {}", cmd, e),
            }
        }
        c
    }

    fn start_kick(c: &mut Conductor) {
        c.apply(Command::Start(StartArgs {
            target: Target::Voice(VoiceId(0)),
        }));
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
        // 120 BPM: one step every 24000 frames
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000ms",
            "kick { seq s 1 2 on 120b }",
            "start kick",
        ]);
        let out = render_i16(&mut c, 1, 30000);
        assert_eq!(triggers(&out), vec![0, 24000]);
    }

    #[test]
    fn seq_added_to_playing_voice_runs() {
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000ms",
            "start kick",
            "kick { seq s 1 2 on 120b }",
        ]);
        let out = render_i16(&mut c, 1, 30000);
        assert_eq!(triggers(&out), vec![0, 24000]);
    }

    #[test]
    fn seq_with_own_tempo() {
        // 1000f -> one step every 1000 frames, period 4
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000ms",
            "kick { seq s 1 3 on 1000f }",
            "start kick",
        ]);
        let out = render_i16(&mut c, 1, 8500);
        assert_eq!(triggers(&out), vec![0, 2000, 4000, 6000, 8000]);
    }

    #[test]
    fn seq_with_voice_tempo() {
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000f",
            "kick { seq s 2 in 2 }",
            "start kick",
        ]);
        let out = render_i16(&mut c, 1, 5500);
        // the voice waits for step 2 (tick 1)
        assert_eq!(triggers(&out), vec![1000, 3000, 5000]);
    }
    #[test]
    fn seq_with_no_steps_or_period_doesnt_panic() {
        for (period, steps) in [(4, Vec::new()), (0, vec![0.0])] {
            let mut c = setup(&["voice kick from \"kick\" on 1000ms"]);
            let tempo = TempoUse::Own(TempoInit {
                id: TempoId(1),
                unit: TempoUnit::Samples,
                interval: 100.0,
            });
            c.apply(Command::Seq(SeqArgs {
                proc: ProcId(0),
                owner: ProcOwner::Voice(VoiceId(0)),
                tempo,
                period,
                chance: vec![100.0; steps.len()],
                jit: vec![0.0; steps.len() * 4],
                pend: Vec::new(),
                steps,
                mask: Vec::new(),
                feel: [100.0, 0.0, 0.0],
                rng: X128P::new(fast_seed()),
                recv: RecvSpec::owner_trig(),
            }));
            c.apply(Command::Start(StartArgs {
                target: Target::Voice(VoiceId(0)),
            }));
            let out = render_i16(&mut c, 1, 1000);
            // a seq without a first step never triggers, so the parked voice stays silent
            assert_eq!(triggers(&out), Vec::<usize>::new());
        }
    }
    fn assert_near(got: &[usize], want: &[f64]) {
        assert_eq!(got.len(), want.len(), "got {:?}, want {:?}", got, want);
        for (g, w) in got.iter().zip(want) {
            assert!(
                (*g as f64 - w.ceil()).abs() <= 1.0,
                "got {:?}, want {:?}",
                got,
                want
            );
        }
    }

    #[test]
    fn seq_fires_on_non_integer_intervals() {
        // b:140 at 48k is 20571.43 samples per step
        let interval = 48000.0 * 60.0 / 140.0;
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000ms",
            "kick { seq s 1 2 3 4 on 140b }",
            "start kick",
        ]);
        // 3 periods (12 steps) minus a little, so step 12 isn't reached
        let out = render_i16(&mut c, 1, 246000);
        let want: Vec<f64> = (0..12).map(|k| k as f64 * interval).collect();
        assert_near(&triggers(&out), &want);
    }

    #[test]
    fn seq_fires_on_fractional_steps() {
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000ms",
            "kick { seq s 1 1.5 2 on 120b }",
            "start kick",
        ]);
        let out = render_i16(&mut c, 1, 3 * 96000);
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
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000ms",
            "kick { seq s 1 2.5 on 3.33ms in 2 }",
            "start kick",
        ]);
        let out = render_i16(&mut c, 1, 1500);
        let mut want = Vec::new();
        let mut p = 0.0;
        while (p + 1.5) * interval < 1499.0 {
            want.push(p * interval);
            want.push((p + 1.5) * interval);
            p += 2.0;
        }
        if p * interval < 1499.0 {
            want.push(p * interval);
        }
        assert_near(&triggers(&out), &want);
    }

    #[test]
    fn seq_steps_out_of_order_and_past_period() {
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000ms",
            "kick { seq s 3 1 on 1000f }",
            "start kick",
        ]);
        let out = render_i16(&mut c, 1, 8500);
        assert_eq!(triggers(&out), vec![0, 2000, 4000, 6000, 8000]);

        // step 3 is outside period 2, so it never fires (the language refuses
        // it, E406, so this one is built by hand)
        let mut c = setup(&["voice kick from \"kick\" on 1000ms"]);
        c.apply(Command::Seq(SeqArgs {
            proc: ProcId(0),
            owner: ProcOwner::Voice(VoiceId(0)),
            tempo: TempoUse::Own(TempoInit {
                id: TempoId(1),
                unit: TempoUnit::Samples,
                interval: 1000.0,
            }),
            period: 2,
            steps: vec![0.0, 3.0],
            chance: vec![100.0; 2],
            jit: vec![0.0; 8],
            pend: Vec::new(),
            mask: Vec::new(),
            feel: [100.0, 0.0, 0.0],
            rng: X128P::new(fast_seed()),
            recv: RecvSpec::owner_trig(),
        }));
        start_kick(&mut c);
        let out = render_i16(&mut c, 1, 4500);
        assert_eq!(triggers(&out), vec![0, 2000, 4000]);
    }

    #[test]
    fn seq_restarts_with_voice() {
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000ms",
            "kick { seq s 2 on 1000f }",
            "start kick",
        ]);
        let out = render_i16(&mut c, 1, 1500);
        assert_eq!(triggers(&out), vec![1000]);

        // restarting mid-period resets the seq and its tempo
        c.apply(Command::Start(StartArgs {
            target: Target::Voice(VoiceId(0)),
        }));
        let out = render_i16(&mut c, 1, 1500);
        assert_eq!(triggers(&out), vec![1000]);
    }

    #[test]
    fn seq_ignores_restart_of_shared_tempo() {
        let mut c = setup(&[
            "clock clk 100f",
            "voice kick from \"kick\" on 1000ms",
            "kick { seq s 2 4 on clk in 4 }",
            "start clk",
            "start kick",
        ]);
        render(&mut c, 1, 250); // fires step 1, stops at 2.5 steps

        // restarting the context without the seq knowing would look
        // like a wrap and fire step 3 straight away
        c.apply(Command::Start(StartArgs {
            target: Target::Tempo(TempoId(0)),
        }));
        let out = render_i16(&mut c, 1, 150);
        let fired = triggers(&out);
        assert!(!fired.is_empty());
        assert!(fired.iter().all(|f| *f > 50), "{:?}", fired);
    }

    // a voice on a clock that has run 150 frames (mid-tick 1), seq "2 4"
    // in 4 -> next hit at tick 3 (frame 300 of the clock)
    fn clocked(extra: &[&str]) -> Conductor {
        let mut cmds = vec![
            "clock clk 100f",
            "voice kick from \"kick\" on 1000ms",
            "kick { seq s 2 4 on clk in 4 }",
        ];
        cmds.extend_from_slice(extra);
        cmds.push("start clk");
        let mut c = setup(&cmds);
        render(&mut c, 1, 150);
        c
    }

    fn first_sound(out: &[i16]) -> Option<usize> {
        out.iter().position(|s| *s != 0)
    }

    #[test]
    fn voice_with_seq_waits_for_its_step() {
        let mut c = clocked(&[]);
        start_kick(&mut c);
        let out = render_i16(&mut c, 1, 400);
        let first = first_sound(&out).expect("never sounded");
        assert!(first > 100, "sounded off the grid at {first}");
        // silent until tick 3 (clock frame 300), then hits on its steps
        assert_eq!(triggers(&out), vec![150, 350]);
    }

    #[test]
    fn voice_without_seq_plays_at_once() {
        let mut c = setup(&["voice kick from \"kick\" on 1000ms", "start kick"]);
        let out = render_i16(&mut c, 1, 100);
        assert_eq!(triggers(&out), vec![0]);
    }

    #[test]
    fn voice_with_seq_on_own_clock_plays_step_one() {
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000ms",
            "kick { seq s 1 3 on 1000f }",
            "start kick",
        ]);
        let out = render_i16(&mut c, 1, 100);
        assert_eq!(triggers(&out), vec![0]);
    }

    #[test]
    fn grouped_voice_with_seq_waits() {
        let mut c = clocked(&["group g on clk { kick }"]);
        c.apply(Command::Start(StartArgs {
            target: Target::Group(GroupId(0)),
        }));
        let out = render_i16(&mut c, 1, 400);
        let first = first_sound(&out).expect("never sounded");
        assert!(first > 100, "sounded off the grid at {first}");
    }

    #[test]
    fn reverse_voice_parks_past_end() {
        let mut c = clocked(&[]);
        c.apply(Command::SetParam(SetParamArgs {
            voice: VoiceId(0),
            param: VoiceParam::Rate,
            value: -1.0,
            frames: 0,
        }));
        start_kick(&mut c);
        assert!(c.voices.get(0).unwrap().state.inst[0].position >= LEN as f64);
        let out = render_i16(&mut c, 1, 400);
        let first = first_sound(&out).expect("never sounded");
        assert!(first > 100, "sounded off the grid at {first}");
        // plays from the end backwards: no frame-0 hit
        assert!(triggers(&out).is_empty());
    }

    fn pause(c: &mut Conductor, target: Target) {
        c.apply(Command::Pause(PauseArgs { target }));
    }

    fn resume(c: &mut Conductor, target: Target) {
        c.apply(Command::Resume(ResumeArgs { target }));
    }

    const KICK: Target = Target::Voice(VoiceId(0));

    #[test]
    fn resume_with_seq_waits_for_next_step() {
        let mut c = clocked(&[]);
        start_kick(&mut c);
        // the hit at 150 is mid-play when the voice is paused
        render(&mut c, 1, 200);
        pause(&mut c, KICK);
        // the clock passes a step (clock frame 500) while paused
        render(&mut c, 1, 160);
        resume(&mut c, KICK);
        let out = render_i16(&mut c, 1, 400);
        // silent until the next step (clock frame 700), no catch-up hit
        assert_eq!(first_sound(&out), Some(190));
        assert_eq!(triggers(&out), vec![190, 390]);
    }

    #[test]
    fn resume_with_seq_no_catch_up_after_wrap() {
        let mut c = clocked(&[]);
        start_kick(&mut c);
        render(&mut c, 1, 200);
        pause(&mut c, KICK);
        // several periods pass
        render(&mut c, 1, 1000);
        resume(&mut c, KICK);
        let out = render_i16(&mut c, 1, 400);
        assert_eq!(first_sound(&out), Some(150));
        assert_eq!(triggers(&out), vec![150, 350]);
    }

    #[test]
    fn resume_on_own_clock_continues() {
        let mut c = setup(&[
            "voice kick from \"kick\" on 1000f",
            "kick { seq s 2 in 2 }",
            "start kick",
        ]);
        let out = render_i16(&mut c, 1, 1500);
        assert_eq!(triggers(&out), vec![1000]);
        pause(&mut c, KICK);
        render(&mut c, 1, 700);
        resume(&mut c, KICK);
        let out = render_i16(&mut c, 1, 4000);
        // the hit in progress goes on at once, and the steps are those of
        // the unpaused run (3000, 5000), shifted by the pause
        assert_ne!(out[0], 0);
        assert_eq!(triggers(&out), vec![1500, 3500]);
    }

    #[test]
    fn resume_without_seq_continues() {
        let mut c = setup(&["voice kick from \"kick\" on 1000ms", "start kick"]);
        render(&mut c, 1, 100);
        pause(&mut c, KICK);
        render(&mut c, 1, 50);
        resume(&mut c, KICK);
        let out = render_i16(&mut c, 1, 100);
        assert_ne!(out[0], 0);
        assert!(triggers(&out).is_empty());
    }

    #[test]
    fn resume_group_member_with_seq_waits() {
        let mut c = clocked(&["group g on clk { kick }"]);
        let g = Target::Group(GroupId(0));
        c.apply(Command::Start(StartArgs { target: g }));
        render(&mut c, 1, 200);
        pause(&mut c, g);
        render(&mut c, 1, 160);
        resume(&mut c, g);
        let out = render_i16(&mut c, 1, 400);
        assert_eq!(first_sound(&out), Some(190));
        assert_eq!(triggers(&out), vec![190, 390]);
    }
}
