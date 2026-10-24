# PI: Voice playback and DSP processes (VOX, LFO, ENV, EUC, FX)

VOX (wave 3) defines the voice parameter model. The four wave-4 tasks build on
it. Plan VOX first and in the most detail. The others consume its interfaces
(README §3.9).

The Process model after E1 (wave 2) no longer uses `Rc`. Read E1's detailed
plan (`plans/detailed/E1*.md`, if it exists yet; otherwise README §3.4) for the
exact `process` signature. Today (`processes.rs`) it's
`process(&mut VoiceState)` once per frame, plus
`process_group(&mut [Voice])`, `reset`, and `update_tempo`. `Seq` is the only
process, and it can only retrigger. `notes.txt` has the
user's direction (`trig`, `melo`, piping `seq | trig`, "velocity → make into a
Process", variable rate without pitch shift). Piping isn't in this roadmap (see
brief PF). Don't design it in, but don't design it out either.

## VOX: voice-playback (wave 3)

In `engine/voice.rs`, `commands/voice.rs`, and `engine/handlers/voice.rs`:
- `VoiceParam`, plus `set_param`/`get_param` (README §3.9). Everything below is
  reachable through it.
- Loop modes: one-shot (today), loop, and ping-pong, with loop start and end
  points.
- Start and end offsets, and slicing (`slice <v> 8` picks one of N equal
  regions, which a seq could later index into).
- Reverse as an explicit flag. Today it's done with negative velocity, and
  `Voice::start` already handles negative velocity. Keep that working.
- Pan: replaces the mono→stereo hack in `Voice::process` ("def need a better
  routing system later"). Use a constant-power pan law for mono sources and a
  balance control for stereo. LOAD leaves mono tracks as mono (brief PB), so
  VOX is the single place where mono gets spread across channels.
- Rate: `velocity` is the current varispeed (pitch and speed change together).
  Add pitch in semitones as sugar for rate. Time-stretching (rate without pitch
  change, from notes.txt) is out of scope; list it as a future task.
- Interpolation: linear today. Recommend whether to move to cubic/Hermite now
  that rates other than 1 are central.
- Gain is applied in `process`. Velocity stays as is, so existing tests keep
  their meaning.
- Click-free parameter changes (short ramps on gain and pan) and a declick on
  retrigger: recommend whether this is in scope here or belongs to ENV.

## LFO: lfo-process (wave 4)

`processes/lfo.rs` and `commands/procs/lfo.rs`: sine, triangle, square, saw,
and sample-and-hold (using `blast_rand`, seeded the way PRC defines). Rate is
tempo-synced through a `TempoId` or free-running in Hz. Depth, offset and target are
set through `VoiceParam`. It can target a group, which modulates every member.
Registry entries: a `processes!` row and Command, parse and apply arms.

## ENV: envelope-process (wave 4)

`processes/env.rs` and `commands/procs/env.rs`: AD/AR/ADSR envelopes and simple
fades, triggered on voice (re)trigger. This needs a hook so a Seq retrigger
fires the envelope. Look at `retrigger()` in `processes.rs`, and propose the
smallest hook with its registry entry. Targets go through `VoiceParam`
(usually Gain). Also cover a fade in or out on start and stop.

## EUC: pattern-generators (wave 4)

Only `commands/procs/seq.rs`: generate `seq` steps instead of listing them by
hand. That covers Euclidean rhythms (`-e 3,8[,rot]`, Bjorklund), and possibly
`--every`/`--offset`/`--random <density>`. Purely command-side: the result is
the same `SeqArgs.steps`, so there are no engine changes. Make sure it composes
with the existing `-c` chance and `-j` jitter indexing.

## FX: insert-effects (wave 4)

Per-voice (and per-group, if cheap) insert chain, in a new `engine/fx/**`, with
its hook in `Voice::process`. Effects: a biquad filter (LP/HP/BP/notch, using
RBJ cookbook coefficients), a one-pole smoother for cutoff modulation, a
waveshaping drive, and a bitcrusher/decimator. Effect parameters are modulatable
through an `FxParam` that extends or sits alongside `VoiceParam`; coordinate
this with VOX's design. The per-channel state lives inside the voice (stereo
voices need two filter states). Everything is preallocated, with nothing
allocated in `process`.
Out of scope: send effects (reverb, delay buses). List them as a future task,
since buses need a mixer-graph design.
