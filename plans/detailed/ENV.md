# ENV: envelope process and fades
Status: draft   ·   Changelog at the bottom

## Summary
Afterwards `env` attaches an AD, AR or ADSR envelope to a voice or a group.
Every (re)trigger fires it: a voice start, or a Seq step. By default it
multiplies Gain, and it can target any modulatable parameter. `fade <v> in|out <time>`
sets the voice's fade-in on start and fade-out on stop.

## Ownership
- **Creates/fills:** `processes/env.rs` and `commands/procs/env.rs` (stubbed by
  the scaffold).
- **Reads only:** `processes/mod.rs`, `processes/seq.rs`, `engine/voice.rs`
  (VOX: `trigger_count`, `modulate`, FadeIn/FadeOut), `commands/voice.rs`
  (`parse_mod_target`, `parse_time_secs`, `SetParamArgs`),
  `commands/process.rs` (PRC helpers), `blast_time.rs`.

## Registry entries
Wave-4 scaffold:
1. `processes/mod.rs`: `pub mod env;` and a `processes!` row `Env`, with a
   no-op `pub struct Env;` stub.
2. `commands/procs/mod.rs`: `pub mod env;`, with `commands/procs/env.rs`
   stubbed (`EnvArgs`, `try_env` and `try_fade` returning "unknown command",
   and an empty `HELP`).
3. `commands/mod.rs`: the variant `Env(EnvArgs)`, and the parse arms
   `"env" => procs::env::try_env(self, args)` and
   `"fade" => procs::env::try_fade(self, args)`. `fade` produces VOX's existing
   `Command::SetParam`, so it needs no variant of its own.
4. `engine/mod.rs` apply arm: `Command::Env(a) => handlers::process::add(self, a.owner, Process::Env(Env::new(&a)), …)`,
   through PRC's generic insert (REQ-020). The envelope has no tempo; pass
   whatever PRC uses for "no tempo".
5. The INS help index: a row for `procs::env::HELP`.

**The trigger hook needs no registry entry.** VOXa gives every voice a
`trigger_count` that `start()` and `retrigger()` (and so every Seq retrigger)
increment. ENV watches it, the same way `Seq` watches `TempoState.epoch`. This
is the smallest hook there is: nothing in `processes/seq.rs` or `retrigger()`
changes, and any future trigger source (`trig`, piping) fires envelopes for
free.

## Architecture changes
None. Depends on REQ-018 (modulation bank and `trigger_count`) and REQ-020.

## Interfaces
**Provides:**
```rust
pub enum EnvCurve { Linear, Exp }
pub struct EnvArgs {
    pub owner: ProcOwner,
    pub target: ModTarget,            // default Voice(Gain)
    pub op: EnvOp,                    // Mul (default for Gain) | Add (default otherwise)
    pub depth: f32,                   // default 1.0
    pub attack: u32, pub decay: u32,  // frames
    pub sustain: f32,                 // 0..=1
    pub hold: u32,                    // frames held at sustain before release; u32::MAX = until the next trigger
    pub release: u32,                 // frames
    pub curve: EnvCurve,
    pub name: Option<String>,
}
pub struct Env { /* args, plus stage: Stage { Idle, Attack, Decay, Sustain{left}, Release }, level: f32, from: f32, t: u32, seen: Option<u32> */ }
```
**Consumes:** `VoiceState::trigger_count()` and `modulate()` (VOXa),
`VoiceParam::FadeIn`/`FadeOut` and `Command::SetParam` (VOXa), the stop and
start fade mechanics (VOXb), and PRC's owner, naming and insert helpers
(REQ-020).

## Steps
1. **Trigger detection** (`processes/env.rs`).
   - Voice: if `Some(v.trigger_count()) != seen`, restart: `from = level`,
     stage = Attack, `t = 0`, and record `seen`.
   - Group: `seen` holds the wrapping sum of the members' counts, and any
     change restarts the one shared envelope. A group Seq retriggers every
     member in the same frame, so they share a single envelope.
   - `reset()` sets `seen = None`, `level = 0` and stage Idle. The first
     `process` after a voice start therefore sees a change and fires.
   - Attack starts from the current `level`, not 0, so a retrigger in the
     middle of a release doesn't click.
2. **Stages**, advanced once per `process` call:
   - Attack: `from → 1` over `attack` frames.
   - Decay: `1 → sustain` over `decay` frames.
   - Sustain: hold for `hold` frames (`u32::MAX` holds until the next trigger).
   - Release: `sustain → 0` over `release` frames.
   - Idle: 0.
   Zero-length stages are skipped in the same call.
   - `Linear`: the level moves by a straight line per stage.
   - `Exp`: attack stays linear (that sounds right for short attacks). Decay
     and release use `level = target + (level - target)·k` with
     `k = exp(ln(0.001)/frames)`, snapping to the target after `frames`. That
     reaches -60 dB at the stage's end, so the stated times hold.
3. **Output.**
   - `Mul`: `modulate(target, Mul(1 - depth + depth·level))`. At depth 1 the
     voice is silent until it's triggered, then follows the envelope.
   - `Add`: `modulate(target, Add(depth·level))`.
4. **Presets → `EnvArgs`** (`commands/procs/env.rs`):
   ```
   env <owner> ad <attack> <decay>                         sustain 0, hold 0, release 0
   env <owner> ar <attack> <release> [<hold>]              sustain 1, hold default 0
   env <owner> adsr <attack> <decay> <sustain> <release> [<hold>]   hold default: until the next trigger
       [--to <target>] [--depth <d>] [--mul | --add] [--exp] [-n <name>]
   ```
   - Times use `parse_time_secs` (`5ms`, `0.2s`, `0.2`). They're converted to
     frames here with `sample_rate::get()`, saturating at `u32::MAX - 1`.
   - `--to` goes through `parse_mod_target`.
   - Default op: `--mul` for Gain, `--add` for everything else.
   - `sustain` must be in `0..=1`; `depth` must be finite.
   - Mirror bookkeeping goes through PRC's helpers. A failed parse leaves the
     mirror unchanged.
5. **`try_fade`**: `fade <v> in|out <time>` →
   `Command::SetParam(SetParamArgs { voice, param: FadeIn | FadeOut, value: secs })`.
   `fade <v> in|out off` sets 0. The mechanics (a fade-in on `start`, a fade-out
   that defers `stop`/`pause`) are VOXb's.
6. `HELP` rows for `env` and `fade`. Run `cargo fmt`, `cargo clippy` and
   `cargo test`.

## Tests
`processes/env.rs` unit tests:
- `ad_shape_linear`: with attack 10 and decay 20 frames, the level is 0.1 at
  frame 1, 1.0 at frame 10, and 0 at frame 30.
- `adsr_holds_then_releases`.
- `adsr_default_hold_waits_for_next_trigger`.
- `exp_reaches_minus_60db_at_stage_end`.
- `zero_length_stages_skip`.
- `retrigger_restarts_from_current_level`: no jump in level.
- `reset_fires_on_next_process`.
- `group_env_fires_once_per_group_retrigger`.

End to end:
- `env_on_seq_retrigger`: `seq v -t s:1000 -s 0,2` plus `env v ad 1ms 5ms` on a
  DC track at 48 kHz. The output peaks at frames 48 and 2048, and is 0 from
  frame 288 (1 ms + 5 ms) until the next step.
- `env_mul_keeps_base_gain`: `gain v 0.5` plus an envelope at full level gives
  0.5.
- `env_add_on_pitch`: `env v ad 0 10ms --to pitch --depth 12` starts an octave
  up and falls back.
- `fade_in_sets_param`, `fade_out_defers_stop`: render after `stop`; the level
  ramps to 0 over the fade time.
- `env_parse_errors`: unknown preset, missing times, negative time,
  `sustain 1.5`, unknown target, `fade v sideways 1s`, unknown voice. Each one
  leaves the mirror unchanged.

## Done when
- README §1.4 holds.
- AD, AR and ADSR fire on start and on every Seq retrigger, for voices and
  groups.
- `fade in`/`fade out` work end to end.
- No allocation in `process`.

## Risks and open questions
- **REQ-018** (`trigger_count`, modulation bank) and **REQ-020** (PRC wiring).
- Without note-off events, ADSR's "release" starts after `hold`, not at a key
  release. If piping (`seq | trig`, brief PF) later brings gate lengths,
  `hold` becomes "gate time" naturally.
- A group shares one envelope, even when members are retriggered
  individually. Per-member envelopes would need `MAX_GROUP` states per Env,
  which is preallocated but larger. That's a follow-up if wanted.
- Tempo-relative times (`-t b:120`, times in steps) are a nice-to-have. Not in
  this plan.

## Size
M: one state machine plus two small parsers.

## Changelog
- 2026-10-24: first draft.
