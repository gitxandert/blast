//! Voice effects (FXa): a chain of named inserts that process a voice's
//! output frame, in the order they were written. The chain is a `Vec` the
//! command thread grows (an insert carries a bigger empty `Vec` when the
//! chain is full, and the old one leaves as garbage), so nothing here
//! allocates or frees on the audio thread.
//!
//! The effects themselves live in `kinds/`, one file each (FXR); the chain
//! only knows the registry.

pub mod kinds;

pub use kinds::{FxMem, FxType, defaults, from_word, own_settings, param_of, spec};

use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::engine::param::Param;
use crate::audio_processing::engine::voice::{MAX_VOICE_CHANNELS, ModOp, ModSlot};

/// Parameters per effect: the most settings an effect has besides `type`
/// (its own, then mix and gain).
pub const FX_PARAMS: usize = 8;

/// What an effect is: its registry entry, and the `type=` word it runs as
/// (an index into `spec(ty).types`; 0 when the effect has none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FxKind {
    pub ty: FxType,
    pub variant: u8,
}

impl FxKind {
    /// The effect with its default `type=`.
    pub const fn new(ty: FxType) -> Self {
        Self { ty, variant: 0 }
    }
}

/// One effect: kind, parameters with their glides, its memory, its own
/// modulation bank (cleared each frame), and its bypass state.
#[derive(Clone, Copy, Debug)]
pub struct FxSlot {
    id: u16,
    kind: FxKind,
    params: [Param; FX_PARAMS],
    mem: FxMem,
    mods: [ModSlot; FX_PARAMS],
    bypass: bool,
    // the effect's own settings (the first `n` parameters), then mix and gain
    n: u8,
    mix: u8,
    gain: u8,
    // each parameter's clamp range, and (as bits) which ones move in octaves
    ranges: [(f32, f32); FX_PARAMS],
    octaves: u8,
}

impl FxSlot {
    fn new(id: u16, kind: FxKind, values: [f32; FX_PARAMS]) -> Self {
        let own = own_settings(kind.ty);
        let n = own.len() - 2;
        let mut ranges = [(0.0, 1.0); FX_PARAMS];
        let mut octaves = 0u8;
        for (i, s) in own.iter().enumerate() {
            ranges[i] = kinds::range_of(s);
            if spec(kind.ty).octaves.contains(&s.name) {
                octaves |= 1 << i;
            }
        }
        let mut slot = Self {
            id,
            kind,
            params: [Param::new(0.0); FX_PARAMS],
            mem: FxMem::new(kind.ty),
            mods: [ModSlot::IDENTITY; FX_PARAMS],
            bypass: false,
            n: n as u8,
            mix: n as u8,
            gain: n as u8 + 1,
            ranges,
            octaves,
        };
        slot.load(values);
        slot
    }

    // every parameter at once, clamped
    fn load(&mut self, values: [f32; FX_PARAMS]) {
        for (i, (p, v)) in self.params.iter_mut().zip(values).enumerate() {
            let (lo, hi) = self.ranges[i];
            let v = if v.is_finite() { v.clamp(lo, hi) } else { lo };
            let generation = p.generation.wrapping_add(1);
            *p = Param::new(v);
            p.generation = generation;
        }
    }

    // `(base + add) * mul`, clamped; an octave setting moves in octaves
    fn effective(&self, i: usize) -> f32 {
        let (lo, hi) = self.ranges[i];
        let (base, m) = (self.params[i].base, self.mods[i]);
        let v = match (self.octaves >> i) & 1 {
            1 => base * m.add.exp2() * m.mul,
            _ => (base + m.add) * m.mul,
        };
        if v.is_finite() { v.clamp(lo, hi) } else { base }
    }

    fn clear_state(&mut self) {
        self.mem = FxMem::new(self.kind.ty);
    }

    /// Runs the effect over one frame in place: the effect makes the wet
    /// frame, the chain blends it with the dry one and applies the gain.
    fn run(&mut self, frame: &mut [f32], sr: f32) {
        let mix = self.effective(usize::from(self.mix));
        let gain = self.effective(usize::from(self.gain));
        let mut dry = [0.0; MAX_VOICE_CHANNELS];
        let len = frame.len().min(MAX_VOICE_CHANNELS);
        dry[..len].copy_from_slice(&frame[..len]);
        let mut p = [0.0; FX_PARAMS];
        let n = usize::from(self.n);
        for (i, v) in p[..n].iter_mut().enumerate() {
            *v = self.effective(i);
        }
        self.mem.run(self.kind.variant, &p[..n], frame, sr);
        for (s, d) in frame.iter_mut().zip(dry) {
            *s = gain * ((1.0 - mix) * d + mix * *s);
        }
    }
}

/// The chain, in written order. Grown only by `insert` with a `room`.
pub struct FxChain {
    fx: Vec<FxSlot>,
    // whether any ramp has frames left, or a mod was written since the last
    // clear; keeps the per-frame work off an idle chain
    ramping: bool,
    mods_dirty: bool,
}

impl FxChain {
    /// An empty chain; allocates nothing.
    pub const fn new() -> Self {
        Self {
            fx: Vec::new(),
            ramping: false,
            mods_dirty: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.fx.is_empty()
    }

    /// How many effects the chain holds.
    pub fn len(&self) -> usize {
        self.fx.len()
    }

    /// The `Vec`'s capacity (the mirror counts the same room).
    pub fn capacity(&self) -> usize {
        self.fx.capacity()
    }

    // an edit can name an effect that is gone: a line that waited for its
    // beat (`at`, `after`) may have been overtaken by a `remove`, so a
    // missing effect is ignored, not a mirror bug
    fn slot(&mut self, id: u16) -> Option<&mut FxSlot> {
        self.fx.iter_mut().find(|s| s.id == id)
    }

    /// Appends an effect. With `room`, the chain first moves into it (it must
    /// hold one more than the chain does) and the old `Vec` comes back, to
    /// leave as garbage. A chain that is full without a `room`, or an `id`
    /// already there, is a mirror bug: the effect is dropped, since pushing
    /// would allocate.
    pub fn insert(
        &mut self,
        id: u16,
        kind: FxKind,
        params: [f32; FX_PARAMS],
        room: Option<Vec<FxSlot>>,
    ) -> Option<Vec<FxSlot>> {
        let mut old = None;
        if let Some(mut room) = room {
            if room.capacity() > self.fx.len() {
                // it fits, so this moves the slots without allocating
                room.append(&mut self.fx);
                old = Some(std::mem::replace(&mut self.fx, room));
            } else {
                debug_assert!(false, "room for {} holds no more", self.fx.len());
                old = Some(room);
            }
        }
        if self.fx.iter().any(|s| s.id == id) {
            debug_assert!(false, "effect {id} is taken");
        } else if self.fx.len() < self.fx.capacity() {
            self.fx.push(FxSlot::new(id, kind, params));
        } else {
            debug_assert!(false, "the effect chain is full");
        }
        // an empty Vec with no buffer frees nothing
        old.filter(|v| v.capacity() > 0)
    }

    /// Sets a parameter (clamped); non-finite values are ignored. With
    /// `frames > 0` it glides there in a straight line from where it is,
    /// replacing any glide in progress. Returns the frames the glide takes
    /// (0 if it jumped).
    pub fn set(&mut self, id: u16, param: u8, value: f32, frames: u32) -> u32 {
        let i = usize::from(param);
        if i >= FX_PARAMS || !value.is_finite() {
            return 0;
        }
        let Some(slot) = self.slot(id) else {
            return 0;
        };
        if i >= usize::from(slot.n) + 2 {
            return 0;
        }
        let (lo, hi) = slot.ranges[i];
        let v = value.clamp(lo, hi);
        let frames = slot.params[i].set(v, frames);
        self.ramping |= frames > 0;
        frames
    }

    /// The generation of one parameter of effect `id`: it changes whenever
    /// the parameter is set or starts a glide. `None` if there is no such
    /// effect.
    pub fn generation(&self, id: u16, param: u8) -> Option<u32> {
        let slot = self.fx.iter().find(|s| s.id == id)?;
        slot.params.get(usize::from(param)).map(|p| p.generation)
    }

    /// Switches a filter to another type: the effect keeps its place and
    /// takes `params`, and its memory is cleared.
    pub fn retype(&mut self, id: u16, kind: FxKind, params: [f32; FX_PARAMS]) {
        let Some(slot) = self.slot(id) else {
            return;
        };
        // only the `type=` word changes; the effect stays what it is
        slot.kind.variant = kind.variant;
        slot.load(params);
        slot.clear_state();
    }

    /// Whether the chain holds effect `id`.
    pub fn has(&self, id: u16) -> bool {
        self.fx.iter().any(|s| s.id == id)
    }

    /// Takes an effect out; the ones after it move up. Never allocates.
    pub fn remove(&mut self, id: u16) {
        if let Some(pos) = self.fx.iter().position(|s| s.id == id) {
            self.fx.remove(pos);
        }
    }

    /// Bypasses an effect or brings it back; `clear` also clears its memory
    /// (`stop` and `start`; `pause` and `resume` keep it).
    pub fn bypass(&mut self, id: u16, on: bool, clear: bool) {
        let Some(slot) = self.slot(id) else {
            return;
        };
        slot.bypass = on;
        if clear {
            slot.clear_state();
        }
    }

    /// Adds this frame's modulation to one parameter of one effect;
    /// `process` clears it. An unknown effect or parameter is ignored (an
    /// effect can be removed while an LFO still names it).
    pub fn modulate(&mut self, id: u16, param: u8, op: ModOp) {
        let i = usize::from(param);
        if i >= FX_PARAMS {
            return;
        }
        let Some(slot) = self.slot(id) else {
            return;
        };
        if i >= usize::from(slot.n) + 2 {
            return;
        }
        match op {
            ModOp::Add(x) => slot.mods[i].add += x,
            ModOp::Mul(x) => slot.mods[i].mul *= x,
        }
        self.mods_dirty = true;
    }

    /// Clears every effect's memory (a voice that starts over).
    pub fn reset(&mut self) {
        for slot in &mut self.fx {
            slot.clear_state();
        }
    }

    /// Moves every glide one frame.
    pub fn advance(&mut self) {
        if !self.ramping {
            return;
        }
        let mut live = false;
        for slot in &mut self.fx {
            for p in &mut slot.params {
                live |= p.advance();
            }
        }
        self.ramping = live;
    }

    /// Runs the chain over one interleaved frame in place, then clears the
    /// modulation. A bypassed effect is skipped. Channels past
    /// `MAX_VOICE_CHANNELS` pass through unprocessed.
    pub fn process(&mut self, frame: &mut [f32]) {
        let sr = match sample_rate::get() {
            0 => 48_000.0,
            n => n as f32,
        };
        let n = frame.len().min(MAX_VOICE_CHANNELS);
        for slot in &mut self.fx {
            if !slot.bypass {
                slot.run(&mut frame[..n], sr);
            }
        }
        if self.mods_dirty {
            for slot in &mut self.fx {
                slot.mods = [ModSlot::IDENTITY; FX_PARAMS];
            }
            self.mods_dirty = false;
        }
    }
}

impl Default for FxChain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
