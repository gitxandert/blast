# EUC: seq pattern generators
Status: draft   ·   Changelog at the bottom

## Summary
Afterwards `seq` can generate its steps instead of having them listed:
Euclidean rhythms (`-e k,n[,rot]`, Bjorklund), regular pulses (`--every n`),
random patterns (`--random <density>`), and a shift of whatever steps exist
(`--offset o`). The result is ordinary `SeqArgs.steps`, so the engine doesn't
change. The generated steps work with `-c` chance and `-j` jitter exactly like
hand-written `-s` steps.

## Ownership
- **Modifies:** `commands/procs/seq.rs`, step generation only: the new
  generator functions and their flag arms in the `seq` argument loop, plus a
  pre-pass. Everything PRC built in wave 3 (owner, tempo, name, seed, edit in
  place) is read only.
- **Reads only:** `blast_rand.rs`, `commands/process.rs`.

## Registry entries
None.

## Architecture changes
None.

## Interfaces
**Provides** (private to `commands/procs/seq.rs`, with `pub(crate)` on
`euclid` for tests and future `pattern` commands):
```rust
/// Bjorklund's algorithm: k onsets spread as evenly as possible over n steps, rotated left by rot.
pub(crate) fn euclid(k: usize, n: usize, rot: isize) -> Vec<usize>;       // sorted onset positions in 0..n
fn tile(cycle: &[usize], n: usize, period: usize) -> Vec<f32>;             // repeat the cycle every n steps while < period
fn every(n: f32, period: usize) -> Vec<f32>;                               // 0, n, 2n, … < period
fn random(density: f32, period: usize, rng: &mut X128P) -> Vec<f32>;       // each integer step kept with probability density/100
fn offset(steps: &mut [f32], o: f32, period: usize);                       // (s + o).rem_euclid(period), in place
```
**Consumes:** `SeqArgs` and the `seq` flag loop as PRC leaves them; PRC's
`--seed` (REQ-020).

## Steps
1. **Pre-pass.** Before the flag loop, collect the arguments into a
   `Vec<&str>` and scan for the last `-p/--period` and `--seed` values (the
   same "last one wins" rule as today). Generators need the period and the
   rng at the point where they appear, and `-p` may come later on the line.
   The main loop still parses `-p`/`--seed` with full error reporting. The
   pre-pass only peeks, and ignores parse errors.
2. **`euclid`**: Bjorklund's pairing form.
   - Start with `k` sequences `[1]` and `n-k` sequences `[0]`.
   - While the remainder group has more than one sequence, append one
     remainder sequence to each head sequence (pairwise, as many as fit).
     Whatever is left over becomes the new remainder.
   - Concatenate the heads, then the remainder. Collect the indices of the 1s,
     then rotate: `(pos - rot).rem_euclid(n)`, and sort.
   - Errors (returned by the flag arm, not panics): `n == 0`, `k == 0`,
     `k > n`, and `n > 1024`.
3. **Flag arms**, each of which **replaces** the current steps and resets
   `chance`/`jit` to their defaults, exactly like a repeated `-s` (today
   `commands.rs:1019-1039`):
   - `-e/--euclid k,n[,rot]`: `steps = tile(euclid(k,n,rot), n, period)`. If
     no `-p` appears anywhere on the line, the period becomes `n`.
   - `--every n`: requires `0 < n`.
   - `--random d`: `d` in `0..=100`, the same scale as `-c`. It uses the rng
     seeded from `--seed` (pre-pass), else `fast_seed()`. This is a different
     rng *instance* from the Seq's runtime chance rng, seeded from the same
     seed with `seed ^ 0x5EED_0E0C` so pattern and chance don't correlate. An
     empty result is an error that tells the user to raise the density or
     change the seed.
   - `--offset o`: needs existing steps (the same error as `-c` before `-s`).
     Shifts them in place, so `chance`/`jit` stay attached to their steps.
     Fractional values are allowed.
   Update the "Must provide arguments to -s/--steps before -c…" message to
   mention `-e/--every/--random` (today `commands.rs:1050-1054`, `:1255-1258`).
4. Sort nothing: `Seq::fire` handles any order, and keeping generator order
   keeps `-c`'s positional values predictable (step `i` of the printed
   pattern).
5. `HELP` text for the new flags, in the `seq` row (`commands/procs/seq.rs`'s
   HELP table).
6. Run `cargo fmt`, `cargo clippy` and `cargo test`.

## Tests
In `commands/procs/seq.rs`'s tests:
- `euclid_matches_toussaint`: E(2,5)=`x.x..`, E(3,4)=`x.xx`,
  E(3,8)=`x..x..x.`, E(4,9)=`x.x.x.x..`, E(5,8)=`x.xx.xx.`,
  E(5,12)=`x..x.x..x.x.`, E(7,16)=`x..x.x.x..x.x.x.`,
  E(13,24)=`x.xx.x.x.x.x.xx.x.x.x.x.` (Toussaint,
  "The Euclidean Algorithm Generates Traditional Musical Rhythms", 2005).
  Patterns are written as strings and compared after conversion.
- `euclid_edges`: E(n,n) is every step, E(1,n)=`[0]`; `k > n`, `k = 0` and
  `n = 0` are errors.
- `euclid_rotation`: E(3,8,1) = `[2,5,7]`, E(3,8,-1) = `[1,4,7]`, rot ≡ rot mod n.
- `euclid_sets_period_without_p`: period 8.
- `euclid_tiles_to_explicit_period`: `-p 16 -e 3,8` gives
  `[0,3,6,8,11,14]`, and `-e 3,8 -p 16` (p after) gives the same.
- `every_and_offset`: `-p 16 --every 4 --offset 2` gives `[2,6,10,14]`, and
  `--offset 15` wraps.
- `random_is_seeded`: the same `--seed` gives the same steps, and densities 0
  and 100 hit the edges (0 → error, 100 → all).
- `generator_replaces_steps`: `-s 1,2 -e 2,4` gives `[0,2]`.
- `chance_indexes_generated_steps`: `-e 3,8 -c 50,_,25` gives chance
  `[50,100,25]`, and `-e 3,8 -c 3:10` sets step 3.
- `jitter_indexes_generated_steps`.
- `offset_before_steps_errors`.
- `generated_seq_plays`: end to end, `seq kick -t s:100 -e 3,8` plus render.
  Triggers land at frames 0, 300 and 600.

## Done when
- README §1.4 holds.
- Every generator's result is plain steps; no engine or `SeqArgs` changes.
- Every pre-existing `seq_tests` test passes unchanged.

## Risks and open questions
- Depends on PRC's final shape of the `seq` loop and `--seed` (REQ-020). The
  pre-pass is the only structural change, and it's local.
- **Rotation direction** follows the Tidal/Strudel convention (rotate left:
  the pattern starts at its `rot`-th step). The user sees this through plan
  approval.
- **Union of generators** (`-e 3,8 --every 4` adding up) is deliberately not
  supported: generators replace, like `-s`. That's a follow-up if wanted.

## Size
S: pure functions plus four flag arms, all command-side.

## Changelog
- 2026-10-24: first draft.
