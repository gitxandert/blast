//! `blast --export script.blast [out.wav]` (PKc): a headless render.
//!
//! It drives the same `Conductor`, `CmdProcessor` and runner as a live
//! session, on one thread, with no device: apply the queued commands and
//! render 64 frames (`Conductor::run_block`, the step the audio callback
//! takes), write them, feed the block's events to the mirror, `pump`, repeat.
//! Nothing here is real time, so a render is faster than real time and,
//! with the session seed set, bit-identical from run to run.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::Quantizer;
use crate::audio_processing::blast_rand::{clear_session_seed, fast_seed, set_session_seed};
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::lang::Code;
use crate::audio_processing::commands::{CmdProcessor, EngineState, cmd_queue};
use crate::audio_processing::engine::events::describe;
use crate::audio_processing::engine::{Conductor, EngineRx, Event};
use crate::cli::Length;
use crate::file_parsing::decode_helpers::DecodeError;
use crate::file_parsing::{load_assets, preferred_config, scan_assets};

/// Frames per block: the size the live callbacks are driven in on most
/// devices, and the unit the shifter's cold-seek budget counts in.
pub const BLOCK_FRAMES: usize = 64;

/// How many segments of a timed script the runner keeps sent. There is no
/// audio thread to keep up with; `Limits::scheduled` still bounds it.
const EXPORT_AHEAD: usize = 1024;

/// Blocks held back before they are written (see the render loop).
const HOLD_BLOCKS: usize = 16;

/// What an export needs to know.
#[derive(Clone, Debug)]
pub struct ExportOpts {
    /// The assets folder (positional argument).
    pub assets_dir: PathBuf,
    /// `--sources`.
    pub sources_dir: PathBuf,
    /// The script, as given (`--export`); resolved like `--script`.
    pub script: PathBuf,
    /// The WAV to write; `None` is the script's name with `.wav` in the
    /// working directory.
    pub out: Option<PathBuf>,
    /// `--length`: a fixed length; `None` renders until the script ends.
    pub length: Option<Length>,
    /// 16, 24 or 32 (float).
    pub bits: u32,
    /// `--rate`; `None` is the assets' preferred rate.
    pub rate: Option<u32>,
    /// `--seed`.
    pub seed: u64,
    /// Print progress and the summary to stderr.
    pub progress: bool,
}

/// What a finished export wrote.
#[derive(Clone, Debug)]
pub struct Summary {
    pub path: PathBuf,
    pub frames: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub wall: Duration,
}

/// Why an export gave up.
#[derive(Debug)]
pub enum ExportErr {
    /// The assets couldn't be scanned or loaded.
    Assets { dir: String, err: DecodeError },
    /// Nothing in the assets folder can be played.
    NoAssets { dir: String },
    /// The script has errors (nothing was written), or E207.
    Script(String),
    /// The WAV couldn't be written.
    Write { path: PathBuf, why: String },
}

impl fmt::Display for ExportErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportErr::Assets { dir, err } => {
                write!(f, "could not load assets from '{dir}': {err}")
            }
            ExportErr::NoAssets { dir } => write!(f, "No playable audio files in '{dir}'"),
            ExportErr::Script(text) => f.write_str(text),
            ExportErr::Write { path, why } => write!(f, "can't write {}: {why}", path.display()),
        }
    }
}

impl std::error::Error for ExportErr {}

/// The default output name: the script's, with `.wav`, in the working
/// directory.
pub fn default_out(script: &Path) -> PathBuf {
    let stem = script
        .file_stem()
        .map_or_else(|| "export".into(), |s| s.to_string_lossy().into_owned());
    PathBuf::from(format!("{stem}.wav"))
}

// the WAV being written, in the format asked for
enum Sink {
    Float(hound::WavWriter<std::io::BufWriter<std::fs::File>>),
    Int16(
        hound::WavWriter<std::io::BufWriter<std::fs::File>>,
        Quantizer,
    ),
    Int24(
        hound::WavWriter<std::io::BufWriter<std::fs::File>>,
        Quantizer,
    ),
}

impl Sink {
    fn create(path: &Path, rate: u32, channels: u16, bits: u32) -> Result<Sink, hound::Error> {
        let (bits_per_sample, sample_format) = match bits {
            16 | 24 => (bits as u16, hound::SampleFormat::Int),
            _ => (32, hound::SampleFormat::Float),
        };
        let w = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels,
                sample_rate: rate,
                bits_per_sample,
                sample_format,
            },
        )?;
        Ok(match bits {
            16 => Sink::Int16(w, Quantizer::new(16, fast_seed())),
            24 => Sink::Int24(w, Quantizer::new(24, fast_seed())),
            _ => Sink::Float(w),
        })
    }

    fn write(&mut self, samples: &[f32]) -> Result<(), hound::Error> {
        match self {
            Sink::Float(w) => samples.iter().try_for_each(|&x| w.write_sample(x)),
            Sink::Int16(w, q) => samples
                .iter()
                .try_for_each(|&x| w.write_sample(q.quantize(x) as i16)),
            Sink::Int24(w, q) => samples
                .iter()
                .try_for_each(|&x| w.write_sample(q.quantize(x))),
        }
    }

    fn finalize(self) -> Result<(), hound::Error> {
        match self {
            Sink::Float(w) => w.finalize(),
            Sink::Int16(w, _) | Sink::Int24(w, _) => w.finalize(),
        }
    }
}

fn e207() -> ExportErr {
    ExportErr::Script(format!(
        "{} export: the script ends at once; add a wait, `end after`, or --length",
        Code::ExportEmpty.as_str()
    ))
}

fn say(opts: &ExportOpts, text: &str) {
    if opts.progress && !text.is_empty() {
        eprintln!("{text}");
    }
}

/// Renders the script to a WAV file.
pub fn render_script(opts: &ExportOpts) -> Result<Summary, ExportErr> {
    let started = Instant::now();
    set_session_seed(opts.seed);
    let done = render(opts, started);
    clear_session_seed();
    done
}

fn render(opts: &ExportOpts, started: Instant) -> Result<Summary, ExportErr> {
    let dir = opts.assets_dir.display().to_string();
    let assets_err = |err| ExportErr::Assets {
        dir: dir.clone(),
        err,
    };
    let assets = scan_assets(&opts.assets_dir).map_err(assets_err)?;
    let Some((want_rate, want_channels)) = preferred_config(&assets) else {
        return Err(ExportErr::NoAssets { dir });
    };
    let rate = opts.rate.unwrap_or(want_rate);
    let channels = want_channels;
    let files = load_assets(&assets, rate, channels).map_err(assets_err)?;
    say(
        opts,
        &format!("export: {} assets · {rate} Hz · {channels} ch", files.len()),
    );

    sample_rate::set(rate);
    let mut engine_state = EngineState::new(files, channels as usize);
    engine_state.set_assets_dir(Some(opts.assets_dir.clone()));
    // the engine shares the mirror's samples: a startup asset lives once
    let mut conductor = Conductor::prepare_shared(channels as usize, engine_state.shared_tracks());
    let mut engine_rx = conductor.take_rx().expect("prepare keeps the REPL ends");
    let (mut cmd_tx, mut cmd_rx) = cmd_queue(4096);

    let mut cp = CmdProcessor::new(engine_state);
    cp.set_sources_dir(opts.sources_dir.clone());
    cp.set_runner_ahead(EXPORT_AHEAD);
    let pushed = cmd_tx.try_push(cp.boot());
    debug_assert!(pushed.is_ok(), "boot command refused by an empty queue");

    // a fixed length counts beats at main's tempo as the render starts
    let fixed: Option<u64> = opts.length.map(|l| {
        let secs = match l {
            Length::Secs(s) => s,
            Length::Beats(b) => b * cp.main_beat_secs().unwrap_or(0.5),
        };
        ((secs * f64::from(rate)).round() as u64).max(1)
    });

    // checked whole, then started: an error leaves nothing written
    let path = cp.resolve_script_arg(&opts.script);
    match cp.run_script_text(&path, &mut cmd_tx) {
        Ok(text) => say(opts, &text),
        Err(e) => return Err(ExportErr::Script(e.to_string())),
    }

    let out_path = opts
        .out
        .clone()
        .unwrap_or_else(|| default_out(&opts.script));
    let write_err = |why: &dyn fmt::Display| ExportErr::Write {
        path: out_path.clone(),
        why: why.to_string(),
    };
    let mut sink: Option<Sink> = None;
    let mut written: u64 = 0;
    let mut quit = false;
    let ch = channels as usize;

    // The mirror learns that the script is over a few blocks after the
    // frame it ended on (events, then `pump`, then the cancel it sends), so
    // the newest blocks are held back until the end is known and the last
    // one is cut to the exact frame.
    let hold = HOLD_BLOCKS * BLOCK_FRAMES * ch;
    let mut held: Vec<f32> = Vec::with_capacity(hold + BLOCK_FRAMES * ch);
    let flush = |held: &mut Vec<f32>, upto: usize, sink: &mut Option<Sink>| {
        if upto == 0 {
            return Ok(0);
        }
        if sink.is_none() {
            *sink = Some(Sink::create(&out_path, rate, channels as u16, opts.bits)?);
        }
        if let Some(s) = sink {
            s.write(&held[..upto])?;
        }
        held.drain(..upto);
        Ok::<u64, hound::Error>((upto / ch) as u64)
    };

    let mut block = vec![0.0f32; BLOCK_FRAMES * ch];
    let mut last_report = Instant::now();
    let end = loop {
        conductor.run_block(&mut cmd_rx, &mut block, ch);
        held.extend_from_slice(&block);
        feed_back(&mut cp, &mut engine_rx, &mut cmd_tx, opts, &mut quit);
        let rendered = conductor.frames_rendered();

        // where the render stops: the fixed length, or the script's end
        // (the last entry retired) and the glides still running then
        let target = match fixed {
            _ if quit => Some(rendered),
            Some(n) => Some(n),
            None if cp.scripts_idle() && cp.engine_state.scheduled_pending() == 0 => {
                Some(conductor.last_retired_at().max(conductor.glides_done_at()))
            }
            None => None,
        };
        if target == Some(0) {
            return Err(e207());
        }
        if let Some(t) = target
            && rendered >= t
        {
            break t;
        }
        if held.len() > hold {
            let upto = held.len() - hold;
            written += flush(&mut held, upto, &mut sink).map_err(|e| write_err(&e))?;
        }
        if opts.progress && last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            let now = rendered as f64 / f64::from(rate);
            match fixed {
                Some(n) => eprintln!("export: {now:.1} s of {:.1} s", n as f64 / f64::from(rate)),
                None => eprintln!("export: {now:.1} s"),
            }
        }
    };
    // what is held, up to the end frame (nothing more if it was passed
    // already)
    let rest = (end.saturating_sub(written) as usize * ch).min(held.len());
    written += flush(&mut held, rest, &mut sink).map_err(|e| write_err(&e))?;
    if let Some(s) = sink {
        s.finalize().map_err(|e| write_err(&e))?;
    }

    // takes with `to=`: stop the running ones and wait for their writers.
    // `finish_buffers` waits on the engine, so it renders on a thread
    // meanwhile (it applies the stops at a block start, then goes on).
    finish_buffers(&mut cp, &mut cmd_tx, &mut engine_rx, conductor, cmd_rx);
    for line in cp.buffer_notices() {
        say(opts, &line);
    }

    let summary = Summary {
        path: out_path,
        frames: written,
        sample_rate: rate,
        channels: channels as u16,
        wall: started.elapsed(),
    };
    say(
        opts,
        &format!(
            "export: wrote {} ({:.1} s, {} frames, {} ch, {} Hz, {}) in {:.1} s",
            summary.path.display(),
            written as f64 / f64::from(rate),
            written,
            channels,
            rate,
            match opts.bits {
                16 => "16-bit",
                24 => "24-bit",
                _ => "32-bit float",
            },
            summary.wall.as_secs_f64()
        ),
    );
    Ok(summary)
}

// what the REPL loop does after a block: the engine's events to the mirror,
// finished takes to their files, `pump`, and the texts to stderr
fn feed_back(
    cp: &mut CmdProcessor,
    rx: &mut EngineRx,
    tx: &mut crate::audio_processing::commands::CmdSender,
    opts: &ExportOpts,
    quit: &mut bool,
) {
    for event in rx.events() {
        match event {
            Event::ScheduleFired {
                seq_no,
                total,
                applied,
            } => cp.engine_state.note_schedule_fired(seq_no, total, applied),
            Event::BufferEnded { slot, take, .. } => cp.engine_state.note_buffer_ended(slot, take),
            Event::Quit => *quit = true,
            other => say(opts, &describe(&other, &cp.engine_state)),
        }
    }
    let mut deferred = Vec::new();
    for (slot, ticket, data, frames) in rx.drain_saves() {
        deferred.extend(cp.note_buffer_save(slot, ticket, data, frames));
    }
    // a replaced track's switch-over; the queue is drained every block
    for cmd in deferred {
        let _ = tx.try_push(cmd);
    }
    for line in cp.pump(tx) {
        say(opts, &line);
    }
    for line in cp.buffer_notices() {
        say(opts, &line);
    }
}

// stops running takes and waits for every writer
fn finish_buffers(
    cp: &mut CmdProcessor,
    tx: &mut crate::audio_processing::commands::CmdSender,
    rx: &mut EngineRx,
    mut conductor: Conductor,
    mut cmd_rx: crate::audio_processing::commands::CmdReceiver,
) {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let channels = conductor.out_channels();
    let renderer = std::thread::Builder::new()
        .name("blast-export-engine".into())
        .spawn(move || {
            let mut block = vec![0.0f32; BLOCK_FRAMES * channels];
            // idle until the stops arrive, so the take ends where the
            // script did; then render on for the saves
            let mut running = false;
            while !flag.load(Ordering::Acquire) {
                if conductor.apply_pending(&mut cmd_rx) > 0 {
                    running = true;
                }
                if running {
                    conductor.render(&mut block, channels);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
    if renderer.is_err() {
        return;
    }
    cp.finish_buffers(tx, rx, Duration::from_secs(30));
    stop.store(true, Ordering::Release);
    if let Ok(handle) = renderer {
        let _ = handle.join();
    }
}
