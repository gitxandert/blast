pub mod aiff;
pub mod decode_helpers;
pub mod mpeg;
pub mod wav;

use std::fs;
use std::collections::HashMap;
use decode_helpers::{AudioFile, DecodeError, DecodeResult};

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

        let ext: String = match path.rsplit_once(|b: char| b == '.') {
            Some((before, after)) if !before.is_empty() && !after.is_empty() => after.to_ascii_lowercase(),
            _ => String::new(),
        };

        let parsed = match ext.as_str() {
            // TODO: figure out actual mpeg decoding...
            "wav" => wav::parse(path),
            "aif" | "aiff" => aiff::parse(path),
            _ => {
                println!("Error: unsupported format for '{}'", path);
                continue;
            }
        };

        let track: AudioFile = match parsed {
            Ok(file) => file,
            Err(error) => {
                println!("Error: could not parse '{}': {:?}", path, error);
                continue;
            }
        };

        if tracks.contains_key(&track.file_name) {
            println!("Error: multiple files with the same name {}, skipping '{}'", track.file_name, path);
            continue;
        }

        *sample_rates.entry(track.sample_rate).or_insert(0) += 1;
        tracks.insert(track.file_name.clone(), track);
    }

    // most common rate wins; ties go to the higher rate
    let Some((mutual_rate, _)) = sample_rates.into_iter().max_by_key(|(rate, count)| (*count, *rate)) else {
        return Err(DecodeError::InvalidData(format!("No playable audio files in '{dir}'")));
    };

    // no resampling yet, so files at other rates would play at the wrong pitch
    tracks.retain(|name, track| {
        if track.sample_rate != mutual_rate {
            println!("Error: skipping '{}': sample rate {} differs from {}", name, track.sample_rate, mutual_rate);
            return false;
        }
        true
    });

    let num_channels: u32 = tracks.values().map(|track| track.num_channels).max().unwrap_or(2);

    Ok((tracks, mutual_rate, num_channels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn wav_bytes(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 2 * channels as u32).to_le_bytes());
        v.extend_from_slice(&(2 * channels).to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&data);
        v
    }

    fn aiff_bytes(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_be_bytes()).collect();
        let shift = (rate as u64).leading_zeros();
        let mut v = b"FORM".to_vec();
        v.extend_from_slice(&(46 + data.len() as u32).to_be_bytes());
        v.extend_from_slice(b"AIFFCOMM");
        v.extend_from_slice(&18u32.to_be_bytes());
        v.extend_from_slice(&channels.to_be_bytes());
        v.extend_from_slice(&(samples.len() as u32 / channels as u32).to_be_bytes());
        v.extend_from_slice(&16u16.to_be_bytes());
        v.extend_from_slice(&(16383 + 63 - shift as u16).to_be_bytes());
        v.extend_from_slice(&((rate as u64) << shift).to_be_bytes());
        v.extend_from_slice(b"SSND");
        v.extend_from_slice(&(8 + data.len() as u32).to_be_bytes());
        v.extend_from_slice(&[0; 8]);
        v.extend_from_slice(&data);
        v
    }

    // fresh directory filled with (file name, bytes)
    fn temp_assets(name: &str, files: &[(&str, Vec<u8>)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blast_assets_{}_{}", name, std::process::id()));
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
        let dir = temp_assets("ext", &[
            ("a.wav", wav_bytes(48000, 2, &[1, 2])),
            ("b.aif", aiff_bytes(48000, 1, &[3])),
            ("c.aiff", aiff_bytes(48000, 2, &[4, 5])),
            ("d.AIF", aiff_bytes(48000, 1, &[6])),
            ("e.WAV", wav_bytes(48000, 1, &[7])),
            ("notes.txt", b"not audio".to_vec()),
            ("noext", b"nothing".to_vec()),
        ]);
        let (tracks, rate, channels) = load(&dir).unwrap();
        assert_eq!(sorted_names(&tracks), vec!["a", "b", "c", "d", "e"]);
        assert_eq!(rate, 48000);
        assert_eq!(channels, 2);
        assert_eq!(tracks["c"].samples, vec![4, 5]);
        assert_eq!(tracks["d"].format, "aiff");
    }

    #[test]
    fn unparseable_file_is_skipped() {
        let dir = temp_assets("bad", &[
            ("good.wav", wav_bytes(48000, 2, &[1, 2])),
            ("bad.wav", b"RIFF\x04\x00\x00\x00WAVE".to_vec()),
        ]);
        let (tracks, _, _) = load(&dir).unwrap();
        assert_eq!(sorted_names(&tracks), vec!["good"]);
    }

    #[test]
    fn majority_rate_wins_and_others_are_excluded() {
        let dir = temp_assets("rates", &[
            ("a.wav", wav_bytes(44100, 1, &[1])),
            ("b.wav", wav_bytes(44100, 1, &[2])),
            ("c.wav", wav_bytes(48000, 2, &[3, 4])),
        ]);
        let (tracks, rate, channels) = load(&dir).unwrap();
        assert_eq!(rate, 44100);
        assert_eq!(sorted_names(&tracks), vec!["a", "b"]);
        // the excluded stereo file doesn't decide the channel count
        assert_eq!(channels, 1);
    }

    #[test]
    fn duplicate_names_count_once() {
        // kick.aif sorts first and is kept; counting the other kicks would tip the vote to 44100
        let dir = temp_assets("dups", &[
            ("kick.aif", aiff_bytes(48000, 1, &[1])),
            ("kick.aiff", aiff_bytes(44100, 1, &[2])),
            ("kick.wav", wav_bytes(44100, 1, &[3])),
            ("snare.wav", wav_bytes(44100, 1, &[4])),
            ("hat.wav", wav_bytes(48000, 1, &[5])),
        ]);
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
        assert!(matches!(load_assets(dir.to_str().unwrap()), Err(DecodeError::Io(_))));
    }

    #[test]
    fn loads_real_assets() {
        let (tracks, rate, channels) = load_assets(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/")).unwrap();
        assert_eq!(sorted_names(&tracks), vec!["kick", "rim1", "shaker", "snare"]);
        assert_eq!(rate, 48000);
        assert_eq!(channels, 2);
    }
}
