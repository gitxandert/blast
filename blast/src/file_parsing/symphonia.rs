//! Decoding through symphonia (README §3.2).

use super::StreamInfo;
use super::decode_helpers::{AudioFile, DecodeError, DecodeResult};
use super::riff_fixup;
use std::io::{Cursor, ErrorKind};
use symphonia::core::audio::Channels;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, Track, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::TimeBase;

/// Upper bound on decoded samples (all channels), so a hostile header or a
/// stream of corrupt packets can't force an unbounded allocation.
const MAX_SAMPLES: usize = 1 << 28;

/// More than this share of packets failing makes the whole file an error.
const MAX_FAILED_PERCENT: usize = 10;

/// What the default audio track's headers say.
struct TrackInfo {
    id: u32,
    params: AudioCodecParameters,
    stream: StreamInfo,
    bits_per_sample: u32,
    num_frames: Option<u64>,
    time_base: Option<TimeBase>,
}

fn open(ext: &str, bytes: Vec<u8>) -> DecodeResult<Box<dyn FormatReader>> {
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(ext);
    let reader = symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;
    Ok(reader)
}

fn track_info(track: &Track) -> DecodeResult<TrackInfo> {
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| DecodeError::UnsupportedFormat("no audio track".to_string()))?;
    let sample_rate = params
        .sample_rate
        .filter(|&r| r > 0)
        .ok_or_else(|| DecodeError::InvalidData("no sample rate".to_string()))?;
    let num_channels = params
        .channels
        .as_ref()
        .map(Channels::count)
        .filter(|&n| n > 0)
        .ok_or_else(|| DecodeError::InvalidData("no channels".to_string()))?;
    let channel_layout = match &params.channels {
        // the first 18 position bits are the WAVE channel mask
        Some(Channels::Positioned(p)) => Some((p.bits() & 0x3FFFF) as u32),
        _ => None,
    };
    Ok(TrackInfo {
        id: track.id,
        params: params.clone(),
        stream: StreamInfo {
            sample_rate,
            num_channels: u32::try_from(num_channels)
                .map_err(|_| DecodeError::InvalidData("too many channels".to_string()))?,
            channel_layout,
        },
        bits_per_sample: params
            .bits_per_sample
            .or(params.bits_per_coded_sample)
            .unwrap_or(0),
        num_frames: track.num_frames,
        time_base: track.time_base,
    })
}

fn default_track(reader: &dyn FormatReader) -> DecodeResult<TrackInfo> {
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or_else(|| DecodeError::UnsupportedFormat("no audio track".to_string()))?;
    track_info(track)
}

// `ticks` of `time_base` as frames at `rate`; None on overflow or without a time base
fn ticks_to_frames(ticks: u64, time_base: Option<TimeBase>, rate: u32) -> Option<u64> {
    let tb = time_base?;
    let scaled = ticks
        .checked_mul(u64::from(tb.numer.get()))?
        .checked_mul(u64::from(rate))?;
    Some(scaled / u64::from(tb.denom.get()))
}

/// Frames of silence for a timestamp gap the demuxer left (a packet it
/// dropped, e.g. a FLAC frame with a bad CRC), capped at one second.
fn gap_frames(gap: u64, time_base: Option<TimeBase>, rate: u32) -> usize {
    // rate ≤ u32::MAX, so this fits in usize
    ticks_to_frames(gap, time_base, rate)
        .unwrap_or(0)
        .min(u64::from(rate)) as usize
}

/// Frames of silence to stand in for a packet that failed to decode
/// (REQ-034). `dur` is in `time_base` units and comes from the file, so the
/// result is always capped.
fn fill_frames(
    last_frames: Option<usize>,
    dur: u64,
    time_base: Option<TimeBase>,
    rate: u32,
    max_per_packet: Option<u64>,
) -> usize {
    let frames = match last_frames {
        Some(n) => n as u64,
        None => ticks_to_frames(dur, time_base, rate).unwrap_or(0),
    };
    let cap = max_per_packet.map_or(u64::from(rate), |m| m.min(u64::from(rate)));
    // cap ≤ u32::MAX, so this fits in usize
    frames.min(cap) as usize
}

fn push(out: &mut Vec<f32>, samples: &[f32]) -> DecodeResult<()> {
    if out.len().saturating_add(samples.len()) > MAX_SAMPLES {
        return Err(DecodeError::InvalidData("file too long".to_string()));
    }
    out.extend_from_slice(samples);
    Ok(())
}

fn push_silence(out: &mut Vec<f32>, samples: usize) -> DecodeResult<()> {
    if out.len().saturating_add(samples) > MAX_SAMPLES {
        return Err(DecodeError::InvalidData("file too long".to_string()));
    }
    out.resize(out.len() + samples, 0.0);
    Ok(())
}

/// Decodes a whole file from memory. `ext` is the lowercase extension, a hint
/// for the probe.
pub(super) fn decode(name: &str, ext: &str, bytes: Vec<u8>) -> DecodeResult<AudioFile> {
    let (bytes, frame_limit) = riff_fixup::repair(bytes);
    let mut reader = open(ext, bytes)?;
    let info = default_track(reader.as_ref())?;
    let channels = info.stream.num_channels as usize;
    let rate = info.stream.sample_rate;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&info.params, &AudioDecoderOptions::default())?;

    let reserve = info
        .num_frames
        .and_then(|f| usize::try_from(f).ok())
        .and_then(|f| f.checked_mul(channels))
        .filter(|&n| n <= MAX_SAMPLES)
        .unwrap_or(0);
    let mut out = Vec::with_capacity(reserve);
    let mut scratch: Vec<f32> = Vec::new();
    let mut last_frames = None;
    let (mut packets, mut failed) = (0usize, 0usize);
    // where the next packet should start, in time-base units
    let mut next_pts: Option<i64> = None;

    loop {
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            // a truncated last packet
            Err(Error::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id != info.id {
            continue;
        }
        packets += 1;

        // a packet the demuxer dropped leaves a gap: keep the timing (REQ-034)
        let pts = packet.pts.get();
        if let Some(gap) = next_pts
            .and_then(|expected| pts.checked_sub(expected))
            .filter(|&gap| gap > 0)
        {
            let frames = gap_frames(gap as u64, info.time_base, rate);
            if frames > 0 {
                packets += 1;
                failed += 1;
                push_silence(&mut out, frames.saturating_mul(channels))?;
            }
        }
        // symphonia advances pts by the untrimmed duration
        let span = packet
            .dur
            .get()
            .saturating_add(packet.trim_start.get())
            .saturating_add(packet.trim_end.get());
        next_pts = i64::try_from(span)
            .ok()
            .and_then(|span| pts.checked_add(span));

        match decoder.decode(&packet) {
            Ok(buf) => {
                if buf.spec().channels().count() != channels {
                    return Err(DecodeError::UnsupportedFormat(
                        "channel count changes mid-file".to_string(),
                    ));
                }
                buf.copy_to_vec_interleaved(&mut scratch);
                last_frames = Some(buf.frames());
                push(&mut out, &scratch)?;
            }
            // a corrupt packet: keep the timing with silence (REQ-034)
            Err(Error::DecodeError(_) | Error::IoError(_)) => {
                failed += 1;
                let frames = fill_frames(
                    last_frames,
                    packet.dur.get(),
                    info.time_base,
                    rate,
                    info.params.max_frames_per_packet,
                );
                push_silence(&mut out, frames.saturating_mul(channels))?;
            }
            Err(e) => return Err(e.into()),
        }
    }

    if failed * 100 > packets * MAX_FAILED_PERCENT {
        return Err(DecodeError::InvalidData(format!(
            "{failed} of {packets} packets failed to decode"
        )));
    }

    // trims AAC end padding (REQ-033) and applies the header's frame count
    // (REQ-031); a bound only ever shortens the output
    let frames = [info.num_frames, frame_limit]
        .into_iter()
        .flatten()
        .fold(out.len() / channels, |n, bound| {
            n.min(usize::try_from(bound).unwrap_or(usize::MAX))
        });
    if frames == 0 {
        return Err(DecodeError::InvalidData("no audio frames".to_string()));
    }
    out.truncate(frames * channels);

    let mut file = AudioFile::new(
        name,
        ext,
        rate,
        info.stream.num_channels,
        info.bits_per_sample,
        out,
    );
    file.channel_layout = info.stream.channel_layout;
    Ok(file)
}

/// Reads the headers only.
pub(super) fn probe(ext: &str, bytes: Vec<u8>) -> DecodeResult<StreamInfo> {
    let (bytes, _) = riff_fixup::repair(bytes);
    let reader = open(ext, bytes)?;
    Ok(default_track(reader.as_ref())?.stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZero;

    fn tb(numer: u32, denom: u32) -> Option<TimeBase> {
        Some(TimeBase {
            numer: NonZero::new(numer).unwrap(),
            denom: NonZero::new(denom).unwrap(),
        })
    }

    #[test]
    fn fill_frames_is_bounded() {
        // the last good buffer wins over the packet's duration
        assert_eq!(
            fill_frames(Some(1152), 9_999, tb(1, 44100), 44100, None),
            1152
        );
        // 1/90000 time base: 1800 ticks are 20 ms, so 882 frames at 44.1 kHz
        assert_eq!(fill_frames(None, 1800, tb(1, 90000), 44100, None), 882);
        // overflow or no time base gives a bounded result
        assert_eq!(fill_frames(None, u64::MAX, tb(1, 44100), 44100, None), 0);
        assert_eq!(fill_frames(None, 4096, None, 44100, None), 0);
        // never above one second, nor above the codec's packet size
        assert_eq!(fill_frames(None, 10_000_000, tb(1, 1), 48000, None), 48000);
        assert_eq!(fill_frames(Some(1 << 30), 0, None, 48000, None), 48000);
        assert_eq!(fill_frames(Some(5000), 0, None, 48000, Some(1024)), 1024);
    }

    #[test]
    fn gap_frames_is_bounded() {
        assert_eq!(gap_frames(4608, tb(1, 44100), 44100), 4608);
        // 1/90000 time base: 9000 ticks are 100 ms
        assert_eq!(gap_frames(9000, tb(1, 90000), 48000), 4800);
        assert_eq!(gap_frames(u64::MAX, tb(1, 44100), 44100), 0);
        assert_eq!(gap_frames(4608, None, 44100), 0);
        assert_eq!(gap_frames(10_000_000, tb(1, 44100), 44100), 44100);
    }
}
