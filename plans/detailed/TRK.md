# TRK: runtime tracks (import, rescan, drop)
Status: draft   ·   Changelog at the bottom

Wave 3. Planned against `integrate/wave-1` @ `2f97859` plus the plans for
C1 (`detailed/C1.md`), F1 (`F1a.md`, `F1b.md`) and LOAD (`LOAD.md`). E1's
and R1's plans did not exist when this was written, so every E1/R1 name
used below is an assumption listed in REQ-039; the wave-3 refresh adopts
E1's real names (editorial) and anything that doesn't map 1:1 becomes a
`plan-change` request.

## Summary
Three new commands change the set of tracks while blast runs:
`import <path> [as <name>] [--fmt <enc>] [--rate <hz>] [--ch <n>]` decodes a
file, conforms it to the engine's rate and channel count, and adds it as a
track; `rescan` imports files that appeared in the assets directory since
startup; `drop [-f] <track>` removes a track (refused while voices use it,
unless `-f` unloads them). Samples are built on the calling thread as
`Arc<[f32]>`, inserted into the engine's track arena by ID, and freed
through the garbage queue.

## Ownership
- **Modifies:** `commands/track.rs` (`TrackRepr` additions, `try_import`,
  `try_rescan`, `try_drop`, argument types, `HELP`, tests),
  `engine/handlers/track.rs` (`add_track`, `drop_track`, tests).
- **Reads only:** `commands/{mod,reprs,errors,voice,group}.rs`,
  `engine/{mod,arena,events}.rs`, `file_parsing/**` (F1/LOAD API),
  `test_util.rs`.

## Registry entries
Added by the architect in the wave-3 scaffold as compiling stubs:
- `Command` variants (in `commands!`):
  `AddTrack(AddTrackArgs)` with
  `pub struct AddTrackArgs { pub id: TrackId, pub samples: Arc<[f32]>, pub channels: u32 }`
  (defined in `commands/track.rs`), and `DropTrack(TrackId)`.
- Parse arms in `commands/mod.rs`: `"import" => self.try_import(args)`,
  `"rescan" => self.try_rescan(args)`, `"drop" => self.try_drop(args)`.
  Stubs return `Err(CmdErr::NoCmd { .. })`.
- Apply arms in the `Conductor::apply` match:
  `Command::AddTrack(a) => self.add_track(a)`,
  `Command::DropTrack(id) => self.drop_track(id)`. Stubs in
  `engine/handlers/track.rs` push their payload to the garbage queue.
- `CmdErr::Import { path: String, reason: String }` in `commands/errors.rs`,
  with its `Display` arm (`could not import '{path}': {reason}`).
- `Garbage::Track(Arc<[f32]>)` in E1's garbage enum, if E1 doesn't already
  have a variant that carries track samples (REQ-039 item 6).
- `HELP` rows: none in the registry (`commands/track.rs`'s own `HELP` is
  TRK's file).

## Architecture changes
None if REQ-040 option 1 (decode on the calling thread) is approved. Option
2 (a loader thread) would be one, and is described there.

## Interfaces
**Provides:**
```rust
// commands/track.rs
pub struct TrackRepr {                    // E1's fields, plus:
    pub(crate) id: TrackId,
    pub(crate) name: String,
    pub(crate) channels: u32,
    pub(crate) frames: usize,
    pub(crate) source: Option<PathBuf>,   // None for tracks loaded at startup by the wrapper
}
pub struct AddTrackArgs { pub id: TrackId, pub samples: Arc<[f32]>, pub channels: u32 }
pub const HELP: &[(&str, &str)];          // import, rescan, drop rows
impl CmdProcessor {                        // or EngineState, wherever E1 puts try_*
    fn try_import(&mut self, args: &str) -> CmdResult<Command>;
    fn try_rescan(&mut self, args: &str) -> CmdResult<Command>;
    fn try_drop(&mut self, args: &str) -> CmdResult<Command>;
}
// engine/handlers/track.rs
impl Conductor {
    pub(crate) fn add_track(&mut self, args: AddTrackArgs);
    pub(crate) fn drop_track(&mut self, id: TrackId);
}
```

**Consumes:**
- F1: `decode_file(&Path)`, `track_name(&Path)`, `is_supported_extension`,
  `raw::{RawParams, SampleEncoding, parse_raw_name, decode_with}`,
  `DecodeError: Display`.
- LOAD: `scan_assets(&Path)`, `load_assets(&[AssetInfo], rate, channels)`,
  `conform(AudioFile, rate, channels)`.
- E1/R1 (assumed, REQ-039): `TrackId`; `EngineState::{alloc_track,
  free_track}` and a name index for tracks; `VoiceRepr.track: TrackId` and a
  way to list every voice (top-level and group members) using a given track;
  the mirror-side unload of one voice as a `Command` usable in a `Batch`;
  `Command::Batch`; `Conductor.tracks: Arena<Track>` with
  `Track { samples: Arc<[f32]>, channels: u32 }`; Voices clone the track's
  `Arc` at load; a garbage push helper; the engine's output rate and
  channel count readable from the mirror; the assets directory readable from
  the mirror (set by R1).

**Contract issues:**
- README §3.5's `submit` returns `Option<String>` only for `Parsed::Local`.
  `import` and `rescan` would like to report what they did (`imported 'kick'
  (stereo, 2.1 s, resampled from 44100 Hz)`, `rescan: 3 new tracks`).
  Asked in REQ-039 item 10 (a note alongside an engine command). Without it,
  they are silent on success like every other engine command.

## Steps
1. **Verify the startup ID coupling is gone** (brief). Read E1's startup
   path: the mirror's `TrackRepr`s and the engine's track arena must both be
   built from one ordered list (`Vec<AudioFile>`, IDs allocated in that
   order), with no `HashMap` iteration deciding an index on either side.
   If E1 still derives track indices from two separate maps, stop and file a
   request; don't patch it here. Write `startup_track_ids_agree` (Tests)
   first: it locks this in for TRK's own inserts too.
2. **Shared insert path** (private, `track.rs`):
   `fn add_track_command(&mut self, file: AudioFile, source: Option<PathBuf>) -> CmdResult<Command>`:
   name must be free in the track name index (else `CmdErr::AlreadyIs`);
   `alloc_track()` (arena full → E1's "limit reached" error, before
   anything else changes); insert `TrackRepr`; build
   `Arc::<[f32]>::from(file.samples)` (allocation on the calling thread,
   never on the audio thread); return `Command::AddTrack`. The order (check
   name, check capacity, then mutate) keeps a failed command from touching
   the mirror; `submit` already refuses before parsing when the queue is
   full.
3. **`try_import(args)`.** Parse words: the path (required, first); then
   optionally `as <name>`; then flags in any order, each at most once:
   `--fmt <enc>` (`SampleEncoding::from_str`), `--rate <hz>` (1..=768000),
   `--ch <n>` (1..=64). Errors: missing path or value, unknown flag,
   repeated flag, bad value (`CmdErr::MissingArg`/`InvalidArg`, whichever
   E1/C1 provide).
   - Name: `as <name>` if given, else `track_name(path)` (stem, raw suffix
     stripped). A given name must be non-empty and contain no `.` (it is the
     `group.voice` separator; REQ-037 sub-question). A default name that
     clashes suggests `as <name>` in the error.
   - Decode (REQ-040 option 1: right here, on the calling thread):
     - any raw flag present → raw: the extension must be `raw`, `pcm` or
       absent/unknown (a `.wav` with `--fmt` is an error: "flags apply to
       raw PCM only"); `fs::read`, then
       `RawParams { flags }.or(parse_raw_name(file_name).1).complete()`,
       then `decode_with(name, &bytes, spec)`.
     - no flags → `decode_file(path)`, which handles `.raw` names with a
       full spec, AU, and everything symphonia reads.
     - `DecodeError` → `CmdErr::Import { path, reason: error.to_string() }`.
   - `conform(file, engine_rate, engine_channels)` (mono stays mono).
   - `add_track_command(file, Some(path))`.
4. **`try_rescan(args)`.** No arguments (anything else → error). No assets
   dir known → error. `scan_assets(dir)` (an unreadable dir →
   `CmdErr::Import`), keep the assets whose name is not a current track,
   `load_assets(&new, engine_rate, engine_channels)` (its per-file skip
   messages print as at startup). None new → `Parsed::Local("rescan: no new
   files")` (or the error path if `Local` can't come from here; see
   REQ-039 item 10). Otherwise insert each via `add_track_command` and
   return one `Command::Batch` of the `AddTrack`s, so the whole rescan
   lands in one period and uses one queue slot. If the arena fills part
   way, the rescan fails as a whole before any mirror change (check
   `free track slots >= new.len()` first). Files whose names already exist
   are not reloaded even if they changed on disk (documented in `HELP`:
   `drop` then `rescan` reloads one).
5. **`try_drop(args)`.** `drop <track>` or `drop -f|--force <track>`.
   Unknown track → `CmdErr` "no track". Voices using it (via REQ-039
   item 3): none → mirror removes the `TrackRepr` and frees the ID, returns
   `Command::DropTrack(id)`. Some, no `-f` → error listing them
   (`track 'kick' is used by voices kick, drums.kick; drop -f unloads
   them`), mirror untouched. Some with `-f` → for each voice, the mirror's
   own unload (REQ-039 item 4) producing its `Command`, then remove/free the
   track, and return `Command::Batch([unloads…, DropTrack(id)])`
   (REQ-041). Freeing the `TrackId` right away is safe: commands are applied
   in queue order, so a later `AddTrack` with the reused ID arrives after
   the `DropTrack`.
6. **Engine handlers** (`engine/handlers/track.rs`), no allocation, lock,
   print or I/O:
   - `add_track(a)`: `self.tracks.insert(a.id, Track { samples: a.samples,
     channels: a.channels })`. If the slot was occupied (a mirror bug),
     the old `Track`'s samples go to the garbage queue and an
     `Event`/`debug_assert` reports it (whichever E1 uses for invariant
     breaks).
   - `drop_track(id)`: `if let Some(t) = self.tracks.remove(id)` → push
     `t.samples` to the garbage queue. Voices that were playing the track
     hold their own `Arc` clone, so nothing on the audio thread ever frees
     sample memory; the last `Arc` drops on the REPL side when the garbage
     queue is drained.
   - Garbage queue full: use E1's policy (its push helper), don't invent one.
7. **`HELP`** rows in `commands/track.rs`:
   - `import <path> [as <name>] [--fmt <enc> --rate <hz> --ch <n>]: decode a file and add it as a track (flags: raw PCM only)`
   - `rescan: import new files from the assets directory`
   - `drop [-f] <track>: remove a track (-f also unloads voices using it)`
8. Tests (below), `cargo build` (zero warnings), `cargo test`,
   `cargo clippy` clean in the two owned files.

## Tests
Through `test_util`'s mirror + engine helpers (`setup`, `run`, `render`, the
garbage-drain helper; names per E1). Fixture files are synthetic WAV/raw
bytes from F1's `test_bytes.rs` builders written to a temp dir; no real
assets needed.

`commands/track.rs`:
- `startup_track_ids_agree`: 32 constant-valued tracks (value = index /
  64) set up at startup; `load` each by name, start, render one frame; each
  voice outputs its own track's value. Then `import` 4 more and repeat.
- `import_wav_then_load_plays_it`: `import <tmp>/hit.wav`, `load hit`,
  `start -v hit`; rendered frames equal the file's samples.
- `import_as_name`: `import <tmp>/hit.wav as snare2` → track `snare2`.
- `import_name_rules`: default name clash → error mentioning `as`;
  `as a.b` → error; `as` without a name → missing-arg error.
- `import_errors_leave_mirror_untouched`: missing file, unsupported
  extension, garbage bytes named `.wav`, each followed by
  `assert_untouched` (C1's helper) and an `Import` error carrying the path.
- `import_resamples_to_engine_rate`: a 24 kHz mono file into a 48 kHz
  engine → `TrackRepr.frames` doubles, `channels == 1`.
- `import_downmixes_wide_files`: a 6-channel file into a 2-channel engine →
  `channels == 2`; a mono file stays 1.
- `import_raw_with_flags`: `import <tmp>/x.raw --fmt s16be --rate 48000
  --ch 1` decodes; a name spec `x.s16le.48000.2.raw` with `--ch 1` uses
  mono (flags win); `.raw` without spec or flags → error naming the missing
  fields; `--fmt` on a `.wav` → "raw PCM only"; unknown flag, repeated
  flag, bad rate → errors.
- `import_full_arena_is_rejected`: `Limits::tracks` small; the import past
  the limit errors with the mirror untouched.
- `import_queue_full_leaves_mirror`: the existing full-queue invariant
  test, with `import`.
- `rescan_imports_only_new`: assets dir with `a.wav`; startup has `a`; add
  `drums/b.wav`; `rescan` adds `drums/b` (REQ-037 name) as one `Batch`;
  a second `rescan` reports nothing new; a changed `a.wav` is not reloaded.
- `rescan_without_assets_dir_errors`.
- `rescan_capacity_is_all_or_nothing`: free slots < new files → error,
  mirror untouched.
- `drop_unused_track`: `drop kick` → track gone, `load kick` now fails.
- `drop_used_track_is_refused`: with voices `kick` and `g.kick` (grouped)
  → error naming both, mirror untouched.
- `drop_force_unloads_voices`: `drop -f kick` → both voices and the track
  gone in mirror and engine; render is silent.
- `drop_then_import_reuses_id`: drop `kick`, import a different file as
  `kick`, `load kick` plays the new samples.

`engine/handlers/track.rs`:
- `add_track_fills_slot`, `drop_track_sends_samples_to_garbage` (the
  drained `Arc` has strong count 1, so it's the last reference and frees
  on the test thread), `drop_with_playing_voice_keeps_voice_audio` (engine
  level: a voice holding the `Arc` keeps rendering after `DropTrack`;
  the mirror never allows this without `-f`, but the engine must stay
  safe), `add_into_occupied_slot_garbages_old`.

## Done when
- `import`, `rescan` and `drop` work end to end in the running program on
  the dev machine (with R1's runtime), including a long MP3 at a different
  rate; the import time for a 3-minute 44.1 kHz MP3 into a 48 kHz engine is
  in the report.
- Tracing `add_track`/`drop_track` shows no allocation, free, lock, print
  or I/O.
- `startup_track_ids_agree` passes and no `HashMap` iteration order decides
  any track ID.

## Risks and open questions
- REQ-039: the E1/R1 surface assumed above (blocks the wave-3 refresh).
- REQ-040: decoding on the calling thread vs. a loader thread (step 3).
  With option 1, a 3-minute MP3 that needs resampling blocks the thread
  that submitted it for about 2.7 s (0.25 s decode + ~2.4 s resample,
  measured). If REQ-008 option 1 (shared `Mutex<Submitter>`) is approved, a
  script's `import` holds that lock and blocks typing for the same time.
- REQ-041: `drop` semantics (step 5).
- Paths with spaces can't be imported: the shell rejects whitespace in a
  command word (`detailed/SH-grammar.md`). Rename the file, or put it in the
  assets dir and `rescan`. `~` is not expanded.
- Memory: an imported 10-minute stereo file at 48 kHz is ~230 MB as f32,
  plus a transient copy while resampling. No limit is enforced; a
  `max_track_seconds` setting could come later.
- `rescan` does not notice deleted or changed files; it only adds.

## Size
M: three parsers and two tiny handlers on top of the F1/LOAD API, ~22 tests.
Most risk is in matching E1's real names.

## Changelog
- 2026-10-24: first draft.
