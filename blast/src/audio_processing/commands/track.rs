//! Tracks: the mirror of a loaded track, and the typed cores behind
//! `track "x" from "path"`, `rescan` and `remove "x"` (TRK). Samples are
//! decoded and built here, on the command thread; the engine only inserts or
//! removes the `Arc`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::reprs::{self, TrackId, VoiceId};
use super::voice::VoiceSrc;
use super::wave::WaveFrom;
use super::{CmdProcessor, CmdResult, Command, EngineState};
use crate::audio_processing::blast_time::sample_rate;
use crate::file_parsing::decode_helpers::AudioFile;
use crate::file_parsing::raw::{RawParams, decode_with, parse_raw_name};
use crate::file_parsing::{conform, decode_file};

/// Where a runtime track came from, as written: `state` prints it and a
/// restatement is compared with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackSource {
    /// The path as written in the statement.
    pub written: String,
    /// The file it resolved to.
    pub path: PathBuf,
    /// Raw PCM settings, as given (`format=`, `rate=`, `channels=`).
    pub format: Option<String>,
    pub rate: Option<u32>,
    pub channels: Option<u32>,
}

impl TrackSource {
    /// The raw settings the statement gave.
    pub fn raw_params(&self) -> RawParams {
        RawParams {
            encoding: self.format.as_deref().and_then(|f| f.parse().ok()),
            sample_rate: self.rate,
            channels: self.channels,
        }
    }
}

/// The mirror of a loaded track.
#[derive(Clone)]
pub struct TrackRepr {
    pub(crate) id: TrackId,
    #[allow(dead_code)] // the map is keyed by name; INS will read this
    pub(crate) name: String,
    #[allow(dead_code)] // INS will show these
    pub(crate) channels: u32,
    #[allow(dead_code)]
    pub(crate) frames: usize,
    /// The samples, shared with the engine (a table wave is built from them
    /// on the command thread).
    pub(crate) samples: Arc<[f32]>,
    /// `Some` for a track a statement imported; `None` for one that came from
    /// the assets folder (at startup, or by `rescan`).
    pub(crate) source: Option<TrackSource>,
}

impl TrackRepr {
    pub(super) fn new(id: TrackId, name: &str, channels: u32, samples: Arc<[f32]>) -> Self {
        Self {
            id,
            name: name.to_string(),
            channels,
            frames: samples.len() / (channels as usize).max(1),
            samples,
            source: None,
        }
    }
}

/// Arguments for `Command::AddTrack`: the samples go in the slot `id`; if the
/// slot is taken (a re-import), they replace what is there.
pub struct AddTrackArgs {
    pub id: TrackId,
    pub samples: Arc<[f32]>,
    pub channels: u32,
}

/// Arguments for `Command::DropTrack`.
pub struct DropTrackArgs {
    pub id: TrackId,
}

/// The engine's rate, for conforming imports (48000 before the runtime has
/// set it).
pub(crate) fn engine_rate() -> u32 {
    match sample_rate::get() {
        0 => 48_000,
        n => n,
    }
}

/// A quoted path as a file: `~/` is `home`, an absolute path is used as is,
/// and a relative path starts at the assets folder (the working directory
/// if there is none). Same rule as `source`'s quoted paths.
pub(crate) fn resolve_track_path(
    assets: Option<&Path>,
    written: &str,
    home: Option<OsString>,
) -> PathBuf {
    if let Some(rest) = written.strip_prefix("~/")
        && let Some(home) = home
    {
        return PathBuf::from(home).join(rest);
    }
    match assets {
        Some(dir) => dir.join(written), // an absolute path replaces the folder
        None => PathBuf::from(written),
    }
}

/// True for the extensions that hold headerless PCM (or none).
pub(crate) fn is_raw_path(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(ext.as_str(), "" | "raw" | "pcm")
}

/// Decodes `path` as track `name`, then conforms it to `rate` and
/// `channels`. `raw` settings (any given) make it raw PCM, the file name's
/// own spec filling what they leave out. Err is the reason, for the message.
pub(crate) fn read_track(
    name: &str,
    path: &Path,
    raw: RawParams,
    rate: u32,
    channels: u32,
) -> Result<AudioFile, String> {
    let mut file = if raw == RawParams::default() {
        decode_file(path).map_err(|e| e.to_string())?
    } else {
        let bytes = std::fs::read(path).map_err(|e| format!("I/O error: {e}"))?;
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let spec = raw
            .or(parse_raw_name(file_name).1)
            .complete()
            .map_err(|e| e.to_string())?;
        decode_with(name, &bytes, spec).map_err(|e| e.to_string())?
    };
    // a voice needs at least one whole frame to play
    if file.num_channels == 0 || file.samples.len() < file.num_channels as usize {
        return Err("the file has no audio".to_string());
    }
    file.file_name = name.to_string();
    Ok(conform(file, rate, channels))
}

impl EngineState {
    /// The tracks as the engine holds them, in `TrackId` order, sharing the
    /// mirror's samples (`Conductor::prepare_shared`).
    pub fn shared_tracks(&self) -> Vec<crate::audio_processing::engine::Track> {
        let mut reprs: Vec<&TrackRepr> = self.tracks.values().collect();
        reprs.sort_by_key(|r| r.id.0);
        reprs
            .into_iter()
            .map(|r| crate::audio_processing::engine::Track {
                samples: Arc::clone(&r.samples),
                channels: r.channels,
                frames: r.frames,
            })
            .collect()
    }

    /// The voices (top-level and grouped) playing `track`, in ID order.
    pub(crate) fn voices_using_track(&self, track: TrackId) -> Vec<VoiceId> {
        self.voices
            .values()
            .filter(|v| v.source == VoiceSrc::Track(track))
            .map(|v| v.id)
            .collect()
    }

    /// How many more tracks can be added.
    pub(crate) fn free_track_slots(&self) -> u32 {
        self.ids.tracks.limit() - self.ids.tracks.live()
    }

    /// Adds a new track to the mirror and returns the command that gives the
    /// engine its samples. The capacity is checked before anything changes.
    pub(crate) fn add_track_command(
        &mut self,
        file: AudioFile,
        source: Option<TrackSource>,
    ) -> CmdResult<Command> {
        let id = reprs::alloc(&mut self.ids.tracks, "tracks")?;
        let channels = file.num_channels;
        let samples: Arc<[f32]> = Arc::from(file.samples);
        let mut repr = TrackRepr::new(id, &file.file_name, channels, Arc::clone(&samples));
        repr.source = source;
        self.tracks.insert(file.file_name.clone(), repr);
        Ok(Command::AddTrack(AddTrackArgs {
            id,
            samples,
            channels,
        }))
    }

    /// Removes a track nothing plays from the mirror, frees its ID, and
    /// returns the command that drops it from the engine.
    pub(crate) fn drop_track_command(&mut self, name: &str) -> Option<Command> {
        let repr = self.tracks.remove(name)?;
        self.ids.tracks.free(repr.id);
        // a table made from it keeps playing: it owns its data
        self.forget_table_source(WaveFrom::Track(repr.id));
        Some(Command::DropTrack(DropTrackArgs { id: repr.id }))
    }
}

impl CmdProcessor {
    /// A quoted path as a file (`resolve_track_path`, with `$HOME`).
    pub(crate) fn track_path(&self, written: &str) -> PathBuf {
        self.track_path_with(written, std::env::var_os("HOME"))
    }

    /// `track_path` with the home folder given, so tests needn't touch the
    /// environment.
    pub(crate) fn track_path_with(&self, written: &str, home: Option<OsString>) -> PathBuf {
        resolve_track_path(self.engine_state.assets_dir(), written, home)
    }

    /// Swaps the samples of the existing track `name` for `file`'s, under the
    /// same ID. The commands are `AddTrack` (it replaces, and the engine
    /// retires the old samples), then a `SetTrack` for each voice playing the
    /// track, so each takes the new samples.
    pub(crate) fn reimport_track(
        &mut self,
        name: &str,
        file: AudioFile,
        source: TrackSource,
    ) -> Vec<Command> {
        self.replace_track(name, file, Some(source))
    }

    /// `reimport_track` for samples that are already in memory (a buffer's
    /// take): no file stands behind the track, so `source` is `None`.
    pub(crate) fn replace_track(
        &mut self,
        name: &str,
        file: AudioFile,
        source: Option<TrackSource>,
    ) -> Vec<Command> {
        let Some(repr) = self.engine_state.tracks.get_mut(name) else {
            return Vec::new();
        };
        let id = repr.id;
        let channels = file.num_channels;
        let samples: Arc<[f32]> = Arc::from(file.samples);
        repr.channels = channels;
        repr.frames = samples.len() / (channels as usize).max(1);
        repr.samples = Arc::clone(&samples);
        repr.source = source;
        let mut cmds = vec![Command::AddTrack(AddTrackArgs {
            id,
            samples,
            channels,
        })];
        for voice in self.engine_state.voices_using_track(id) {
            cmds.push(self.set_voice_track(voice, id));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::blast_time::sample_rate;
    use crate::audio_processing::commands::Parsed;
    use crate::audio_processing::commands::lang::Code;
    use crate::audio_processing::commands::testing::{lerr, local, ok, processor, processor_with};
    use crate::audio_processing::engine::arena::Limits;
    use crate::audio_processing::engine::{Conductor, EngineRx, Garbage};
    use crate::audio_processing::test_util::{render_i16, run, rx, setup, track};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("blast-trk-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            path
        }

        fn wav(&self, rel: &str, rate: u32, channels: u16, samples: &[i16]) -> PathBuf {
            self.write(rel, &wav_bytes(rate, channels, samples))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // a 16-bit PCM WAV file
    fn wav_bytes(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let block = channels * 2;
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
        v.extend_from_slice(&block.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&data);
        v
    }

    fn be16(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_be_bytes()).collect()
    }

    // a mono 48 kHz engine with the startup track `kick`, importing from `dir`
    fn rig(dir: &TempDir) -> (CmdProcessor, Conductor) {
        sample_rate::set(48000);
        let (mut cp, c) = setup(1, vec![track("kick", 1000, 16)]);
        cp.engine_state.set_assets_dir(Some(dir.0.clone()));
        (cp, c)
    }

    // the first `n` frames a fresh voice on `track` plays
    fn play(cp: &mut CmdProcessor, c: &mut Conductor, track: &str, n: usize) -> Vec<i16> {
        run(cp, c, &format!("voice probe from \"{track}\" on 1000ms"));
        run(cp, c, "start probe");
        let out = render_i16(c, 1, n);
        run(cp, c, "remove probe");
        out
    }

    fn live_tracks(cp: &CmdProcessor) -> (u32, usize) {
        (
            cp.engine_state.ids.tracks.live(),
            cp.engine_state.tracks.len(),
        )
    }

    #[test]
    fn startup_track_ids_agree() {
        // 32 constant tracks: the mirror and the engine number them alike
        sample_rate::set(48000);
        let names: Vec<String> = (0..32).map(|i| format!("t{i:02}")).collect();
        let files = names
            .iter()
            .enumerate()
            .map(|(i, n)| track(n, (i as i16 + 1) * 100, 4))
            .collect();
        let (mut cp, mut c) = setup(1, files);
        for (i, n) in names.iter().enumerate() {
            assert_eq!(play(&mut cp, &mut c, n, 1), [(i as i16 + 1) * 100], "{n}");
        }
        // inserts agree too: four more, then all again
        let dir = TempDir::new("ids");
        cp.engine_state.set_assets_dir(Some(dir.0.clone()));
        for i in 0..4i16 {
            dir.wav(&format!("n{i}.wav"), 48000, 1, &[(i + 1) * 7; 4]);
            run(
                &mut cp,
                &mut c,
                &format!("track \"n{i}\" from \"n{i}.wav\""),
            );
        }
        for i in 0..4i16 {
            assert_eq!(play(&mut cp, &mut c, &format!("n{i}"), 1), [(i + 1) * 7]);
        }
        for (i, n) in names.iter().enumerate() {
            assert_eq!(play(&mut cp, &mut c, n, 1), [(i as i16 + 1) * 100], "{n}");
        }
    }

    #[test]
    fn track_from_file() {
        let dir = TempDir::new("file");
        let path = dir.wav("hit.wav", 48000, 1, &[100, 200, 300, 400, 500, 600]);
        let (mut cp, mut c) = rig(&dir);
        // an absolute path is used as is
        let line = format!("track \"hit\" from \"{}\"", path.display());
        let cmd = ok(&mut cp, &line);
        assert!(matches!(cmd, Command::AddTrack(_)));
        c.apply(cmd);
        assert_eq!(
            play(&mut cp, &mut c, "hit", 6),
            [100, 200, 300, 400, 500, 600]
        );
        let repr = &cp.engine_state.tracks["hit"];
        assert_eq!((repr.channels, repr.frames), (1, 6));
    }

    #[test]
    fn track_resamples_and_downmixes() {
        let dir = TempDir::new("conform");
        // 24 kHz stereo into a 48 kHz mono engine
        dir.wav("wide.wav", 24000, 2, &[1000; 40]);
        let (mut cp, _c) = rig(&dir);
        ok(&mut cp, "track \"wide\" from \"wide.wav\"");
        let repr = &cp.engine_state.tracks["wide"];
        assert_eq!(repr.channels, 1);
        assert!(repr.frames >= 38, "{}", repr.frames);
    }

    #[test]
    fn track_raw_with_settings() {
        let dir = TempDir::new("raw");
        dir.write("x.raw", &be16(&[10, 20, 30, 40]));
        dir.write("y.s16le.48000.1.raw", &[1, 0, 2, 0, 3, 0]);
        let (mut cp, mut c) = rig(&dir);
        run(
            &mut cp,
            &mut c,
            "track \"x\" from \"x.raw\" format=s16be rate=48000 channels=1",
        );
        assert_eq!(play(&mut cp, &mut c, "x", 4), [10, 20, 30, 40]);
        // the file's own name supplies the settings
        run(&mut cp, &mut c, "track \"y\" from \"y.s16le.48000.1.raw\"");
        assert_eq!(play(&mut cp, &mut c, "y", 3), [1, 2, 3]);
        // short forms
        run(
            &mut cp,
            &mut c,
            "track \"z\" from \"x.raw\" f=s16be r=48000 c=1",
        );
        assert_eq!(play(&mut cp, &mut c, "z", 4), [10, 20, 30, 40]);
        // a bare .raw says what it lacks
        let errs = lerr(&mut cp, "track \"w\" from \"x.raw\" format=s16be");
        assert_eq!(errs[0].code, Code::ImportFailed);
        assert!(errs[0].message.contains("rate"), "{}", errs[0]);
    }

    #[test]
    fn track_settings_on_wav_errors() {
        let dir = TempDir::new("wavset");
        dir.wav("a.wav", 48000, 1, &[1, 2]);
        let (mut cp, _c) = rig(&dir);
        let before = live_tracks(&cp);
        let errs = lerr(&mut cp, "track \"a\" from \"a.wav\" rate=44100");
        assert_eq!(errs[0].code, Code::WrongType);
        assert!(errs[0].message.contains("raw PCM"), "{}", errs[0]);
        assert_eq!(live_tracks(&cp), before);
        // an unknown format word, and a rate out of range
        assert_eq!(
            lerr(&mut cp, "track \"a\" from \"a.raw\" format=mp3")[0].code,
            Code::WrongType
        );
        assert_eq!(
            lerr(&mut cp, "track \"a\" from \"a.raw\" rate=0")[0].code,
            Code::OutOfRange
        );
        assert_eq!(live_tracks(&cp), before);
    }

    #[test]
    fn track_errors_leave_the_mirror_alone() {
        let dir = TempDir::new("errs");
        dir.write("junk.wav", b"not audio at all");
        dir.write("a.xyz", b"?");
        let (mut cp, _c) = rig(&dir);
        let before = live_tracks(&cp);
        for line in [
            "track \"j\" from \"junk.wav\"",
            "track \"m\" from \"missing.wav\"",
            "track \"a\" from \"a.xyz\"",
        ] {
            let errs = lerr(&mut cp, line);
            assert_eq!(errs[0].code, Code::ImportFailed, "{line}: {}", errs[0]);
            assert_eq!(live_tracks(&cp), before, "{line}");
        }
        // a new track must say where it comes from
        assert_eq!(lerr(&mut cp, "track \"n\"")[0].code, Code::MissingItem);
        assert_eq!(live_tracks(&cp), before);
    }

    // Track names are unique among tracks and restating one is a re-import,
    // so a name never clashes: the same file is W101 instead.
    #[test]
    fn track_restated_is_a_reimport_not_a_clash() {
        let dir = TempDir::new("clash");
        dir.wav("a.wav", 48000, 1, &[1, 2]);
        dir.wav("b.wav", 48000, 1, &[3, 4, 5]);
        let (mut cp, _c) = rig(&dir);
        ok(&mut cp, "track \"t\" from \"a.wav\"");
        let id = cp.engine_state.tracks["t"].id;
        let Ok(Parsed::Local(text)) = cp.parse("track \"t\" from \"a.wav\"") else {
            panic!("expected a warning");
        };
        assert!(text.contains("W101"), "{text}");
        ok(&mut cp, "track \"t\" from \"b.wav\"");
        assert_eq!(cp.engine_state.tracks["t"].id, id);
        assert_eq!(cp.engine_state.tracks["t"].frames, 3);
        // a name can't be empty
        assert_eq!(
            lerr(&mut cp, "track \"\" from \"a.wav\"")[0].code,
            Code::WrongType
        );
    }

    #[test]
    fn track_relative_path_from_assets_dir() {
        let dir = TempDir::new("rel");
        dir.wav("sub/hit.wav", 48000, 1, &[5, 6, 7]);
        let (mut cp, mut c) = rig(&dir);
        run(&mut cp, &mut c, "track \"h\" from \"sub/hit.wav\"");
        assert_eq!(play(&mut cp, &mut c, "h", 3), [5, 6, 7]);
    }

    #[test]
    fn track_home_path() {
        let dir = TempDir::new("home");
        let home = TempDir::new("home-dir");
        home.wav("s/bass.wav", 48000, 1, &[9, 8]);
        let (cp, _c) = rig(&dir);
        let home_os = Some(home.0.clone().into_os_string());
        // `~/` is the home folder, through the seam
        let path = cp.track_path_with("~/s/bass.wav", home_os.clone());
        assert_eq!(path, home.0.join("s/bass.wav"));
        // an absolute path is used as is; a relative one starts in the assets
        assert_eq!(
            cp.track_path_with("/etc/x.wav", home_os.clone()),
            PathBuf::from("/etc/x.wav")
        );
        assert_eq!(
            cp.track_path_with("a/b.wav", home_os),
            dir.0.join("a/b.wav")
        );
        // no home: `~/` is just a relative path
        assert_eq!(cp.track_path_with("~/a.wav", None), dir.0.join("~/a.wav"));
        // the file the seam named decodes like any other
        let file = read_track("bass", &path, RawParams::default(), 48000, 1).unwrap();
        assert_eq!(file.samples.len(), 2);
    }

    fn retired(queues: &mut EngineRx) -> Vec<Arc<[f32]>> {
        let mut out = Vec::new();
        while let Ok(g) = queues.garbage.pop() {
            if let Garbage::Track(a) = g {
                out.push(a);
            }
        }
        out
    }

    #[test]
    fn reimport_swaps_voices() {
        let dir = TempDir::new("reimport");
        dir.wav("a.wav", 48000, 1, &[100; 16]);
        dir.wav("b.wav", 48000, 1, &[300; 16]);
        let (mut cp, mut c) = rig(&dir);
        let mut queues = rx(&mut c);
        run(&mut cp, &mut c, "track \"t\" from \"a.wav\"");
        run(&mut cp, &mut c, "voice v from \"t\" on 1000ms");
        run(&mut cp, &mut c, "voice k from \"kick\" on 1000ms");
        run(&mut cp, &mut c, "start v and start k");
        // v plays t (100) and k plays kick (1000)
        assert_eq!(render_i16(&mut c, 1, 2), [1100, 1100]);
        let id = cp.engine_state.tracks["t"].id;

        let Command::Batch(b) = ok(&mut cp, "track \"t\" from \"b.wav\"") else {
            panic!("expected a Batch");
        };
        assert!(matches!(
            b.cmds.as_slice(),
            [Command::AddTrack(_), Command::SetTrack(_)]
        ));
        assert_eq!(cp.engine_state.tracks["t"].id, id);
        for cmd in b.cmds {
            c.apply(cmd);
        }
        // v now plays the new samples (300), k is unmoved
        assert_eq!(render_i16(&mut c, 1, 2), [1300, 1300]);
        // the old samples left through the garbage queue, not freed here:
        // once from the track slot and once from the voice
        let old = retired(&mut queues);
        assert_eq!(old.len(), 2);
        assert!(Arc::ptr_eq(&old[0], &old[1]));
        assert_eq!(old[0][0], 100.0 / 32768.0);
    }

    #[test]
    fn rescan_imports_new_only() {
        let dir = TempDir::new("rescan");
        dir.wav("a.wav", 48000, 1, &[1, 2, 3]);
        let (mut cp, mut c) = rig(&dir);
        dir.wav("drums/b.wav", 48000, 1, &[4, 5]);
        let Command::Batch(b) = ok(&mut cp, "rescan") else {
            panic!("expected a Batch");
        };
        // a and drums/b: two new
        assert!(matches!(
            b.cmds.as_slice(),
            [Command::AddTrack(_), Command::AddTrack(_)]
        ));
        for cmd in b.cmds {
            c.apply(cmd);
        }
        assert_eq!(play(&mut cp, &mut c, "drums/b", 2), [4, 5]);
        assert_eq!(play(&mut cp, &mut c, "a", 3), [1, 2, 3]);
        // nothing new now, and a changed file is not reloaded
        dir.wav("a.wav", 48000, 1, &[9, 9, 9]);
        assert_eq!(local(&mut cp, "rescan"), "rescan: no new files");
        assert_eq!(play(&mut cp, &mut c, "a", 3), [1, 2, 3]);
        // one new file is a single command
        dir.wav("d.wav", 48000, 1, &[8]);
        assert!(matches!(ok(&mut cp, "rescan"), Command::AddTrack(_)));
    }

    #[test]
    fn rescan_nothing_new() {
        let dir = TempDir::new("nonew");
        dir.wav("kick.wav", 48000, 1, &[1]);
        let (mut cp, _c) = rig(&dir);
        assert_eq!(local(&mut cp, "rescan"), "rescan: no new files");
        // without an assets folder it says so
        let mut bare = processor(&["a"]);
        let errs = lerr(&mut bare, "rescan");
        assert_eq!(errs[0].code, Code::ImportFailed);
    }

    #[test]
    fn rescan_capacity_is_all_or_nothing() {
        let dir = TempDir::new("cap");
        dir.wav("a.wav", 48000, 1, &[1]);
        dir.wav("b.wav", 48000, 1, &[2]);
        let limits = Limits {
            tracks: 2,
            ..Limits::default()
        };
        let mut cp = processor_with(&["kick"], limits);
        cp.engine_state.set_assets_dir(Some(dir.0.clone()));
        let before = live_tracks(&cp);
        let errs = lerr(&mut cp, "rescan");
        assert_eq!(errs[0].code, Code::LimitReached);
        assert_eq!(live_tracks(&cp), before);
    }

    #[test]
    fn remove_used_track_refused() {
        let dir = TempDir::new("used");
        dir.wav("a.wav", 48000, 1, &[1, 2]);
        let (mut cp, _c) = rig(&dir);
        ok(&mut cp, "track \"t\" from \"a.wav\"");
        ok(&mut cp, "voice v from \"t\" on 1000ms");
        ok(&mut cp, "voice w from \"t\" on 1000ms");
        ok(&mut cp, "group g on 1000ms { w }");
        let before = live_tracks(&cp);
        let errs = lerr(&mut cp, "remove \"t\"");
        assert_eq!(errs[0].code, Code::InUse);
        assert!(errs[0].message.contains("v, g.w"), "{}", errs[0]);
        assert_eq!(live_tracks(&cp), before);
        // once its voices go, it can
        ok(&mut cp, "remove v and remove g");
        assert!(matches!(ok(&mut cp, "remove \"t\""), Command::DropTrack(_)));
    }

    #[test]
    fn remove_unused_track() {
        let dir = TempDir::new("unused");
        dir.wav("a.wav", 48000, 1, &[1, 2]);
        let (mut cp, mut c) = rig(&dir);
        let mut queues = rx(&mut c);
        run(&mut cp, &mut c, "track \"t\" from \"a.wav\"");
        let id = cp.engine_state.tracks["t"].id;
        assert!(c.tracks.get(id.index()).is_some());
        run(&mut cp, &mut c, "remove \"t\"");
        assert!(!cp.engine_state.tracks.contains_key("t"));
        assert!(c.tracks.get(id.index()).is_none());
        assert_eq!(retired(&mut queues).len(), 1);
        assert_eq!(
            lerr(&mut cp, "voice x from \"t\" on 1000ms")[0].code,
            Code::UnknownName
        );
        assert_eq!(lerr(&mut cp, "remove \"t\"")[0].code, Code::UnknownName);
        // `remove all` leaves tracks alone
        run(&mut cp, &mut c, "voice x from \"kick\" on 1000ms");
        run(&mut cp, &mut c, "remove all");
        assert!(cp.engine_state.tracks.contains_key("kick"));
    }

    #[test]
    fn remove_then_reimport_reuses_id() {
        let dir = TempDir::new("reuse");
        dir.wav("a.wav", 48000, 1, &[1, 2]);
        dir.wav("b.wav", 48000, 1, &[7, 8, 9]);
        let (mut cp, mut c) = rig(&dir);
        run(&mut cp, &mut c, "track \"t\" from \"a.wav\"");
        let id = cp.engine_state.tracks["t"].id;
        run(&mut cp, &mut c, "remove \"t\"");
        run(&mut cp, &mut c, "track \"u\" from \"b.wav\"");
        assert_eq!(cp.engine_state.tracks["u"].id, id);
        assert_eq!(play(&mut cp, &mut c, "u", 3), [7, 8, 9]);
    }

    #[test]
    fn state_prints_runtime_tracks() {
        let dir = TempDir::new("state");
        dir.wav("a.wav", 48000, 1, &[1, 2]);
        dir.write("r.raw", &be16(&[5, 6]));
        let (mut cp, _c) = rig(&dir);
        // a.wav and r.raw are in the folder; `rescan` would take them as
        // assets, but here they are imported by statements
        ok(&mut cp, "track \"t\" from \"a.wav\"");
        ok(
            &mut cp,
            "track \"r\" from \"r.raw\" format=s16be rate=48000 channels=1",
        );
        ok(&mut cp, "voice v from \"t\" on 1000ms");
        dir.wav("new.wav", 48000, 1, &[3]);
        ok(&mut cp, "rescan");
        let text = local(&mut cp, "state");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "track \"r\" from \"r.raw\" format=s16be rate=48000 channels=1"
        );
        assert_eq!(lines[1], "track \"t\" from \"a.wav\"");
        assert!(lines[2].starts_with("voice v from \"t\""), "{text}");
        // the startup track and the rescanned file are assets: not printed
        assert!(!text.contains("kick") && !text.contains("new"), "{text}");

        // feeding it back into a session that has the files changes nothing
        let (mut again, _c2) = rig(&dir);
        for line in &lines {
            ok(&mut again, line);
        }
        assert_eq!(local(&mut again, "state"), text);
    }
}
