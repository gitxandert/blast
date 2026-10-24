# VOXa: voice parameter model, playback modes, pan
Status: draft   ·   Changelog at the bottom

VOX is split in two sequential parts in wave 3 that own the same files:
**VOXa** (this plan: the `VoiceParam` model, modulation bank, regions, loops,
slicing, reverse, pan, pitch, commands) and **VOXb** (interpolation, parameter
ramps, declick, fades). VOXb starts from VOXa's merged branch.

Line references are to `dev` today (`engine.rs`, `commands.rs`,
`processes.rs`). After E1 this code lives in `engine/voice.rs`,
`engine/handlers/voice.rs` and `commands/voice.rs`. The planner refreshes the
references against `integrate/wave-3` before dispatch (ORG §4.4 step 3).

## Summary
Afterwards every voice has a parameter model, `VoiceParam`, with base values
set by commands and a per-frame modulation bank that LFO/ENV/FX write into
(README §3.9, extended by REQ-018). Voices can play a region of the track, loop
it forward or ping-pong, pick one of N slices, play reversed by flag, pan
mono sources with a constant-power law, apply stereo balance, and shift pitch in
semitones. New commands `gain`, `pan`, `pitch`, `rate`, `region`, `loop`,
`reverse`, `slice` and `set` reach all of it. `velocity` keeps working
unchanged, including negative values.

## Ownership
- **Modifies:** `engine/voice.rs` (whole file: `Voice`, `VoiceState`, their
  methods and tests), `engine/handlers/voice.rs` (whole file),
  `commands/voice.rs` (whole file).
- **Reads only:** `engine/mod.rs` and `engine/render.rs` (the per-frame loop
  that calls into `Voice`), `engine/group.rs` (group start/stop call
  `Voice::start/stop`), `processes/**` (`retrigger`), `commands/reprs.rs`
  (voice lookup), `blast_time.rs` (`sample_rate::get`).

## Registry entries
Added by the architect in the wave-3 scaffold, as compiling stubs:

1. **`engine/voice.rs`: the parameter types, written out in full** (GRP builds
   against them in parallel, so they must exist before wave 3 starts):
   ```rust
   /// A continuous voice parameter. Base values are set by commands; processes add per-frame modulation.
   #[derive(Clone, Copy, Debug, PartialEq, Eq)]
   pub enum VoiceParam { Gain, Pan, Rate, Pitch, Start, End, LoopStart, LoopEnd, Slice, FadeIn, FadeOut }
   impl VoiceParam {
       pub const COUNT: usize = 11;
       pub const ALL: [VoiceParam; Self::COUNT] = [/* in declaration order */];
       pub fn index(self) -> usize { self as usize }
       pub fn name(self) -> &'static str;              // "gain", "pan", "rate", "pitch", "start", "end",
                                                        // "loopstart", "loopend", "slice", "fadein", "fadeout"
       pub fn from_name(s: &str) -> Option<VoiceParam>;
   }
   #[derive(Clone, Copy, Debug, PartialEq, Eq)]
   pub enum LoopMode { Off, Forward, PingPong }
   /// What a modulating process writes to.
   #[derive(Clone, Copy, Debug, PartialEq, Eq)]
   pub enum ModTarget { Voice(VoiceParam), Fx { slot: u8, param: u8 } }
   #[derive(Clone, Copy, Debug, PartialEq)]
   pub enum ModOp { Add(f32), Mul(f32) }
   pub const FX_SLOTS: usize = 4;
   /// FX parameter names, indexed by `ModTarget::Fx::param`. FX (wave 4) defines `FxParam` to match.
   pub const FX_PARAM_NAMES: [&str; 6] = ["cutoff", "q", "drive", "bits", "down", "mix"];
   impl VoiceState {
       pub fn set_param(&mut self, p: VoiceParam, v: f32) { /* stub: Gain → self.gain, Rate → self.velocity, else nothing */ }
       pub fn get_param(&self, p: VoiceParam) -> f32 { /* stub: Gain → self.gain, Rate → self.velocity, else 0.0 */ }
       pub fn modulate(&mut self, t: ModTarget, op: ModOp) {}
       pub fn retrigger(&mut self) { /* today's body of processes.rs:181-186 */ }
   }
   ```
2. **`processes/…`: `retrigger(voice)`** (today `processes.rs:181-186`; after
   E1 wherever it lands) becomes `voice.retrigger()` (a one-line body change,
   so PRC's edits to `processes/seq.rs` don't collide with VOX).
3. **`commands/mod.rs`: Command variants** `SetParam(SetParamArgs)` and
   `SetMode(SetModeArgs)`. The structs are stubbed in `commands/voice.rs`:
   ```rust
   pub struct SetParamArgs { pub voice: VoiceId, pub param: VoiceParam, pub value: f32 }
   pub struct SetModeArgs { pub voice: VoiceId, pub mode: VoiceMode }
   pub enum VoiceMode { Loop(LoopMode), Reverse(bool), Slices(u16) }
   ```
   (`VoiceId` is E1's voice ID type; if E1 keeps an `Idx`-style target, the
   field takes that type.)
4. **`commands/mod.rs`: one parse arm**:
   `"gain" | "pan" | "pitch" | "rate" | "region" | "loop" | "reverse" | "slice" | "set" => voice::try_param_cmd(self, cmd, args)`,
   with `try_param_cmd` stubbed in `commands/voice.rs` to return the
   "unknown command" error.
5. **`engine/mod.rs`: apply arms** `Command::SetParam(a) => handlers::voice::set_param(self, a)`
   and `Command::SetMode(a) => handlers::voice::set_mode(self, a)`, with no-op
   stubs in `engine/handlers/voice.rs`.
6. **Render call site (conditional, REQ-019).** If E1's `render` mixes voices
   per channel (`voice.process(&mut acc, ch)`, today `engine.rs:74-78`), the
   scaffold changes that call to one per-frame call,
   `voice.render_frame(&mut frame[..channels])`, before the per-channel
   write-out, with `render_frame` stubbed to forward to the old per-channel
   code. If E1 already renders per frame, this entry is empty.

## Architecture changes
- **Modulation layer on `VoiceParam`** (extends README §3.9): base values plus
  a per-frame modulation bank, `ModTarget`, `ModOp`, and FX parameter slots
  that live in `VoiceState`. Filed as **REQ-018**. Steps 2-3 depend on it.
- No new crates, threads, queues or modules.

## Interfaces
**Provides** (all in `engine/voice.rs` unless noted):
```rust
impl VoiceState {
    /// Sets the base value (clamped to the parameter's range).
    pub fn set_param(&mut self, p: VoiceParam, v: f32);
    /// The base value, without modulation.
    pub fn get_param(&self, p: VoiceParam) -> f32;
    /// Adds this frame's modulation. Cleared after the frame is rendered.
    pub fn modulate(&mut self, t: ModTarget, op: ModOp);
    /// (base + Σadd) · Πmul, clamped. What playback uses this frame.
    pub fn effective(&self, p: VoiceParam) -> f32;
    /// This frame's FX modulation for one slot: (add, mul) per FX_PARAM_NAMES index. Read by FX.
    pub fn fx_mods(&self, slot: usize) -> &[ModSlot; FX_PARAM_NAMES.len()];
    /// Restarts playback at the start of the region (or the end, when playing backwards). Bumps `trigger_count`.
    pub fn retrigger(&mut self);
    /// Incremented by every start and retrigger. ENV watches it.
    pub fn trigger_count(&self) -> u32;
    pub fn set_loop_mode(&mut self, m: LoopMode);
    pub fn set_reverse(&mut self, on: bool);
    pub fn set_slices(&mut self, n: u16);   // 0 = slicing off
}
#[derive(Clone, Copy, Debug)] pub struct ModSlot { pub add: f32, pub mul: f32 }
impl Voice {
    /// Adds this voice's contribution to one interleaved output frame.
    pub fn render_frame(&mut self, out: &mut [f32]);
}
pub const MAX_VOICE_CHANNELS: usize = 8;
// handlers/voice.rs, for GRP's group-wide commands:
pub fn set_voice_param(c: &mut Conductor, v: VoiceId, p: VoiceParam, value: f32);
// commands/voice.rs, for LFO/ENV/FX command parsing:
pub fn parse_mod_target(s: &str) -> CmdResult<ModTarget>;   // "gain", "pitch", "fx0.cutoff", ...
pub fn parse_time_secs(s: &str) -> Option<f32>;             // "200ms", "1.5s", bare number = seconds
```

Parameter table (ranges are clamps applied to base and to effective values):

| Param | Unit | Default | Range | Notes |
|---|---|---|---|---|
| Gain | linear | 1.0 | 0..=8 | command accepts `-6db` |
| Pan | -1 (L) .. 1 (R) | 0 | -1..=1 | constant power for mono, balance for multichannel |
| Rate | varispeed | 1.0 | -16..=16 | same as `velocity`; negative plays backwards |
| Pitch | semitones | 0 | -48..=48 | effective rate = Rate · 2^(Pitch/12) (REQ-023) |
| Start, End | fraction of track | 0, 1 | 0..=1 | the play region |
| LoopStart, LoopEnd | fraction of track | 0, 1 | 0..=1 | intersected with the region |
| Slice | slice index | 0 | 0..=n-1 | floor; used only while `slices > 0` |
| FadeIn, FadeOut | seconds | 0, 0 | 0..=60 | stored by VOXa, used by VOXb; not modulatable |

**Consumes:** E1's `Voice`/`VoiceState`/`Conductor` and voice lookup (REQ-019),
`sample_rate::get()`, E1's `Command::Batch` (for `region` and `loop … <a> <b>`).

**Contract issues:** README §3.9 names only `set_param`/`get_param`. A
modulator that writes base values with `set_param` would erase the user's
setting and fight other modulators, so VOXa adds `modulate`/`effective`
(REQ-018).

## Steps
1. **State rewrite** (`engine/voice.rs`; today `engine.rs:365-379` and
   `:407-435`). `VoiceState` becomes: `active`, `position: f64` (f32 stops
   counting single frames past 2^24 frames, about 6 minutes at 48 kHz),
   `len: usize` (frames; replaces `end`), `base: [f32; VoiceParam::COUNT]`,
   `mods: [ModSlot; VoiceParam::COUNT]`,
   `fx_mods: [[ModSlot; 6]; FX_SLOTS]`, `loop_mode`, `reverse`, `slices: u16`,
   `pp_dir: f64` (ping-pong direction, ±1), `trigger_count: u32`, plus E1's
   tempo ID. `velocity` and `gain` fields go away. `set_param`/`get_param`
   read and write `base` (Rate is what `velocity` used to be). Keep every
   existing voice test passing. They check `position` and output values,
   which don't change at rate 1, pan aside (step 5).
2. **Modulation bank.** `modulate` folds `Add` into `mods[i].add` and `Mul` into
   `mods[i].mul`. `effective(p)` = `clamp((base + add) · mul)`. `fx_mods`
   exposes the FX bank. A private `clear_mods()` resets every slot to
   `{add: 0, mul: 1}` and is called at the end of `Voice::advance` (today
   `engine.rs:578-589`), so the order within a frame is: processes write mods,
   `render_frame` reads them, `advance` reads the rate and clears them. FadeIn
   and FadeOut ignore modulation.
3. **Region and loop geometry.** A private
   `fn bounds(&self) -> Bounds { rs, re, ls, le, looping }` computes frames
   from the effective params once per frame: `rs = floor(start·len)`,
   `re = ceil(end·len) - 1`. With `slices = n > 0`, the region is slice
   `i = floor(eff Slice)` clamped to `0..n`: `rs = i·len/n`, `re = (i+1)·len/n - 1`
   (Start and End are ignored). `ls/le` are LoopStart/LoopEnd mapped the same
   way, intersected with `[rs, re]`. If the intersection is empty, the loop
   counts as off. If `re < rs`, the voice renders silence. Cache the result in
   a `frame: FrameCache` field (bounds, gains, step), filled at the top of
   `render_frame` and reused by `advance`, so nothing is computed twice.
4. **Direction, start and retrigger.** `dir = sign(eff Rate) · (reverse ? -1 : 1) · pp_dir`.
   `start()` (today `engine.rs:437-463`) and `retrigger()` set `pp_dir = 1`,
   bump `trigger_count`, and put `position` at `rs` when `dir > 0`, or `re`
   otherwise. Keep the tempo and process-reset behaviour of `start`/`stop` as
   E1 left it. Negative `velocity` still starts at the end
   (`reverse_playback_plays_every_frame_then_stops`).
5. **`render_frame(out)`** replaces `process(acc, ch)` (`engine.rs:532-575`).
   - Return if inactive, if `re < rs`, or if one-shot playback has left
     `[rs, re]` (today's `in_bounds`).
   - Read the source frame into a stack `[f32; MAX_VOICE_CHANNELS]`:
     `idx = floor(position)`. Read the frame directly when `frac == 0`.
     Otherwise interpolate linearly toward the *next frame along the play
     direction's geometry*: `le → ls` when looping forward, clamped at `re`
     otherwise (today's "last frame has no next frame" rule). VOXb replaces
     this with Hermite.
   - Leave a marked hook for FX: `// FX hook (wave 4): inserts process src[..nch] here.`
   - Mix out. **Mono source:** with 1 output channel, `out[0] += s·g`. With 2
     or more, constant power: `θ = (pan+1)·π/4`, `out[0] += s·g·cos θ`,
     `out[1] += s·g·sin θ`, other channels untouched. **Source channels ==
     output channels:** pass through, with balance on channels 0/1
     (`gL = min(1, 1-pan)`, `gR = min(1, 1+pan)`). **Multichannel source,
     1 output channel:** average the channels. **Other mismatches:** channel
     `c` goes to output `c` for `c < min(src, out)`. Channels beyond
     `MAX_VOICE_CHANNELS` are ignored.
   - Update the voice tests: `mono_voice_on_stereo_advances_once_per_frame`
     (today `engine.rs:843-854`) now expects `s·cos(π/4)` on both channels.
     Assert within 1e-6.
6. **`advance()`** (`engine.rs:578-589`): `step = |eff Rate| · 2^(eff Pitch/12) · dir`, then
   `position += step`, then:
   - **Off:** nothing more. Playback ends when the position leaves `[rs, re]`.
   - **Forward:** with `L = le - ls + 1`, if moving forward and
     `position ≥ le + 1`, or moving backward and `position < ls`, set
     `position = ls + (position - ls).rem_euclid(L)`. An intro before `ls`, or
     a tail after `le` when reversed, plays once.
   - **PingPong:** if `position > le`, set `position = le - (position - le)` and
     flip `pp_dir`. If `position < ls`, mirror the same way. Then clamp to
     `[ls, le]` (covers steps longer than the loop). `ls == le` holds the frame.
   - Then `clear_mods()`.
7. **Handlers** (`engine/handlers/voice.rs`): `set_param` and `set_mode` look
   up the voice by ID and call `set_param`, `set_loop_mode`, `set_reverse` or
   `set_slices`. Unknown IDs can't happen, because the mirror validated them;
   use `let Some(..) else { return }`, never `unwrap`. Add
   `pub fn set_voice_param(..)` for GRP. The `Velocity` arm becomes
   `set_param(Rate, val)`. `Command::Velocity` stays, so existing tests and
   scripts keep their meaning.
8. **Commands** (`commands/voice.rs`): `try_param_cmd(cp, cmd, args)`:
   - `gain <v> <x | x db>` → `SetParam(Gain)`. `db` is case-insensitive:
     `10^(x/20)`.
   - `pan <v> <-1..1>`, `pitch <v> <semitones>`, `rate <v> <x>` (an alias for
     `velocity`).
   - `region <v> <start> <end>` (fractions) → `Batch[SetParam(Start), SetParam(End)]`.
     Error if `start ≥ end`.
   - `loop <v> off|on|pp [<ls> <le>]` → `SetMode(Loop)`, plus LoopStart/LoopEnd
     in a `Batch` when given.
   - `reverse <v> [on|off]` (default `on`) → `SetMode(Reverse)`.
   - `slice <v> <n> [<i>]` → `Batch[SetMode(Slices(n)), SetParam(Slice, i)]`.
     `slice <v> off` → `Slices(0)`. Error if `i ≥ n` or `n > 1024`.
   - `set <v> <param> <value>` → `SetParam` for any `VoiceParam::from_name`.
   - Values are parsed and range-checked here. Out-of-range values are errors,
     not clamps, so the user sees them. The engine clamps again as a safety
     net. A voice lookup failure leaves the mirror untouched; no mirror state
     changes at all.
   - `parse_mod_target`: a `VoiceParam` name (except fadein/fadeout), or
     `fx<N>.<name>` with `N < FX_SLOTS` and `<name>` in `FX_PARAM_NAMES`.
     `parse_time_secs`: `<x>ms`, `<x>s`, bare `<x>` = seconds; must be finite
     and `≥ 0`.
   - `HELP` rows for every new command.
9. Run `cargo fmt`, `cargo clippy` on owned files, and `cargo test`.

## Tests
In `engine/voice.rs` (through the mirror and engine with `test_util::run`,
unless noted):
- `param_defaults_and_roundtrip`: every `VoiceParam` has its default; `set`
  then `get` returns the clamped value.
- `modulation_does_not_touch_base`: `modulate(Add)` changes `effective` and
  leaves `get_param` alone; mods are cleared after one rendered frame.
- `mul_and_add_combine`: `(base + Σadd) · Πmul` with two adds and two muls.
- `gain_scales_output`, `gain_db_parses`: `gain v -6db` gives ≈0.501.
- `mono_center_pan_is_constant_power`, `mono_hard_left_right`: pan -1 puts
  everything on ch0 and pan 1 everything on ch1. The power sum is 1 at every
  pan.
- `stereo_balance_keeps_unity_at_center`: the existing stereo tests stay exact.
- `region_plays_only_its_frames`: track `0..100`, `region v 0.1 0.2` plays
  10..=19, then silence.
- `region_reverse_plays_backwards`: `reverse v on` plays 19..=10.
- `negative_velocity_still_reverses`: the existing reverse tests pass unchanged.
- `reverse_flag_and_negative_rate_cancel`.
- `loop_forward_wraps`: `loop v on` on a 5-frame track plays
  `0,1,2,3,4,0,1,…`.
- `loop_points_with_intro`: loop 0.5..1 on `0..10` plays `0..9`, then `5..9`
  repeatedly.
- `loop_reverse_wraps`, `pingpong_reflects`: `0,1,2,3,4,3,2,1,0,1,…`.
- `pingpong_fast_rate_stays_in_bounds`: rate 7 on a 4-frame loop never reads
  outside `[ls, le]`.
- `slice_selects_region`: `slice v 4 2` on `0..100` plays 50..=74.
- `slice_mod_scans`: `modulate(Voice(Slice), Add(1.0))` moves to the next
  slice.
- `pitch_octave_doubles_rate`: `pitch v 12` advances 2 frames per frame.
- `start_mod_out_of_region_ends_oneshot`.
- `retrigger_bumps_trigger_count`: start and Seq retrigger each bump it.
- `position_is_f64_past_2_24`: set `position = 2^24 + 0.5` and advance 1 at
  rate 1; the fraction survives.

In `commands/voice.rs`:
- `param_cmds_parse` (each command produces the right Args),
  `param_cmd_errors` (missing value, NaN, out of range, `start ≥ end`,
  `slice i ≥ n`, unknown voice, unknown param),
  `failed_param_cmd_leaves_mirror_unchanged`,
  `parse_mod_target_names` (`gain`, `fx0.cutoff`, `fx4.q` → error,
  `fadein` → error), `parse_time_units`.

## Done when
- README §1.4 holds.
- Every voice parameter is reachable through `set_param` and `get_param`, and
  every command in step 8 works.
- Every pre-existing voice and seq test still passes. The one mono-pan test is
  updated as described in step 5.
- `render_frame` and `advance` allocate nothing. They use only stack arrays.

## Risks and open questions
- **Modulation layer** (REQ-018): if it's rejected, LFO and ENV have to write
  base values directly, and a voice can carry only one modulator per param.
- **E1 shapes** (REQ-019): the per-frame render API, where the process loop
  runs, and the `VoiceId`/`Idx` type. The plan is refreshed once E1 lands.
- **Pitch as its own parameter** (REQ-023), instead of sugar that rewrites
  Rate.
- **Centre level of mono voices drops 3 dB** on stereo outputs. That's
  inherent to the constant-power law the brief asks for. A compensated law
  (+3 dB at the sides) is the alternative if the user prefers today's level.
- **Region modulation** can end a one-shot voice early, if Start moves past
  the play position. This is documented in HELP. A "latch region at trigger"
  option is a possible follow-up.
- The mirror doesn't record parameter values, so `info` (INS) can't show them.
  That's a follow-up: mirror the parameters in `VoiceRepr`
  (`commands/reprs.rs`, not owned here).
- **Future task, time-stretching** (rate without pitch change, from
  `notes.txt`): granular or phase-vocoder playback as a new voice read mode.
  Out of scope.
- `MAX_VOICE_CHANNELS = 8`: tracks with more channels play only their first 8.
  If E1's `Limits` defines a channel cap, use that instead.

## Size
L: it rewrites the core of `Voice`, adds 9 commands, and has about 30 tests.
Interpolation and ramps are split off into VOXb to keep this part to one
session.

## Changelog
- 2026-10-24: first draft.
