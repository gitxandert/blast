---
id: REQ-034
type: question
from: planner (PB-audio-files / F1a)
tasks: [F1a]
blocks: []
status: awaiting-user
created: 2026-10-24
---

## Request
When a packet in the middle of a file fails to decode, should F1 skip it or
fail the whole file? The brief asks for a recommendation.

## Context
`AudioDecoder::decode` returns `Err(DecodeError)` for a corrupt packet (bad
MP3 frame, damaged FLAC frame CRC, …); the format reader usually keeps
delivering the following packets. Desktop players skip such packets. For a
sampler, a dropped packet that shifts everything after it would break loop
timing, so if we skip we should keep the length.

## Options
1. **Substitute silence and continue**: push `packet.dur` frames of zeros
   for the failed packet (keeps timing), count failures, and fail the file
   with `InvalidData` if more than 10% of packets failed (garbage named
   `.mp3` stays an error). No message per packet (the decoder is quiet; a
   count could be logged by the caller later). (Recommended)
2. **Fail the file** on the first bad packet: simplest, but one flipped bit
   in a long file makes it unloadable.
3. **Skip without silence**: what players do; shifts timing.

## Impact
F1a step 2 only (packet loop in `symphonia.rs`). Tests:
`garbage_with_audio_extension_is_error` covers the threshold. No interface
change.

## Architect review
**Architect, 2026-10-24: option 1, with a required fix to how the silence is sized.**

Substituting silence keeps loop timing, and the 10% threshold keeps junk
files as errors. That's the right policy for a sampler.

Required fix (F1a step 2): `Packet::dur` is in the track's **`TimeBase`
units**, not frames. MP4 time bases often aren't `1/sample_rate`, and `dur`
comes from the file, so it can be hostile. Size the fill as follows:
- Prefer the frame count of the last successfully decoded buffer.
- Failing that, `dur` converted through `track.time_base` to frames.
- Cap the result at `codec_params.max_frames_per_packet` when known, and in
  any case at a fixed bound (e.g. 1 s of frames).
- Count the fill against the same 2^28-sample sanity cap as the output
  reservation.

A single corrupt packet must not be able to request an unbounded allocation
(README §1.2, hostile input).

This is user-visible behaviour (a damaged file loads with a short silent gap),
so the user decides.

## Decision
