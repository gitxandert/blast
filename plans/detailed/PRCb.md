# PRCb: group seqs; enable, disable and remove processes
Status: draft   ·   Changelog at the bottom

## Summary
`seq -g <group> …` creates a seq owned by a group, which retriggers every
member on each step (the engine already runs these, `processes.rs:83-91`;
only parsing and the mirror were missing). `proc --off <path>`,
`proc --on <path>` and `proc --rm <path>` disable, re-enable and remove a
process by its stable `ProcId`; removal goes through the garbage queue.
Resolves the `SeqState.active` TODO (`processes.rs:62`).

Runs after PRCa.

## Ownership
- **Modifies:** `commands/process.rs` (`try_proc`, `ProcRepr.enabled`,
  `HELP`), `commands/procs/seq.rs` (group target), `processes/seq.rs`
  (`Seq::set_enabled`), `engine/handlers/process.rs` (`proc_edit`).
- **Reads only:** `commands/group.rs` (`GroupRepr`; PRCb inserts into and
  removes from its process list only), `commands/voice.rs` (same for
  `VoiceRepr`), `engine/mod.rs` (process arena, garbage).

## Registry entries
1. `commands!` variant `ProcEdit` with args (stub in `commands/process.rs`):
   ```rust
   pub struct ProcEditArgs { pub id: ProcId, pub op: ProcOp }
   #[derive(Clone, Copy, Debug, PartialEq)]
   pub enum ProcOp { Enable, Disable, Remove }
   ```
2. Parse dispatch: `"proc" => self.try_proc(args)` (stub).
3. Engine `apply`: `Command::ProcEdit(a) => self.proc_edit(a)` (stub in
   `engine/handlers/process.rs`).
4. `processes!` macro (`processes/mod.rs` after E1): a dispatch method
   `pub fn set_enabled(&mut self, on: bool)` calling each variant's
   `set_enabled`, with a stub `Seq::set_enabled` in `processes/seq.rs`.
   Wave-4 processes (LFO, ENV) must implement it too.
5. `Garbage::Process(Process)` if E1 lacks it.
6. `HELP` rows (own files):
   - `("seq", "seq -g <group> [flags]  a seq that retriggers every voice in the group")`
   - `("proc", "proc --on|--off|--rm <owner.proc>  enable, disable or remove a process")`
7. Completion spec rows for `proc` and `seq -g` (REQ-017).

## Architecture changes
None.

## Interfaces
- **Provides:** `fn try_proc(&mut self, args: &str) -> CmdResult<Command>`;
  `Conductor::proc_edit(ProcEditArgs)`; `Seq::set_enabled(bool)`;
  `ProcRepr.enabled: bool` (for INS).
- **Consumes:** PRCa's `resolve_proc`, `resolve_owner`, `proc_path`;
  E1's process arena with owners (REQ-011 item 10); TMPa's `tempo_users`
  is *not* needed (a process's own tempo has no other users: nothing can
  reference it, `-t` only names voices, groups and contexts).
- **Contract issues:** none.

## Behaviour
- **`seq -g <g>`.** Same flags as a voice seq. Tempo: default is an own
  tempo (120 BPM, as for voices); `-t c:<tc>` and `-t g:<name>` work;
  `-t v` is rejected for a group seq ("a group seq has no voice; use
  `-t g:<g>`"). The positional voice form is unchanged.
- **`proc --off <path>`**: the process stops acting (a seq stops
  retriggering) but keeps its tempo running, so `--on` lands back on the
  same grid.
- **`proc --on <path>`**: the process acts again from the next step
  crossing. For a seq this clears `last`, so steps passed while it was off
  don't all fire at once (today `fire` returns before updating `last`
  while inactive, `processes.rs:98-99`).
- **`proc --rm <path>`**: the process and its own tempo are removed; the
  name is free again.
- Voice/group `start`/`stop` reset processes but never change `enabled`.

## Steps
0. **Refresh** against `integrate/wave-3` after PRCa: E1's seq target type
   in `SeqArgs`, the process arena API, whether group seqs already exist on
   the engine side.
1. **`seq -g`.** In `try_seq`, accept `-g|--group <g>` as the first token
   (the positional voice form stays); resolve to `ProcOwner::Group`; reject
   `-t v`; insert the `ProcRepr` into the group's process list. Mirror
   order and validation as for voices.
2. **`Seq::set_enabled`.** `active = on`; when turning on, `last = None`
   (and `idx = 0`). Implement the macro dispatch stub.
3. **`try_proc`.** Flags `--on|--off|--rm` then a path; resolve with
   `resolve_proc`. `--on/--off` set `ProcRepr.enabled` (no-op if already in
   that state, still sent so the engine agrees). `--rm`: remove the
   `ProcRepr` from its owner, free its `ProcId` and its own `TempoId`.
4. **Engine `proc_edit`.** Enable/Disable → `procs.get_mut(id)?.set_enabled(..)`.
   Remove → `procs.remove(id)` into `Garbage::Process`; if its tempo is
   owned by the process, `tempos.remove(tempo_id)`.
5. `HELP`, doc comments; delete the `TODO: impl activation methods`.

## Tests
- `group_seq_retriggers_all_members` (through the parser): `load a,b; group g -v a,b;
  seq -g g -t s:1000 -s 0,2; start -g g` → both voices retrigger at 0, 2000,
  4000 (port of `engine.rs:971-1017`'s helper, now via `run`).
- `group_seq_on_group_tempo`: `group g -t s:500 -v a; seq -g g -t g:g -s 1`.
- `group_seq_rejects_voice_tempo`: `seq -g g -t v -s 0` → error, mirror
  untouched.
- `group_seq_named_and_addressable`: `seq -g g -n beat -s 0` →
  `resolve_proc("g.beat")` works.
- `proc_off_stops_triggers_on_resumes_grid`: seq on s:1000 steps 0..3;
  `--off` at 1500; no triggers until `--on` at 3500; next trigger at 4000
  (not a burst at 3500).
- `proc_off_keeps_tempo_running`: the seq's own tempo `current` keeps
  advancing while off.
- `proc_rm_frees_everything`: process slot empty, its tempo slot empty, IDs
  free, garbage saw one process; `seq kick -n hats …` can reuse the name.
- `proc_errors_leave_mirror_unchanged`: `proc`, `proc --on`,
  `proc --on nope.x`, `proc --zap kick.seq`.
- `start_doesnt_reenable`: `--off`, then `start -v kick` → still off.

## Done when
- The `active` TODO is gone and every process can be addressed, toggled and
  removed by path.
- No allocation or heap drop in `proc_edit` (removed process goes to
  garbage).
- README §1.4.

## Risks and open questions
- REQ-011 item 10 (process arena with owners).
- Nice-to-have: `proc --on all` / per-owner bulk toggles once `all` is
  decided (REQ-006); `proc --mv` to move a process to another owner.

## Size
M: one parser branch, one new command family verb, one engine op, ~9 tests.

## Changelog
- 2026-10-24: first draft (REQ-011, REQ-017 open).
