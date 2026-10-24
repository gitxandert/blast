---
id: REQ-045
type: merge
from: PM
tasks: [F1a, F1b, C1]
blocks: [E1, LOAD, SH]
status: awaiting-user
created: 2026-10-24
---

## Request
Merge `integrate/wave-1` @ a652f36 into `dev` (wave-1 gate-out, ORG §4.4 step 6).

## Context
Wave 1 = F1 (decode adapter, split F1a/F1b) and C1 (commands split). All three
tasks were implemented by the PM (reduced org) and reviewed and merged by the
architect: F1a d8d6509, F1b 1d39902, C1 c254084, README a652f36. Reports:
`reports/F1a.md`, `reports/F1b.md`, `reports/C1.md`. 15 commits, 41 files.
A merge-tree check shows the merge into `dev` is clean (integrate/wave-1 doesn't touch `plans/`).

Checks on a652f36 (PM re-ran them):
- `cargo build`: 0 warnings. `cargo fmt --check`: clean.
- `cargo test`: 225 passed, 1 ignored (the release-only MP3 speed test; 0.321 s
  for 3 min). 179 before wave 1.
- `cargo clippy --all-targets`: none in the wave-1 code; crate warnings down from ~85 to 17.
  **Still fails** on the deny-level `not_unsafe_ptr_arg_deref` at
  `engine.rs:40`, which was already on `dev`; E1 (wave 2) rewrites that code.

Deviations (all accepted by the architect; details in the reports):
- F1a: demuxer timestamp gaps are now filled with silence too (architect's
  plan fix; symphonia drops CRC-bad FLAC frames before decoding). Two AIFF
  test-data fixes; `riff_fixup` detects by magic.
- F1b: three test-expectation adjustments (ADPCM reference length, stem names).
- C1: one commit for steps 1–8 instead of eight; `seq -t v:a:b` is now a
  formatting error and some error labels read `<cmd> -t` (both accepted in the plan).

For the user (REQ-031 condition 4): still dropped are WAV files with a
non-standard `fmt ` size (e.g. 20 bytes), A-law WAVs with a 16-byte `fmt `,
and extensible µ-law. Wrong-magic files now report `UnsupportedFormat`; f16
WAV reports `InvalidData`. Newly loadable: .wave .aifc .flac .mp1/2/3 .m4a/.mp4
.aac/.adts .au/.snd .raw/.pcm.

Carry-over: `probe_file` checks raw before AU; LOAD (wave 2) makes it match
`decode_bytes` (AU first).

## Options
1. **Merge into `dev`** (the architect does a `--no-ff` merge in the main checkout). Wave 2 can start
   once its plans (E1, LOAD, SH) are approved. (Recommended)
2. **Hold**: keep wave 1 on `integrate/wave-1` until something is checked.

## Impact
Nothing is pushed. `master` is untouched.

## Architect review
Reviewed and merged every task (state/architect.md, "Integration log").

## Decision
