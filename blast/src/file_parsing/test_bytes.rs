//! Builders for synthetic WAV and AIFF files, shared by the decoder tests.

/// Inverse of a 16-bit decode, so tests can state expectations in i16.
pub(super) fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|&s| (s * 32768.0) as i16).collect()
}

pub(super) fn i16_le(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

pub(super) fn i16_be(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_be_bytes()).collect()
}

/// Little-endian chunk: id + size + body (+ pad byte if odd).
pub(super) fn chunk_le(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

pub(super) fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = chunks.concat();
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
    v.extend_from_slice(b"WAVE");
    v.extend_from_slice(&body);
    v
}

pub(super) fn fmt_body(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
    let block = channels * bits.div_ceil(8);
    let mut v = Vec::new();
    v.extend_from_slice(&tag.to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * block as u32).to_le_bytes());
    v.extend_from_slice(&block.to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    v
}

/// WAVE_FORMAT_EXTENSIBLE fmt body (40 bytes, cbSize 22).
pub(super) fn fmt_ext_body(
    sub_tag: u16,
    channels: u16,
    rate: u32,
    bits: u16,
    valid_bits: u16,
) -> Vec<u8> {
    let mut v = fmt_body(0xFFFE, channels, rate, bits);
    v.extend_from_slice(&22u16.to_le_bytes());
    v.extend_from_slice(&valid_bits.to_le_bytes());
    v.extend_from_slice(&3u32.to_le_bytes());
    // KSDATAFORMAT_SUBTYPE GUID: first 2 bytes are the format tag
    v.extend_from_slice(&sub_tag.to_le_bytes());
    v.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
    ]);
    v
}

/// A 16-bit PCM WAV.
pub(super) fn wav_bytes(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    riff(&[
        chunk_le(b"fmt ", &fmt_body(1, channels, rate, 16)),
        chunk_le(b"data", &i16_le(samples)),
    ])
}

/// Big-endian chunk: id + size + body (+ pad byte if odd).
pub(super) fn chunk_be(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend_from_slice(&(body.len() as u32).to_be_bytes());
    v.extend_from_slice(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

pub(super) fn form(kind: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = chunks.concat();
    let mut v = b"FORM".to_vec();
    v.extend_from_slice(&(body.len() as u32 + 4).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(&body);
    v
}

/// 80-bit IEEE extended for a positive integer rate.
pub(super) fn extended(rate: u32) -> [u8; 10] {
    let mant = rate as u64;
    let shift = mant.leading_zeros();
    let exp = 16383 + 63 - shift as u16;
    let mut v = [0u8; 10];
    v[..2].copy_from_slice(&exp.to_be_bytes());
    v[2..].copy_from_slice(&(mant << shift).to_be_bytes());
    v
}

pub(super) fn comm_body(channels: u16, frames: u32, bits: u16, rate: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&channels.to_be_bytes());
    v.extend_from_slice(&frames.to_be_bytes());
    v.extend_from_slice(&bits.to_be_bytes());
    v.extend_from_slice(&extended(rate));
    v
}

pub(super) fn ssnd_body(offset: u32, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&offset.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(data);
    v
}

/// A 16-bit AIFF.
pub(super) fn aiff_bytes(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let frames = samples.len() as u32 / channels as u32;
    form(
        b"AIFF",
        &[
            chunk_be(b"COMM", &comm_body(channels, frames, 16, rate)),
            chunk_be(b"SSND", &ssnd_body(0, &i16_be(samples))),
        ],
    )
}
