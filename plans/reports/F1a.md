# F1a report
Status: ready (for architect review; implemented by the PM under the reduced org, ORG.md top note)
Branch: task/F1a @ 510d020 (one commit on integrate/wave-1 @ a224767)
Worktree: /home/alexander-toth/rust_proj/blast-F1a (contains an untracked `blast/assets` symlink to the main checkout's assets, for the real-asset tests; not committed)

## What was built
- `file_parsing/decode_helpers.rs`: types only. `DecodeError` gets `Display`,
  `std::error::Error`, `From<symphonia::core::errors::Error>` (mapping as in step 1;
  a catch-all arm maps future variants to `InvalidData` because symphonia's
  `Error` is `#[non_exhaustive]`). `AudioFile` gains `channel_layout: Option<u32>`;
  `new` keeps 6 arguments (layout `None`). `pcm_to_i16`, `float_to_i16`,
  `i16_to_f32`, `file_stem` and their tests are gone.
- `file_parsing/symphonia.rs`: `decode`/`probe` as planned, `fill_frames`
  (REQ-034 sizing: last good buffer, else `dur` through `time_base` with checked
  maths, capped at `max_frames_per_packet` and 1 s), the 2^28-sample cap on
  output and fill, the >10% failed-packets rule, and the
  `min(decoded, num_frames, riff_fixup limit)` truncation.
- `file_parsing/riff_fixup.rs`: `repair(bytes) -> (bytes, limit)`. WAV: clamps
  an over-long `data` size, raises (never lowers) the RIFF size to cover
  `fmt `/`data`, moves `fmt ` before `data` (the only case that copies), returns
  `fact` as a limit for non-PCM/float formats. AIFF/AIFC: raises the FORM size,
  returns COMM `num_frames`. All reads are `get`/checked; no indexing on file data.
- `file_parsing/mod.rs`: `decode_file`, `decode_bytes`, `probe_file`,
  `is_supported_extension`, `StreamInfo`, `track_name`, `format_family`;
  re-exports `AudioFile`, `DecodeError`, `DecodeResult`. `load_assets`: only the
  decoder dispatch changed (REQ-035).
- `file_parsing/test_bytes.rs`, `file_parsing/adapter_tests.rs`: builders and
  the ported test table (41 adapter tests, plus 8 `riff_fixup` unit tests and
  `fill_frames_is_bounded`).
- `wav.rs`, `aiff.rs`, `mpeg.rs` deleted. Repo `README.md` module list updated.
- Verified that exactly the five "fails plainly" rows (`wav_data_before_fmt`,
  `wav_streamed_data_size`, `wav_riff_size_too_small`,
  `wav_adpcm_fact_trims_padding`, `aiff_reads_only_num_frames`) fail with
  `riff_fixup` disabled, and pass with it.

## Deviations from plan (with justification)
1. `riff_fixup::repair` takes only `bytes` (no `ext`): it detects WAV/AIFF by
   magic, which is more reliable than the extension. Crate-private; no interface change.
2. `aiff_skips_chunks_before_ssnd`: the old test's `COMT` body `[1,2,3]` is
   malformed (it declares 258 comments), and symphonia parses COMT, so it fails
   with `UnexpectedEof`. The ported test uses a valid empty `COMT` (`[0,0]`)
   plus an unknown odd-sized `XTRA` chunk to keep the pad-byte coverage. (`APPL`
   is also parsed by symphonia and rejects short bodies.)
3. `aiff_truncated_is_eof`: a 6-byte prefix can't be identified at all, so it
   is now some `Err` (not `UnexpectedEof`); a cut inside COMM is
   `UnexpectedEof`, and a cut inside SSND is `UnexpectedEof` (as the old parser).
   Note the asymmetry: a cut-off WAV `data` chunk reads the whole frames present;
   a cut-off AIFF SSND is an error.
4. `aiff_wrong_magic`: 8SVX and `RIFF…AIFF` are `UnsupportedFormat` as planned; the
   short-COMM AIFC case asserts `is_err()`.
5. Steps were done as one commit instead of one per step (the steps depend on
   each other's module declarations; the plan allowed merging steps 1–3).

## For the user (REQ-031 condition 4)
- Still dropped even with the pre-pass: WAV `fmt ` chunks of non-standard size
  (e.g. 20 bytes; symphonia accepts 16, 18, 40). A-law with a 16-byte `fmt` and
  extensible µ-law are errors (A-law/µ-law with an 18-byte `fmt` decode).
- Error-kind changes: wrong magic → `UnsupportedFormat` (was `InvalidData`);
  f16 WAV → `InvalidData` (was `UnsupportedFormat`).
- Newly loadable in `load_assets`: `.wave`, `.aifc`, `.flac`, `.mp1/2/3`,
  `.m4a/.mp4`, `.aac/.adts`.

## Checklist results
- `cargo build`: 0 warnings.
- `cargo test`: `test result: ok. 190 passed; 0 failed` (179 before; with
  `blast/assets` linked so `loads_real_assets`/`decodes_real_assets` ran).
  `corrupt_flac_frame_is_filled` skips until F1b adds `assets/.fixtures/a.flac`.
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets`: no lint in `file_parsing/**`. Clippy still fails
  on the pre-existing deny-level `not_unsafe_ptr_arg_deref` in
  `audio_processing/engine.rs:40` (not F1's; E1/R1 rewrite that code), plus the
  ~53 pre-existing warnings elsewhere.
- No `unwrap()`/`expect()`/indexing in non-test code of `symphonia.rs` or
  `riff_fixup.rs`. No `unsafe`. Nothing outside `file_parsing/` and `README.md` changed
  (`git diff --stat integrate/wave-1...task/F1a`).
- Real-time: none of this code is reachable from `render`/`apply` (decoding
  runs at startup).

## Notes for integration
- No registry stubs. F1b branches from `task/F1a`.
- The engine.rs clippy error predates F1; the architect may want to note it for E1.

## Follow-ups
- None new.
