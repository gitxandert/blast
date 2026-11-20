mod au;
pub mod decode_helpers;
mod mix;
mod pcm;
pub mod raw;
mod resample;
mod riff_fixup;
mod symphonia;

#[cfg(test)]
mod adapter_tests;
#[cfg(test)]
mod fixture_tests;
#[cfg(test)]
mod test_bytes;

pub use decode_helpers::{AudioFile, DecodeError, DecodeResult};
pub use mix::remap;
pub use resample::resample;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

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
    let bytes = fs::read(path)?;
    // same order as `decode_bytes`, so a `.raw` holding an AU file probes as AU
    if is_au(&ext, &bytes) {
        return au::probe(&bytes);
    }
    if is_raw(&ext) {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        return raw::probe(file_name);
    }
    symphonia::probe(&ext, bytes)
}

/// One playable file found by `scan_assets`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetInfo {
    /// `dir.join(relative path)`, as given to `scan_assets`.
    pub path: PathBuf,
    /// Track name: the relative folders joined with `/`, then the file's
    /// track name (`drums/kick`, REQ-037).
    pub name: String,
    /// Rate, channels and layout from `probe_file`.
    pub info: StreamInfo,
}

// folders nested deeper than this below the assets directory are skipped
const MAX_SCAN_DEPTH: usize = 32;

// REQ-037: "drums/909/hat" for root/drums/909/hat.wav
fn asset_name(root: &Path, path: &Path) -> DecodeResult<String> {
    let invalid = || DecodeError::InvalidData(format!("invalid unicode in '{}'", path.display()));
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut name = String::new();
    for part in relative.parent().into_iter().flat_map(Path::components) {
        if let Component::Normal(part) = part {
            name.push_str(part.to_str().ok_or_else(invalid)?);
            name.push('/');
        }
    }
    if path.file_name().and_then(|n| n.to_str()).is_none() {
        return Err(invalid());
    }
    name.push_str(&track_name(path)?);
    Ok(name)
}

// sorted entries of one folder, hidden ones (leading `.`) left out
fn visible_entries(entries: fs::ReadDir) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(error) => {
                eprintln!("Error: {error}");
                None
            }
        })
        .filter(|entry| !entry.file_name().as_encoded_bytes().starts_with(b"."))
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    paths
}

/// Walks `dir` and its subfolders (REQ-037), probing every supported file
/// without decoding it. Hidden files and folders (leading `.`) are skipped,
/// symlinks are followed, each folder is entered at most once, and folders
/// more than 32 levels deep are skipped. Files that fail to probe, and later
/// files whose name clashes with an earlier one, are skipped with a message
/// on stderr. Sorted by name. Errors only if `dir` itself can't be read.
pub fn scan_assets(dir: &Path) -> DecodeResult<Vec<AssetInfo>> {
    let root_entries = fs::read_dir(dir)?;
    let mut visited = HashSet::new();
    if let Ok(root) = fs::canonicalize(dir) {
        visited.insert(root);
    }
    let mut found = Vec::new();
    let mut warned_depth = false;
    let mut pending = vec![(visible_entries(root_entries), 0)];

    while let Some((paths, depth)) = pending.pop() {
        for path in paths {
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    eprintln!("Error: could not read '{}': {error}", path.display());
                    continue;
                }
            };
            if metadata.is_dir() {
                if depth >= MAX_SCAN_DEPTH {
                    if !warned_depth {
                        eprintln!("Warning: skipping folders deeper than {MAX_SCAN_DEPTH} levels");
                        warned_depth = true;
                    }
                    continue;
                }
                let canonical = match fs::canonicalize(&path) {
                    Ok(canonical) => canonical,
                    Err(error) => {
                        eprintln!("Error: skipping folder '{}': {error}", path.display());
                        continue;
                    }
                };
                // a symlink back to a folder already seen (loops included)
                if !visited.insert(canonical) {
                    continue;
                }
                match fs::read_dir(&path) {
                    Ok(entries) => pending.push((visible_entries(entries), depth + 1)),
                    Err(error) => {
                        eprintln!("Error: skipping folder '{}': {error}", path.display());
                    }
                }
                continue;
            }
            if !is_supported_extension(&extension(&path)) {
                // keeps today's output for flat folders; subfolders stay quiet
                if depth == 0 {
                    eprintln!("Error: unsupported format for '{}'", path.display());
                }
                continue;
            }
            let probed = probe_file(&path).and_then(|info| Ok((info, asset_name(dir, &path)?)));
            match probed {
                Ok((info, _)) if info.sample_rate == 0 || info.num_channels == 0 => {
                    eprintln!("Error: '{}' has no audio, skipping", path.display());
                }
                Ok((info, name)) => found.push(AssetInfo { path, name, info }),
                Err(error) => {
                    eprintln!("Error: could not parse '{}': {error:?}", path.display());
                }
            }
        }
    }

    // stable order, so duplicates resolve the same way every run
    found.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
    let mut assets: Vec<AssetInfo> = Vec::with_capacity(found.len());
    for asset in found {
        if assets.last().is_some_and(|kept| kept.name == asset.name) {
            eprintln!(
                "Error: multiple files with the same name {}, skipping '{}'",
                asset.name,
                asset.path.display()
            );
            continue;
        }
        assets.push(asset);
    }
    Ok(assets)
}

/// The configuration to ask the device for: the most common sample rate
/// (ties go to the higher rate) and the largest channel count. `None` if
/// `assets` is empty.
pub fn preferred_config(assets: &[AssetInfo]) -> Option<(u32, u32)> {
    let mut rate_counts = HashMap::<u32, u32>::new();
    for asset in assets {
        *rate_counts.entry(asset.info.sample_rate).or_default() += 1;
    }
    let (rate, _) = rate_counts
        .into_iter()
        .max_by_key(|&(rate, count)| (count, rate))?;
    let channels = assets.iter().map(|asset| asset.info.num_channels).max()?;
    Some((rate, channels))
}

/// Brings one decoded file to `rate` and at most `channels` channels, the
/// way `load_assets` does: downmix if it has more than `channels` channels,
/// then resample. Narrower files keep their channel count.
pub fn conform(mut file: AudioFile, rate: u32, channels: u32) -> AudioFile {
    // downmix first, so there are fewer channels to resample
    if channels > 0 && file.num_channels > channels {
        file.samples = remap(
            &file.samples,
            file.num_channels as usize,
            file.channel_layout,
            channels as usize,
        );
        file.num_channels = channels;
        file.channel_layout = None;
    }
    if rate > 0 && file.sample_rate > 0 && file.sample_rate != rate {
        file.samples = resample(
            &file.samples,
            file.num_channels as usize,
            file.sample_rate,
            rate,
        );
        file.sample_rate = rate;
    }
    file
}

// decode, rename and conform one asset; Err is the message to print
fn load_one(asset: &AssetInfo, rate: u32, channels: u32) -> Result<AudioFile, String> {
    let path = asset.path.display();
    let mut file = decode_file(&asset.path)
        .map_err(|error| format!("Error: could not parse '{path}': {error:?}"))?;
    // a Voice needs at least one whole frame to play
    if file.num_channels == 0 || file.samples.len() < file.num_channels as usize {
        return Err(format!("Error: '{path}' has no audio, skipping"));
    }
    file.file_name = asset.name.clone();
    Ok(conform(file, rate, channels))
}

// most worker threads `load_assets` uses
const MAX_LOAD_THREADS: usize = 8;

// `load_one` for every asset on `threads` scoped threads; results in input order
fn load_with_threads(
    assets: &[AssetInfo],
    rate: u32,
    channels: u32,
    threads: usize,
) -> Vec<Result<AudioFile, String>> {
    if threads <= 1 {
        return assets
            .iter()
            .map(|asset| load_one(asset, rate, channels))
            .collect();
    }
    let next = AtomicUsize::new(0);
    let worker = || {
        let mut done = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(asset) = assets.get(index) else {
                return done;
            };
            done.push((index, load_one(asset, rate, channels)));
        }
    };
    let mut slots: Vec<Option<Result<AudioFile, String>>> = Vec::new();
    slots.resize_with(assets.len(), || None);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..threads).map(|_| scope.spawn(worker)).collect();
        for handle in handles {
            // a panicking decoder loses only the files that worker had done
            for (index, result) in handle.join().unwrap_or_default() {
                slots[index] = Some(result);
            }
        }
    });
    slots
        .into_iter()
        .zip(assets)
        .map(|(slot, asset)| {
            slot.unwrap_or_else(|| Err(format!("Error: could not load '{}'", asset.path.display())))
        })
        .collect()
}

/// Decodes every asset, resamples it to `rate`, and downmixes files with
/// more than `channels` channels; mono and other narrower files keep their
/// channel count. Loads in parallel; output order is input order. Files that
/// fail or have no audio are skipped with a message on stderr. Errors with
/// `InvalidData` if `rate` or `channels` is 0 or nothing loads.
pub fn load_assets(assets: &[AssetInfo], rate: u32, channels: u32) -> DecodeResult<Vec<AudioFile>> {
    if rate == 0 || channels == 0 {
        return Err(DecodeError::InvalidData(format!(
            "can't load at {rate} Hz with {channels} channels"
        )));
    }
    let threads = thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_LOAD_THREADS)
        .min(assets.len());
    let mut files = Vec::with_capacity(assets.len());
    // printed after the join, in order, so output is deterministic
    for result in load_with_threads(assets, rate, channels, threads) {
        match result {
            Ok(file) => files.push(file),
            Err(message) => eprintln!("{message}"),
        }
    }
    if files.is_empty() {
        return Err(DecodeError::InvalidData(
            "No playable audio files".to_string(),
        ));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_parsing::test_bytes::{aiff_bytes, f32_to_i16, wav_bytes};
    use std::path::PathBuf;

    // what `run_blast` does with a directory: scan, pick the preferred
    // config, load at it; tracks by name, with the rate and channel count
    fn load_assets_dir(dir: &str) -> DecodeResult<(HashMap<String, AudioFile>, u32, u32)> {
        let assets = scan_assets(Path::new(dir))?;
        let Some((rate, channels)) = preferred_config(&assets) else {
            return Err(DecodeError::InvalidData(format!(
                "No playable audio files in '{dir}'"
            )));
        };
        let tracks = load_assets(&assets, rate, channels)?
            .into_iter()
            .map(|file| (file.file_name.clone(), file))
            .collect();
        Ok((tracks, rate, channels))
    }

    // fresh directory filled with (file name, bytes)
    fn temp_assets(name: &str, files: &[(&str, Vec<u8>)]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blast_assets_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (file, bytes) in files {
            let path = dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        dir
    }

    // scan, then remove the directory
    fn scan(dir: &PathBuf) -> Vec<AssetInfo> {
        let assets = scan_assets(dir).unwrap();
        fs::remove_dir_all(dir).unwrap();
        assets
    }

    fn names(assets: &[AssetInfo]) -> Vec<&str> {
        assets.iter().map(|a| a.name.as_str()).collect()
    }

    fn asset(name: &str, rate: u32, channels: u32) -> AssetInfo {
        AssetInfo {
            path: PathBuf::from(format!("{name}.wav")),
            name: name.to_string(),
            info: StreamInfo {
                sample_rate: rate,
                num_channels: channels,
                channel_layout: None,
            },
        }
    }

    // `frames` frames of a ramp, as 16-bit samples
    fn ramp(frames: usize, channels: usize) -> Vec<i16> {
        (0..frames * channels)
            .map(|i| (i % 1000) as i16 * 10)
            .collect()
    }

    fn load(dir: &PathBuf) -> DecodeResult<(HashMap<String, AudioFile>, u32, u32)> {
        let result = load_assets_dir(&format!("{}/", dir.to_str().unwrap()));
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
    fn majority_rate_wins_and_others_are_resampled() {
        let dir = temp_assets(
            "rates",
            &[
                ("a.wav", wav_bytes(44100, 1, &[1])),
                ("b.wav", wav_bytes(44100, 1, &[2])),
                ("c.wav", wav_bytes(48000, 2, &ramp(4800, 2))),
            ],
        );
        let (tracks, rate, channels) = load(&dir).unwrap();
        assert_eq!(rate, 44100);
        assert_eq!(sorted_names(&tracks), vec!["a", "b", "c"]);
        assert_eq!(tracks["c"].sample_rate, 44100);
        assert_eq!(tracks["c"].num_channels, 2);
        assert_eq!(tracks["c"].samples.len(), 2 * 4410);
        // every file counts now, so the stereo one decides the channel count
        assert_eq!(channels, 2);
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
        // snare is off-rate, so it's resampled (it used to be dropped)
        assert_eq!(sorted_names(&tracks), vec!["hat", "kick", "snare"]);
        assert_eq!(tracks["snare"].sample_rate, 48000);
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
            load_assets_dir(dir.to_str().unwrap()),
            Err(DecodeError::Io(_))
        ));
    }

    // every non-hidden file below `dir` with a supported extension, walked
    // independently of `scan_assets`
    fn expected_asset_names(dir: &Path) -> Vec<String> {
        let mut names = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(folder) = stack.pop() {
            for entry in fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path
                    .file_name()
                    .unwrap()
                    .as_encoded_bytes()
                    .starts_with(b".")
                {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if is_supported_extension(&ext) {
                    names.push(asset_name(dir, &path).unwrap());
                }
            }
        }
        names.sort();
        names
    }

    // the names in `a` that `b` lacks
    fn missing_from(a: &[String], b: &[String]) -> Vec<String> {
        a.iter().filter(|n| !b.contains(n)).cloned().collect()
    }

    #[test]
    fn loads_real_assets() {
        // assets/ is gitignored, so skip on checkouts without it
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/");
        if !Path::new(dir).is_dir() {
            return;
        }
        let expected = expected_asset_names(Path::new(dir));
        let (tracks, rate, channels) = load_assets_dir(dir).unwrap();
        let loaded: Vec<String> = sorted_names(&tracks)
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            loaded.len(),
            expected.len(),
            "track count differs; not loaded: {:?}; unexpected: {:?}",
            missing_from(&expected, &loaded),
            missing_from(&loaded, &expected)
        );
        assert_eq!(loaded, expected);
        for core in ["kick", "rim1", "shaker", "snare"] {
            assert!(
                loaded.iter().any(|n| n == core),
                "core asset {core} missing"
            );
        }
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
    fn scan_recurses_and_names_by_path() {
        let dir = temp_assets(
            "recurse",
            &[
                ("kick.wav", wav_bytes(48000, 2, &[1, 2])),
                ("drums/snare.wav", wav_bytes(44100, 1, &[3])),
                ("drums/909/hat.aif", aiff_bytes(22050, 2, &[4, 5])),
                ("drums/notes.txt", b"not audio".to_vec()),
            ],
        );
        let assets = scan(&dir);
        assert_eq!(names(&assets), vec!["drums/909/hat", "drums/snare", "kick"]);
        let config: Vec<(u32, u32)> = assets
            .iter()
            .map(|a| (a.info.sample_rate, a.info.num_channels))
            .collect();
        assert_eq!(config, vec![(22050, 2), (44100, 1), (48000, 2)]);
        assert!(assets[0].path.ends_with("drums/909/hat.aif"));
    }

    #[test]
    fn scan_skips_hidden() {
        let dir = temp_assets(
            "hidden",
            &[
                ("kick.wav", wav_bytes(48000, 1, &[1])),
                (".fixtures/a.wav", wav_bytes(48000, 1, &[1])),
                (".git/x/b.wav", wav_bytes(48000, 1, &[1])),
                (".hidden.wav", wav_bytes(48000, 1, &[1])),
            ],
        );
        assert_eq!(names(&scan(&dir)), vec!["kick"]);
    }

    #[test]
    fn scan_dedupes_same_folder_stems() {
        let dir = temp_assets(
            "dedupe",
            &[
                ("drums/kick.wav", wav_bytes(44100, 1, &[1])),
                ("drums/kick.aif", aiff_bytes(48000, 1, &[2])),
            ],
        );
        let assets = scan(&dir);
        assert_eq!(names(&assets), vec!["drums/kick"]);
        assert!(assets[0].path.ends_with("kick.aif"));
    }

    #[test]
    fn scan_depth_cap() {
        // a file in every folder from depth 0 (the root) to depth 34
        let mut files = Vec::new();
        let mut folder = String::new();
        for depth in 0..=34 {
            files.push((format!("{folder}f{depth}.wav"), wav_bytes(48000, 1, &[1])));
            folder.push_str("d/");
        }
        let files: Vec<(&str, Vec<u8>)> =
            files.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
        let dir = temp_assets("depth", &files);
        let assets = scan(&dir);
        assert_eq!(assets.len(), 33);
        let deepest = assets.iter().map(|a| a.name.matches('/').count()).max();
        assert_eq!(deepest, Some(32));
    }

    #[cfg(unix)]
    #[test]
    fn scan_visits_each_folder_once() {
        let dir = temp_assets("once", &[("drums/kick.wav", wav_bytes(48000, 1, &[1]))]);
        std::os::unix::fs::symlink("../drums", dir.join("drums/again")).unwrap();
        assert_eq!(names(&scan(&dir)), vec!["drums/kick"]);
    }

    #[cfg(unix)]
    #[test]
    fn scan_symlink_loop_terminates() {
        let dir = temp_assets("loop", &[("a/kick.wav", wav_bytes(48000, 1, &[1]))]);
        std::os::unix::fs::symlink("..", dir.join("a/up")).unwrap();
        std::os::unix::fs::symlink("../b", dir.join("a/b")).unwrap();
        std::os::unix::fs::symlink("a", dir.join("b")).unwrap();
        let assets = scan(&dir);
        assert_eq!(assets.len(), 1);
        assert!(assets[0].name.ends_with("kick"));
    }

    #[test]
    fn preferred_config_votes() {
        assert_eq!(preferred_config(&[]), None);
        let tie = [asset("a", 44100, 1), asset("b", 48000, 1)];
        assert_eq!(preferred_config(&tie), Some((48000, 1)));
        let majority = [
            asset("a", 44100, 1),
            asset("b", 44100, 2),
            asset("c", 96000, 6),
        ];
        assert_eq!(preferred_config(&majority), Some((44100, 6)));
    }

    #[test]
    fn load_downmixes_wide_files_only() {
        let dir = temp_assets(
            "downmix",
            &[
                ("wide.wav", wav_bytes(48000, 6, &ramp(10, 6))),
                ("mono.wav", wav_bytes(48000, 1, &ramp(10, 1))),
            ],
        );
        let assets = scan_assets(&dir).unwrap();
        let files = load_assets(&assets, 48000, 2).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        let shape: Vec<(&str, u32, usize)> = files
            .iter()
            .map(|f| (f.file_name.as_str(), f.num_channels, f.samples.len()))
            .collect();
        assert_eq!(shape, vec![("mono", 1, 10), ("wide", 2, 20)]);
    }

    #[test]
    fn load_keeps_input_order() {
        let files: Vec<(String, Vec<u8>)> = (0..12)
            .map(|i| (format!("t{i:02}.wav"), wav_bytes(48000, 1, &[i])))
            .collect();
        let files: Vec<(&str, Vec<u8>)> =
            files.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
        let dir = temp_assets("order", &files);
        let mut assets = scan_assets(&dir).unwrap();
        // an order scan_assets wouldn't produce
        assets.reverse();
        assets.swap(2, 7);
        let expected: Vec<&str> = names(&assets);
        for threads in [1, 3] {
            let loaded = load_with_threads(&assets, 48000, 1, threads);
            let got: Vec<&str> = loaded
                .iter()
                .map(|r| r.as_ref().unwrap().file_name.as_str())
                .collect();
            assert_eq!(got, expected, "{threads} threads");
        }
        let got: Vec<String> = load_assets(&assets, 48000, 1)
            .unwrap()
            .into_iter()
            .map(|f| f.file_name)
            .collect();
        assert_eq!(got, expected);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conform_downmixes_then_resamples() {
        let wide = AudioFile::new("wide", "wav", 24000, 6, 16, vec![0.25; 6 * 100]);
        let wide = conform(wide, 48000, 2);
        assert_eq!((wide.sample_rate, wide.num_channels), (48000, 2));
        assert_eq!(wide.samples.len(), 2 * 200);
        let mono = AudioFile::new("mono", "wav", 24000, 1, 16, vec![0.25; 100]);
        let mono = conform(mono, 48000, 2);
        assert_eq!((mono.sample_rate, mono.num_channels), (48000, 1));
        assert_eq!(mono.samples.len(), 200);
    }

    #[test]
    fn load_rejects_zero_config() {
        let assets = [asset("a", 48000, 1)];
        assert!(matches!(
            load_assets(&assets, 0, 2),
            Err(DecodeError::InvalidData(_))
        ));
        assert!(matches!(
            load_assets(&assets, 48000, 0),
            Err(DecodeError::InvalidData(_))
        ));
    }

    #[test]
    fn load_raw_by_name() {
        let bytes: Vec<u8> = [1i16, 2, 3, 4]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let dir = temp_assets("raw", &[("kick.s16le.48000.2.raw", bytes)]);
        let (tracks, rate, channels) = load(&dir).unwrap();
        assert_eq!(sorted_names(&tracks), vec!["kick"]);
        assert_eq!((rate, channels), (48000, 2));
        assert_eq!(f32_to_i16(&tracks["kick"].samples), vec![1, 2, 3, 4]);
    }

    // cargo test --release -- --ignored load_speed --nocapture
    #[test]
    #[ignore]
    fn load_speed_fixture_dir() {
        let minute: Vec<i16> = (0..60 * 44100)
            .flat_map(|n| {
                let v =
                    (8000.0 * (std::f64::consts::TAU * 440.0 * n as f64 / 44100.0).sin()) as i16;
                [v, v]
            })
            .collect();
        let bytes = wav_bytes(44100, 2, &minute);
        let files: Vec<(String, Vec<u8>)> = (0..16)
            .map(|i| (format!("s{i:02}.wav"), bytes.clone()))
            .collect();
        let files: Vec<(&str, Vec<u8>)> =
            files.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
        let dir = temp_assets("speed", &files);
        let start = std::time::Instant::now();
        let assets = scan_assets(&dir).unwrap();
        println!("scan: {:?}", start.elapsed());
        let threads = thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(MAX_LOAD_THREADS);
        for threads in [1, threads] {
            let start = std::time::Instant::now();
            let loaded = load_with_threads(&assets, 48000, 2, threads);
            assert!(loaded.iter().all(|r| r.is_ok()));
            println!("load, {threads} thread(s): {:?}", start.elapsed());
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn probe_matches_decode_for_snd_magic_raw() {
        // a valid AU file (16-bit, 22050 Hz, stereo) under a raw name that says otherwise
        let mut au = b".snd".to_vec();
        for word in [24u32, 8, 3, 22050, 2] {
            au.extend_from_slice(&word.to_be_bytes());
        }
        au.extend_from_slice(&[0, 1, 0, 2, 0, 3, 0, 4]);
        let dir = temp_assets("snd_raw", &[("x.s16le.8000.1.raw", au)]);
        let path = dir.join("x.s16le.8000.1.raw");
        let info = probe_file(&path).unwrap();
        let file = decode_file(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!((info.sample_rate, info.num_channels), (22050, 2));
        assert_eq!((file.sample_rate, file.num_channels), (22050, 2));
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
