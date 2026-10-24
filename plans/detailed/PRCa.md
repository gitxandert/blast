# PRCa: process lookup and seeds
Status: draft   ·   Changelog at the bottom

## Summary
One generic lookup turns a process path (`voice.proc`, `group.proc`,
`group.voice.proc`) into a `ProcId`, and one owner lookup turns a target
name into a voice or group. `seq` uses them instead of its hand-rolled
lookup, and `ProcRepr.owner` stops being dead code. `seq --seed <u64>`
makes a seq's chance pattern reproducible; the seed actually used is kept in
the mirror. `fast_seed` loses its `unsafe` (README §1.2: no `unsafe` after
wave 3).

Part a of PRC: small, and first in PH's wave-3 order because TMPb and PRCb
use the lookup (REQ-017).

## Ownership
- **Modifies:** `commands/process.rs` (`ProcRepr`, `resolve_proc`,
  `resolve_owner`, seed helpers, `HELP`), `commands/procs/seq.rs`
  (`try_seq`'s target lookup and `--seed`; the TODOs moved there by C1 from
  `commands.rs:881-884, 899, 1376`), `blast_rand.rs`.
- **Reads only:** `commands/reprs.rs`, `commands/{voice,group}.rs` reprs.
- `processes/seq.rs` and `engine/handlers/process.rs` are untouched in PRCa.

## Registry entries
1. `HELP` row (own file, `commands/procs/seq.rs` or wherever C1 put seq's
   rows): extend the `seq` usage with `[--seed <u64>]`.
2. Completion spec: `--seed` (free value) on `seq` (REQ-017).
None in shared files.

## Architecture changes
None.

## Interfaces
- **Provides** (in `commands/process.rs`):
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq)]
  pub(crate) enum ProcOwner { Voice(VoiceId), Group(GroupId) }   // or E1's equivalent
  /// "v.p", "g.p" or "g.v.p" → the process. Errors: NoItem, or Formatting when
  /// "x.p" matches both voice x's and group x's process p.
  pub(crate) fn resolve_proc(state: &EngineState, path: &str) -> CmdResult<ProcId>;
  /// "v", "g.v" → a voice owner. (PRCb adds the group form via -g.)
  pub(crate) fn resolve_owner(state: &EngineState, name: &str) -> CmdResult<ProcOwner>;
  /// "<owner path>.<proc name>", for messages and INS.
  pub(crate) fn proc_path(state: &EngineState, id: ProcId) -> String;
  /// Parses --seed's value (decimal u64).
  pub(crate) fn parse_seed(raw: &str, cmd: &str) -> CmdResult<u64>;
  /// The seed to use: the given one, or a fresh one from fast_seed().
  pub(crate) fn pick_seed(given: Option<u64>) -> u64;
  ```
  `ProcRepr` gains `seed: u64` (the seed used, for `info` and for
  reproducing a run); `owner` loses `#[allow(dead_code)]`
  (`commands.rs:291-293`).
- **Consumes:** E1's name index and reprs (REQ-011 items 2, 6).
- **Contract issues:** none. LFO (wave 4, sample-and-hold) is expected to
  seed through `parse_seed`/`pick_seed` (brief PI: "seeded the way PRC
  defines").

## Steps
0. **Refresh** against `integrate/wave-3`: where C1/E1 put `ProcRepr`, the
   seq target lookup, and the moved TODOs.
1. **`blast_rand.rs`.** Replace the three `fast_seed` variants
   (`blast_rand.rs:65-88`; two use `unsafe`) with one safe function:
   `RandomState::new().build_hasher()` fed a global `AtomicU64` counter and
   `SystemTime::now()` nanoseconds. It runs on the REPL thread only (at
   parse time); say so in its doc comment. Replace
   `next_i64_range`'s `try_into().unwrap()` with `upper.abs_diff(lower)`.
   Doc comments on the `pub` items.
2. **Lookups.** Implement `resolve_proc`, `resolve_owner`, `proc_path` on
   top of E1's name index. Path rules: split on `.`; 3 parts → group, member,
   process; 2 parts → try voice `x` and group `x`, error if both have a
   process `p`; 1 part → `Formatting` ("a process is addressed as
   owner.name").
3. **`seq` uses them.** Replace the voice lookups in `try_seq`
   (`commands.rs:898-902` and `:1376-1378` today) with `resolve_owner`.
   Delete the resolved TODOs (`:881-884` naming is already done by
   `-n`; `:899` and `:1376` become PRCb's group form; leave one `TODO(PRCb)`
   line only if PRCb isn't merged in the same wave).
4. **`--seed <u64>`.** New flag in `try_seq`; `SeqArgs.rng =
   X128P::new(pick_seed(given))`; store the seed in `ProcRepr.seed`.
5. Doc comments, `HELP`.

## Tests
- `resolve_voice_proc`, `resolve_group_member_proc`,
  `resolve_group_proc` (group proc built with a hand-made `Command` until
  PRCb, as in GRPa's tests).
- `resolve_ambiguous_path_errors`: voice `x` and group `x` both with process
  `seq` → `Formatting` naming both.
- `resolve_errors`: `nope.seq`, `kick.nope`, `kick`, `a.b.c.d` → errors.
- `proc_path_round_trips`: `resolve_proc(proc_path(id)) == id` for all three
  forms.
- `seq_seed_is_reproducible`: two processors, `seq kick -s 0,1,2,3
  -c a:50 --seed 42` on both; render 8 periods each → identical trigger
  lists.
- `seq_seed_recorded`: `ProcRepr.seed == 42`; without `--seed`, a seed is
  still recorded and two seqs get different ones.
- `seq_seed_errors`: `--seed`, `--seed x`, `--seed -1` → errors, mirror
  untouched.
- `fast_seed_varies`: 1000 calls, all distinct.
- `next_i64_range_bounds` for `(0,100)`, `(100,0)`, `(5,5)`.
- All existing `seq_tests` (`commands.rs:2185-2432`) pass unchanged.

## Done when
- `blast_rand.rs` contains no `unsafe`.
- `try_seq` has no hand-rolled lookup; `ProcRepr.owner` is read.
- README §1.4.

## Risks and open questions
- REQ-011 item 6 (`ProcRepr` shape).
- `fast_seed` becomes slower (tens of ns → ~100 ns). It runs once per
  `seq` command on the REPL thread, so it doesn't matter.

## Size
S: three small functions, one flag, one rewrite of an 88-line file, ~12
tests.

## Changelog
- 2026-10-24: first draft (REQ-011, REQ-017 open).
