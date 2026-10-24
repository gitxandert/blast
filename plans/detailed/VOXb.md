# VOXb: interpolation, parameter ramps, declick, fades
Status: draft   ·   Changelog at the bottom

The second half of VOX (wave 3). It starts from VOXa merged on
`task/VOXa` (or on `integrate/wave-3` if the architect has merged VOXa by
then), and owns the same files.

## Summary
Afterwards voices interpolate with 4-point cubic Hermite (Catmull-Rom), so
rates other than 1 sound clean. Command changes to gain and pan ramp over
about 5 ms instead of jumping. Retriggering a sounding voice crossfades the
old read head out over about 3 ms. Start fades in, and stop and pause fade
out, by `max(declick, FadeIn/FadeOut)`. The FadeIn/FadeOut parameters (stored
by VOXa) become audible. ENV's `fade` command (wave 4) sets them.

## Ownership
- **Modifies:** `engine/voice.rs` (playback internals and tests),
  `engine/handlers/voice.rs` (stop and pause routing only), and
  `commands/voice.rs` (HELP text for the fades only).
- **Reads only:** the same files VOXa reads.

## Registry entries
None. (If E1's group code calls `voice.stop()` and checks `active` straight
afterwards, nothing breaks: see step 4.)

## Architecture changes
None. The scope split between VOX and ENV is a user decision, filed as
**REQ-022**. Interpolation is **REQ-021**.

## Interfaces
- **Provides:** no new public API. `set_param(Gain|Pan)` now ramps, and
  `stop()`/`pause()` now take effect after the fade-out.
  `VoiceState::is_stopping() -> bool` is added for INS/GRP status.
- **Consumes:** VOXa's `VoiceState`, `FrameCache`, `bounds()`, and
  `sample_rate::get()`.
- **Contract issues:** none.

## Steps
1. **Hermite read** (in VOXa's `render_frame` read stage). Use 4-point,
   3rd-order Catmull-Rom on frames `i-1, i, i+1, i+2`:
   `y = ((c3·t + c2)·t + c1)·t + c0` with
   `c0 = x0`, `c1 = ½(x1 - x₋₁)`, `c2 = x₋₁ - 2.5x0 + 2x1 - ½x2`,
   `c3 = ½(x2 - x₋₁) + 1.5(x0 - x1)`. Neighbours come from one private
   `fn neighbour(&self, b: &Bounds, i: isize) -> isize`:
   - Looping forward: wrap within `[ls, le]` once the position is inside the
     loop.
   - Ping-pong: reflect.
   - One-shot or region edges: **linear extrapolation** (`x₋₁ = 2x0 - x1`,
     `x_{n} = 2x_{n-1} - x_{n-2}`). Catmull-Rom then reproduces linear ramps
     exactly, so the existing `half_speed_interpolates` tests stay exact.
   When `frac == 0`, read the frame directly. That keeps rate-1 output
   bit-exact.
2. **Ramps.** Add a `Ramp { cur: f32, target: f32, step: f32, left: u32 }` for
   Gain and for Pan, with a length of `RAMP_SECS = 0.005`·sr frames.
   `set_param(Gain|Pan)` sets the target. When the voice is inactive, it jumps
   instead (no ramp before the first sound). `effective()` uses `ramp.cur` as
   the base for these two params. Advance the ramps once per frame in
   `advance()`. Modulation is not smoothed; LFO/ENV produce smooth values
   themselves.
3. **Retrigger declick.** Add `tail: Tail { pos: f64, step: f64, left: u32, len: u32 }`.
   `retrigger()` and `start()` on a voice that is sounding (active, inside its
   region, effective gain > 0) copy the current position and step into
   the tail, with `left = DECLICK_SECS (0.003)·sr`. `render_frame` also reads
   the tail (same interpolation) and adds it, with a linear fade `left/len`,
   into the **source frame**, before the FX hook and before gain and pan. The
   insert chain (wave 4) then sees one signal, and the tail uses the current
   gain and pan, which differ from the old ones for 3 ms at most. The tail's position advances in `advance()` without looping (it
   is at most 3 ms long). The new head starts at full level, because the
   sample start is the material's own attack. Cost: one extra read per
   channel, for about 144 frames after each retrigger.
4. **Start fade-in, stop and pause fade-out.** Add
   `fade: Fade { Idle | In{left,len} | Out{left,len,then: AfterFade} }` with
   `AfterFade { Stop, Pause }`.
   - `start()` sets `In` with `len = max(DECLICK, FadeIn·sr)`. On a voice that
     was already sounding, the tail from step 3 covers the old sound.
   - `stop()` and `pause()` (today `engine.rs:465-504`) on a sounding voice
     set `Out` with `len = max(DECLICK, FadeOut·sr)` and leave `active` true.
     Processes and tempos still reset or stop **immediately** (E1's current
     rules), so a Seq can't retrigger during the fade. When `left` reaches 0,
     `advance()` applies the deferred part: `Stop` sets `active = false` and
     resets the position to the region start; `Pause` sets `active = false`
     and keeps the position.
   - A voice that isn't sounding stops or pauses at once, as today.
   - `start()` or `resume()` during `Out` cancels it.
   - `is_stopping()` reports `Out`.
   - Group stop calls `voice.stop()` for each member, so it fades the same
     way. Update the existing tests that assert `!active` straight after
     `stop`/`pause` (`voice_start_pause_resume_stop`, `group_stop_resets_voices`,
     the `start_stop_reach_group_voice` sequence). They either render the
     declick first, or use a voice that hasn't sounded yet. Every such edit is
     listed in the report.
5. `FadeIn`/`FadeOut` HELP text in `commands/voice.rs` (`set <v> fadein 0.2`),
   noting that ENV adds `fade` sugar in wave 4.
6. Run `cargo fmt`, `cargo clippy` and `cargo test`.

## Tests
- `rate_one_is_bit_exact`: output equals the source sample for sample.
- `hermite_reproduces_linear_ramp`: the existing half-speed tests pass at
  every edge.
- `hermite_beats_linear_on_sine`: a 1 kHz sine resampled at rate 0.37 has a
  lower RMS error against the analytic sine than the linear result. Measure
  both in the test.
- `hermite_wraps_at_loop_end`: in a forward loop of a sine of exactly one
  period, the output is continuous across the seam (max sample-to-sample
  delta below a bound).
- `gain_change_ramps`: `gain v 0` on a playing DC track decreases
  monotonically over ~240 frames at 48 kHz, then reaches 0.
- `gain_change_before_start_is_immediate`.
- `pan_change_ramps`.
- `retrigger_crossfades_tail`: a DC track of 1.0, retriggered mid-play, never
  shows a jump bigger than the declick allows, and the sum is bounded by 2.
- `retrigger_on_silent_voice_has_no_tail`.
- `stop_fades_then_deactivates`: still active for DECLICK frames, level
  decreasing, then inactive with the position reset.
- `pause_fades_then_keeps_position`.
- `fadeout_param_lengthens_stop`: `set v fadeout 0.1` gives 4800 frames.
- `fadein_param_lengthens_start`.
- `resume_during_fadeout_cancels`.
- `stop_while_not_sounding_is_immediate`.
- `seq_does_not_retrigger_during_fadeout`.

## Done when
- README §1.4 holds.
- No discontinuity larger than the source's own steps on retrigger, stop,
  pause or gain/pan change (asserted by the tests above).
- Rate-1 playback is bit-exact.
- `render_frame`/`advance` still allocate nothing.

## Risks and open questions
- **REQ-022:** the scope of declick and ramps (recommended here, in VOX), and
  the change that stop and pause become short fades instead of instant cuts.
  If the user wants instant stop, step 4 uses `DECLICK = 0` and keeps only the
  FadeOut parameter.
- **REQ-021:** cubic Hermite against linear (or against a windowed sinc). Sinc
  sounds better at high rates but costs 8-16 taps. Hermite is the usual
  sampler compromise. Aliasing on downward transposition (rate > 1) isn't
  addressed by any of these. A future option is mip-mapped or band-limited
  reads.
- With E1's tempo semantics, a voice that is fading out has already stopped
  its own tempo. LFOs on that voice that follow the voice tempo freeze during
  the 3 ms. That's inaudible.

## Size
M: four contained mechanisms in one file, each with direct tests.

## Changelog
- 2026-10-24: first draft.
