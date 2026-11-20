//! WAV recording and export (REC).
//!
//! A recording is a ring (`rtrb`) of interleaved f32 samples plus a writer
//! thread that drains it into WAV files. The producer end goes to whoever
//! generates audio; dropping it (or `RecHandle::request_stop`) ends the take.
//! Nothing here is touched by the audio thread except `Producer::push_*`.

mod dither;
mod naming;
pub mod render;
#[cfg(test)]
mod render_tests;
mod writer;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

pub use dither::Quantizer;
pub use naming::{default_path, next_free, part_path, utc_stamp};

/// Seconds of audio the ring holds by default.
pub const RING_SECONDS: u32 = 4;

/// Default data bytes per file: under 4 GiB with room for the headers in the
/// RIFF size (hound's data counter is a `u32`).
pub const MAX_DATA_BYTES: u32 = u32::MAX - 4096;

/// Sample format of the written file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleKind {
    /// 32-bit IEEE float, written unchanged (overs survive).
    Float32,
    /// 16-bit integer, clamped and dithered.
    Int16,
    /// 24-bit integer, clamped and dithered.
    Int24,
}

/// What a recording looks like.
#[derive(Clone, Copy, Debug)]
pub struct RecSpec {
    /// Frames per second.
    pub sample_rate: u32,
    /// Interleaved channels per frame.
    pub channels: u16,
    /// Sample format of the file.
    pub kind: SampleKind,
}

/// Why a recording couldn't start.
#[derive(Debug)]
pub enum RecError {
    /// The spec is unusable (0 channels or 0 rate).
    BadSpec(&'static str),
    /// The file couldn't be created.
    Create {
        /// The file that failed.
        path: PathBuf,
        /// The underlying error.
        source: hound::Error,
    },
    /// The writer thread couldn't be spawned.
    Spawn(std::io::Error),
}

impl fmt::Display for RecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecError::BadSpec(why) => write!(f, "bad recording format: {why}"),
            RecError::Create { path, source } => {
                write!(f, "can't create {}: {source}", path.display())
            }
            RecError::Spawn(e) => write!(f, "can't start the writer thread: {e}"),
        }
    }
}

impl std::error::Error for RecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RecError::BadSpec(_) => None,
            RecError::Create { source, .. } => Some(source),
            RecError::Spawn(e) => Some(e),
        }
    }
}

/// Tunables for the writer; tests shrink them.
#[derive(Clone, Copy, Debug)]
pub struct WriterOpts {
    /// Ring length in seconds.
    pub ring_seconds: u32,
    /// Data bytes per file before the take continues in the next part.
    pub max_data_bytes: u32,
    /// How long the writer sleeps when the ring is empty.
    pub poll: Duration,
    /// How often the file header is patched, so a crash leaves a valid file.
    pub checkpoint: Duration,
}

impl Default for WriterOpts {
    fn default() -> Self {
        Self {
            ring_seconds: RING_SECONDS,
            max_data_bytes: MAX_DATA_BYTES,
            poll: Duration::from_millis(10),
            checkpoint: Duration::from_secs(1),
        }
    }
}

/// How a take is going.
#[derive(Clone, Debug, PartialEq)]
pub enum RecStatus {
    /// Still writing.
    Running {
        /// Frames written so far.
        frames: u64,
        /// Files opened so far.
        files: u32,
    },
    /// Done; every file is finalised (none, if nothing was recorded).
    Finished {
        /// Frames written.
        frames: u64,
        /// Files written.
        files: u32,
        /// Sample data bytes written.
        bytes: u64,
    },
    /// Stopped by an error.
    Failed(String),
}

/// State shared between the writer thread and the handles.
pub(crate) struct Shared {
    pub(crate) path: PathBuf,
    pub(crate) stop: AtomicBool,
    pub(crate) status: Mutex<RecStatus>,
    pub(crate) notices: Mutex<Vec<String>>,
    pub(crate) done: (Mutex<bool>, Condvar),
}

impl Shared {
    /// Queues a line for `take_notices`.
    pub(crate) fn notify(&self, line: String) {
        self.notices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line);
    }
}

/// The control end of a take. Cheap to clone.
#[derive(Clone)]
pub struct RecHandle(Arc<Shared>);

impl RecHandle {
    /// The first file of the take.
    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// A snapshot of how the take is going.
    pub fn status(&self) -> RecStatus {
        self.0
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// True until the writer has finished (or failed).
    pub fn is_running(&self) -> bool {
        !*self.0.done.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends the take even if the producer is still alive (shutdown fallback).
    pub fn request_stop(&self) {
        self.0.stop.store(true, Ordering::Release);
    }

    /// Waits up to `timeout` for the writer to finish; true if it did.
    pub fn wait(&self, timeout: Duration) -> bool {
        let (lock, cvar) = &self.0.done;
        let guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = cvar
            .wait_timeout_while(guard, timeout, |done| !*done)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }

    /// Lines for the terminal since the last call, such as "rec: continuing
    /// in take-002.wav" or "rec: wrote take.wav (3:02, 69 MB)". The REPL polls
    /// this each pass.
    pub fn take_notices(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .0
                .notices
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

/// Opens the file, allocates the ring and spawns the writer. REPL thread only.
pub fn start(path: &Path, spec: RecSpec) -> Result<(rtrb::Producer<f32>, RecHandle), RecError> {
    start_with(path, spec, WriterOpts::default())
}

/// Like [`start`], with tunables (tests use a small ring, a small size cap and
/// a fast poll).
pub fn start_with(
    path: &Path,
    spec: RecSpec,
    opts: WriterOpts,
) -> Result<(rtrb::Producer<f32>, RecHandle), RecError> {
    if spec.channels == 0 {
        return Err(RecError::BadSpec("0 channels"));
    }
    if spec.sample_rate == 0 {
        return Err(RecError::BadSpec("sample rate 0"));
    }
    // Created here so path and permission errors come back synchronously.
    let file = hound::WavWriter::create(path, writer::wav_spec(spec)).map_err(|source| {
        RecError::Create {
            path: path.to_path_buf(),
            source,
        }
    })?;

    let slots =
        opts.ring_seconds.max(1) as usize * spec.sample_rate as usize * usize::from(spec.channels);
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(slots);
    let shared = Arc::new(Shared {
        path: path.to_path_buf(),
        stop: AtomicBool::new(false),
        status: Mutex::new(RecStatus::Running {
            frames: 0,
            files: 1,
        }),
        notices: Mutex::new(Vec::new()),
        done: (Mutex::new(false), Condvar::new()),
    });
    let thread_shared = Arc::clone(&shared);
    let spawned = std::thread::Builder::new()
        .name("blast-rec".into())
        .spawn(move || writer::run(consumer, &thread_shared, spec, opts, file));
    if let Err(e) = spawned {
        // The empty file was already created; don't leave it behind.
        let _ = std::fs::remove_file(path);
        return Err(RecError::Spawn(e));
    }
    Ok((producer, RecHandle(shared)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("blast-rec-{}-{name}", std::process::id()))
    }

    fn stereo() -> RecSpec {
        RecSpec {
            sample_rate: 48_000,
            channels: 2,
            kind: SampleKind::Float32,
        }
    }

    #[test]
    fn create_error_is_synchronous() {
        let path = temp_path("no-such-dir").join("x.wav");
        let r = start(&path, stereo());
        assert!(matches!(r, Err(RecError::Create { .. })));
    }

    #[test]
    fn bad_spec_rejected() {
        let path = temp_path("bad.wav");
        let no_channels = RecSpec {
            channels: 0,
            ..stereo()
        };
        let no_rate = RecSpec {
            sample_rate: 0,
            ..stereo()
        };
        assert!(matches!(
            start(&path, no_channels),
            Err(RecError::BadSpec(_))
        ));
        assert!(matches!(start(&path, no_rate), Err(RecError::BadSpec(_))));
        assert!(!path.exists());
    }

    #[test]
    fn notices_report_split_and_finish() {
        let path = temp_path("notices.wav");
        let opts = WriterOpts {
            max_data_bytes: 1000,
            poll: Duration::from_millis(2),
            ..WriterOpts::default()
        };
        let (mut p, h) = start_with(&path, stereo(), opts).unwrap();
        p.push_entire_slice(&vec![0.5f32; 400]).unwrap();
        drop(p);
        assert!(h.wait(Duration::from_secs(5)));
        let notices = h.take_notices();
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert!(notices[0].starts_with("rec: continuing in "));
        assert!(notices[0].ends_with("-002.wav"));
        assert!(notices[1].starts_with("rec: wrote "));
        assert!(h.take_notices().is_empty());
        for k in 1..=2 {
            let _ = std::fs::remove_file(part_path(&path, k));
        }
    }
}
