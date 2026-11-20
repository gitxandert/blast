//! The writer thread: drains the ring into WAV files.

use std::fs::File;
use std::io::BufWriter;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Instant;

use hound::{SampleFormat, WavSpec, WavWriter};

use super::dither::Quantizer;
use super::naming::part_path;
use super::{RecSpec, RecStatus, SampleKind, Shared, WriterOpts};
use crate::audio_processing::blast_rand::fast_seed;

pub(super) type Wav = WavWriter<BufWriter<File>>;

/// Frames popped from the ring per pass.
const CHUNK_FRAMES: usize = 4096;

/// The hound spec for a recording.
pub(super) fn wav_spec(spec: RecSpec) -> WavSpec {
    let (bits_per_sample, sample_format) = match spec.kind {
        SampleKind::Float32 => (32, SampleFormat::Float),
        SampleKind::Int16 => (16, SampleFormat::Int),
        SampleKind::Int24 => (24, SampleFormat::Int),
    };
    WavSpec {
        channels: spec.channels,
        sample_rate: spec.sample_rate,
        bits_per_sample,
        sample_format,
    }
}

fn bytes_per_sample(kind: SampleKind) -> u32 {
    match kind {
        SampleKind::Float32 => 4,
        SampleKind::Int16 => 2,
        SampleKind::Int24 => 3,
    }
}

/// The open take: current file, size accounting and format conversion.
struct Take<'a> {
    shared: &'a Shared,
    spec: RecSpec,
    writer: Option<Wav>,
    quantizer: Option<Quantizer>,
    /// Samples (not frames) allowed per file.
    max_samples: u64,
    samples_in_file: u64,
    total_samples: u64,
    files: u32,
}

impl<'a> Take<'a> {
    fn new(shared: &'a Shared, spec: RecSpec, opts: WriterOpts, writer: Wav) -> Self {
        let channels = u64::from(spec.channels);
        let block_align = channels * u64::from(bytes_per_sample(spec.kind));
        // At least one frame per file, or a tiny cap would never advance.
        let frames = (u64::from(opts.max_data_bytes) / block_align).max(1);
        let quantizer = match spec.kind {
            SampleKind::Float32 => None,
            SampleKind::Int16 => Some(Quantizer::new(16, fast_seed())),
            SampleKind::Int24 => Some(Quantizer::new(24, fast_seed())),
        };
        Self {
            shared,
            spec,
            writer: Some(writer),
            quantizer,
            max_samples: frames * channels,
            samples_in_file: 0,
            total_samples: 0,
            files: 1,
        }
    }

    fn frames(&self) -> u64 {
        self.total_samples / u64::from(self.spec.channels)
    }

    fn data_bytes(&self) -> u64 {
        self.total_samples * u64::from(bytes_per_sample(self.spec.kind))
    }

    /// Writes interleaved samples, starting a new part file at the size cap.
    fn write(&mut self, mut samples: &[f32]) -> Result<(), String> {
        while !samples.is_empty() {
            if self.samples_in_file >= self.max_samples {
                self.next_part()?;
            }
            let room = (self.max_samples - self.samples_in_file) as usize;
            let (now, rest) = samples.split_at(room.min(samples.len()));
            let writer = self.writer.as_mut().ok_or("no open file")?;
            let result = match (&mut self.quantizer, self.spec.kind) {
                (None, _) => now.iter().try_for_each(|&x| writer.write_sample(x)),
                (Some(q), SampleKind::Int16) => now
                    .iter()
                    .try_for_each(|&x| writer.write_sample(q.quantize(x) as i16)),
                (Some(q), _) => now
                    .iter()
                    .try_for_each(|&x| writer.write_sample(q.quantize(x))),
            };
            result.map_err(|e| e.to_string())?;
            self.samples_in_file += now.len() as u64;
            self.total_samples += now.len() as u64;
            samples = rest;
        }
        self.publish_running();
        Ok(())
    }

    /// Finalises the full file and opens the next part.
    fn next_part(&mut self) -> Result<(), String> {
        if let Some(full) = self.writer.take() {
            full.finalize().map_err(|e| e.to_string())?;
        }
        self.files += 1;
        let path = part_path(&self.shared.path, self.files);
        let writer = WavWriter::create(&path, wav_spec(self.spec))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.writer = Some(writer);
        self.samples_in_file = 0;
        self.shared
            .notify(format!("rec: continuing in {}", path.display()));
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<(), String> {
        match self.writer.as_mut() {
            Some(w) => w.flush().map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    fn publish_running(&self) {
        let mut status = self
            .shared
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *status = RecStatus::Running {
            frames: self.frames(),
            files: self.files,
        };
    }

    fn finalize(&mut self) -> Result<(), String> {
        match self.writer.take() {
            Some(w) => w.finalize().map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }
}

/// Sets `done` when the thread ends, even by panic, so `wait` can't hang.
struct DoneGuard<'a>(&'a Shared);

impl Drop for DoneGuard<'_> {
    fn drop(&mut self) {
        {
            let mut status = self.0.status.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(*status, RecStatus::Running { .. }) {
                *status = RecStatus::Failed("writer thread died".into());
            }
        }
        *self.0.done.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.0.done.1.notify_all();
    }
}

/// The thread body. Allocates freely: it is not the audio thread.
pub(super) fn run(
    mut consumer: rtrb::Consumer<f32>,
    shared: &Shared,
    spec: RecSpec,
    opts: WriterOpts,
    writer: Wav,
) {
    let _done = DoneGuard(shared);
    let mut take = Take::new(shared, spec, opts, writer);
    let mut scratch = vec![0.0f32; usize::from(spec.channels) * CHUNK_FRAMES];
    let mut last_checkpoint = Instant::now();

    let outcome = (|| -> Result<(), String> {
        loop {
            let n = consumer.pop_partial_slice(&mut scratch).0.len();
            if n > 0 {
                take.write(&scratch[..n])?;
                if last_checkpoint.elapsed() >= opts.checkpoint {
                    take.checkpoint()?;
                    last_checkpoint = Instant::now();
                }
                continue;
            }
            // The Acquire load in `is_abandoned` makes every committed
            // sample visible, so the drain below sees all of them.
            if consumer.is_abandoned() || shared.stop.load(Ordering::Acquire) {
                loop {
                    let n = consumer.pop_partial_slice(&mut scratch).0.len();
                    if n == 0 {
                        return Ok(());
                    }
                    take.write(&scratch[..n])?;
                }
            }
            sleep(opts.poll);
        }
    })();

    let closed = take.finalize();
    let outcome = outcome.and(closed);
    // Dropping the consumer tells the engine (via is_abandoned) to retire its
    // producer.
    drop(consumer);

    let frames = take.frames();
    let status = match outcome {
        Err(msg) => {
            shared.notify(format!("rec: failed: {msg}"));
            RecStatus::Failed(msg)
        }
        Ok(()) if frames == 0 => {
            // Nothing recorded: don't leave an empty file behind.
            let _ = std::fs::remove_file(&shared.path);
            RecStatus::Finished {
                frames: 0,
                files: 0,
                bytes: 0,
            }
        }
        Ok(()) => {
            let bytes = take.data_bytes();
            shared.notify(format!(
                "rec: wrote {} ({}, {})",
                shared.path.display(),
                duration_label(frames, spec.sample_rate),
                size_label(bytes)
            ));
            RecStatus::Finished {
                frames,
                files: take.files,
                bytes,
            }
        }
    };
    *shared.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
}

/// `m:ss` for a frame count.
fn duration_label(frames: u64, rate: u32) -> String {
    let secs = frames / u64::from(rate);
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn size_label(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{} MB", bytes / 1_000_000)
    } else {
        format!("{} KB", bytes / 1000)
    }
}

/// Paths of every part file of a take, for tests and cleanup.
#[cfg(test)]
pub(super) fn part_paths(first: &std::path::Path, files: u32) -> Vec<std::path::PathBuf> {
    (1..=files).map(|k| part_path(first, k)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::start_with;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("blast-rec-{}-{name}", std::process::id()))
    }

    fn opts() -> WriterOpts {
        WriterOpts {
            poll: Duration::from_millis(2),
            ..WriterOpts::default()
        }
    }

    fn spec(channels: u16, kind: SampleKind) -> RecSpec {
        RecSpec {
            sample_rate: 44_100,
            channels,
            kind,
        }
    }

    /// Pushes everything, retrying while the ring is full.
    fn push_all(p: &mut rtrb::Producer<f32>, block: &[f32]) {
        let mut rest = block;
        while !rest.is_empty() {
            let done = p.slots().min(rest.len());
            if done == 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            p.push_entire_slice(&rest[..done]).unwrap();
            rest = &rest[done..];
        }
    }

    fn remove(first: &Path, files: u32) {
        for p in part_paths(first, files.max(1)) {
            let _ = std::fs::remove_file(p);
        }
    }

    fn read_f32(path: &Path) -> Vec<f32> {
        let mut r = hound::WavReader::open(path).unwrap();
        r.samples::<f32>().map(Result::unwrap).collect()
    }

    #[test]
    fn float_roundtrip_keeps_overs() {
        let path = temp_path("float.wav");
        let (mut p, h) = start_with(&path, spec(2, SampleKind::Float32), opts()).unwrap();
        let data: Vec<f32> = vec![
            0.0, 1.7, -1.7, 0.25, 0.5, -0.5, 1.0, -1.0, 0.1, 0.2, 0.3, 0.4,
        ];
        for block in data.chunks(4) {
            p.push_entire_slice(block).unwrap();
        }
        drop(p);
        assert!(h.wait(Duration::from_secs(5)));
        let r = hound::WavReader::open(&path).unwrap();
        let s = r.spec();
        assert_eq!(s.sample_format, SampleFormat::Float);
        assert_eq!(
            (s.channels, s.sample_rate, s.bits_per_sample),
            (2, 44_100, 32)
        );
        drop(r);
        assert_eq!(read_f32(&path), data);
        remove(&path, 1);
    }

    fn int_roundtrip(name: &str, channels: u16, kind: SampleKind, bits: u32) {
        let path = temp_path(name);
        let (mut p, h) = start_with(&path, spec(channels, kind), opts()).unwrap();
        let data: Vec<f32> = (0..channels as usize * 200)
            .map(|i| ((i as f32) * 0.37).sin() * 0.9)
            .collect();
        p.push_entire_slice(&data).unwrap();
        drop(p);
        assert!(h.wait(Duration::from_secs(5)));
        let mut r = hound::WavReader::open(&path).unwrap();
        assert_eq!(r.spec().bits_per_sample as u32, bits);
        assert_eq!(r.spec().channels, channels);
        let got: Vec<i32> = r.samples::<i32>().map(Result::unwrap).collect();
        assert_eq!(got.len(), data.len());
        let scale = (1u32 << (bits - 1)) as f32;
        for (g, x) in got.iter().zip(&data) {
            assert!((*g as f32 - x * scale).abs() <= 1.5, "{g} vs {}", x * scale);
        }
        drop(r);
        remove(&path, 1);
    }

    #[test]
    fn int24_header_and_values() {
        int_roundtrip("i24.wav", 3, SampleKind::Int24, 24);
    }

    #[test]
    fn int16_roundtrip() {
        int_roundtrip("i16.wav", 2, SampleKind::Int16, 16);
    }

    #[test]
    fn flushes_everything_after_producer_drop() {
        let path = temp_path("flush.wav");
        let o = WriterOpts {
            poll: Duration::from_millis(5),
            ..WriterOpts::default()
        };
        let (mut p, h) = start_with(&path, spec(2, SampleKind::Float32), o).unwrap();
        let total = 50_000usize;
        let mut sent = 0usize;
        let mut block = 1usize;
        while sent < total {
            let frames = block.min(total - sent);
            let samples: Vec<f32> = (sent..sent + frames)
                .flat_map(|f| [f as f32, -(f as f32)])
                .collect();
            push_all(&mut p, &samples);
            sent += frames;
            block = block % 997 + 331;
        }
        drop(p);
        assert!(h.wait(Duration::from_secs(10)));
        let got = read_f32(&path);
        assert_eq!(got.len(), total * 2);
        assert_eq!(got[2 * 49_999], 49_999.0);
        assert_eq!(
            h.status(),
            RecStatus::Finished {
                frames: total as u64,
                files: 1,
                bytes: total as u64 * 8
            }
        );
        remove(&path, 1);
    }

    #[test]
    fn split_at_size_limit() {
        let path = temp_path("split.wav");
        let o = WriterOpts {
            max_data_bytes: 1000,
            ..opts()
        };
        let (mut p, h) = start_with(&path, spec(2, SampleKind::Float32), o).unwrap();
        let data: Vec<f32> = (0..600).map(|i| i as f32).collect();
        p.push_entire_slice(&data).unwrap();
        drop(p);
        assert!(h.wait(Duration::from_secs(5)));
        let parts = part_paths(&path, 3);
        let lens: Vec<usize> = parts.iter().map(|f| read_f32(f).len() / 2).collect();
        assert_eq!(lens, [125, 125, 50]);
        assert_eq!(
            parts[1].file_name().unwrap(),
            part_path(&path, 2).file_name().unwrap()
        );
        let all: Vec<f32> = parts.iter().flat_map(|f| read_f32(f)).collect();
        assert_eq!(all, data);
        assert!(!part_path(&path, 4).exists());
        assert!(matches!(
            h.status(),
            RecStatus::Finished {
                frames: 300,
                files: 3,
                ..
            }
        ));
        remove(&path, 3);
    }

    #[test]
    fn stop_flag_ends_take_while_producer_alive() {
        let path = temp_path("stop.wav");
        let (mut p, h) = start_with(&path, spec(1, SampleKind::Float32), opts()).unwrap();
        let data: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        p.push_entire_slice(&data).unwrap();
        h.request_stop();
        assert!(h.wait(Duration::from_secs(5)));
        assert!(!h.is_running());
        assert_eq!(read_f32(&path), data);
        remove(&path, 1);
        drop(p);
    }

    #[test]
    fn empty_take_deletes_file() {
        let path = temp_path("empty.wav");
        let (p, h) = start_with(&path, spec(2, SampleKind::Int16), opts()).unwrap();
        drop(p);
        assert!(h.wait(Duration::from_secs(5)));
        assert!(!path.exists());
        assert!(matches!(h.status(), RecStatus::Finished { frames: 0, .. }));
    }
}
