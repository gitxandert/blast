//! The old WAV/AIFF parser tests, run through `decode_bytes` (symphonia).

use super::test_bytes::*;
use super::*;

const SAMPLES: [i16; 6] = [0, 1, -1, i16::MAX, i16::MIN, 1234];

fn wav(chunks: &[Vec<u8>]) -> DecodeResult<AudioFile> {
    decode_bytes("test.wav", riff(chunks))
}

fn pcm16_wav() -> Vec<u8> {
    riff(&[
        chunk_le(b"fmt ", &fmt_body(1, 2, 48000, 16)),
        chunk_le(b"data", &i16_le(&SAMPLES)),
    ])
}

fn aiff(bytes: Vec<u8>) -> DecodeResult<AudioFile> {
    decode_bytes("test.aif", bytes)
}

fn aiff_depth(channels: u16, bits: u16, data: &[u8]) -> AudioFile {
    let width = (bits as usize).div_ceil(8);
    let frames = (data.len() / width / channels as usize) as u32;
    aiff(form(
        b"AIFF",
        &[
            chunk_be(b"COMM", &comm_body(channels, frames, bits, 44100)),
            chunk_be(b"SSND", &ssnd_body(0, data)),
        ],
    ))
    .unwrap()
}

// full-scale positive (as close as the depth allows), full-scale negative, zero
fn full_scale(bits: u32) -> Vec<f32> {
    let top = (1u64 << (bits - 1)) as f64;
    vec![((top - 1.0) / top) as f32, -1.0, 0.0]
}

#[test]
fn wav_pcm16() {
    let file = decode_bytes("kick.wav", pcm16_wav()).unwrap();
    assert_eq!(file.sample_rate, 48000);
    assert_eq!(file.num_channels, 2);
    assert_eq!(file.bits_per_sample, 16);
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
    assert_eq!(file.format, "wav");
    assert_eq!(file.file_name, "kick");
}

#[test]
fn wav_extensible16() {
    let file = wav(&[
        chunk_le(b"fmt ", &fmt_ext_body(1, 2, 44100, 16, 16)),
        chunk_le(b"data", &i16_le(&SAMPLES)),
    ])
    .unwrap();
    assert_eq!(file.sample_rate, 44100);
    assert_eq!(file.num_channels, 2);
    assert_eq!(file.bits_per_sample, 16);
    assert_eq!(file.channel_layout, Some(3));
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
}

#[test]
fn wav_fmt18_zero_cb_size() {
    let mut fmt = fmt_body(1, 1, 48000, 16);
    fmt.extend_from_slice(&0u16.to_le_bytes());
    let file = wav(&[
        chunk_le(b"fmt ", &fmt),
        chunk_le(b"data", &i16_le(&SAMPLES)),
    ])
    .unwrap();
    assert_eq!(file.num_channels, 1);
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
}

#[test]
fn wav_truncated_data_reads_whole_frames() {
    let bytes = pcm16_wav();
    let file = decode_bytes("t.wav", bytes[..bytes.len() - 3].to_vec()).unwrap();
    assert_eq!(f32_to_i16(&file.samples), SAMPLES[..4]);
}

#[test]
fn wav_truncated_header_is_eof() {
    let bytes = pcm16_wav();
    assert!(matches!(
        decode_bytes("t.wav", bytes[..20].to_vec()),
        Err(DecodeError::UnexpectedEof)
    ));
}

fn wav_depth(fmt: Vec<u8>, data: &[u8]) -> AudioFile {
    wav(&[chunk_le(b"fmt ", &fmt), chunk_le(b"data", data)]).unwrap()
}

#[test]
fn wav_u8() {
    let file = wav_depth(fmt_body(1, 1, 48000, 8), &[0xFF, 0x00, 0x80]);
    assert_eq!(file.samples, vec![127.0 / 128.0, -1.0, 0.0]);
    assert_eq!(file.bits_per_sample, 8);
}

#[test]
fn wav_s24() {
    let file = wav_depth(
        fmt_body(1, 1, 48000, 24),
        &[0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00],
    );
    assert_eq!(file.samples, full_scale(24));
    assert_eq!(file.bits_per_sample, 24);
}

#[test]
fn wav_s32() {
    let data: Vec<u8> = [i32::MAX, i32::MIN, 0]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    let file = wav_depth(fmt_body(1, 1, 48000, 32), &data);
    // i32::MAX / 2^31 rounds to 1.0 in f32
    assert_eq!(file.samples, vec![1.0, -1.0, 0.0]);
}

#[test]
fn wav_f32() {
    let data: Vec<u8> = [1.0f32, -1.0, 0.0, 0.25]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    let file = wav_depth(fmt_body(3, 1, 48000, 32), &data);
    assert_eq!(file.samples, vec![1.0, -1.0, 0.0, 0.25]);
}

#[test]
fn wav_f64() {
    let data: Vec<u8> = [1.0f64, -1.0, 0.0, -0.5]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    let file = wav_depth(fmt_body(3, 1, 48000, 64), &data);
    assert_eq!(file.samples, vec![1.0, -1.0, 0.0, -0.5]);
}

#[test]
fn wav_extensible_sub_format_and_valid_bits() {
    let data = [0xF0, 0xFF, 0x7F, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00];
    let file = wav_depth(fmt_ext_body(1, 1, 48000, 24, 20), &data);
    assert_eq!(f32_to_i16(&file.samples), vec![i16::MAX, i16::MIN, 0]);
    // informational: symphonia reports the valid bits
    assert_eq!(file.bits_per_sample, 20);

    let data: Vec<u8> = [1.0f32, -1.0, 0.0]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    let file = wav_depth(fmt_ext_body(3, 1, 48000, 32, 32), &data);
    assert_eq!(file.samples, vec![1.0, -1.0, 0.0]);
}

#[test]
fn wav_unsupported_formats_are_errors() {
    // A-law with a 16-byte fmt (symphonia wants 18)
    assert!(
        wav(&[
            chunk_le(b"fmt ", &fmt_body(6, 1, 8000, 8)),
            chunk_le(b"data", &[0xD5])
        ])
        .is_err()
    );
    // extensible µ-law
    assert!(
        wav(&[
            chunk_le(b"fmt ", &fmt_ext_body(7, 1, 8000, 8, 8)),
            chunk_le(b"data", &[0xFF]),
        ])
        .is_err()
    );
    // f16 is malformed to symphonia
    assert!(matches!(
        wav(&[
            chunk_le(b"fmt ", &fmt_body(3, 1, 48000, 16)),
            chunk_le(b"data", &[0, 0])
        ]),
        Err(DecodeError::InvalidData(_))
    ));
    // MPEG inside WAV
    assert!(
        wav(&[
            chunk_le(b"fmt ", &fmt_body(0x55, 1, 48000, 16)),
            chunk_le(b"data", &[0, 0]),
        ])
        .is_err()
    );
}

#[test]
fn wav_skips_list_chunk() {
    let file = wav(&[
        chunk_le(b"fmt ", &fmt_body(1, 2, 48000, 16)),
        chunk_le(b"LIST", b"INFOISFT\x05\x00\x00\x00blast\x00"),
        chunk_le(b"data", &i16_le(&SAMPLES)),
    ])
    .unwrap();
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
}

#[test]
fn wav_odd_chunk_padding() {
    let chunks = [
        chunk_le(b"JUNK", &[1, 2, 3]),
        chunk_le(b"fmt ", &fmt_body(1, 1, 48000, 16)),
        chunk_le(b"bext", &[9; 7]),
        chunk_le(b"data", &i16_le(&SAMPLES)),
    ];
    // odd chunks really are padded in the test data
    assert_eq!(
        riff(&chunks).len(),
        12 + (8 + 4) + (8 + 16) + (8 + 8) + (8 + 12)
    );
    let file = wav(&chunks).unwrap();
    assert_eq!(file.num_channels, 1);
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
}

#[test]
fn wav_data_before_fmt() {
    let file = wav(&[
        chunk_le(b"data", &i16_le(&SAMPLES)),
        chunk_le(b"fmt ", &fmt_body(1, 2, 44100, 16)),
    ])
    .unwrap();
    assert_eq!(file.sample_rate, 44100);
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
}

#[test]
fn wav_streamed_data_size() {
    let mut bytes = pcm16_wav();
    // the data size field sits just before the samples
    let at = bytes.len() - SAMPLES.len() * 2 - 4;
    bytes[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    let file = decode_bytes("s.wav", bytes).unwrap();
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
}

#[test]
fn wav_streamed_riff_and_data_size() {
    let mut bytes = pcm16_wav();
    bytes[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    let at = bytes.len() - SAMPLES.len() * 2 - 4;
    bytes[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    let file = decode_bytes("s.wav", bytes).unwrap();
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
}

#[test]
fn wav_riff_size_too_small() {
    let mut bytes = pcm16_wav();
    bytes[4..8].copy_from_slice(&20u32.to_le_bytes());
    let file = decode_bytes("s.wav", bytes).unwrap();
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
}

#[test]
fn wav_trailing_bytes_after_riff() {
    let mut junk = pcm16_wav();
    junk.extend((0..37u8).map(|b| b.wrapping_mul(97)));
    let file = decode_bytes("j.wav", junk).unwrap();
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());

    let tagged = riff(&[
        chunk_le(b"fmt ", &fmt_body(1, 2, 48000, 16)),
        chunk_le(b"data", &i16_le(&SAMPLES)),
        chunk_le(b"id3 ", b"ID3\x04\x00\x00\x00\x00\x00\x00"),
    ]);
    let file = decode_bytes("t.wav", tagged).unwrap();
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
}

// one IMA ADPCM block of silence: 505 frames, mono
fn ima_adpcm(fact: Option<u32>) -> Vec<u8> {
    let mut fmt = fmt_body(0x11, 1, 8000, 4);
    fmt[8..12].copy_from_slice(&4055u32.to_le_bytes());
    fmt[12..14].copy_from_slice(&256u16.to_le_bytes());
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&505u16.to_le_bytes());
    let mut chunks = vec![chunk_le(b"fmt ", &fmt)];
    if let Some(frames) = fact {
        chunks.push(chunk_le(b"fact", &frames.to_le_bytes()));
    }
    chunks.push(chunk_le(b"data", &[0; 256]));
    riff(&chunks)
}

#[test]
fn wav_adpcm_fact_trims_padding() {
    let file = decode_bytes("a.wav", ima_adpcm(None)).unwrap();
    assert_eq!(file.samples.len(), 505);
    let file = decode_bytes("a.wav", ima_adpcm(Some(500))).unwrap();
    assert_eq!(file.samples.len(), 500);
}

#[test]
fn riff_fixup_limit_only_shortens() {
    // fact larger than the data
    let file = decode_bytes("a.wav", ima_adpcm(Some(100_000))).unwrap();
    assert_eq!(file.samples.len(), 505);
    // COMM larger than SSND
    let data = i16_be(&[1, 2, 3, 4, 5, 6]);
    let file = aiff(form(
        b"AIFF",
        &[
            chunk_be(b"COMM", &comm_body(2, 10, 16, 44100)),
            chunk_be(b"SSND", &ssnd_body(0, &data)),
        ],
    ))
    .unwrap();
    assert_eq!(f32_to_i16(&file.samples), vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn wav_wrong_magic() {
    let mut bytes = pcm16_wav();
    bytes[8..12].copy_from_slice(b"AVI ");
    assert!(matches!(
        decode_bytes("w.wav", bytes.clone()),
        Err(DecodeError::UnsupportedFormat(_))
    ));
    bytes[0..4].copy_from_slice(b"FORM");
    bytes[8..12].copy_from_slice(b"WAVE");
    assert!(matches!(
        decode_bytes("w.wav", bytes),
        Err(DecodeError::UnsupportedFormat(_))
    ));
}

#[test]
fn wav_missing_chunks() {
    assert!(matches!(
        wav(&[
            chunk_le(b"fmt ", &fmt_body(1, 2, 48000, 16)),
            chunk_le(b"LIST", &[0; 4]),
        ]),
        Err(DecodeError::UnsupportedFormat(_))
    ));
    assert!(matches!(
        wav(&[chunk_le(b"data", &i16_le(&SAMPLES))]),
        Err(DecodeError::InvalidData(_))
    ));
    assert!(matches!(
        wav(&[
            chunk_le(b"fmt ", &[1, 0, 2, 0]),
            chunk_le(b"data", &i16_le(&SAMPLES)),
        ]),
        Err(DecodeError::InvalidData(_))
    ));
}

#[test]
fn wav_empty_data_is_no_audio() {
    assert!(matches!(
        wav(&[
            chunk_le(b"fmt ", &fmt_body(1, 2, 48000, 16)),
            chunk_le(b"data", &[]),
        ]),
        Err(DecodeError::InvalidData(m)) if m == "no audio frames"
    ));
}

#[test]
fn aiff_s16() {
    let file = aiff_depth(2, 16, &i16_be(&SAMPLES));
    assert_eq!(file.sample_rate, 44100);
    assert_eq!(file.num_channels, 2);
    assert_eq!(file.bits_per_sample, 16);
    assert_eq!(file.format, "aiff");
    assert_eq!(f32_to_i16(&file.samples), SAMPLES.to_vec());
}

#[test]
fn aiff_s8() {
    let file = aiff_depth(1, 8, &[0x7F, 0x80, 0x00]);
    assert_eq!(file.samples, full_scale(8));
}

#[test]
fn aiff_s24() {
    let file = aiff_depth(
        1,
        24,
        &[0x7F, 0xFF, 0xFF, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    assert_eq!(file.samples, full_scale(24));
    assert_eq!(file.bits_per_sample, 24);
}

#[test]
fn aiff_s32() {
    let data: Vec<u8> = [i32::MAX, i32::MIN, 0]
        .iter()
        .flat_map(|s| s.to_be_bytes())
        .collect();
    let file = aiff_depth(1, 32, &data);
    assert_eq!(file.samples, vec![1.0, -1.0, 0.0]);
}

#[test]
fn aiff_skips_chunks_before_ssnd() {
    let samples = [5i16, -5, i16::MAX, i16::MIN];
    let file = aiff(form(
        b"AIFF",
        &[
            chunk_be(b"COMM", &comm_body(2, 2, 16, 48000)),
            chunk_be(b"MARK", &[0, 0]),
            chunk_be(b"COMT", &[0, 0]),
            // unknown, odd-sized (padded)
            chunk_be(b"XTRA", &[1, 2, 3]),
            chunk_be(b"SSND", &ssnd_body(0, &i16_be(&samples))),
        ],
    ))
    .unwrap();
    assert_eq!(file.sample_rate, 48000);
    assert_eq!(f32_to_i16(&file.samples), samples.to_vec());
}

#[test]
fn aiff_ssnd_before_comm() {
    let samples = [7i16, -7];
    let mut comm = comm_body(1, 2, 16, 44100);
    comm.extend_from_slice(&[0xAA; 4]);
    let file = aiff(form(
        b"AIFF",
        &[
            chunk_be(b"SSND", &ssnd_body(0, &i16_be(&samples))),
            chunk_be(b"APPL", &[0; 6]),
            chunk_be(b"COMM", &comm),
        ],
    ))
    .unwrap();
    assert_eq!(file.num_channels, 1);
    assert_eq!(f32_to_i16(&file.samples), samples.to_vec());
}

#[test]
fn aiff_ssnd_offset() {
    let mut data = vec![0xEE; 4];
    data.extend_from_slice(&[0x12, 0x34, 0x80, 0x00]);
    let file = aiff(form(
        b"AIFF",
        &[
            chunk_be(b"COMM", &comm_body(1, 2, 16, 44100)),
            chunk_be(b"SSND", &ssnd_body(4, &data)),
        ],
    ))
    .unwrap();
    assert_eq!(f32_to_i16(&file.samples), vec![0x1234, i16::MIN]);

    let result = aiff(form(
        b"AIFF",
        &[
            chunk_be(b"COMM", &comm_body(1, 2, 16, 44100)),
            chunk_be(b"SSND", &ssnd_body(9, &data)),
        ],
    ));
    assert!(result.is_err());
}

#[test]
fn aiff_reads_only_num_frames() {
    let samples = [1i16, 2, 3, 4, 5, 6];
    // 2 stereo frames declared, 3 in the chunk
    let file = aiff(form(
        b"AIFF",
        &[
            chunk_be(b"COMM", &comm_body(2, 2, 16, 44100)),
            chunk_be(b"SSND", &ssnd_body(0, &i16_be(&samples))),
        ],
    ))
    .unwrap();
    assert_eq!(f32_to_i16(&file.samples), vec![1, 2, 3, 4]);
}

#[test]
fn aiff_truncated_is_eof() {
    let bytes = form(
        b"AIFF",
        &[
            chunk_be(b"COMM", &comm_body(1, 4, 16, 44100)),
            chunk_be(b"SSND", &ssnd_body(0, &[0; 8])),
        ],
    );
    // cut inside COMM
    assert!(matches!(
        aiff(bytes[..20].to_vec()),
        Err(DecodeError::UnexpectedEof)
    ));
    // too short to recognise at all
    assert!(aiff(bytes[..6].to_vec()).is_err());
    // cut inside SSND (as the old parser did; WAV reads the whole frames)
    assert!(matches!(
        aiff(bytes[..bytes.len() - 3].to_vec()),
        Err(DecodeError::UnexpectedEof)
    ));
}

#[test]
fn aiff_tiny_ssnd_is_error() {
    for ssnd in [vec![0; 4], vec![]] {
        let result = aiff(form(
            b"AIFF",
            &[
                chunk_be(b"COMM", &comm_body(1, 1, 16, 44100)),
                chunk_be(b"SSND", &ssnd),
            ],
        ));
        assert!(result.is_err());
    }
}

#[test]
fn aiff_missing_chunks() {
    assert!(matches!(
        aiff(form(
            b"AIFF",
            &[chunk_be(b"COMM", &comm_body(1, 1, 16, 44100))]
        )),
        Err(DecodeError::InvalidData(_))
    ));
    assert!(matches!(
        aiff(form(b"AIFF", &[chunk_be(b"SSND", &ssnd_body(0, &[0, 1]))])),
        Err(DecodeError::InvalidData(_))
    ));
    assert!(matches!(
        aiff(form(
            b"AIFF",
            &[
                chunk_be(b"COMM", &[0, 1, 0, 0]),
                chunk_be(b"SSND", &ssnd_body(0, &[0, 1])),
            ],
        )),
        Err(DecodeError::InvalidData(_))
    ));
}

#[test]
fn aiff_wrong_magic() {
    let chunks = [
        chunk_be(b"COMM", &comm_body(1, 1, 16, 44100)),
        chunk_be(b"SSND", &ssnd_body(0, &[0, 1])),
    ];
    assert!(matches!(
        aiff(form(b"8SVX", &chunks)),
        Err(DecodeError::UnsupportedFormat(_))
    ));
    let mut bytes = form(b"AIFF", &chunks);
    bytes[0..4].copy_from_slice(b"RIFF");
    assert!(matches!(
        aiff(bytes),
        Err(DecodeError::UnsupportedFormat(_))
    ));
    // AIFC needs a longer COMM (compression type and name)
    assert!(aiff(form(b"AIFC", &chunks)).is_err());
}

#[test]
fn aiff_unsupported_sample_size() {
    let result = aiff(form(
        b"AIFF",
        &[
            chunk_be(b"COMM", &comm_body(1, 1, 40, 44100)),
            chunk_be(b"SSND", &ssnd_body(0, &[0; 5])),
        ],
    ));
    assert!(result.is_err());
}

#[test]
fn unknown_extension_is_unsupported() {
    for name in ["notes.txt", "cover.png", "noext", ".wav"] {
        assert!(matches!(
            decode_bytes(name, pcm16_wav()),
            Err(DecodeError::UnsupportedFormat(_))
        ));
    }
    assert!(!is_supported_extension("png"));
    assert!(is_supported_extension("flac"));
}

#[test]
fn garbage_with_audio_extension_is_error() {
    let mut sync_noise = Vec::new();
    for i in 0..256u32 {
        sync_noise.extend_from_slice(&[0xFF, 0xFB, (i * 37) as u8, (i * 11) as u8]);
    }
    assert!(decode_bytes("x.mp3", sync_noise).is_err());

    // xorshift, so the noise is the same every run
    let mut state = 0x2545_F491u32;
    let noise: Vec<u8> = (0..4096)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    for name in ["x.wav", "x.flac", "x.m4a", "x.aiff", "x.aac"] {
        assert!(decode_bytes(name, noise.clone()).is_err(), "{name}");
    }
}

#[test]
fn corrupt_flac_frame_is_filled() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/.fixtures/a.flac");
    let Ok(intact) = std::fs::read(path) else {
        return;
    };
    let frames = decode_bytes("a.flac", intact.clone())
        .unwrap()
        .samples
        .len();
    let mut corrupt = intact;
    let mid = corrupt.len() / 2;
    corrupt[mid] ^= 0xFF;
    let file = decode_bytes("a.flac", corrupt).unwrap();
    assert_eq!(file.samples.len(), frames);
}

#[test]
fn truncated_prefixes_never_panic() {
    let wav = pcm16_wav();
    let aiff = aiff_bytes(44100, 2, &SAMPLES);
    for n in 0..=wav.len() {
        let _ = decode_bytes("p.wav", wav[..n].to_vec());
    }
    for n in 0..=aiff.len() {
        let _ = decode_bytes("p.aif", aiff[..n].to_vec());
    }
}
