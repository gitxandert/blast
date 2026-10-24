# TMPa: retime tempos, remove tempo contexts
Status: draft   ·   Changelog at the bottom

## Summary
Tempos can be changed while playing without a jump: `tempo -t <tc> <spec>`,
`tempo -v <voice> <spec>` and `tempo -g <group> <spec>` set a new interval
and unit, and the step position carries on exactly where it was. `tc --rm
<name>` removes a tempo context, and is refused (listing the users) while
anything still uses it. `TempoState::set_interval` (unused today) is
replaced by a phase-preserving `retime`.

Part a of TMP. TMPb adds `assign` and the `-p` (process) target, which need
PRCa's process lookup.

## Ownership
- **Modifies:** `blast_time.rs` (`TempoState`: anchors, `retime`, epochs),
  `commands/tempo.rs` (`try_tc` gains `--rm`; new `try_tempo`,
  `tempo_users`, `TempoRepr` fields as needed; `HELP`),
  `engine/handlers/tempo.rs` (`retime_tempo`, `free_tempo`).
- **Reads only:** `commands/reprs.rs`, `commands/voice.rs`,
  `commands/group.rs`, `commands/process.rs` (the reprs, to find users),
  `engine/mod.rs` (tempo arena), `processes/seq.rs` (to check `Seq::fire`).

## Registry entries
1. `commands!` variants (args stubs in `commands/tempo.rs`):
   ```rust
   pub struct TempoSetArgs { pub id: TempoId, pub unit: TempoUnit, pub interval: f32 }
   pub struct TempoFreeArgs { pub id: TempoId }
   ```
   → `Command::TempoSet(TempoSetArgs)`, `Command::TempoFree(TempoFreeArgs)`.
2. Parse dispatch: `"tempo" => self.try_tempo(args)` (stub in
   `commands/tempo.rs`). The `"tc" | "tempocon"` arm is unchanged
   (TMPa extends `try_tc`).
3. Engine `apply`: `Command::TempoSet(a) => self.retime_tempo(a)`,
   `Command::TempoFree(a) => self.free_tempo(a)` (stubs in
   `engine/handlers/tempo.rs`).
4. `commands/errors.rs`: `CmdErr::InUse { ty: String, name: String, users: String }`
   with `Display` "Can't remove {ty} '{name}': used by {users}". (If C1 merged
   `StateErr` into `CmdErr`, only the one enum.)
5. `HELP` rows (own file):
   - `("tc", "tc <name> <unit:interval>  create a tempo context (units b|bpm, m|ms, s|smp)")`
   - `("tc", "tc --rm <name>  remove a tempo context nothing uses")`
   - `("tempo", "tempo -t <tc>|-v <voice>|-g <group> <unit:interval>  change a tempo; keeps the beat position")`
6. Completion spec rows for `tempo` and `tc --rm` (REQ-017).

## Architecture changes
None.

## Interfaces
- **Provides**
  - `impl TempoState { pub fn retime(&mut self, unit: TempoUnit, interval: f32); pub fn steps(&self) -> f64; }`
    (`set_interval` is deleted; it has no callers, `blast_time.rs:157-160`).
  - `pub(crate) enum TempoUser { Voice(VoiceId), Group(GroupId), Proc(ProcId) }`
    and `pub(crate) fn tempo_users(state: &EngineState, id: TempoId) -> Vec<TempoUser>`
    plus `fn describe_users(state, &[TempoUser]) -> String` ("voice kick, seq
    kick.hats, group g"). TMPb and INS use these.
  - `fn try_tempo(&mut self, args: &str) -> CmdResult<Command>`.
- **Consumes:** E1's tempo arena, the "follow" info on each tempo reference
  and the ownership tag (REQ-011 items 4, 5, 11), and the name indexes.
- **Contract issues:** none.

## Phase rule (precise)
After E1, a tempo is ticked once per frame (`update(1.0)`), `current: u64`
counts samples since its last reset, and `interval: f32` is samples per
step. Today the step position is `current / interval`
(`blast_time.rs:122-131`), so changing `interval` mid-play rescales the
whole elapsed time: at 2.5 steps of 1000 samples, switching to 500 would
jump to step 5.0. TMPa adds an anchor:

```rust
pub struct TempoState {
    // … E1's fields …
    pub current: u64,        // samples since the last reset (unchanged)
    anchor_samples: u64,     // `current` at the last retime
    anchor_steps: f64,       // step position at the last retime
}
pub fn steps(&self) -> f64 {
    self.anchor_steps + (self.current - self.anchor_samples) as f64 / self.interval as f64
}
pub fn current(&self) -> f32 { self.steps() as f32 }
pub fn current_in(&self, period: f32) -> f32 { (self.steps() % period as f64) as f32 }
pub fn retime(&mut self, unit: TempoUnit, interval: f32) {
    self.anchor_steps = self.steps();
    self.anchor_samples = self.current;
    self.unit = unit;
    self.interval = convert_interval(&unit, interval);
}
// reset(): current = 0, anchor_samples = 0, anchor_steps = 0.0, new epoch
```

- At the retime instant `steps()` is identical before and after (both terms
  are evaluated from the same `current`), so the position doesn't move.
  From the next frame it advances at `1/new_interval` steps per sample.
- With no retime the anchors are zero and every formula reduces to today's,
  so `current_in_stays_exact_after_hours` still holds.
- Retime doesn't bump `epoch` (it isn't a restart) and doesn't touch
  `active`. Retiming a paused tempo is allowed.
- **`Seq::fire` stays correct.** Its `last` was taken at the previous frame
  as `current_in(period)`. After the retime the next value is `last +
  1/new_interval` (mod period): strictly forward, never smaller unless the
  period genuinely wraps. So no step fires twice, none is skipped, and a
  wrap is still detected by `current < last` (`processes.rs:127-145`). The
  "one sample earlier" fallback (`processes.rs:121-124`) uses the *current*
  interval, which is right for the first frame after a reset.
- **Epochs become unique across tempos.** `new()` and `reset()` take their
  epoch from a global `AtomicU32` counter (`fetch_add(1, Relaxed)`, lock-
  free, fine on the audio thread). Today two tempos can share an epoch
  value, so a seq moved to another tempo (GRP adoption, TMPb `assign`) can
  miss the switch and read the position change as a wrap. With unique
  epochs, `fire`'s existing check (`processes.rs:112-116`) catches every
  switch without any change in `processes/seq.rs`.
- Validation (mirror): interval must be finite and `> 0`; units `b|bpm`,
  `m|ms`, `s|smp`.

## Steps
0. **Refresh** against `integrate/wave-3`: E1's `TempoState` fields, where
   ticking happens, whether C1/E1 centralised tempo-spec parsing in
   `commands/tempo.rs` (if so, the unit aliases go there and apply to every
   `-t`), and the names of the "follow" info and ownership tag.
1. **`blast_time.rs`.** Add the anchors, `steps()`, `retime`, unique epochs;
   rewrite `current`/`current_in` on top of `steps()`; delete
   `set_interval` and replace its test. All existing tempo tests stay green.
2. **`tempo_users`.** Scan voices, groups and processes in the mirror for
   references that resolve to `id`. Order the result by kind then name so
   error messages are stable.
3. **`tc --rm <name>`.** In `try_tc`: if the first token is `--rm`, resolve
   the context, call `tempo_users`; non-empty → `CmdErr::InUse` (mirror
   untouched); else remove the `TempoRepr`, its name, free the `TempoId`,
   return `TempoFree { id }`. Rewrite the rest of `try_tc` in the §1.3 style
   while there (`commands.rs:824-879` today).
4. **`tempo` command.** `try_tempo`: `-t|--tempocontext <tc>`,
   `-v|--voice <v or g.v>`, `-g|--group <g>`, then a spec. For `-v`/`-g`,
   the target must *own* its tempo (ownership tag); otherwise
   `CmdErr::Formatting` explaining what it follows and pointing to
   `tempo -t <tc>` or `assign`. Update the `TempoRepr` (unit, interval as
   typed), return `TempoSet`. `-p` is rejected here with "not yet" and
   added in TMPb.
5. **Engine.** `retime_tempo`: `tempos.get_mut(id)` → `retime(unit, interval)`.
   `free_tempo`: `tempos.remove(id)`; `TempoState` holds no heap data, so it
   drops in place without the garbage queue (confirm against E1's
   `TempoState`; if it ever holds heap data, push it to garbage instead).
6. `HELP` rows, doc comments.

## Tests
`blast_time.rs`:
- `retime_keeps_position`: s:100, 250 updates (2.5 steps), retime s:200 →
  `steps() == 2.5`; 100 more updates → 3.0.
- `retime_changes_unit`: b:120 → m:250; interval 24000 → 12000 samples at
  48k; position kept.
- `retime_while_paused_then_resume`: position kept; advances at the new rate
  after `resume`.
- `reset_clears_anchors`: retime, then `stop`/`start` → `steps() == 0`.
- `current_in_wraps_after_retime`: period 4, retime at 3.5, advance past 4 →
  wraps to the right fraction.
- `epochs_are_unique_across_tempos`: two fresh tempos, `start` each → all
  four epochs distinct.
- `current_in_stays_exact_after_hours` (existing) unchanged.

Mirror + engine (`test_util::setup`/`run`, 1 output channel):
- `seq_keeps_phase_when_context_speeds_up`: `tc clk s:1000; load kick;
  seq kick -t c:clk -s 0,2; start -t clk; start -v kick`; render 2500
  (triggers 0, 2000); `tempo -t clk s:500`; render 2000 → triggers at 750
  and 1750 (±1 frame, `assert_near` style from `processes.rs:310-315`).
- `seq_keeps_phase_when_context_slows_down`: same, `s:2000` → next trigger
  at 3000.
- `retime_right_after_a_step_doesnt_refire`: retime one frame after the step
  at 2000 → no trigger in the next 100 frames.
- `tempo_voice_own`: `load kick -t s:1000; tempo -v kick s:500` → engine
  interval 500, mirror updated.
- `tempo_voice_following_context_is_rejected`: `load kick -t c:clk;
  tempo -v kick b:90` → error mentions `clk`; mirror untouched.
- `tempo_group_own`: `group g -t s:100 -v a; tempo -g g s:50`.
- `tempo_parse_errors_leave_mirror_unchanged`: `tempo`, `tempo -t nope b:1`,
  `tempo -t clk b:0`, `tempo -t clk b:-3`, `tempo -t clk x:1`,
  `tempo -t clk b:fast`.
- `tempo_unit_aliases`: `tc a bpm:120`, `tc b ms:500`, `tc c smp:100`.
- `tc_rm_unused`: removed from mirror and arena; name reusable; the ID is
  free.
- `tc_rm_used_is_rejected`: `load kick -t c:clk; seq kick -t c:clk -s 0` →
  `CmdErr::InUse` listing `voice kick` and the seq; mirror untouched.
- `tc_rm_unknown`: `CmdErr::NoItem`.

## Done when
- Retiming any owned tempo or context mid-play produces no jump and no
  double or missed seq step (tests above).
- `tc --rm` never leaves a dangling `TempoId` in the mirror or engine.
- README §1.4.

## Risks and open questions
- REQ-011 items 4, 5, 11 (follow info, ownership tag, once-per-frame tick).
- `tempo -v` on a voice that follows a context is refused rather than
  changing the shared context; confirmed in REQ-014 together with `assign`.
- **Nice-to-have (not in the steps): tempo ramps** (`tempo -t clk b:140
  --over 8` for an accelerando over 8 steps). With the anchor model it's an
  interval that changes per frame plus a re-anchor each frame, about 40
  lines and a per-frame branch in `update`. Proposed as a follow-up task.
- Removing a context by force (`tc --rm --force` reassigning users) is left
  out.

## Size
M: one struct change with exact maths, two small parsers, two engine ops,
~19 tests.

## Changelog
- 2026-10-24: first draft (REQ-011, REQ-014, REQ-017 open).
