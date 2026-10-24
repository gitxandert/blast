---
id: REQ-033
type: question
from: planner (PB-audio-files / F1a, F1b)
tasks: [F1a, F1b]
blocks: []
status: approved
created: 2026-10-24
---

## Request
README §3.2 promises gapless trimming for MP3 and AAC. Symphonia 0.6.1 trims
MP3 but not AAC. Accept the partial trim F1a can do, or ask for more?

## Context
Symphonia's own support table lists ISO/MP4 and AAC as not gapless. Measured
with a 66,150-frame stereo sine (`detailed/F1b.md` fixtures):
- `a.mp3` (LAME tag): exactly 66,150 frames. Gapless works.
- `a.m4a`: 67,584 frames decoded; the track's `num_frames` is 67,174, so
  truncating to `num_frames` (F1a step 2) removes the end padding and leaves
  the 1,024-frame encoder priming at the start.
- `a.aac` (ADTS): 67,584 frames; ADTS carries no length, so nothing to trim.

The priming delay could be removed by reading the MP4 `edts/elst` edit list
or iTunes `iTunSMPB` metadata, neither of which symphonia 0.6.1 exposes to
the caller. That would mean parsing MP4 atoms ourselves.

## Options
1. **Accept**: trim to `num_frames` where known (removes end padding in
   MP4), leave priming. Document in README §3.2 that AAC is "end-trimmed
   only" and that users should use FLAC/WAV for tight loops. F1b's
   `aac_length_and_level` test asserts the measured bounds. (Recommended)
2. **Drop a fixed 1,024 frames from the start of AAC** (ffmpeg's and Apple's
   usual priming): closer to gapless for common encoders, wrong for encoders
   with 2,112-frame priming (iTunes) or none. Small, but a guess.
3. **Parse `elst`/`iTunSMPB` ourselves** in a small MP4 atom walker: exact
   for files that carry them; ~150 lines and another hand-written parser,
   which the brief tries to avoid.

## Impact
F1a step 2 (truncate to `num_frames`), F1b's `aac_length_and_level`
tolerances, README §3.2 wording (architect). No interface change.

## Architect review
**Architect, 2026-10-24: option 1. Recommended.**

It's a limitation of the approved decoder. Option 2 is a guess that is wrong
for iTunes-encoded files. Option 3 adds a hand-written MP4 parser, which
§3.2 deliberately avoids. (symphonia-format-isomp4 0.6.1 does parse
`edts/elst` internally, but doesn't apply it or expose it.) If tight AAC
loops matter later, option 3 can be a separate request.

This changes the wording of a §3.2 contract ("Gapless trimming is on (encoder
delay and padding)"), so it needs the user's approval. If approved, I amend
§3.2 to: "Gapless trimming is on: exact for MP3 (LAME/Xing); AAC in MP4 is
trimmed at the end only (to the track's frame count), ADTS not at all. Use
FLAC/WAV for sample-accurate loops." F1b's `aac_length_and_level` test pins
the measured bounds.

## Decision

User, 2026-10-24 (via phone): **approved**, option 1: end-trim only for AAC; architect amends README §3.2 wording as in its review.
