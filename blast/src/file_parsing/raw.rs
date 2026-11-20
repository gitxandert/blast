//! Headerless PCM (README §3.2, §4 item 5). The parameters come from the
//! file name (`kick.s16le.48000.2.raw`) or from `import` flags.

use super::StreamInfo;
use super::decode_helpers::{AudioFile, DecodeError, DecodeResult};
use super::pcm;
use std::str::FromStr;

/// How one sample is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleEncoding {
    S8,
    U8,
    S16Le,
    S16Be,
    S24Le,
    S24Be,
    S32Le,
    S32Be,
    F32Le,
    F32Be,
    F64Le,
    F64Be,
}

impl SampleEncoding {
    const ALL: [(&'static str, SampleEncoding); 12] = [
        ("s8", Self::S8),
        ("u8", Self::U8),
        ("s16le", Self::S16Le),
        ("s16be", Self::S16Be),
        ("s24le", Self::S24Le),
        ("s24be", Self::S24Be),
        ("s32le", Self::S32Le),
        ("s32be", Self::S32Be),
        ("f32le", Self::F32Le),
        ("f32be", Self::F32Be),
        ("f64le", Self::F64Le),
        ("f64be", Self::F64Be),
    ];

    /// Bytes per sample.
    pub fn bytes(self) -> usize {
        self.bits() as usize / 8
    }

    pub fn bits(self) -> u32 {
        use SampleEncoding::*;
        match self {
            S8 | U8 => 8,
            S16Le | S16Be => 16,
            S24Le | S24Be => 24,
            S32Le | S32Be | F32Le | F32Be => 32,
            F64Le | F64Be => 64,
        }
    }
}

impl FromStr for SampleEncoding {
    type Err = DecodeError;

    /// `s8`, `u8`, `s16le`, `s16be`, ... `f64be`, ignoring ASCII case.
    fn from_str(s: &str) -> DecodeResult<Self> {
        Self::ALL
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(s))
            .map(|&(_, encoding)| encoding)
            .ok_or_else(|| DecodeError::UnsupportedFormat(format!("sample encoding '{s}'")))
    }
}

/// A complete raw PCM description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawSpec {
    pub encoding: SampleEncoding,
    pub sample_rate: u32,
    pub channels: u32,
}

/// Parameters that may be missing; the file name supplies some, flags others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawParams {
    pub encoding: Option<SampleEncoding>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
}

impl RawParams {
    /// Fields set in `self` win over `fallback` (flags over file name).
    pub fn or(self, fallback: RawParams) -> RawParams {
        RawParams {
            encoding: self.encoding.or(fallback.encoding),
            sample_rate: self.sample_rate.or(fallback.sample_rate),
            channels: self.channels.or(fallback.channels),
        }
    }

    /// The spec, or an error naming each missing field.
    pub fn complete(self) -> DecodeResult<RawSpec> {
        match self {
            RawParams {
                encoding: Some(encoding),
                sample_rate: Some(sample_rate),
                channels: Some(channels),
            } => Ok(RawSpec {
                encoding,
                sample_rate,
                channels,
            }),
            _ => {
                let missing: Vec<&str> = [
                    (self.encoding.is_none(), "encoding"),
                    (self.sample_rate.is_none(), "rate"),
                    (self.channels.is_none(), "channels"),
                ]
                .iter()
                .filter(|(gone, _)| *gone)
                .map(|&(_, name)| name)
                .collect();
                Err(DecodeError::InvalidData(format!(
                    "raw PCM needs {} (name it like 'kick.s16le.48000.2.raw')",
                    missing.join(", ")
                )))
            }
        }
    }
}

fn spec_rate(s: &str) -> Option<u32> {
    s.parse().ok().filter(|r| (1..=768_000).contains(r))
}

fn spec_channels(s: &str) -> Option<u32> {
    s.parse().ok().filter(|c| (1..=64).contains(c))
}

/// Splits `kick.s16le.48000.2.raw` into ("kick", params). Recognises the
/// suffixes `<enc>.<rate>.<ch>`, `<enc>.<rate>` and `<enc>` before
/// `.raw`/`.pcm`; anything else is part of the stem.
pub fn parse_raw_name(file_name: &str) -> (&str, RawParams) {
    let stem = match file_name.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("raw") || ext.eq_ignore_ascii_case("pcm") => {
            stem
        }
        _ => return (file_name, RawParams::default()),
    };
    let parts: Vec<&str> = stem.split('.').collect();
    // longest suffix first; the stem before it must be non-empty
    for take in [3, 2, 1] {
        if parts.len() <= take {
            continue;
        }
        let tail = &parts[parts.len() - take..];
        let Ok(encoding) = tail[0].parse::<SampleEncoding>() else {
            continue;
        };
        let sample_rate = match tail.get(1) {
            Some(r) => match spec_rate(r) {
                Some(r) => Some(r),
                None => continue,
            },
            None => None,
        };
        let channels = match tail.get(2) {
            Some(c) => match spec_channels(c) {
                Some(c) => Some(c),
                None => continue,
            },
            None => None,
        };
        let name_len = stem.len() - tail.iter().map(|p| p.len() + 1).sum::<usize>();
        return (
            &stem[..name_len],
            RawParams {
                encoding: Some(encoding),
                sample_rate,
                channels,
            },
        );
    }
    (stem, RawParams::default())
}

/// Decodes headerless PCM with an explicit spec (README §3.2).
pub fn decode_with(name: &str, bytes: &[u8], spec: RawSpec) -> DecodeResult<AudioFile> {
    if spec.channels == 0 || spec.sample_rate == 0 {
        return Err(DecodeError::InvalidData(
            "raw PCM needs a non-zero rate and channel count".to_string(),
        ));
    }
    let mut samples = pcm::to_f32(bytes, spec.encoding);
    let channels = spec.channels as usize;
    samples.truncate(samples.len() / channels * channels);
    if samples.is_empty() {
        return Err(DecodeError::InvalidData("no audio frames".to_string()));
    }
    Ok(AudioFile::new(
        name,
        "raw",
        spec.sample_rate,
        spec.channels,
        spec.encoding.bits(),
        samples,
    ))
}

/// Rate and channels from the file name alone.
pub(super) fn probe(file_name: &str) -> DecodeResult<StreamInfo> {
    let spec = parse_raw_name(file_name).1.complete()?;
    Ok(StreamInfo {
        sample_rate: spec.sample_rate,
        num_channels: spec.channels,
        channel_layout: None,
    })
}

#[cfg(test)]
mod tests {
    use super::SampleEncoding::*;
    use super::*;

    fn params(e: Option<SampleEncoding>, r: Option<u32>, c: Option<u32>) -> RawParams {
        RawParams {
            encoding: e,
            sample_rate: r,
            channels: c,
        }
    }

    #[test]
    fn raw_name_parsing() {
        assert_eq!(
            parse_raw_name("kick.s16le.48000.2.raw"),
            ("kick", params(Some(S16Le), Some(48000), Some(2)))
        );
        assert_eq!(
            parse_raw_name("kick.f32be.44100.pcm"),
            ("kick", params(Some(F32Be), Some(44100), None))
        );
        assert_eq!(
            parse_raw_name("kick.u8.raw"),
            ("kick", params(Some(U8), None, None))
        );
        assert_eq!(
            parse_raw_name("my.loop.s16le.44100.2.raw"),
            ("my.loop", params(Some(S16Le), Some(44100), Some(2)))
        );
        assert_eq!(
            parse_raw_name("kick.S16LE.48000.2.RAW"),
            ("kick", params(Some(S16Le), Some(48000), Some(2)))
        );
        // a bad channel count falls back to the shorter suffix
        assert_eq!(
            parse_raw_name("kick.s16le.48000.99.raw"),
            ("kick.s16le.48000.99", RawParams::default())
        );
        assert_eq!(
            parse_raw_name("kick.s16le.0.raw"),
            ("kick.s16le.0", RawParams::default())
        );
        // the encoding alone can't be the whole name
        assert_eq!(parse_raw_name("s16le.raw"), ("s16le", RawParams::default()));
        assert_eq!(parse_raw_name("kick.raw"), ("kick", RawParams::default()));
        assert_eq!(
            parse_raw_name("kick.wav"),
            ("kick.wav", RawParams::default())
        );
    }

    #[test]
    fn raw_params_flags_win() {
        let flags = params(None, Some(22050), None);
        let name = params(Some(S8), Some(44100), Some(1));
        assert_eq!(flags.or(name), params(Some(S8), Some(22050), Some(1)));
        assert!(flags.or(name).complete().is_ok());
        match params(None, Some(1), None).complete() {
            Err(DecodeError::InvalidData(m)) => {
                assert!(m.contains("encoding") && m.contains("channels") && !m.contains("rate"))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn raw_decode_with_each_encoding() {
        for (_, encoding) in SampleEncoding::ALL {
            let spec = RawSpec {
                encoding,
                sample_rate: 8000,
                channels: 2,
            };
            // two whole frames plus one extra sample
            let bytes = vec![0u8; encoding.bytes() * 5];
            let file = decode_with("x", &bytes, spec).unwrap();
            assert_eq!(file.samples.len(), 4, "{encoding:?}");
            assert_eq!(file.bits_per_sample, encoding.bits());
            assert_eq!(file.format, "raw");
        }
        let spec = |sample_rate, channels| RawSpec {
            encoding: S16Le,
            sample_rate,
            channels,
        };
        for bad in [spec(8000, 0), spec(0, 1)] {
            assert!(matches!(
                decode_with("x", &[0; 4], bad),
                Err(DecodeError::InvalidData(_))
            ));
        }
        assert!(matches!(
            decode_with("x", &[0; 3], spec(8000, 2)),
            Err(DecodeError::InvalidData(_))
        ));
    }
}
