# PJ: Recording and export (REC, wave 4)

- **`rec start [path] [--bits 16|24]` and `rec stop`.** The default path is
  `recordings/<timestamp>.wav`. The default format is 32-bit float, at the
  output rate and channel count (README §4 item 6).
- **Tap.** In `engine/render.rs`, before the backend's clamp, so overs survive
  in float recordings. The audio thread copies each rendered block into an
  `rtrb` ring of f32 (preallocated; size it and justify the size). On
  overflow, drop the samples and push an `Event`. Never block.
- **Writer thread.** Drains the ring into `hound`. Integer modes clamp and
  apply TPDF dither in the writer thread, never on the audio thread. Check that
  hound 3.5.1 writes 32-bit float and 24-bit WAV, and patches the header on
  `finalize`.
- **Files over 4 GB.** hound can't write RF64. Recommend splitting into
  numbered files or stopping with an event. 4 GB is about 3 hours of float
  stereo at 48 kHz.
- **Starting and stopping.** The ring and the writer thread are created on the
  REPL thread, and the producer half goes to the engine in a `Command` (it's an
  allocation, so it's built off the audio thread). On stop, the producer comes
  back through the garbage queue. Spell out exactly how the last samples get
  flushed.
- **Offline bounce.** `bounce <seconds> <path>` renders faster than real time.
  The Conductor lives inside the cpal callback after R1, so a bounce can't just
  call `render` from the REPL thread. Evaluate three options: (a) send a
  "render N frames into this preallocated buffer" command handled inside the
  callback across several periods, (b) build a second Conductor from the mirror
  state, or (c) put bounce out of scope. Recommend one. (b) probably counts as
  an architecture change (README §2).
