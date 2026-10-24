# BLAST: Bash-Like Audio Scripting Tool

This project aims to facilitate real-time algorithmic audio in Rust. It features a Bash-like scripting language that is interpreted by a REPL.

## Dependencies

One of the goals of this project is to realize as many features as possible with as few dependencies as possible. It currently only uses the `alsa-sys` and `libc` crates for interaction with OS audio and terminal internals.

## Modules

**src/main.rs**:
- executes audio parsing library functions through extension matching
- catches unsupported audio formats

**src/lib.rs**:
- exposes modules to main.rs and hosts testing

**src/audio_processing**  
- pre-parses all audio files in the assets/ folder
- configures ALSA according to a consensus based on the audio files' properties (namely sample rate and number of channels)
- interacts directly with hardware and the DMA buffer for low-latency writes
- processes Commands separately from audio thread for string parsing, hashmap operations, and robust error-handling
- uses terminal in raw mode for custom terminal rendering
- implements fast RNG with xoroshiro128+ generation, Lemire's fast modulo, and architecture-specific seeding

**src/file_parsing**:
- mod
  - `decode_file`/`decode_bytes`/`probe_file`: one entry point for every supported extension
  - `load_assets`: decodes the assets/ folder and agrees on a sample rate
- symphonia
  - decodes WAV, AIFF/AIFC, FLAC, MP1/2/3, AAC (MP4/ADTS), ALAC and ADPCM through symphonia
  - fills corrupt packets with silence, so timing holds
- riff_fixup
  - repairs WAV/AIFF quirks symphonia rejects (wrong sizes, `data` before `fmt `, `fact`/COMM frame counts)
- decode_helpers
  - implements custom DecodeErrors and DecodeResult for in-memory file parsing
  - provides AudioFile struct to return necessary data for audio APIs

## Documents consulted

**Audio specs**:  
- [DataVoyage's informal MPEG overview](http://mpgedit.org/mpgedit/mpeg_format/mpeghdr.htm#MPEG%20HEADER)  
- [Original AIFF-1.3 specification](https://mmsp.ece.mcgill.ca/Documents/AudioFormats/AIFF/Docs/AIFF-1.3.pdf)  
- [McGill on WAVE](https://www.mmsp.ece.mcgill.ca/Documents/AudioFormats/WAVE/WAVE.html)
