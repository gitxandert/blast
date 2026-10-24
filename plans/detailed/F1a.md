# F1a: decode adapter (symphonia, decode_file, old parsers deleted)
Status: approved   ·   Changelog at the bottom

F1 is split into two parts that run one after the other in wave 1 and own the
same files: **F1a** (this plan: the symphonia adapter, the public entry points,
porting the old tests, deleting the old parsers) and **F1b** (raw PCM, Sun AU,
the shared PCM converters, fixture smoke tests, speed measurement). F1b starts
from F1a's merged branch tip (`task/F1a`), not from `integrate/wave-1`.

Line numbers are for `integrate/wave-1` @ `2f97859` (after scaffold step 0's
`cargo fmt`). The implementer re-checks them on the scaffolded branch.

Decided requests (2026-10-24): REQ-031 option 1 (`riff_fixup` pre-pass, with
the architect's 4 conditions), REQ-032 approved (the architect adds the
`id3v2` feature while scaffolding), REQ-033 option 1 (AAC end-trim only),
REQ-034 option 1 (silence fill, sized as in step 2), REQ-035 answered (F1a
edits only the decoder dispatch in `load_assets`), REQ-036 option 1
(extension whitelist).

## Summary
`file_parsing/` decodes through symphonia 0.6.1 instead of the hand-written
WAV/AIFF parsers. Afterwards there is one entry point,
`decode_file(path) -> DecodeResult<AudioFile>`, plus `decode_bytes` (same thing
from memory, used by tests and by TRK) and `probe_file` (rate and channels
without decoding, used by LOAD's `scan_assets`). WAV, AIFF/AIFC, FLAC,
MP1/2/3, AAC-LC (MP4/M4A/ADTS), ALAC, MS/IMA ADPCM, A-law and µ-law load.
`wav.rs`, `aiff.rs` and `mpeg.rs` are gone, and their edge-case tests live on
as adapter tests. `load_assets` keeps its signature and behaviour but calls
`decode_file`.

## Ownership
- **Creates:** `file_parsing/symphonia.rs`, `file_parsing/riff_fixup.rs`
  (REQ-031), `file_parsing/test_bytes.rs`
  (`#[cfg(test)]` builders), `file_parsing/adapter_tests.rs` (`#[cfg(test)]`).
- **Modifies:** `file_parsing/mod.rs` (module list, new entry points, the
  decoder dispatch inside `load_assets` only, REQ-035), `file_parsing/decode_helpers.rs`
  (becomes types only).
- **Deletes:** `file_parsing/wav.rs`, `file_parsing/aiff.rs`,
  `file_parsing/mpeg.rs`.
- **Reads only:** `main.rs`, `audio_processing/{engine,runtime,processes,test_util}.rs`,
  `commands/**` (they import `file_parsing::decode_helpers::AudioFile` and
  `AudioFile::new`; both paths and the 6-argument `new` must keep compiling).

## Registry entries
None. `lib.rs` already declares `pub mod file_parsing;`, and the module list
inside `file_parsing/mod.rs` is F1's own. `Cargo.toml` gets symphonia in
scaffold step 0. The architect adds `"id3v2"` to symphonia's feature list in
`Cargo.toml` during wave-1 scaffolding (REQ-032); F1 needs no code for it.

## Architecture changes
None in §3. Two decided items touch README §1.1/§3.2 (the architect updates
the README):
- REQ-032 (approved): symphonia's `id3v2` feature (no new crates).
- REQ-036 (approved): `decode_file` sends only whitelisted extensions to
  symphonia.

## Interfaces
**Provides** (`file_parsing/mod.rs`, re-exported at `file_parsing::`):
```rust
/// Decode a file into interleaved f32. The track is named by file stem.
pub fn decode_file(path: &Path) -> DecodeResult<AudioFile>;
/// Same, from bytes already in memory. `file_name` (no directory needed) gives
/// the extension that picks the decoder and, via `track_name`, the track name.
pub fn decode_bytes(file_name: &str, bytes: Vec<u8>) -> DecodeResult<AudioFile>;
/// Rate, channels and layout from the headers only (no packet decoding).
pub fn probe_file(path: &Path) -> DecodeResult<StreamInfo>;
/// Whether `decode_file` would try this extension (lowercase, no dot).
pub fn is_supported_extension(ext: &str) -> bool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamInfo { pub sample_rate: u32, pub num_channels: u32, pub channel_layout: Option<u32> }
```
`file_parsing/decode_helpers.rs` keeps its path (outside code imports it) and
holds only types:
```rust
pub struct AudioFile { file_name, format, sample_rate, num_channels, bits_per_sample,
                       pub channel_layout: Option<u32>, samples: Vec<f32> }   // README §3.1
impl AudioFile { pub fn new(file_name: &str, format: &str, sample_rate: u32, num_channels: u32,
                            bits_per_sample: u32, samples: Vec<f32>) -> Self; }  // layout = None
pub enum DecodeError { Io(io::Error), UnsupportedFormat(String), UnexpectedEof, InvalidData(String) }
impl Display for DecodeError; impl std::error::Error for DecodeError;
pub type DecodeResult<T> = Result<T, DecodeError>;
```
`AudioFile::new` keeps its 6 arguments so the ~15 call sites in engine,
commands and test code compile untouched. If scaffold step 0 has not added
`channel_layout`, F1a adds it (initialised to `None` in `new`).

`file_parsing/symphonia.rs` (crate-private):
```rust
pub(super) fn decode(name: &str, ext: &str, bytes: Vec<u8>) -> DecodeResult<AudioFile>;
pub(super) fn probe(ext: &str, bytes: Vec<u8>) -> DecodeResult<StreamInfo>;
```

**Consumes:** symphonia 0.6.1 (read from the registry source):
`symphonia::default::{get_probe, get_codecs}`,
`core::formats::probe::Hint` (`with_extension`),
`core::io::MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default())`,
`Probe::probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())`,
`FormatReader::{default_track(TrackType::Audio), next_packet() -> Result<Option<Packet>>}`,
`Track::{id, codec_params, num_frames}`, `CodecParameters::audio()`,
`AudioCodecParameters::{sample_rate, channels, bits_per_sample, bits_per_coded_sample}`,
`Channels::{count(), Positioned(Position)}` (`Position::bits()` is the WAVE mask
for its first 18 bits), `CodecRegistry::make_audio_decoder(&params, &AudioDecoderOptions::default())`
(gapless trimming is **on by default** in 0.6: `AudioDecoderOptions { gapless: true }`;
there is no `enable_gapless` on `FormatOptions` any more),
`AudioDecoder::decode(&packet) -> Result<GenericAudioBufferRef>`,
`GenericAudioBufferRef::copy_to_vec_interleaved::<f32>(&mut Vec<f32>)`,
`core::errors::Error::{IoError, DecodeError, Unsupported, LimitError, SeekError, ResetRequired}`.

**Contract issues:**
- README §3.2: "`decode_file` … to symphonia otherwise." The probe scans up
  to 1 MB for any format marker, so a stray `.png` or `.txt` can be "found"
  as MP3 frames. Only whitelisted extensions reach symphonia (REQ-036,
  approved).
- README §3.2 says gapless trimming covers MP3 and AAC. Symphonia 0.6.1
  trims MP3 (LAME/Xing tag) but **not** AAC in MP4/ADTS (its own support
  table says ISO/MP4 and AAC are not gapless). Measured: a 66,150-frame
  source decodes to 67,584 frames from `.m4a` and `.aac`. REQ-033
  (approved, option 1): AAC is end-trimmed only, and README §3.2 says so.
- `AudioFile.format`: the extension family (`wav`, `aiff`, `flac`, `mp3`,
  `m4a`, `aac`), not symphonia's `short_name` (which says `wave`), so today's
  `"wav"`/`"aiff"` strings and the `load_assets` test stay valid.

## Steps
1. **Types.** In `decode_helpers.rs`, delete `pcm_to_i16`, `float_to_i16`
   and their tests (`decode_helpers.rs:62-146` on `dev`; on
   `integrate/wave-1` @ `2f97859` they are at `:76-245`; F1b rewrites the
   converters for raw/AU in `pcm.rs` with their own tests). Scaffold step 0
   also added `i16_to_f32` (`:50`, used only by `wav.rs`/`aiff.rs`: delete
   it with them) and the `#[cfg(test)]` `f32_to_i16` (`:56`, used by the
   `mod.rs` load tests: move it into `test_bytes.rs`). Keep `file_stem` for now
   (step 6 moves it). Add `channel_layout` if step 0 hasn't, a hand-written
   `Display` and `impl std::error::Error` for `DecodeError`, and `///` docs.
   Add `impl From<symphonia::core::errors::Error> for DecodeError`:
   `IoError` of kind `UnexpectedEof` → `UnexpectedEof`, other `IoError` → `Io`,
   `DecodeError(m)`/`LimitError(m)`/`SeekError(_)` → `InvalidData`,
   `Unsupported(m)` → `UnsupportedFormat`, `ResetRequired` →
   `UnsupportedFormat("stream changes format mid-file")`.
   The crate still compiles: `wav.rs`/`aiff.rs` still use the deleted
   helpers, so in the same commit step 2 must already route around them;
   do steps 1–3 as one commit if that's simpler.
2. **`symphonia.rs`.** `decode(name, ext, bytes)`:
   - Build the stream from `Cursor<Vec<u8>>` (seekable, so AIFF with SSND
     before COMM works). Hint with `ext`.
   - `default_track(TrackType::Audio)`, else `UnsupportedFormat("no audio track")`.
     Read rate (missing or 0 → `InvalidData`), channels (`count()`, 0 →
     `InvalidData`), `channel_layout` (`Positioned(p)` →
     `Some((p.bits() & 0x3FFFF) as u32)`, anything else → `None`),
     `bits_per_sample.or(bits_per_coded_sample).unwrap_or(0)` (0 means "lossy,
     not applicable"; document it on the field).
   - Packet loop: `Ok(None)` ends; `Err(IoError)` of kind `UnexpectedEof`
     ends (a truncated last packet; measured on a WAV with `0xFFFFFFFF` sizes);
     other `Err` → return it. Skip packets of other tracks.
   - Decode: `Ok(buf)` → `buf.copy_to_vec_interleaved` into a reused scratch
     `Vec<f32>`, then `extend_from_slice` onto the output; remember the
     buffer's frame count as `last_frames`. If the buffer's channel count
     differs from the track's, return `UnsupportedFormat`.
   - Corrupt packet (REQ-034, approved): `Err(DecodeError | IoError)` on one
     packet → push silence and keep going, counting the failure. The fill
     size, because `Packet::dur` is in `TimeBase` units and file-controlled:
     `last_frames` if a packet has decoded already; else `dur` converted to
     frames through `track.time_base` (`dur * tb.numer * rate / tb.denom`,
     in `u64` with `checked_mul`; overflow → 0); else 0. Cap it at
     `max_frames_per_packet` when the codec parameters give one, and always
     at one second of frames (`sample_rate`). The fill counts against the
     same 2^28-sample cap as the output (exceeding it → `InvalidData`).
     Put the sizing in a small pure function,
     `fn fill_frames(last_frames: Option<usize>, dur: u64, time_base: Option<TimeBase>, rate: u32, max_per_packet: Option<u64>) -> usize`,
     so it's unit-tested without a corrupt file.
   - Packets the demuxer drops (REQ-034, added at review): some readers
     (FLAC: a frame that fails its CRC) skip a corrupt frame themselves, so
     the decoder never sees it and the output comes out short. Track the
     expected next timestamp, `pts + dur + trim_start + trim_end` of the
     previous packet of this track (the untrimmed span, because symphonia
     advances `pts` by the untrimmed duration). If a later packet's `pts` is
     greater than that, fill the gap with silence: convert it to frames
     through `time_base` (checked, as for `dur`), cap it at one second of
     frames per gap and at the 2^28 cap overall, and count it as one failed
     packet. No fill before the first packet, and none when `pts` is not
     ahead (MP3/ADTS readers resync without a gap, which is fine). Put the
     gap-to-frames conversion in a pure helper and unit-test it next to
     `fill_frames`.
     After the loop, fail with `InvalidData` if more than 10% of packets
     failed. Any other `Err` → return it.
   - After the loop: truncate the output to
     `min(decoded, num_frames, riff_fixup limit)` frames, where each bound
     applies only when known (REQ-033: drops AAC end padding; harmless
     elsewhere because the lengths already match). A limit can only shorten
     the output, never lengthen it (REQ-031 condition 2). Zero frames →
     `InvalidData("no audio frames")` (keeps the old "missing SSND is an
     error" behaviour and gives TRK a clean error for empty files).
   - Reserve the output from `num_frames` when it's known and below a sanity
     cap of 2^28 samples, so a hostile header can't force a huge allocation
     up front. Decoded output past the cap → `InvalidData`.
   `probe(ext, bytes)`: the same setup, returning `StreamInfo` from the
   track's codec parameters without reading packets.
3. **Entry points in `mod.rs`.** Replace `pub mod aiff; pub mod mpeg; pub mod
   wav;` (`mod.rs:1-4`) with `pub mod decode_helpers; mod symphonia;`
   (+ `mod riff_fixup;` from step 5, `#[cfg(test)] mod test_bytes;
   #[cfg(test)] mod adapter_tests;`). Add `pub use decode_helpers::{AudioFile,
   DecodeError, DecodeResult};`.
   - `const SYMPHONIA_EXTS: &[&str] = &["wav", "wave", "aif", "aiff", "aifc",
     "flac", "mp1", "mp2", "mp3", "m4a", "mp4", "aac", "adts"]`;
     `is_supported_extension` checks it (F1b adds `au`, `snd`, `raw`, `pcm`).
   - `fn format_family(ext) -> &'static str` (`wave`→`wav`, `aif`/`aifc`→`aiff`,
     `mp1`/`mp2`→`mp3`, `mp4`→`m4a`, `adts`→`aac`).
   - `decode_bytes(file_name, bytes)`: extension lowercased from
     `Path::new(file_name).extension()`; unsupported →
     `UnsupportedFormat(format!("'.{ext}' files"))`; else
     `symphonia::decode`, then set
     `file_name = track_name(Path::new(file_name))?` (step 6),
     `format = format_family(ext)`.
   - `decode_file(path)`: check the extension **before** reading the file,
     then `fs::read(path)?` and `decode_bytes(<path's file name>, bytes)`.
   - `probe_file(path)`: same dispatch, `symphonia::probe`. It reads the
     whole file for now (simple, and LOAD measures it; see Risks).
4. **`load_assets` dispatch** (`mod.rs:42-50`, the `match ext.as_str()`).
   REQ-035 (answered from the brief): F1a may edit only this decoder
   dispatch. Replace the match with: unsupported extension →
   the existing "unsupported format" message and `continue`; else
   `decode_file(&pathbuf)`. Nothing else in `load_assets` changes: not the
   signature, the sorting, duplicate handling, rate vote, or the
   rate-mismatch filter (LOAD replaces those in wave 2). Remove the
   `path.rsplit_once` extension code (`mod.rs:36-41`) if `Path::extension`
   replaces it.
5. **`riff_fixup.rs`** (REQ-031, approved with the architect's conditions).
   `pub(super) fn repair(ext: &str, bytes: Vec<u8>) -> (Vec<u8>, Option<u64>)`
   returns the bytes to hand to symphonia and an optional frame limit.
   Called from `symphonia::decode`/`probe` before probing. Conditions:
   bounds-checked throughout with `get(..)` and checked arithmetic (no
   indexing on file data, no panic); at most one copy of the input, made
   only when chunks are reordered (size fields are patched in place in the
   owned `Vec`); the limit is applied only as a `min` (step 2). Rules:
   - **WAV** (`RIFF`…`WAVE`): wrong sizes are common in the wild, and
     symphonia rejects a chunk that runs past the RIFF size. Walk the chunks;
     if the RIFF size ends before the end of the `fmt `/`data` chunks (as
     present in the file), raise it to cover them (at most `len - 8`). Never
     shrink a RIFF size that already covers them, and never turn bytes after
     `data` into audio: a WAV with junk or an `id3 ` chunk after `data` must
     decode exactly as without them (REQ-031 condition 3). clamp a
     `data` size that runs past the end of the file to the bytes present
     (streamed/unfinalised files with `0xFFFFFFFF`). If `data` precedes
     `fmt `, rebuild the buffer with `fmt ` moved before `data` (the old
     parser accepted this; symphonia stops at `data`). If a `fact` chunk is
     present and the format tag is not PCM/float (i.e. ADPCM), return its
     frame count as the limit (symphonia ignores `fact` and ADPCM files
     otherwise decode up to one block of padding: measured 67,122 frames for
     a 66,150-frame source).
   - **AIFF/AIFC** (`FORM`): same RIFF-size fix; return COMM's `num_frames`
     as the limit (symphonia reads the whole SSND even when COMM declares
     fewer frames).
   - Anything else: unchanged, no limit.
   `symphonia::decode` applies the limit in step 2's `min`.
6. **Track naming.** Move `file_stem` (`decode_helpers.rs:60-71`) to
   `mod.rs` as `pub fn track_name(path: &Path) -> DecodeResult<String>`
   (same rule: the stem, empty is an error). F1b extends it to strip a raw
   spec suffix. LOAD (wave 2) adds the subfolder scheme (REQ-037).
7. **Test helpers and ported tests.** Create `test_bytes.rs` with the old
   builders, made `pub(super)`: `wav_bytes`, `aiff_bytes` (from
   `mod.rs:115-151`), and the `wav.rs` builders `chunk`/`riff`/`fmt_body`/
   `fmt_ext_body` and friends (`wav.rs:244-292`, `chunk` renamed
   `chunk_le`), the `aiff.rs` builders `chunk`/`form`/`extended`/
   `comm_body`/`ssnd_body` (`aiff.rs:242-288`, `chunk` renamed `chunk_be`). Point the `mod.rs` load tests at them.
   Write `adapter_tests.rs` (table below) calling `decode_bytes` directly,
   no temp files.
8. **Delete** `wav.rs`, `aiff.rs`, `mpeg.rs`. `mpeg.rs` has no tests and
   was never wired into `load_assets`.
9. Update the README module list if it lists `file_parsing` files
   (README §1.4). `cargo build` (zero warnings), `cargo test`,
   `cargo clippy` clean in `file_parsing/**`.

## Tests
All in `file_parsing/adapter_tests.rs` unless noted, feeding the same
synthetic bytes as the old tests through `decode_bytes`. Expected samples are
now f32 at full scale (`x / 2^(bits-1)`, u8 as `(x - 128) / 128`), compared
exactly (these conversions are exact in f32). Measured results against
symphonia 0.6.1 in a scratch crate are in the "today" column.

| New test | Ported from | Result today with plain symphonia |
|---|---|---|
| `wav_pcm16` | `wav::parses_pcm_wav` (rate, channels, bits, samples, format `"wav"`, name) | passes |
| `wav_extensible16` | `wav::parses_extensible_wav` | passes |
| `wav_fmt18_zero_cb_size` | `wav::parses_fmt_18_with_zero_cb_size` | passes |
| `wav_truncated_data_reads_whole_frames` | `wav::truncated_file_is_eof` (first half: 4 of 6 samples) | passes |
| `wav_truncated_header_is_eof` | `wav::truncated_file_is_eof` (second half) | passes (`UnexpectedEof`) |
| `wav_u8`, `wav_s24`, `wav_s32`, `wav_f32`, `wav_f64` | `wav::converts_*` | pass |
| `wav_extensible_sub_format_and_valid_bits` | `wav::extensible_uses_sub_format_and_valid_bits` | samples pass; check what `bits_per_sample` reports for 20-in-24 and assert that (informational field) |
| `wav_unsupported_formats_are_errors` | `wav::unsupported_formats_are_errors` | all four are errors; f16 now maps to `InvalidData` (symphonia calls it malformed), so assert `is_err()` for that case. Note: A-law/µ-law with an 18-byte `fmt` now **decode** (F1b fixture test); the old 16-byte-fmt A-law and extensible µ-law cases stay errors |
| `wav_skips_list_chunk` | `wav::skips_list_chunk_between_fmt_and_data` | passes |
| `wav_odd_chunk_padding` | `wav::honours_pad_byte_of_odd_sized_chunk` | passes |
| `wav_data_before_fmt` | `wav::data_before_fmt` | fails plainly (`missing format chunk`); passes with `riff_fixup` |
| `wav_streamed_data_size` | `wav::streamed_data_size_reads_to_end` (data size `0xFFFFFFFF`, RIFF size valid) | fails plainly (`chunk length exceeds parent`); passes with `riff_fixup` |
| `wav_streamed_riff_and_data_size` | new: both sizes `0xFFFFFFFF` (what ffmpeg writes to a pipe) | passes |
| `wav_riff_size_too_small` | new (old parser ignored the RIFF size) | fails plainly; passes with `riff_fixup` |
| `wav_trailing_bytes_after_riff` | new (REQ-031 condition 3): the `wav_pcm16` bytes plus 37 junk bytes after the RIFF chunk, and plus an `id3 ` chunk after `data` | both decode to exactly the `wav_pcm16` samples |
| `riff_fixup_limit_only_shortens` | new (REQ-031 condition 2): `fact` and COMM counts larger than the data | output length equals the data's frame count, not the declared one |
| `wav_wrong_magic` | `wav::wrong_magic_is_invalid` | errors; now `UnsupportedFormat` (no reader matches) instead of `InvalidData`; assert that |
| `wav_missing_chunks` | `wav::missing_chunks_are_errors` | no `data` → `UnsupportedFormat`; no `fmt ` → `InvalidData`; 4-byte `fmt ` → `InvalidData` |
| `wav_empty_data_is_no_audio` | new | `InvalidData("no audio frames")` (our check) |
| `wav_adpcm_fact_trims_padding` | new, synthetic IMA ADPCM block + `fact` | passes with `riff_fixup` |
| `aiff_s16`, `aiff_s8`, `aiff_s24`, `aiff_s32` | `aiff::parses_16_bit_aiff`, `converts_*` | pass |
| `aiff_skips_chunks_before_ssnd` | `aiff::skips_chunks_before_ssnd` | passes |
| `aiff_ssnd_before_comm` | `aiff::ssnd_before_comm_and_larger_comm` | passes |
| `aiff_ssnd_offset` | `aiff::ssnd_offset_is_skipped` (both halves) | passes |
| `aiff_reads_only_num_frames` | `aiff::reads_only_num_frames` | first half fails plainly (reads all 3 frames); passes with `riff_fixup`. Second half passes |
| `aiff_truncated_is_eof` | `aiff::truncated_file_is_eof` | passes |
| `aiff_tiny_ssnd_is_error` | `aiff::tiny_ssnd_is_error_not_panic` | passes |
| `aiff_missing_chunks` | `aiff::missing_chunks_are_errors` | no SSND → our zero-frames `InvalidData`; no/short COMM → `InvalidData` |
| `aiff_wrong_magic` | `aiff::wrong_magic_is_rejected` | 8SVX and `RIFF…AIFF` → `UnsupportedFormat`; an `AIFC` with an 18-byte COMM → error (assert `is_err()`; real AIFC now decodes, see F1b's `sowt` fixture) |
| `aiff_unsupported_sample_size` | `aiff::unsupported_sample_size_is_error` | error, now `InvalidData`; assert `is_err()` |
| `unknown_extension_is_unsupported` | new | `.txt`, `.png`, no extension → `UnsupportedFormat` without probing |
| `garbage_with_audio_extension_is_error` | new: 1 KiB of `0xFF 0xFB …` noise named `.mp3`, random bytes named `.wav`/`.flac`/`.m4a` | `Err`, no panic (REQ-034's 10% rule) |
| `fill_frames_is_bounded` (`symphonia.rs`) | new (REQ-034) | prefers `last_frames`; converts `dur` through a 1/90000 time base correctly; `u64::MAX` `dur` and a zero/absent time base give a bounded result; never above `max_per_packet` or one second of frames |
| `corrupt_flac_frame_is_filled` | new (REQ-034): F1b's `.fixtures/a.flac` read into memory with one byte flipped in the middle of the stream (skips if the fixture is missing) | `Ok`, same frame count as the intact file |
| `truncated_prefixes_never_panic` | new: every prefix length 0..len of a valid WAV and AIFF | each returns `Ok` or `Err`, none panics |
| `track_name_needs_no_directory` (`mod.rs`) | `wav::name_needs_no_directory` | passes |
| `decodes_real_assets` (`mod.rs`) | `wav::parses_real_assets` (skips without `assets/`) | passes |

Dropped with their helpers (they tested internals that no longer exist):
`wav::parse_bytes_little_endian`, `aiff::parse_bytes_big_endian`,
`aiff::ieee_extended_rates` (symphonia's AIFF reader parses the rate; the
`extended()` builder survives in `test_bytes.rs` and `aiff_s16` checks the
rate end to end), `decode_helpers::*` (rewritten in F1b's `pcm.rs`).

The existing `load_assets` tests in `mod.rs` stay and must pass unchanged
(after step 0's f32 conversion).

## Done when
- `wav.rs`, `aiff.rs`, `mpeg.rs` and the `pcm_to_*`/`float_to_*` helpers are
  gone; nothing outside `file_parsing/` changed.
- Every row above exists and passes (the "fails plainly" rows pass through
  `riff_fixup`).
- The F1 report lists for the user (REQ-031 condition 4) the cases that stay
  dropped (WAV `fmt ` chunks of non-standard size such as 20 bytes) and the
  error-kind changes (wrong magic → `UnsupportedFormat`, f16 WAV →
  `InvalidData`).
- `load_assets` loads the same four real assets as before, with identical
  frame counts.
- No `unwrap()`/`expect()`/indexing on file-controlled data in
  `symphonia.rs` or `riff_fixup.rs`.

## Risks and open questions
- All F1 requests are decided (see the top). Remaining known limitation:
  AAC keeps its 1,024–2,112-frame encoder priming (REQ-033); README §3.2
  documents it.
- Formats that the old parser accepted and stay dropped even with the
  pre-pass: WAV `fmt ` chunks of non-standard size (e.g. 20 bytes; symphonia
  accepts 16, 18, 40). Listed in REQ-031 and in the F1 report.
- `probe_file` reads the whole file. LOAD measures `scan_assets` on a large
  directory in wave 2; if it's slow, LOAD can switch `probe` to a
  `File`-backed `MediaSourceStream` (no API change).
- Symphonia logs through the `log` crate; no logger is installed, so it's
  silent. Nothing to do.

## Size
L: one new adapter module, one small repair module, ~35 ported tests. Fits
one session because the decode logic is short; the bulk is mechanical test
porting.

## Changelog
- 2026-10-24: first draft.
- 2026-10-24: step 1 line refs for `integrate/wave-1`; scaffold step 0's `i16_to_f32`/`f32_to_i16` handled (editorial).
- 2026-10-24: approved (REQ-043), conditional on the architect's fixes, now applied: REQ-034 silence sizing (step 2, new tests `fill_frames_is_bounded`, `corrupt_flac_frame_is_filled`); REQ-031 conditions (step 5, `min`-only limit, tests `wav_trailing_bytes_after_riff` and `riff_fixup_limit_only_shortens`, report list); `Path::new` in `decode_bytes`; line refs and header moved to 2f97859. Request-dependent branches resolved per the decisions on REQ-031..036. Status → approved.
- 2026-10-24 (architect, branch review): step 2 gains the demuxer-gap fill. `corrupt_flac_frame_is_filled` failed once F1b's fixtures existed (123,084 vs 132,300 samples: symphonia's FLAC reader drops the CRC-failing 4,608-frame block before the decoder sees it). This is within REQ-034's approved intent (keep timing).
