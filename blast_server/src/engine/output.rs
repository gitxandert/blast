//! Where the engine's audio goes. The `Conductor` lives in a `Renderer`
//! behind a mutex that outlives any one output, so the output can change
//! (one device to another, or to the software clock) while the session
//! keeps playing, and a restarted session can drop a new `Conductor` in.
//!
//! The audio callback only ever `try_lock`s: the lock is contended just
//! while an output is being swapped, and then the callback plays silence
//! for that period rather than wait.
//!
//! Every rendered block is also copied, as stereo, into the tap ring the
//! browser stream reads (`stream.rs`).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use blast::audio_processing::commands::CmdReceiver;
use blast::audio_processing::engine::Conductor;
use blast::audio_processing::runtime::audio;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use serde::Serialize;

/// Most frames rendered in one pass; callbacks asking for more loop.
pub const MAX_FRAMES: usize = 1024;

/// The tap ring holds this much stereo audio.
const TAP_SECONDS: usize = 2;
/// A stream whose callback hasn't run for this long is stuck (seen with
/// PipeWire's ALSA plugin: the node runs, the plugin's poll never wakes).
const STALL: Duration = Duration::from_secs(2);
/// Reopens of a stuck device within `STALL_WINDOW` before giving up on it.
const MAX_REOPENS: u32 = 2;
const STALL_WINDOW: Duration = Duration::from_secs(60);

/// What the callbacks render: the session's engine (absent between
/// sessions) and the tap.
pub struct Renderer {
    core: Option<Core>,
    tap: rtrb::Producer<f32>,
    scratch: Vec<f32>,
}

/// One session's engine end.
pub struct Core {
    pub conductor: Conductor,
    pub commands: CmdReceiver,
    pub channels: usize,
}

impl Renderer {
    /// Renders `frames` (at most `MAX_FRAMES`) at the session's channel
    /// count, clamped, and taps them. `None` when there is no session.
    fn render_block(&mut self, frames: usize, tap_on: bool) -> Option<(&[f32], usize)> {
        let core = self.core.as_mut()?;
        while let Some(cmd) = core.commands.try_pop() {
            core.conductor.apply(cmd);
        }
        let ch = core.channels.max(1);
        let need = frames * ch;
        if self.scratch.len() < need {
            // only after a restart with more channels; never in steady state
            self.scratch.resize(need, 0.0);
        }
        let buf = &mut self.scratch[..need];
        core.conductor.render(buf, ch);
        for x in buf.iter_mut() {
            *x = x.clamp(-1.0, 1.0);
        }
        if tap_on
            && self.tap.slots() >= frames * 2
            && let Ok(chunk) = self.tap.write_chunk_uninit(frames * 2)
        {
            chunk.fill_from_iter(
                buf.chunks_exact(ch).flat_map(
                    |f| {
                        if ch == 1 { [f[0], f[0]] } else { [f[0], f[1]] }
                    },
                ),
            );
        }
        Some((&self.scratch[..need], ch))
    }
}

/// Shared between the output and the session manager.
pub struct Shared {
    pub renderer: Mutex<Renderer>,
    /// Someone is listening in a browser: tap the output.
    pub tap_on: AtomicBool,
    /// The session's sample rate (the tap's too).
    pub rate: AtomicU32,
    /// Frames per device period asked for (`engine.period_frames`).
    pub period: AtomicU32,
    pub stats: Stats,
}

impl Shared {
    /// The shared state and the tap's reading end.
    pub fn new() -> (Arc<Shared>, rtrb::Consumer<f32>) {
        // sized for 192 kHz so a restart at any rate fits
        let (tap, tap_rx) = rtrb::RingBuffer::new(192_000 * 2 * TAP_SECONDS);
        let shared = Arc::new(Shared {
            renderer: Mutex::new(Renderer {
                core: None,
                tap,
                scratch: vec![0.0; MAX_FRAMES * 8],
            }),
            tap_on: AtomicBool::new(false),
            rate: AtomicU32::new(48_000),
            period: AtomicU32::new(128),
            stats: Stats::default(),
        });
        (shared, tap_rx)
    }

    /// Puts a session's engine in place; the old one is returned so it is
    /// dropped outside the lock.
    pub fn swap_core(&self, core: Option<Core>, rate: u32) -> Option<Core> {
        let mut r = self.renderer.lock().unwrap_or_else(|e| e.into_inner());
        self.rate.store(rate, Ordering::Relaxed);
        std::mem::replace(&mut r.core, core)
    }
}

/// Stream health, written by callbacks, read by the session's status.
#[derive(Default)]
pub struct Stats {
    pub xruns: AtomicU32,
    pub period_frames: AtomicU32,
    pub latency_us: AtomicU32,
    /// Device callbacks so far: the watchdog's heartbeat.
    pub callbacks: AtomicU64,
    /// The stream can't continue (device gone).
    pub failed: AtomicBool,
    pub last_error: Mutex<Option<String>>,
}

/// What is playing the engine now.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OutputInfo {
    /// `none` for the software clock, else the cpal id.
    pub id: String,
    pub name: String,
    pub rate: u32,
    pub channels: u16,
    pub format: String,
    pub buffer: String,
}

impl OutputInfo {
    pub fn clock(rate: u32) -> OutputInfo {
        OutputInfo {
            id: "none".into(),
            name: "No local output (browser only)".into(),
            rate,
            channels: 2,
            format: "F32".into(),
            buffer: "-".into(),
        }
    }
}

/// One entry of the device list.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub summary: String,
    /// Can run at the session's rate (else switching needs a restart).
    pub rate_ok: bool,
    pub is_default: bool,
    /// A short list worth showing first: default, sound servers, and one
    /// entry per card.
    pub common: bool,
    /// Opens the card directly, bypassing the sound server.
    pub direct: bool,
}

/// Which devices belong on the short list? `entries` is each device's id and
/// whether it has usable output configs. Usable sound-server devices are
/// always common. The raw card entries (`front:`, `sysdefault:`, `plughw:`)
/// are common only when no sound-server device is usable: alsa-plugins hints
/// `pulse` and `jack` even when no server runs. Unusable devices are never
/// common.
fn mark_common(entries: &[(&str, bool)]) -> Vec<bool> {
    let has_server = entries
        .iter()
        .any(|(id, usable)| *usable && !audio::is_direct(id));
    entries
        .iter()
        .map(|(id, usable)| {
            if !usable {
                return false;
            }
            if !audio::is_direct(id) {
                return true;
            }
            let pcm = id.strip_prefix("alsa:").unwrap_or(id);
            !has_server
                && (pcm.starts_with("sysdefault:")
                    || (pcm.starts_with("plughw:CARD=")
                        && !pcm.contains("CARD=0")
                        && !pcm.contains("CARD=1,"))
                    || pcm.starts_with("front:"))
        })
        .collect()
}

/// Every output device, probed (slow: tens of ms per device).
pub fn list_devices(rate: u32) -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default_id = host
        .default_output_device()
        .and_then(|d| d.id().ok())
        .map(|i| i.to_string());
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    let probed: Vec<(DeviceInfo, bool)> = devices
        .map(|d| {
            let id = d.id().map(|i| i.to_string()).unwrap_or_else(|_| "?".into());
            let name = d
                .description()
                .map(|x| x.name().trim_end_matches([',', ' ']).to_string())
                .unwrap_or_else(|_| "?".into());
            let ranges: Vec<_> = d
                .supported_output_configs()
                .map(|r| r.collect())
                .unwrap_or_default();
            let usable: Vec<_> = ranges
                .iter()
                .filter(|r| {
                    matches!(
                        r.sample_format(),
                        SampleFormat::F32 | SampleFormat::I32 | SampleFormat::I16
                    )
                })
                .collect();
            let rate_ok = usable
                .iter()
                .any(|r| r.min_sample_rate() <= rate && rate <= r.max_sample_rate());
            let summary = if usable.is_empty() {
                "no usable output configs".to_string()
            } else {
                let mut ch: Vec<u16> = usable.iter().map(|r| r.channels()).collect();
                ch.sort_unstable();
                ch.dedup();
                let lo = usable
                    .iter()
                    .map(|r| r.min_sample_rate())
                    .min()
                    .unwrap_or(0);
                let hi = usable
                    .iter()
                    .map(|r| r.max_sample_rate())
                    .max()
                    .unwrap_or(0);
                let rates = match (lo, hi) {
                    (0..=1, u32::MAX) | (0..=1, 384_000..) => "any rate".to_string(),
                    (lo, u32::MAX) => format!("{lo}+ Hz"),
                    (lo, hi) if lo == hi => format!("{lo} Hz"),
                    (lo, hi) => format!("{lo}-{hi} Hz"),
                };
                let chs = match (ch.first(), ch.last()) {
                    (Some(a), Some(b)) if a == b => format!("{a} ch"),
                    (Some(a), Some(b)) => format!("{a}-{b} ch"),
                    _ => String::new(),
                };
                format!("{chs}, {rates}")
            };
            let info = DeviceInfo {
                is_default: default_id.as_deref() == Some(id.as_str()),
                common: false,
                direct: audio::is_direct(&id),
                id,
                name,
                summary,
                rate_ok,
            };
            (info, !usable.is_empty())
        })
        .collect();
    let entries: Vec<(&str, bool)> = probed.iter().map(|(i, u)| (i.id.as_str(), *u)).collect();
    let common = mark_common(&entries);
    probed
        .into_iter()
        .zip(common)
        .map(|((mut info, _), c)| {
            info.common = c;
            info
        })
        .collect()
}

// ---- the output thread ----

enum Msg {
    Open {
        /// `None`: the software clock.
        id: Option<String>,
        reply: mpsc::Sender<Result<OutputInfo, String>>,
    },
}

/// Running output: a cpal stream (not `Send` on every host, so it never
/// leaves the output thread) or the software clock.
enum Current {
    Nothing,
    Stream(cpal::Stream),
    Clock(Arc<AtomicBool>, thread::JoinHandle<()>),
}

impl Current {
    fn stop(&mut self) {
        match std::mem::replace(self, Current::Nothing) {
            Current::Nothing => (),
            Current::Stream(s) => {
                let _ = s.pause();
                drop(s);
            }
            Current::Clock(stop, h) => {
                stop.store(true, Ordering::Relaxed);
                let _ = h.join();
            }
        }
    }
}

/// Handle to the output thread.
#[derive(Clone)]
pub struct Output {
    tx: mpsc::Sender<Msg>,
    pub current: Arc<Mutex<OutputInfo>>,
}

impl Output {
    /// Starts the output thread with nothing playing. `on_fail` hears about
    /// a device that died and what replaced it.
    pub fn start(
        shared: Arc<Shared>,
        fallback_to_none: bool,
        on_fail: impl Fn(String, Option<OutputInfo>) + Send + 'static,
    ) -> Output {
        let (tx, rx) = mpsc::channel::<Msg>();
        let current = Arc::new(Mutex::new(OutputInfo::clock(48_000)));
        let cur = current.clone();
        thread::Builder::new()
            .name("blast-server-output".into())
            .spawn(move || {
                let mut running = Current::Nothing;
                // watchdog: callbacks seen, when they last moved, recent reopens
                let mut beat = (0u64, Instant::now());
                let mut reopens: (u32, Instant) = (0, Instant::now());
                loop {
                    match rx.recv_timeout(Duration::from_millis(250)) {
                        Ok(Msg::Open { id, reply }) => {
                            let prev = cur.lock().unwrap().clone();
                            running.stop();
                            let result = open(&shared, id.as_deref());
                            let result = match result {
                                Ok((c, info)) => {
                                    running = c;
                                    Ok(info)
                                }
                                Err(e) => {
                                    // back to what was playing, else the clock
                                    let back = if prev.id == "none" {
                                        None
                                    } else {
                                        Some(prev.id.as_str())
                                    };
                                    match open(&shared, back).or_else(|_| open(&shared, None)) {
                                        Ok((c, _)) => running = c,
                                        Err(_) => running = Current::Nothing,
                                    }
                                    Err(e)
                                }
                            };
                            if let Ok(info) = &result {
                                *cur.lock().unwrap() = info.clone();
                            }
                            beat = (
                                shared.stats.callbacks.load(Ordering::Relaxed),
                                Instant::now(),
                            );
                            reopens.0 = 0;
                            let _ = reply.send(result);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => (),
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            running.stop();
                            return;
                        }
                    }
                    if matches!(running, Current::Stream(_)) {
                        let n = shared.stats.callbacks.load(Ordering::Relaxed);
                        if n != beat.0 {
                            beat = (n, Instant::now());
                        } else if beat.1.elapsed() >= STALL {
                            let was = cur.lock().unwrap().clone();
                            running.stop();
                            if reopens.1.elapsed() > STALL_WINDOW {
                                reopens = (0, Instant::now());
                            }
                            reopens.0 += 1;
                            let again = if reopens.0 <= MAX_REOPENS {
                                open(&shared, Some(&was.id)).ok()
                            } else {
                                None
                            };
                            let clock = || {
                                if fallback_to_none {
                                    open(&shared, None).ok()
                                } else {
                                    None
                                }
                            };
                            let replacement = again.or_else(clock).map(|(c, info)| {
                                running = c;
                                *cur.lock().unwrap() = info.clone();
                                info
                            });
                            beat = (
                                shared.stats.callbacks.load(Ordering::Relaxed),
                                Instant::now(),
                            );
                            on_fail(
                                format!("output '{}' stopped responding", was.name),
                                replacement,
                            );
                        }
                    }
                    if shared.stats.failed.swap(false, Ordering::Relaxed) {
                        let why = shared
                            .stats
                            .last_error
                            .lock()
                            .unwrap()
                            .clone()
                            .unwrap_or_else(|| "stream failed".into());
                        running.stop();
                        let replacement = if fallback_to_none {
                            open(&shared, None).ok().map(|(c, info)| {
                                running = c;
                                *cur.lock().unwrap() = info.clone();
                                info
                            })
                        } else {
                            None
                        };
                        on_fail(why, replacement);
                    }
                }
            })
            .expect("spawn output thread");
        Output { tx, current }
    }

    /// Plays the engine on `id` (`None` or `"none"`: the software clock);
    /// blocks until it is running or refused.
    pub fn open(&self, id: Option<&str>) -> Result<OutputInfo, String> {
        let id = id.filter(|i| *i != "none").map(str::to_string);
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Msg::Open { id, reply })
            .map_err(|_| "output thread is gone".to_string())?;
        rx.recv().map_err(|_| "output thread is gone".to_string())?
    }

    pub fn info(&self) -> OutputInfo {
        self.current.lock().unwrap().clone()
    }
}

/// The device's config for the session's rate, with the session's channel
/// count preferred. Errors name the rate the device wants instead.
pub fn probe(
    id: Option<&str>,
    rate: u32,
    channels: u32,
    period: u32,
) -> Result<(cpal::Device, audio::Output), String> {
    let name = match id {
        None | Some("default") => None,
        Some(i) => Some(i),
    };
    let device = audio::open_device(name).map_err(|e| e.to_string())?;
    let out = audio::choose_config_with_buffer(&device, rate, channels, period)
        .map_err(|e| e.to_string())?;
    if out.config.sample_rate != rate {
        return Err(format!(
            "'{}' can't run at {rate} Hz (it would run at {} Hz); pick its plughw/sysdefault/pipewire entry, or restart the engine at the device's rate",
            out.device_name, out.config.sample_rate
        ));
    }
    Ok((device, out))
}

fn open(shared: &Arc<Shared>, id: Option<&str>) -> Result<(Current, OutputInfo), String> {
    let rate = shared.rate.load(Ordering::Relaxed);
    let Some(id) = id else {
        return Ok((start_clock(shared.clone(), rate), OutputInfo::clock(rate)));
    };
    let channels = {
        let r = shared.renderer.lock().unwrap_or_else(|e| e.into_inner());
        r.core.as_ref().map_or(2, |c| c.channels as u32)
    };
    let period = shared.period.load(Ordering::Relaxed);
    let (device, out) = probe(Some(id), rate, channels, period)?;
    *shared.stats.last_error.lock().unwrap() = None;
    shared.stats.failed.store(false, Ordering::Relaxed);
    let stream = match out.format {
        SampleFormat::F32 => build::<f32>(&device, &out, shared.clone()),
        SampleFormat::I32 => build::<i32>(&device, &out, shared.clone()),
        SampleFormat::I16 => build::<i16>(&device, &out, shared.clone()),
        f => return Err(format!("unsupported sample format {f:?}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    // `default` (and names) are recorded as the device's cpal id, so the
    // device list can mark it
    let canonical = device
        .id()
        .map(|i| i.to_string())
        .unwrap_or_else(|_| id.to_string());
    let info = OutputInfo {
        id: canonical,
        name: out.device_name.clone(),
        rate: out.config.sample_rate,
        channels: out.config.channels,
        format: format!("{:?}", out.format),
        buffer: out.buffer_text(),
    };
    Ok((Current::Stream(stream), info))
}

fn build<T: SizedSample + FromSample<f32> + Send + 'static>(
    device: &cpal::Device,
    out: &audio::Output,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String> {
    let dev_ch = usize::from(out.config.channels).max(1);
    let errs = shared.clone();
    device
        .build_output_stream::<T, _, _>(
            out.config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                shared.stats.callbacks.fetch_add(1, Ordering::Relaxed);
                let tap_on = shared.tap_on.load(Ordering::Relaxed);
                let Ok(mut r) = shared.renderer.try_lock() else {
                    data.fill(T::EQUILIBRIUM);
                    return;
                };
                for chunk in data.chunks_mut(MAX_FRAMES * dev_ch) {
                    let frames = chunk.len() / dev_ch;
                    match r.render_block(frames, tap_on) {
                        Some((buf, ch)) => {
                            for (o, f) in chunk.chunks_exact_mut(dev_ch).zip(buf.chunks_exact(ch)) {
                                for (j, s) in o.iter_mut().enumerate() {
                                    let x = if ch == 1 {
                                        if j < 2 { f[0] } else { 0.0 }
                                    } else if j < ch {
                                        f[j]
                                    } else {
                                        0.0
                                    };
                                    *s = T::from_sample_(x);
                                }
                            }
                        }
                        None => chunk.fill(T::EQUILIBRIUM),
                    }
                }
                drop(r);
                let st = &shared.stats;
                st.period_frames
                    .store((data.len() / dev_ch) as u32, Ordering::Relaxed);
                let t = info.timestamp();
                if let Some(d) = t.playback.checked_duration_since(t.callback) {
                    st.latency_us.store(
                        u32::try_from(d.as_micros()).unwrap_or(u32::MAX),
                        Ordering::Relaxed,
                    );
                }
            },
            move |e: cpal::Error| {
                let st = &errs.stats;
                match e.kind() {
                    cpal::ErrorKind::Xrun => {
                        st.xruns.fetch_add(1, Ordering::Relaxed);
                    }
                    kind => {
                        if let Ok(mut l) = st.last_error.try_lock() {
                            *l = Some(format!("audio stream error: {kind}"));
                        }
                        if matches!(
                            kind,
                            cpal::ErrorKind::StreamInvalidated
                                | cpal::ErrorKind::DeviceNotAvailable
                        ) {
                            st.failed.store(true, Ordering::Relaxed);
                        }
                    }
                }
            },
            None,
        )
        .map_err(|e| e.to_string())
}

/// Renders in real time with no device: the browser is the only ear.
fn start_clock(shared: Arc<Shared>, rate: u32) -> Current {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let h = thread::Builder::new()
        .name("blast-server-clock".into())
        .spawn(move || {
            let start = Instant::now();
            let mut done: u64 = 0;
            let rate = f64::from(rate);
            while !flag.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(4));
                let due = (start.elapsed().as_secs_f64() * rate) as u64;
                // asleep for over a second (suspend): skip, don't burst
                if due.saturating_sub(done) > rate as u64 {
                    done = due;
                }
                while done < due {
                    let n = ((due - done) as usize).min(MAX_FRAMES);
                    let tap_on = shared.tap_on.load(Ordering::Relaxed);
                    let mut r = shared.renderer.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = r.render_block(n, tap_on);
                    drop(r);
                    done += n as u64;
                }
                shared.stats.period_frames.store(0, Ordering::Relaxed);
            }
        })
        .expect("spawn clock thread");
    Current::Clock(stop, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_prefers_sound_server() {
        let c = mark_common(&[("alsa:default", true), ("alsa:front:CARD=USB,DEV=0", true)]);
        assert_eq!(c, [true, false]);
    }

    #[test]
    fn common_raw_without_server() {
        let c = mark_common(&[
            ("alsa:front:CARD=USB,DEV=0", true),
            ("alsa:plughw:CARD=USB,DEV=0", true),
        ]);
        assert_eq!(c, [true, true]);
    }

    #[test]
    fn common_raw_with_unusable_server() {
        let c = mark_common(&[
            ("alsa:pulse", false),
            ("alsa:jack", false),
            ("alsa:front:CARD=USB,DEV=0", true),
        ]);
        assert_eq!(c, [false, false, true]);
    }
}
