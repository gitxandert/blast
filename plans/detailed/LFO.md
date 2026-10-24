# LFO: low-frequency oscillator process
Status: draft   ·   Changelog at the bottom

## Summary
Afterwards `lfo` attaches a modulating process to a voice or a group. It has
five waveforms: sine, triangle, square, saw, and sample-and-hold. It runs
either tempo-synced (a `TempoId`, one cycle per `-p` steps) or free in Hz. It
modulates any `VoiceParam` or FX parameter through VOX's modulation bank
(`VoiceState::modulate`), so it never overwrites the user's base value, and
several LFOs stack.

## Ownership
- **Creates/fills:** `processes/lfo.rs` and `commands/procs/lfo.rs` (stubbed by
  the scaffold).
- **Reads only:** `processes/mod.rs` (the `processes!` macro, PRC's process
  plumbing), `processes/seq.rs` (pattern), `commands/process.rs` and
  `commands/procs/seq.rs` (PRC's owner and tempo parsing),
  `engine/voice.rs` (VOX), `blast_rand.rs`, `blast_time.rs`.

## Registry entries
Wave-4 scaffold:
1. `processes/mod.rs`: `pub mod lfo;` and a `processes!` row `Lfo`, with
   `processes/lfo.rs` stubbed as `pub struct Lfo;` and no-op methods matching
   the macro's per-process signature (as E1/PRC leave it).
2. `commands/procs/mod.rs`: `pub mod lfo;`, with `commands/procs/lfo.rs`
   stubbed: `pub struct LfoArgs { /* fields as in Interfaces */ }`,
   `pub fn try_lfo(..) -> CmdResult<Command>` returning the "unknown command"
   error, and `pub const HELP: &[(&str, &str)] = &[];`.
3. `commands/mod.rs`: the variant `Lfo(LfoArgs)` and the parse arm
   `"lfo" => procs::lfo::try_lfo(self, args)`.
4. `engine/mod.rs` apply arm: `Command::Lfo(a) => handlers::process::add(self, a.owner, Process::Lfo(Lfo::new(&a)), a.tempo)`.
   It uses PRC's generic process-insert function (REQ-020); the exact call
   follows PRC's signature.
5. The INS help index: a row for `procs::lfo::HELP`.

## Architecture changes
None. It depends on REQ-018 (the modulation layer) and REQ-020 (PRC's process
API).

## Interfaces
**Provides:**
```rust
pub enum LfoWave { Sine, Triangle, Square, Saw, SampleHold }
pub enum LfoClock { Synced { period: f32 }, Free { inc: f64 } }   // period in tempo steps; inc = hz / sample_rate
pub struct LfoArgs {
    pub owner: ProcOwner,       // PRC's voice-or-group owner type
    pub tempo: TempoRepr,       // used only when clock is Synced (as for seq)
    pub target: ModTarget,
    pub wave: LfoWave,
    pub clock: LfoClock,
    pub depth: f32,
    pub offset: f32,
    pub phase: f32,             // 0..1 start phase
    pub seed: u64,
    pub name: Option<String>,   // or PRC's name handling
}
pub struct Lfo { wave, clock, target, depth, offset, phase0: f64, phase: f64, last: f64, held: f32, rng: X128P }
impl Lfo {
    pub fn new(a: &LfoArgs) -> Self;
    /// Current output: offset + depth · w(phase), w ∈ [-1, 1].
    fn value(&mut self, tempo: &TempoState) -> f32;
}
```
**Consumes:** `VoiceState::modulate(ModTarget, ModOp::Add(v))` (VOXa), and
`commands::voice::parse_mod_target` / `parse_time_secs` (VOXa). From PRC
(REQ-020): owner parsing (voice vs group), tempo-flag parsing for `-t`
(shared with `seq`), `--seed` handling, process naming, and the
process-insert handler. From E1: the per-process signature, assumed to be
`process(&mut self, v: &mut VoiceState, t: &TempoState)` and a group variant
that visits every member (REQ-019).

## Steps
1. **`processes/lfo.rs`: phase.**
   - Synced: `phase = (tempo.current_in(period) / period + phase0).fract()`.
     Phase is a pure function of the tempo position, so it stays locked to the
     tempo, restarts with it, and holds while the tempo is paused.
   - Free: `phase = (phase + inc).fract()` once per `process` call.
   - `reset()` sets `phase = phase0` and `last = phase0`, and draws a fresh S&H
     value.
2. **Waveforms**, with `p = phase ∈ [0,1)`:
   - sine: `sin(2πp)`
   - triangle: `1 - 4·|p - 0.5|`, shifted so `p = 0` gives 0 rising:
     `tri(p) = 1 - 4·|((p + 0.25).fract()) - 0.5|`
   - square: `p < 0.5 ? 1 : -1`
   - saw: `2p - 1` (rising)
   - S&H: `held`, redrawn as `rng.next_f32()·2 - 1` whenever the phase wraps
     (`phase < last`)
   Compute in f64 and convert once.
3. **`process`**: `v.modulate(target, ModOp::Add(offset + depth·w))`.
   **`process_group`**: compute `value` once, then modulate every member, so
   all members move in phase. Tempo handling (owned Process tempo, `-t v`,
   `-t g:…`, `-t c:…`) follows `Seq` exactly, through PRC's shared code.
4. **`commands/procs/lfo.rs`: `try_lfo`.** Syntax:
   ```
   lfo <owner> <target> -d <depth> [-w sine|tri|square|saw|sh] [-o <offset>]
       [-f <hz> | -t <tempo> [-p <steps>]] [--phase <0..1>] [--seed <u64>] [-n <name>]
   ```
   - `<owner>` uses PRC's owner syntax: a voice name (`v`, `g.v`), or
     `-g <group>` as the first token, as `seq -g` does (PRCb, REQ-020).
   - `<target>` goes through `parse_mod_target` (`gain`, `pan`, `pitch`,
     `start`, `fx0.cutoff`, …).
   - `-d` is required, because no default depth fits every parameter. It may
     be negative (an inverted LFO).
   - Default wave: sine.
   - Default clock: synced to a Process-owned tempo at 120 BPM with `-p 4`
     (one cycle per bar), the same default tempo as `seq`.
   - `-f` (free-running Hz, `0 < f ≤ sr/2`) and `-t`/`-p` are mutually
     exclusive.
   - `--phase` must be in `0..1`.
   - The seed comes from `--seed`, else `fast_seed()`.
   - Convert Hz to `inc` here, with `sample_rate::get()`.
   - Mirror bookkeeping (process name and ID, tempo) goes through PRC's
     helpers exactly as `seq` does. A parse failure leaves the mirror
     untouched.
   - `HELP` rows.
5. **`set_enabled(on)`** (PRCb's `processes!` dispatch method): while off,
   `process`/`process_group` write no modulation; turning on keeps the phase
   (synced phase is a function of the tempo anyway).
   Test: `disabled_lfo_writes_nothing`.
6. Run `cargo fmt`, `cargo clippy` and `cargo test`.

## Tests
`processes/lfo.rs` (unit tests on `Lfo` with a hand-built `TempoState` and
`VoiceState`):
- `sine_quarter_points`: at phases 0, .25, .5 and .75, the output is 0, 1, 0, -1
  (±1e-6) times depth, plus offset.
- `triangle_square_saw_shapes`: the same four sample points, with the expected
  values.
- `sample_hold_changes_only_on_wrap`: constant within a cycle, and a new value
  after the wrap; the same seed gives the same sequence.
- `synced_phase_follows_tempo`: `s:100` with `-p 4` completes one cycle per
  400 frames, and pausing the tempo freezes the phase.
- `free_running_hz`: 480 Hz at 48 kHz has period 100 frames.
- `reset_restores_start_phase`.
- `adds_not_overwrites`: the base gain stays 1.0 while the effective gain
  follows `1 + lfo`, and two LFOs on the same param sum.

End to end (through `test_util::run` and render):
- `lfo_gain_tremolo`: `lfo v gain -d 0.5 -f 480` on a DC track: the output
  envelope is `1 + 0.5·sin` within 1e-3.
- `lfo_on_group_moves_all_members_in_phase`.
- `lfo_pan_sweeps`.
- `lfo_fx_target_parses` (FX may not be merged yet; the test checks only the
  `ModTarget`).
- `lfo_parse_errors`: no `-d`, both `-f` and `-t`, `-f 0`, unknown wave,
  unknown target, `--phase 1.5`, unknown owner. Each one leaves the mirror
  unchanged.

## Done when
- README §1.4 holds.
- All five waveforms work, synced and free.
- Group targets modulate every member.
- No allocation in `process`/`process_group`. The rng and every buffer are
  built in `Lfo::new`, on the REPL thread or in the handler, without heap use.

## Risks and open questions
- **REQ-018** (modulation bank) and **REQ-020** (PRC's owner, tempo, seed and
  insert API): the command and handler wiring follow whatever PRC lands.
- Square and S&H are hard steps. On gain they can click. Possible follow-ups
  are a `--slew <ms>` one-pole on the LFO output, or reusing FX's one-pole
  smoother. Not in this plan.
- Modulating cutoff in Hz is linear. An octave-based depth for `fx*.cutoff`
  would feel more musical. That's a follow-up, coordinated with FX.
- Unipolar output (`0..1`) is `-o 0.5 -d 0.5` on any waveform, since they're
  all bipolar. No extra flag.

## Size
M: one small DSP struct plus a parser that mostly reuses PRC's helpers.

## Changelog
- 2026-10-24: first draft.
- 2026-10-24: implements PRCb's `set_enabled` dispatch method (PRCb registry entry 4); owner syntax follows PRCb's `-g <group>` form (REQ-020).
