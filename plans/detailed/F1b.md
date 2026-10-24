# F1b: raw PCM, Sun AU, fixture smoke tests
Status: draft   ·   Changelog at the bottom

Second part of F1 (see `F1a.md`). Runs after F1a in wave 1, on `task/F1b`
branched from `task/F1a`'s tip, and owns the same files.

## Summary
Two small hand-written decoders join the symphonia adapter: headerless PCM
(`raw.rs`; parameters from the file name, `decode_with` for explicit specs)
and Sun/NeXT `.au`/`.snd` (`au.rs`). Both share one set of sample converters
(`pcm.rs`), including the G.711 A-law and µ-law tables. `decode_file` routes
to them before symphonia. A fixture suite generated with ffmpeg checks every
supported codec end to end, and the decode speed of a 3-minute MP3 is
measured.

## Ownership
- **Creates:** `file_parsing/pcm.rs`, `file_parsing/raw.rs`,
  `file_parsing/au.rs`, `file_parsing/fixture_tests.rs` (`#[cfg(test)]`).
- **Modifies:** `file_parsing/mod.rs` (module list, `decode_bytes`
  dispatch, `is_supported_extension`, `track_name`).
- **Reads only:** everything F1a reads.

## Registry entries
None.

## Architecture changes
None.

## Interfaces
**Provides** (`file_parsing::raw`, public because TRK's `import --fmt/--rate/--ch`
uses it in wave 3):
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleEncoding { S8, U8, S16Le, S16Be, S24Le, S24Be, S32Le, S32Be, F32Le, F32Be, F64Le, F64Be }
impl FromStr for SampleEncoding;            // "s8" "u8" "s16le" "s16be" … "f64be", ASCII case-insensitive
impl SampleEncoding { pub fn bytes(self) -> usize; pub fn bits(self) -> u32; }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawSpec { pub encoding: SampleEncoding, pub sample_rate: u32, pub channels: u32 }

/// Parameters that may be missing; the file name supplies some, flags others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawParams { pub encoding: Option<SampleEncoding>, pub sample_rate: Option<u32>, pub channels: Option<u32> }
impl RawParams {
    /// Fields set in `self` win over `fallback` (flags over file name).
    pub fn or(self, fallback: RawParams) -> RawParams;
    /// Error naming each missing field.
    pub fn complete(self) -> DecodeResult<RawSpec>;
}

/// Split `kick.s16le.48000.2.raw` into ("kick", params). Recognises the
/// suffixes `<enc>.<rate>.<ch>`, `<enc>.<rate>` and `<enc>` before `.raw`/`.pcm`;
/// anything else is part of the stem.
pub fn parse_raw_name(file_name: &str) -> (&str, RawParams);

/// Decode headerless PCM with an explicit spec (README §3.2).
pub fn decode_with(name: &str, bytes: &[u8], spec: RawSpec) -> DecodeResult<AudioFile>;
```
`file_parsing::au` (crate-private): `pub(super) fn decode(name: &str, bytes: &[u8]) -> DecodeResult<AudioFile>`
and `pub(super) fn probe(bytes: &[u8]) -> DecodeResult<StreamInfo>`.

`file_parsing::pcm` (crate-private):
```rust
pub(super) fn to_f32(bytes: &[u8], encoding: SampleEncoding) -> Vec<f32>;   // trailing partial sample ignored
pub(super) fn ulaw_to_f32(byte: u8) -> f32;
pub(super) fn alaw_to_f32(byte: u8) -> f32;
```
Integer scaling is `x / 2^(bits-1)` (u8: `(x - 128) / 128`), the same full
scale symphonia uses, so a file decodes to identical samples whichever path
reads it. G.711 goes through the standard 16-bit expansion, then `/ 32768`.

**Consumes:** `AudioFile`, `DecodeError`, `StreamInfo`, `track_name` from F1a.

**Contract issues:** none. README §3.2's `decode_with(name, bytes, RawSpec)`
is provided as written; `RawParams`/`parse_raw_name` are additions for TRK's
per-flag overrides (README §4 item 5).

## Steps
1. **`pcm.rs`.** `to_f32` over `chunks_exact(encoding.bytes())`: signed
   integers assembled big- or little-endian into an `i32` (sign-extended for
   24-bit), divided by `2^(bits-1)` as f32 (f64 for 32-bit, then cast, so
   `i32::MAX` maps to just under 1.0); floats read with `from_le_bytes`/
   `from_be_bytes` (f64 cast to f32). No clamping (floats outside ±1 pass
   through; the backend clamps). G.711: the classic `g711.c` expansions,
   written as two small functions (no 256-entry literal tables needed):
   - µ-law: `u = !b; t = (((u & 0x0F) << 3) + 0x84) << ((u >> 4) & 7);`
     result `0x84 - t` if `u & 0x80` else `t - 0x84`.
   - A-law: `a = b ^ 0x55; t = (a & 0x0F) << 4; seg = (a >> 4) & 7;`
     `t += 8` for seg 0, `t = (t + 0x108) << (seg - 1)` for seg ≥ 1; result
     `t` if `a & 0x80` else `-t`.
2. **`raw.rs`.** `SampleEncoding` + `FromStr`, `RawSpec`, `RawParams`,
   `parse_raw_name` (match the three suffix shapes right-anchored, so a stem
   may contain dots: `my.loop.s16le.44100.2.raw` → `my.loop`; rate must be
   1..=768000 and channels 1..=64 to count as spec segments), and
   `decode_with`: channels 0 or rate 0 → `InvalidData`; samples from
   `pcm::to_f32`, then drop a trailing partial frame; zero frames →
   `InvalidData("no audio frames")`. `format = "raw"`, `bits_per_sample =
   encoding.bits()`, `channel_layout = None`. `probe` = `parse_raw_name(..)
   .1.complete()` into a `StreamInfo` (no bytes needed).
3. **`au.rs`.** Header (all big-endian u32): magic `.snd` (`0x2E736E64`),
   data offset, data size, encoding, rate, channels. Header shorter than 24
   bytes → `UnexpectedEof`. Offset < 24 or > file length → `InvalidData`.
   Size `0xFFFFFFFF` or past the end → read to the end of the file.
   Encodings: 1 → µ-law, 2 → s8, 3 → s16be, 4 → s24be, 5 → s32be,
   6 → f32be, 7 → f64be, 27 → A-law; anything else → `UnsupportedFormat`
   naming the code. Rate 0 or channels 0 → `InvalidData`. Drop a trailing
   partial frame; zero frames → `InvalidData`. `format = "au"`,
   `bits_per_sample` 8/8/16/24/32/32/64/8. Little-endian `dns.` files are
   rare and stay unsupported (`UnsupportedFormat`), documented.
4. **Dispatch in `mod.rs`.** `decode_bytes(file_name, bytes)` checks, in
   order: AU magic `.snd` in the first 4 bytes, or ext `au`/`snd` → `au`;
   ext `raw`/`pcm` → `raw` with `parse_raw_name(file_name).1.complete()?`;
   else the F1a symphonia path. Same order in `probe_file`. `is_supported_extension`
   gains `au`, `snd`, `raw`, `pcm`. `track_name` strips a raw spec suffix
   for `.raw`/`.pcm` files (`kick.s16le.48000.2.raw` → `kick`).
5. **Fixture tests** (`fixture_tests.rs`), see Tests. Fixtures live in
   `blast/assets/.fixtures/` (gitignored with `assets/`; the leading dot
   keeps LOAD's recursive scan from treating them as tracks, see
   `LOAD.md`). A missing fixture makes its test return early, the
   `loads_real_assets` pattern.
6. **Speed.** `#[test] #[ignore] fn decode_speed_long_mp3` decodes
   `.fixtures/long.mp3` and prints the elapsed time. Run with
   `cargo test --release -- --ignored decode_speed` and paste the number into
   the report. (Measured in a scratch crate: 0.25 s for 3 minutes of
   44.1 kHz stereo MP3, so "well under a second" holds.)
7. `cargo build`, `cargo test`, `cargo clippy` clean; README module list.

## Tests
**Unit (`pcm.rs`, `raw.rs`, `au.rs`):**
- `pcm_8_bit_signed_and_unsigned`, `pcm_16_both_endians`, `pcm_24_both_endians`,
  `pcm_32`, `pcm_float_32_and_64`, `pcm_ignores_trailing_partial_sample`:
  rewritten from the old `decode_helpers` tests with f32 expectations
  (`0x8000` → -1.0, `0x7FFF` → 32767/32768, …).
- `ulaw_known_values` (0xFF → 0, 0x00 → -32124/32768, 0x80 → 32124/32768),
  `alaw_known_values` (0xD5 → 8/32768, 0x55 → -8/32768, 0xAA → 32256/32768,
  0x2A → -32256/32768).
- `g711_matches_symphonia`: build an A-law WAV and a µ-law WAV (18-byte
  `fmt `, tags 6 and 7) holding all 256 byte values, decode them through
  `decode_bytes("g711.wav", ..)` (symphonia's decoder), and compare with
  `alaw_to_f32`/`ulaw_to_f32` for every byte, exactly.
- `pcm_matches_symphonia`: the same for s16le, s24le, s32le, f32le, u8 via
  WAV, and s16be/s24be via AIFF: our `to_f32` equals symphonia's output.
- `raw_name_parsing`: full, two-part and one-part suffixes; dotted stems;
  bad rate/channel segments stay in the stem; `.pcm` works; uppercase
  `S16LE` parses; a name with no spec returns all-`None` params.
- `raw_params_flags_win`: `RawParams::or` precedence;
  `complete()` errors name the missing fields.
- `raw_decode_with_each_encoding`: every `SampleEncoding`, 2 channels,
  partial final frame dropped; zero channels/rate/frames → `InvalidData`.
- `raw_decode_file_uses_name`: `decode_bytes("x.s16be.22050.1.raw" …)`
  gives rate 22050, mono, name `x`; `x.raw` with no spec →
  `InvalidData` naming encoding, rate and channels.
- `au_each_encoding`: synthetic headers for codes 1–7 and 27.
- `au_unknown_size_reads_to_end`, `au_size_past_end_is_clamped`,
  `au_offset_skips_annotation` (offset 32 with 8 annotation bytes),
  `au_bad_headers` (short header → `UnexpectedEof`, offset < 24 or past end,
  rate 0, channels 0 → `InvalidData`, code 23 → `UnsupportedFormat`),
  `au_magic_wins_over_extension` (AU bytes named `.wav` still decode as AU).
- `truncated_prefixes_never_panic_raw_au`: every prefix of a valid AU file.

**Fixtures** (`fixture_tests.rs`; each test skips when its files are missing).
Generate once with:
```sh
cd blast/assets && mkdir -p .fixtures && cd .fixtures
ffmpeg -v error -y -filter_complex "sine=f=440:r=44100:d=1.5[a];sine=f=660:r=44100:d=1.5[b];[a][b]amerge=inputs=2" -c:a pcm_s16le src.wav
ffmpeg -v error -y -i src.wav -c:a flac          a.flac
ffmpeg -v error -y -i src.wav -c:a alac          a_alac.m4a
ffmpeg -v error -y -i src.wav -c:a pcm_s16be     a.aiff
ffmpeg -v error -y -i src.wav -c:a pcm_s16le -f aiff a_sowt.aifc
ffmpeg -v error -y -i src.wav -c:a pcm_s16be -f au   a.au
ffmpeg -v error -y -i src.wav -f s16le           a.s16le.44100.2.raw
ffmpeg -v error -y -i src.wav -c:a pcm_alaw      a_alaw.wav
ffmpeg -v error -y -i src.wav -c:a pcm_mulaw     a_mulaw.wav
ffmpeg -v error -y -i src.wav -c:a adpcm_ima_wav a_ima.wav
ffmpeg -v error -y -i src.wav -c:a adpcm_ms      a_ms.wav
ffmpeg -v error -y -i src.wav -c:a libmp3lame -b:a 192k a.mp3
ffmpeg -v error -y -i src.wav -c:a aac -b:a 192k        a.m4a
ffmpeg -v error -y -i src.wav -c:a aac -b:a 192k -f adts a.aac
for f in a.mp3 a.m4a a.aac a_ima.wav a_ms.wav a_alaw.wav a_mulaw.wav; do ffmpeg -v error -y -i $f -f f32le -c:a pcm_f32le $f.ref.f32; done
ffmpeg -v error -y -filter_complex "sine=f=440:r=44100:d=180[a];sine=f=660:r=44100:d=180[b];[a][b]amerge=inputs=2" -c:a libmp3lame -b:a 192k long.mp3
ffmpeg -v error -y -f lavfi -i "sine=f=440:r=48000:d=1" -af "pan=5.1|c0=c0|c1=c0|c2=c0|c3=c0|c4=c0|c5=c0" -c:a pcm_s16le six.wav
```
(`*.ref.f32` are ffmpeg's own decodes, read in the tests as raw f32le; the
test reads them with `raw::decode_with`, not by shelling out.)

| Test | Checks |
|---|---|
| `lossless_fixtures_match_source` | `a.flac`, `a_alac.m4a`, `a.aiff`, `a_sowt.aifc`, `a.au`, `a.s16le.44100.2.raw` decode to exactly the samples of `src.wav` (66,150 stereo frames, 44.1 kHz) |
| `g711_fixtures_match_ffmpeg` | `a_alaw.wav`, `a_mulaw.wav` equal their `.ref.f32` exactly |
| `adpcm_fixtures_match_ffmpeg` | `a_ima.wav`, `a_ms.wav`: length equals the reference (66,150) if REQ-031 option 1 is in (the `fact` trim), else within one block (≤ 1,017 frames longer); RMS difference to the reference under -60 dB |
| `mp3_is_gapless` | `a.mp3` decodes to exactly 66,150 frames (measured), RMS of the difference to `a.mp3.ref.f32` under -60 dB |
| `aac_length_and_level` | `a.m4a`: ≤ 1,024 + 66,150 frames (end padding trimmed to `num_frames`; measured 67,174); `a.aac`: ≤ 67,584; RMS of each within 1% of the reference's RMS. Tolerance documented in a comment citing REQ-033 |
| `six_channel_layout` | `six.wav`: 6 channels, `channel_layout == Some(0x3F)` (5.1 mask) |
| `fixture_names_and_formats` | `track_name`/`format` for each fixture (`a_sowt.aifc` → `aiff`, `a.aac` → `aac`, raw → `raw`, name `a`) |
| `decode_speed_long_mp3` (ignored) | prints the time; asserts < 1 s in release builds only (`cfg!(debug_assertions)` → skip the assert) |

## Done when
- Every supported codec in README §3.2 has a fixture test that passes on the
  dev machine with fixtures generated, and skips cleanly without them.
- `decode_file` on `.au`/`.snd`/`.raw`/`.pcm` never touches symphonia.
- The speed number is in the report.

## Risks and open questions
- The lossy-codec tolerances depend on REQ-033 (AAC) and REQ-031 (ADPCM
  `fact`). The tests are written against whichever option is approved.
- `amerge` requires the two sines to share a rate; they do. If an older
  ffmpeg lacks a codec (e.g. no `libmp3lame`), that fixture is missing and
  its test skips.
- Raw files with no spec in the name and no flags can't load at startup;
  they're skipped with a message naming the convention. Only TRK's `import`
  flags can supply it later.

## Size
M: three small modules (~250 lines with tests) plus the fixture suite.

## Changelog
- 2026-10-24: first draft.
