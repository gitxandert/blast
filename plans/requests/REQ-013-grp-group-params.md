---
id: REQ-013
type: question
from: planner (PH-engine-commands)
tasks: [GRPb, VOX, E1c]
blocks: [GRPb]
status: open
created: 2026-10-24
---

## Request
Three linked points for group-wide parameters: (a) what `velocity -g` means,
(b) a mirror-side set-param builder from VOX, (c) who applies `GroupState.gain`
in the mix. Touches the README §3.9 contract, so it needs an architect review.

## Context
`plans/detailed/GRPb.md`. The brief says `velocity -g` "probably scales
every member, or becomes a group-level rate", and that GRP must go through
`VoiceParam` instead of editing `voice.rs`. `GroupState.gain` exists
(`engine.rs:592-611`) but nothing reads or sets it; in wave 3 nobody owns
the mix code (`engine/render.rs` is E1's in wave 2 and REC's in wave 4).

## Options
**(a) `velocity -g <g> <val>`**
1. **Set every member's rate to `val`**, as one atomic `Command::Batch` of
   VOX set-param commands. Same meaning as `velocity <v> <val>`. (Recommended)
2. **Multiply every member's current rate by `val`**: useful for "speed the
   whole group up", but not idempotent and members drift if repeated.
3. **A group-level rate multiplier** read by `Voice::process`: one value, but
   it needs a hook in VOX's `engine/voice.rs` and changes what "rate" means
   for grouped voices.

**(b) Mirror builder.** VOX exposes
`pub(crate) fn set_param_cmd(state: &mut EngineState, v: VoiceId, p: VoiceParam, val: f32) -> CmdResult<Command>`
(or equivalent) in `commands/voice.rs`, so group-wide and modulation commands
keep the mirror in sync without duplicating VOX's validation. (Recommended;
README §3.9 then names it next to `VoiceState::set_param`.)

**(c) Group gain in the mix.**
1. **E1 applies it**: the render loop multiplies each grouped voice's output
   by its group's `gain` (one multiply; E1 is porting `GroupState` anyway).
   (Recommended)
2. **The architect adds it as a wave-3 registry entry** in the render loop.
3. **GRPb fakes it** by scaling each member's `Gain` param: no engine hook,
   but it overwrites per-voice gain.

## Impact
GRPb (small either way), VOX (one `pub(crate)` function), E1 (one multiply
in render). (a)3 or (c)3 would change VOX's semantics.

## Architect review

## Decision
