//! Audio output through cpal (README §3.6): device choice, config choice,
//! and the stream callback that owns the `Conductor`.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{
    BufferSize, ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig,
    SupportedBufferSize, SupportedStreamConfigRange,
};

use crate::audio_processing::commands::CmdReceiver;
use crate::audio_processing::engine::Conductor;

/// Frames rendered per pass for the formats that need a conversion scratch.
const SCRATCH_FRAMES: usize = 4096;

/// The buffer size asked for when the device allows it (today's ALSA period).
const WANTED_BUFFER: u32 = 128;

/// Formats blast renders to, best first.
const FORMATS: [SampleFormat; 3] = [SampleFormat::F32, SampleFormat::I32, SampleFormat::I16];

/// Stream health, written by the audio callbacks and polled by the REPL
/// side. Atomics only: no queue, no locks.
#[derive(Default)]
pub struct StreamStatus {
    xruns: AtomicU32,
    last_error: AtomicU8,
    period_frames: AtomicU32,
    latency_us: AtomicU32,
}

// every ErrorKind we tell apart; a stored code is the position + 1, 0 is
// "none yet", and anything cpal adds later is stored as `Other`
const ERROR_KINDS: [ErrorKind; 14] = [
    ErrorKind::DeviceBusy,
    ErrorKind::DeviceChanged,
    ErrorKind::DeviceNotAvailable,
    ErrorKind::HostUnavailable,
    ErrorKind::InvalidInput,
    ErrorKind::PermissionDenied,
    ErrorKind::RealtimeDenied,
    ErrorKind::ResourceExhausted,
    ErrorKind::StreamInvalidated,
    ErrorKind::UnsupportedConfig,
    ErrorKind::UnsupportedOperation,
    ErrorKind::Xrun,
    ErrorKind::BackendError,
    ErrorKind::Other,
];

fn error_code(kind: ErrorKind) -> u8 {
    let pos = ERROR_KINDS.iter().position(|k| *k == kind);
    pos.unwrap_or(ERROR_KINDS.len() - 1) as u8 + 1
}

impl StreamStatus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Buffer underruns or overruns reported by the stream so far.
    pub fn xruns(&self) -> u32 {
        self.xruns.load(Ordering::Relaxed)
    }

    /// The kind of the latest stream error other than an xrun, if any.
    pub fn last_error(&self) -> Option<ErrorKind> {
        let code = self.last_error.load(Ordering::Relaxed);
        ERROR_KINDS.get(usize::from(code).checked_sub(1)?).copied()
    }

    /// Frames in the latest data callback.
    pub fn period_frames(&self) -> u32 {
        self.period_frames.load(Ordering::Relaxed)
    }

    /// Latest measured time from callback to playback, in microseconds.
    pub fn latency_us(&self) -> u32 {
        self.latency_us.load(Ordering::Relaxed)
    }

    // runs on cpal's thread; atomics only
    fn on_error(&self, kind: ErrorKind) {
        match kind {
            ErrorKind::Xrun => {
                self.xruns.fetch_add(1, Ordering::Relaxed);
            }
            other => self.last_error.store(error_code(other), Ordering::Relaxed),
        }
    }
}

/// What the device accepted.
#[derive(Clone, Debug)]
pub struct Output {
    pub device_name: String,
    pub config: StreamConfig,
    pub format: SampleFormat,
}

impl Output {
    /// `default` or the frame count, for the startup line.
    pub fn buffer_text(&self) -> String {
        match self.config.buffer_size {
            BufferSize::Fixed(n) => n.to_string(),
            BufferSize::Default => "default".to_string(),
        }
    }
}

#[derive(Debug)]
pub enum AudioErr {
    NoDevice,
    DeviceNotFound {
        name: String,
        available: Vec<String>,
    },
    NoSupportedConfig,
    Cpal(cpal::Error),
}

impl fmt::Display for AudioErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioErr::NoDevice => f.write_str("no audio output device found"),
            AudioErr::DeviceNotFound { name, available } => {
                write!(f, "no output device '{name}'")?;
                if available.is_empty() {
                    write!(f, " (no output devices found)")
                } else {
                    write!(f, "; available: {}", available.join(", "))
                }
            }
            AudioErr::NoSupportedConfig => {
                f.write_str("the device supports no f32, i32 or i16 output")
            }
            AudioErr::Cpal(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AudioErr {}

impl From<cpal::Error> for AudioErr {
    fn from(e: cpal::Error) -> Self {
        AudioErr::Cpal(e)
    }
}

fn device_id(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.to_string())
}

fn device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_string())
}

/// cpal's host names, as they prefix a device id (`coreaudio:...`).
const HOSTS: [&str; 11] = [
    "coreaudio",
    "wasapi",
    "asio",
    "jack",
    "pulseaudio",
    "pipewire",
    "aaudio",
    "oboe",
    "webaudio",
    "emscripten",
    "null",
];

/// Does `id` open an ALSA card directly, bypassing the sound server? Only
/// ALSA has direct devices: an id with another host's prefix is never
/// direct. For ALSA (prefix `alsa:` or none), `default`, `pipewire`, `pulse`
/// and `jack` go through a sound server; every other pcm is direct.
pub fn is_direct(id: &str) -> bool {
    if let Some((host, _)) = id.split_once(':')
        && HOSTS.contains(&host)
    {
        return false;
    }
    let pcm = id.strip_prefix("alsa:").unwrap_or(id);
    !matches!(pcm, "default" | "pipewire" | "pulse" | "jack")
}

/// One line per output device: `<id>  <name>  (<summary>)`, the default
/// device marked `*`. A direct device's summary ends in `, direct`.
pub fn list_devices() -> Result<String, AudioErr> {
    let host = cpal::default_host();
    let default_id = host.default_output_device().and_then(|d| device_id(&d));
    let mut out = String::new();
    for device in host.output_devices()? {
        let id = device_id(&device).unwrap_or_else(|| "?".to_string());
        let name = device_name(&device).unwrap_or_else(|| "?".to_string());
        // ALSA card names can end in ", "
        let name = name.trim_end_matches([',', ' ']);
        let mark = if default_id.as_deref() == Some(id.as_str()) {
            '*'
        } else {
            ' '
        };
        let ranges: Vec<_> = device
            .supported_output_configs()
            .map(|r| r.collect())
            .unwrap_or_default();
        let direct = if is_direct(&id) { ", direct" } else { "" };
        out.push_str(&format!(
            "{mark} {id}  {name}  ({}{direct})\n",
            summarize(&ranges)
        ));
    }
    Ok(out)
}

/// A device's configs in a few words, counting only the formats BLAST can
/// open: `1-8 ch, 8000-192000 Hz, F32 I16`.
fn summarize(ranges: &[SupportedStreamConfigRange]) -> String {
    if ranges.is_empty() {
        return "no output configs".to_string();
    }
    let usable: Vec<_> = ranges
        .iter()
        .filter(|r| format_rank(r.sample_format()).is_some())
        .collect();
    if usable.is_empty() {
        return "no f32, i32 or i16 output".to_string();
    }
    let mut channels: Vec<u16> = usable.iter().map(|r| r.channels()).collect();
    channels.sort_unstable();
    channels.dedup();
    let min = usable
        .iter()
        .map(|r| r.min_sample_rate())
        .min()
        .unwrap_or(0);
    let max = usable
        .iter()
        .map(|r| r.max_sample_rate())
        .max()
        .unwrap_or(0);
    // plugins that resample report 1..u32::MAX
    let rates = match (min, max) {
        (0..=1, u32::MAX) => "any rate".to_string(),
        (lo, u32::MAX) => format!("{lo}+ Hz"),
        (lo, hi) if lo == hi => format!("{lo} Hz"),
        (lo, hi) => format!("{lo}-{hi} Hz"),
    };
    let formats: Vec<String> = FORMATS
        .iter()
        .filter(|f| usable.iter().any(|r| r.sample_format() == **f))
        .map(|f| format!("{f:?}"))
        .collect();
    format!("{} ch, {rates}, {}", runs(&channels), formats.join(" "))
}

/// Sorted, distinct numbers as runs: `1-8`, `2, 6, 8`, `1-2, 6`.
fn runs(nums: &[u16]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < nums.len() {
        let mut j = i;
        while j + 1 < nums.len() && nums[j + 1] == nums[j] + 1 {
            j += 1;
        }
        parts.push(match j > i {
            true => format!("{}-{}", nums[i], nums[j]),
            false => nums[i].to_string(),
        });
        i = j + 1;
    }
    parts.join(", ")
}

/// The default output device, or the one whose `id()` string (`alsa:hw:0,0`)
/// or else description name is exactly `name`.
pub fn open_device(name: Option<&str>) -> Result<cpal::Device, AudioErr> {
    let host = cpal::default_host();
    let Some(name) = name else {
        return host.default_output_device().ok_or(AudioErr::NoDevice);
    };
    let devices: Vec<cpal::Device> = host.output_devices()?.collect();
    let by_id = devices
        .iter()
        .position(|d| device_id(d).as_deref() == Some(name));
    let found = by_id.or_else(|| {
        devices
            .iter()
            .position(|d| device_name(d).as_deref() == Some(name))
    });
    match found {
        Some(i) => Ok(devices.into_iter().nth(i).expect("index from position")),
        None => Err(AudioErr::DeviceNotFound {
            name: name.to_string(),
            available: devices.iter().filter_map(device_id).collect(),
        }),
    }
}

fn format_rank(f: SampleFormat) -> Option<usize> {
    FORMATS.iter().position(|x| *x == f)
}

// how far `rate` is outside the range; 0 when inside
fn rate_distance(r: &SupportedStreamConfigRange, rate: u32) -> u32 {
    rate.clamp(r.min_sample_rate(), r.max_sample_rate())
        .abs_diff(rate)
}

/// Picks a config for (rate, channels) from `ranges` (REQ-030). Formats: F32,
/// then I32, then I16; others are ignored. Channels: exactly `channels` if
/// any range has it, else the smallest count above it, else the largest
/// below. Rate: `rate` if a range holds it (that beats format preference),
/// else the closest bound. Buffer: 128 frames clamped to the range, else the
/// default.
pub fn pick(
    ranges: &[SupportedStreamConfigRange],
    rate: u32,
    channels: u32,
) -> Option<(StreamConfig, SampleFormat)> {
    pick_with_buffer(ranges, rate, channels, WANTED_BUFFER)
}

/// `pick`, asking for `frames` per period instead of 128.
pub fn pick_with_buffer(
    ranges: &[SupportedStreamConfigRange],
    rate: u32,
    channels: u32,
    frames: u32,
) -> Option<(StreamConfig, SampleFormat)> {
    let usable = || {
        ranges
            .iter()
            .filter(|r| format_rank(r.sample_format()).is_some())
    };
    let count = |r: &SupportedStreamConfigRange| u32::from(r.channels());
    let chosen_channels = if usable().any(|r| count(r) == channels) {
        channels
    } else if let Some(above) = usable().map(count).filter(|c| *c > channels).min() {
        above
    } else {
        usable().map(count).filter(|c| *c < channels).max()?
    };

    let best = usable()
        .filter(|r| count(r) == chosen_channels)
        .min_by_key(|r| (rate_distance(r, rate), format_rank(r.sample_format())))?;

    let buffer_size = match *best.buffer_size() {
        SupportedBufferSize::Range { min, max } => BufferSize::Fixed(frames.max(min).min(max)),
        SupportedBufferSize::Unknown => BufferSize::Default,
    };
    let config = StreamConfig {
        channels: best.channels(),
        sample_rate: rate.clamp(best.min_sample_rate(), best.max_sample_rate()),
        buffer_size,
    };
    Some((config, best.sample_format()))
}

/// Picks a supported output config for (rate, channels): see `pick`.
pub fn choose_config(device: &cpal::Device, rate: u32, channels: u32) -> Result<Output, AudioErr> {
    choose_config_with_buffer(device, rate, channels, WANTED_BUFFER)
}

/// `choose_config`, asking for `frames` per period instead of 128.
pub fn choose_config_with_buffer(
    device: &cpal::Device,
    rate: u32,
    channels: u32,
    frames: u32,
) -> Result<Output, AudioErr> {
    let ranges: Vec<_> = device.supported_output_configs()?.collect();
    let (config, format) =
        pick_with_buffer(&ranges, rate, channels, frames).ok_or(AudioErr::NoSupportedConfig)?;
    Ok(Output {
        device_name: device_name(device).unwrap_or_else(|| "unknown".to_string()),
        config,
        format,
    })
}

// a rendered sample in the device's format
fn convert<T: FromSample<f32>>(x: f32) -> T {
    T::from_sample_(x.clamp(-1.0, 1.0))
}

// renders straight into `data`, then clamps in place (the callback goes through
// `run_block`; this is the reference the test compares to)
#[cfg(test)]
fn render_f32(conductor: &mut Conductor, data: &mut [f32], channels: usize) {
    conductor.render(data, channels);
    clamp(data);
}

fn clamp(data: &mut [f32]) {
    for x in data.iter_mut() {
        *x = x.clamp(-1.0, 1.0);
    }
}

// renders in chunks of at most `scratch.len() / channels` frames and
// converts each into `data`
fn render_converted<T: FromSample<f32>>(
    conductor: &mut Conductor,
    data: &mut [T],
    channels: usize,
    scratch: &mut [f32],
) {
    let chunk_samples = scratch.len() / channels * channels;
    for out in data.chunks_mut(chunk_samples) {
        let n = out.len() / channels * channels;
        let buf = &mut scratch[..n];
        conductor.render(buf, channels);
        for (o, x) in out.iter_mut().zip(buf.iter()) {
            *o = convert(*x);
        }
    }
}

// the part of the callback every format shares
fn record(status: &StreamStatus, samples: usize, channels: usize, info: &cpal::OutputCallbackInfo) {
    status
        .period_frames
        .store((samples / channels) as u32, Ordering::Relaxed);
    let t = info.timestamp();
    if let Some(d) = t.playback.checked_duration_since(t.callback) {
        let us = u32::try_from(d.as_micros()).unwrap_or(u32::MAX);
        status.latency_us.store(us, Ordering::Relaxed);
    }
}

/// Builds and starts the stream; the callback owns `conductor` and
/// `commands`.
pub fn start_stream(
    device: &cpal::Device,
    out: &Output,
    conductor: Conductor,
    commands: CmdReceiver,
    status: Arc<StreamStatus>,
) -> Result<cpal::Stream, AudioErr> {
    let stream = match out.format {
        SampleFormat::F32 => build_f32(device, out, conductor, commands, status)?,
        SampleFormat::I32 => build::<i32>(device, out, conductor, commands, status)?,
        SampleFormat::I16 => build::<i16>(device, out, conductor, commands, status)?,
        _ => return Err(AudioErr::NoSupportedConfig),
    };
    cpal::traits::StreamTrait::play(&stream)?;
    Ok(stream)
}

// cpal builds the Error on its own thread and drops it there when this
// closure returns; we add no allocation of our own
fn error_callback(status: Arc<StreamStatus>) -> impl FnMut(cpal::Error) + Send + 'static {
    move |e| status.on_error(e.kind())
}

fn build_f32(
    device: &cpal::Device,
    out: &Output,
    mut conductor: Conductor,
    mut commands: CmdReceiver,
    status: Arc<StreamStatus>,
) -> Result<cpal::Stream, AudioErr> {
    let channels = usize::from(out.config.channels);
    let errors = error_callback(status.clone());
    let stream = device.build_output_stream::<f32, _, _>(
        out.config,
        move |data, info| {
            conductor.run_block(&mut commands, data, channels);
            if channels == 0 {
                return;
            }
            clamp(data);
            record(&status, data.len(), channels, info);
        },
        errors,
        None,
    )?;
    Ok(stream)
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    out: &Output,
    mut conductor: Conductor,
    mut commands: CmdReceiver,
    status: Arc<StreamStatus>,
) -> Result<cpal::Stream, AudioErr> {
    let channels = usize::from(out.config.channels);
    // allocated here, so the callback never allocates
    let mut scratch = vec![0.0f32; SCRATCH_FRAMES * channels];
    let errors = error_callback(status.clone());
    let stream = device.build_output_stream::<T, _, _>(
        out.config,
        move |data, info| {
            conductor.apply_pending(&mut commands);
            if channels == 0 {
                return;
            }
            render_converted(&mut conductor, data, channels, &mut scratch);
            record(&status, data.len(), channels, info);
        },
        errors,
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_ids() {
        for id in ["coreaudio:Built-in", "wasapi:Speakers", "jack:system"] {
            assert!(!is_direct(id), "{id}");
        }
        for pcm in ["default", "pipewire", "pulse", "jack"] {
            assert!(!is_direct(pcm), "{pcm}");
            assert!(!is_direct(&format!("alsa:{pcm}")), "alsa:{pcm}");
        }
        for pcm in [
            "hw:0,0",
            "plughw:CARD=USB,DEV=0",
            "front:CARD=USB,DEV=0",
            "sysdefault:CARD=USB",
            "dmix",
        ] {
            assert!(is_direct(pcm), "{pcm}");
            assert!(is_direct(&format!("alsa:{pcm}")), "alsa:{pcm}");
        }
    }
    use crate::audio_processing::test_util::{pcm, run, setup};
    use crate::file_parsing::decode_helpers::AudioFile;

    fn range(
        channels: u16,
        min: u32,
        max: u32,
        buffer: SupportedBufferSize,
        format: SampleFormat,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(channels, min, max, buffer, format)
    }

    fn simple(
        channels: u16,
        min: u32,
        max: u32,
        format: SampleFormat,
    ) -> SupportedStreamConfigRange {
        range(channels, min, max, SupportedBufferSize::Unknown, format)
    }

    #[test]
    fn summarize_collapses_ranges() {
        let mut ranges = Vec::new();
        for ch in 1..=8 {
            for f in [SampleFormat::I8, SampleFormat::I16, SampleFormat::F32] {
                ranges.push(simple(ch, 8000, 192000, f));
            }
        }
        assert_eq!(summarize(&ranges), "1-8 ch, 8000-192000 Hz, F32 I16");
    }

    #[test]
    fn summarize_edge_cases() {
        assert_eq!(summarize(&[]), "no output configs");
        assert_eq!(
            summarize(&[simple(2, 1, u32::MAX, SampleFormat::U8)]),
            "no f32, i32 or i16 output"
        );
        let ranges = [
            simple(2, 1, u32::MAX, SampleFormat::I32),
            simple(6, 48000, 48000, SampleFormat::I32),
        ];
        assert_eq!(summarize(&ranges), "2, 6 ch, any rate, I32");
        assert_eq!(
            summarize(&[simple(2, 48000, 48000, SampleFormat::F32)]),
            "2 ch, 48000 Hz, F32"
        );
        assert_eq!(
            summarize(&[simple(2, 4000, u32::MAX, SampleFormat::I16)]),
            "2 ch, 4000+ Hz, I16"
        );
        assert_eq!(runs(&[1, 2, 3, 6, 8, 9]), "1-3, 6, 8-9");
    }

    #[test]
    fn pick_exact_match_prefers_f32() {
        let ranges = [
            simple(2, 44100, 48000, SampleFormat::I16),
            simple(2, 44100, 48000, SampleFormat::F32),
            simple(2, 44100, 48000, SampleFormat::I32),
        ];
        let (config, format) = pick(&ranges, 48000, 2).unwrap();
        assert_eq!(format, SampleFormat::F32);
        assert_eq!((config.channels, config.sample_rate), (2, 48000));
        assert_eq!(config.buffer_size, BufferSize::Default);

        let ranges = [
            simple(2, 44100, 48000, SampleFormat::I16),
            simple(2, 44100, 48000, SampleFormat::I32),
        ];
        assert_eq!(pick(&ranges, 48000, 2).unwrap().1, SampleFormat::I32);
    }

    #[test]
    fn pick_rate_beats_format() {
        let ranges = [
            simple(2, 44100, 44100, SampleFormat::F32),
            simple(2, 48000, 48000, SampleFormat::I16),
        ];
        let (config, format) = pick(&ranges, 48000, 2).unwrap();
        assert_eq!(format, SampleFormat::I16);
        assert_eq!(config.sample_rate, 48000);
    }

    #[test]
    fn pick_widens_channels() {
        let ranges = [
            simple(1, 48000, 48000, SampleFormat::F32),
            simple(4, 48000, 48000, SampleFormat::F32),
            simple(6, 48000, 48000, SampleFormat::F32),
        ];
        // none has 2: the smallest above
        assert_eq!(pick(&ranges, 48000, 2).unwrap().0.channels, 4);
        // none has 8: the largest below
        assert_eq!(pick(&ranges, 48000, 8).unwrap().0.channels, 6);
        // exact wins over anything else
        assert_eq!(pick(&ranges, 48000, 1).unwrap().0.channels, 1);
    }

    #[test]
    fn pick_clamps_rate_when_unsupported() {
        let ranges = [
            simple(2, 8000, 44100, SampleFormat::F32),
            simple(2, 96000, 192000, SampleFormat::F32),
        ];
        // 48000 is 3900 above the first and 48000 below the second
        assert_eq!(pick(&ranges, 48000, 2).unwrap().0.sample_rate, 44100);
        assert_eq!(pick(&ranges, 100, 2).unwrap().0.sample_rate, 8000);
    }

    #[test]
    fn pick_fixed_buffer_clamped() {
        let buf = |min, max| SupportedBufferSize::Range { min, max };
        let at = |b| pick(&[range(2, 48000, 48000, b, SampleFormat::F32)], 48000, 2).unwrap();
        assert_eq!(at(buf(16, 4096)).0.buffer_size, BufferSize::Fixed(128));
        assert_eq!(at(buf(256, 4096)).0.buffer_size, BufferSize::Fixed(256));
        assert_eq!(at(buf(16, 64)).0.buffer_size, BufferSize::Fixed(64));
        assert_eq!(
            at(SupportedBufferSize::Unknown).0.buffer_size,
            BufferSize::Default
        );
    }

    #[test]
    fn pick_ignores_unknown_formats() {
        let ranges = [
            simple(2, 48000, 48000, SampleFormat::F64),
            simple(2, 48000, 48000, SampleFormat::U8),
        ];
        assert!(pick(&ranges, 48000, 2).is_none());
        let ranges = [
            simple(2, 48000, 48000, SampleFormat::F64),
            simple(1, 48000, 48000, SampleFormat::I16),
        ];
        let (config, format) = pick(&ranges, 48000, 2).unwrap();
        assert_eq!((config.channels, format), (1, SampleFormat::I16));
    }

    #[test]
    fn convert_clamps_i16() {
        assert_eq!(convert::<i16>(2.0), i16::MAX);
        assert_eq!(convert::<i16>(-2.0), i16::MIN);
        assert_eq!(convert::<i16>(0.0), 0);
    }

    #[test]
    fn convert_clamps_i32() {
        assert_eq!(convert::<i32>(2.0), i32::MAX);
        assert_eq!(convert::<i32>(-2.0), i32::MIN);
        assert_eq!(convert::<i32>(0.0), 0);
    }

    // a stereo Conductor playing two loud, jumpy voices that clip together
    fn playing() -> Conductor {
        crate::audio_processing::blast_time::sample_rate::set(48000);
        let samples: Vec<i16> = (0..24_000)
            .map(|i| ((i * 211) % 60000 - 30000) as i16)
            .collect();
        let files = ["a", "b"]
            .map(|name| AudioFile::new(name, "wav", 48000, 2, 16, pcm(&samples)))
            .to_vec();
        let (mut cp, mut c) = setup(2, files);
        for cmd in [
            "voice a from \"a\" on 1000ms",
            "voice b from \"b\" on 1000ms",
            "b rate=1.5",
            "start a",
            "start b",
        ] {
            run(&mut cp, &mut c, cmd);
        }
        c
    }

    #[test]
    fn render_into_chunks_matches_one_pass() {
        const FRAMES: usize = 10_000;
        let mut reference = vec![0.0f32; FRAMES * 2];
        playing().render(&mut reference, 2);
        assert!(reference.iter().any(|x| x.abs() > 1.0), "should clip");

        let mut f32_out = vec![0.0f32; FRAMES * 2];
        render_f32(&mut playing(), &mut f32_out, 2);
        let clamped: Vec<f32> = reference.iter().map(|x| x.clamp(-1.0, 1.0)).collect();
        assert_eq!(f32_out, clamped);

        let mut scratch = vec![0.0f32; SCRATCH_FRAMES * 2];
        let mut i16_out = vec![0i16; FRAMES * 2];
        render_converted(&mut playing(), &mut i16_out, 2, &mut scratch);
        let want: Vec<i16> = reference.iter().map(|x| convert(*x)).collect();
        assert_eq!(i16_out, want);
    }
}
