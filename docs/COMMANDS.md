# BLAST command reference

Everything you can type at the BLAST prompt (or put in a script), with
examples. Every example was checked against the current build.

Examples that name your own files (`~/samples/bass.wav`, `loop.raw`) need
those files to exist; the examples use the voices `kick`, `hat`, `snare`,
`rim`, `bass` and so on as if you had created them.

You can get much of this from inside BLAST too:

- `help` lists every kind, action and script word.
- `help voice` (or any kind or action) prints its full schema: every setting, its range, default and short form.
- `blast --card` prints a one-page summary and exits.

## Contents

1. [Starting BLAST](#1-starting-blast)
2. [The prompt: keys, history, completion](#2-the-prompt-keys-history-completion)
3. [How statements work](#3-how-statements-work)
4. [Values: numbers, decibels, pitches, durations](#4-values-numbers-decibels-pitches-durations)
5. [Clocks and tempo](#5-clocks-and-tempo)
6. [Tracks](#6-tracks)
7. [Voices](#7-voices)
8. [Groups](#8-groups)
9. [Seqs: rhythm](#9-seqs-rhythm)
10. [Pipes: `trig` and `melo`](#10-pipes-trig-and-melo)
11. [Packets](#11-packets)
12. [Glides: `over`](#12-glides-over)
13. [Actions: start, stop, pause, resume, remove, ungroup, rescan](#13-actions)
14. [Scheduling: `at`, `after`, `by`, `and`, `then`, `thenif`, `wait`](#14-scheduling-at-after-by-and-then-thenif-wait)
15. [LFOs](#15-lfos)
16. [Envelopes](#16-envelopes)
17. [Effects: filter, drive, crush](#17-effects-filter-drive-crush)
18. [Waves: oscillators](#18-waves-oscillators)
19. [Buffers: recording and resampling](#19-buffers-recording-and-resampling)
20. [Variables: `let`](#20-variables-let)
21. [Scripts: `source`, `check`, `--script`](#21-scripts-source-check---script)
22. [Queries: `state`, `help`, `clear`, `quit`](#22-queries-state-help-clear-quit)
23. [History](#23-history)
24. [Errors and warnings](#24-errors-and-warnings)
25. [Quick reference](#25-quick-reference)

---

## 1. Starting BLAST

```
blast [OPTIONS] [ASSETS_DIR]
```

| Option | What it does |
|---|---|
| `ASSETS_DIR` | Folder of audio files to load as tracks (default: the crate's `assets/`). Subfolders become track paths: `drums/kick.wav` is the track `"drums/kick"`. |
| `--device <NAME>` | Output device: an id or a name from `--list-devices`. |
| `--list-devices` | Print the output devices and exit. |
| `--script <NAME>` | Run a script before the prompt appears. It looks in the working directory first, then in the sources folder. The whole script is checked first; if anything is wrong, none of it runs. |
| `--sources <DIR>` | Folder that `source NAME` reads from and `state to NAME` writes to (default: the crate's `sources/`). |
| `--card` | Print the one-page language card and exit. |
| `--agent` | No terminal UI. Reads JSON requests on stdin, one per line, and answers on stdout. See "Driving BLAST from a program" in the README. |
| `--export <NAME> [OUT.wav]` | Render a script to a WAV file without a device or a prompt (see below). |
| `--length 64b\|90s\|N`, `--bits 16\|24\|32`, `--rate HZ` | Export only: fixed length (bare number = beats of `main`), sample format, sample rate. |
| `--seed N` | Seed for `chance`, `jitter`, random choices and dither (export default 0; a live session accepts it but ignores it for now). |
| `-h`, `--help` | Print usage. |

```sh
blast                                   # default assets, default device
blast ~/samples                         # your own sample folder
blast --list-devices
blast --device alsa:pipewire ~/samples
blast --script groove ~/samples         # runs sources/groove.blast first
blast --sources ~/sets --script live    # runs ~/sets/live.blast first
```

`--list-devices` ends a direct device's summary with `, direct`. Use the
sound server (`alsa:default`, `alsa:pipewire`, `alsa:pulse`, `alsa:jack`)
and pick a USB interface as its sink: a direct ALSA device takes the card
exclusively, and on snd-usb-audio a direct 128- or 256-frame two-period
ring can run about 25% slow with no xrun (workaround: `options snd_usb_audio lowlatency=0`,
which is machine-wide and needs root; blast-server also has
`period_frames = 512`).

At startup BLAST prints the tracks it loaded and the output device line
(rate, channels, buffer size). Supported files: WAV, AIFF/AIFC, FLAC, MP3
(and MP1/MP2), AAC (MP4/ADTS), ALAC, ADPCM, Sun `.au`/`.snd`, and raw PCM.
Every file is resampled and remixed to the output format when it loads.

### Rendering a script to a file: `--export`

```sh
blast [ASSETS_DIR] --export finished.blast [out.wav] [--length 64b|90s|N] [--bits 16|24|32] [--rate HZ] [--seed N]
```

`blast --export finished.blast` plays the script without a device or a
prompt, as fast as it can compute, and writes `finished.wav` in the working
directory (an existing file is overwritten). The script is found like
`--script`. Name an output with a second argument ending in `.wav`.

The render lasts until the script ends: its last `wait`, or a written `end`
(including `end after 8b`), plus any glide still running then, so
`kick gain=0 over 4b then end` is exactly 4 beats long. The implicit end
stops nothing, so whatever is still sounding is cut at that frame.
`--length` overrides it (cut exactly there).

| Flag | Meaning |
|---|---|
| `--length 64b` | Exactly 64 beats of `main` (at its tempo when the render starts). A bare number is beats. |
| `--length 90s` | Exactly 90 seconds. |
| `--bits 16` / `24` | Integers with TPDF dither. Default: 32-bit float, kept as rendered (not clamped). |
| `--rate HZ` | Sample rate (default: the assets' preferred rate). Channels: the assets' preferred count. |
| `--seed N` | Default 0. The same script, assets and seed always give the same bytes. |

A script that ends at frame 0 is refused (E207). Buffers with `to=` are
written as in a live session; a take still running at the end is stopped
and written. Progress goes to stderr once a second; the exit code is 0 on
success and 1 on error. `--export` can't be combined with `--agent`,
`--device`, `--list-devices` or `--card`. A script that never ends (a wait
on a stopped clock) renders until you stop it (Ctrl-C).

---

## 2. The prompt: keys, history, completion

| Key | Action |
|---|---|
| Enter | Run the line. |
| Up / Down | Step back and forward through history. A multi-line entry comes back on one line. |
| Ctrl-R | Search history backwards. Type to narrow it; press Ctrl-R again for an older match. Enter runs the match; any editing key keeps it on the line; Esc, Ctrl-G or Ctrl-C cancels. |
| Tab | Complete a kind, name, setting or value. If several match, the first Tab lists them and each further Tab cycles through them. |
| Left / Right | Move one character. |
| Ctrl-Left / Ctrl-Right, Alt-B / Alt-F | Move one word. |
| Home / Ctrl-A, End / Ctrl-E | Start or end of the line. |
| Backspace / Delete | Delete a character. |
| Ctrl-W | Delete the word before the cursor. |
| Ctrl-K / Ctrl-U | Delete to the end / to the start of the line. |
| Ctrl-C | Clear the line, or abandon an unfinished block. On an empty prompt, press it twice to quit. |
| Esc / Ctrl-G | Cancel a Ctrl-R search. |

**Multi-line input.** A line continues on the next one (the prompt changes)
while a `{` is open, when the line ends with `|`, `,`, `and`, `then` or `thenif`, or when it
ends with `\`. The whole block runs when it closes, and it becomes a single
history entry.

```blast
kick {
  seq pat 1 2 3 4 in 4
  filter lo cutoff=800
}
```

---

## 3. How statements work

A statement is about a **named thing**. If the name is new, the statement
creates it; if it exists, the statement updates it.

```
[kind] name [from "track"] [on clock] [in N] [setting=value ...] [{ ... }] [| receiver, ...] [at N | after N [by clock]]
```

```blast
voice kick from "kick" gain=0.8     # create (kind word + new name)
kick gain=0.5                       # update (no kind word needed)
voice kick gain=0.5                 # also an update
```

The rules:

- **A typo never creates anything.** A statement without a kind word is
  always an update, so an unknown name is an error, with a suggestion
  (`unknown name 'kcik' (did you mean 'kick'?)`).
- **Quoted strings are things from outside**, such as tracks and files:
  `"kick"`. Bare words are your own objects: `kick`. So the track `"kick"`
  and the voice `kick` never clash.
- **Names** start with a lowercase letter, then lowercase letters, digits
  and `_`. They can't be reserved words, and they can't look like a clock
  rate (`b93`).
- **One namespace.** Clocks, voices, groups, buffers and top-level
  seqs/packets all share one namespace. Anything an object owns (seqs,
  LFOs, envelopes, effects) is scoped to that owner, so `kick.pat` and
  `snare.pat` can both exist.
- **Blocks `{ }` mean "belongs to".** `kick { seq pat 1 2 3 4 }` makes
  `kick.pat`. `group drums { kick; hat }` puts `kick` and `hat` in `drums`.
  Blocks only add things; use `ungroup` or `remove` to take them out.
- **Only what you write changes.** Settings you leave out keep their
  values. A list you write again (steps, receivers, notes) replaces the old
  one.
- **All or nothing.** A statement, its block included, applies completely
  or not at all. Statements joined with `and` apply in the same audio
  instant, or none of them do.
- A **newline** or `;` ends a statement. `#` starts a comment that runs to
  the end of the line.
- **Short forms.** Every setting has a one-letter form: `kick r=1.5` is
  `kick rate=1.5`. `help <kind>` shows them in brackets.

```blast
voice snare from "snare"; snare { seq back 2 4 }   # two statements, one line
kick g=0.5 p=-0.3 r=0.9                             # gain, pan, rate
```

---

## 4. Values: numbers, decibels, pitches, durations

| Form | Example | Meaning |
|---|---|---|
| Number | `0.5`, `-1`, `12` | Write `0.5`, not `.5`. |
| Decibels | `-6db`, `6db` | Accepted by every `gain` and by filter `base`. Stored as a linear value. |
| Pitch | `1`, `0.5`, `1.5x`, `7st`, `50c` | A ratio (a bare number or `x`; 1 is normal, 0.5 an octave down, 2 an octave up), semitones (`st`) or cents (`c`) (`pitch` only). |
| Clock rate | `120b`, `500ms`, `24000f` | `120b` is 120 beats a minute; `500ms` and `24000f` set the time between beats. |
| Duration, bare | `0.5`, `2` | A fraction or multiple of the object's **period** (`in N`, 4 beats by default). |
| Duration, beats | `2b` | Beats of the object's clock. |
| Duration, time | `1.5s`, `250ms`, `10us`, `200ns` | Clock time. |
| Duration, frames | `480f` | Frames at the output sample rate. |
| Frequency | `5hz` | LFO `freq` only. |
| Word | `pingpong`, `yes`, `no` | Enumerated values. |
| String | `"kick"`, `"take.wav"` | Tracks, files and paths. `\"` and `\\` escape. |
| Variable / expression | `$bpm`, `($bpm * 2)` | See [Variables](#20-variables-let). |

**Periods.** Every voice, group and seq has a period: a number of beats
of its clock, set with `in N` (default 4). Bare durations count periods,
so with `in 4`, `over 1` means 4 beats and `decay=0.25` means one beat.

```blast
kick in 8          # kick's period is now 8 beats
kick gain=0 over 1 # fades over 8 beats
```

---

## 5. Clocks and tempo

Clocks are objects. The session starts with one clock, **`main`, at 120
bpm**. Anything with no `on` follows its parent's clock: a voice in a
group follows the group, a seq follows its owner, and anything at the
session level follows `main`.

### Setting the tempo

```blast
clock main 124b       # main at 124 bpm
clock main 500ms      # one beat every 500 ms (= 120 bpm)
clock main 24000f     # one beat every 24000 frames (= 0.5 s at 48 kHz)
```

The new tempo applies without a jump: everything on the clock keeps its
place in the bar.

### Changing tempo gradually (accelerando / ritardando)

```blast
clock main 140b over 8      # glide to 140 bpm over 8 periods (32 beats)
clock main 140b over 16b    # ... over 16 beats
clock main 90b over 10s     # ... over 10 seconds
```

A bare `over` number counts periods of 4 beats. A `b` clock glides
linearly in bpm. You can't glide a clock that is stopped, which includes a
named clock you just created and haven't started (E409: "start it first,
or change the rate without over").

### Changing tempo on a beat

```blast
clock main 120b at 4             # on the next multiple of 4 beats (next bar in 4/4)
clock main 100b after 2          # 2 beats from now
clock main 128b at 16 by slow    # count the wait on another (running) clock
```

See [Scheduling](#14-scheduling-at-after-by-and-).

### More clocks: polytempo

 A **new named clock starts stopped**: `start slow` sets it running. `main`
runs from startup. A private clock (`on 93b`) starts by itself when its
owner is playing.

```blast
clock slow 80b                # a named clock anyone can share
start slow                    # new named clocks are stopped until started
voice snare from "snare" on slow
hat on 93b                    # hat gets a private 93 bpm clock
hat on kick                   # hat follows whatever clock kick hears
hat on main                   # back onto main
hat on drums                  # follow its group's clock
```

When an object moves to another clock with `on`, everything that follows
it moves too (its seqs, LFOs and so on).

### Starting, stopping and removing clocks

```blast
stop main       # stop and rewind everything on main
start main      # play everything on main from the top
pause main      # hold everything on main in place
resume main     # continue
remove slow     # delete a clock (only once nothing uses it; main can't be removed)
```

---

## 6. Tracks

A **track** is decoded audio. Every file in the assets folder becomes a
track at startup, named by its path without the extension
(`assets/drums/kick.wav` becomes `"drums/kick"`). Track names are always
quoted.

```blast
track "snare2" from "snare.wav"           # import (a relative path starts in the assets folder)
track "bass" from "~/samples/bass.wav"    # absolute or ~ paths work too
track "snare2" from "kick.wav"            # re-import: every voice playing "snare2" swaps samples
track "raw" from "loop.raw" format=s16le rate=44100 channels=2   # headerless PCM
rescan                                    # import files that are new in the assets folder
remove "snare2"                           # delete a track (only while no voice uses it)
```

| Setting | Short | Values | Notes |
|---|---|---|---|
| `format` | `f` | `s8 u8 s16le s16be s24le s24be s32le s32be f32le f32be f64le f64be` | Raw PCM only. |
| `rate` | `r` | 1..768000 | Raw PCM only. |
| `channels` | `c` | 1..64 | Raw PCM only. |

A raw file can also carry its format in its name, e.g.
`kick.s16le.48000.2.raw`.

---

## 7. Voices

A **voice** plays a track (or a [buffer](#19-buffers-recording-and-resampling)), [waves](#18-waves-oscillators), or both. Its `from` is optional when its block holds a wave.

```blast
voice kick from "kick"                    # create
voice hat from "shaker" gain=0.4 pan=0.3  # create with settings
voice pad from "pad" on slow in 8         # on a clock, with an 8-beat period
kick from "snare"                         # swap the track under an existing voice
start kick                                # play it once from its start point
```

### Voice settings

| Setting | Short | Values | Default | Glides | Meaning |
|---|---|---|---|---|---|
| `gain` | `g` | 0..8, or dB | 1 | yes | Level. `gain=-6db` works. |
| `pan` | `p` | -1..1 | 0 | yes | -1 left, 1 right. |
| `rate` | `r` | -16..16 | 1 | yes | Playback speed (pitch and speed together). Negative plays backwards. |
| `pitch` | `t` | ratio 0.0625..16 (-48..48 semitones) | 1 | yes | Pitch only. A bare number is a ratio: `1` is normal, `0.5` an octave down, `2` an octave up. Also `1.5x` (the same, explicit), `7st` (semitones) and `50c` (cents). The ratio must be above 0. Any value other than normal uses the granular engine, so the speed is unchanged. |
| `stretch` | `x` | 0.0625..16 | 1 | yes | Speed only, pitch unchanged (2 = twice as fast). |
| `resample` | `z` | `iir` `cubic` | iir | no | How a plain playback head reads the source; see below. |
| `start` | `s` | 0..1 | 0 | yes | Start point, as a fraction of the track. |
| `end` | `e` | 0..1 | 1 | yes | End point. |
| `loop` | `l` | `off` `forward` `pingpong` | off | no | Loop mode. |
| `loopstart` | `a` | 0..1 | 0 | yes | Loop start. |
| `loopend` | `b` | 0..1 | 1 | yes | Loop end. |
| `slices` | `n` | 0..1024 | 0 | no | Cut the track into N equal slices (0 = off). |
| `slice` | `c` | 1..1024 | 1 | no | Which slice plays, counting from 1. |
| `choir` | `h` | 1..8 | 1 | no | Number of instances of the voice. |
| `choirmode` | `m` | `seq` `sim` (or `sequential`, `simultaneous`) | seq | no | `seq`: each step uses the next instance (a round). `sim`: each step starts all of them. |
| `intonation` | `i` | 0..1 | 1 | no | 1 keeps the instances identical; lower values detune, spread, soften and delay each one. |

```blast
kick rate=-1                       # backwards
kick pitch=1.5                     # up by a 3:2 ratio (a fifth), same speed
kick pitch=7st                     # up seven semitones (a fifth)
kick pitch=0.5                     # an octave down
kick pitch=50c                     # up a quarter tone
kick stretch=2                     # twice as fast, same pitch
kick start=0.1 end=0.9 loop=pingpong loopstart=0.2 loopend=0.6
kick loop=off slices=4 slice=2     # play only the second quarter
kick choir=4 choirmode=sim intonation=0.8   # four detuned copies at once
kick g=-6db p=-0.5                 # short forms
```

`resample` sets how a plain playback head reads the source. `iir` (the
default) is band-limited at any rate: fast playback (`rate=2`, a `rate` glide,
choir detune above 1) doesn't fold high frequencies back as aliasing, and 1×
playback stays flat to about 15 kHz (within 0.25 dB). It is aligned with
`cubic`, grains and spectral voices below about 10 kHz: the filter's 55 us
delay is cancelled by reading the source ahead, so layered voices don't comb.
Above about 10 kHz the filter's own phase remains. Only a note that starts
from silence is onset-aligned with `cubic`; a retrigger of a sounding note
carries the output across instead (its onset sits about 2.8 frames later,
inaudibly). After a large instant drop of speed the output runs a little
ahead until the lead is used up. `cubic` is the older 4-point Hermite read:
cheaper, but content above the new Nyquist aliases. A note that is sounding
keeps its method until the next hit or stop. `resample` applies to the plain
head of a track or buffer voice; `pitch`/`stretch` (grains, spectral) read
the source themselves. `state` prints `resample=cubic` when it isn't the
default.

```blast
voice shaker from "shaker" loop=forward rate=3 resample=cubic
shaker resample=iir                # band-limited from the next hit
```

To fade, glide the gain (see [Glides](#12-glides-over)):
`kick gain=0 over 2`.

---

## 8. Groups

A **group** holds voices that start, stop and share a clock together. A
group can own seqs, LFOs, envelopes and effects, and has its own `gain`.

```blast
group drums { kick; hat }               # put existing voices in a group
group drums {                           # or declare voices inside it
  voice snare from "snare"
  voice rim from "rim1" gain=0.5
}
group drums { kick; hat; seq extra 1 3 } # a group seq retriggers every member
start drums                             # start all members
drums gain=0.5 over 2                   # fade the whole group
drums gain=-12db
drums on slow                           # move the group (and members following it) to clock slow
drums in 8                              # set the group's period
```

| Setting | Short | Values | Default | Glides |
|---|---|---|---|---|
| `gain` | `g` | 0..8, or dB | 1 | yes |

### Editing membership

```blast
group drums { hat }    # add a voice; a voice in another group moves here
ungroup drums.hat      # take one member out (it stays on the clock it was hearing)
ungroup drums          # dissolve the group; the members stay
```

`remove drums` deletes the group **and** its members; use `ungroup drums`
first if you want to keep the voices.

Members keep their own names: `hat` still works, and so does `drums.hat`.

---

## 9. Seqs: rhythm

A **seq** is a list of positions on a clock. Each position sends a "bang".
By itself a seq makes no sound: its owner, or its [pipe](#10-pipes-trig-and-melo),
decides what each bang plays.

```blast
kick { seq pat 1 2 3 4 in 4 }          # kick.pat: four on the floor, cycle of 4 beats
kick { seq pat 1 2 3 4 in 4 | trig }   # the same, with an explicit pipe
snare { seq back 2 4 }                  # `in` defaults to the owner's period (4)
hat { seq off 1.5 2.5 3.5 4.5 in 4 }    # offbeats
seq pulse 1 3 | trig kick               # top-level seq: needs a pipe, follows main
```

A seq inside a voice's block with no pipe retriggers that voice. Inside a
group's block, it retriggers every member. A top-level seq has no owner,
so it must have a pipe (E502).

### Steps

Steps count **from 1**, in beats: with `in 4`, the positions are 1 up to
(but not including) 5. Fractions are fine.

| Step | Meaning |
|---|---|
| `3` | Beat 3. |
| `2.5` | Halfway between 2 and 3. |
| `1..4` | 1, 2, 3, 4 (inclusive, step 1). |
| `4%50` | Plays 50% of the time. |
| `2~0.05` | Lands up to 0.05 beat early or late. |
| `1..16%70~0.02` | Marks on a range apply to every step in it. |

```blast
shk { seq s 1..16%70~0.02 in 16 }   # 16 steps, each 70% likely, a little loose
```

Steps outside the cycle are refused: `step 5 is outside the cycle of 4`,
`step 0: counting starts at 1`.

**Grid and rotation.** With `grid=G`, the step numbers count slots of a
G-slot grid over the cycle instead of beats: slot k sits at beat
`1 + (k-1) * period / G`. With `rot=N`, every step moves N slots later,
wrapping within the cycle (a negative N moves earlier). Without `grid=`, a slot
is a beat, so `rot=1` moves every step one beat later. A step must be inside
the grid: `step 17 is outside the grid of 16`.

```blast
hat { seq e 1 5 9 13 grid=16 in 4 }          # beats 1, 2, 3 and 4 (sixteenth slots)
hat { seq off 3 7 11 15 grid=16 in 4 }       # the "and" of every beat
hat.e rot=2                                   # every step two slots later
hat { seq r 1 2 rot=-1 in 4 }                 # beats 4 and 1: rot without a grid is in beats
```

Ranges (`1..16`) count slots too. The marks stay in beats: `%` chance is a
percentage, and `~` jitter, `jitter=`, `late=` and `early=` are beats (or a
duration), whatever the grid. Restating `grid=`, `rot=` or `in` on a seq
with written steps moves the steps again from the numbers you wrote;
restating the step list keeps its `grid=` and `rot=`. `state` prints the
steps as written, with `grid=` and `rot=` when they are set.

### Seq settings

| Setting | Short | Values | Default | Glides | Meaning |
|---|---|---|---|---|---|
| `seed` | `s` | whole number | random | no | Makes chance and jitter repeat exactly. |
| `chance` | `c` | 0..100 | 100 | yes | Percent chance for every step. A step's own `%n` overrides it. |
| `jitter` | `j` | beats or duration | 0 | yes | Each step lands early or late by up to this. A step's own `~n` overrides it. |
| `late` | `l` | beats or duration | 0 | yes | Each step lands up to this late. |
| `early` | `e` | beats or duration | 0 | yes | Each step lands up to this early. |
| `steps` | `n` | 1..1024 | none | no | Generate N steps per cycle, spread evenly over the grid (Euclidean). Replaces written steps. |
| `grid` | `g` | 1..1024 | the cycle's beats | no | Slots per cycle: `steps=` spreads over them, and written steps count them instead of beats (`grid=16 in 4`: step 5 is beat 2). |
| `rot` | `r` | -1024..1024 | 0 | no | Moves every step (generated or written) this many slots later, wrapping within the cycle. Without `grid=`, a slot is a beat. |

Use either `jitter`, or `late`/`early`, not both. For seq settings, a bare
number counts **beats**; `10ms` or `480f` count time.

```blast
kick.pat seed=7 chance=70        # the same 70% pattern every cycle
snare { seq back 2 4~0.05 late=0.03 in 4 | trig }   # lazy backbeat
kick.pat late=10ms
kick.pat chance=100 over 4       # bring every step back gradually
```

### Euclidean rhythms

```blast
rim { seq euc steps=3 grid=8 in 8 | trig }   # 3 steps spread over 8 slots (tresillo)
rim.euc steps=5 rot=2                        # 5 over 8, rotated two slots later
```

### Editing a seq in place

Restating a seq changes it without restarting it:

```blast
kick.pat 1 3          # new steps (replaces the old list)
kick.pat 1..4
kick.pat in 8         # new cycle length
kick.pat on slow      # move just this seq to another clock
```

### Controlling a seq

```blast
pause kick.pat     # stop firing (the voice rings out), keep the position
resume kick.pat    # carry on from where it paused
stop kick.pat      # stop firing and rewind
start kick.pat     # play from the top of the cycle
remove kick.pat    # delete it
```

---

## 10. Pipes: `trig` and `melo`

A pipe `|` says what a seq's (or packet's) bangs do. Separate several
receivers with commas.

| Receiver | Meaning |
|---|---|
| `trig` | Restart the owner from its start point. |
| `trig VOICE_OR_GROUP` | Restart that voice, or every member of that group. |
| `melo VOICE n n n ...` | On each bang, set the voice's pitch to the next note in the list (MIDI note numbers, 60 = no shift) and play it. The list wraps around. |

```blast
kick { seq pat 1 2 3 4 in 4 | trig }       # restart kick
kick.pat | trig snare                      # restating only the pipe replaces it
kick.pat | trig, melo bass 36 36 43 41     # fan-out: kick plays, AND bass plays a riff
seq pulse 1 3 | trig drums                  # restart every member of drums
```

Several seqs may pipe into the same voice (fan-in).

---

## 11. Packets

A **packet** bundles seqs that share one pipe. `on` and `in` on the packet
become the defaults for its seqs. Actions on the packet act on all of its
seqs.

```blast
packet fills {
  seq a 4 4.5 in 4
  seq b 2.75 4.25 in 8
} | trig snare

pause fills       # pause both seqs
resume fills
remove fills
```

A packet can also live in a voice's or group's block, like a seq.

---

## 12. Glides: `over`

`over D` after a setting moves it to the new value gradually instead of
at once. Each setting can have its own `over`. A bare number counts
periods; `b` counts beats; you can also use time.

```blast
kick gain=0 over 2                        # fade out over 2 periods
kick gain=1 over 1b                       # fade in over one beat
kick rate=2 over 1                        # speed up over a period
kick pitch=7st over 2b gain=0.5 over 3b   # two glides, different lengths
snare pan=1 over 500ms
drums gain=0.5 over 4                     # group gain
kick.pat chance=50 over 8                # seq feel
kick.wob depth=0.3 over 4                 # lfo depth
kick.lo cutoff=4000 over 4                # effect parameter
clock main 140b over 8                    # tempo
```

- A new change during a glide starts from the current value, so nothing
  jumps.
- The glide's length is fixed when the line runs, so retiming the clock
  partway through doesn't stretch it.
- Pitch glides in semitones (which sounds even) however it is written; a `b` clock glides in bpm.
- Only settings marked "glides" in `help <kind>` accept `over`. For
  example, `loop`, `slices`, `slice`, `seed` and `type` can't (E402).

---

## 13. Actions

Actions take one or more names separated by spaces, or `all`.

| Action | Takes | Does |
|---|---|---|
| `start` | voices, groups, clocks, seqs, packets, buffers, LFOs, envs, effects, `all` | Play from the top. On a clock: everything on it. On a buffer: start a recording. |
| `stop` | the same | Stop and rewind. |
| `pause` | the same | Hold in place. |
| `resume` | the same | Continue after `pause`. |
| `remove` | any object, quoted tracks, `all` | Delete it. A group goes with its members; a track only while no voice uses it; `remove all` deletes everything except `main`. |
| `ungroup` | a group, or `group.member`, `all` | A member goes back to the session level; a whole group dissolves and keeps its members. |
| `rescan` | nothing | Import files that are new in the assets folder. |

```blast
start kick hat           # several at once
stop all
start drums
pause main               # everything on the clock
resume main
ungroup drums.hat
remove kick.pat
remove bass              # the voice...
remove "bass"            # ...then its track (quoted)
rescan
remove all               # clean slate (main survives)
```

What `stop`/`pause`/`start` mean for processes and effects:

| Object | `pause` | `stop` | `start` |
|---|---|---|---|
| seq / packet | stops firing, keeps the position | stops firing, rewinds | fires from the top |
| lfo | holds its current value | removes its effect | runs again from `phase` |
| env | holds its level | removes its effect | waits for the next trigger |
| effect | bypassed, memory kept | bypassed, memory cleared | back in, from a clean state |
| buffer | pauses the recording | ends the take | records a new take |

---

## 14. Scheduling: `at`, `after`, `by`, `and`, `then`, `thenif`, `wait`

By default a line takes effect immediately. To land it on a beat, end
the line with:

| Ending | When it applies |
|---|---|
| `at N` | On the next multiple of N beats. |
| `after N` | N beats from now. |
| `... by CLOCK` | Count the beats on another clock. |

The beats are counted on the clock of the line's first target (for a
`clock` statement, on that clock itself).

```blast
start kick at 4                       # on the next bar (in 4)
start rim after 2 by main
start lead at 16 by main              # on main's next 16-beat boundary
clock main 120b at 4                  # change tempo on the bar
kick gain=0.2 and hat pan=-1 at 4     # both changes on the same beat
start pulse and stop hat at 4 by main
```

- **`and` joins statements** so they apply in the same audio instant, or
  not at all. `at`/`after` go at the **end of the whole line** and apply
  to all of it (they delay its first part; the rest of the chain follows
  from there). `&` was the old spelling; it is now error E105.
- **`then` runs the right side once the left side is done**: when the
  longest glide (`over`) the left side started is over, whatever
  happened to it (a retarget does not stop it). With no glides it runs
  in the same instant, in order. `and` binds tighter than `then`:
  `a gain=0 over 2b and b gain=0 over 3b then c gain=1` sets `c` after 3 beats.
- **`thenif` runs the right side only if the left side was not cut short.**
  If a glide it started was retargeted (another `over`, or a plain set)
  before it ended, or an object it names is gone, everything to the
  right of the `thenif` is dropped and warning W105 says which glide
  ("thenif: the glide on kick.gain was cut short; the rest of the line was
  dropped"). A left side whose voices were not playing jumped, so it is
  never cut short. `then` and `thenif` have the same precedence and group
  from the left: `a then b thenif c`.
- **`wait D [by CLOCK]`** is a part that does nothing and is done after D.
  A bare number and `b` are beats of the clock (`main` unless `by`); time
  units (`500ms`, `2s`) are clock time, so a paused clock holds the wait.
  `wait 4b then start lead` works at the prompt. A `wait` needs a `then`
  and something to run after it (a lone `wait` is E201 until timed
  scripts); `wait D and x` is read as `wait D then x` (W106).
- `at`/`after`/`then`/`thenif`/`wait` can't be used inside a block
  (E204).
- **Creating and removing can wait** after a `then`/`thenif`, after
  `wait D then`, and in a script after a `wait`: voices, groups (made,
  dissolved with `ungroup`, removed, members joined or left), clocks, seqs,
  lfos, envs, effect inserts and waves. A plain `at`/`after` line, and the first part
  of a line that ends in `at`/`after`, still can't (E410). Removing an effect or a wave,
  buffers, tracks and moving onto a clock of its own can't wait yet (E410).

      drums gain=0 over 8 thenif remove drums   # the group goes when the fade ends
      wait 4 then voice x from "t" on main and start x

- Until its line runs, a **new object** isn't in `state`, and a line typed now
  can't name it (E411 "'x' doesn't exist yet (a waiting line creates it)"); a
  line that waits can.
- An object of any kind **whose removal waits stays there under its name** for
  every line typed now, until the removal runs: `state` shows it, `kick gain=1`
  works, `remove kick` removes it at once (the waiting line is dropped, W103).
  A set on the glide a `thenif` watches cuts the line short (W105): the removal
  is dropped and kick stays. Creating the same name with its kind word
  (`voice kick from "x"`, `clock q 100b`, `kick { seq beat … }`) is E302 while
  the removal waits; `kick.beat 1 4` edits as usual.
- For a line that waits (and later script lines) the object is already gone and
  its name is free: a later part may create a new `kick` (it gets a new ID; the
  old ID stays taken until the removal runs, and the limits count it). If the
  removal is dropped instead, the old one stays and is called `kick_` when the
  new `kick` exists. A waiting group removal keeps its members in the group
  until it runs.
- When a waiting line is dropped, what it would have made is forgotten and what
  it would have removed stays (W103 "the waiting line '…' did not run: z was not
  created, kick stays"). A pipe or buffer source that a removal clears on
  another object is not put back.
- If something a waiting line refers to gets removed before it fires,
  that part of the line is dropped with warning W103 (after a `then`,
  only the statement that names it; the rest of the chain still runs).
- A line with `then`s counts every waiting piece against the table (E601).

```blast
clock main 128b and hat on main                    # retime and move hat in one instant
snare gain=0 over 2b then stop snare               # fade, then stop
kick gain=0 over 4b thenif start lead              # lead only if the fade ran its course
wait 4b then start lead                            # four beats of main from now
snare gain=0 over 1b then wait 2b then snare gain=1
```

---

## 15. LFOs

An **lfo** is owned by a voice or group. It adds a wave to one parameter
(the parameter's base value doesn't change).

```blast
kick { lfo wob to=pan depth=0.8 freq=2 }               # pan swings ±0.8, one cycle per 2 periods
kick { lfo vib to=pitch depth=0.5 freq=5hz }           # vibrato: ±0.5 semitone at 5 Hz
drums { lfo swell to=gain depth=0.5 offset=0.5 freq=8b }  # group gain moves 0..1 over 8 beats
kick { lfo sweep to=lo.cutoff depth=2 freq=8b }        # an effect parameter: ±2 octaves
```

### LFO settings

| Setting | Short | Values | Default | Glides | Meaning |
|---|---|---|---|---|---|
| `to` | `t` | a parameter name | **required** | no | What it modulates. Voice: `gain pan rate pitch stretch start end loopstart loopend slice intonation`. Effect: `FX.PARAM`, e.g. `lo.cutoff`. |
| `depth` | `d` | -1000..1000 | **required** | yes | Peak deviation in the target's own units (a cutoff: octaves; pitch: semitones, written `7`, `7st` or `50c`; a ratio is not allowed here). |
| `freq` | `f` | see below | 1 | yes | Speed. |
| `shape` | `s` | `sine tri square saw sh` | sine | no | `saw` rises; `sh` is sample-and-hold, a random value each cycle. |
| `offset` | `o` | -1000..1000 | 0 | yes | Added to the wave (`depth=0.5 offset=0.5` swings 0..1). |
| `phase` | `p` | 0..1 | 0 | no | Where in the cycle it starts (0.25 = a quarter in). |
| `seed` | `r` | whole number | random | no | Makes `sh` repeat. |

**`freq`:** `5hz` means cycles per second (0.001..200 Hz). Any other value is
**the length of one cycle**: a bare number in periods of the owner (`0.5`
means two cycles per period), `4b` in beats, or `250ms`, `2s`, `4800f`. An
LFO doesn't take `in` or `on`; it follows its owner's clock.

```blast
kick.wob depth=0.3 over 4       # glide the depth
kick.wob freq=5hz shape=tri
kick.wob freq=4b shape=square phase=0.25
kick.wob shape=sh seed=3        # repeatable random steps
pause kick.wob                  # hold the current value
resume kick.wob
stop kick.wob                   # take the modulation away
start kick.wob                  # run again from phase
remove kick.wob
```

---

## 16. Envelopes

An **env** is owned by a voice or group. It fires every time its owner is triggered
(a `start`, a seq step, a `trig`, a `melo` note) and shapes one parameter.
On `gain` it multiplies; on any other target it adds `depth × level`.

```blast
kick { env amp attack=5ms decay=200ms sustain=0 }              # a percussive gain envelope
kick { env pe to=pitch depth=12 decay=50ms sustain=0 }          # pitch drop: +12 st falling to 0
snare { env gate decay=0.25 sustain=0.3 hold=0.5 release=100ms } # ADSR with a timed hold
kick { env fenv to=lo.cutoff depth=3 decay=100ms sustain=0 }   # filter envelope: +3 octaves
```

### Envelope settings

| Setting | Short | Values | Default | Meaning |
|---|---|---|---|---|
| `attack` | `a` | duration | 0 | Rise from the current level to 1 (always linear). |
| `decay` | `d` | duration | 0 | Fall from 1 to `sustain`. |
| `sustain` | `s` | 0..1 | 1 | Level held after the decay. |
| `hold` | `h` | duration or `retrigger` | retrigger | How long the sustain lasts before the release. `retrigger` means until the next trigger; there is no note-off. |
| `release` | `r` | duration | 0 | Fall from sustain to 0 after the hold. |
| `to` | `t` | a parameter name | gain | `gain`, or anything an LFO can target (`pitch`, `lo.cutoff`, ...). |
| `depth` | `p` | -1000..1000 | 1 | On gain, 0..1 (1 means silence between envelopes). Elsewhere, the amount added at full level (cutoff: octaves; pitch: semitones, so `12`, `12st` or `50c`). |
| `curve` | `c` | `lin` `exp` | exp | Shape of decay and release. `exp` reaches -60 dB at the end of the stage. |

Durations: a bare number is a fraction of the owner's period; also `2b`,
`5ms`, `480f`. Changes take effect from the next trigger.

```blast
kick.amp decay=400ms curve=lin
snare.gate hold=retrigger
pause kick.amp     # hold the current level
stop kick.amp      # take the envelope's effect away
start kick.amp     # wait for the next trigger
remove kick.amp
```

---

## 17. Effects: filter, drive, crush

Effects belong to a voice or group and run **in the order you write
them**. Each one is named, so you can edit it later as `owner.NAME`.

```blast
kick { filter lo type=lp cutoff=800 q=2 }
kick { drive dirt amount=24 mix=0.5 }
kick { crush bit bits=6 down=4 mix=0.7 }
drums { filter dark cutoff=1200 }           # on a group's mix

kick.lo cutoff=4000 over 4                  # glide a parameter
kick.lo type=hp                             # change type (restarts the filter)
kick.bit bits=4
drums.dark cutoff=600 over 2
```

### `filter`: a biquad

| Setting | Short | Values | Default | Glides | Meaning |
|---|---|---|---|---|---|
| `type` | `t` | `lp hp bp notch` | lp | no | Lowpass, highpass, bandpass, notch. |
| `cutoff` | `c` | 20..20000 Hz | 1000 | yes | Corner or centre frequency. |
| `q` | `q` | 0.1..24 | 0.707 | yes | Resonance. |
| `base` | `b` | 0..1, or dB | 0 | yes | How loud the filtered-out part stays. 1 = the filter does nothing. A `base` makes shelves and bells: `type=lp base=-12db` is a high shelf cut. |
| `mix` | `m` | 0..1 | 1 | yes | Dry/wet. |
| `gain` | `g` | 0..8, or dB | 1 | yes | Output level, applied last. |

### `drive`: tanh saturation

| Setting | Short | Values | Default | Glides | Meaning |
|---|---|---|---|---|---|
| `amount` | `a` | 0..48 dB | 12 | yes | Gain into the shaper. |
| `mix` | `m` | 0..1 | 1 | yes | Dry/wet. |
| `gain` | `g` | 0..8, or dB | 1 | yes | Output level. |

### `crush`: bit and sample-rate reduction

| Setting | Short | Values | Default | Glides | Meaning |
|---|---|---|---|---|---|
| `bits` | `b` | 1..24 (fractions allowed) | 8 | yes | Bit depth. |
| `down` | `n` | 1..64 | 1 | no | Hold each sample this many frames. |
| `mix` | `m` | 0..1 | 1 | yes | Dry/wet. |
| `gain` | `g` | 0..8, or dB | 1 | yes | Output level. |

### Modulating effects

LFOs and envelopes reach effect parameters as `FX.PARAM`:

```blast
kick { lfo sweep to=lo.cutoff depth=2 freq=8b }
kick { env fenv to=lo.cutoff depth=3 decay=100ms sustain=0 }
```

If you remove an effect, the LFOs and envelopes that target it are
removed too (warning W102).

### Bypassing

```blast
stop kick.dirt      # bypass and clear its memory
start kick.dirt     # back in, from a clean state
pause kick.bit      # bypass, keep its memory
resume kick.bit
remove kick.bit
```

---

## 18. Waves: oscillators

A voice can make sound as well as play it. `wave` adds an oscillator to a
voice's block (up to 8 per voice). A voice that holds only waves needs no
`from`: it is a synth. Everything a voice has applies to its waves: `gain`,
`pan`, `pitch`, `rate`, effects, lfos, envs, seqs, `trig`, `melo`, `choir`,
groups, buffers that record it, `--export`.

```blast
voice organ {
  wave s1 shape=sine freq=220
  wave s2 shape=sine freq=a4          # a note name: 440 Hz
  wave s3 shape=saw freq=110 gain=-6db
}
start organ
organ.s2 freq=660 over 2                # glide it
organ { wave s4 shape=square pw=0.25 }  # add one later
wave organ.s5 shape=noise gain=0.2      # or by path
organ.s3 shape=tri                      # restating edits in place
```

A voice with a track and waves plays both, summed.

### Wave settings

| Setting | Short | Values | Default | Glides | Meaning |
|---|---|---|---|---|---|
| `shape` | `s` | `sine tri saw square noise` | sine | no | The waveform. Saw, square and tri are band-limited (Elliptic BLEP, after Signalsmith Audio); noise is white. Every shape but noise is silent at or above half the sample rate. |
| `freq` | `f` | 0.01..20000 Hz, `220hz`, or a note (`a3`, `c#4`, `eb2`) | 220 | yes | Pitch. Notes are 12-tone equal tempered with A4 = 440 Hz and are stored as Hz. |
| `gain` | `g` | 0..8, or dB | 1 | yes | Level. |
| `phase` | `p` | 0..1 | 0 | no | Where in the cycle a retriggered wave starts (1 is the same as 0). |
| `pw` | `w` | 0.01..0.99 | 0.5 | yes | A square's pulse width. |
| `retrig` | `r` | `yes no` | yes | no | `yes`: every trigger (a start, a seq step, a `trig`, a `melo` note) restarts the phase. `no`: the wave runs free. |
| `fm` | `m` | a wave of the same voice, several joined by `+` (`a+b`), or `none` | none | no | The wave(s) that phase-modulate this one (see [FM](#fm)). A noise wave ignores it. |
| `index` | `i` | 0..32 | 1 | yes | FM depth in radians: the modulator's output (its gain included) times this. `0` is the plain carrier. |
| `fb` | `b` | 0..4 | 0 | yes | Feedback: the wave modulates itself, in radians (see [FM](#fm)). |
| `ratio` | `a` | 0.01..64, or `none` | none | yes | The frequency as a multiple of the voice's note instead of an absolute `freq`; a fraction such as `3/2` is kept as typed. `none` goes back to `freq`. |
| `out` | `u` | `yes no` | yes | no | `no`: the wave is not heard (a modulator only). |

The voice's `pitch` (a ratio, `st` or `c`) and `rate` multiply every wave's
frequency: `pitch=2` or `pitch=12st` is an octave up. `melo` sets the pitch,
so a melody plays on a synth voice with no new syntax (60 is the note as
written; 72 is an octave up):

```blast
voice bass gain=0.5 {
  wave b shape=saw freq=a1
  filter lo type=lp cutoff=500
  env amp attack=3ms decay=300ms sustain=0
  seq line 1 2 3 4 in 4 | melo bass 60 63 67 72
}
```

A wave sounds while its voice runs and keeps sounding after a track's region
ends: shape it with an `env` (a wave never stops by itself; `stop` does).
With `choir`, each instance plays every wave with its own phase and the
choir's detune.

### Modulating waves

LFOs and envelopes reach a wave's `freq`, `gain`, `pw`, `index` and `ratio`
as `NAME.PARAM`. `freq` and `ratio` move in octaves (an lfo `depth=1` is one
octave); `index` adds; an env on `gain` multiplies, as on a voice.

```blast
organ { lfo wob to=s3.freq depth=0.05 freq=5hz }
organ { env pluck to=s1.gain decay=300ms sustain=0 }
```

If you remove a wave, the LFOs and envelopes that target it are removed too
(W102).

### FM

One wave can phase-modulate another. `fm=NAME` makes the wave NAME (of the
same voice, made earlier) the modulator of this one, the carrier; `index` is
the depth in radians. A wave used only as a modulator is not heard: `out=no`.

```blast
voice bell { wave m ratio=3.5 out=no; wave c fm=m index=4 }
bell.c index=0 over 2        # the bell's brightness dies away
bell.c fm=none               # the carrier plays plain again
```

`ratio` puts a wave's frequency at a multiple of the voice's note, which is
220 Hz times the voice's `pitch` (so a `melo` note moves it): `ratio=1` is
`freq=220`, and `ratio=2` is an octave above whatever the voice plays. `freq`
and `ratio` are one or the other: setting one switches to it, and both in one
statement is E402 ("freq or ratio, not both"). `ratio=none` goes back to
`freq`. An lfo or env on `freq` still multiplies a ratio wave.

`ratio` is an ordinary number, so `ratio=3/2` and `ratio=(7/4)` work, and
`state` prints them as typed (`ratio=3/2`). A value that uses a `$variable` is
printed as its number, because the variable may not exist where the state is
replayed. `ratio=none` and `freq=` forget the text.

**Several modulators.** `fm=a+b` (no spaces around the `+`) lets two or more
waves modulate one carrier: its phase is pushed by the sum of their outputs
times its `index`, and each modulator's own `gain` sets its share.
`fm=a+b` replaces the whole set, `fm=none` clears it, and a name twice is
E402. Any wave in the set that would loop back is E412.

```blast
voice ep { wave a ratio=1 out=no; wave b ratio=14 out=no gain=0.3; wave c fm=a+b index=2 fb=0.5 }
```

**Feedback.** `fb=` (0..4, radians, glides) makes a wave modulate itself:
its own last two samples, averaged, push its phase (as on a DX7). It is
computed before the wave's `gain`, so it doesn't change with the level. `fb=0`
is the plain wave; a sine with `fb` grows saw-like harmonics. A noise wave
ignores it. `fm=` naming the wave itself is E412: use `fb=` for feedback.
lfos and envs can target `w.fb`.

A wave can modulate several waves; chains work (`c` modulates `b` modulates
`a`). Modulators are computed before the waves they modulate. A loop (`a.fm=b`
while `b` follows `a`) is E412; `fm=` of a wave the voice doesn't have is E405.
**Removing a modulator is fine**: it drops out of its carriers' sets, and a
carrier left with none plays plain. The one exception is a wave that a
waiting line (`at`, `after`) will name in an `fm=`: removing it is E407 ("pad.c is
named by a waiting fm= line") until that line has run, or you remove the
carrier first. Changing a carrier's modulators while it sounds doesn't click:
the phase stays where it is and only the frequency's motion changes. `stop` on a modulator is fine too. With
`choir`, each instance's carrier reads its own instance of the modulator.
`state` prints a voice's waves modulators first, so it replays.

Modulation is a change of the carrier's phase by the modulator's output
(one sample late), so saw, square and tri carriers are band-limited as they
are without FM. Deep, fast modulation past the sample rate's half aliases by
nature.

### Starting, stopping, removing

```blast
stop organ.s1      # silence it and put its phase back
start organ.s1
pause organ.s2     # silence it, keep its phase
resume organ.s2
remove organ.s3
```

In waiting lines a wave behaves like an effect: a waiting line that names a
wave is dropped (W103) if the wave is removed, and `thenif` watches wave
glides. A wave can't be added in the first part of an `at`/`after` line, and
can't be removed in a part that waits (E410), as for effects.

A voice with no track or buffer keeps at least one wave: `remove organ.s1` on
its last wave is E407; remove the voice instead, or add another wave first.

### Table waves

`wave t shape=table from "pad" slices=8 pos=0.3` plays single cycles cut from
a track (`from "name"`, or `from="name"`) or a buffer (`from name`, or
`from=name`). The source is averaged to mono and cut into `slices` equal parts
(default 1; 1..256). Each part is resampled to one cycle of `frames` samples
(a power of two, 256..4096, default 2048), has its DC removed, and is
band-limited into mip levels by FFT: level 0 keeps harmonics 1..frames/2-1 and
each level halves that. The reader picks the level for the pitch it plays
(fading into the next over the top tenth of each level's range), so no
partial passes 90 % of Nyquist.

```blast
voice pad gain=0.35 {
  wave t shape=table from="pad" slices=8 freq=110 pos=0
}
start pad
pad.t pos=1 over 8b                     # morph through the 8 cycles
pad.t freq=880 over 4b pos=0 over 4b    # each setting has its own over
```

`pos` (`o`, 0..1, glides) moves among the cycles: 0 is the first, 1 the last,
between two it crossfades. An lfo or env can target `t.pos` (added linearly).
`freq`, `gain`, `phase`, `retrig`, `pitch`/`rate` and the voice's effects work
as on any wave. A table wave with no table, or whose pitch is at or past
Nyquist, is silent.

The table is built when the wave is made, and again when `from` is restated
(`voice.t from=BUF` re-reads a buffer as it is then), or `slices` or `frames`
change. Moving `pos`, `freq` and so on does not rebuild it. A buffer being
recorded is read as it is at that moment (what has been heard so far); a
table doesn't follow the take. A script is checked whole before it runs, so a
table can't be cut from a buffer that the same script records. Removing the
source track or buffer leaves the table playing (it owns its data); `state`
then prints comments where `from` was, and for lines that depended on that
wave.

A table cut from a buffer with nothing recorded is allowed: the wave is made
with no table and stays silent, with W109 (`b is empty: t stays silent until
from=b is restated`). Restate `voice.t from=b` after recording to build it.
This is also how a `state` that holds a buffer table replays into a new
session. A track with no frames is E402.

The tables are built with a scalar FFT, so the same script renders to the
same bits on every machine.

**Level-0 images.** Tables are read with 4-point Hermite interpolation. A
table dominated by one high harmonic, played below about 21 Hz (where level
0, with every harmonic up to frames/2-1, is in use), shows interpolation
images about 30 dB under that partial; for example a table that is a
harmonic-440 sine, read at 4 Hz, has an image at (2048-440)·4 = 6432 Hz. Rich
tables (a saw, noise, a pulse) are unaffected: measured non-harmonic floors
are -87 to -93 dB.

Errors: `from`, `slices` or `frames` on a wave that isn't `shape=table` is
E402; `shape=table` without `from` is E405; `frames` that isn't a power of two
from 256 to 4096, or `slices` outside 1..256, is E403; a table over 16 M
samples (`slices × levels × frames`) would be E403 "table too large: fewer
slices or frames" (the language's own ranges stay under it).

### Settings that need a recording

`start`, `end`, `loopstart`, `loopend`, `slices`, `slice`, `stretch`, `loop`,
`bounce`, `shift`, `window` and `resample` mean nothing to a voice of waves only. They
are accepted and kept, with W108 (`organ plays waves only: start does
nothing`). Giving the voice a `from` later (`voice organ from "x"`) makes it
play the track as well as its waves.

## 19. Buffers: recording and resampling

A **buffer** is memory that records audio. Voices can play a buffer, even
while it is still recording. Optionally, each finished take can be written
to a WAV file or turned into a track.

```blast
buffer take from=out length=4b on main      # 4 beats of main's output
start take at 4                             # record on the next bar
voice re from take                          # a voice that plays the buffer
re { seq b 1 3 in 4 | trig }
```

### Buffer settings

| Setting | Short | Values | Default | Meaning |
|---|---|---|---|---|
| `from` | `f` | `out`, a voice, or a group | out | What gets recorded. `out` is the whole mix (before the output clamp); a voice is recorded after its effects, pan and gain; a group is its mix after effects and gain. |
| `length` | `l` | duration | 30s | Size: `30s`, `500ms`, `48000f`. `4b` (or a bare `4`) counts beats of the `on` clock, so it needs `on CLOCK`. |
| `loop` | `o` | yes/no | no | `yes`: when full, keep recording from the beginning until `stop` or `loop=no`. |
| `live` | `v` | yes/no | no | `no`: voices hear the new take only once it ends (uses twice the memory). `yes`: voices hear each frame as soon as it is written. |
| `bits` | `b` | 8, 16, 24, 32 | 32 | Stored precision; lower values are quantised with dither while recording. |
| `to` | `t` | string | none | What happens to each take when it stops. `"name.wav"` (anything with an extension or a `/`) writes a WAV file, overwriting it; a path is relative to the working directory, `~/` works, and the folder must already exist (E708). `"name"` creates or replaces a track. `""` turns output off. |

`on CLOCK` (a clock name) sets the clock that `b` lengths count on.

```blast
buffer loopy from=kick length=2s loop=yes live=yes bits=16   # a live looping sampler of kick
start loopy
voice re2 from loopy rate=0.5 loop=forward                   # half-speed playback of it

buffer saved from=drums length=8b on main to="drums.wav"   # save each take to a file
buffer grab length=1s to="captured"                        # each take becomes track "captured"
start grab                                                 # once it ends: voice c from "captured"

pause loopy     # pause recording
resume loopy
stop loopy      # end the take (a take with to= is written now)
remove loopy
```

- A restated `length`, `live` or `bits` takes effect at the next `start`,
  which allocates fresh memory and moves the voices playing the buffer to
  it.
- When a file or track is written, a `buf` notice is printed (for example
  `buf take: wrote take.wav (0:42, 8.1 MB)`).
- Takes that are still running when you quit are written out first.
- BLAST only opens an output stream, so there is no microphone or line
  input.

---

## 20. Variables: `let`

```blast
let bpm = 96
let tempo = 96b
let half = ($bpm / 2)
let fade = 250ms
clock main $tempo
clock main ($bpm * 2)b
kick gain=($half / 100)
kick gain=0 over $fade
```

- A value can be a number, a clock rate or a duration. Use it as `$name`,
  or in an expression `( )` with `+ - * /`.
- Variables are replaced when a line is parsed, so objects store plain
  numbers, and `state` shows those numbers.
- Variables last for the whole session. `let` again to change one.
- Write `$tempo` with the unit inside the variable, or `($bpm)b`. The old
  `b$bpm` form doesn't work.

---

## 21. Scripts: `source`, `check`, `--script`

A script is a text file of statements, saved as `NAME.blast` in the
sources folder (default `sources/`, or set it with `--sources`).

```blast
# sources/groove.blast
clock main 124b
voice kick from "kick"
kick { seq pat 1 2 3 4 in 4 | trig }
voice hat from "shaker" gain=0.4
hat { seq off 1.5 2.5 3.5 4.5%40 in 4 | trig }
group drums { kick; hat }
```

| Command | Does |
|---|---|
| `source groove` | Run `sources/groove.blast`. |
| `source "path/to/file.blast"` | Run a file by path (quoted). |
| `source groove read` | Print the file without running it. |
| `source groove history` | Add the file's statements to your history (Up arrow) without running them, so you can step through and fire them one at a time. Prints `history: 6 statements from groove.blast`. |
| `source groove stop` | Cancel a running timed script: its unsent lines and waiting entries go; nothing is stopped. |
| `end groove` | End a running timed script as its own `end` would (stops what it started). |
| `check groove` | Check the whole file against the current session; nothing changes. Prints `ok` or every error. |
| `blast --script groove` | Run it at startup, before the prompt. |

How a script runs:

1. **The whole script is checked first**, against a copy of the session.
   Every error is reported, with the file, line and column, and if there
   is any error nothing is applied.
2. Then the lines apply in order, each as its own batch. So a script can
   use `at` / `after` to schedule things.
3. Scripts can `source` other scripts, up to 16 deep. Cycles are refused
   (E702).

### Timed scripts: `wait` and `end`

A line of a script that is just `wait D [by CLOCK]` is a **lone wait**. In a
script a newline is not `then`: the lines run together until the next wait,
and the lines after it run D later. Waits add up (the script has a cursor).
A bare number is beats of `main`, `by CLOCK` counts another clock, and
`500ms`, `2s` are clock time at that clock's rate.

```blast
# sources/build.blast
start kick
wait 16
start hat
wait 16 by main
drums gain=0 over 8 then end
```

- A script with a lone `wait` or an `end` **runs in the background**: the first
  segment runs at once and the prompt comes back. Each later segment is counted
  from the frame the previous one actually fired, so waits add up without drift;
  a paused clock delays everything after it.
- The runner sends at most two segments beyond the one that sounds, so `state`
  can show up to two segments early. A wait shorter than the command thread's
  round trip (a few ms) arrives late and fires at once.
- **`end`** stops everything the script started with `start` (that still
  exists) and cancels its remaining lines; the session stays open. Lines after a
  written `end` never run. `end` can close a chain (`... then end`).
  `end after D` (a line of its own) means `wait D`, then `end`.
- Without a written `end`, a script **ends implicitly** when its last segment
  fires: it leaves the list and stops nothing.
- At the prompt, `end` alone is E206; `end NAME` and `source NAME stop` are
  prompt-only (E201 in a script). `end` is a reserved word.
- A line of a later segment that no longer works when its turn comes (it names
  something you removed) is skipped with W107; the rest of the segment runs. If
  the clock a wait counts on is removed, the script is cancelled.
- Up to 8 scripts run at once (E601 beyond). On a cancel, `state` keeps the
  changes that had already been sent.
- Lines after a `wait` may create and remove objects (see §14); a later line
  of the same script may name them. `source NAME stop` drops what has not run
  yet: what it would have created is forgotten, what it would have removed stays.
- A script with no lone wait and no `end` runs exactly as described above.

```text
src/broken.blast:1:16: E301 unknown track 'nope'
```

### Saving the session as a script

```blast
state to snapshot      # writes sources/snapshot.blast
source snapshot        # rebuilds it later (on a fresh session)
```

---

## 22. Queries: `state`, `help`, `clear`, `quit`

| Command | Does |
|---|---|
| `state` | Print the whole session as a script. |
| `state kick` / `state drums` / `state take` | Print one object (with what it owns). |
| `state to NAME` | Write the session to `sources/NAME.blast`. |
| `help` | List the kinds, actions and script words. |
| `help KIND` / `help ACTION` / `help WORD` | One page: `help voice`, `help seq`, `help lfo`, `help start`, `help source`. `help gain` tells you which kinds have that setting. |
| `clear` | Clear the terminal window. |
| `quit` | Leave BLAST. Ctrl-C twice on an empty prompt does the same. |

While timed scripts run, `state` ends with a comment line, `# scripts: demo  line 12, next wait 4 by main; ...`.

`state` output is valid input. It leaves out defaults and any `on` that
just follows the parent. The order is: clocks, buffers, groups with their
members, ungrouped voices, then each owner's processes and effects.

```text
> state
clock main 124b
group drums {
  voice kick from "kick"
  voice hat from "shaker" gain=0.4
}
kick { seq pat 1 2 3 4 }
hat { seq off 1.5 2.5 3.5 4.5%40 }
```

---

## 23. History

- Every line you run is saved, and survives restarts. A multi-line block
  is saved as one entry. Empty lines and immediate repeats aren't saved.
- The history file is `$XDG_STATE_HOME/blast/history`, or
  `~/.local/state/blast/history` if `XDG_STATE_HOME` isn't set. It keeps
  the last 1000 entries, one per line (multi-line entries are stored with
  `\n` escapes). To see your full history outside BLAST:

  ```sh
  cat ~/.local/state/blast/history
  tail -n 50 ~/.local/state/blast/history
  ```

- Inside BLAST, press **Up/Down** to browse, and **Ctrl-R** to search (see
  [the prompt](#2-the-prompt-keys-history-completion)).
- `source NAME history` loads a script's statements into history without
  running them, which is handy for a prepared set you want to play line
  by line.
- `--agent` mode has no history.

BLAST has no command that prints the history inside the session; use Ctrl-R
or the file above.

---

## 24. Errors and warnings

Errors are reported as `[file:]line:col: CODE message (suggestion)`.

| Code | Meaning |
|---|---|
| E101 | Unclosed string |
| E102 | Bad number |
| E104 | Unexpected character (e.g. `$` not followed by a name) |
| E105 | `&` is now `and`; `&&` is now `then` |
| E201 | Unexpected token |
| E202 | Block never closed / file ends mid-statement |
| E203 | `\|` without a receiver |
| E204 | `at`/`after`/`then`/`thenif`/`wait` inside a block |
| E205 | `=` without a value |
| E206 | `end`: no script is running, or name the script (`end demo`) |
| E207 | export: the script ends at once; add a wait, `end after`, or `--length` |
| E301 | Unknown name (with a "did you mean"); also "no script 'NAME' is running" |
| E302 | Name already used by another kind; also "'kick' is still in use: a waiting line removes it" |
| E303 | Reserved word used as a name (including `and`, `then`, `thenif`, `wait`, `end`) |
| E304 | Ambiguous path |
| E401 | Unknown setting (lists what the kind takes) |
| E402 | Wrong type, or a combination that isn't allowed (e.g. `over` on a setting that can't glide, `jitter` together with `late`) |
| E403 | Out of range (also division by zero) |
| E404 | Not built yet (names the task) |
| E405 | Missing item (e.g. a voice with no track, buffer or wave: `from` on create) |
| E406 | Step outside the cycle |
| E407 | In use (e.g. removing a track a voice plays, the last wave of a voice with no track or buffer, or a wave that a waiting `fm=` line names) |
| E408 | Import failed |
| E409 | Clock is stopped or new; it can't glide |
| E410 | Creating or removing can't wait for a beat (`at`/`after`); or "that can't wait yet: buffers, tracks, removing an effect or a wave and moving onto a clock of its own are not staged" |
| E411 | 'x' doesn't exist yet (a waiting line creates it) |
| E412 | A wave's `fm=` would make a loop of modulators (`a.fm=b would make a loop: b already follows a`) |
| E501 | Unknown clock |
| E502 | Nothing to trigger (a top-level seq without a pipe) |
| E503 | `main` can't be removed |
| E504 | Bad receiver target |
| E601 | Limit reached |
| E602 | Command queue full |
| E701 | Source not found |
| E702 | Source cycle |
| E703 | Sources nested too deep (over 16) |
| E704 | Couldn't write a file (`state to`) |
| E708 | `to=` folder doesn't exist |
| E709 | Not enough memory for a buffer |
| W101 | The statement changes nothing ("already as stated") |
| W102 | LFOs or envelopes removed along with the effect they targeted |
| W103 | A waiting (`at`/`after`/`then`) line, or one statement of it, was dropped because what it names was removed |
| W105 | `thenif`: a glide on the left was cut short (or an object is gone), so the rest of the line was dropped |
| W106 | `wait D and x` does nothing joined with `and`; read as `wait D then x` |
| W107 | A timed script's line was skipped when its turn came (`script NAME line N skipped: WHY`) |
| W108 | A voice of waves only was given a setting that needs a recording (`organ plays waves only: start does nothing`) |
| W109 | A table wave was cut from a buffer with nothing recorded: it has no table (silent) until `from=BUFFER` is restated |

Examples:

```text
kick gian=0.5      -> E401 voice has no setting 'gian' (did you mean 'gain'?)
kcik gain=0.5      -> E301 unknown name 'kcik' (did you mean 'kick'?)
kick { seq x 0 1 } -> E406 step 0: counting starts at 1
seq lonely 1 3     -> E502 nothing to trigger ... (seq x 1 3 | trig kick)
remove main        -> E503 main is the session's clock and can't be removed
```

---

## 25. Quick reference

```text
STATEMENT  [kind] name [from "track"] [on clock] [in N] [k=v [over D] ...] [{ ... }] [| recv, ...] [at N | after N [by clock]]
JOIN       a and b            same instant      ;  or newline ends   # comment   \ continues
           a then b           b after a's longest glide is over, whatever happened
           a thenif b         like then; dropped (W105) if a glide of a was cut short
           wait D [by clock] then b     b after D (beats of main; 500ms is clock time)

KINDS      clock   NAME RATE                     clock main 124b
           track   "NAME" from "file"            track "bass" from "~/bass.wav"
           voice   NAME from "track"|buffer      voice kick from "kick" gain=0.8
           group   NAME { members }              group drums { kick; hat }
           seq     NAME steps [in N]             kick { seq pat 1 2 3 4 in 4 }
           packet  NAME { seqs } | pipe          packet f { seq a 4; seq b 3 } | trig snare
           lfo     NAME to= depth= [freq=]       kick { lfo wob to=pan depth=0.5 freq=2 }
           env     NAME [attack= decay= ...]     kick { env amp decay=200ms sustain=0 }
           filter  NAME [type= cutoff= q=]       kick { filter lo cutoff=800 }
           drive   NAME [amount=]                kick { drive dirt amount=24 }
           crush   NAME [bits= down=]            kick { crush bit bits=6 }
           wave    NAME [shape= freq= gain= pw= fm= index= fb= ratio= out=] voice organ { wave s1 shape=saw freq=a2 }
           buffer  NAME [from= length= to=]      buffer take length=4b on main

PIPES      | trig [target]     | melo voice 36 43 ...

ACTIONS    start stop pause resume   NAME... | all
           remove NAME... | "track" | all        ungroup group[.member] | all     rescan

TIME       124b 500ms 24000f (clocks)   0.5 2b 1.5s 5ms 480f (durations)   5hz (lfo)
STEPS      3  2.5  1..4  4%50  2~0.05

SCRIPT     let x = 3   $x   ($x * 2)   source NAME [read|history|stop]   check NAME
           wait D [by clock]   (a line of its own: the script's cursor)   end   end after D   end NAME
QUERIES    state [path] [to NAME]   help [word]   clear   quit
KEYS       Up/Down history   Ctrl-R search   Tab complete   Ctrl-C x2 quit
```
