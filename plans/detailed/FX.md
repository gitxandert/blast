# FX: per-voice insert effects
Status: draft   ·   Changelog at the bottom

## Summary
Afterwards every voice carries a fixed chain of 4 insert slots, processed in
order on the voice's source frame, before gain and pan. Each slot can be off,
or one of:
- a biquad filter (LP/HP/BP/notch, RBJ cookbook)
- a tanh waveshaping drive
- a bitcrusher/decimator

Each slot has a dry/wet mix. Filter cutoff and Q pass through a one-pole
smoother, so modulation doesn't zipper. Every slot parameter can be modulated
by LFO/ENV through VOX's modulation bank (`ModTarget::Fx`). Everything is
preallocated inside the voice. `fx -g <group>` applies the same settings to
every member voice. A group bus is out of scope.

## Ownership
- **Creates/fills:** `engine/fx/mod.rs` (`FxChain`, `FxSlot`, `FxParam`),
  `engine/fx/biquad.rs`, `engine/fx/shape.rs` (drive and crush),
  `engine/fx/smooth.rs` (one-pole), `commands/fx.rs` and
  `engine/handlers/fx.rs`.
- **Modifies:** `engine/voice.rs`, **FX hook only**:
  - the `fx: FxChain` field on `Voice`
  - its initialisation in `Voice::new`
  - the call at the `// FX hook (wave 4)` marker that VOXa leaves in
    `render_frame`
  - a `reset` call in `Voice::stop` (completed stop only)
  Nothing else in the file changes.
- **Reads only:** `engine/voice.rs` (the rest of it), `commands/voice.rs`
  (`FX_SLOTS`, `FX_PARAM_NAMES`, voice lookup), `commands/reprs.rs` (group
  members, for `fx -g`), `blast_time.rs`.

## Registry entries
Wave-4 scaffold:
1. `engine/mod.rs`: `pub mod fx;`. `engine/fx/mod.rs` is stubbed empty.
   `engine/handlers/mod.rs`: `pub mod fx;`, with a no-op
   `pub fn apply(c: &mut Conductor, a: FxArgs) {}` stub.
2. `commands/mod.rs`: `pub mod fx;`, the variant `Fx(FxArgs)`, and the parse
   arm `"fx" => fx::try_fx(self, args)`. `commands/fx.rs` is stubbed with
   `pub struct FxArgs { pub voice: VoiceId, pub slot: u8, pub op: FxOp }`, a
   `pub enum FxOp {}` placeholder, a `try_fx` that returns "unknown command",
   and an empty `HELP`.
3. `engine/mod.rs` apply arm: `Command::Fx(a) => handlers::fx::apply(self, a)`.
4. The INS help index: a row for `fx::HELP`.

## Architecture changes
None. FX parameters reach modulators through VOX's `ModTarget::Fx { slot, param }`,
whose names VOX fixes in `FX_PARAM_NAMES` (REQ-018). `FxParam` here matches
them. That means LFO and ENV, which run in parallel in wave 4, parse
`fx0.cutoff` without depending on this task.

## Interfaces
**Provides:**
```rust
// engine/fx/mod.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxParam { Cutoff = 0, Q = 1, Drive = 2, Bits = 3, Down = 4, Mix = 5 }   // name() == FX_PARAM_NAMES[i]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterType { LowPass, HighPass, BandPass, Notch }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxKind { Off, Filter(FilterType), Drive, Crush }
pub struct FxChain { slots: [FxSlot; FX_SLOTS], sr: f32 }
impl FxChain {
    pub fn new(sample_rate: u32) -> Self;               // no heap
    /// Runs the active slots over one source frame (src.len() = voice channels ≤ MAX_VOICE_CHANNELS).
    pub fn process(&mut self, src: &mut [f32], mods: &VoiceState);   // reads fx_mods(slot)
    pub fn apply(&mut self, slot: usize, op: FxOp);
    pub fn reset(&mut self);                            // clears filter/hold state, keeps settings
}
// commands/fx.rs
pub enum FxOp { Set { kind: FxKind, params: [Option<f32>; 6] }, Param(FxParam, f32), Off }
```
`FxSlot { kind, base: [f32; 6], smooth_cutoff: OnePole, smooth_q: OnePole, coefs: Biquad, last_fc: f32, last_q: f32, ch: [ChanState; MAX_VOICE_CHANNELS], dec_phase: f32 }`
with `ChanState { z1: f64, z2: f64, hold: f32 }`.

| Param | Unit | Default | Range | Used by |
|---|---|---|---|---|
| cutoff | Hz | 1000 | 20 ..= 0.45·sr | filter |
| q | - | 0.707 | 0.1 ..= 30 | filter |
| drive | dB | 12 | 0 ..= 48 | drive |
| bits | bits | 8 | 1 ..= 24 | crush |
| down | factor | 1 | 1 ..= 64 | crush |
| mix | 0..1 | 1 | 0 ..= 1 | all |

The effective value is `(base + add)·mul` from `fx_mods(slot)[i]`, clamped
as above. Modulating a parameter the slot's kind doesn't use has no effect.

**Consumes:** VOXa's `render_frame` hook marker, `VoiceState::fx_mods`,
`FX_SLOTS`, `FX_PARAM_NAMES` and `MAX_VOICE_CHANNELS`; E1's voice and group
lookup in the mirror.

## Steps
1. **`engine/fx/smooth.rs`**: `OnePole { y, a }` with
   `a = 1 - exp(-1 / (0.005·sr))` (5 ms), `next(x) = y += a·(x - y)`, and
   `snap(x)` (used when a kind is set, so a new filter doesn't sweep from the
   old cutoff).
2. **`engine/fx/biquad.rs`**: RBJ cookbook coefficients, computed in f64.
   `w0 = 2π·fc/sr`, `α = sin w0 / (2Q)`, then:
   - LP: `b = [(1-c)/2, 1-c, (1-c)/2]`
   - HP: `b = [(1+c)/2, -(1+c), (1+c)/2]`
   - BP (constant 0 dB peak): `b = [α, 0, -α]`
   - Notch: `b = [1, -2c, 1]`
   - all of them: `a = [1+α, -2c, 1-α]`, normalised by `a0`.
   Filter as transposed direct form II with f64 state per channel (f32 state
   is noisy at low cutoffs).
3. **`engine/fx/shape.rs`**:
   - drive: `g = 10^(dB/20)`, `y = tanh(g·x) / tanh(g)`. Unity at |x| = 1,
     bounded by 1 for |x| ≤ 1.
   - crush: `q = 2^(bits-1)`, `y = round(x·q)/q`. Fractional bits are allowed;
     the step size is continuous, so the parameter can be modulated smoothly.
   - decimate: `dec_phase += 1`; when `dec_phase ≥ down`, subtract `down` and
     latch `hold = x` per channel. Output `hold`. Quantise first, then hold.
4. **`engine/fx/mod.rs`: `FxChain::process`.** For each slot that isn't `Off`:
   - Compute the effective params.
   - Filter: smooth the cutoff and Q, and recompute coefficients only if
     either moved by more than 1e-4 (relative).
   - Run the kind on each channel `src[c]`, then mix:
     `src[c] = dry + mix·(wet - dry)`.
   - `apply(Set)` sets the kind and any given params, resets the slot's state,
     and snaps the smoothers. `apply(Param)` sets the base only.
     `apply(Off)` turns the slot off and resets it.
   - `FxParam::name()` and `from_name()` are built from `FX_PARAM_NAMES`.
5. **The voice hook** (`engine/voice.rs`, the hook region only):
   - Add the `fx` field.
   - `Voice::new` calls `FxChain::new(sample_rate::get())`.
   - At the marker: `self.fx.process(&mut src[..nch], &self.state);`.
   - Call `self.fx.reset()` where VOXb completes a stop (filter tails don't
     leak into the next start).
   - Early return when every slot is `Off`, so voices without FX cost one
     branch.
6. **`engine/handlers/fx.rs`**: look up the voice (`let … else return`), then
   `voice.fx.apply(slot, op)`.
7. **`commands/fx.rs`: `try_fx`.** Syntax:
   ```
   fx <v> <slot> filter lp|hp|bp|notch [<cutoff>] [<q>]
   fx <v> <slot> drive [<dB>]
   fx <v> <slot> crush [<bits>] [<down>]
   fx <v> <slot> off
   fx <v> <slot> <param> <value>          param ∈ cutoff|q|drive|bits|down|mix
   fx -g <group> <slot> …                 the same, for every member voice (Command::Batch)
   ```
   - `<slot>` must be in `0..FX_SLOTS`.
   - Cutoff accepts a `k` suffix (`1.2k`).
   - Every value is range-checked here, with an error out of range.
   - `fx -g` reads the group's members from the mirror and errors on an empty
     group.
   - The mirror is never modified.
   - `HELP` rows.
8. Run `cargo fmt`, `cargo clippy` and `cargo test`.

## Tests
`engine/fx/*` unit tests (synthetic buffers, sr = 48000):
- `lp_passes_dc`: DC 1.0 settles to 1.0 ± 1e-4.
- `lp_attenuates_high`: fc 200 Hz on a 10 kHz sine, RMS down > 30 dB after
  settling.
- `hp_blocks_dc`.
- `bp_peak_is_unity`: a sine at fc comes out at 0 dB ± 0.1 dB.
- `notch_kills_center`: a sine at fc is down > 40 dB.
- `stereo_channels_independent`: L sine and R silence; R stays exactly 0.
- `extreme_modulation_stays_finite`: cutoff `Add(-1e9)`, then `Add(1e9)`, and
  Q `Mul(1e6)`. The output is finite, and the clamps apply.
- `cutoff_mod_is_smoothed`: after a step in cutoff modulation, the effective
  coefficient moves over about 5 ms, not in one frame.
- `crush_one_bit`: outputs are in {-1, 0, 1}.
- `decimate_holds`: `down 4` repeats each latched value 4 times.
- `drive_bounded_monotonic`: on a ramp from -1 to 1, the output is monotonic
  with |y| ≤ 1.
- `mix_zero_is_bypass`: bit-exact.
- `set_resets_state_param_does_not`.
- `fx_param_names_match_vox`: `FxParam::name(i) == FX_PARAM_NAMES[i]` for
  every i.
- `inactive_chain_is_identity`.

End to end (`test_util::run` and render):
- `fx_filter_on_voice`: `fx v 0 filter lp 200` on a 10 kHz sine track is
  attenuated.
- `fx_two_slots_in_order`: drive then crush differs from crush then drive.
- `fx_group_applies_to_members`.
- `fx_mod_via_bank`: `modulate(Fx{slot:0,param:0}, Add(..))` called directly
  moves the cutoff. LFO isn't needed, so the test doesn't depend on merge
  order.
- `fx_parse_errors`: bad slot, unknown kind, unknown filter type,
  out-of-range values, unknown voice, empty group.

## Done when
- README §1.4 holds.
- All four effect kinds work on mono and stereo voices, and are modulatable.
- No allocation in `FxChain::process` or anywhere in `render_frame` (all state
  is fixed arrays in `Voice`).
- `voice.rs` changes are confined to the four hook points, checked with
  `git diff`.

## Risks and open questions
- Depends on VOXa's hook marker and the `fx_mods` bank (REQ-018).
- **Per-voice size** grows to about 4 slots × (8 channels × 20 B + ~100 B),
  roughly 1 KB per voice. That's fine for arena sizes in the hundreds. If E1's
  `Limits` caps voices low, keep it as is; otherwise `MAX_VOICE_CHANNELS`
  drives the cost.
- **CPU:** coefficient recomputation (sin/cos) runs only while cutoff or Q are
  moving. A modulated filter costs about 1 `sin_cos` per frame per voice,
  which is acceptable. A control-rate update (every 16 frames) is the fallback
  if profiling says otherwise.
- **Group effects** here mean "the same chain on every member", not a group
  bus. A real group or bus insert needs a summing stage in `render`, a
  mixer-graph design.
- **Future task, send effects** (reverb, delay buses): needs a mixer graph
  (buses, sends, a return path in `render`). Out of scope.
- Cutoff modulation is in Hz. Octave-scaled modulation would track better
  musically. That's a follow-up, together with the LFO.

## Size
L: four small DSP kernels, a chain, a hook and a command parser. They're
self-contained and individually testable, so one session is realistic.

## Changelog
- 2026-10-24: first draft.
