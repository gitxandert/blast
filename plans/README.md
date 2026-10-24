# BLAST roadmap: architecture and contracts (architect-owned)

This file is the approved architecture: ground rules, waves, file ownership and
cross-task contracts. **Who does what, and who approves what, is in
[`ORG.md`](ORG.md).** To start or resume the project, see
[`KICKOFF.md`](KICKOFF.md).

Roles: the user; under the user, the architect and the PM as peers (the PM is
the active session and relays for the architect); under the PM, planners (one
per brief); under each planner, implementers (one per task). Planners don't write code. Implementers don't
change their plan; if they need to, they stop and report to their planner.

**Planner deployment.** There are 6 planners, one per brief, all spawned at once
by the PM in phase 1. While planning they only read code and write their own
files under `plans/`, so they can't conflict. Each brief bundles a set of tasks
with the downstream tasks that consume their interfaces, so dependent plans are
written by the same planner, who later oversees those tasks' implementers.

| Brief | Tasks |
|---|---|
| `PA-core` | C1, E1, R1 |
| `PB-audio-files` | F1, LOAD, TRK |
| `PF-shell-repl` | SH, REPL |
| `PH-engine-commands` | GRP, TMP, PRC, INS |
| `PI-voice-dsp` | VOX, LFO, ENV, EUC, FX |
| `PJ-recording` | REC |

(`PG-backends` is obsolete and not deployed. See §4 item 3.)

---

## 1. Ground rules (apply to every task)

### 1.1 Dependencies

We use vetted, standard crates instead of hand-writing heavy machinery, as long
as the build stays light and the runtime stays fast. The **approved set** is
below. The architect adds all of it to `blast/Cargo.toml` and `Cargo.lock` in
scaffold step 0, so no task ever edits `Cargo.toml`.

| Crate | Version | Features | Used for |
|---|---|---|---|
| `symphonia` | 0.6.1 | `default-features = false`, `["wav","aiff","pcm","adpcm","flac","mpa","aac","alac","isomp4"]` | every decoder (F1) |
| `rubato` | 5.0.1 | `default-features = false` | resampling (LOAD) |
| `cpal` | 0.18.2 | default (no features) | audio output (R1) |
| `crossterm` | 0.29.0 | `default-features = false`, `["events"]` | raw mode, key events (R1) |
| `rtrb` | 0.4.0 | default | every SPSC queue: commands, events, garbage, recording |
| `hound` | 3.5.1 | default | WAV writing (REC) |
| `lexopt` | 0.3.2 | default | CLI parsing (R1) |

Measured on the dev machine: the full set builds clean in about 11 s in
release, against 5.7 s today. Incremental builds don't change. These rules
apply to dependencies:
- **Any crate not on this list needs the user's approval.** A planner that
  wants one says so under "Risks and open questions" with: what it saves, its
  clean build time, its transitive crate count, and whether it contains
  `unsafe`. Measure it in a scratch crate.
- Always `default-features = false` unless the default set is needed.
  `Cargo.lock` stays committed. Only adopt versions published at least 2 weeks
  ago.
- `deny.toml` (added in scaffold step 0) enforces crates.io as the only source,
  RustSec advisories, a license allowlist (MIT, Apache-2.0, MPL-2.0, BSD,
  Unicode, ISC, Zlib), and warnings on duplicate versions.
- Proc-macro crates (which pull in `syn`) cost the most to build. Avoid them
  (e.g. no `thiserror`).
- `alsa-sys` and `libc` stay as direct dependencies until R1 removes them.
  cpal brings in its own ALSA binding.

### 1.2 Safety and speed

- **Real-time safety on the audio thread.** Code reachable from the cpal
  callback (today: `Conductor::apply` and `Conductor::render`) must not
  allocate, free, lock, block, print, or do I/O. Heap objects are built on the
  REPL thread and moved in through a queue. Anything removed goes back out
  through the garbage queue (§3.4).
- **No `unsafe` in blast.** After wave 3, the architect adds
  `#![forbid(unsafe_code)]` to the crate. R1 removes the last of it (ALSA
  FFI, termios, `CmdQueue`). From now on, no task adds new `unsafe`.
- Hostile input files must not panic the program. Decode errors become
  `DecodeError` values and the file is skipped.

### 1.3 Conventions (they replace the old style; rewriting existing code to match is welcome)

- `rustfmt` with default settings. Scaffold step 0 formats the whole repo once,
  so no task's diff contains formatting noise.
- `cargo clippy` must be clean in every file a task owns. (Files owned by
  nobody may still warn until they're rewritten.)
- `///` doc comments on `pub` items. `//` comments explain *why*, not *what*.
- Borrow instead of taking ownership: `&str` instead of `String` parameters,
  slices instead of `&Vec`.
- Prefer `let … else`, `?`, and iterator chains over nested `match` ladders.
- Error types are enums with a hand-written `Display` and
  `impl std::error::Error`. No `unwrap()`/`expect()` on anything a user or a
  file controls.
- Clear names. Rename unclear names when touching them (e.g. `Header::barf`,
  `vs_fs_ps`).
- Code inside a task's ownership may be rewritten rather than ported if the
  result is clearer and keeps the behaviour (or changes it as the plan says).
  Rewriting is not the same as changing architecture; see §2's "Architecture
  rule".
- Commit messages: `area: summary` (e.g. `engine: store voices in a slot arena`).

### 1.4 Tests and definition of done

- Unit tests build synthetic input in memory. Tests against real files are
  allowed only if they **skip when the fixture is missing** (the
  `loads_real_assets` pattern). Fixtures may be generated with `ffmpeg`
  (installed on the dev machine) into the gitignored `blast/assets/`. Tests
  never shell out.
- Done means: `cargo build` has zero warnings, `cargo test` passes,
  `cargo clippy` is clean in owned files, there are no
  `todo!()`/`unimplemented!()`, and the README module list is updated if
  modules changed.

## 2. Waves and file ownership

**Paths.** All paths are relative to `blast/src/`. Bare paths (`commands/…`,
`engine/…`, `runtime/…`, `processes/…`, `blast_time.rs`, …) live under
`blast/src/audio_processing/`. `file_parsing/…`, `shell/…`, `export/…`,
`cli.rs`, `main.rs` and `lib.rs` are directly under `blast/src/`.

**Branching.** Each wave branches `integrate/wave-N` from `dev`. Each task
branches `task/<id>` from it, with one worktree per task. The architect merges
task branches in the listed order, then merges the wave into `dev`.

**Scaffold steps.** Before each wave, the architect adds that wave's *registry
entries*: enum variants, dispatch arms, module declarations (in `lib.rs` and
`audio_processing/mod.rs`), CLI flags, and help-table rows. They go in as
compiling stubs, listed exactly in each detailed plan. That way no two parallel
tasks edit the same dispatch table.

**Scaffold step 0** (before wave 1) also does:
- Adds the approved crates and `deny.toml`.
- Runs `cargo fmt` across the repo.
- Changes `AudioFile.samples` to `Vec<f32>` mechanically. The decoders divide
  by full scale; `Voice::new` converts back to i16 for now. F1 and E1 both
  start from this.

**Ownership rule.** A task may edit only the files it owns in its wave, plus
new files under paths it owns. Everything else is read-only. If a plan needs to
touch a file it doesn't own, it asks for a registry entry or an interface
instead.

**Architecture rule.** §3 is approved architecture. Any plan that would change
it, or introduce something of the same weight (a new thread, a new queue, a
change to the engine's ownership model, a new top-level module, replacing a
subsystem), puts it under "Architecture changes" in the plan for the user to
approve. Implementers never make such changes on their own.

### Wave 1: file decoding and the command split (2 tasks)

| Task | Owns |
|---|---|
| **F1** decode-adapter | `file_parsing/**` except `load_assets` (it keeps working unchanged) |
| **C1** commands-split | `commands.rs` becomes `commands/**` |

### Wave 2: engine rework, loading, shell core (3 tasks)

| Task | Owns |
|---|---|
| **E1** engine-rework | `engine.rs` becomes `engine/**`; `processes.rs` becomes `processes/**`; `blast_time.rs`; `test_util.rs`; `commands/**` (the mirror's ID bookkeeping runs through all of it) |
| **LOAD** load-pipeline | `file_parsing/mod.rs` (`load_assets` and the new `scan_assets`), `file_parsing/resample.rs`, `file_parsing/mix.rs` |
| **SH** shell-core | new `shell/**` (standalone, with no REPL integration yet) |

### Wave 3: runtime, engine features, runtime tracks (7 tasks)

| Task | Owns |
|---|---|
| **R1** runtime | `runtime.rs` becomes `runtime/**`; `line_editor.rs` moves to `runtime/line_editor.rs`; `main.rs`; new `cli.rs`; `commands/queue.rs`; `engine/alsa_shim.rs` (deleted); `audio_processing/mod.rs` |
| **GRP** group-management | `commands/group.rs`, `engine/group.rs`, `engine/handlers/group.rs` |
| **TMP** tempo-management | `commands/tempo.rs`, `engine/handlers/tempo.rs`, `blast_time.rs` |
| **PRC** process-management | `commands/process.rs`, `commands/procs/seq.rs`, `processes/seq.rs`, `engine/handlers/process.rs`, `blast_rand.rs` |
| **VOX** voice-playback | `commands/voice.rs`, `engine/voice.rs`, `engine/handlers/voice.rs` |
| **INS** inspection-help | `commands/inspect.rs` |
| **TRK** runtime-tracks | `commands/track.rs`, `engine/handlers/track.rs` |

### Wave 4: recording, REPL, modulation, effects (6 tasks)

| Task | Owns |
|---|---|
| **REC** record-export | `engine/render.rs`, new `export/**`, `commands/record.rs`, `engine/handlers/record.rs` |
| **REPL** repl | `runtime/repl.rs`, `runtime/line_editor.rs`, new `runtime/history.rs` |
| **LFO** lfo-process | `processes/lfo.rs`, `commands/procs/lfo.rs` |
| **ENV** envelope-process | `processes/env.rs`, `commands/procs/env.rs` |
| **EUC** pattern-generators | `commands/procs/seq.rs` (step generation only) |
| **FX** insert-effects | new `engine/fx/**`, `commands/fx.rs`, `engine/handlers/fx.rs`, `engine/voice.rs` (only the FX hook) |

After wave 3, the architect adds `#![forbid(unsafe_code)]` and removes
`alsa-sys`/`libc` from `Cargo.toml`, if R1 hasn't already.

## 3. Approved architecture and cross-task contracts

Planners build against these. If a contract turns out to be unworkable, the
plan says so under "Contract issues", with a proposal. Planners don't quietly
diverge from a contract.

### 3.1 Samples (scaffold step 0, F1)

```rust
pub struct AudioFile {
    pub file_name: String,
    pub format: String,
    pub sample_rate: u32,
    pub num_channels: u32,
    pub bits_per_sample: u32,          // of the source, informational
    pub channel_layout: Option<u32>,   // WAVE-style speaker mask when known
    pub samples: Vec<f32>,             // interleaved, full scale -1.0..1.0
}
```

### 3.2 Decoding (F1)

`file_parsing/` becomes a thin adapter:
- `symphonia.rs`: probes and decodes everything symphonia supports (WAV,
  AIFF/AIFC, FLAC, MP1/2/3, AAC-LC in MP4/M4A or ADTS, ALAC, MS and IMA
  ADPCM, A-law, µ-law) into `AudioFile`. Gapless trimming is on (encoder delay
  and padding). Symphonia's errors map onto `DecodeError`.
- `raw.rs`: headerless PCM. Parameters come from the filename
  (`name.s16le.48000.2.raw`), and `decode_with(name, bytes, RawSpec)` lets
  flags override them.
- `au.rs`: Sun/NeXT `.au`/`.snd`. Hand-written, since symphonia lacks it.
- `mod.rs`: `pub fn decode_file(path: &Path) -> DecodeResult<AudioFile>`
  dispatches to `raw`/`au` by magic number or extension, and to symphonia
  otherwise. Tracks are named by file stem.
- The old `wav.rs`, `aiff.rs`, `mpeg.rs` and `pcm_to_*` helpers are deleted.
  Before deleting them, port their edge-case tests into adapter tests that
  feed the same synthetic bytes through symphonia.
- Formats intentionally unsupported (decided): 8SVX, RF64, 16-bit float WAV,
  AIFC `ima4`, Ogg Vorbis, Opus, CAF, W64, and HE-AAC (SBR/PS).

### 3.3 Loading and resampling (LOAD)

```rust
pub fn scan_assets(dir: &Path) -> DecodeResult<Vec<AssetInfo>>;   // rate/channels via probe, no full decode
pub fn load_assets(assets: &[AssetInfo], rate: u32, channels: u32) -> DecodeResult<Vec<AudioFile>>;
pub fn resample(samples: &[f32], channels: usize, from: u32, to: u32) -> Vec<f32>;   // rubato inside
pub fn remap(samples: &[f32], from_ch: usize, layout: Option<u32>, to_ch: usize) -> Vec<f32>;
```
The startup sequence (wired by R1) is: scan, pick a preferred rate and channel
count, open the device asking for them, then load at whatever the device
actually accepted. Files at other rates are resampled, not dropped. TRK reuses
`resample` and `remap` for runtime imports.

### 3.4 Engine: arenas, stable IDs, queues (E1). Approved by the user.

- **The mirror (`EngineState`) is the ID authority.** It allocates `VoiceId`,
  `GroupId`, `TempoId` and `ProcId` (newtype `u32`s) from a free list, and
  every `Command` carries IDs. IDs never shift, so all index-shift patching
  (e.g. in `try_unload`) disappears.
- **The engine owns storage.** `Conductor` holds fixed-capacity slot arenas
  (`Vec<Option<T>>` preallocated to configured limits), indexed by ID. Inserting
  and removing are O(1) and never allocate. When an arena is full, the mirror
  rejects the command before anything is queued.
- **No `Rc`/`RefCell`.** Objects refer to each other by ID (a Voice holds a
  `TempoId`, a Group holds a preallocated list of `VoiceId`s). `Conductor` is
  `Send`. Recommended, for E1 to confirm: every active tempo is ticked once per
  frame by the Conductor itself. That replaces today's "who ticks which tempo"
  rules, which are spread across `Voice::tick`/`Group::tick`.
- Track samples are `Arc<[f32]>` (built on the REPL thread), shared by every
  Voice playing the track. Voices hold f32.
- Three `rtrb` queues connect the threads: commands (REPL to engine), events
  (engine to REPL: small `Copy` enums replacing every `println!` on the audio
  thread), and garbage (engine to REPL: removed objects, dropped off the audio
  thread). `Command::Batch(Vec<Command>)` applies atomically within one period.
- `pub fn render(&mut self, out: &mut [f32], channels: usize)`: interleaved
  output, unclamped (the backend clamps). E1 keeps the ALSA loop working through
  `engine/alsa_shim.rs`, a temporary adapter (no allocation) from ALSA areas to
  `render`, which R1 deletes.

### 3.5 Command layer (C1, then E1)

Layout:
```
commands/mod.rs      Command (commands! macro), CmdProcessor, parse dispatch, submit
commands/queue.rs    command queue (R1 swaps it to rtrb in wave 3)
commands/reprs.rs    EngineState, ID types and allocator (E1)
commands/errors.rs   CmdErr, StateErr, Display
commands/{voice,group,tempo,track,process}.rs   *Repr + try_* + *Args + HELP
commands/procs/seq.rs  try_seq + SeqArgs
```
Each family file has a `pub const HELP: &[(&str, &str)]` table (`command`,
`usage + summary`). REPL-facing API:
```rust
pub enum Parsed { Engine(Command), Local(String) }   // Local: text to print, no engine involvement
impl CmdProcessor {
    pub fn parse(&mut self, line: &str) -> CmdResult<Parsed>;
    pub fn submit(&mut self, line: &str, queue: &mut CmdSender) -> Result<Option<String>, CmdErr>;
}
```
A failed parse or a full queue leaves the mirror untouched (that invariant is
already tested).

### 3.6 Runtime (R1)

- cpal: default host. `--device <name>` picks an output device. The stream
  config prefers the assets' rate and channel count and f32 samples, then falls
  back to whatever the device supports (I16/I32/F32 with conversion in the
  callback). Use `BufferSize::Fixed` when it's supported. The callback drains
  the command queue, calls `Conductor::render`, clamps, and converts.
- crossterm: raw mode and key events, mapped onto the existing `LineEditor`.
  `parse_key` and termios go away. Keep the animated prompt marker and the
  redraw thread. The split-screen view from `notes.txt` must stay possible.
- lexopt: `cli.rs` with `Options { assets_dir, device, script, … }`. Keep the
  positional assets-dir argument.
- Quit: replace `libc::raise(SIGTERM)` with a quit flag or event.

### 3.7 Shell (SH, integrated by REPL)

```rust
pub struct Shell { /* variables, functions, aliases */ }
impl Shell {
    /// Feed one input line. Returns finished output, or nothing while a block is still open.
    pub fn feed(&mut self, line: &str) -> Result<Vec<ShellOut>, ShellErr>;
}
pub enum ShellOut { Line(String), Batch(Vec<String>), Wait(Wait), Print(String) }
```
The command layer never sees shell syntax. Scripts (`--script`, `source`) run
on their own thread, so `wait` never blocks typing.

### 3.8 Completion and introspection (INS, used by REPL)

```rust
impl CmdProcessor {
    pub fn completions(&self, line: &str, cursor: usize) -> Vec<String>;
    pub fn names(&self, kind: NameKind) -> Vec<String>;
}
```

### 3.9 Voice parameters (VOX, used by LFO/ENV/FX/GRP)

`pub enum VoiceParam { Gain, Pan, Rate, Start, End, … }` with
`VoiceState::set_param` / `get_param`. Modulating processes and group-wide
commands go through this API only.

## 4. Decisions (all made by the user, 2026-10-24)

1. **f32 internal samples: yes.**
2. **Shell syntax:** start from `blast/notes.txt` (`name { … | … }`, `&`,
   `-n/--name`, `all`) plus `let`, `for`, `if`, `fn` and `source`. The SH
   planner proposes a concrete grammar for the user to approve before SH is
   implemented.
3. **macOS / Windows: deferred.** cpal and crossterm are cross-platform, so
   nothing new should be Linux-only without a reason. But nothing is planned or
   tested for those platforms. `briefs/PG-backends.md` is obsolete under cpal
   and kept only as a note.
4. **AAC: LC only.** Symphonia's AAC decoder is LC-only anyway.
5. **Raw PCM:** parameters from the filename (`name.s16le.48000.2.raw`), with
   `import` flags (`--fmt/--rate/--ch`) taking precedence.
6. **Recording:** 32-bit float WAV, tapped before the output clamp, with an
   optional 16- or 24-bit dithered mode.
7. **Subfolder track naming:** the LOAD planner proposes a scheme for the user
   to approve.
8. **Dependencies:** the approved set in §1.1. Anything else needs approval.
9. **Decoders:** symphonia, plus hand-written raw and AU. The niche formats in
   §3.2 are dropped.
10. **Audio output:** cpal, replacing the hand-written ALSA code.
11. **Terminal:** crossterm, keeping our own line editor.
12. **Engine:** slot arenas with stable IDs everywhere (§3.4).
