# Architect journal

## Current job
**2026-10-24, phase 0 → 1 kickoff** (spawned by the PM on the user's behalf).
1. Scaffold step 0 (README §2) on new branch `integrate/wave-1` from `dev`,
   in worktree `/home/alexander-toth/rust_proj/blast-integrate`. Build and
   test green. Status: **done** (head `2f97859`; see "Scaffolding log").
   Wave 1 registry entries (C1, F1) still to add once their plans are
   user-approved.
2. Review the 6 planners' detailed plans as the PM relays them (verdicts
   below under "Plan reviews"). Status: waiting for plans.
Not to do: merge into `dev`, push.

## Plan reviews
(one section per plan ID: verdict and required fixes)

## Scaffolding log
(one section per wave: what was stubbed, and the commit)

### Wave 1, scaffold step 0 (2026-10-24)
Branch `integrate/wave-1`, created from `dev` at `faace53`. Worktree
`/home/alexander-toth/rust_proj/blast-integrate`. Five commits, head `2f97859`:
- `a86c524 style: run cargo fmt across the workspace`: `cargo fmt --all`,
  default settings (no rustfmt.toml), both crates. Pure formatting.
- `0372901 build: add the approved crates and deny.toml`: the §1.1 set with
  the listed features, in `blast/Cargo.toml` (now carries a header comment
  saying tasks never edit it). Also `publish = false` on `blast` and
  `blast_macros` so cargo-deny skips our unlicensed private crates.
  - **alsa-sys 0.3.1 → 0.4.0.** cpal 0.18.2 → alsa 0.11 → alsa-sys 0.4, and
    `links = "alsa"` forbids two copies, so the approved set doesn't resolve
    next to alsa-sys 0.3. Bumped the direct dep to 0.4.0 (the same crate cpal
    uses); blast's ALSA code compiles and tests unchanged. Recorded below as a
    clarification of §1.1.
  - **2-week rule.** Checked every registry crate in `Cargo.lock` against
    crates.io publish dates (cutoff 2026-10-10). Pinned the six newer ones
    back: syn 3.0.9→3.0.7, litrs 1.0.1→1.0.0, combine 4.6.9→4.6.8,
    equivalent 1.0.3→1.0.2, smallvec 1.16.4→1.16.3, libc 0.2.191→0.2.190.
    All seven approved crates are at exactly the listed versions.
  - Licenses: every locked crate is covered by the allowlist (Unicode-3.0 for
    unicode-ident; the 0BSD/Unlicense crates are dual-licensed with MIT or
    Apache). 136 lock entries, 67 crates in the host's normal graph.
  - `deny.toml`: advisories (yanked = deny), license allowlist (MIT,
    Apache-2.0, MPL-2.0, BSD-2/3-Clause, Unicode-3.0, ISC, Zlib), bans
    (multiple-versions = warn), sources (crates.io only).
- `28cc7c1 runtime: cast the SIGTERM handler through a pointer`: Rust 1.99
  added `function_casts_as_integer`, the only build warning on `dev`. One-line
  fix in `runtime.rs` (owned by nobody until R1).
- `e14a9c0 file_parsing: store AudioFile samples as f32`:
  - `AudioFile.samples: Vec<f32>`; `AudioFile::new` takes `Vec<f32>`.
  - New `decode_helpers::i16_to_f32` (divide by 32768). `wav.rs`/`aiff.rs`
    still decode to i16 via `pcm_to_i16`/`float_to_i16`, then convert. So
    24/32-bit and float sources still lose precision through i16 until F1.
  - `#[cfg(test)] decode_helpers::f32_to_i16`; the decoder tests compare
    `f32_to_i16(&file.samples)` with their old i16 expectations.
  - `Voice::new` converts f32 back to i16 (`* 32768`, clamped); `Voice` still
    stores `Vec<i16>`.
  - `test_util::pcm(&[i16]) -> Vec<f32>` builds engine/process test tracks;
    the round trip is exact, so every engine expectation is unchanged.
  - **Not done in step 0:** `AudioFile.channel_layout` (§3.1) isn't added;
    F1 adds it. `AudioFile` still lives at `file_parsing::decode_helpers`.
- `2f97859 build: allow unused license entries in deny.toml`: ISC is on the
  allowlist but unused; `unused-allowed-license = "allow"` keeps
  `cargo deny check` quiet about it.
cargo-deny isn't installed on the machine; I installed v0.20.2 into the
session scratchpad only (gone after this session) and ran `cargo deny check`:
advisories ok, bans ok, licenses ok, sources ok. Remaining warnings are
duplicate versions, all pulled in by cpal for other platforms (syn 2/3,
thiserror 1/2 and thiserror-impl, jni-sys); `multiple-versions = "warn"`
allows them. For final hardening, reinstall it the same way
(`cargo install --locked cargo-deny --root <scratchpad>/tools`).
Results: `cargo build` 0 warnings; `cargo test` 179 passed (also with the real
`blast/assets/` linked in, so `loads_real_assets` ran); `cargo fmt --check`
clean. `cargo clippy --all-targets`: about 85 pre-existing warnings, none in
new code (commands.rs ~38, mpeg.rs ~18, engine.rs ~9, the rest in
blast_time/runtime/aiff/wav/decode_helpers/line_editor/blast_rand).

## Integration log
(one section per wave: merge order, results, issues)

## Clarifications to README (no change in meaning)
- 2026-10-24, §1.1: "`alsa-sys` and `libc` stay as direct dependencies" means
  alsa-sys **0.4.0**, the version cpal 0.18.2 links (only one crate may link
  native `alsa`). The 0.3 → 0.4 bump is needed for the approved set to resolve
  at all, and blast's FFI code is unchanged. Not edited into README; the
  manifest comment says it.

## Notes for planners from scaffold step 0 (relayed via the PM)
- `AudioFile` stays at `file_parsing::decode_helpers::AudioFile`, and
  `AudioFile::new(name, format, rate, channels, bits, Vec<f32>)` keeps 6
  arguments. engine.rs, commands.rs, processes.rs and test_util.rs (not F1's)
  import that path and call `new` in tests, so F1 must keep both (or
  re-export); `channel_layout` needs a default in `new` plus a setter, or a
  second constructor.
- F1 may delete `pcm_to_i16`, `float_to_i16`, `i16_to_f32` and
  `#[cfg(test)] f32_to_i16`: only `file_parsing/` uses them.
- `Voice` still stores `Vec<i16>`; `Voice::new` converts. E1 removes that.
  `test_util::pcm(&[i16]) -> Vec<f32>` exists for test tracks (E1 owns
  test_util in wave 2).
- The whole repo is rustfmt-formatted, so line numbers quoted from `dev`
  are stale. Plans should cite `integrate/wave-1`.
- Clippy, pre-existing: commands.rs has ~38 warnings that C1 must clear
  (it owns `commands/**`). The one in `file_parsing/mod.rs` sits in
  `load_assets` (LOAD's, wave 2), so F1 isn't responsible for it.
