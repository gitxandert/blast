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
3. **2026-10-24, respawned after a context clear.** Review list from the PM,
   in order: (1) wave 1: C1 + REQ-024, F1a, F1b, REQ-031..036 if written;
   (2) REC: RECa/b/c, REQ-001/002 (architecture, design if needed),
   REQ-003; (3) wave 2 shell: SH-grammar, SHa, SHb, REQ-004/005
   (architecture), REQ-006. Progress: see "Plan reviews" (a group is done
   when all its entries exist).
   **Status: done, all three groups** (see "Review run of 2026-10-24"
   below). Next: wave-1 scaffolding (`id3v2`, if REQ-032 is approved) once
   C1/F1a/F1b are user-approved; README/brief edits for each approved
   REQ-001/002/005/006/033/036.
Not to do: merge into `dev`, push, edit plans (list fixes here instead).

**Request status convention used here.** A request that I've reviewed and
that only needs the user's decision is set to `awaiting-user`. One the
approved docs already answer stays `open`, with a note in its review that the
PM can close it.

## Plan reviews
(one section per plan ID: verdict and required fixes)

### C1 (reviewed 2026-10-24 against `integrate/wave-1` @ 2f97859): approve with editorial fixes
Conforms to §3.5 (layout, `Parsed`, `parse(&str)`), §1.3 and the ownership
tables. `submit(&CmdQueue)` instead of `&mut CmdSender` is flagged as the
brief requires. No registry entries, which is correct: the call-site edits go
through REQ-024. TODOs moved unchanged; all 44 tests ported (checked against
the actual test list). Required fixes (editorial; no re-review needed):
1. **Visibility in `procs/seq.rs`.** `pub(super)` there means "visible in
   `commands::procs`", so `parse` in `commands/mod.rs` can't call `try_seq`.
   Use `pub(in crate::audio_processing::commands)` (or `pub(crate)`) for
   items in `procs/seq.rs` that `mod.rs`/siblings call. `pub(super)` in the
   direct children (`voice.rs`, `tempo.rs`, …) is fine.
2. `mod seq_tests` holds **17** tests, not 22 (27 in `mod tests` + 17 = 44).
3. Integration-edit lines for `engine.rs` at 2f97859 are 1424, 1444, 1449,
   1499 (plan says 1412/1432/1437/1487).
4. Spell out each `HELP` row as a tuple. `row.0` is the canonical parseable
   name (`"tc"`, `"quit"`, `"load"`), and `row.1` is `"usage: summary"`.
   Otherwise `help_rows_name_real_commands` is ambiguous for
   `tc|tempocon`. RECb already uses this form.
Note: later plans (GRP/TMP/PRC/VOX/REC) add
`CmdErr` variants in `errors.rs` and HELP rows; that's checked when E1's
plans exist (E1 owns `commands/**` in wave 2).

### REQ-024: option 1 (C1 makes the call-site edits in one commit; I verify the out-of-ownership diff at merge). Status → awaiting-user (ratified with C1's plan approval).

### F1a (reviewed 2026-10-24): approve with fixes
Conforms to §3.1/§3.2 (adds `channel_layout`; `AudioFile::new` keeps 6 args
and the `decode_helpers` path; `main.rs`'s `decode_helpers::DecodeResult`
keeps resolving). Ownership is fine, given the README §2 clarification
(REQ-035). Verified symphonia 0.6.1 API claims in the registry source:
`AudioDecoderOptions { gapless: true }` is the default,
`copy_to_vec_interleaved` resizes its destination, the error variants match,
and the i32→f32 conversion is `(s as f64 / 2^31) as f32`. New pub
interfaces (`decode_bytes`, `probe_file`, `is_supported_extension`,
`StreamInfo`) are additions that LOAD/TRK need. Fine.
Required fixes:
1. **(REQ-034) Silence fill size.** `packet.dur` is in `TimeBase` units, not
   frames, and is file-controlled. Size the fill from the last good buffer's
   frame count, or from `dur` converted via `track.time_base`. Cap it at
   `max_frames_per_packet` (or a fixed bound such as 1 s), and count it
   against the 2^28 sanity cap.
2. Step 1: also delete step 0's `i16_to_f32` and `#[cfg(test)] f32_to_i16`
   (only `file_parsing/` uses them). Cite lines at 2f97859, not faace53.
   **Resolved** by the planner's editorial refresh (changelog entry 2,
   step 1 now cites 2f97859 and handles both helpers). Only the header line
   still says faace53, which is cosmetic.
3. (REQ-031 conditions) Add a `riff_fixup` test: a WAV with bytes after the
   RIFF chunk decodes the same. The frame limit is only ever applied as
   `min`.
4. Editorial: `track_name` takes `&Path`, but `decode_bytes` has a `&str`
   (wrap it with `Path::new`).
Step → request map: step 2 corrupt packets → REQ-034, `num_frames`
truncation → REQ-033; step 3 whitelist → REQ-036; step 4 `load_assets`
dispatch → REQ-035; step 5 + the five "fails" test rows → REQ-031; the
`id3v2` feature → REQ-032 (architect edits `Cargo.toml` at wave-1
scaffolding).

### F1b (reviewed 2026-10-24): approve with editorial fixes
Conforms to §3.2 and §4.5: `decode_with(name, bytes, RawSpec)` as written,
plus `RawParams`/`parse_raw_name` for TRK's flag overrides. Checked the
G.711 expansions and the known values against `g711.c`: correct. The integer
scaling equals symphonia's (`x/2^(bits-1)`, with f64 for 32-bit), so the two
decode paths agree exactly.
Fixes (editorial):
1. The comment "`i32::MAX` maps to just under 1.0" is wrong in f32 (it rounds
   to 1.0). Drop the claim; `pcm_matches_symphonia` pins the behaviour.
2. G.711 arithmetic must be done in `i32` (the `<< 3`/`<< seg` overflow
   `u8`). Say so in step 1.
3. Fixtures in `assets/.fixtures/` rely on LOAD's recursive scan skipping
   dot-directories. That's a cross-plan requirement for LOAD, and I'll check
   it there. (Until LOAD, the non-recursive `load_assets` prints one
   "unsupported" line for the directory, which is harmless.)
Depends on: REQ-031 (ADPCM tolerance in `adpcm_fixtures_match_ffmpeg`),
REQ-033 (`aac_length_and_level` bounds), REQ-036 (`au snd raw pcm` added to
the whitelist).

### REQ-031..036 (all written by the time of review; none were placeholders)
- REQ-031 (pre-pass): option 1 recommended, with conditions. → awaiting-user.
- REQ-032 (id3v2 feature): recommend approval; verified that the lock and
  graph are unchanged (67 crates). → awaiting-user. On approval: add the
  feature to `Cargo.toml` on `integrate/wave-1`, and update README §1.1.
- REQ-033 (AAC not gapless): option 1. §3.2 wording change on approval.
  → awaiting-user.
- REQ-034 (corrupt packets): option 1, with the required silence-sizing fix.
  → awaiting-user.
- REQ-035 (`load_assets` dispatch): option 1, answered by the brief. README §2
  F1 row clarified. Status stays `open`, for the PM to close as answered.
- REQ-036 (extension whitelist): option 1. §3.2 wording change on approval.
  → awaiting-user.

### RECa (reviewed 2026-10-24): approve with fixes
Conforms to §1.1 (hound, rtrb only), §1.2 (nothing on the audio path except
the `Producer` type) and §1.3. `export/**` is REC's in wave 4 (new top-level
module, already in the README §2 table). I checked the rtrb 0.4.0 calls
(`push_entire_slice`, `pop_partial_slice`, `is_abandoned` with Acquire) and the
hound limits cited. Frame alignment holds: whole-frame pushes, plus a scratch
buffer that's a multiple of `channels`.
Required fixes:
1. (REQ-001 amendment 2) No test calls `finish_all` (it's process-global, and
   tests run in parallel). Test the stop path through `request_stop` only.
2. Step 4 follows REQ-003 (split); the option-2 fallback text can stay.
Note: `recordings/` in `.gitignore` is a wave-4 registry entry (RECb #12).

### RECb (reviewed 2026-10-24): approve with fixes
The tap is real-time safe (`push_entire_slice`, `is_abandoned`, `emit`,
`discard`). It's before the clamp (§3.4/§3.6). Overflow is reported per
episode. The flush sequence is correct. Mirror changes happen only after
`export::start` succeeds.
Required fixes:
1. (REQ-001 amendment 1) When all 4 `retiring` slots are full, use
   `std::mem::forget(producer)` (a documented leak), never a drop on the
   audio thread.
2. Registry entry 3: C1's arms return `CmdResult<Command>` and `parse`
   wraps them, so the `"rec" => return self.try_rec(rest)` form applies,
   unless E1 changes it (aligned at the wave-4 refresh).
3. Registry entry 11 (runtime lines): I'll pin the exact file at the wave-4
   scaffold. If it's `runtime/repl.rs` (REPL's in wave 4), REPL must keep
   the lines.
Depends on E1 (names `Garbage`, `emit`, `discard`, `out_channels`; `discard`
must return the value) and R1 (a thread-safe line printer; a quit path).
Both are recorded as requirements in design/REQ-001.md.

### RECc (reviewed 2026-10-24): approve with fixes (only if REQ-002 = a)
No drops, no allocation (the device buffer is the scratch), exact frame
count, `rec`/`bounce` mutually exclusive.
Required fix: bound the per-callback work (time budget of about 50% of the
period via `Instant::now()`, capped by `BOUNCE_BLOCKS`), or show in the
manual check that xrun events don't flood the REPL. Report the speed.

### REQ-001: design approved with 4 amendments (design/REQ-001.md). → awaiting-user.
On approval: add README §3.10 and the §3.4 sentence, and the PA brief E1/R1
bullets.
### REQ-002: design approved, option (a); (b) rejected (design/REQ-002.md). → awaiting-user.
### REQ-003: agree with option 1 (split at `u32::MAX - 4096`); 2 GiB not needed. → awaiting-user.

### SH-grammar (reviewed 2026-10-24): approve with fixes
Conforms to §3.7 (it produces only `Line`/`Batch`/`Wait`/`Print`, with no
engine semantics) and §4.2 (notes.txt plus let/for/if/fn/source). D-1..D-7
recommendations are endorsed.
Required fixes:
1. **Step budget.** Add a per-`feed`/`source` statement/iteration cap (e.g.
   10^6 → `StepLimit`). The 100 000-output cap doesn't bound loops that
   emit nothing (two nested 65 536 ranges are 4·10^9 steps on the REPL
   thread).
2. **Interactive `else`.** At the prompt, `if … { }` runs as soon as `}`
   closes, so a next-line `else` is a stray statement. Rule: at the top level
   of interactive input, `else` goes on the same line as `}`. Inside open
   blocks and in files, the next line is fine.
3. Editorial: reserve `and`, `or`, `not` as names too.

### SHa (reviewed 2026-10-24): approve with fixes
Standalone `shell/**`; registry entries (`pub mod shell;` + stub) are correct
for the wave-2 scaffold. No dependencies, no `unsafe`.
Fixes: follow the grammar fixes. Add `ShellErrKind::StepLimit`. The parser
treats top-level `}` + newline as complete (no `else` lookahead at the
interactive top level); test both forms. Reserve `and/or/not`.

### SHb (reviewed 2026-10-24): approve with fixes
`feed`/`ShellOut` are exactly §3.7, and everything else is additive
(REQ-005). `Shell: Send`, `Arc` bodies, no `Rc`.
Required fixes:
1. (REQ-005 amendment 1) `names()` → `var_names()` + `fn_names()`.
   REPLb step 7 needs them separate.
2. (REQ-005 amendment 2) `source(&mut self, path: &Path)`.
3. Step budget (grammar fix 1): count statements/iterations in `Run`, plus a
   `step_limit` test next to `output_limit`.
4. Top-level interactive `else` rule (grammar fix 2) in `feed`.
Cross-plan: REPLb consumes this API (checked: `pending`, `reset`, `source`,
`command_start`, `Wait::duration` match); it needs the same two renames.

### REQ-004: conforms; recommend approval with the D-1..D-7 answers plus grammar fixes 1–2. → awaiting-user.
### REQ-005: design approved (design/REQ-005.md), option 1, with 2 amendments. → awaiting-user. On approval: replace the §3.7 code block.
### REQ-006: option 1. E1 adds `EngineState::all_ids(kind)` and reserves the name `all` in the mirror; each wave-3 family accepts `all` in its own file as one `Batch`; INS documents and completes it. → awaiting-user. On approval: update the PA, PH and PI briefs.

### Review run of 2026-10-24 (respawn): all three groups done
Not reviewed (out of scope for this run): LOAD, TRK, REQ-037..041 (drafted;
the PM said they're lower priority), REQ-007..010, REPLa/b, and the PH/PI
plans. E1, R1 and INS are unwritten (planners PA/PH/PI stopped).
Carry-overs for later reviews:
- LOAD: `scan_assets` must skip dot-directories (F1b's `assets/.fixtures/`)
  and use `file_parsing::is_supported_extension`.
- TRK: `rescan` must use `is_supported_extension`.
- E1: `discard` must hand the value back; `Garbage` must allow non-`Copy`
  payloads; `Command` is move-only; the clock advances only in `render`
  (design/REQ-001.md); `all_ids` (REQ-006).
- R1: `print_line` callable from any non-audio thread; the quit path drops
  the stream, then runs the hooks.
- Wave-1 scaffold, after the user approves: the `id3v2` feature
  (REQ-032). C1/F1 need no stubs.

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
- 2026-10-24, §2 wave-1 F1 row: "except `load_assets` (it keeps working
  unchanged)" now reads "except `load_assets` (LOAD's; F1 changes only its
  decoder dispatch to call `decode_file`, and its signature and behaviour
  stay)". This matches the PB brief, which was already approved (REQ-035).

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
