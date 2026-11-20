//! Voices: one playing instance of a track, with the parameter model
//! (README §3.9, REQ-018): base values set by the language, plus a per-frame
//! modulation bank that processes write into and `advance` clears.

use std::cell::RefCell;
use std::sync::Arc;

use crate::audio_processing::blast_rand::{X128P, fast_seed};
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::{GroupId, ResampleMode, ShiftMode, TempoId};
use crate::audio_processing::engine::blep::{BlepState, BlepTable, QUIET};
use crate::audio_processing::engine::buffer::BufData;
use crate::audio_processing::engine::fx::FxChain;
use crate::audio_processing::engine::grains::{GrainSet, align};
use crate::audio_processing::engine::shift::{
    ColdBudget, HitKey, HitOutcome, ReadPath, ShiftParams, VoiceShifters,
};
use crate::audio_processing::engine::track::Track;
use crate::audio_processing::engine::wave::Waves;

/// The most source channels a voice reads; more are ignored.
pub const MAX_VOICE_CHANNELS: usize = 8;

/// A continuous voice parameter. Base values are set by commands; processes
/// add per-frame modulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceParam {
    Gain,
    Pan,
    Rate,
    Pitch,
    /// Speed without pitch (VOXc): the read pointer moves at `rate * stretch`.
    Stretch,
    Start,
    End,
    LoopStart,
    LoopEnd,
    /// The slice to play, 0-based (the language counts from 1).
    Slice,
    /// How exactly choir instances follow the voice (VOXd): 1 = exactly.
    Intonation,
}

impl VoiceParam {
    pub const COUNT: usize = 11;
    pub const ALL: [VoiceParam; Self::COUNT] = [
        VoiceParam::Gain,
        VoiceParam::Pan,
        VoiceParam::Rate,
        VoiceParam::Pitch,
        VoiceParam::Stretch,
        VoiceParam::Start,
        VoiceParam::End,
        VoiceParam::LoopStart,
        VoiceParam::LoopEnd,
        VoiceParam::Slice,
        VoiceParam::Intonation,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The name the language uses (`slice` is written 1-based there).
    pub fn name(self) -> &'static str {
        match self {
            VoiceParam::Gain => "gain",
            VoiceParam::Pan => "pan",
            VoiceParam::Rate => "rate",
            VoiceParam::Pitch => "pitch",
            VoiceParam::Stretch => "stretch",
            VoiceParam::Start => "start",
            VoiceParam::End => "end",
            VoiceParam::LoopStart => "loopstart",
            VoiceParam::LoopEnd => "loopend",
            VoiceParam::Slice => "slice",
            VoiceParam::Intonation => "intonation",
        }
    }

    pub fn from_name(s: &str) -> Option<VoiceParam> {
        Self::ALL.into_iter().find(|p| p.name() == s)
    }

    /// The value a new voice starts with.
    pub fn default_value(self) -> f32 {
        match self {
            VoiceParam::Gain
            | VoiceParam::Rate
            | VoiceParam::Stretch
            | VoiceParam::End
            | VoiceParam::LoopEnd
            | VoiceParam::Intonation => 1.0,
            _ => 0.0,
        }
    }

    /// The inclusive range base and effective values are clamped to.
    pub fn range(self) -> (f32, f32) {
        match self {
            VoiceParam::Gain => (0.0, 8.0),
            VoiceParam::Pan => (-1.0, 1.0),
            VoiceParam::Rate => (-16.0, 16.0),
            VoiceParam::Pitch => (-48.0, 48.0),
            VoiceParam::Stretch => (0.0625, 16.0),
            VoiceParam::Start | VoiceParam::End | VoiceParam::LoopStart | VoiceParam::LoopEnd => {
                (0.0, 1.0)
            }
            VoiceParam::Slice => (0.0, (MAX_SLICES - 1) as f32),
            VoiceParam::Intonation => (0.0, 1.0),
        }
    }
}

/// The most slices a track can be cut into.
pub const MAX_SLICES: u16 = 1024;

/// The most instances a choir has.
pub const MAX_CHOIR: usize = 8;

/// How hits spread over a choir's instances.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChoirMode {
    /// A round: each hit takes the next instance.
    #[default]
    Sequential,
    /// Each hit starts every instance.
    Simultaneous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopMode {
    Off,
    Forward,
    PingPong,
}

/// What a modulating process writes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModTarget {
    Voice(VoiceParam),
    /// A parameter of one of the voice's effects, by the effect's id and its
    /// index among the effect's settings that aren't `type`.
    Fx {
        id: u16,
        param: u8,
    },
    /// A parameter of one of the voice's waves, by the wave's id and its
    /// `WaveParam` index.
    Wave {
        id: u8,
        param: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModOp {
    Add(f32),
    Mul(f32),
}

/// One parameter's modulation this frame: `(base + add) * mul`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModSlot {
    pub add: f32,
    pub mul: f32,
}

impl ModSlot {
    pub const IDENTITY: ModSlot = ModSlot { add: 0.0, mul: 1.0 };

    fn fold(&mut self, op: ModOp) {
        match op {
            ModOp::Add(x) => self.add += x,
            ModOp::Mul(x) => self.mul *= x,
        }
    }
}

/// The frames a voice plays this frame, from the effective parameters.
#[derive(Clone, Copy, Debug)]
struct Bounds {
    /// The region, inclusive. `re < rs` means nothing plays.
    rs: i64,
    re: i64,
    /// The loop, inclusive and inside the region.
    ls: i64,
    le: i64,
    looping: bool,
    /// The half of a buffer that voices hear, loaded once per frame (0 for
    /// a track).
    front: usize,
}

// whether `position` sits on a playable frame of the region (or its loop)
fn region_plays(b: &Bounds, position: f64) -> bool {
    let p = position;
    b.re >= b.rs && p >= b.rs as f64 && (p <= b.re as f64 || (b.looping && p < (b.le + 1) as f64))
}

// a fraction of the track in frames; fractions of an `f32` that land within
// a hair of a frame boundary (0.2 * 100) snap to it
fn frame_of(fraction: f32, len: usize) -> f64 {
    let x = f64::from(fraction) * len as f64;
    let r = x.round();
    if (x - r).abs() < 1e-4 { r } else { x }
}

/// A glide in progress on one base value (`over`): `step` per frame for
/// `left` more frames, ending exactly on `target`.
#[derive(Clone, Copy, Debug, Default)]
struct Ramp {
    target: f32,
    step: f32,
    left: u32,
}

/// One singer of a voice's choir: its own head and grains, plus the offsets
/// it drew when it was last hit (`intonation`). Ramps and modulation stay on
/// the voice.
pub(crate) struct Instance {
    /// In frames; `f64`, since `f32` stops counting single frames after
    /// 2^24 (about six minutes at 48 kHz).
    pub position: f64,
    /// Ping-pong direction, +1 or -1.
    pp_dir: f64,
    // granular playback (VOXc): whether the grains play the instance, the
    // grains, and the frames left of the crossfade back to the plain head
    granular: bool,
    grains: GrainSet,
    // the spectral shifter plays the instance (PSHa); exclusive with
    // `granular`
    spectral: bool,
    // a hit is pending: the shifter must be hit at the position, in the
    // instance's direction (set by `restart_grains`, which has no source to
    // read). `Voice::resolve_hits` settles it before the next frame renders,
    // by a snapshot copy, a budgeted `output_seek`, or the grains. Until
    // then the instance renders nothing.
    shift_hit: bool,
    exit_left: u32,
    // hit and not stopped or parked
    live: bool,
    rate_mul: f32,
    pan_add: f32,
    gain_mul: f32,
    // frames before it sounds, after a hit
    delay_left: u32,
    // the plain head reads through the Elliptic-BLEP resampler: the
    // `resample=` setting as it was at the instance's last hit (REQ-143)
    iir: bool,
}

impl Instance {
    fn new(live: bool) -> Self {
        Self {
            position: 0.0,
            pp_dir: 1.0,
            granular: false,
            grains: GrainSet::new(),
            spectral: false,
            shift_hit: false,
            exit_left: 0,
            live,
            rate_mul: 1.0,
            pan_add: 0.0,
            gain_mul: 1.0,
            delay_left: 0,
            iir: ResampleMode::default() == ResampleMode::Iir,
        }
    }

    fn exact(&mut self) {
        self.rate_mul = 1.0;
        self.pan_add = 0.0;
        self.gain_mul = 1.0;
        self.delay_left = 0;
    }
}

/// The `bounce=` default, in seconds (REQ-106).
pub(crate) const DEFAULT_BOUNCE_S: f64 = 0.002;
/// The clamp on `bounce=`, in seconds (REQ-108).
pub(crate) const BOUNCE_RANGE_S: (f64, f64) = (0.0, 0.005);

/// The default `bounce=` in frames at `sample_rate`.
pub(crate) fn default_bounce(sample_rate: f32) -> u32 {
    (DEFAULT_BOUNCE_S * f64::from(sample_rate)).round() as u32
}

/// The gain at source frame `frame` for the fade at a ping-pong turnaround:
/// `sin^2(pi/2 * d / w')`, `d` the distance to the nearer loop edge and
/// `w' = min(w, (le - ls) / 2)`. The fade mirrors across each edge: a frame
/// in `ls - w' .. ls` or `le .. le + w'` (which a voice reads on its way in)
/// has `d` its distance to that edge, so entering a loop dips like a
/// turnaround. Exactly 1 when `w == 0`, when the voice isn't in a ping-pong
/// loop, when the frame is `w'` or more from both edges, or when the loop is
/// too short to fade.
fn bounce_gain(b: &Bounds, mode: LoopMode, frame: i64, w: usize) -> f32 {
    if w == 0 || !b.looping || mode != LoopMode::PingPong {
        return 1.0;
    }
    let w = (w as i64).min((b.le - b.ls) / 2);
    let d = match frame {
        f if f < b.ls => b.ls - f,
        f if f > b.le => f - b.le,
        f => (f - b.ls).min(b.le - f),
    };
    if d >= w {
        return 1.0;
    }
    let s = (std::f32::consts::FRAC_PI_2 * d as f32 / w as f32).sin();
    s * s
}

/// A Voice's playback state, which Processes act on.
pub struct VoiceState {
    pub active: bool,
    /// The choir; only the first `choir` are used.
    pub(crate) inst: [Instance; MAX_CHOIR],
    choir: u8,
    choir_mode: ChoirMode,
    // the instance the next hit takes (sequential)
    next: usize,
    // draws the instances' offsets; plain data
    rng: X128P,
    #[cfg(test)]
    draws: u32,
    /// Frames in the track.
    len: usize,
    base: [f32; VoiceParam::COUNT],
    mods: [ModSlot; VoiceParam::COUNT],
    // whether any mod was written since the last clear
    mods_dirty: bool,
    // glides in progress, one per parameter
    ramps: [Ramp; VoiceParam::COUNT],
    // changes whenever a parameter is set or starts a glide (`thenif`
    // compares it)
    generations: [u32; VoiceParam::COUNT],
    // whether any ramp has frames left; keeps `advance` from scanning
    ramping: bool,
    loop_mode: LoopMode,
    slices: u16,
    /// `bounce=`: the fade at a ping-pong turnaround, in frames (0 = off).
    bounce: usize,
    /// Which engine plays `pitch=`/`stretch=` (REQ-099).
    pub(crate) shift: ShiftMode,
    /// How the plain head reads the source (REQ-143); instances latch it at
    /// their hit.
    resample: ResampleMode,
    // how many instances can play spectral: the voice's shifters cover them
    // and fit the source (`Voice::refresh_shift`); 0 = the grains play
    spectral_avail: usize,
    trigger_count: u32,
    // true for a voice that plays waves only (no source): there is no
    // position to move and nothing for the grains or shifters to read
    waves_only: bool,
    // the choir instances the last `hit` started, one bit each; the voice
    // retriggers their waves from it
    started: u8,
    /// The tempo it uses (its own, a context's or its group's).
    pub tempo: TempoId,
    /// Loaded without `-t`: adopts its group's tempo when grouped.
    pub adopts: bool,
}

impl VoiceState {
    fn new(len: usize, tempo: TempoId, adopts: bool) -> Self {
        let mut base = [0.0; VoiceParam::COUNT];
        for p in VoiceParam::ALL {
            base[p.index()] = p.default_value();
        }
        Self {
            active: false,
            // the first instance sits at rest, so a voice that is switched on
            // without a hit plays
            inst: std::array::from_fn(|i| {
                let mut inst = Instance::new(i == 0);
                inst.iir = ResampleMode::initial() == ResampleMode::Iir;
                inst
            }),
            choir: 1,
            choir_mode: ChoirMode::Sequential,
            next: 0,
            rng: X128P::new(fast_seed()),
            #[cfg(test)]
            draws: 0,
            len,
            base,
            mods: [ModSlot::IDENTITY; VoiceParam::COUNT],
            mods_dirty: false,
            ramps: [Ramp::default(); VoiceParam::COUNT],
            generations: [0; VoiceParam::COUNT],
            ramping: false,
            loop_mode: LoopMode::Off,
            slices: 0,
            bounce: default_bounce(match sample_rate::get() {
                0 => 48_000.0,
                n => n as f32,
            }) as usize,
            shift: ShiftMode::default(),
            resample: ResampleMode::initial(),
            spectral_avail: 0,
            trigger_count: 0,
            waves_only: false,
            started: 0,
            tempo,
            adopts,
        }
    }

    /// Sets the base value, clamped to the parameter's range. Non-finite
    /// values are ignored. With `frames > 0` the value glides there in a
    /// straight line from where it is now; a new glide replaces the old
    /// one. A voice that isn't running can't glide (nothing advances it),
    /// so it jumps. Returns the frames the glide really takes (0 if it
    /// jumped).
    pub fn set_param(&mut self, p: VoiceParam, v: f32, frames: u32) -> u32 {
        if !v.is_finite() {
            return 0;
        }
        let (lo, hi) = p.range();
        let v = v.clamp(lo, hi);
        let i = p.index();
        self.generations[i] = self.generations[i].wrapping_add(1);
        if frames == 0 || !self.active {
            self.base[i] = v;
            self.ramps[i].left = 0;
            return 0;
        }
        self.ramps[i] = Ramp {
            target: v,
            step: (v - self.base[i]) / frames as f32,
            left: frames,
        };
        self.ramping = true;
        frames
    }

    /// Changes whenever `p` is set or starts a glide.
    pub fn glide_generation(&self, p: VoiceParam) -> u32 {
        self.generations[p.index()]
    }

    // moves every glide one frame; a finished one lands exactly on target
    fn advance_ramps(&mut self) {
        if !self.ramping {
            return;
        }
        let mut live = false;
        for (base, r) in self.base.iter_mut().zip(self.ramps.iter_mut()) {
            if r.left == 0 {
                continue;
            }
            r.left -= 1;
            *base = match r.left {
                0 => r.target,
                _ => *base + r.step,
            };
            live |= r.left > 0;
        }
        self.ramping = live;
    }

    /// The base value, without modulation.
    pub fn get_param(&self, p: VoiceParam) -> f32 {
        self.base[p.index()]
    }

    /// Adds this frame's modulation to a voice parameter. `advance` clears
    /// it. An effect's parameter belongs to the chain (`Voice::modulate`).
    pub fn modulate(&mut self, t: ModTarget, op: ModOp) {
        let ModTarget::Voice(p) = t else {
            return;
        };
        self.mods[p.index()].fold(op);
        self.mods_dirty = true;
    }

    /// `(base + Σadd) · Πmul`, clamped: what playback uses this frame.
    pub fn effective(&self, p: VoiceParam) -> f32 {
        let (lo, hi) = p.range();
        let m = self.mods[p.index()];
        ((self.base[p.index()] + m.add) * m.mul).clamp(lo, hi)
    }

    fn clear_mods(&mut self) {
        if self.mods_dirty {
            self.mods = [ModSlot::IDENTITY; VoiceParam::COUNT];
            self.mods_dirty = false;
        }
    }

    /// Sets the fade at a ping-pong turnaround, in frames (0 = off).
    pub fn set_bounce(&mut self, frames: u32) {
        self.bounce = frames as usize;
    }

    pub fn set_loop_mode(&mut self, m: LoopMode) {
        self.loop_mode = m;
        for inst in &mut self.inst {
            inst.pp_dir = 1.0;
        }
    }

    /// Sets how many instances the voice has (1..=`MAX_CHOIR`). Fewer than
    /// are sounding stops the extra ones at once.
    pub fn set_choir(&mut self, n: u8) {
        let n = n.clamp(1, MAX_CHOIR as u8);
        self.choir = n;
        for inst in &mut self.inst[usize::from(n)..] {
            inst.live = false;
        }
        self.next %= usize::from(n);
    }

    pub fn choir(&self) -> u8 {
        self.choir
    }

    pub fn set_choir_mode(&mut self, m: ChoirMode) {
        self.choir_mode = m;
    }

    pub fn choir_mode(&self) -> ChoirMode {
        self.choir_mode
    }

    /// Reseeds the offsets' RNG (tests want repeatable choirs).
    #[cfg(test)]
    fn seed(&mut self, seed: u64) {
        self.rng = X128P::new(seed);
    }

    /// Sets the resampler of the plain head. A sounding instance keeps its
    /// method until it is hit again or stopped (a switch never clicks mid
    /// note); a voice that is switched off takes it at once.
    pub fn set_resample(&mut self, m: ResampleMode) {
        self.resample = m;
        // a paused note is mid-note: it keeps its method like a sounding one
        for i in 0..MAX_CHOIR {
            let at_rest = !self.active && self.at_rest(i);
            if !self.inst[i].live || at_rest {
                self.inst[i].iir = m == ResampleMode::Iir;
            }
        }
    }

    // whether instance `i` sits where a hit would put it
    fn at_rest(&self, i: usize) -> bool {
        let b = self.bounds();
        let top = match self.direction(i) >= 0.0 {
            true => b.rs as f64,
            false => b.re as f64,
        };
        self.inst[i].position == top
    }

    pub fn loop_mode(&self) -> LoopMode {
        self.loop_mode
    }

    /// Cuts the track into `n` equal slices; 0 turns slicing off.
    pub fn set_slices(&mut self, n: u16) {
        self.slices = n.min(MAX_SLICES);
    }

    pub fn slices(&self) -> u16 {
        self.slices
    }

    /// Incremented by every start and retrigger. ENV watches it.
    pub fn trigger_count(&self) -> u32 {
        self.trigger_count
    }

    fn bounds(&self) -> Bounds {
        let len = self.len;
        let last = len as i64 - 1;
        let (rs, re) = if self.slices > 0 {
            let n = usize::from(self.slices);
            // (`as usize` is `floor() as usize` for every value, the
            // negative ones and NaN included, without a libm call)
            let i = (self.effective(VoiceParam::Slice) as usize).min(n - 1);
            ((i * len / n) as i64, ((i + 1) * len / n) as i64 - 1)
        } else {
            let rs = floor_i64(frame_of(self.effective(VoiceParam::Start), len));
            let re = ceil_i64(frame_of(self.effective(VoiceParam::End), len)) - 1;
            (rs.max(0), re.min(last))
        };
        let (mut ls, mut le) = (rs, re);
        let mut looping = false;
        if self.loop_mode != LoopMode::Off {
            let a = floor_i64(frame_of(self.effective(VoiceParam::LoopStart), len));
            let b = ceil_i64(frame_of(self.effective(VoiceParam::LoopEnd), len)) - 1;
            ls = a.max(rs);
            le = b.min(re);
            looping = ls <= le;
        }
        Bounds {
            rs,
            re,
            ls,
            le,
            looping,
            front: 0,
        }
    }

    // +1 plays forward; the sign of the rate, flipped by the ping-pong bounce
    fn direction(&self, i: usize) -> f64 {
        self.rate_sign() * self.inst[i].pp_dir
    }

    fn rate_sign(&self) -> f64 {
        match self.effective(VoiceParam::Rate) < 0.0 {
            true => -1.0,
            false => 1.0,
        }
    }

    // whether `position` sits on a playable frame
    fn playing(&self, b: &Bounds, position: f64) -> bool {
        region_plays(b, position)
    }

    /// A hit (a start or a seq step): the next instance restarts at the top
    /// of the region with fresh offsets, or every instance does. Bumps
    /// `trigger_count`.
    pub fn hit(&mut self) {
        let n = usize::from(self.choir);
        self.started = 0;
        match self.choir_mode {
            ChoirMode::Sequential => {
                let i = self.next % n;
                self.next = (i + 1) % n;
                self.start_instance(i);
            }
            ChoirMode::Simultaneous => {
                for i in 0..n {
                    self.start_instance(i);
                }
            }
        }
        self.trigger_count = self.trigger_count.wrapping_add(1);
    }

    // restarts one instance, with offsets scaled by `1 - intonation`; at
    // intonation 1 they are identities and the RNG isn't touched
    fn start_instance(&mut self, i: usize) {
        let spread = 1.0 - self.effective(VoiceParam::Intonation);
        self.started |= 1 << i;
        self.inst[i].iir = self.resample == ResampleMode::Iir;
        self.rewind_instance(i);
        if spread <= 0.0 {
            self.inst[i].exact();
        } else {
            // each in -1..1, drawn in a fixed order
            let [r, p, g, d] = [(); 4].map(|()| self.draw());
            let sr = match sample_rate::get() {
                0 => 48_000.0,
                n => n as f32,
            };
            let inst = &mut self.inst[i];
            inst.rate_mul = 1.0 + 0.03 * spread * r;
            inst.pan_add = 0.5 * spread * p;
            // 0 to -6 dB
            inst.gain_mul = 10f32.powf(-6.0 * spread * (g * 0.5 + 0.5) / 20.0);
            inst.delay_left = (0.020 * sr * spread * (d * 0.5 + 0.5)) as u32;
        }
        self.inst[i].live = true;
    }

    // a value in -1..1
    fn draw(&mut self) -> f32 {
        #[cfg(test)]
        {
            self.draws += 1;
        }
        self.rng.next_f32() * 2.0 - 1.0
    }

    // the first frame of the region, or the last when playing backwards
    fn rewind_instance(&mut self, i: usize) {
        self.inst[i].pp_dir = 1.0;
        let b = self.bounds();
        self.inst[i].position = match self.direction(i) >= 0.0 {
            true => b.rs as f64,
            false => b.re as f64,
        };
        self.restart_grains(i);
    }

    // every instance back to the top; the first one is at rest, playable
    // (a voice that is resumed after a stop plays)
    fn rewind(&mut self) {
        for i in 0..MAX_CHOIR {
            self.rewind_instance(i);
            self.inst[i].exact();
            self.inst[i].live = i == 0;
            self.inst[i].iir = self.resample == ResampleMode::Iir;
        }
        self.next = 0;
    }

    // whether the granular engine is wanted: stretch or pitch moved off 1
    // and 0 (effective values, so a glide switches it only at its ends)
    // (never for a voice of waves only: they take the pitch themselves)
    fn engine_wanted(&self) -> bool {
        !self.waves_only
            && (self.effective(VoiceParam::Stretch) != 1.0
                || self.effective(VoiceParam::Pitch) != 0.0)
    }

    // grains start at the position with the first one fully open (a hard
    // cut); with the engine unwanted, back to the plain head
    //
    // With the spectral shifter in charge of the instance (PSHa) the grains
    // stay idle and the shifter is hit at the position instead; that waits
    // in `shift_hit` until the voice, which holds the source, can do it
    // (`Voice::resolve_hits`, once per frame before the voice renders)
    fn restart_grains(&mut self, i: usize) {
        let wanted = self.engine_wanted();
        let spectral = wanted && self.spectral_for(i);
        let dir = self.direction(i);
        let inst = &mut self.inst[i];
        inst.exit_left = 0;
        inst.spectral = spectral;
        inst.shift_hit = spectral;
        inst.granular = wanted && !spectral;
        if inst.granular {
            inst.grains.reseed(inst.position, dir);
        }
    }

    // whether instance `i` plays spectral when the engine is wanted
    fn spectral_for(&self, i: usize) -> bool {
        self.shift == ShiftMode::Spectral && i < self.spectral_avail
    }

    // the shifter's parameters for instance `i`, from the effective values
    fn shift_params(&self, i: usize) -> ShiftParams {
        let rate =
            f64::from(self.effective(VoiceParam::Rate).abs()) * f64::from(self.inst[i].rate_mul);
        let stretch = f64::from(self.effective(VoiceParam::Stretch));
        let pitch = self.effective(VoiceParam::Pitch);
        shift_params(rate, self.rate_sign(), stretch, pitch_ratio(pitch))
    }

    // just outside the region, on the side it plays toward
    fn park_instance(&mut self, i: usize) {
        let b = self.bounds();
        self.inst[i].position = match self.direction(i) >= 0.0 {
            true => (b.rs - 1) as f64,
            false => (b.re + 1) as f64,
        };
        self.inst[i].live = false;
    }

    fn park(&mut self) {
        for i in 0..MAX_CHOIR {
            self.park_instance(i);
        }
    }
}

/// What a voice plays: a track's samples, or a buffer's (RECb), or nothing
/// (a voice of waves only, WAVa).
#[derive(Clone)]
pub enum VoiceSource {
    /// No recorded source: only the voice's waves sound.
    None,
    /// Interleaved samples, shared with every other voice playing the track.
    Track(Arc<[f32]>),
    /// A buffer, which may be recording while the voice plays it.
    Buffer(Arc<BufData>),
}

impl VoiceSource {
    /// Frames that can be played, with `channels` samples a frame.
    fn frames(&self, channels: usize) -> usize {
        match self {
            VoiceSource::None => 0,
            VoiceSource::Track(s) => s.len() / channels.max(1),
            VoiceSource::Buffer(b) => b.frames_heard(),
        }
    }

    // one sample
    fn sample(&self, frame: usize, ch: usize, channels: usize, front: usize) -> f32 {
        match self {
            VoiceSource::None => 0.0,
            VoiceSource::Track(s) => s[frame * channels + ch],
            VoiceSource::Buffer(b) => b.read_in(front, frame, ch),
        }
    }

    // the buffer half to read this frame (0 for a track)
    fn heard_half(&self) -> usize {
        match self {
            VoiceSource::None | VoiceSource::Track(_) => 0,
            VoiceSource::Buffer(b) => b.heard_half(),
        }
    }
}

/// One playing instance of a track or buffer.
pub struct Voice {
    /// What it plays.
    pub(crate) source: VoiceSource,
    pub(crate) channels: usize,
    pub state: VoiceState,
    pub group: Option<GroupId>,
    /// The voice's effects, run over its output frame (FXa).
    pub(crate) fx: FxChain,
    /// The voice's waves, summed into its source frame (WAVa).
    pub(crate) waves: Waves,
    /// The buffers recording this voice's sound (slots).
    pub(crate) taps: Vec<u32>,
    /// The spectral shifters, one per built choir instance (PSHa); built on
    /// the command thread and carried in by `Command::SetShifters`.
    pub(crate) shifters: Option<Box<VoiceShifters>>,
    // the Elliptic-BLEP resampler's state, one per choir instance (REQ-143).
    // Inline (a voice is built on the audio thread too), and in a cell so
    // that `render_frame` can write it while it reads the rest of the voice.
    iir: RefCell<IirBank>,
    // the buffer half last heard, to see a flip (`sync_len`)
    seen_half: usize,
}

impl Voice {
    pub(crate) fn new(track: &Track, tempo: TempoId, adopts: bool) -> Self {
        Self {
            // a refcount bump; loading never copies samples
            source: VoiceSource::Track(Arc::clone(&track.samples)),
            channels: track.channels as usize,
            // an empty track has no frames; load_assets skips those,
            // but don't underflow if one gets through
            state: VoiceState::new(track.frames, tempo, adopts),
            group: None,
            fx: FxChain::new(),
            waves: Waves::new(),
            taps: Vec::new(),
            shifters: None,
            iir: RefCell::new(new_bank()),
            seen_half: 0,
        }
    }

    /// A voice of waves only: no source, one channel (a mono track, panned
    /// by `pan`).
    pub(crate) fn from_waves(tempo: TempoId, adopts: bool) -> Self {
        let mut state = VoiceState::new(0, tempo, adopts);
        state.waves_only = true;
        Self {
            source: VoiceSource::None,
            channels: 1,
            state,
            group: None,
            fx: FxChain::new(),
            waves: Waves::new(),
            taps: Vec::new(),
            shifters: None,
            iir: RefCell::new(new_bank()),
            seen_half: 0,
        }
    }

    /// The track's samples (tests; panics for a buffer voice).
    #[cfg(test)]
    pub(crate) fn track(&self) -> &Arc<[f32]> {
        match &self.source {
            VoiceSource::Track(samples) => samples,
            VoiceSource::Buffer(_) => panic!("a buffer voice has no track"),
            VoiceSource::None => panic!("a voice of waves has no track"),
        }
    }

    /// A voice that plays a buffer.
    pub(crate) fn from_buffer(data: Arc<BufData>, tempo: TempoId, adopts: bool) -> Self {
        let channels = usize::from(data.channels);
        let frames = data.frames_heard();
        let seen_half = data.heard_half();
        Self {
            source: VoiceSource::Buffer(data),
            channels,
            state: VoiceState::new(frames, tempo, adopts),
            group: None,
            fx: FxChain::new(),
            waves: Waves::new(),
            taps: Vec::new(),
            shifters: None,
            iir: RefCell::new(new_bank()),
            seen_half,
        }
    }

    /// A buffer voice plays the frames recorded so far, which grow while
    /// the take runs: the region follows. Called before the voice reads.
    pub(crate) fn sync_len(&mut self) {
        if let VoiceSource::Buffer(b) = &self.source {
            self.state.len = b.frames_heard();
            // a flip of the heard half changes what a snapshot read (the
            // key holds the half too, but two flips could return to it)
            let half = b.heard_half();
            if half != self.seen_half {
                self.seen_half = half;
                if let Some(s) = self.shifters.as_deref_mut() {
                    s.invalidate();
                }
            }
        }
    }

    /// Sets the choir size; the resampler state of instances that go quiet
    /// is dropped.
    pub(crate) fn set_choir(&mut self, n: u8) {
        self.state.set_choir(n);
        for q in &mut self.iir.get_mut()[usize::from(self.state.choir())..] {
            q.silence();
        }
    }

    /// Back to the top of the region: its first frame, or its last when
    /// playing backwards. Doesn't count as a trigger. Every instance goes,
    /// and only the first stays playable.
    pub(crate) fn rewind(&mut self) {
        self.sync_len();
        self.state.rewind();
        self.waves.reset_phases();
        self.silence_iir();
    }

    /// Silences the Voice until something retriggers it: puts the position
    /// just outside the region, on the side it plays toward.
    pub(crate) fn park(&mut self) {
        self.sync_len();
        self.state.park();
        self.silence_iir();
    }

    // the resampler forgets every instance's line: a stop is a hard cut, and
    // what plays next begins from silence
    fn silence_iir(&mut self) {
        for q in self.iir.get_mut().iter_mut() {
            q.silence();
        }
    }

    /// A hit: plays from the top of the region (see `VoiceState::hit`) and
    /// counts a trigger.
    pub(crate) fn hit(&mut self) {
        self.sync_len();
        self.state.hit();
        let started = self.state.started;
        for i in (0..MAX_CHOIR).filter(|i| started >> i & 1 == 1) {
            self.waves.hit(i);
        }
    }

    // whether the spectral shifters can play this source: a track, or a
    // buffer with two halves (a `live=yes` buffer grows while it is read,
    // so the shifter, which reads ahead, can't follow it)
    fn shiftable(&self) -> bool {
        match &self.source {
            VoiceSource::None => false,
            VoiceSource::Track(_) => true,
            VoiceSource::Buffer(b) => b.halves() == 2,
        }
    }

    // recomputes how many instances can play spectral; call whenever the
    // shifters or the source change
    fn refresh_shift(&mut self) {
        self.state.spectral_avail = match self.shifters.as_deref() {
            Some(s)
                if self.shiftable() && s.channels() == self.channels.min(MAX_VOICE_CHANNELS) =>
            {
                s.instances()
            }
            _ => 0,
        };
    }

    /// Takes new shifters and returns the old ones, for the garbage queue.
    /// Each instance that plays spectral (or now can) is hit again at its
    /// head, so the new shifters start aligned.
    pub(crate) fn set_shifters(
        &mut self,
        shifters: Box<VoiceShifters>,
    ) -> Option<Box<VoiceShifters>> {
        let old = self.shifters.replace(shifters);
        self.refresh_shift();
        for i in 0..MAX_CHOIR {
            let inst = &self.state.inst[i];
            let moves_to_spectral = inst.granular && self.state.spectral_for(i);
            if inst.spectral || moves_to_spectral {
                self.state.restart_grains(i);
            }
        }
        old
    }

    /// Chooses the engine for `pitch=`/`stretch=`. Flipping it while an
    /// instance plays is a hard cut, as `restart_grains` is.
    pub(crate) fn set_shift_mode(&mut self, mode: ShiftMode) {
        if self.state.shift == mode {
            return;
        }
        self.state.shift = mode;
        for i in 0..MAX_CHOIR {
            let inst = &self.state.inst[i];
            if inst.granular || inst.spectral {
                self.state.restart_grains(i);
            }
        }
    }

    /// Settles the pending hits (`restart_grains` flags them; REQ-099
    /// section 4). Called once a frame, before the voice renders, so an
    /// onset sounds on the frame it is due. Each live pending instance:
    /// - whose hit matches the voice's snapshot copies it, free of budget;
    /// - otherwise runs a cold `output_seek` into the snapshot if `budget`
    ///   allows and copies that (so a choir that shares a key costs one);
    /// - with the budget spent plays this note on the grains, and is
    ///   spectral again from its next hit.
    ///
    /// Instances that aren't live are hit when they start.
    pub(crate) fn resolve_hits(&mut self, budget: &mut ColdBudget) {
        if !self.state.inst.iter().any(|i| i.shift_hit) {
            return;
        }
        let choir = usize::from(self.state.choir);
        let Some(shifters) = self.shifters.as_deref_mut() else {
            for inst in &mut self.state.inst {
                inst.shift_hit = false;
            }
            return;
        };
        let mut b = self.state.bounds();
        b.front = self.source.heard_half();
        let path = Path {
            source: &self.source,
            channels: self.channels,
            frames: self.source.frames(self.channels),
            b: &b,
            mode: self.state.loop_mode,
            bounce: self.state.bounce,
        };
        let source_id = match &self.source {
            VoiceSource::None => 0,
            VoiceSource::Track(s) => s.as_ptr() as usize,
            VoiceSource::Buffer(d) => Arc::as_ptr(d) as usize,
        };
        let loop_mode = (match self.state.loop_mode {
            LoopMode::Off => 0,
            LoopMode::Forward => 1,
            LoopMode::PingPong => 2,
        }) | if b.looping { 0x80 } else { 0 };
        for i in 0..MAX_CHOIR {
            let inst = &mut self.state.inst[i];
            if !inst.shift_hit {
                continue;
            }
            inst.shift_hit = false;
            if !inst.live || i >= choir {
                continue;
            }
            let (pos, dir) = (inst.position, self.state.direction(i));
            let params = self.state.shift_params(i);
            let key = HitKey::new(
                source_id,
                b.front,
                (pos, dir),
                (b.rs, b.re, b.ls, b.le),
                loop_mode,
                &params,
            );
            if shifters.resolve_hit(i, key, &path, (pos, dir), params, budget)
                == HitOutcome::Refused
            {
                // no budget for a fresh seek: this note plays on the grains
                let inst = &mut self.state.inst[i];
                inst.spectral = false;
                inst.granular = true;
                inst.exit_left = 0;
                inst.grains.reseed(pos, dir);
            }
        }
    }

    /// Plays another track. The caller gets the old source back, to retire
    /// it, since dropping the last `Arc` would free on the audio thread.
    /// A voice that was playing keeps its place in the region (by fraction);
    /// one that wasn't stays silent.
    pub(crate) fn swap_track(&mut self, track: &Track) -> VoiceSource {
        self.swap_source(
            VoiceSource::Track(Arc::clone(&track.samples)),
            track.channels as usize,
        )
    }

    /// `swap_track` for any source (`channels` samples a frame).
    pub(crate) fn swap_source(&mut self, source: VoiceSource, channels: usize) -> VoiceSource {
        let old_len = self.state.len;
        // a voice of waves gains a source: its sounding instances start
        // the new region at the top
        let was_waves = matches!(self.source, VoiceSource::None);
        let mut was_in = [false; MAX_CHOIR];
        {
            let b = self.state.bounds();
            for (w, inst) in was_in.iter_mut().zip(&self.state.inst) {
                *w = inst.live && (was_waves || self.state.playing(&b, inst.position));
            }
        }
        let old = std::mem::replace(&mut self.source, source);
        for q in self.iir.get_mut().iter_mut() {
            q.on = false;
        }
        self.state.waves_only = matches!(self.source, VoiceSource::None);
        self.channels = channels;
        self.state.len = self.source.frames(channels);
        if let Some(s) = self.shifters.as_deref_mut() {
            s.invalidate();
        }
        self.refresh_shift();
        let new_len = self.state.len;
        let b = self.state.bounds();
        for (i, was) in was_in.into_iter().enumerate() {
            match was {
                true => {
                    let forward = self.state.direction(i) >= 0.0;
                    let inst = &mut self.state.inst[i];
                    inst.position = if was_waves {
                        // no place in a region yet: start at its playing edge
                        match forward {
                            true => b.rs as f64,
                            false => b.re.max(b.rs) as f64,
                        }
                    } else {
                        let scaled = inst.position * new_len as f64 / old_len.max(1) as f64;
                        scaled.clamp(b.rs as f64, (b.re.max(b.rs)) as f64)
                    };
                }
                false => self.state.park_instance(i),
            }
            if self.state.inst[i].granular || self.state.inst[i].spectral {
                self.state.restart_grains(i);
            }
        }
        old
    }

    /// Adds this Voice's contribution to one output frame: every instance
    /// that is sounding.
    pub(crate) fn render_frame(&mut self, frame: &mut [f32], blep: &BlepTable, now: u64) {
        let has_source = self.source.frames(self.channels) > 0;
        if !self.state.active || self.channels == 0 || (!has_source && self.waves.is_empty()) {
            return;
        }
        // the waves first: they move their phases, which the track part
        // (which only reads) can't. Each sounding instance plays them at
        // the voice's pitch and rate, and its own detune.
        let mut wave = [0.0f32; MAX_CHOIR];
        if !self.waves.is_empty() {
            self.waves.set_now(now);
            let state = &self.state;
            let sr = match sample_rate::get() {
                0 => 48_000.0,
                n => f64::from(n),
            };
            let base = pitch_ratio(state.effective(VoiceParam::Pitch))
                * f64::from(state.effective(VoiceParam::Rate).abs());
            for (i, inst) in state.inst[..usize::from(state.choir)].iter().enumerate() {
                if inst.live && inst.delay_left == 0 {
                    let ratio = base * f64::from(inst.rate_mul);
                    wave[i] = self.waves.render(i, ratio, sr, blep);
                }
            }
        }
        let sounds_wave = |inst: &Instance| inst.live && inst.delay_left == 0;
        let mut iir = self.iir.borrow_mut();
        let state = &self.state;
        let mut b = state.bounds();
        b.front = self.source.heard_half();
        let gain = state.effective(VoiceParam::Gain);
        let pan = state.effective(VoiceParam::Pan);
        let nch = self.channels.min(MAX_VOICE_CHANNELS);
        let lanes = &state.inst[..usize::from(state.choir)];
        for (i, inst) in lanes.iter().enumerate() {
            if !inst.live || inst.delay_left > 0 {
                iir[i].silence();
                continue;
            }
            let mut src = [0.0f32; MAX_VOICE_CHANNELS];
            // a pending hit is settled before the frame (`resolve_hits`);
            // its shifter holds the last note until then
            let track = has_source && !inst.shift_hit && state.playing(&b, inst.position);
            let mut tail = false;
            if track {
                let plain = inst.iir && !inst.granular && !inst.spectral;
                if !(plain && self.render_iir(&b, inst, blep, &mut iir[i], &mut src[..nch])) {
                    iir[i].on = false;
                    self.render_instance(&b, i, inst, &mut src[..nch]);
                    iir[i].last[..nch].copy_from_slice(&src[..nch]);
                }
            } else if has_source
                && inst.iir
                && !inst.shift_hit
                && (iir[i].on || iir[i].tail_left > 0)
            {
                // the head has left the region: the filter rings out
                tail = self.render_tail(blep, &mut iir[i], &mut src[..nch]);
            } else {
                iir[i].silence();
            }
            let waves = sounds_wave(inst) && !self.waves.is_empty();
            if waves {
                add_mono(&mut src[..nch], wave[i]);
            }
            if !track && !tail && !waves {
                continue;
            }
            let pan = (pan + inst.pan_add).clamp(-1.0, 1.0);
            mix(&src[..nch], frame, gain * inst.gain_mul, pan);
        }
    }

    // One instance's source frame through the Elliptic-BLEP resampler
    // (REQ-143). The head moves `speed` source frames per output frame; the
    // continuous-time input is the line through the pre-emphasised source
    // frames (`PREEMPH`, which undoes the line's sinc^2 droop). The output
    // is that line, low-passed at 20 kHz: the line at the head, plus the
    // filter's response to each corner the head passes (the BLEP residue),
    // plus its delay of the slope (`slope_term`).
    //
    // The filter is causal, so its output is the line `delay()` frames late
    // (HPRL). The line is therefore read at a virtual head that leads the
    // real one by `delay() * speed` source frames (`q.lead`, in the head's
    // direction, folded through the loop like `travel` folds a step): the
    // output at frame n is the low-passed source at n + delay(), which is the
    // source at n below the filter's corner. `q.lead` follows the speed
    // (`LEAD_SLEW`), so an instant change of speed bends the virtual head's
    // speed for a few frames instead of moving it.
    //
    // `q.slope` is the slope the filter has been told about (the sum of the
    // order-2 events it has taken). Every frame the slope of the segment the
    // virtual head is on, at its speed, is compared with it and the
    // difference is an event: that is how a change of speed, of direction or
    // of the segment's own slope at a hit becomes band-limited, and why
    // `slope_term` never jumps.
    //
    // Loops and ping-pong are no special case: a loop is the periodic (or
    // mirrored) extension of the source, whose knots `tap_in` folds, so a
    // wrap or a turn is a knot like any other.
    //
    // A hit, or any other jump of the head, restarts the filter from the last
    // output, and the lead can't reach into the past: the filter runs
    // `floor(delay())` frames without output first (a "pre-roll"), from the
    // onset at `delay() - floor(delay())` frames before its first frame, so
    // the first output frame is exactly `delay()` frames into the note.
    //
    // False when the frame has to be read the old way (cubic): the head is
    // faster than the corner loop follows (a frame or two while the engine
    // switches to grains), or it is close to the end of a buffer that is
    // still being recorded, where the source isn't settled, or the region
    // is shorter than the lead. Nothing is output then; the next frame
    // restarts from the output the caller makes.
    fn render_iir(
        &self,
        b: &Bounds,
        inst: &Instance,
        table: &BlepTable,
        q: &mut IirInst,
        out: &mut [f32],
    ) -> bool {
        let st = &self.state;
        let mode = st.loop_mode;
        let p = inst.position;
        let signed_rate = st.effective(VoiceParam::Rate);
        // the lead is kept as the unfolded head sees it (the rate's sign): a
        // ping-pong turn flips the head's direction, not the lead
        let rate_sign = if signed_rate < 0.0 { -1.0 } else { 1.0 };
        let bounce = inst.pp_dir;
        let dir = rate_sign * bounce;
        // the speed the head moves at (`VoiceState::step`, bit for bit)
        let rate = f64::from(signed_rate.abs()) * f64::from(inst.rate_mul);
        let speed = rate * f64::from(st.effective(VoiceParam::Stretch));
        let tau = table.delay();
        let du0 = dir * speed;
        let target = rate_sign * speed * tau;
        let jump = (p - q.expect).abs() > 1e-7;
        let lead = if jump || !q.on { target } else { q.lead };
        if speed > IIR_MAX_SPEED
            || self.growing_edge_near(b, p, speed + lead.abs().max(target.abs()))
        {
            q.on = false;
            return false;
        }
        // where the real head goes next, folded the way `step` does
        let mut next = p;
        travel(&mut next, dir, speed, b, mode);
        let tail_running = !q.on && q.tail_left > 0;
        if !jump && (q.on || tail_running) {
            let (v, sigma) = fold_lead(p, bounce * q.lead, b, mode);
            let err = target - q.lead;
            // the lead may shrink by at most `LEAD_SHRINK` of the head's step
            // a frame, so the virtual head never turns back against the real
            // one (a drop to rate 0 leaves it waiting for the lead to be
            // eaten up)
            let new_lead = if err.abs() < 1e-9 {
                target
            } else {
                let along = (rate_sign * err / LEAD_SLEW).max(-LEAD_SHRINK * speed);
                q.lead + rate_sign * along
            };
            if !region_plays(b, v) {
                // the virtual head has left the region, `delay()` before the
                // real one: the filter rings out from here
                q.lead = new_lead;
                q.expect = next;
                self.render_tail(table, q, out);
                return true;
            }
            if q.on {
                let du = sigma * (du0 + bounce * (new_lead - q.lead));
                q.lead = new_lead;
                let head = VHead {
                    pos: v,
                    du,
                    dir: heading(du, sigma * dir),
                    onset: None,
                };
                if !self.iir_frame(b, table, q, head, Some(out)) {
                    q.on = false;
                    return false;
                }
                q.expect = next;
                return true;
            }
            // else a ringing tail, and the virtual head is back in a
            // region: restart below
        }
        // a restart. From silence the lead is reached by a pre-roll, so the
        // onset is where cubic's is. Where the output was sounding (a
        // retrigger), a pre-roll would cut the first half of the step from the
        // last output into the output, so the virtual head starts the lead
        // ahead and the step goes through the filter whole, as it did before
        // the lead
        q.tail_left = 0;
        q.lead = target;
        let silent = q.last[..out.len()].iter().all(|x| x.abs() < 1e-6);
        // the onset's step is put `half_time()` before the first output
        // frame, so that it is half done at the hit (a few hundredths of a
        // frame before the line starts, which goes back by that much)
        let onset = table.half_time();
        let (pre, first, past) = match silent {
            true => {
                let pre = (onset.floor() as usize).min(IIR_MAX_PREROLL);
                (pre, tau - pre as f64, onset - pre as f64)
            }
            false => (0, tau, 0.0),
        };
        for j in 0..=pre {
            let (v, sigma) = fold_lead(p, du0 * (first + j as f64), b, mode);
            if !region_plays(b, v) {
                q.on = false;
                return false;
            }
            let du = sigma * du0;
            let head = VHead {
                pos: v,
                du,
                dir: heading(du, dir),
                onset: (j == 0).then_some(past),
            };
            let o = (j == pre).then_some(&mut *out);
            if !self.iir_frame(b, table, q, head, o) {
                q.on = false;
                return false;
            }
        }
        q.expect = next;
        true
    }

    // One frame of the filter, for the virtual head `h`: its output, if `out`
    // is given (a frame of pre-roll has none), and the events up to the next
    // frame. False when the frame needs more of the source than the window
    // holds.
    #[inline(always)]
    fn iir_frame(
        &self,
        b: &Bounds,
        table: &BlepTable,
        q: &mut IirInst,
        h: VHead,
        mut out: Option<&mut [f32]>,
    ) -> bool {
        let VHead {
            pos: p,
            du,
            dir,
            onset,
        } = h;
        let speed = du.abs();
        let segment = |x: f64| -> i64 {
            match dir >= 0.0 {
                true => floor_i64(x),
                false => ceil_i64(x) - 1,
            }
        };
        let a = segment(p);
        let frac = p - a as f64;
        // where the onset was (the virtual head's position when the note
        // started, `past` frames ago)
        // (a hit that wrapped the loop within `past` frames began on the other
        // side of the seam: the step goes at the head, a hair late, rather
        // than reading frames that aren't the path's)
        let (ls, le1) = (b.ls as f64, (b.le + 1) as f64);
        let in_loop = |x: f64| b.looping && x >= ls && x < le1;
        let start = onset
            .map(|past| p - du * past)
            .map(|s| if in_loop(p) && !in_loop(s) { p } else { s });
        let start_seg = start.map(segment);
        // the knots (integer positions) between the start (or the head) and
        // the head's next position
        let from = start.unwrap_or(p);
        let (first, last) = match du {
            d if d > 0.0 => (floor_i64(from) + 1, floor_i64(p + du)),
            d if d < 0.0 => (ceil_i64(p + du), ceil_i64(from) - 1),
            _ => (1, 0),
        };
        let has = first <= last;
        // the knots up to the head (the onset's stretch, when there is one)
        // and the knots on from it, as ranges
        let (back, ahead) = match (onset.is_some(), du > 0.0) {
            (false, _) => ((1, 0), (first, last)),
            (true, true) => (
                (first, last.min(floor_i64(p))),
                (first.max(floor_i64(p) + 1), last),
            ),
            (true, false) => (
                (first.max(ceil_i64(p)), last),
                (first, last.min(ceil_i64(p) - 1)),
            ),
        };
        // the pre-emphasised source is needed at m_lo..=m_hi, so the raw
        // source from 3 frames before to 3 after
        let (mut m_lo, mut m_hi) = (a, a + 1);
        if has {
            m_lo = m_lo.min(first - 1);
            m_hi = m_hi.max(last + 1);
        }
        if let Some(s) = start_seg {
            m_lo = m_lo.min(s);
            m_hi = m_hi.max(s + 1);
        }
        let (lo, hi) = (m_lo - 3, m_hi + 3);
        let n_raw = (hi - lo + 1) as usize;
        if n_raw > IIR_WINDOW {
            return false;
        }
        let plain = self.plain_range(b, lo, hi);
        // all the windows fold alike, or (once per entry into a loop) not
        let uniform = folds(m_lo, dir, a, b) == folds(m_hi, dir, a, b);
        let fold = folds(m_lo, dir, a, b);
        let mut added = false;
        // (scratch kept in `q`, not zeroed per frame: every entry read below
        // is written first, PERF-IIR)
        let (raw, pre) = (&mut q.raw, &mut q.pre);
        let nch = self.channels.min(MAX_VOICE_CHANNELS);
        for c in 0..nch {
            if uniform {
                match (&self.source, plain) {
                    (VoiceSource::Track(t), true) => {
                        // the common case: a strided run of a slice
                        let (at, ch) = (lo as usize * self.channels + c, self.channels);
                        for (k, r) in raw[..n_raw].iter_mut().enumerate() {
                            *r = f64::from(t[at + k * ch]);
                        }
                    }
                    (_, true) => {
                        for (j, r) in (lo..hi + 1).zip(raw.iter_mut()) {
                            let x = self.source.sample(j as usize, c, self.channels, b.front);
                            *r = f64::from(x);
                        }
                    }
                    (_, false) => {
                        for (j, r) in (lo..hi + 1).zip(raw.iter_mut()) {
                            *r = self.path_sample(b, fold, j, c);
                        }
                    }
                }
                for (v, w) in pre.iter_mut().zip(raw[..n_raw].windows(7)) {
                    *v = pre_emphasis(w);
                }
            } else {
                for (m, v) in (m_lo..m_hi + 1).zip(pre.iter_mut()) {
                    let f = folds(m, dir, a, b);
                    let raw: [f64; 7] =
                        std::array::from_fn(|t| self.path_sample(b, f, m + t as i64 - 3, c));
                    *v = pre_emphasis(&raw);
                }
            }
            // the corner at knot `k`: the change of the line's slope per
            // source frame, 0 where the region has no source
            let corner = |k: i64| -> f64 {
                if !b.looping && (k < b.rs || k > b.re) {
                    return 0.0;
                }
                let at = (k - m_lo) as usize;
                pre[at + 1] - 2.0 * pre[at] + pre[at - 1]
            };
            let (x0, x1) = (pre[(a - m_lo) as usize], pre[(a + 1 - m_lo) as usize]);
            let line = x0 + frac * (x1 - x0);
            let state = &mut q.state[c];
            if let (Some(past), Some(s), Some(sa)) = (onset, start, start_seg) {
                // the note's onset: a step from the last output to the line,
                // and the slope the line starts on
                let (s0, s1) = (pre[(sa - m_lo) as usize], pre[(sa + 1 - m_lo) as usize]);
                let line_s = s0 + (s - sa as f64) * (s1 - s0);
                *state = QUIET;
                table.add(state, line_s - f64::from(q.last[c]), 1, past);
                q.slope[c] = du * (s1 - s0);
                table.add(state, q.slope[c], 2, past);
                added = true;
                // and the corners between the onset and the head
                for k in back.0..back.1 + 1 {
                    let sd = corner(k);
                    if sd != 0.0 {
                        let ago = ((p - k as f64) / du).clamp(0.0, 1.0);
                        table.add(state, speed * sd, 2, ago);
                        q.slope[c] += speed * sd;
                    }
                }
            }
            // what the filter hasn't been told of the slope yet
            let want = du * (x1 - x0);
            let diff = want - q.slope[c];
            if diff.abs() > 1e-12 * (1.0 + want.abs()) {
                table.add(state, diff, 2, 0.0);
                added = true;
            }
            // (a rounding-sized difference is not told, but `slope_term` goes
            // by the slope the line really has)
            q.slope[c] = want;
            let y = line + table.get(state) + table.slope_term(q.slope[c]);
            if let Some(o) = out.as_deref_mut() {
                o[c] = y as f32;
                q.last[c] = o[c];
            }
            table.step(state);
            // the line where the head will be next frame, from the knots at
            // hand: what the filter is on, should the source end there
            let pn = p + du;
            let an = segment(pn);
            let at_n = (an - m_lo) as usize;
            if let (Some(n0), Some(n1)) = (pre.get(at_n), pre.get(at_n + 1)) {
                q.next_line[c] = n0 + (pn - an as f64) * (n1 - n0);
            } else {
                q.next_line[c] = line;
            }
            // the corners passed on the way to the next frame
            for k in ahead.0..ahead.1 + 1 {
                let sd = corner(k);
                if sd != 0.0 {
                    let past = (1.0 - (k as f64 - p) / du).clamp(0.0, 1.0);
                    table.add(state, speed * sd, 2, past);
                    q.slope[c] += speed * sd;
                    added = true;
                }
            }
        }
        // a line that has no corners any more (silence, a stopped head) lets
        // the filter's state die away; zero it before it goes denormal
        match added {
            true => q.idle = 0,
            false => {
                q.idle = q.idle.saturating_add(1);
                if q.idle > IIR_IDLE {
                    q.state = [QUIET; MAX_VOICE_CHANNELS];
                }
            }
        }
        q.on = true;
        true
    }

    // The frames of an instance whose head has left a region that doesn't
    // loop: the source goes silent at that point, which is a step and a
    // change of slope like any other, so it goes through the filter, and the
    // filter rings out (about 20 ms) before the instance stops. False when
    // there is nothing (left) to output.
    fn render_tail(&self, table: &BlepTable, q: &mut IirInst, out: &mut [f32]) -> bool {
        if q.on {
            // the first frame: the line the filter was on is replaced by
            // silence (the head may be anywhere by now, if the region moved
            // off it: the source isn't read)
            for (c, _) in out.iter().enumerate() {
                let state = &mut q.state[c];
                table.add(state, -q.next_line[c], 1, 0.0);
                table.add(state, -q.slope[c], 2, 0.0);
                q.slope[c] = 0.0;
            }
            q.on = false;
            q.tail_left = IIR_TAIL;
        }
        if q.tail_left == 0 {
            q.silence();
            return false;
        }
        q.tail_left -= 1;
        for (c, o) in out.iter_mut().enumerate() {
            *o = table.get(&q.state[c]) as f32;
            q.last[c] = *o;
            table.step(&mut q.state[c]);
        }
        true
    }

    // the source at frame `j` as the head's path sees it (folded through the
    // loop where the head is in it or past it)
    fn path_sample(&self, b: &Bounds, fold: bool, j: i64, c: usize) -> f64 {
        let (mode, bounce) = (self.state.loop_mode, self.state.bounce);
        let at = if fold { b.ls } else { j };
        f64::from(tap_in(
            &self.source,
            self.channels,
            mode,
            bounce,
            b,
            at,
            j,
            c,
        ))
    }

    // whether every frame in lo..=hi is the source's own, untouched: inside
    // the region (and the loop, clear of the ping-pong fade), where there is
    // nothing to fold or extend. Reads are then plain loads.
    fn plain_range(&self, b: &Bounds, lo: i64, hi: i64) -> bool {
        if lo < b.rs || hi > b.re {
            return false;
        }
        if !b.looping {
            return true;
        }
        let fade = match self.state.loop_mode {
            LoopMode::PingPong => (self.state.bounce as i64).min((b.le - b.ls) / 2),
            _ => 0,
        };
        lo - b.ls >= fade && b.le - hi >= fade
    }

    // whether the head of a buffer that is still being recorded is within
    // reach of its newest frame: the source past there isn't there yet.
    // `reach` is how far the reader looks ahead of `p`: its step, and the
    // lead of the virtual head
    fn growing_edge_near(&self, b: &Bounds, p: f64, reach: f64) -> bool {
        match &self.source {
            VoiceSource::Buffer(buf) if buf.halves() == 1 => {
                p + reach + 8.0 > (b.re + 1) as f64 && b.re + 1 >= 0
            }
            _ => false,
        }
    }

    // one instance's source frame
    fn render_instance(&self, b: &Bounds, i: usize, inst: &Instance, src: &mut [f32]) {
        let nch = src.len();
        if inst.spectral {
            let shifted = self
                .shifters
                .as_deref()
                .and_then(|s| s.voice(i))
                .map(|sv| sv.frame())
                .filter(|f| f.len() == nch);
            let Some(shifted) = shifted else {
                // can't happen: spectral instances have shifters
                self.read_at(b, inst.position, src);
                return;
            };
            match inst.exit_left {
                0 => src.copy_from_slice(shifted),
                left => {
                    // leaving the engine: the shifter fades out into the head
                    let w = left as f32 / inst.grains.hop() as f32;
                    self.read_at(b, inst.position, src);
                    for (s, g) in src.iter_mut().zip(shifted) {
                        *s = *s * (1.0 - w) + g * w;
                    }
                }
            }
            return;
        }
        if !inst.granular {
            self.read_at(b, inst.position, src);
            return;
        }
        let mut grained = [0.0f32; MAX_VOICE_CHANNELS];
        // a region that moved (a start glide) can leave a grain outside it
        let (lo, hi) = (b.rs as f64, (b.re + 1) as f64 - 1e-6);
        inst.grains.render(
            |p, buf| self.read_at(b, p.clamp(lo, hi), buf),
            &mut grained[..nch],
        );
        match inst.exit_left {
            0 => src.copy_from_slice(&grained[..nch]),
            left => {
                // leaving the engine: grains fade out into the head
                let w = left as f32 / inst.grains.hop() as f32;
                self.read_at(b, inst.position, src);
                for (s, g) in src.iter_mut().zip(&grained) {
                    *s = *s * (1.0 - w) + g * w;
                }
            }
        }
    }

    // the source frame at `pos` (Hermite between frames), as many channels
    // as `out` holds. The position is inside the region.
    fn read_at(&self, b: &Bounds, pos: f64, out: &mut [f32]) {
        let idx = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let g = bounce_gain(b, self.state.loop_mode, idx as i64, self.state.bounce);
        for (c, s) in out.iter_mut().enumerate() {
            *s = self.source.sample(idx, c, self.channels, b.front) * g;
        }
        if frac != 0.0 {
            let t = frac;
            let i = idx as i64;
            for (c, s) in out.iter_mut().enumerate() {
                let [xm, x0, x1, x2] = [i - 1, i, i + 1, i + 2].map(|j| self.tap(b, i, j, c));
                *s = hermite(xm, x0, x1, x2, t);
            }
        }
    }

    // the sample at frame `j` of channel `c`, as a read around frame `i` sees it
    fn tap(&self, b: &Bounds, i: i64, j: i64, c: usize) -> f32 {
        let (mode, bounce) = (self.state.loop_mode, self.state.bounce);
        tap_in(&self.source, self.channels, mode, bounce, b, i, j, c)
    }

    /// Moves the position; called once per frame, after `render_frame`,
    /// while the voice is running. Clears the frame's modulation.
    pub(crate) fn advance(&mut self) {
        if !self.state.active {
            return;
        }
        let channels = self.channels;
        let front = self.source.heard_half();
        // the source as one channel, for lining grains up
        let mono = |frame: usize| -> f32 {
            (0..channels)
                .map(|c| self.source.sample(frame, c, channels, front))
                .sum()
        };
        let spec = match self.state.spectral_avail {
            0 => None,
            _ => self.shifters.as_deref_mut().map(|shifters| SpectralCtx {
                source: &self.source,
                channels,
                front,
                frames: self.source.frames(channels),
                shifters,
            }),
        };
        self.state.step(Some(&mono), spec);
        self.state.advance_ramps();
        self.state.clear_mods();
        self.fx.advance();
        self.waves.advance_ramps();
    }

    /// Adds this frame's modulation to a voice parameter or to one of its
    /// effects' parameters.
    pub fn modulate(&mut self, t: ModTarget, op: ModOp) {
        match t {
            ModTarget::Voice(_) => self.state.modulate(t, op),
            ModTarget::Fx { id, param } => self.fx.modulate(id, param, op),
            ModTarget::Wave { id, param } => self.waves.modulate(id, param, op),
        }
    }
}

// How far past a region's edge a read continues the line through its last two
// frames: a 4-point or 7-tap read needs a few frames; a head that is further
// out (a region that moved off it) must not read ever bigger numbers.
const EXTRAPOLATE: i64 = 8;

// The sample at frame `j` of channel `c`, as a read around frame `i` sees
// it. Inside a loop the neighbours wrap or bounce; at a region's edge they
// continue the line through the last two frames, so a linear ramp stays
// exact up to the end.
#[allow(clippy::too_many_arguments)]
fn tap_in(
    source: &VoiceSource,
    channels: usize,
    mode: LoopMode,
    bounce: usize,
    b: &Bounds,
    i: i64,
    j: i64,
    c: usize,
) -> f32 {
    let at = |f: i64| source.sample(f as usize, c, channels, b.front);
    if b.looping && i >= b.ls && i <= b.le {
        let span = b.le - b.ls + 1;
        let off = match mode {
            LoopMode::PingPong if span > 1 => {
                let m = (j - b.ls).rem_euclid(2 * (span - 1));
                m.min(2 * (span - 1) - m)
            }
            LoopMode::PingPong => 0,
            _ => (j - b.ls).rem_euclid(span),
        };
        let f = b.ls + off;
        return at(f) * bounce_gain(b, mode, f, bounce);
    }
    match j {
        j if j > b.re => {
            let edge = at(b.re);
            match b.re > b.rs {
                true => edge + (j - b.re).min(EXTRAPOLATE) as f32 * (edge - at(b.re - 1)),
                false => edge,
            }
        }
        j if j < b.rs => {
            let edge = at(b.rs);
            match b.re > b.rs {
                true => edge + (b.rs - j).min(EXTRAPOLATE) as f32 * (edge - at(b.rs + 1)),
                false => edge,
            }
        }
        // outside the loop the entry fade still applies
        j => at(j) * bounce_gain(b, mode, j, bounce),
    }
}

/// The resampler's state for one instance of a voice (REQ-143): a BLEP
/// filter per channel, and what is needed to tell whether the head moved
/// as expected.
pub(crate) struct IirInst {
    state: [BlepState; MAX_VOICE_CHANNELS],
    // the slope each channel's filter has been told of (its order-2 events)
    slope: [f64; MAX_VOICE_CHANNELS],
    // the last frame it output, per channel: the level a restart begins at
    last: [f32; MAX_VOICE_CHANNELS],
    // the line the head reaches next frame, per channel (a region that ends
    // there steps from it to silence)
    next_line: [f64; MAX_VOICE_CHANNELS],
    // the head's position after this frame; a different position next frame
    // is a jump (a hit, a seek)
    expect: f64,
    // `state` holds a running line
    on: bool,
    // frames since the filter was last told of a corner
    idle: u32,
    // frames of ringing left after the head left its region
    tail_left: u32,
    // how far the virtual head leads the real one, in source frames (signed:
    // the direction of travel); it follows `delay() * speed` (HPRL)
    lead: f64,
    // `iir_frame`'s scratch: the raw source window and its pre-emphasis
    // (kept here so a frame doesn't zero 2 KiB of stack per call)
    raw: [f64; IIR_WINDOW],
    pre: [f64; IIR_WINDOW],
}

type IirBank = [IirInst; MAX_CHOIR];

impl IirInst {
    // forgets the line: a note that starts again begins from silence
    fn silence(&mut self) {
        self.on = false;
        self.last = [0.0; MAX_VOICE_CHANNELS];
        self.slope = [0.0; MAX_VOICE_CHANNELS];
        self.tail_left = 0;
        self.idle = 0;
        self.lead = 0.0;
    }
}

fn new_bank() -> IirBank {
    std::array::from_fn(|_| IirInst {
        state: [QUIET; MAX_VOICE_CHANNELS],
        slope: [0.0; MAX_VOICE_CHANNELS],
        last: [0.0; MAX_VOICE_CHANNELS],
        next_line: [0.0; MAX_VOICE_CHANNELS],
        expect: 0.0,
        on: false,
        idle: 0,
        tail_left: 0,
        lead: 0.0,
        raw: [0.0; IIR_WINDOW],
        pre: [0.0; IIR_WINDOW],
    })
}

// the fastest head the corner loop follows, in source frames per output frame
const IIR_MAX_SPEED: f64 = 32.0;
// the raw source one frame reads: the corners it passes plus the taps
const IIR_WINDOW: usize = 64;
// frames the filter rings after the head left its region (the slowest pole
// is gone by e^-47 in that time)
const IIR_TAIL: u32 = 1024;
// the frames over which the virtual head's lead follows a change of speed (an
// instant one would move the head by `delay() * change`)
const LEAD_SLEW: f64 = 32.0;
// the most the lead shrinks in a frame, as a fraction of the head's step
const LEAD_SHRINK: f64 = 0.75;
// the most frames a restart runs the filter before its first output
const IIR_MAX_PREROLL: usize = 16;
// frames without a corner after which the filter's state is zeroed
const IIR_IDLE: u32 = 2048;

// The pre-emphasis the resampler applies to the traversed source frames: 7
// taps, symmetric (linear phase, so no delay) and read ahead in the source.
// A least-squares fit of 1 / sinc^2(f / fs) up to 18 kHz at 48 kHz, held
// flat above, which cancels the droop of the line through the samples
// (-1.4 dB at 10 kHz, -3 dB at 15 kHz): flat within 0.1 dB to 15 kHz.
const PREEMPH: [f64; 7] = [
    0.000026, 0.033473, -0.185291, 1.309137, -0.185291, 0.033473, 0.000026,
];

// the pre-emphasised source at the middle of 7 raw frames
// `x.floor() as i64` and `x.ceil() as i64`, the same integer for every x
// (NaN, infinities and the saturating ends included), without the call to
// libm that `floor`/`ceil` are on the baseline x86-64 target
#[inline(always)]
fn floor_i64(x: f64) -> i64 {
    let i = x as i64;
    if (i as f64) > x {
        i.saturating_sub(1)
    } else {
        i
    }
}

#[inline(always)]
fn ceil_i64(x: f64) -> i64 {
    let i = x as i64;
    if (i as f64) < x {
        i.saturating_add(1)
    } else {
        i
    }
}

fn pre_emphasis(w: &[f64]) -> f64 {
    let w: &[f64; 7] = w.try_into().expect("seven frames");
    PREEMPH[3] * w[3]
        + PREEMPH[2] * (w[2] + w[4])
        + PREEMPH[1] * (w[1] + w[5])
        + PREEMPH[0] * (w[0] + w[6])
}

// Whether a read around knot `k` folds through the loop, for a head on the
// segment that starts at knot `a`: in the loop, and past it in the direction
// of travel (the head has wrapped). `tap_in` then folds every index it is
// given, which is what puts a wrap or a turn into the source as the knot it
// is, and any loop frame will do to anchor it. Before the loop (going
// forward), or before the head has reached it (going backward, from the
// region's tail), the frames are the source's own, except that the segment
// that starts on the loop's last frame ends on the seam, whichever way the
// head got there (Hermite folds it too).
fn folds(k: i64, dir: f64, a: i64, b: &Bounds) -> bool {
    b.looping
        && ((k >= b.ls && k <= b.le)
            || (dir >= 0.0 && k > b.le)
            || (dir < 0.0 && (k < b.ls || (k == b.le + 1 && a == b.le))))
}

// 4-point cubic Hermite (Catmull-Rom) between `x0` and `x1` at `t` in 0..1
fn hermite(xm: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c1 = 0.5 * (x1 - xm);
    let c2 = xm - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

// adds one sample of a mono signal (the voice's waves) to every channel of a
// source frame; the one place that decides where a wave's sound goes
fn add_mono(src: &mut [f32], s: f32) {
    for c in src {
        *c += s;
    }
}

// mixes one source frame into one output frame (the channel rules of the
// VOXa plan)
fn mix(src: &[f32], out: &mut [f32], gain: f32, pan: f32) {
    let (nsrc, nout) = (src.len(), out.len());
    match (nsrc, nout) {
        (0, _) | (_, 0) => {}
        (1, 1) => out[0] += src[0] * gain,
        (1, _) => {
            // constant power: the same level in the middle as at the sides
            let theta = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
            out[0] += src[0] * gain * theta.cos();
            out[1] += src[0] * gain * theta.sin();
        }
        (_, 1) => out[0] += src.iter().sum::<f32>() / nsrc as f32 * gain,
        _ => {
            // same count, or the channels they share: balance on 0 and 1
            let (g_left, g_right) = ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0));
            for (c, (o, s)) in out.iter_mut().zip(src).enumerate() {
                let balance = match c {
                    0 => g_left,
                    1 => g_right,
                    _ => 1.0,
                };
                *o += s * gain * balance;
            }
        }
    }
}

// the frequency ratio of a pitch in semitones
fn pitch_ratio(pitch: f32) -> f64 {
    match pitch == 0.0 {
        true => 1.0,
        false => f64::from(pitch / 12.0).exp2(),
    }
}

// the shifter's parameters from a rate magnitude, its sign, the stretch and
// the pitch ratio
fn shift_params(rate: f64, sign: f64, stretch: f64, ratio: f64) -> ShiftParams {
    ShiftParams {
        speed: rate * stretch,
        transpose: (rate * ratio) as f32,
        rate_sign: sign,
    }
}

// the way the shifters read a voice's source: whole frames, with the
// region, the loop and the buffer half the voice hears
struct Path<'a> {
    source: &'a VoiceSource,
    channels: usize,
    frames: usize,
    b: &'a Bounds,
    mode: LoopMode,
    bounce: usize,
}

impl ReadPath for Path<'_> {
    fn sample(&self, frame: usize, ch: usize) -> f32 {
        match frame < self.frames {
            true => {
                self.source.sample(frame, ch, self.channels, self.b.front)
                    * bounce_gain(self.b, self.mode, frame as i64, self.bounce)
            }
            false => 0.0,
        }
    }

    fn playing(&self, pos: f64) -> bool {
        region_plays(self.b, pos)
    }

    fn step(&self, pos: &mut f64, dir: f64) -> bool {
        travel(pos, dir, 1.0, self.b, self.mode)
    }
}

// what `step` needs to drive a voice's shifters
struct SpectralCtx<'a> {
    source: &'a VoiceSource,
    channels: usize,
    front: usize,
    frames: usize,
    shifters: &'a mut VoiceShifters,
}

// moves `position` by `speed` frames in direction `dir`, with the loop;
// true when a ping-pong loop bounced (the caller flips the direction)
fn travel(position: &mut f64, dir: f64, speed: f64, b: &Bounds, mode: LoopMode) -> bool {
    *position += speed * dir;
    if !b.looping {
        return false;
    }
    let (ls, le) = (b.ls as f64, b.le as f64);
    match mode {
        LoopMode::Off => {}
        LoopMode::Forward => {
            let span = le - ls + 1.0;
            let past = dir > 0.0 && *position >= le + 1.0;
            let before = dir < 0.0 && *position < ls;
            if past || before {
                *position = ls + (*position - ls).rem_euclid(span);
            }
        }
        LoopMode::PingPong => {
            let mut bounced = false;
            if dir > 0.0 && *position > le {
                *position = le - (*position - le);
                bounced = true;
            } else if dir < 0.0 && *position < ls {
                *position = ls + (ls - *position);
                bounced = true;
            }
            if bounced {
                // a step longer than the loop
                *position = position.clamp(ls, le);
            }
            return bounced;
        }
    }
    false
}

// the virtual head of one frame of the resampler (`render_iir`)
#[derive(Clone, Copy)]
struct VHead {
    // where it is, folded into the region
    pos: f64,
    // its step to the next frame, in `pos`'s own direction (a bounce between
    // here and there is not folded in)
    du: f64,
    // the side of `pos` it is heading to
    dir: f64,
    // a note starts: the frames since its onset
    onset: Option<f64>,
}

// the direction of a head that steps `du` (`otherwise` where it doesn't)
fn heading(du: f64, otherwise: f64) -> f64 {
    match du {
        d if d < 0.0 => -1.0,
        d if d > 0.0 => 1.0,
        _ => otherwise,
    }
}

// where a head at `p` is `lead` source frames on (a signed displacement),
// folded through the loop the way `travel` folds a step, and the sign of the
// fold there (-1 on a ping-pong's mirrored stretch)
fn fold_lead(p: f64, lead: f64, b: &Bounds, mode: LoopMode) -> (f64, f64) {
    let u = p + lead;
    if !b.looping {
        return (u, 1.0);
    }
    let (ls, le) = (b.ls as f64, b.le as f64);
    let span = le - ls + 1.0;
    match mode {
        LoopMode::Off => (u, 1.0),
        LoopMode::Forward => {
            let out = (lead > 0.0 && u >= le + 1.0) || (lead < 0.0 && u < ls);
            match out {
                true => (ls + (u - ls).rem_euclid(span), 1.0),
                false => (u, 1.0),
            }
        }
        LoopMode::PingPong => {
            let out = (lead > 0.0 && u > le) || (lead < 0.0 && u < ls);
            if !out {
                return (u, 1.0);
            }
            if span <= 1.0 {
                return (ls, 1.0);
            }
            let period = 2.0 * (span - 1.0);
            let m = (u - ls).rem_euclid(period);
            match m <= span - 1.0 {
                true => (ls + m, 1.0),
                false => (ls + period - m, -1.0),
            }
        }
    }
}

// the frame a read at `pos` lands on: inside the region, and inside the loop
// where there is one (the way `travel` keeps a read there)
fn fold_pos(pos: f64, step: f64, b: &Bounds, mode: LoopMode) -> usize {
    let mut p = pos;
    if b.looping {
        let (ls, le) = (b.ls as f64, b.le as f64);
        let span = le - ls + 1.0;
        let out = p >= le + 1.0 || (p < ls && step < 0.0);
        if out {
            p = match mode {
                LoopMode::PingPong if span > 1.0 => {
                    let m = (p - ls).rem_euclid(2.0 * (span - 1.0));
                    ls + m.min(2.0 * (span - 1.0) - m)
                }
                LoopMode::PingPong => ls,
                _ => ls + (p - ls).rem_euclid(span),
            };
        }
    }
    p.round().clamp(b.rs as f64, b.re as f64) as usize
}

impl VoiceState {
    // moves each sounding instance's read pointer by one frame's step
    // (`rate * stretch`), and its grains by theirs (`rate * pitch ratio`),
    // with the loop and the region
    //
    // A spectral instance (PSHa) moves its head the same way, which still
    // decides the region's end and `playing`, and then its shifter one
    // frame on
    fn step(&mut self, mono: Option<&dyn Fn(usize) -> f32>, mut spec: Option<SpectralCtx<'_>>) {
        let b = self.bounds();
        let sign = self.rate_sign();
        let rate = f64::from(self.effective(VoiceParam::Rate).abs());
        let stretch = f64::from(self.effective(VoiceParam::Stretch));
        let pitch = self.effective(VoiceParam::Pitch);
        let wanted = self.engine_wanted();
        let ratio = pitch_ratio(pitch);
        let mode = self.loop_mode;
        // the region as the shifters read it, with the buffer half
        let feed_bounds = Bounds {
            front: spec.as_ref().map_or(0, |s| s.front),
            ..b
        };
        let (use_spectral, spectral_avail) =
            (self.shift == ShiftMode::Spectral, self.spectral_avail);
        for i in 0..usize::from(self.choir) {
            let playing = self.playing(&b, self.inst[i].position);
            let inst = &mut self.inst[i];
            if !inst.live {
                continue;
            }
            if inst.delay_left > 0 {
                inst.delay_left -= 1;
                continue;
            }
            if !playing {
                continue;
            }
            let rate = rate * f64::from(inst.rate_mul);
            let dir = sign * inst.pp_dir;

            if travel(&mut inst.position, dir, rate * stretch, &b, mode) {
                inst.pp_dir = -inst.pp_dir;
            }

            let params = shift_params(rate, sign, stretch, ratio);
            if inst.spectral {
                // a pending hit is settled before the frame renders
                if let Some(sp) = spec.as_mut().filter(|_| !inst.shift_hit) {
                    let path = Path {
                        source: sp.source,
                        channels: sp.channels,
                        frames: sp.frames,
                        b: &feed_bounds,
                        mode,
                        bounce: self.bounce,
                    };
                    if let Some(sv) = sp.shifters.voice_mut(i) {
                        sv.advance(&path, params);
                    }
                }
                match (wanted, inst.exit_left) {
                    (true, _) => inst.exit_left = 0,
                    (false, 0) => inst.exit_left = inst.grains.hop(),
                    _ => {}
                }
                if inst.exit_left > 0 {
                    inst.exit_left -= 1;
                    if inst.exit_left == 0 {
                        inst.spectral = false;
                    }
                }
                continue;
            }
            if !inst.granular {
                if wanted && use_spectral && i < spectral_avail {
                    // the shifter is hit at the head before the next frame
                    // renders (`resolve_hits`: a copy, a budgeted seek, or
                    // the grains)
                    inst.spectral = true;
                    inst.exit_left = 0;
                    inst.shift_hit = true;
                } else if wanted {
                    // seamless: the first grain reads where the head would
                    inst.granular = true;
                    inst.exit_left = 0;
                    inst.grains.reseed(inst.position, sign * inst.pp_dir);
                }
                continue;
            }
            let region = (b.rs as f64, b.re as f64);
            let spawn_dir = sign * inst.pp_dir;
            let nominal = inst.position;
            inst.grains.advance(
                spawn_dir,
                |pos, gdir| {
                    if travel(pos, *gdir, rate * ratio, &b, mode) {
                        *gdir = -*gdir;
                    }
                    if !b.looping {
                        // reads hold the region's edge
                        *pos = pos.clamp(region.0, region.1);
                    }
                },
                |outgoing, search| match mono {
                    // only two grains that read the same way line up
                    Some(m) if outgoing.dir == spawn_dir => {
                        let step = rate * ratio * outgoing.dir;
                        let found = align(nominal, outgoing.pos, step, search, |p| {
                            m(fold_pos(p, step, &b, mode))
                        });
                        found.clamp(region.0, region.1)
                    }
                    _ => nominal,
                },
            );
            match (wanted, inst.exit_left) {
                (true, _) => inst.exit_left = 0,
                (false, 0) => inst.exit_left = inst.grains.hop(),
                _ => {}
            }
            if inst.exit_left > 0 {
                inst.exit_left -= 1;
                if inst.exit_left == 0 {
                    inst.granular = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod resample_tests;
#[cfg(test)]
mod tests;
