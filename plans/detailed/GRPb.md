# GRPb: group-wide rate and group gain
Status: draft   ·   Changelog at the bottom

## Summary
`velocity -g <g> <val>` sets the playback rate of every member of a group in
one atomic batch, through VOX's `VoiceParam` API (README §3.9).
`group <g> --gain <x>` sets the group's own gain (`GroupState.gain`, which
exists today but nothing sets, `engine.rs:592-611`), applied on top of each
member's gain in the mix.

Part b of GRP. Runs after GRPa and after VOX is merged into
`integrate/wave-3` (REQ-017).

## Ownership
- **Modifies:** `commands/group.rs` (`try_group` gains `--gain`; new
  `try_group_velocity`; `GroupOp::Gain`; `HELP`), `engine/group.rs`
  (`GroupState.gain` setter), `engine/handlers/group.rs` (`GroupOp::Gain`).
- **Reads only:** `commands/voice.rs` (VOX's set-param builder),
  `engine/voice.rs` (`VoiceParam`), the mix code that applies group gain
  (REQ-013).

## Registry entries
1. `commands/mod.rs` parse dispatch, in the existing `"velocity"` arm (owned
   by the architect's registry, routing into VOX's `try_velocity`): when the
   first token is `-g` or `--group`, call `self.try_group_velocity(rest)`
   (stub in `commands/group.rs`); otherwise VOX's parser as before.
   ```rust
   "velocity" => match args.split_once(' ') {
       Some(("-g" | "--group", rest)) => self.try_group_velocity(rest),
       _ => self.try_velocity(args),
   },
   ```
2. Group gain in the mix (REQ-013): each member voice's output is multiplied
   by its group's `gain`. If E1 already does this, nothing to add.
3. `HELP` rows (GRP's own file):
   - `("group", "group <g> --gain <x>  set the group's gain (1.0 = unity)")`
   - `("velocity", "velocity -g <g> <rate>  set the rate of every voice in a group")`
4. Completion spec rows for `velocity -g` and `group --gain` (REQ-017).

## Architecture changes
None. Group-wide voice parameters go through README §3.9 as that section
requires.

## Interfaces
- **Provides:** `fn try_group_velocity(&mut self, args: &str) -> CmdResult<Command>`;
  `GroupOp::Gain(f32)` handled by `Conductor::group_edit`.
- **Consumes:** VOX's mirror-side builder that validates a value, updates the
  member's mirror (if VOX mirrors params) and returns the engine command.
  Assumed shape (REQ-013):
  `pub(crate) fn set_param_cmd(state: &mut EngineState, v: VoiceId, p: VoiceParam, val: f32) -> CmdResult<Command>`.
  Validation must happen before any mirror mutation, so GRPb validates the
  value once (same rule VOX uses for `velocity`) and then calls the builder
  for each member.
- **Contract issues:** README §3.9 names `VoiceState::set_param` (engine) but
  no mirror-side builder. GRPb needs one so the mirror stays in sync for
  every member (REQ-013).

## Steps
0. **Refresh** against `integrate/wave-3` after VOX and GRPa merged: VOX's
   actual builder name and the rate param (`VoiceParam::Rate`); where the
   mix applies group gain.
1. **`velocity -g`.** `try_group_velocity`: parse `<g> <val>`; resolve the
   group; parse `val` with VOX's rule; for each member (in `members` order),
   call VOX's builder; return `Command::Batch` (a one-member group returns the
   bare command; an empty group returns `CmdErr` "group has no voices").
2. **`group <g> --gain <x>`.** In `try_group`, a `--gain` form: parse `x`
   (finite, `>= 0`), set `GroupRepr.gain`, return
   `GroupEdit { group, op: GroupOp::Gain(x) }`. Add `gain: f32` to
   `GroupRepr` (default 1.0) for INS's `info`.
3. **Engine.** `GroupOp::Gain(x)` sets `group.state.gain = x`. Clicks: the
   jump is immediate, same as voice gain today; VOX's declick decision
   (brief PI) applies if it introduces a ramp helper GRPb can reuse.
4. Doc comments and `HELP` rows.

## Tests
- `velocity_group_sets_every_member`: `load a,b,c; group g -v a,b;
  velocity -g g 2` → both members' rate is 2.0 (engine `get_param`), `c`
  unchanged; mirror agrees for both.
- `velocity_group_is_one_batch`: parsed command is a `Batch` with one entry
  per member.
- `velocity_group_errors_leave_mirror_unchanged`: `velocity -g nope 2`,
  `velocity -g g x`, `velocity -g g` → errors; mirror untouched.
- `velocity_voice_form_still_works`: `velocity g.a 0.5` and `velocity c 2`
  still reach VOX's parser.
- `group_gain_scales_members`: two members rendering 0.25 each (f32 track
  helper), `group g --gain 0.5`, `start -g g` → output 0.25.
- `group_gain_default_is_unity` and `group_gain_rejects_negative_and_nan`.

## Done when
- Both commands work through `submit` and the engine, and failures leave the
  mirror untouched.
- README §1.4.

## Risks and open questions
- Absolute set vs. relative scaling for `velocity -g`, the mirror builder
  from VOX, and who applies group gain in the mix: REQ-013.
- Group-level modulation (an LFO targeting a group) is LFO's (wave 4); it
  will go through the same per-member path.
- Nice-to-have: `pan -g`, `gain -g` as aliases once VOX settles its verbs.

## Size
S: two small parsers, one engine op, six tests.

## Changelog
- 2026-10-24: first draft (REQ-011, REQ-013, REQ-017 open).
