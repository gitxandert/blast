//! Waves (WAVa): a voice's generated sound. Up to `MAX_WAVES` oscillators
//! (sine, tri, saw, square, noise) sum into the voice's source frame, one
//! phase per choir instance, so everything a voice has (gain, pan, effects,
//! lfos, envs, seqs) applies to them. Everything is fixed-size and `Copy`:
//! nothing here allocates or frees on the audio thread.
//!
//! The voice's `pitch` and `rate` multiply every wave's frequency: the voice
//! passes the product as the `ratio` of `Waves::render`.
//!
//! A table wave (WAVb) plays single cycles cut from a track or buffer
//! (`wave/table.rs`); the table arrives as an `Arc` from the command thread.
//!
//! Saw, square and tri are made band-limited by Elliptic BLEP (`engine/blep.rs`):
//! the naive waveform plus a correction for each of its steps and corners.

use crate::audio_processing::blast_rand::fast_seed;
use crate::audio_processing::engine::blep::{BlepState, BlepTable, QUIET};
use crate::audio_processing::engine::param::Param;
use crate::audio_processing::engine::voice::{MAX_CHOIR, ModOp, ModSlot};
use std::sync::Arc;

mod table;
pub use table::{
    MAX_TABLE_FRAMES, MAX_TABLE_SLICES, MIN_TABLE_FRAMES, WaveTable, fits, frames_ok, levels_for,
};

/// The most waves a voice holds; a wave's id is its slot.
pub const MAX_WAVES: usize = 8;

/// Parameters per wave (`WaveParam`).
pub const WAVE_PARAMS: usize = 8;

/// What a wave plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveShape {
    Sine,
    Tri,
    Saw,
    Square,
    Noise,
    /// Single cycles cut from a track or buffer (`from`).
    Table,
}

impl WaveShape {
    pub const ALL: [WaveShape; 6] = [
        WaveShape::Sine,
        WaveShape::Tri,
        WaveShape::Saw,
        WaveShape::Square,
        WaveShape::Noise,
        WaveShape::Table,
    ];

    /// The word the language uses.
    pub fn name(self) -> &'static str {
        match self {
            WaveShape::Sine => "sine",
            WaveShape::Tri => "tri",
            WaveShape::Saw => "saw",
            WaveShape::Square => "square",
            WaveShape::Noise => "noise",
            WaveShape::Table => "table",
        }
    }

    pub fn from_name(s: &str) -> Option<WaveShape> {
        Self::ALL.into_iter().find(|w| w.name() == s)
    }
}

/// A continuous wave parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveParam {
    /// In Hz; modulation moves it in octaves.
    Freq,
    Gain,
    /// Where a retriggered wave starts, 0..1.
    Phase,
    /// The square's pulse width.
    Pw,
    /// Where a table wave is among its cycles, 0..1.
    Pos,
    /// FM depth in radians: the modulator's output times this.
    Index,
    /// The frequency as a multiple of the voice's note (220 Hz times the
    /// voice's pitch ratio), used in ratio mode; modulation moves it in
    /// octaves.
    Ratio,
    /// Feedback in radians: the wave's own last two samples, averaged, push
    /// its phase.
    Fb,
}

impl WaveParam {
    pub const ALL: [WaveParam; WAVE_PARAMS] = [
        WaveParam::Freq,
        WaveParam::Gain,
        WaveParam::Phase,
        WaveParam::Pw,
        WaveParam::Pos,
        WaveParam::Index,
        WaveParam::Ratio,
        WaveParam::Fb,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The name the language uses.
    pub fn name(self) -> &'static str {
        match self {
            WaveParam::Freq => "freq",
            WaveParam::Gain => "gain",
            WaveParam::Phase => "phase",
            WaveParam::Pw => "pw",
            WaveParam::Pos => "pos",
            WaveParam::Index => "index",
            WaveParam::Ratio => "ratio",
            WaveParam::Fb => "fb",
        }
    }

    pub fn from_name(s: &str) -> Option<WaveParam> {
        Self::ALL.into_iter().find(|p| p.name() == s)
    }

    /// The parameter at `index`, if there is one.
    pub fn from_index(i: u8) -> Option<WaveParam> {
        Self::ALL.get(usize::from(i)).copied()
    }

    /// The value a new wave starts with.
    pub fn default_value(self) -> f32 {
        match self {
            WaveParam::Freq => 220.0,
            WaveParam::Gain => 1.0,
            WaveParam::Phase => 0.0,
            WaveParam::Pw => 0.5,
            WaveParam::Pos => 0.0,
            WaveParam::Index => 1.0,
            WaveParam::Ratio => 1.0,
            WaveParam::Fb => 0.0,
        }
    }

    /// The inclusive range base and effective values are clamped to.
    pub fn range(self) -> (f32, f32) {
        match self {
            WaveParam::Freq => (0.01, 20_000.0),
            WaveParam::Gain => (0.0, 8.0),
            WaveParam::Phase => (0.0, 1.0),
            WaveParam::Pw => (0.01, 0.99),
            WaveParam::Pos => (0.0, 1.0),
            WaveParam::Index => (0.0, 32.0),
            WaveParam::Ratio => (0.01, 64.0),
            WaveParam::Fb => (0.0, 4.0),
        }
    }
}

/// Every parameter's default.
pub fn wave_defaults() -> [f32; WAVE_PARAMS] {
    WaveParam::ALL.map(WaveParam::default_value)
}

// where a table wave reads its table: the cycle position and the mip level
// (with a fraction) for the frequency it plays
#[derive(Clone, Copy)]
struct TableRead<'a> {
    table: &'a WaveTable,
    slice_pos: f32,
    level: f32,
}

// the naive (aliasing) waveform at `phase`; a table's is the table's own
// (already band-limited, so only an onset or a jump needs the filter)
fn naive(shape: WaveShape, phase: f64, pw: f64, tab: Option<TableRead>) -> f64 {
    match shape {
        WaveShape::Table => tab.map_or(0.0, |t| {
            f64::from(t.table.sample(t.slice_pos, t.level, phase))
        }),
        WaveShape::Sine => (std::f64::consts::TAU * phase).sin(),
        WaveShape::Saw => 2.0 * phase - 1.0,
        WaveShape::Square => match phase < pw {
            true => 1.0,
            false => -1.0,
        },
        WaveShape::Tri => match phase < 0.5 {
            true => 4.0 * phase - 1.0,
            false => 3.0 - 4.0 * phase,
        },
        WaveShape::Noise => 0.0,
    }
}

// the naive waveform's slope per sample at `phase`, for the shapes that have
// corners (a retrigger or an onset starts from the slope it had, or from 0)
fn slope(shape: WaveShape, phase: f64, dt: f64) -> f64 {
    match shape {
        WaveShape::Saw => 2.0 * dt,
        WaveShape::Tri if phase < 0.5 => 4.0 * dt,
        WaveShape::Tri => -4.0 * dt,
        // a sine's kink is too small to alias, and the filter's answer to a
        // change of slope is a slow tail bigger than the kink: left out
        WaveShape::Sine | WaveShape::Square | WaveShape::Noise | WaveShape::Table => 0.0,
    }
}

// a frame no real one follows: nothing has sounded yet
const NEVER: u64 = u64::MAX - 1;

// how long, in seconds, an instance's filter rings before it counts as
// silent (a pole decays by e^-30 in that time: far below any sample)
const RING_SECONDS: f64 = 3.0;

/// One wave: shape, parameters with their glides, a phase per choir
/// instance, its own modulation bank (cleared each frame) and its bypass.
#[derive(Clone, Copy, Debug)]
pub struct Osc {
    shape: WaveShape,
    params: [Param; WAVE_PARAMS],
    mods: [ModSlot; WAVE_PARAMS],
    phase: [f64; MAX_CHOIR],
    // each instance's band-limiting filter, the samples since it last got
    // an event (it is left alone once it has rung out), the engine frame it
    // last sounded in (a wave that sounded in the frame before is
    // continuous; any other start is an onset from silence), and the phase
    // increment it last used
    blep: [BlepState; MAX_CHOIR],
    // the slope (per sample) each filter has been told of: the sum of its
    // order-2 events. The waveform's slope is compared with it every sample,
    // and the difference is an event, so a change of frequency or of ratio
    // is band-limited and `slope_term` never jumps
    slope_acc: [f64; MAX_CHOIR],
    idle: [u32; MAX_CHOIR],
    last: [u64; MAX_CHOIR],
    dt: [f64; MAX_CHOIR],
    // the phase modulation (cycles) each instance applied last sample: the
    // read phase moves by the change of it, so the filter's bookkeeping
    // (events, slope) sees the phase increment `dt_eff`, not `dt`
    pm_prev: [f64; MAX_CHOIR],
    // the instances whose modulator set changed: the next render takes the
    // modulation as it is (`pm_prev = pm`) instead of jumping the phase by it
    rebase: u8,
    // each instance's last two samples before `gain` (the feedback)
    fb_hist: [[f32; 2]; MAX_CHOIR],
    // the waves that phase-modulate this one (a bitmask over the slots of
    // the same voice, 0: none), whether this one is heard, and whether its
    // frequency is `ratio` times the voice's note instead of `freq`
    fm: u8,
    out: bool,
    ratio_mode: bool,
    // the instances a trigger restarted since they last rendered
    pending: u8,
    noise: u64,
    retrig: bool,
    bypass: bool,
}

impl Osc {
    fn new(shape: WaveShape, values: [f32; WAVE_PARAMS], retrig: bool) -> Self {
        let mut osc = Self {
            shape,
            params: [Param::new(0.0); WAVE_PARAMS],
            mods: [ModSlot::IDENTITY; WAVE_PARAMS],
            phase: [0.0; MAX_CHOIR],
            blep: [QUIET; MAX_CHOIR],
            slope_acc: [0.0; MAX_CHOIR],
            idle: [u32::MAX; MAX_CHOIR],
            last: [NEVER; MAX_CHOIR],
            dt: [0.0; MAX_CHOIR],
            pm_prev: [0.0; MAX_CHOIR],
            rebase: 0,
            fb_hist: [[0.0; 2]; MAX_CHOIR],
            fm: 0,
            out: true,
            ratio_mode: false,
            pending: 0,
            // the stream is never zero (xorshift would stay there)
            noise: fast_seed() | 1,
            retrig,
            bypass: false,
        };
        for p in WaveParam::ALL {
            let (lo, hi) = p.range();
            let v = values[p.index()];
            let v = if v.is_finite() { v.clamp(lo, hi) } else { lo };
            osc.params[p.index()] = Param::new(v);
        }
        osc.reset_phases();
        osc
    }

    // `(base + add) * mul`, clamped; the frequency moves in octaves
    fn effective(&self, p: WaveParam) -> f32 {
        let (lo, hi) = p.range();
        let (base, m) = (self.params[p.index()].base, self.mods[p.index()]);
        let v = match p {
            WaveParam::Freq | WaveParam::Ratio if m.add != 0.0 => base * m.add.exp2() * m.mul,
            WaveParam::Freq | WaveParam::Ratio => base * m.mul,
            _ => (base + m.add) * m.mul,
        };
        if v.is_finite() { v.clamp(lo, hi) } else { base }
    }

    // the cycle position a trigger starts at: `phase`, with 1 the same as 0
    // (a phase of exactly 1 would miss its own wrap)
    fn start_phase(&self) -> f64 {
        f64::from(self.params[WaveParam::Phase.index()].base).rem_euclid(1.0)
    }

    // one instance back to the wave's own `phase`, with a quiet filter (a
    // restart from silence: nothing to cancel)
    fn reset_phase(&mut self, inst: usize) {
        self.phase[inst] = self.start_phase();
        self.pending &= !(1 << inst);
        self.blep[inst] = QUIET;
        self.slope_acc[inst] = 0.0;
        self.idle[inst] = u32::MAX;
        self.last[inst] = NEVER;
        self.pm_prev[inst] = 0.0;
        self.fb_hist[inst] = [0.0; 2];
    }

    fn reset_phases(&mut self) {
        for i in 0..MAX_CHOIR {
            self.reset_phase(i);
        }
    }

    // a retrigger of a wave that is sounding: its waveform jumps to the
    // start, and the jump goes through the filter, so it doesn't click
    fn retrigger(&mut self, inst: usize, table: &BlepTable, wt: Option<&WaveTable>) {
        let new = self.start_phase();
        let old = self.phase[inst];
        if self.shape != WaveShape::Noise {
            let pw = f64::from(self.effective(WaveParam::Pw));
            let dt = self.dt[inst];
            let tab = self.table_read(wt, dt.abs());
            let jump = naive(self.shape, new, pw, tab) - naive(self.shape, old, pw, tab);
            let bend = slope(self.shape, new, dt) - slope(self.shape, old, dt);
            self.add(inst, table, jump, 1, 0.0);
            self.add(inst, table, bend, 2, 0.0);
        }
        self.phase[inst] = new;
        self.pm_prev[inst] = 0.0;
        self.fb_hist[inst] = [0.0; 2];
    }

    // the frequency in Hz before the voice's ratio: `freq`, or in ratio
    // mode 220 Hz times `ratio`, with `freq`'s modulation multiplying it
    // (octaves, then `mul`, as `effective` does)
    fn base_hz(&self) -> f64 {
        if !self.ratio_mode {
            return f64::from(self.effective(WaveParam::Freq));
        }
        let m = self.mods[WaveParam::Freq.index()];
        let octaves = if m.add != 0.0 { m.add.exp2() } else { 1.0 };
        let hz = 220.0 * f64::from(self.effective(WaveParam::Ratio)) * f64::from(octaves * m.mul);
        if hz.is_finite() { hz } else { 220.0 }
    }

    // the table to read and where, for a table wave at `dt` of a cycle per
    // sample (`None` for any other shape, or a table wave with no table yet)
    fn table_read<'a>(&self, wt: Option<&'a WaveTable>, dt: f64) -> Option<TableRead<'a>> {
        let table = wt.filter(|_| self.shape == WaveShape::Table)?;
        let pos = self.effective(WaveParam::Pos);
        Some(TableRead {
            table,
            slice_pos: pos * (table.slices() - 1) as f32,
            level: table.level_for(dt),
        })
    }

    // adds a discontinuity to one instance's filter
    fn add(&mut self, inst: usize, table: &BlepTable, amount: f64, order: usize, past: f64) {
        if amount != 0.0 {
            table.add(&mut self.blep[inst], amount, order, past);
            if order == 2 {
                self.slope_acc[inst] += amount;
            }
            self.idle[inst] = 0;
        }
    }

    fn noise_next(&mut self) -> f32 {
        let mut x = self.noise;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.noise = x;
        // 24 bits, in -1..1
        ((x >> 40) as f32) * (1.0 / 8_388_608.0) - 1.0
    }

    // one sample for one instance at `ratio` times the wave's frequency,
    // phase-modulated by `modulator` (its modulator's sample for this
    // instance, 0 when it has none), and the phase moves on
    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        inst: usize,
        ratio: f64,
        sr: f64,
        table: &BlepTable,
        wt: Option<&WaveTable>,
        now: u64,
        modulator: f32,
    ) -> f32 {
        // sounded in the frame before: the waveform is continuous
        let continuous = self.last[inst].wrapping_add(1) == now;
        if self.pending >> inst & 1 == 1 {
            self.pending &= !(1 << inst);
            match continuous {
                true => self.retrigger(inst, table, wt),
                // a hit on a silent instance: the phase restarts, and the
                // onset below does the rest
                false => self.reset_phase(inst),
            }
        }
        let freq = self.base_hz() * ratio;
        // a stopped read (rate 0) holds the phase silently
        if !freq.is_finite() || freq <= 0.0 {
            return 0.0;
        }
        let gain = self.effective(WaveParam::Gain);
        let dt = freq / sr;
        let phase = self.phase[inst];
        // phase modulation as a phase increment: the read phase moves by the
        // change of the modulation as well as by `dt`. Noise has no phase
        // an onset from silence starts the feedback over
        if self.last[inst].wrapping_add(1) != now {
            self.fb_hist[inst] = [0.0; 2];
        }
        let pm = if self.shape == WaveShape::Noise {
            0.0
        } else {
            let [y1, y2] = self.fb_hist[inst];
            let tau = std::f64::consts::TAU;
            let fb = f64::from(self.effective(WaveParam::Fb)) / tau * (f64::from(y1 + y2) * 0.5);
            f64::from(self.effective(WaveParam::Index)) / tau * f64::from(modulator) + fb
        };
        if self.rebase >> inst & 1 == 1 {
            self.rebase &= !(1 << inst);
            self.pm_prev[inst] = pm;
        }
        let dt_eff = dt + (pm - self.pm_prev[inst]);
        self.pm_prev[inst] = pm;
        self.dt[inst] = dt_eff;
        // anything but noise at or past Nyquist would only alias, and a
        // table wave with no table has nothing to play
        let no_table = self.shape == WaveShape::Table && wt.is_none();
        if (dt >= 0.5 && self.shape != WaveShape::Noise) || no_table {
            let end = phase + dt_eff;
            self.phase[inst] = end - end.floor();
            self.blep[inst] = QUIET;
            self.slope_acc[inst] = 0.0;
            self.idle[inst] = u32::MAX;
            self.last[inst] = NEVER;
            return 0.0;
        }
        let continuous = self.last[inst].wrapping_add(1) == now;
        self.last[inst] = now;
        if self.shape == WaveShape::Noise {
            self.phase[inst] = (phase + dt).fract();
            return self.noise_next() * gain;
        }
        let pw = f64::from(self.effective(WaveParam::Pw));
        let tab = self.table_read(wt, dt_eff.abs());
        if !continuous {
            // an onset from silence: the signal steps from 0 to its value
            // and slope here, and that goes through the filter (a stop is a
            // hard cut: nothing of the old filter state outlives it)
            self.blep[inst] = QUIET;
            self.slope_acc[inst] = 0.0;
            self.add(inst, table, naive(self.shape, phase, pw, tab), 1, 0.0);
            self.add(inst, table, slope(self.shape, phase, dt_eff), 2, 0.0);
        }
        // a sample whose read phase moves half a cycle or more (FM past
        // Nyquist aliases by nature) adds no events and no slope change
        let events = dt_eff.abs() < 0.5;
        // the slope the filter hasn't been told of yet (the frequency moved)
        let now_slope = slope(self.shape, phase, dt_eff);
        let diff = now_slope - self.slope_acc[inst];
        if events && diff.abs() > 1e-12 * (1.0 + now_slope.abs()) {
            self.add(inst, table, diff, 2, 0.0);
        }
        let ringing = f64::from(self.idle[inst]) < RING_SECONDS * sr;
        let mut out = naive(self.shape, phase, pw, tab);
        if ringing {
            out += table.get(&self.blep[inst]);
            table.step(&mut self.blep[inst]);
            self.idle[inst] = self.idle[inst].saturating_add(1);
        } else {
            self.blep[inst] = QUIET;
        }
        // the filter's delay of the waveform's slope (the part of the answer
        // to a change of slope that doesn't die away)
        out += table.slope_term(self.slope_acc[inst]);
        // the steps and corners of the naive waveform crossed between this
        // sample and the next, `past` samples before the next one. Forward
        // (`dt_eff > 0`) the phase passes `at` going up; backward it passes
        // it going down, and the amount is negated (the steps and corners
        // are met in the other order). Each shape's points are listed for the
        // cycles around `0..1`, since the phase may be in the next or the
        // previous one by the end of the sample
        let end = phase + dt_eff;
        if events && dt_eff != 0.0 {
            let event = |me: &mut Self, at: f64, amount: f64, order: usize| {
                if dt_eff > 0.0 && phase < at && at <= end {
                    me.add(inst, table, amount, order, (end - at) / dt_eff);
                } else if dt_eff < 0.0 && end < at && at <= phase {
                    me.add(inst, table, -amount, order, (at - end) / -dt_eff);
                }
            };
            match self.shape {
                WaveShape::Saw => {
                    event(self, 0.0, -2.0, 1);
                    event(self, 1.0, -2.0, 1);
                }
                WaveShape::Square => {
                    for k in [-1.0, 0.0, 1.0] {
                        event(self, pw + k, -2.0, 1);
                    }
                    event(self, 0.0, 2.0, 1);
                    event(self, 1.0, 2.0, 1);
                }
                WaveShape::Tri => {
                    let bend = 8.0 * dt_eff;
                    for k in [-1.0, 0.0, 1.0] {
                        event(self, 0.5 + k, -bend, 2);
                        event(self, 1.0 + k, bend, 2);
                    }
                }
                WaveShape::Sine | WaveShape::Noise | WaveShape::Table => {}
            }
        }
        self.phase[inst] = end - end.floor();
        self.fb_hist[inst] = [out as f32, self.fb_hist[inst][0]];
        out as f32 * gain
    }
}

/// A voice's waves, by id (the slot). Fixed size.
#[derive(Clone, Debug)]
pub struct Waves {
    osc: [Option<Osc>; MAX_WAVES],
    // each table wave's table, by the same slot; a table is shared with the
    // command thread's `Arc` and leaves through the garbage queue
    tables: [Option<Arc<WaveTable>>; MAX_WAVES],
    // the slots in evaluation order, modulators before the waves they
    // modulate (`count` of them are used); kept up to date by `reorder`
    order: [u8; MAX_WAVES],
    count: u8,
    // whether any glide has frames left, or a mod was written since the last
    // clear; keeps the per-frame work off idle waves
    ramping: bool,
    mods_dirty: bool,
    // the engine frame being rendered (see `set_now`)
    now: u64,
}

impl Waves {
    pub const fn new() -> Self {
        Self {
            osc: [None; MAX_WAVES],
            tables: [const { None }; MAX_WAVES],
            order: [0; MAX_WAVES],
            count: 0,
            ramping: false,
            mods_dirty: false,
            now: 0,
        }
    }

    /// The engine's frame number, set before the frame's `render`s: a wave
    /// that sounded in the frame before is continuous, any other starts as an
    /// onset from silence (a voice start, a hit on a silent instance, a stop
    /// and start, a choir delay ending, a return from Nyquist).
    pub fn set_now(&mut self, frame: u64) {
        self.now = frame;
    }

    /// True when the voice has no wave.
    pub fn is_empty(&self) -> bool {
        self.osc.iter().all(Option::is_none)
    }

    /// Whether wave `id` is there.
    pub fn has(&self, id: u8) -> bool {
        self.osc.get(usize::from(id)).is_some_and(Option::is_some)
    }

    // a command can name a wave that is gone: a line that waited for its
    // beat may have been overtaken by a `remove`, so a missing wave is
    // ignored, not a mirror bug
    fn slot(&mut self, id: u8) -> Option<&mut Osc> {
        self.osc.get_mut(usize::from(id))?.as_mut()
    }

    /// Makes wave `id` (a taken slot or an id past `MAX_WAVES` is a mirror
    /// bug: ignored). Parameters are clamped.
    pub fn insert(&mut self, id: u8, shape: WaveShape, params: [f32; WAVE_PARAMS], retrig: bool) {
        match self.osc.get_mut(usize::from(id)) {
            Some(slot @ None) => *slot = Some(Osc::new(shape, params, retrig)),
            _ => debug_assert!(false, "wave {id} is taken or out of range"),
        }
        self.reorder();
    }

    // evaluation order by Kahn's algorithm over at most `MAX_WAVES` nodes
    // (fixed arrays, no allocation): a wave comes after the one it follows
    // with `fm`. The mirror refuses cycles; one that got here anyway has its
    // waves put last in slot order, so nothing hangs
    fn reorder(&mut self) {
        let mut placed = [false; MAX_WAVES];
        let mut n = 0;
        loop {
            let mut progress = false;
            for id in 0..MAX_WAVES {
                let Some(osc) = &self.osc[id] else {
                    placed[id] = true;
                    continue;
                };
                // a modulator that isn't there doesn't hold a wave back
                let ready =
                    (0..MAX_WAVES).all(|m| osc.fm >> m & 1 == 0 || placed[m] || !self.has(m as u8));
                if !placed[id] && ready {
                    placed[id] = true;
                    self.order[n] = id as u8;
                    n += 1;
                    progress = true;
                }
            }
            if !progress {
                break;
            }
        }
        for (id, done) in placed.iter().enumerate() {
            if !done {
                self.order[n] = id as u8;
                n += 1;
            }
        }
        self.count = n as u8;
    }

    /// Makes wave `id` follow the waves in the bitmask `fm` (0: none). A
    /// set with a wave that is not there, or the wave itself, is ignored (the
    /// mirror checks both). The carrier's modulation starts from none.
    pub fn set_fm(&mut self, id: u8, fm: u8) {
        let present = (0..MAX_WAVES).fold(0u8, |m, i| m | u8::from(self.has(i as u8)) << i);
        if fm & !present != 0 || fm >> id & 1 == 1 {
            return;
        }
        // a line that waited can close a loop the mirror saw as open: refuse
        // it, leaving the links as they were (the mirror then agrees). Walk
        // the modulators of the set, and theirs, each wave once
        let (mut seen, mut todo) = (0u8, fm);
        while todo & !seen != 0 {
            let m = (todo & !seen).trailing_zeros() as usize;
            if m == usize::from(id) {
                return;
            }
            seen |= 1 << m;
            todo |= self.osc[m].as_ref().map_or(0, |o| o.fm);
        }
        if let Some(osc) = self.slot(id) {
            osc.fm = fm;
            osc.rebase = u8::MAX;
        }
        self.reorder();
    }

    /// The waves that modulate wave `id`, as a bitmask (0: none).
    pub fn modulators_of(&self, id: u8) -> u8 {
        self.osc
            .get(usize::from(id))
            .and_then(Option::as_ref)
            .map_or(0, |o| o.fm)
    }

    /// Whether wave `id` is heard (`false`: a modulator only).
    pub fn set_out(&mut self, id: u8, out: bool) {
        if let Some(osc) = self.slot(id) {
            osc.out = out;
        }
    }

    /// Whether wave `id`'s frequency is `ratio` times the voice's note
    /// (`true`) or its own `freq`.
    pub fn set_ratio_mode(&mut self, id: u8, on: bool) {
        if let Some(osc) = self.slot(id) {
            osc.ratio_mode = on;
        }
    }

    /// Sets a parameter (clamped); non-finite values are ignored. With
    /// `frames > 0` it glides there. Returns the frames the glide takes
    /// (0 if it jumped).
    pub fn set(&mut self, id: u8, param: u8, value: f32, frames: u32) -> u32 {
        let Some(p) = WaveParam::from_index(param) else {
            return 0;
        };
        if !value.is_finite() {
            return 0;
        }
        let Some(osc) = self.slot(id) else {
            return 0;
        };
        let (lo, hi) = p.range();
        let used = osc.params[p.index()].set(value.clamp(lo, hi), frames);
        self.ramping |= used > 0;
        used
    }

    /// The generation of one parameter of wave `id` (it changes whenever the
    /// parameter is set or starts a glide); `None` if there is no such wave.
    pub fn generation(&self, id: u8, param: u8) -> Option<u32> {
        let osc = self.osc.get(usize::from(id))?.as_ref()?;
        osc.params.get(usize::from(param)).map(|p| p.generation)
    }

    /// The base value of one parameter of wave `id` (no modulation).
    pub fn base(&self, id: u8, param: u8) -> Option<f32> {
        let osc = self.osc.get(usize::from(id))?.as_ref()?;
        osc.params.get(usize::from(param)).map(|p| p.base)
    }

    /// Changes what wave `id` plays.
    pub fn set_shape(&mut self, id: u8, shape: WaveShape) {
        if let Some(osc) = self.slot(id) {
            // a hard switch: the filter starts clean
            osc.shape = shape;
            osc.blep = [QUIET; MAX_CHOIR];
            osc.slope_acc = [0.0; MAX_CHOIR];
            osc.idle = [u32::MAX; MAX_CHOIR];
        }
    }

    /// Whether wave `id` restarts its phase on every trigger.
    pub fn set_retrig(&mut self, id: u8, retrig: bool) {
        if let Some(osc) = self.slot(id) {
            osc.retrig = retrig;
        }
    }

    /// Silences a wave or brings it back; `clear` also puts its phase back
    /// (`stop` and `start`; `pause` and `resume` hold it).
    pub fn bypass(&mut self, id: u8, on: bool, clear: bool) {
        if let Some(osc) = self.slot(id) {
            osc.bypass = on;
            if clear {
                osc.reset_phases();
            }
        }
    }

    /// Gives wave `id` its table and returns the one it had, for the garbage
    /// queue. `Err` hands the table back when there is no such wave (a line
    /// that waited for its beat was overtaken by a `remove`).
    pub fn set_table(
        &mut self,
        id: u8,
        table: Arc<WaveTable>,
    ) -> Result<Option<Arc<WaveTable>>, Arc<WaveTable>> {
        if !self.has(id) {
            return Err(table);
        }
        Ok(self.tables[usize::from(id)].replace(table))
    }

    /// Whether wave `id` has a table.
    pub fn has_table(&self, id: u8) -> bool {
        self.tables
            .get(usize::from(id))
            .is_some_and(Option::is_some)
    }

    /// Takes a wave out, and returns its table (if it had one) for the
    /// garbage queue. Never allocates.
    pub fn remove(&mut self, id: u8) -> Option<Arc<WaveTable>> {
        let slot = self.osc.get_mut(usize::from(id))?;
        *slot = None;
        // whatever it modulated plays plain again
        for osc in self.osc.iter_mut().flatten() {
            if osc.fm >> id & 1 == 1 {
                osc.fm &= !(1 << id);
                osc.rebase = u8::MAX;
            }
        }
        self.reorder();
        self.tables[usize::from(id)].take()
    }

    /// Adds this frame's modulation to one parameter of one wave; the next
    /// `advance_ramps` clears it. An unknown wave or parameter is ignored
    /// (a wave can be removed while an LFO still names it).
    pub fn modulate(&mut self, id: u8, param: u8, op: ModOp) {
        let Some(p) = WaveParam::from_index(param) else {
            return;
        };
        let Some(osc) = self.slot(id) else {
            return;
        };
        match op {
            ModOp::Add(x) => osc.mods[p.index()].add += x,
            ModOp::Mul(x) => osc.mods[p.index()].mul *= x,
        }
        self.mods_dirty = true;
    }

    /// A trigger of choir instance `inst`: every wave that retriggers starts
    /// that instance over.
    pub fn hit(&mut self, inst: usize) {
        if inst >= MAX_CHOIR {
            return;
        }
        for osc in self.osc.iter_mut().flatten() {
            if osc.retrig {
                // settled by the next `render`, which has the filter table
                osc.pending |= 1 << inst;
            }
        }
    }

    /// Every instance of every wave back to its `phase` (a voice that starts
    /// over).
    pub fn reset_phases(&mut self) {
        for osc in self.osc.iter_mut().flatten() {
            osc.reset_phases();
        }
    }

    /// Moves every glide one frame and clears the frame's modulation.
    pub fn advance_ramps(&mut self) {
        if self.ramping {
            let mut live = false;
            for osc in self.osc.iter_mut().flatten() {
                for p in &mut osc.params {
                    live |= p.advance();
                }
            }
            self.ramping = live;
        }
        if self.mods_dirty {
            for osc in self.osc.iter_mut().flatten() {
                osc.mods = [ModSlot::IDENTITY; WAVE_PARAMS];
            }
            self.mods_dirty = false;
        }
    }

    /// One sample of every sounding wave for choir instance `inst`, summed;
    /// `ratio` multiplies every frequency (the voice's pitch, rate and the
    /// instance's detune). Moves the phases.
    pub fn render(&mut self, inst: usize, ratio: f64, sr: f64, table: &BlepTable) -> f32 {
        if inst >= MAX_CHOIR {
            return 0.0;
        }
        let mut sum = 0.0;
        // each wave's sample, for the waves that follow it
        let mut heard = [0.0f32; MAX_WAVES];
        for &id in &self.order[..usize::from(self.count)] {
            let id = usize::from(id);
            let Some(osc) = &mut self.osc[id] else {
                continue;
            };
            if osc.bypass {
                continue;
            }
            let modulator: f32 = (0..MAX_WAVES)
                .filter(|&m| osc.fm >> m & 1 == 1)
                .map(|m| heard[m])
                .sum();
            let s = osc.render(
                inst,
                ratio,
                sr,
                table,
                self.tables[id].as_deref(),
                self.now,
                modulator,
            );
            heard[id] = s;
            if osc.out {
                sum += s;
            }
        }
        sum
    }
}

impl Default for Waves {
    fn default() -> Self {
        Self::new()
    }
}

/// A table at 48 kHz, built once for the tests.
#[cfg(test)]
pub(crate) fn test_table() -> &'static BlepTable {
    static TABLE: std::sync::OnceLock<Box<BlepTable>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| BlepTable::new(48_000.0))
}

#[cfg(test)]
mod tests;
