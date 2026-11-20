# BLAST: Bash-Like Audio Scripting Tool

This project aims to facilitate real-time algorithmic audio in Rust. It features a Bash-like scripting language that is interpreted by a REPL.

## Commands

[`docs/COMMANDS.md`](docs/COMMANDS.md) documents every command, with examples: clocks and tempo, tracks, voices, groups, seqs, pipes, glides, scheduling, LFOs, envelopes, effects, buffers, variables, scripts, `state`, `help` and history.

## Dependencies

One of the goals of this project is to realize as many features as possible with as few dependencies as possible. It uses vetted crates for the heavy parts (`cpal` for audio output, `rtrb` for queues, `lexopt` for the command line, `symphonia` and `rubato` for decoding and resampling), and `libc` for terminal internals until the terminal moves to `crossterm`.

## Modules

**src/main.rs**:
- executes audio parsing library functions through extension matching
- catches unsupported audio formats

**src/lib.rs**:
- exposes modules to main.rs and hosts testing

**src/audio_processing**  
- pre-parses all audio files in the assets/ folder
- opens the output device through cpal and picks a config (preferring the audio files' sample rate and channels, f32 samples) from a consensus based on the audio files' properties (namely sample rate and number of channels)
- processes Commands separately from audio thread for string parsing, hashmap operations, and robust error-handling
- uses terminal in raw mode for custom terminal rendering
- implements fast RNG with xoroshiro128+ generation, Lemire's fast modulo, and architecture-specific seeding
- commands/
  - mod: `Command` (with `Batch`), `Parsed`, `CmdProcessor::parse`/`submit`/`submit_batch`
  - errors: `CmdErr` (and the reserved name `all`)
  - queue: `cmd_queue`, `CmdSender`/`CmdReceiver` (the command queue to the audio thread, an rtrb pair)
  - reprs: the ID types, `IdPool`, `Target`/`ProcOwner`/`TempoOwner`/`TempoLink`, `EngineState` (the REPL-side mirror of the engine and the ID authority) and its lookups, `all_ids`
  - tempo: `TempoRepr` (with `ClockRun`, the mirror's copy of which clocks run), `TempoInit`/`TempoUse`, `make_clock`
  - voice, group, track, process: each family's mirror reprs and typed cores (`load_voice`, `make_group`, `transport`); `ProcRepr` keeps a seq's steps and period
  - procs/seq: `make_seq`, `SeqArgs` and `SeqStep`
  - procs/lfo: `make_lfo`/`edit_lfo`, `LfoArgs`/`LfoEditArgs` and the mirror's `LfoRepr` (`ProcRepr.lfo`)
  - procs/env: `make_env`/`edit_env`, `EnvArgs`/`EnvEditArgs` and the mirror's `EnvRepr` (`ProcRepr.env`)
  - fx: `FxArgs`/`FxOp`, the mirror's `FxRepr` (a voice's effects, in chain order) and the typed core `make_fx`/`set_fx_param`/`retype_fx`/`bypass_fx`/`remove_fx`
  - buffer: `BufferArgs`/`BufOp`/`SetTapsArgs`/`SetSourceArgs`, the mirror's `BufferRepr` and the typed core `make_buffer`/`renew_buffer`/`remove_buffer`/`set_buffer_source` (memory is built on the command thread); `to=` (RECc): `BufTo`, `BufSaves`, `note_buffer_save`, `finish_buffers`, `buffer_notices`: a finished take goes to a WAV file (via `export`) or a track
  - lang/: the language of `plans/design/REQ-056.md`, source text to engine commands
    - lexer, ast, parser: tokens, syntax tree and the recursive-descent parser (`parse`, `Parse::{Complete, Incomplete}`)
    - errors: `Span`, `LangErr` and the error codes
    - names: the session's one namespace and `lookup` (clocks, voices, groups, `group.voice`, `voice.seq`)
    - schema: what each kind and action takes, and the checks (`check_items`)
    - lower: a script lowered against the mirror into one `Command` (all or nothing), including seq steps
    - session: incomplete input kept between lines (`CmdProcessor::parse`, `pending`)
  - testing: shared test helpers
- engine/
  - mod: `Conductor` (one arena per kind), `prepare`/`new`, `apply`, `emit`/`retire`
  - events: `Event` and `Garbage` (what leaves the audio thread), `EngineRx` (the REPL ends of their queues), `describe`
  - render: `Conductor::render`, the interleaved f32 per-frame mix (processes, scheduled lines, tempos, mix, advance, clock)
  - schedule: `Schedule`, the preallocated table of lines waiting for their beat (`at`, `after`); `fire_due` applies them
  - voice, group: `Voice` (`render_frame`/`advance`) and `Group` (its members' IDs)
  - fx/: a voice's effect chain (`FxChain`, `FxKind`): `biquad` (lp, hp, bp, notch), `shape` (drive, crush); a `Vec` grown from the command thread, run over the voice's output frame
  - buffer: `BufData` (shared `AtomicU32` samples, one half live or two), `BufferSlot` (a take: position, pause, loop, dither), `BufSource`; voices play a buffer while it records (REQ-095); a take with `to=` also fills a private `SaveBox`, handed back as `Garbage::BufferSave`
  - tempo, process: `Tempo` and `ProcSlot`, what the tempo and process arenas hold
  - track: `Track`, decoded samples shared by every Voice as an `Arc<[f32]>`
  - arena: `Arena` (fixed-capacity slots indexed by ID) and `Limits`
  - handlers/: `apply`'s handlers, one file per command family
- runtime/
  - mod: `run_blast` (startup, the marker timer thread, the exit code)
  - audio: device and config choice, the cpal stream callback, `StreamStatus`
  - repl: `Repl`, the input thread's timed loop (keys, engine events, stream status)
  - terminal: `Term` (raw mode, the only path to stdout), `key_from`
  - line_editor: `LineEditor` and `Key`
  - agent: the `--agent` loop in the REPL's place (`handle_line` answers one request); `agent/json`: the small JSON reader and writer
- processes/
  - mod: the `Process` enum
  - seq: the `Seq` step sequencer
  - lfo: the `Lfo` low-frequency oscillator (adds a wave to one voice or effect parameter through the modulation bank)
  - env: the `Env` envelope (fires on every trigger of its owner and shapes one parameter: multiplies gain, adds elsewhere)

**src/cli.rs**: `parse_args` (lexopt): `--device`, `--list-devices`, `--script`, `--card` (print the language card and exit), `--agent` (JSON lines, below), `--export` (render a script to a WAV file, below), `--sources DIR` (default `sources/`; where `source NAME` looks and `state to NAME` writes), the assets directory

**src/export**:
- WAV recording: `start`/`start_with` open a file, allocate an `rtrb` ring of interleaved f32 and spawn the `blast-rec` writer thread; `RecHandle` reports `status`, `take_notices`, `request_stop` and `wait`
  - 32-bit float by default, or 16/24-bit integer (clamped, TPDF-dithered on the writer thread); the header is patched every second; takes continue in `name-002.wav` before the 4 GiB WAV limit
  - `naming`: default `YYYY-MM-DD_HH-MM-SS.wav` (UTC) names, part names, `next_free`
- `render`: `render_script`, the headless render behind `blast --export` (below): one thread, 64-frame blocks through `Conductor::run_block`, the runner's `pump` after each, and a `hound` writer

**src/file_parsing**:
- mod
  - `decode_file`/`decode_bytes`/`probe_file`: one entry point for every supported extension
  - `scan_assets`/`preferred_config`/`load_assets`: walks the assets folder and its subfolders (tracks named by path, e.g. `drums/kick`), picks a sample rate and channel count, then decodes, resamples and downmixes every file to it
  - `conform`: brings one decoded file to a rate and channel count
- symphonia
  - decodes WAV, AIFF/AIFC, FLAC, MP1/2/3, AAC (MP4/ADTS), ALAC and ADPCM through symphonia
  - fills corrupt packets with silence, so timing holds
- raw, au, pcm
  - headerless PCM (spec from the file name, e.g. `kick.s16le.48000.2.raw`) and Sun `.au`/`.snd`
  - shared sample converters, including G.711 A-law and µ-law
- riff_fixup
  - repairs WAV/AIFF quirks symphonia rejects (wrong sizes, `data` before `fmt `, `fact`/COMM frame counts)
- resample
  - offline sinc resampling of whole files (rubato)
- mix
  - `remap`: channel conversion, with ITU-R BS.775 downmixes to stereo or mono
- decode_helpers
  - implements custom DecodeErrors and DecodeResult for in-memory file parsing
  - provides AudioFile struct to return necessary data for audio APIs

## Driving BLAST from a program

`blast --card` prints the language card and exits. `blast --agent [ASSETS_DIR]` starts the session without the terminal UI: it reads one JSON object per line on stdin and writes one JSON object per line on stdout. Everything else (track list, output device, latency) goes to stderr, so stdout is only JSON lines.

Requests (unknown keys are ignored; `id` is any JSON number or string, echoed back, or `null` if missing):

```json
{"id": 7, "script": "voice kick from \"kick\"\nkick { seq pat 1 2 3 4 in 4 | trig }\nstart kick"}
{"id": 8, "check": "kick { lfo w depth=0.3 to=pan in 8 }"}
```

- `script`: checked whole against a copy of the session, then applied; if anything is wrong nothing is applied. What `state`, `help` and `source NAME read` print is in `output`.
- `check`: checked against a copy of the session, nothing applied.

Answers:

```json
{"id": 7, "ok": true, "output": "", "warnings": [{"code": "W101", "line": 2, "col": 1, "message": "..."}]}
{"id": 8, "ok": false, "errors": [{"code": "E401", "line": 1, "col": 13, "message": "...", "suggestion": "did you mean 'depth'?"}]}
```

An error may also carry `file` (when it is in a sourced file). A line that is not a JSON object with a string `script` or `check` is answered with code `J001` ("not a request: ..."); a failure that is not a language error (a full command queue) has line and col 0.

Notices have an `event` and no `id`:

```json
{"event": "warn", "message": "Warn: tempo not active for voice 'kick'"}
{"event": "buf", "message": "buf take: wrote take.wav (0:42, 8.1 MB)"}
{"event": "script", "ok": true, "output": ""}
{"event": "quit"}
```

`script` is the result of `--script`, which runs before the first request. `quit` (or `exit`) as a script, end of stdin, SIGTERM or a failed audio stream ends the session (running takes with `to=` are written first); `{"event":"quit"}` is the last line. History does not exist in agent mode (`source NAME history` is ignored).

## Rendering a script: `blast --export`

`blast --export finished.blast [out.wav]` renders a script to a WAV file without opening an audio device or a prompt, faster than real time. It drives the same engine, command processor and script runner as a live session (the audio callback and the export loop both call `Conductor::run_block`), on one thread, 64 frames at a time. The script is found like `--script`: the working directory, else the sources folder. `out.wav` defaults to the script's name with `.wav` in the working directory (`finished.wav`); an existing file is overwritten. `blast [ASSETS_DIR] --export x.blast [out.wav]` takes the assets folder first, as everywhere.

```sh
blast --export finished.blast                 # writes finished.wav
blast assets/ --export song.blast mix.wav --bits 24
blast --export loop.blast --length 64b        # exactly 64 beats of main
```

- **Length.** The render lasts until the script ends: its last `wait`, or a written `end` (the implicit `end` at the bottom stops nothing, so sounds still playing are cut at that frame), plus the glides still running at that moment, and every waiting line. The last block is cut to the exact frame. `--length 64b` (beats of `main`, at its tempo as the render starts), `--length 90s` (seconds) or `--length 64` (beats) renders exactly that instead. A script that ends at frame 0 (no `wait`, no glide, no `--length`) is refused with `E207`, and nothing is written.
- **Format.** 32-bit float at the assets' preferred rate and channel count, as in a live session; samples are written as rendered (overs are kept). `--bits 16|24` writes integers with TPDF dither; `--rate HZ` loads and renders at another rate.
- **Repeatable.** `--seed N` (default 0) fixes every random feature (`chance`, `jitter`, random choices, the dither), so the same script, assets and seed give the same bytes. `--seed` is accepted in a live session and ignored there.
- **Buffers.** Takes with `to=` are written as in a live session; a take still running when the render ends is stopped there and written before the program exits.
- **Output.** Progress goes to stderr once a second (`export: 12.0 s`, with `of 64.0 s` when `--length` fixes it), then a line with the path, frames and wall time. The exit code is 0 on success and 1 with the error otherwise; script errors print as `--script` prints them. `--export` cannot be combined with `--agent`, `--device`, `--list-devices` or `--card`.
- **Tests.** Golden SHA-256 hashes of two renders live in `blast/tests/golden/`; after an intended change to the sound, regenerate them with `BLAST_BLESS=1 cargo test --test export_golden`.

## The web UI: `blast server`

`blast server` serves BLAST in a browser: the same REPL (language, blocks, completion, history, `source`, `state`), the engine's output streamed to the browser, output devices switchable live, and panels for sources and assets. It is built for testing from a phone over Tailscale. The server is its own binary, `blast-server` (crate `blast_server`), which `blast server` runs; a workspace `cargo build --release` builds both.

```sh
blast server                 # serve in the foreground on 127.0.0.1:8800
blast server --bg            # detach; the log goes to the log file
blast server status          # running? on which URLs?
blast server stop
blast server config          # where the config file is
```

`--bg` runs it in the background; it is a flag only, never a config key. Other options override the config file: `--port`, `--host`, `--log`, `--device` (`none`: browsers only), `--assets`, `--sources`, `--script`, `--web-root`, `--config`, `--list-devices`. The config is `~/.config/blast/server.toml` (or under `$XDG_CONFIG_HOME`); the first run writes it with every key at its default, so the file shows everything that can be set. State (the pid file, the log, per-user preferences) goes to `~/.local/state/blast/server/`. See [`blast_server/README.md`](blast_server/README.md) for Tailscale, the systemd unit, and how the server is built.

## Documents consulted

**Audio specs**:  
- [DataVoyage's informal MPEG overview](http://mpgedit.org/mpgedit/mpeg_format/mpeghdr.htm#MPEG%20HEADER)  
- [Original AIFF-1.3 specification](https://mmsp.ece.mcgill.ca/Documents/AudioFormats/AIFF/Docs/AIFF-1.3.pdf)  
- [McGill on WAVE](https://www.mmsp.ece.mcgill.ca/Documents/AudioFormats/WAVE/WAVE.html)
