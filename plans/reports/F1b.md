# F1b report
Status: ready (for architect review; implemented by the PM)
Branch: task/F1b @ edaf9b2 (one commit on task/F1a @ ffa3a2c)
Worktree: /home/alexander-toth/rust_proj/blast-F1b (untracked `blast/assets` symlink, not committed)

## What was built
- `pcm.rs`: `to_f32` for every `SampleEncoding` (scaling identical to
  symphonia's: `x/2^(bits-1)`, i32 through f64, u8 as `x/128 - 1`), and
  `ulaw_to_f32`/`alaw_to_f32` (g711.c expansions, arithmetic in i32).
- `raw.rs` (public, for TRK): `SampleEncoding` (+ `FromStr`, `bytes`, `bits`),
  `RawSpec`, `RawParams` (`or`, `complete` naming missing fields),
  `parse_raw_name` (right-anchored 3/2/1-part suffixes, rate 1..=768000,
  channels 1..=64, dotted stems kept), `decode_with`, crate-private `probe`.
- `au.rs`: big-endian `.snd` header, encodings 1–7 and 27, size
  `0xFFFFFFFF`/past end read to end of file, partial frame dropped; `dns.`
  is `UnsupportedFormat`.
- `mod.rs`: dispatch AU (magic or `au`/`snd` ext) → raw (`raw`/`pcm`) →
  symphonia, in both `decode_bytes` and `probe_file` (raw probe needs no file
  read); whitelist gains `au snd raw pcm`; `track_name` strips the raw spec.
- `fixture_tests.rs`: every row of the plan's fixture table, plus the
  cross-checks `g711_matches_symphonia`, `pcm_matches_symphonia`,
  `raw_decode_file_uses_name`, `au_magic_wins_over_extension`.
- Fixtures generated with the plan's ffmpeg commands (ffmpeg 6.1.1) into the
  main checkout's `blast/assets/.fixtures/` (gitignored with assets).
- Repo `README.md` module list updated.

## Deviations from plan
1. The four cross-check tests sit in `fixture_tests.rs` rather than the
   unit-test modules (they need `test_bytes` and `decode_bytes`).
2. `adpcm_fixtures_match_ffmpeg`: ffmpeg also ignores `fact` (its references are
   67,122 and 66,792 frames), so the length is checked against the source
   (66,150, exact via `riff_fixup`) and the RMS difference over the overlap.
3. `fixture_names_and_formats`: names are the full stems (`a_sowt`, `a_alac`),
   not `a`; the plan's "name `a`" applies to the `a.*` files.

## Speed
`cargo test --release -- --ignored decode_speed --nocapture`:
`decoded 7938000 frames of long.mp3 in 0.321 s` (3 min, 44.1 kHz stereo, 192k).

## Checklist results
- `cargo build`: 0 warnings. `cargo fmt --check`: clean.
- `cargo test`: `219 passed; 0 failed; 1 ignored` with fixtures; the same
  with `.fixtures` moved away (fixture tests return early).
- `cargo clippy --all-targets`: no lint in `file_parsing/**`; the
  pre-existing deny-level `engine.rs:40` error remains (not F1's).
- `decode_file` on `.au/.snd/.raw/.pcm` never reaches symphonia. No `unsafe`;
  slice indexing only on `chunks_exact` chunks and the length-checked AU header.
- Not reachable from `render`/`apply`.
- Only `file_parsing/**` and `README.md` changed vs task/F1a.

## Notes for integration
- Merge after task/F1a (F1b is branched from it). No registry stubs.
- Until LOAD (wave 2), the flat `load_assets` prints one "unsupported format"
  line for the `.fixtures` directory when fixtures exist.

## Follow-ups
- None.
