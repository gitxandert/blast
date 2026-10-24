# TMPb: assign tempos, process tempo target
Status: draft   ·   Changelog at the bottom

## Summary
`assign -v|-g|-p <target> <spec>` gives a voice, group or process a
different tempo: a named context (`c:<tc>`), a group's tempo (`g:<group>`),
its voice's tempo (`v`, processes only), or a fresh tempo of its own
(`b:120`). Objects that *follow* the target move with it, and the target's
old own tempo is freed. `tempo -p <proc> <spec>` retimes a process's own
tempo. Runs after TMPa and PRCa (which provides the process lookup).

## Ownership
- **Modifies:** `commands/tempo.rs` (`try_assign`, `-p` in `try_tempo`,
  `HELP`), `engine/handlers/tempo.rs` (`new_tempo`, `assign_tempo`).
- **Reads only:** `commands/process.rs` (PRCa's `resolve_proc`),
  `commands/{voice,group}.rs` reprs (TMPb mutates only their tempo
  reference fields), `engine/{voice,group}.rs` and the process arena (the
  engine sets their `TempoId` field), `blast_time.rs`.

## Registry entries
1. `commands!` variants (stubs in `commands/tempo.rs`):
   ```rust
   pub struct TempoNewArgs { pub id: TempoId, pub owner: TempoOwner, pub unit: TempoUnit, pub interval: f32 }
   pub struct TempoAssignArgs { pub target: TempoTarget, pub tempo: TempoId }
   #[derive(Clone, Copy, Debug, PartialEq)]
   pub enum TempoTarget { Voice(VoiceId), Group(GroupId), Proc(ProcId) }
   ```
   `TempoOwner` is E1's ownership tag (REQ-011 item 5). If E1 already has a
   generic "create tempo" command (e.g. what `tc` sends), TMPb reuses it and
   `TempoNew` is dropped.
2. Parse dispatch: `"assign" => self.try_assign(args)` (stub).
3. Engine `apply`: `Command::TempoNew(a) => self.new_tempo(a)`,
   `Command::TempoAssign(a) => self.assign_tempo(a)`.
4. `HELP` rows (own file):
   - `("assign", "assign -v <voice>|-g <group>|-p <proc> c:<tc>|g:<group>|v|<unit:interval>  give an object another tempo")`
   - `("tempo", "tempo -p <proc> <unit:interval>  change a process's own tempo")`
5. Completion spec rows for `assign` (REQ-017).

## Architecture changes
None.

## Interfaces
- **Provides:** `fn try_assign(&mut self, args: &str) -> CmdResult<Command>`;
  `Conductor::{new_tempo, assign_tempo}`.
- **Consumes:** TMPa's `tempo_users`; PRCa's
  `pub(crate) fn resolve_proc(state: &EngineState, path: &str) -> CmdResult<ProcId>`;
  E1's follow info and ownership tag (REQ-011 items 4, 5).
- **Contract issues:** none.

## Behaviour (proposed; decided in REQ-014)
`assign <target> <spec>`:
1. Resolve the new tempo: `c:<tc>` → that context; `g:<g>` → group `g`'s
   tempo; `v` → the owner voice's tempo (process targets only; same meaning
   as `seq -t v`); `<unit:interval>` → a fresh tempo owned by the target
   (allocated `TempoId`, `TempoNew`). Assigning a group its own tempo with
   `g:<itself>`, or a voice to a tempo that follows the voice itself
   (cycles), is rejected.
2. **Who moves.** The target, plus everything that follows it:
   - voice target: its processes that follow the voice (`-t v`);
   - group target: members that adopted the group's tempo and their
     voice-following processes, and every object created with `-t g:<g>`;
   - process target: only itself.
   "Follows" is read from the mirror's follow info, not from `TempoId`
   equality, so a voice that happens to share a context with its target is
   not dragged along.
3. **Old tempo.** If the target owned its old tempo, and after the moves
   `tempo_users` reports nobody else, the old tempo is freed (`TempoFree`).
   If someone else still uses it (e.g. a voice loaded with `-t g:<g>` and
   not counted as a follower), it's kept, and becomes a context named after
   the target, using GRPa's promotion rule (REQ-012). If REQ-012 isn't
   accepted, the assign is refused instead, listing the users.
4. **Playing state.** A fresh own tempo assigned to an active voice, group
   or process starts immediately (as a seq added to a playing voice does
   today, `engine.rs:303-311`). Contexts and group tempos keep their own
   state.
5. The engine work is one atomic `Command::Batch`:
   `[TempoNew?, TempoAssign × (target + followers), TempoFree?]`.

`tempo -p <proc> <spec>`: like `-v`, the process must own its tempo.

## Steps
0. **Refresh** against `integrate/wave-3` after TMPa and PRCa merged:
   `resolve_proc`, the follow info, the ownership tag, and whether E1 has a
   generic create-tempo command.
1. **Spec parsing.** `fn parse_assign_spec(&self, target, s) -> CmdResult<NewTempo>`
   with `enum NewTempo { Existing(TempoId, FollowKind), Fresh(TempoUnit, f32) }`.
   Reuse TMPa's unit/interval parser.
2. **Followers.** `fn followers(state, target) -> Vec<TempoTarget>` from the
   mirror's follow info, per the list above.
3. **`try_assign`.** Validate everything (target exists, spec valid, no
   cycle, capacity for a fresh `TempoId`) before mutating; then update the
   reprs' tempo references (target and followers), allocate/free IDs, and
   build the batch.
4. **`tempo -p`.** In `try_tempo`, resolve with `resolve_proc`; same
   own-tempo check as `-v`.
5. **Engine.** `new_tempo`: insert a `TempoState` (no allocation: the arena
   slot is preallocated; `TempoState` is plain data). `assign_tempo`: set the
   target's `TempoId`; if the new tempo is owned by the target and the
   target is active, `start()` it. Seqs notice the switch through TMPa's
   unique epochs.
6. `HELP` rows, doc comments.

## Tests
- `assign_voice_to_context`: `load kick -t s:1000; tc clk s:500;
  assign -v kick c:clk` → engine voice tempo is `clk`'s ID; kick's old tempo
  is gone from the arena and its ID is free.
- `assign_moves_voice_following_seq`: `seq kick -t v -s 0` follows kick to
  `clk`; `seq kick -t b:90 -s 0` (own tempo) doesn't move.
- `assign_doesnt_drag_context_sharers`: `load a -t c:clk; load b -t c:clk;
  assign -v a b:90` → `b` still on `clk`.
- `assign_group_moves_adopted_members`: `group g -t s:100 -v a,b` (`b` loaded
  with its own tempo) → after `assign -g g c:clk`, `a` and the group use
  `clk`, `b` keeps its own.
- `assign_group_moves_named_group_followers`: `load d -t g:g` moves too; the
  old group tempo is freed.
- `assign_fresh_spec_starts_on_active_voice`: playing kick, `assign -v kick
  s:250` → new tempo active, ticking.
- `assign_proc_and_tempo_p`: after PRCa, `seq kick -n hats -s 0;
  assign -p kick.hats c:clk`; `tempo -p kick.hats s:10` rejected (follows
  clk); `assign -p kick.hats s:100; tempo -p kick.hats s:10` works.
- `assign_running_seq_doesnt_misfire`: seq at 2.5 steps on its own tempo
  `s:1000`, assigned to a context at 0.2 steps → no trigger in the first
  frame after the switch, then fires on the context's grid.
- `assign_rejects_cycles_and_bad_specs`: `assign -g g g:g`, `assign -v kick v`,
  `assign -v nope c:clk`, `assign -v kick c:nope`, `assign -v kick b:0` →
  errors; mirror untouched (compare a snapshot of every repr's tempo ref).

## Done when
- After any `assign`, every `TempoId` in the mirror and engine refers to a
  live tempo, and the two agree (a test helper walks both and compares).
- README §1.4.

## Risks and open questions
- Followers-move semantics, refusing `tempo -v` on a shared tempo: REQ-014.
- Old own tempo still used by a non-follower: depends on REQ-012's promotion
  rule.
- Positional sugar from `notes.txt` (`assign organ tg`) is ambiguous between
  voices, groups and processes; left as a nice-to-have once `all`/name
  resolution rules settle (REQ-006).

## Size
M: one parser with a validation pass, a follower walk, two engine ops,
~9 tests. Dispatched after PRCa merges (REQ-017).

## Changelog
- 2026-10-24: first draft (REQ-011, REQ-012, REQ-014, REQ-017 open).
