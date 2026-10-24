---
id: REQ-031
type: question
from: planner (PB-audio-files / F1a)
tasks: [F1a, F1b]
blocks: []
status: awaiting-user
created: 2026-10-24
---

## Request
Symphonia 0.6.1 rejects or misreads five WAV/AIFF cases that the old
hand-written parsers handled. Should F1a add a small byte-level pre-pass
(`file_parsing/riff_fixup.rs`) that repairs them before symphonia sees the
file, or accept the regressions?

## Context
The brief (F1, "Deleting the old parsers") asks to list for the user any case
symphonia rejects that the old parser accepted. Measured in a scratch crate
against symphonia 0.6.1 with the approved features, feeding the old tests'
synthetic bytes (`detailed/F1a.md`, Tests table, "fails" rows):

| Case | Old parser | Plain symphonia | With pre-pass |
|---|---|---|---|
| WAV `data` chunk before `fmt ` | decodes | `missing format chunk` | decodes (chunks reordered) |
| WAV `data` size `0xFFFFFFFF` with a valid RIFF size (streamed/unfinalised) | reads to end | `chunk length exceeds parent` | decodes (data size clamped) |
| WAV RIFF size smaller than the file | ignored the RIFF size | error | decodes (RIFF size fixed) |
| IMA/MS ADPCM WAV with `fact` | n/a (unsupported) | ignores `fact`: 67,122 frames for a 66,150-frame source | exact length |
| AIFF COMM `num_frames` smaller than SSND | reads `num_frames` | reads the whole SSND | reads `num_frames` |

Not recoverable even with the pre-pass (stays dropped, listed for the user):
- WAV `fmt ` chunks of non-standard size (e.g. 20 bytes). Symphonia accepts
  16, 18 and 40.
- Behaviour changes that are not regressions: wrong-magic files now report
  `UnsupportedFormat` instead of `InvalidData`; f16 WAV (already dropped,
  README §3.2) reports `InvalidData`.

Both-sizes-`0xFFFFFFFF` WAV (what ffmpeg writes to a pipe) already works
without the pre-pass.

## Options
1. **Pre-pass `riff_fixup.rs`** (~80 lines + tests, bounds-checked, no
   `unsafe`): fixes the RIFF/FORM size, clamps an over-long `data` size,
   moves `fmt ` before `data`, and returns a frame limit from `fact` (non-PCM
   WAV) or COMM `num_frames` (AIFF). Copies the buffer only when it reorders
   chunks. (Recommended)
2. **Accept the regressions.** The five tests are `#[ignore]`d with this REQ
   ID; ADPCM files may carry up to one block (≤ ~1,000 frames) of trailing
   silence, which matters for loops. No extra code.

## Impact
F1a step 5 and five rows of its test table; F1b's `adpcm_fixtures_match_ffmpeg`
tolerance. Option 1 adds one crate-private module inside F1's ownership; no
interface change. Option 2 costs real-world compatibility (unfinalised WAVs
from crashed recorders, odd chunk orders) and ADPCM loop accuracy.

## Architect review
**Architect, 2026-10-24: option 1 is acceptable architecturally. Recommended.**

`riff_fixup.rs` is a crate-private module inside F1's ownership. It changes no
interface and nothing in §3, and adds no `unsafe` and no dependency. Option 2
regresses real-world files (unfinalised WAVs from crashed recorders) and
leaves ADPCM loops up to a block long, so option 1 is the better trade for a
sampler.

Conditions, which the F1a plan should state:
1. Bounds-checked throughout: no indexing on file data, and no panic. It
   allocates at most one copy of the input, and only when it reorders chunks.
2. The frame limit only ever shortens output
   (`min(limit, decoded, num_frames)`). A hostile `fact`/COMM count can't
   make output longer.
3. An extra test: a WAV with bytes after the RIFF chunk (appended junk, or an
   `id3 ` chunk after `data`) decodes the same as without them. The
   RIFF-size rewrite must not regress that case.
4. The F1 report lists, for the user, the cases that stay dropped (20-byte
   `fmt `) and the error-kind changes (wrong magic → `UnsupportedFormat`,
   f16 → `InvalidData`).

The behaviour choice is the user's.

## Decision
