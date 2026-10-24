use std::fs::File;
use std::io::{self, Read, SeekFrom};
use std::ops::{Shl, BitOr, AddAssign};
use super::decode_helpers::{AudioFile, DecodeError, DecodeResult, pcm_to_i16, float_to_i16};

// format codes
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatCode {
    WaveFormatPcm = 0x0001,
    WaveFormatIeeeFloat = 0x0003,
    WaveFormatAlaw = 0x0006,
    WaveFormatMulaw = 0x0007,
    WaveFormatExtensible = 0xFFFE,
}

impl FormatCode {
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            0x0001 => Some(Self::WaveFormatPcm),
            0x0003 => Some(Self::WaveFormatIeeeFloat),
            0x0006 => Some(Self::WaveFormatAlaw),
            0x0007 => Some(Self::WaveFormatMulaw),
            0xFFFE => Some(Self::WaveFormatExtensible),
            _ => None,
        }
    }
}

pub fn read_id(vec: &mut Vec<u8>, start: &mut usize, end: &mut usize) -> DecodeResult<[u8; 4]> {
    *end += 4;

    let id: [u8; 4] = match vec.get(*start..*end) {
        Some(val) => [val[0], val[1], val[2], val[3]],
        None => return Err(DecodeError::UnexpectedEof),
    };

    *start = *end;

    Ok(id)
}

fn parse_bytes(bytes: &mut Vec<u8>, start: &mut usize, end: &mut usize, inc: usize) -> DecodeResult<u32> {
    let mut value: u32 = 0;

    *end += inc;

    // little-endian
    let mut shift: u32 = 0;
    for i in *start..*end {
        let b: u8 = match bytes.get(i) {
            Some(val) => *val,
            None => return Err(DecodeError::UnexpectedEof),
        };
    
        value += (b as u32) << shift;

        shift += 8;
    }

    *start = *end;

    Ok(value)
}

pub fn parse(path: &str) -> DecodeResult<AudioFile> {
    let mut f = File::open(path)?;
    let mut reader = Vec::new();
    f.read_to_end(&mut reader)?;

    let mut start: usize= 0;
    let mut end: usize = 0;

    // RIFF
    // (read_id always increments end by four before reading
    //  and sets start to end afterward)
    let riff_id = read_id(&mut reader, &mut start, &mut end)?;

    // (parse_bytes increments end by the integer argument
    //  before decoding the reader from start to end
    //  and sets start to end afterward))
    // riff size is often wrong in the wild, so chunks are bounded by the file instead
    parse_bytes(&mut reader, &mut start, &mut end, 4)?;

    // WAVE
    let wave_id = read_id(&mut reader, &mut start, &mut end)?;

    if &riff_id != b"RIFF" || &wave_id != b"WAVE" {
        return Err(DecodeError::InvalidData("Not a RIFF/WAVE file".to_string()));
    }

    // walk chunks until both "fmt " and "data" are found;
    // each is (start of chunk data, chunk size)
    let mut fmt_chunk: Option<(usize, u32)> = None;
    let mut data_chunk: Option<(usize, u32)> = None;

    while start + 8 <= reader.len() && (fmt_chunk.is_none() || data_chunk.is_none()) {
        let id = read_id(&mut reader, &mut start, &mut end)?;
        let size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

        match &id {
            b"fmt " => fmt_chunk = Some((start, size)),
            b"data" => data_chunk = Some((start, size)),
            // LIST, fact, bext, JUNK, cue , ...
            _ => {}
        }

        if (&id == b"fmt " || &id == b"data") && start + size as usize > reader.len() {
            return Err(DecodeError::UnexpectedEof);
        }

        // chunks are padded to an even size
        end = start + size as usize + (size & 1) as usize;
        start = end;
    }

    let Some((fmt_start, fmt_size)) = fmt_chunk else {
        return Err(DecodeError::InvalidData("Missing fmt chunk".to_string()));
    };
    let Some((data_start, data_size)) = data_chunk else {
        return Err(DecodeError::InvalidData("Missing data chunk".to_string()));
    };

    if fmt_size < 16 {
        return Err(DecodeError::InvalidData(format!("fmt chunk too small ({fmt_size} bytes)")));
    }

    start = fmt_start;
    end = fmt_start;

    let Some(fmt_tag) = FormatCode::from_u16(parse_bytes(&mut reader, &mut start, &mut end, 2)?.try_into().unwrap())
    else {
        return Err(DecodeError::UnsupportedFormat(String::from("Unrecognized format tag")));
    };
    
    let num_channels: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;
    
    let sample_rate: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

    let data_rate: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

    let data_blk_sz: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

    let bits_per_sample: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

    // extensible files carry the real format in the SubFormat GUID
    let mut sample_fmt: FormatCode = fmt_tag;
    let mut file_bits: u32 = bits_per_sample;

    // if !WaveFormatPcm (i.e. is extensible)
    if fmt_size >= 18 {
        // extension is either 0 or 22
        let cb_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

        if cb_size >= 22 && fmt_size >= 40 {
            let valid_bits: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

            let dw_channel_mask: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

            let old_fmt: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

            // rest of the SubFormat GUID is ignored
            // TODO: compare against audio media subtype
            if fmt_tag == FormatCode::WaveFormatExtensible {
                sample_fmt = match FormatCode::from_u16(old_fmt as u16) {
                    Some(code) => code,
                    None => return Err(DecodeError::UnsupportedFormat(format!("Unrecognized SubFormat {old_fmt:#06x}"))),
                };
                if valid_bits > 0 {
                    file_bits = valid_bits;
                }
            }
        }
    }

    //
    // TODO: parse "fact" chunk if non-PCM (and if exists)
    //

    // "data"
    start = data_start;
    end = data_start + data_size as usize;
    let data: &[u8] = match reader.get(start..end) {
        Some(val) => val,
        None => return Err(DecodeError::UnexpectedEof),
    };

    // container size (bits_per_sample) decides how many bytes each sample takes
    let samples: Vec<i16> = match sample_fmt {
        // 8-bit WAV is unsigned
        FormatCode::WaveFormatPcm => pcm_to_i16(data, bits_per_sample, false, true)?,
        FormatCode::WaveFormatIeeeFloat => float_to_i16(data, bits_per_sample, false)?,
        other => return Err(DecodeError::UnsupportedFormat(format!("{:?}", other))),
    };

    let file_name: &str = match path.rsplit_once(|b: char| b == '.') {
        Some((before, after)) if !before.is_empty() && !after.is_empty() => {
            match before.rsplit_once(|b: char| b == '/') {
                Some((assets, name)) => name,
                None => return Err(DecodeError::InvalidData("File is not nested".to_string())),
            }
        }
        _ => return Err(DecodeError::InvalidData("File has no name".to_string())),
    };

    Ok(AudioFile::new(file_name, "wav", sample_rate, num_channels, file_bits, samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    // little-endian chunk: id + size + body (+ pad byte if odd)
    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        if body.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = chunks.concat();
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(&body);
        v
    }

    fn fmt_body(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
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

    // WAVE_FORMAT_EXTENSIBLE fmt body (40 bytes, cbSize 22)
    fn fmt_ext_body(sub_tag: u16, channels: u16, rate: u32, bits: u16, valid_bits: u16) -> Vec<u8> {
        let mut v = fmt_body(0xFFFE, channels, rate, bits);
        v.extend_from_slice(&22u16.to_le_bytes());
        v.extend_from_slice(&valid_bits.to_le_bytes());
        v.extend_from_slice(&3u32.to_le_bytes());
        // KSDATAFORMAT_SUBTYPE GUID: first 2 bytes are the format tag
        v.extend_from_slice(&sub_tag.to_le_bytes());
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]);
        v
    }

    fn i16_data(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn parse_temp(name: &str, bytes: &[u8]) -> DecodeResult<AudioFile> {
        let path = std::env::temp_dir().join(format!("blast_wav_{}_{}.wav", name, std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        let result = parse(path.to_str().unwrap());
        std::fs::remove_file(&path).unwrap();
        result
    }

    const SAMPLES: [i16; 6] = [0, 1, -1, i16::MAX, i16::MIN, 1234];

    #[test]
    fn parses_pcm_wav() {
        let bytes = riff(&[chunk(b"fmt ", &fmt_body(1, 2, 48000, 16)), chunk(b"data", &i16_data(&SAMPLES))]);
        let file = parse_temp("pcm", &bytes).unwrap();
        assert_eq!(file.sample_rate, 48000);
        assert_eq!(file.num_channels, 2);
        assert_eq!(file.bits_per_sample, 16);
        assert_eq!(file.samples, SAMPLES.to_vec());
        assert_eq!(file.format, "wav");
        assert!(file.file_name.starts_with("blast_wav_pcm_"));
    }

    #[test]
    fn parses_extensible_wav() {
        let bytes = riff(&[chunk(b"fmt ", &fmt_ext_body(1, 2, 44100, 16, 16)), chunk(b"data", &i16_data(&SAMPLES))]);
        let file = parse_temp("ext", &bytes).unwrap();
        assert_eq!(file.sample_rate, 44100);
        assert_eq!(file.num_channels, 2);
        assert_eq!(file.bits_per_sample, 16);
        assert_eq!(file.samples, SAMPLES.to_vec());
    }

    #[test]
    fn parses_fmt_18_with_zero_cb_size() {
        let mut fmt = fmt_body(1, 1, 48000, 16);
        fmt.extend_from_slice(&0u16.to_le_bytes());
        let bytes = riff(&[chunk(b"fmt ", &fmt), chunk(b"data", &i16_data(&SAMPLES))]);
        let file = parse_temp("fmt18", &bytes).unwrap();
        assert_eq!(file.num_channels, 1);
        assert_eq!(file.samples, SAMPLES.to_vec());
    }

    #[test]
    fn truncated_file_is_eof() {
        let bytes = riff(&[chunk(b"fmt ", &fmt_body(1, 2, 48000, 16)), chunk(b"data", &i16_data(&SAMPLES))]);
        let cut = &bytes[..bytes.len() - 3];
        assert!(matches!(parse_temp("trunc", cut), Err(DecodeError::UnexpectedEof)));
        assert!(matches!(parse_temp("trunc_hdr", &bytes[..20]), Err(DecodeError::UnexpectedEof)));
    }

    // full-scale positive, full-scale negative, zero
    const EXPECTED: [i16; 3] = [i16::MAX, i16::MIN, 0];

    fn depth_file(name: &str, fmt: Vec<u8>, data: &[u8]) -> AudioFile {
        parse_temp(name, &riff(&[chunk(b"fmt ", &fmt), chunk(b"data", data)])).unwrap()
    }

    #[test]
    fn converts_8_bit_unsigned() {
        let file = depth_file("u8", fmt_body(1, 1, 48000, 8), &[0xFF, 0x00, 0x80]);
        assert_eq!(file.samples, vec![32512, i16::MIN, 0]);
        assert_eq!(file.bits_per_sample, 8);
    }

    #[test]
    fn converts_24_bit() {
        let file = depth_file("s24", fmt_body(1, 1, 48000, 24), &[0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00]);
        assert_eq!(file.samples, EXPECTED.to_vec());
        assert_eq!(file.bits_per_sample, 24);
    }

    #[test]
    fn converts_32_bit_int() {
        let data: Vec<u8> = [i32::MAX, i32::MIN, 0].iter().flat_map(|s| s.to_le_bytes()).collect();
        let file = depth_file("s32", fmt_body(1, 1, 48000, 32), &data);
        assert_eq!(file.samples, EXPECTED.to_vec());
    }

    #[test]
    fn converts_32_bit_float() {
        let data: Vec<u8> = [1.0f32, -1.0, 0.0].iter().flat_map(|s| s.to_le_bytes()).collect();
        let file = depth_file("f32", fmt_body(3, 1, 48000, 32), &data);
        assert_eq!(file.samples, EXPECTED.to_vec());
    }

    #[test]
    fn extensible_uses_sub_format_and_valid_bits() {
        let data = [0xF0, 0xFF, 0x7F, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00];
        let file = depth_file("ext24", fmt_ext_body(1, 1, 48000, 24, 20), &data);
        assert_eq!(file.samples, EXPECTED.to_vec());
        assert_eq!(file.bits_per_sample, 20);

        let data: Vec<u8> = [1.0f32, -1.0, 0.0].iter().flat_map(|s| s.to_le_bytes()).collect();
        let file = depth_file("extf32", fmt_ext_body(3, 2, 48000, 32, 32), &data);
        assert_eq!(file.samples, EXPECTED.to_vec());
    }

    #[test]
    fn unsupported_formats_are_errors() {
        let bytes = riff(&[chunk(b"fmt ", &fmt_body(6, 1, 8000, 8)), chunk(b"data", &[0xD5])]);
        assert!(matches!(parse_temp("alaw", &bytes), Err(DecodeError::UnsupportedFormat(_))));
        let bytes = riff(&[chunk(b"fmt ", &fmt_ext_body(7, 1, 8000, 8, 8)), chunk(b"data", &[0xFF])]);
        assert!(matches!(parse_temp("ext_mulaw", &bytes), Err(DecodeError::UnsupportedFormat(_))));
        let bytes = riff(&[chunk(b"fmt ", &fmt_body(3, 1, 48000, 16)), chunk(b"data", &[0, 0])]);
        assert!(matches!(parse_temp("f16", &bytes), Err(DecodeError::UnsupportedFormat(_))));
        let bytes = riff(&[chunk(b"fmt ", &fmt_body(0x55, 1, 48000, 16)), chunk(b"data", &[0, 0])]);
        assert!(matches!(parse_temp("mp3tag", &bytes), Err(DecodeError::UnsupportedFormat(_))));
    }

    #[test]
    fn skips_list_chunk_between_fmt_and_data() {
        let bytes = riff(&[
            chunk(b"fmt ", &fmt_body(1, 2, 48000, 16)),
            chunk(b"LIST", b"INFOISFT\x05\x00\x00\x00blast\x00"),
            chunk(b"data", &i16_data(&SAMPLES)),
        ]);
        assert_eq!(parse_temp("list", &bytes).unwrap().samples, SAMPLES.to_vec());
    }

    #[test]
    fn honours_pad_byte_of_odd_sized_chunk() {
        let bytes = riff(&[
            chunk(b"JUNK", &[1, 2, 3]),
            chunk(b"fmt ", &fmt_body(1, 1, 48000, 16)),
            chunk(b"bext", &[9; 7]),
            chunk(b"data", &i16_data(&SAMPLES)),
        ]);
        // odd chunks really are padded in the test data
        assert_eq!(bytes.len(), 12 + (8 + 4) + (8 + 16) + (8 + 8) + (8 + 12));
        let file = parse_temp("pad", &bytes).unwrap();
        assert_eq!(file.num_channels, 1);
        assert_eq!(file.samples, SAMPLES.to_vec());
    }

    #[test]
    fn data_before_fmt() {
        let bytes = riff(&[chunk(b"data", &i16_data(&SAMPLES)), chunk(b"fmt ", &fmt_body(1, 2, 44100, 16))]);
        let file = parse_temp("datafirst", &bytes).unwrap();
        assert_eq!(file.sample_rate, 44100);
        assert_eq!(file.samples, SAMPLES.to_vec());
    }

    #[test]
    fn wrong_magic_is_invalid() {
        let mut bytes = riff(&[chunk(b"fmt ", &fmt_body(1, 2, 48000, 16)), chunk(b"data", &i16_data(&SAMPLES))]);
        bytes[8..12].copy_from_slice(b"AVI ");
        assert!(matches!(parse_temp("avi", &bytes), Err(DecodeError::InvalidData(_))));
        bytes[0..4].copy_from_slice(b"FORM");
        bytes[8..12].copy_from_slice(b"WAVE");
        assert!(matches!(parse_temp("form", &bytes), Err(DecodeError::InvalidData(_))));
    }

    #[test]
    fn missing_chunks_are_errors() {
        let bytes = riff(&[chunk(b"fmt ", &fmt_body(1, 2, 48000, 16)), chunk(b"LIST", &[0; 4])]);
        assert!(matches!(parse_temp("nodata", &bytes), Err(DecodeError::InvalidData(_))));
        let bytes = riff(&[chunk(b"data", &i16_data(&SAMPLES))]);
        assert!(matches!(parse_temp("nofmt", &bytes), Err(DecodeError::InvalidData(_))));
        let bytes = riff(&[chunk(b"fmt ", &[1, 0, 2, 0]), chunk(b"data", &i16_data(&SAMPLES))]);
        assert!(matches!(parse_temp("shortfmt", &bytes), Err(DecodeError::InvalidData(_))));
    }

    #[test]
    fn parse_bytes_little_endian() {
        let mut bytes = vec![0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0xFF];
        let (mut start, mut end) = (0, 0);
        assert_eq!(parse_bytes(&mut bytes, &mut start, &mut end, 2).unwrap(), 0x1234);
        assert_eq!((start, end), (2, 2));
        assert_eq!(parse_bytes(&mut bytes, &mut start, &mut end, 4).unwrap(), 0x12345678);
        assert_eq!((start, end), (6, 6));
        assert!(matches!(parse_bytes(&mut bytes, &mut start, &mut end, 2), Err(DecodeError::UnexpectedEof)));
    }

    #[test]
    fn parses_real_assets() {
        for name in ["kick", "rim1", "shaker", "snare"] {
            let path = format!("{}/assets/{}.wav", env!("CARGO_MANIFEST_DIR"), name);
            let file = parse(&path).unwrap();
            assert_eq!(file.file_name, name);
            assert_eq!(file.sample_rate, 48000);
            assert_eq!(file.num_channels, 2);
            assert_eq!(file.bits_per_sample, 16);
            assert_eq!(file.samples.len(), 72000);
        }
    }
}
