mod au;
pub mod decode_helpers;
mod pcm;
pub mod raw;
mod riff_fixup;
mod symphonia;

#[cfg(test)]
mod adapter_tests;
#[cfg(test)]
mod fixture_tests;
#[cfg(test)]
mod test_bytes;

pub use decode_helpers::{AudioFile, DecodeError, DecodeResult};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Rate, channels and layout from a file's headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    pub sample_rate: u32,
    pub num_channels: u32,
    pub channel_layout: Option<u32>,
}

// extensions symphonia decodes for us (README §3.2, REQ-036)
const SYMPHONIA_EXTS: &[&str] = &[
    "wav", "wave", "aif", "aiff", "aifc", "flac", "mp1", "mp2", "mp3", "m4a", "mp4", "aac", "adts",
];

// extensions the hand-written decoders read
const OWN_EXTS: &[&str] = &["au", "snd", "raw", "pcm"];

/// Whether `decode_file` would try this extension (lowercase, no dot).
pub fn is_supported_extension(ext: &str) -> bool {
    SYMPHONIA_EXTS.contains(&ext) || OWN_EXTS.contains(&ext)
}

fn is_raw(ext: &str) -> bool {
    ext == "raw" || ext == "pcm"
}

fn is_au(ext: &str, bytes: &[u8]) -> bool {
    ext == "au" || ext == "snd" || bytes.starts_with(au::MAGIC)
}

// the format family an extension belongs to, for `AudioFile::format`
fn format_family(ext: &str) -> &'static str {
    match ext {
        "wav" | "wave" => "wav",
        "aif" | "aiff" | "aifc" => "aiff",
        "flac" => "flac",
        "mp1" | "mp2" | "mp3" => "mp3",
        "m4a" | "mp4" => "m4a",
        "aac" | "adts" => "aac",
        _ => "unknown",
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn check_extension(path: &Path) -> DecodeResult<String> {
    let ext = extension(path);
    if is_supported_extension(&ext) {
        Ok(ext)
    } else {
        Err(DecodeError::UnsupportedFormat(format!("'.{ext}' files")))
    }
}

/// A track's name: its file name without directory or extension (and
/// without a raw spec suffix such as `.s16le.48000.2`).
pub fn track_name(path: &Path) -> DecodeResult<String> {
    let stem = if is_raw(&extension(path)) {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| raw::parse_raw_name(n).0)
    } else {
        path.file_stem().and_then(|s| s.to_str())
    };
    match stem {
        Some(name) if !name.is_empty() => Ok(name.to_string()),
        _ => Err(DecodeError::InvalidData(format!(
            "File has no name: '{}'",
            path.display()
        ))),
    }
}

/// Decodes a file into interleaved f32. The track is named by file stem.
pub fn decode_file(path: &Path) -> DecodeResult<AudioFile> {
    check_extension(path)?;
    let bytes = fs::read(path)?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    decode_bytes(file_name, bytes)
}

/// Same as `decode_file`, from bytes already in memory. `file_name` (no
/// directory needed) gives the extension that picks the decoder and the track
/// name.
pub fn decode_bytes(file_name: &str, bytes: Vec<u8>) -> DecodeResult<AudioFile> {
    let path = Path::new(file_name);
    let ext = check_extension(path)?;
    let name = track_name(path)?;
    if is_au(&ext, &bytes) {
        return au::decode(&name, &bytes);
    }
    if is_raw(&ext) {
        let spec = raw::parse_raw_name(file_name).1.complete()?;
        return raw::decode_with(&name, &bytes, spec);
    }
    let mut file = symphonia::decode(&name, &ext, bytes)?;
    file.format = format_family(&ext).to_string();
    Ok(file)
}

/// Rate, channels and layout from the headers only (no packet decoding).
pub fn probe_file(path: &Path) -> DecodeResult<StreamInfo> {
    let ext = check_extension(path)?;
    if is_raw(&ext) {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        return raw::probe(file_name);
    }
    let bytes = fs::read(path)?;
    if is_au(&ext, &bytes) {
        return au::probe(&bytes);
    }
    symphonia::probe(&ext, bytes)
}

// parse every supported file in dir and agree on a sample rate
// and channel count; returns (tracks by name, sample rate, num channels)
pub fn load_assets(dir: &str) -> DecodeResult<(HashMap<String, AudioFile>, u32, u32)> {
    let mut tracks = HashMap::<String, AudioFile>::new();
    let mut sample_rates = HashMap::<u32, u32>::new();

    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        match entry {
            Ok(dir_entry) => paths.push(dir_entry.path()),
            Err(error) => println!("Error: {error}"),
        }
    }
    // stable order, so duplicates resolve the same way every run
    paths.sort();

    for pathbuf in paths {
        let path = match pathbuf.to_str() {
            Some(valid) => valid,
            None => {
                println!("Error: invalid unicode in '{:?}'", pathbuf);
                continue;
            }
        };

        if !is_supported_extension(&extension(&pathbuf)) {
            println!("Error: unsupported format for '{}'", path);
            continue;
        }
        let parsed = decode_file(&pathbuf);

        let track: AudioFile = match parsed {
            Ok(file) => file,
            Err(error) => {
                println!("Error: could not parse '{}': {:?}", path, error);
                continue;
            }
        };

        // a Voice needs at least one whole frame to play
        if track.num_channels == 0 || track.samples.len() < track.num_channels as usize {
            println!("Error: '{}' has no audio, skipping", path);
            continue;
        }

        if tracks.contains_key(&track.file_name) {
            println!(
                "Error: multiple files with the same name {}, skipping '{}'",
                track.file_name, path
            );
            continue;
        }

        *sample_rates.entry(track.sample_rate).or_insert(0) += 1;
        tracks.insert(track.file_name.clone(), track);
    }

    // most common rate wins; ties go to the higher rate
    let Some((mutual_rate, _)) = sample_rates
        .into_iter()
        .max_by_key(|(rate, count)| (*count, *rate))
    else {
        return Err(DecodeError::InvalidData(format!(
            "No playable audio files in '{dir}'"
        )));
    };

    // no resampling yet, so files at other rates would play at the wrong pitch
    tracks.retain(|name, track| {
        if track.sample_rate != mutual_rate {
            println!(
                "Error: skipping '{}': sample rate {} differs from {}",
                name, track.sample_rate, mutual_rate
            );
            return false;
        }
        true
    });

    let num_channels: u32 = tracks
        .values()
        .map(|track| track.num_channels)
        .max()
        .unwrap_or(2);

    Ok((tracks, mutual_rate, num_channels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_parsing::test_bytes::{aiff_bytes, f32_to_i16, wav_bytes};
    use std::path::PathBuf;

    // fresh directory filled with (file name, bytes)
    fn temp_assets(name: &str, files: &[(&str, Vec<u8>)]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blast_assets_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (file, bytes) in files {
            fs::write(dir.join(file), bytes).unwrap();
        }
        dir
    }

    fn load(dir: &PathBuf) -> DecodeResult<(HashMap<String, AudioFile>, u32, u32)> {
        let result = load_assets(&format!("{}/", dir.to_str().unwrap()));
        fs::remove_dir_all(dir).unwrap();
        result
    }

    fn sorted_names(tracks: &HashMap<String, AudioFile>) -> Vec<&str> {
        let mut names: Vec<&str> = tracks.keys().map(|k| k.as_str()).collect();
        names.sort();
        names
    }

    #[test]
    fn loads_all_supported_extensions() {
        let dir = temp_assets(
            "ext",
            &[
                ("a.wav", wav_bytes(48000, 2, &[1, 2])),
                ("b.aif", aiff_bytes(48000, 1, &[3])),
                ("c.aiff", aiff_bytes(48000, 2, &[4, 5])),
                ("d.AIF", aiff_bytes(48000, 1, &[6])),
                ("e.WAV", wav_bytes(48000, 1, &[7])),
                ("notes.txt", b"not audio".to_vec()),
                ("noext", b"nothing".to_vec()),
            ],
        );
        let (tracks, rate, channels) = load(&dir).unwrap();
        assert_eq!(sorted_names(&tracks), vec!["a", "b", "c", "d", "e"]);
        assert_eq!(rate, 48000);
        assert_eq!(channels, 2);
        assert_eq!(f32_to_i16(&tracks["c"].samples), vec![4, 5]);
        assert_eq!(tracks["d"].format, "aiff");
    }

    #[test]
    fn unparseable_file_is_skipped() {
        let dir = temp_assets(
            "bad",
            &[
                ("good.wav", wav_bytes(48000, 2, &[1, 2])),
                ("bad.wav", b"RIFF\x04\x00\x00\x00WAVE".to_vec()),
            ],
        );
        let (tracks, _, _) = load(&dir).unwrap();
        assert_eq!(sorted_names(&tracks), vec!["good"]);
    }

    #[test]
    fn majority_rate_wins_and_others_are_excluded() {
        let dir = temp_assets(
            "rates",
            &[
                ("a.wav", wav_bytes(44100, 1, &[1])),
                ("b.wav", wav_bytes(44100, 1, &[2])),
                ("c.wav", wav_bytes(48000, 2, &[3, 4])),
            ],
        );
        let (tracks, rate, channels) = load(&dir).unwrap();
        assert_eq!(rate, 44100);
        assert_eq!(sorted_names(&tracks), vec!["a", "b"]);
        // the excluded stereo file doesn't decide the channel count
        assert_eq!(channels, 1);
    }

    #[test]
    fn duplicate_names_count_once() {
        // kick.aif sorts first and is kept; counting the other kicks would tip the vote to 44100
        let dir = temp_assets(
            "dups",
            &[
                ("kick.aif", aiff_bytes(48000, 1, &[1])),
                ("kick.aiff", aiff_bytes(44100, 1, &[2])),
                ("kick.wav", wav_bytes(44100, 1, &[3])),
                ("snare.wav", wav_bytes(44100, 1, &[4])),
                ("hat.wav", wav_bytes(48000, 1, &[5])),
            ],
        );
        let (tracks, rate, _) = load(&dir).unwrap();
        assert_eq!(rate, 48000);
        assert_eq!(sorted_names(&tracks), vec!["hat", "kick"]);
        assert_eq!(tracks["kick"].format, "aiff");
    }

    #[test]
    fn empty_dir_is_error() {
        let dir = temp_assets("empty", &[]);
        assert!(matches!(load(&dir), Err(DecodeError::InvalidData(_))));
        let dir = temp_assets("only_txt", &[("readme.txt", b"hi".to_vec())]);
        assert!(matches!(load(&dir), Err(DecodeError::InvalidData(_))));
    }

    #[test]
    fn missing_dir_is_io_error() {
        let dir = std::env::temp_dir().join(format!("blast_assets_missing_{}", std::process::id()));
        assert!(matches!(
            load_assets(dir.to_str().unwrap()),
            Err(DecodeError::Io(_))
        ));
    }

    #[test]
    fn loads_real_assets() {
        // assets/ is gitignored, so skip on checkouts without it
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/");
        let names = ["kick", "rim1", "shaker", "snare"];
        if !names
            .iter()
            .all(|n| std::path::Path::new(&format!("{dir}{n}.wav")).exists())
        {
            return;
        }
        let (tracks, rate, channels) =
            load_assets(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/")).unwrap();
        assert_eq!(
            sorted_names(&tracks),
            vec!["kick", "rim1", "shaker", "snare"]
        );
        assert_eq!(rate, 48000);
        assert_eq!(channels, 2);
    }

    #[test]
    fn empty_tracks_are_skipped() {
        let dir = temp_assets(
            "no_audio",
            &[
                ("a.wav", wav_bytes(48000, 2, &[1, 2])),
                ("e.wav", wav_bytes(48000, 2, &[])),
            ],
        );
        let (tracks, _, _) = load(&dir).unwrap();
        assert_eq!(sorted_names(&tracks), vec!["a"]);
    }

    #[test]
    fn track_name_needs_no_directory() {
        assert_eq!(track_name(Path::new("kick.wav")).unwrap(), "kick");
        assert_eq!(track_name(Path::new("a/b/snare.aif")).unwrap(), "snare");
        assert_eq!(track_name(Path::new("noext")).unwrap(), "noext");
        assert!(track_name(Path::new("")).is_err());
    }

    #[test]
    fn decodes_real_assets() {
        // assets/ is gitignored, so skip on checkouts without it
        for name in ["kick", "rim1", "shaker", "snare"] {
            let path = format!("{}/assets/{}.wav", env!("CARGO_MANIFEST_DIR"), name);
            if !Path::new(&path).exists() {
                continue;
            }
            let file = decode_file(Path::new(&path)).unwrap();
            assert_eq!(file.file_name, name);
            assert_eq!(file.sample_rate, 48000);
            assert_eq!(file.num_channels, 2);
            assert_eq!(file.bits_per_sample, 16);
            assert_eq!(file.samples.len(), 72000);
        }
    }
}
