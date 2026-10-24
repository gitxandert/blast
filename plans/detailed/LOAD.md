# LOAD: load pipeline (scan, resample, remap, recursive assets)
Status: draft   ·   Changelog at the bottom

Wave 2. Planned against `integrate/wave-1` @ `2f97859` plus F1a/F1b as
planned (`detailed/F1a.md`, `detailed/F1b.md`). F1 rewrites the top of
`file_parsing/mod.rs`, so the line numbers below are for today's
`load_assets` (`mod.rs:10-104` on `integrate/wave-1`); the implementer
re-reads them on `integrate/wave-2`.

## Summary
Startup loading becomes three steps (README §3.3): `scan_assets` walks the
assets directory recursively and probes every supported file for rate and
channels without decoding it; the caller picks a rate and channel count; and
`load_assets` decodes, resamples (rubato, high-quality sinc) and downmixes
each file to that configuration. Files at other rates are resampled instead of
dropped. Files in subfolders become tracks named by their relative path
(`drums/kick`, REQ-037). Until R1 rewires startup in wave 3, the old
directory-based entry point survives as a thin wrapper.

## Ownership
- **Creates:** `file_parsing/resample.rs`, `file_parsing/mix.rs`.
- **Modifies:** `file_parsing/mod.rs`: `load_assets` (rewritten), the new
  `scan_assets`, `AssetInfo`, `preferred_config`, the wrapper
  `load_assets_dir` (REQ-038), private helpers placed next to them, the
  `mod resample; mod mix;` declarations and their `pub use`, and the
  `load_assets` tests in `mod.rs`'s test module.
- **Reads only:** the rest of `file_parsing/**` (F1's `decode_file`,
  `probe_file`, `is_supported_extension`, `track_name`, `StreamInfo`,
  `test_bytes.rs` builders), `main.rs`, `audio_processing/runtime.rs`.

## Registry entries
- `main.rs`: the one call `load_assets(&dir)` becomes `load_assets_dir(&dir)`
  (REQ-038 option 1). The architect makes this edit in the wave-2 scaffold
  together with a stub `pub fn load_assets_dir(dir: &str) -> … { load_assets(dir) }`
  in `file_parsing/mod.rs`, so the crate compiles before LOAD lands. LOAD
  then replaces the stub and gives `load_assets` its new signature.
- No other entries. `mod resample; mod mix;` live inside LOAD's own region
  of `file_parsing/mod.rs`.

## Architecture changes
None in README §3. Notes for the architect:
- Parallel loading uses `std::thread::scope` inside `load_assets`: worker
  threads that are joined before it returns. No long-lived thread, queue or
  shared state is added. Listed here in case the architect counts it under
  the "new thread" rule.
- Track names for subfolders: REQ-037 (user decision, README §4 item 7).

## Interfaces
**Provides** (`file_parsing::`):
```rust
/// One playable file found by `scan_assets`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetInfo {
    pub path: PathBuf,       // absolute or as given: dir.join(relative)
    pub name: String,        // track name, REQ-037 scheme ("drums/kick")
    pub info: StreamInfo,    // from F1's probe_file: rate, channels, layout
}

/// Walk `dir` recursively (REQ-037), probe each supported file, skip (with a
/// message) anything that fails or clashes with an earlier name.
/// Sorted by name. Errors only if `dir` itself can't be read.
pub fn scan_assets(dir: &Path) -> DecodeResult<Vec<AssetInfo>>;

/// The configuration to ask the device for: the most common rate (ties go to
/// the higher rate) and the largest channel count. `None` if `assets` is empty.
pub fn preferred_config(assets: &[AssetInfo]) -> Option<(u32, u32)>;

/// Decode every asset, resample to `rate`, downmix files with more than
/// `channels` channels. Mono and other narrower files keep their channel
/// count (VOX pans/maps them). Output order = input order. Files that fail
/// are skipped with a message; an empty result is `InvalidData`.
pub fn load_assets(assets: &[AssetInfo], rate: u32, channels: u32) -> DecodeResult<Vec<AudioFile>>;

/// Bring one decoded file to `rate`/`channels` the way `load_assets` does:
/// downmix if it has more than `channels` channels, then resample. Used by
/// `load_assets` and by TRK's `import`.
pub fn conform(file: AudioFile, rate: u32, channels: u32) -> AudioFile;

/// Wave-2 wrapper for today's `main.rs` (REQ-038): scan, preferred_config,
/// load, returned in the old shape. R1 deletes it in wave 3.
pub fn load_assets_dir(dir: &str) -> DecodeResult<(HashMap<String, AudioFile>, u32, u32)>;

/// Offline sinc resampling (rubato). Same frame count as `from→to` implies
/// (rounded up), startup delay trimmed. Returns a copy if `from == to`, and
/// the input unchanged if any argument is 0 (callers validate first).
pub fn resample(samples: &[f32], channels: usize, from: u32, to: u32) -> Vec<f32>;

/// Channel conversion. `from_ch > to_ch` downmixes (ITU-R BS.775 for
/// `to_ch` 1 or 2, using `layout` or the default WAVE layout for the count).
/// Mono to N duplicates. Other widenings copy and zero-fill.
pub fn remap(samples: &[f32], from_ch: usize, layout: Option<u32>, to_ch: usize) -> Vec<f32>;
```
`resample`, `remap` and `conform` are re-exported from `file_parsing::` so
TRK can use them (README §3.3; `conform` is an addition so TRK's `import`
and startup loading treat a file identically).

**Consumes** (F1): `decode_file(&Path) -> DecodeResult<AudioFile>`,
`probe_file(&Path) -> DecodeResult<StreamInfo>`,
`is_supported_extension(&str) -> bool`, `track_name(&Path) -> DecodeResult<String>`.
rubato 5.0.1 (read from the registry source):
`rubato::{Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType, WindowFunction}`,
`Async::<f32>::new_sinc(ratio: f64, max_ratio_relative: f64, &params, chunk_size: usize, channels: usize, FixedAsync::Input) -> Result<Self, ResamplerConstructionError>`,
`Resampler::process_all(&mut self, &dyn Adapter<f32>, input_len: usize, None) -> ResampleResult<InterleavedOwned<f32>>`
(trims the startup delay itself, returns exactly `ceil(ratio * input_len)`
frames), `InterleavedOwned::take_data() -> Vec<f32>`, and
`rubato::audioadapter_buffers::direct::InterleavedSlice::new(&[f32], channels, frames)`.
rubato 5 takes **interleaved** data directly through `InterleavedSlice`, so
no planar conversion is needed. With `default-features = false` there is no
`Fft` resampler, only `Async` (sinc or polynomial).

**Contract issues:**
- README §3.3 shows `load_assets(assets, rate, channels)` while today's
  `main.rs` calls `load_assets(dir)`, and LOAD doesn't own `main.rs`. Same
  name, two signatures: REQ-038.
- README §3.3 doesn't say who picks the rate and channel count. LOAD
  provides `preferred_config` (today's vote, unchanged). R1 opens the device
  with it and caps channels at what the device offers. If REQ-030 (PA,
  R1's config chooser) moves this into R1, `preferred_config` is deleted
  (editorial).

## Steps
1. **`resample.rs`.** `resample(samples, channels, from, to)`:
   - `from == to` → `samples.to_vec()`; any of `channels`, `from`, `to` = 0,
     or fewer samples than one frame → `samples.to_vec()`. Frames =
     `samples.len() / channels` (a trailing partial frame is ignored).
   - Parameters (offline, so quality over speed):
     `SincInterpolationParameters { sinc_len: 256, f_cutoff: None (auto,
     rubato's calculate_cutoff), oversampling_factor: 128, interpolation:
     SincInterpolationType::Cubic, window: WindowFunction::BlackmanHarris2 }`,
     `Async::<f32>::new_sinc(to as f64 / from as f64, 1.0, &params, 1024,
     channels, FixedAsync::Input)`.
   - `process_all(&InterleavedSlice::new(samples, channels, frames)?, frames, None)`
     then `take_data()`. Construction and processing errors can't happen
     with validated inputs and rubato's own buffers; map them to "return the
     input unchanged" plus a `debug_assert!(false, …)`, never a panic in
     release.
   - Expected quality (measured in a scratch crate, 44.1→48 kHz stereo):
     passband flat to ~0.95 of the lower Nyquist (auto cutoff), aliasing
     below the BlackmanHarris2 sidelobes (well under -100 dB), RMS error
     against an ideal sine about 3e-4 (-70 dB) after aligning a residual
     sub-sample delay of 0.2-1.0 output frames. Speed: about 2.4 s per
     3 minutes of stereo in release with `sinc_len` 256 (1.5 s with 128).
     The residual delay is below one frame, so loop points stay frame-exact.
2. **`mix.rs`.** `remap(samples, from_ch, layout, to_ch)`:
   - `from_ch == to_ch` or either is 0 → copy.
   - `from_ch == 1` → duplicate into every output channel.
   - `from_ch < to_ch` → copy channel `c` to `c`, zero the rest.
   - `from_ch > to_ch`, `to_ch <= 2`: a `from_ch × 2` matrix from the
     layout. Mask: `layout` if its popcount equals `from_ch`, else the WAVE
     default for the count (1: 0x4, 2: 0x3, 3: 0x7, 4: 0x33, 5: 0x37,
     6: 0x3F, 7: 0x13F, 8: 0x63F; >8: first 8 by that default, extra
     channels ignored). Channel `i` is the `i`-th set bit, lowest first.
     BS.775 gains per position: FL, FLC, TFL → (1, 0); FR, FRC, TFR → (0, 1);
     FC, BC, TC, TFC, TBC → (0.7071, 0.7071); BL, SL, TBL → (0.7071, 0);
     BR, SR, TBR → (0, 0.7071); LFE → (0, 0). Then normalise each output
     row so its gains sum to 1 (no clipping on correlated content; a 5.1 file
     with identical channels downmixes to the same signal). `to_ch == 1`:
     the stereo result averaged. `2 → 1` is `(L + R) / 2`.
   - `from_ch > to_ch >= 3`: keep the first `to_ch` channels (documented;
     no standard matrix, and such devices are rare).
   - Matrix built once per call (a fixed `[[f32; 2]; 8]`), then one pass
     over frames.
3. **Naming helper** (private, `mod.rs`): `fn asset_name(root: &Path, path: &Path) -> DecodeResult<String>`:
   the relative directory components joined with `/`, then
   `track_name(path)` (stem, raw suffix stripped). Non-UTF-8 components →
   `InvalidData` (skipped with today's "invalid unicode" message). Exact
   scheme per REQ-037 (recommended option 1).
4. **`scan_assets(dir)`.** Iterative walk (explicit stack, depth limit 8,
   so symlinked directory cycles end). `fs::read_dir(dir)?` errors for the
   root only; errors below it are printed and skipped. Skip entries whose
   name starts with `.` (hidden files, `.fixtures/`, `.DS_Store`). Follow
   symlinks via `fs::metadata`. For each file with
   `is_supported_extension`: `probe_file`, then `asset_name`. Unsupported
   extensions are skipped silently in subfolders and with today's message
   at the top level (keeps today's output for flat folders). Collect, sort
   by `(name, path)`, then dedupe by name: the first wins, later ones print
   today's "multiple files with the same name" message. Messages go to
   stderr with `eprintln!` (startup only, never the audio thread).
5. **`preferred_config`.** Today's vote (`mod.rs:73-81`): count rates over
   the deduped assets, max by `(count, rate)`; channels = max `num_channels`.
6. **`conform` and `load_assets(assets, rate, channels)`.** `conform`: if
   `num_channels > channels` → `remap(.., channel_layout, channels)`, set
   `num_channels`, `channel_layout = None`; then if `sample_rate != rate` →
   `resample`, set `sample_rate = rate` (downmix first: fewer channels to
   resample). `load_assets`, per asset: `decode_file`, set
   `file_name = asset.name`; skip (message) on error or fewer samples than
   one frame (today's check, `mod.rs:56-60`); then `conform`.
   `rate == 0 || channels == 0` → `InvalidData`. Empty result →
   `InvalidData("No playable audio files")`.
7. **Parallel loading.** Run step 6's per-asset work on
   `std::thread::available_parallelism()` (capped at 8, at least 1) scoped
   threads pulling indices from an `AtomicUsize`; each result lands in its
   own `Mutex<Option<…>>` slot (or the worker returns `(index, result)`
   pairs that are sorted afterwards), so output order equals input order.
   Messages are printed after the join, in order, so output is
   deterministic. Measure first (step 9): if the benchmark below loads in
   under 0.3 s single-threaded, keep the sequential loop and record the
   numbers in the report instead.
8. **`load_assets_dir(dir)`** (replaces the scaffold stub): `scan_assets`,
   `preferred_config` (none → today's `InvalidData`), `load_assets` at that
   config, collected into a `HashMap` by name, returning
   `(map, rate, channels)` exactly as `main.rs` expects. Behaviour changes
   versus today: subfolders are scanned, and off-rate files are resampled
   instead of skipped.
9. **Measure.** `#[test] #[ignore] fn load_speed_fixture_dir`: builds 16
   one-minute stereo 44.1 kHz WAVs in a temp dir (synthetic sine), loads
   them at 48 kHz, prints the time sequential and parallel. Run with
   `cargo test --release -- --ignored load_speed` and paste into the report.
   Expected from the scratch measurement: ~13 s sequential, ~3 s on 4+ cores.
10. Update tests (below), README module list (`resample.rs`, `mix.rs`),
    `cargo build` (zero warnings), `cargo test`, `cargo clippy` clean in the
    owned files.

## Tests
`resample.rs`:
- `resample_identity_and_degenerate`: `from == to` returns the same
  samples; zero rate/channels returns the input; one frame works.
- `resample_length`: 44.1→48 kHz of 44,100 frames gives 48,000 frames;
  48→44.1 of 48,000 gives 44,100; 22,050→48,000 of 1,000 gives
  `ceil(1000·48000/22050)`; stereo keeps two channels.
- `resample_sine_error_bound`: 1 kHz sine, amplitude 0.5, stereo
  (right = left inverted), 44.1→48 kHz, one second. Skip 512 frames at
  each end; fit the residual delay by scanning `d` in -1.5..1.5 frames in
  0.01 steps; the RMS error against `0.5·sin(2π·1000·(n/48000 - d/48000))`
  is below 1e-3 (measured ~3e-4), and the right channel is the negated left
  within 1e-6.
- `resample_downsample_rejects_alias`: 20 kHz sine at 48 kHz → 22,050 Hz;
  output RMS below 1e-3 (the tone is above the new Nyquist).
- `resample_dc_stays_dc`: constant 0.25 resampled 48→44.1 stays within
  1e-3 of 0.25 away from the edges.

`mix.rs`:
- `remap_mono_duplicates`, `remap_same_count_copies`, `remap_widen_zero_fills`.
- `remap_stereo_to_mono_averages`.
- `remap_51_identical_channels_is_unity`: 6 identical channels (layout
  0x3F) → stereo equal to the input channel within 1e-6.
- `remap_51_coefficients`: an impulse on each of the six channels in turn
  gives the expected normalised (L, R) gains (FL → (1/2.414, 0),
  FC → (0.7071/2.414, 0.7071/2.414), LFE → (0, 0), …).
- `remap_unknown_or_bad_layout_uses_default`: `None` and a mask with the
  wrong popcount both behave like the default for the count.
- `remap_side_vs_back`: 7.1 with layout 0x63F routes SL/SR like BL/BR.
- `remap_to_four_keeps_first`: 6 → 4 keeps channels 0-3.

`mod.rs` (temp directories, as today's tests do; builders from F1's
`test_bytes.rs`):
- Kept and passing unchanged: `loads_all_supported_extensions`,
  `unparseable_file_is_skipped`, `duplicate_names_count_once`,
  `empty_dir_is_error`, `missing_dir_is_io_error`, `loads_real_assets`,
  `empty_tracks_are_skipped` (all via `load_assets_dir`).
- `majority_rate_wins_and_others_are_excluded` becomes
  `majority_rate_wins_and_others_are_resampled`: `c.wav` (48 kHz) is now
  present at 44.1 kHz with the resampled length; the channel count is 2
  (all files count now).
- `scan_recurses_and_names_by_path`: `kick.wav`, `drums/snare.wav`,
  `drums/909/hat.aif` → names `kick`, `drums/snare`, `drums/909/hat`;
  rates and channels come from the probe.
- `scan_skips_hidden`: `.fixtures/a.wav`, `.hidden.wav` not listed.
- `scan_dedupes_same_folder_stems`: `drums/kick.wav` and `drums/kick.aif`
  → one asset (`.aif` sorts first and wins, as today).
- `scan_depth_limit`: nesting 10 deep stops at 8 without error.
- `scan_symlink_loop_terminates` (unix only, `#[cfg(unix)]`): a symlink to
  the parent directory doesn't hang.
- `preferred_config_votes`: ties go to the higher rate; channels are the max.
- `load_downmixes_wide_files_only`: a 6-channel WAV and a mono WAV loaded
  with `channels = 2` → the 6-channel one becomes stereo, the mono one stays
  mono.
- `load_keeps_input_order`: output names equal the input names in order
  (sequential and parallel paths).
- `conform_downmixes_then_resamples`: a 6-channel 24 kHz file conformed to
  (48000, 2) is stereo, 48 kHz, twice the frames; a mono file stays mono.
- `load_raw_by_name`: `kick.s16le.48000.2.raw` in a folder loads as `kick`.
- `load_speed_fixture_dir` (ignored): step 9.

## Done when
- `main.rs` (after the scaffold rename) starts and loads the real assets
  exactly as before; a folder with mixed rates loads every file.
- `scan_assets` never decodes packets (it calls only `probe_file`).
- The speed numbers and the parallel/sequential decision are in the report.

## Risks and open questions
- REQ-037: subfolder naming scheme (user decision). Steps 3-4 depend on it.
- REQ-038: the `load_assets` name clash and the `main.rs` rename. Step 8
  and the scaffold entry depend on it.
- Track names containing `.`: today a stem like `my.loop` already becomes a
  track named `my.loop`, and `.` is the `group.voice` separator. Folders
  with dots (`v1.2/`) would add more. Raised in REQ-037 option notes.
- F1's `probe_file` reads each whole file, so startup reads every file
  twice (scan, then load). Fine for typical sample folders; if the speed
  test shows it matters, F1's `probe` can switch to a `File`-backed stream
  (F1-owned code, a `plan-change` request then).
- Memory: parallel loading holds up to 8 decoded files in flight plus
  their resampled copies. A folder of long files could peak at a few
  hundred MB. Not a problem for samples; noted for TRK's long imports.
- BS.775 normalisation makes a lone front channel ~7.7 dB quieter after a
  5.1 downmix. Alternative (unnormalised, clip-prone): a one-line change if
  the user prefers it.
- `remap` for `to_ch >= 3` with fewer outputs than inputs just truncates.

## Size
L: two small modules (~200 lines with tests), a rewritten `load_assets`
and a recursive scan, about 25 tests. One session, since rubato and the
mixing matrix are short.

## Changelog
- 2026-10-24: first draft.
