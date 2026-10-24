//! Sun/NeXT `.au`/`.snd`: a 24-byte big-endian header, then the samples.
//! Little-endian `dns.` files are rare and unsupported.

use super::StreamInfo;
use super::decode_helpers::{AudioFile, DecodeError, DecodeResult};
use super::pcm;
use super::raw::SampleEncoding;

pub(super) const MAGIC: &[u8; 4] = b".snd";
const HEADER_LEN: usize = 24;

enum Encoding {
    Ulaw,
    Alaw,
    Pcm(SampleEncoding),
}

struct Header {
    data: std::ops::Range<usize>,
    encoding: Encoding,
    bits: u32,
    rate: u32,
    channels: u32,
}

fn word(bytes: &[u8], i: usize) -> u32 {
    let at = 4 * i;
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn header(bytes: &[u8]) -> DecodeResult<Header> {
    let Some(head) = bytes.get(..HEADER_LEN) else {
        return Err(DecodeError::UnexpectedEof);
    };
    if &head[..4] != MAGIC {
        return Err(DecodeError::UnsupportedFormat(
            "not a big-endian AU file".to_string(),
        ));
    }
    let offset = word(head, 1) as usize;
    if offset < HEADER_LEN || offset > bytes.len() {
        return Err(DecodeError::InvalidData(format!(
            "AU data offset {offset} is outside the file"
        )));
    }
    // 0xFFFFFFFF means unknown; either way, read no further than the file
    let size = word(head, 2) as usize;
    let end = offset.saturating_add(size).min(bytes.len());
    let (encoding, bits) = match word(head, 3) {
        1 => (Encoding::Ulaw, 8),
        2 => (Encoding::Pcm(SampleEncoding::S8), 8),
        3 => (Encoding::Pcm(SampleEncoding::S16Be), 16),
        4 => (Encoding::Pcm(SampleEncoding::S24Be), 24),
        5 => (Encoding::Pcm(SampleEncoding::S32Be), 32),
        6 => (Encoding::Pcm(SampleEncoding::F32Be), 32),
        7 => (Encoding::Pcm(SampleEncoding::F64Be), 64),
        27 => (Encoding::Alaw, 8),
        code => {
            return Err(DecodeError::UnsupportedFormat(format!(
                "AU encoding {code}"
            )));
        }
    };
    let (rate, channels) = (word(head, 4), word(head, 5));
    if rate == 0 || channels == 0 {
        return Err(DecodeError::InvalidData(
            "AU rate or channel count is 0".to_string(),
        ));
    }
    Ok(Header {
        data: offset..end,
        encoding,
        bits,
        rate,
        channels,
    })
}

pub(super) fn decode(name: &str, bytes: &[u8]) -> DecodeResult<AudioFile> {
    let h = header(bytes)?;
    let data = bytes.get(h.data.clone()).unwrap_or(&[]);
    let mut samples = match h.encoding {
        Encoding::Ulaw => data.iter().map(|&b| pcm::ulaw_to_f32(b)).collect(),
        Encoding::Alaw => data.iter().map(|&b| pcm::alaw_to_f32(b)).collect(),
        Encoding::Pcm(encoding) => pcm::to_f32(data, encoding),
    };
    let channels = h.channels as usize;
    samples.truncate(samples.len() / channels * channels);
    if samples.is_empty() {
        return Err(DecodeError::InvalidData("no audio frames".to_string()));
    }
    Ok(AudioFile::new(
        name, "au", h.rate, h.channels, h.bits, samples,
    ))
}

pub(super) fn probe(bytes: &[u8]) -> DecodeResult<StreamInfo> {
    let h = header(bytes)?;
    Ok(StreamInfo {
        sample_rate: h.rate,
        num_channels: h.channels,
        channel_layout: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn au(offset: u32, size: u32, code: u32, rate: u32, channels: u32, rest: &[u8]) -> Vec<u8> {
        let mut v = MAGIC.to_vec();
        for w in [offset, size, code, rate, channels] {
            v.extend_from_slice(&w.to_be_bytes());
        }
        v.extend_from_slice(rest);
        v
    }

    fn simple(code: u32, data: &[u8]) -> Vec<u8> {
        au(24, data.len() as u32, code, 8000, 1, data)
    }

    #[test]
    fn au_each_encoding() {
        let cases: [(u32, &[u8], Vec<f32>, u32); 8] = [
            (1, &[0xFF, 0x80], vec![0.0, 32124.0 / 32768.0], 8),
            (2, &[0x40, 0x80], vec![0.5, -1.0], 8),
            (3, &[0x40, 0x00], vec![0.5], 16),
            (4, &[0xC0, 0x00, 0x00], vec![-0.5], 24),
            (5, &[0x40, 0, 0, 0], vec![0.5], 32),
            (6, &0.25f32.to_be_bytes(), vec![0.25], 32),
            (7, &(-0.75f64).to_be_bytes(), vec![-0.75], 64),
            (27, &[0xD5], vec![8.0 / 32768.0], 8),
        ];
        for (code, data, expected, bits) in cases {
            let file = decode("x", &simple(code, data)).unwrap();
            assert_eq!(file.samples, expected, "code {code}");
            assert_eq!(file.bits_per_sample, bits);
            assert_eq!(file.format, "au");
            assert_eq!(file.sample_rate, 8000);
        }
    }

    #[test]
    fn au_unknown_size_reads_to_end() {
        let file = decode("x", &au(24, 0xFFFF_FFFF, 3, 8000, 1, &[0, 1, 0, 2])).unwrap();
        assert_eq!(file.samples.len(), 2);
    }

    #[test]
    fn au_size_past_end_is_clamped() {
        let file = decode("x", &au(24, 1000, 3, 8000, 2, &[0, 1, 0, 2, 0, 3])).unwrap();
        // the partial last frame is dropped
        assert_eq!(file.samples.len(), 2);
    }

    #[test]
    fn au_offset_skips_annotation() {
        let mut rest = b"comment\0".to_vec();
        rest.extend_from_slice(&[0x40, 0x00]);
        let file = decode("x", &au(32, 2, 3, 8000, 1, &rest)).unwrap();
        assert_eq!(file.samples, vec![0.5]);
    }

    #[test]
    fn au_bad_headers() {
        let good = simple(3, &[0, 1]);
        assert!(matches!(
            decode("x", &good[..20]),
            Err(DecodeError::UnexpectedEof)
        ));
        for bad in [
            au(16, 2, 3, 8000, 1, &[0, 1]),
            au(100, 2, 3, 8000, 1, &[0, 1]),
            au(24, 2, 3, 0, 1, &[0, 1]),
            au(24, 2, 3, 8000, 0, &[0, 1]),
            au(24, 0, 3, 8000, 1, &[]),
        ] {
            assert!(matches!(
                decode("x", &bad),
                Err(DecodeError::InvalidData(_))
            ));
        }
        assert!(matches!(
            decode("x", &simple(23, &[0, 1])),
            Err(DecodeError::UnsupportedFormat(_))
        ));
        let mut dns = good.clone();
        dns[..4].copy_from_slice(b"dns.");
        assert!(matches!(
            decode("x", &dns),
            Err(DecodeError::UnsupportedFormat(_))
        ));
    }

    #[test]
    fn truncated_prefixes_never_panic_raw_au() {
        let bytes = au(
            32,
            8,
            1,
            8000,
            2,
            b"note\0\0\0\0\x01\x02\x03\x04\x05\x06\x07\x08",
        );
        for n in 0..=bytes.len() {
            let _ = decode("x", &bytes[..n]);
            let _ = probe(&bytes[..n]);
        }
    }
}
