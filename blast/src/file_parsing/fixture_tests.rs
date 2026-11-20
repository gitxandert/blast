//! End-to-end decoder checks. The ffmpeg-generated fixtures live in
//! `assets/.fixtures/` (see `plans/detailed/F1b.md` for the commands); each
//! test returns early when its files are missing.

use super::raw::{RawSpec, SampleEncoding, decode_with};
use super::test_bytes::*;
use super::*;

const FRAMES: usize = 66_150;

fn fixture(name: &str) -> Option<Vec<u8>> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/.fixtures/");
    fs::read(format!("{dir}{name}")).ok()
}

fn decode(name: &str) -> Option<AudioFile> {
    fixture(name).map(|bytes| decode_bytes(name, bytes).unwrap())
}

// ffmpeg's own decode, stored as raw stereo f32le
fn reference(name: &str) -> Option<Vec<f32>> {
    let spec = RawSpec {
        encoding: SampleEncoding::F32Le,
        sample_rate: 44100,
        channels: 2,
    };
    fixture(&format!("{name}.ref.f32")).map(|b| decode_with("ref", &b, spec).unwrap().samples)
}

fn rms(samples: &[f32]) -> f64 {
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / samples.len().max(1) as f64).sqrt()
}

fn rms_diff(a: &[f32], b: &[f32]) -> f64 {
    let diff: Vec<f32> = a.iter().zip(b).map(|(x, y)| x - y).collect();
    rms(&diff)
}

// -60 dB below full scale
const CLOSE: f64 = 1e-3;

#[test]
fn g711_matches_symphonia() {
    let bytes: Vec<u8> = (0..=255).collect();
    let mut fmt = fmt_body(0, 1, 8000, 8);
    fmt.extend_from_slice(&0u16.to_le_bytes());
    for (tag, ours) in [
        (6u16, pcm::alaw_to_f32 as fn(u8) -> f32),
        (7, pcm::ulaw_to_f32),
    ] {
        fmt[..2].copy_from_slice(&tag.to_le_bytes());
        let wav = riff(&[chunk_le(b"fmt ", &fmt), chunk_le(b"data", &bytes)]);
        let theirs = decode_bytes("g711.wav", wav).unwrap().samples;
        let ours: Vec<f32> = bytes.iter().map(|&b| ours(b)).collect();
        assert_eq!(ours, theirs, "tag {tag}");
    }
}

#[test]
fn pcm_matches_symphonia() {
    // every 16-bit pattern's high and low bytes, plus extremes
    let data: Vec<u8> = (0..=255u8)
        .flat_map(|b| [b, b.wrapping_mul(7), !b])
        .collect();
    let data = &data[..data.len() / 24 * 24];
    let wav = |tag, bits| {
        decode_bytes(
            "p.wav",
            riff(&[
                chunk_le(b"fmt ", &fmt_body(tag, 1, 8000, bits)),
                chunk_le(b"data", data),
            ]),
        )
        .unwrap()
        .samples
    };
    use SampleEncoding::*;
    assert_eq!(pcm::to_f32(data, U8), wav(1, 8));
    assert_eq!(pcm::to_f32(data, S16Le), wav(1, 16));
    assert_eq!(pcm::to_f32(data, S24Le), wav(1, 24));
    assert_eq!(pcm::to_f32(data, S32Le), wav(1, 32));
    let floats: Vec<u8> = (-100..100)
        .flat_map(|i| (i as f32 / 37.0).to_le_bytes())
        .collect();
    let f32_wav = riff(&[
        chunk_le(b"fmt ", &fmt_body(3, 1, 8000, 32)),
        chunk_le(b"data", &floats),
    ]);
    assert_eq!(
        pcm::to_f32(&floats, F32Le),
        decode_bytes("f.wav", f32_wav).unwrap().samples
    );
    let aiff = |bits| {
        let frames = (data.len() / (bits as usize / 8)) as u32;
        decode_bytes(
            "p.aif",
            form(
                b"AIFF",
                &[
                    chunk_be(b"COMM", &comm_body(1, frames, bits, 8000)),
                    chunk_be(b"SSND", &ssnd_body(0, data)),
                ],
            ),
        )
        .unwrap()
        .samples
    };
    assert_eq!(pcm::to_f32(data, S16Be), aiff(16));
    assert_eq!(pcm::to_f32(data, S24Be), aiff(24));
}

#[test]
fn raw_decode_file_uses_name() {
    let file = decode_bytes("x.s16be.22050.1.raw", vec![0x40, 0x00, 0xC0, 0x00]).unwrap();
    assert_eq!(file.sample_rate, 22050);
    assert_eq!(file.num_channels, 1);
    assert_eq!(file.file_name, "x");
    assert_eq!(file.samples, vec![0.5, -0.5]);
    match decode_bytes("x.raw", vec![0; 4]) {
        Err(DecodeError::InvalidData(m)) => {
            assert!(m.contains("encoding") && m.contains("rate") && m.contains("channels"))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn au_magic_wins_over_extension() {
    let mut au = au::MAGIC.to_vec();
    for w in [24u32, 2, 3, 8000, 1] {
        au.extend_from_slice(&w.to_be_bytes());
    }
    au.extend_from_slice(&[0x40, 0x00]);
    let file = decode_bytes("odd.wav", au).unwrap();
    assert_eq!(file.format, "au");
    assert_eq!(file.samples, vec![0.5]);
}

#[test]
fn lossless_fixtures_match_source() {
    let Some(src) = decode("src.wav") else {
        return;
    };
    assert_eq!(src.samples.len(), FRAMES * 2);
    assert_eq!(src.sample_rate, 44100);
    for name in [
        "a.flac",
        "a_alac.m4a",
        "a.aiff",
        "a_sowt.aifc",
        "a.au",
        "a.s16le.44100.2.raw",
    ] {
        let Some(file) = decode(name) else {
            continue;
        };
        assert_eq!(file.sample_rate, 44100, "{name}");
        assert_eq!(file.num_channels, 2, "{name}");
        assert!(file.samples == src.samples, "{name} differs from src.wav");
    }
}

#[test]
fn g711_fixtures_match_ffmpeg() {
    for name in ["a_alaw.wav", "a_mulaw.wav"] {
        let (Some(file), Some(reference)) = (decode(name), reference(name)) else {
            continue;
        };
        assert!(file.samples == reference, "{name}");
    }
}

#[test]
fn adpcm_fixtures_match_ffmpeg() {
    for name in ["a_ima.wav", "a_ms.wav"] {
        let (Some(file), Some(reference)) = (decode(name), reference(name)) else {
            continue;
        };
        // ffmpeg ignores `fact` too, so only the length check uses the source
        assert_eq!(file.samples.len(), FRAMES * 2, "{name}");
        assert!(rms_diff(&file.samples, &reference) < CLOSE, "{name}");
    }
}

#[test]
fn mp3_is_gapless() {
    let (Some(file), Some(reference)) = (decode("a.mp3"), reference("a.mp3")) else {
        return;
    };
    assert_eq!(file.samples.len(), FRAMES * 2);
    assert!(rms_diff(&file.samples, &reference) < CLOSE);
}

#[test]
fn aac_length_and_level() {
    // REQ-033: AAC keeps its encoder priming; MP4 is trimmed at the end only
    for (name, max_frames) in [("a.m4a", 1024 + FRAMES), ("a.aac", 67_584)] {
        let (Some(file), Some(reference)) = (decode(name), reference(name)) else {
            continue;
        };
        assert!(
            file.samples.len() <= max_frames * 2,
            "{name}: {} frames",
            file.samples.len() / 2
        );
        let (ours, theirs) = (rms(&file.samples), rms(&reference));
        assert!(
            (ours - theirs).abs() < theirs * 0.01,
            "{name}: {ours} vs {theirs}"
        );
    }
}

#[test]
fn six_channel_layout() {
    let Some(file) = decode("six.wav") else {
        return;
    };
    assert_eq!(file.num_channels, 6);
    assert_eq!(file.channel_layout, Some(0x3F));
}

#[test]
fn fixture_names_and_formats() {
    for (name, format) in [
        ("a.flac", "flac"),
        ("a_sowt.aifc", "aiff"),
        ("a.aac", "aac"),
        ("a.m4a", "m4a"),
        ("a.mp3", "mp3"),
        ("a.au", "au"),
        ("a.s16le.44100.2.raw", "raw"),
    ] {
        let Some(file) = decode(name) else {
            continue;
        };
        assert_eq!(file.format, format, "{name}");
        assert_eq!(file.file_name, name.split('.').next().unwrap());
    }
}

#[test]
#[ignore]
fn decode_speed_long_mp3() {
    let Some(bytes) = fixture("long.mp3") else {
        return;
    };
    let start = std::time::Instant::now();
    let file = decode_bytes("long.mp3", bytes).unwrap();
    let elapsed = start.elapsed();
    println!(
        "decoded {} frames of long.mp3 in {:.3} s",
        file.samples.len() / 2,
        elapsed.as_secs_f64()
    );
    if !cfg!(debug_assertions) {
        assert!(elapsed.as_secs_f64() < 1.0);
    }
}
