use super::decode_helpers::{AudioFile, DecodeError, DecodeResult, file_stem, pcm_to_i16};
use std::fs::File;
use std::io::Read;

fn read_id(vec: &mut Vec<u8>, start: &mut usize, end: &mut usize) -> DecodeResult<[u8; 4]> {
    *end += 4;

    let id: [u8; 4] = match vec.get(*start..*end) {
        Some(val) => [val[0], val[1], val[2], val[3]],
        None => return Err(DecodeError::UnexpectedEof),
    };

    *start = *end;

    Ok(id)
}

fn parse_bytes(
    bytes: &mut Vec<u8>,
    start: &mut usize,
    end: &mut usize,
    inc: usize,
) -> DecodeResult<u32> {
    let mut value: u32 = 0;

    *end += inc;

    // big-endian
    let mut shift: u32 = 8 * inc as u32 - 8;
    for i in *start..*end {
        let b: u8 = match bytes.get(i) {
            Some(val) => *val,
            None => return Err(DecodeError::UnexpectedEof),
        };

        value += (b as u32) << shift;

        if shift >= 8 {
            shift -= 8;
        }
    }

    *start = *end;

    Ok(value)
}
//
// special function to parse IEEE 80-bit extended floating-point
fn parse_ieee_extended(
    reader: &mut Vec<u8>,
    start: &mut usize,
    end: &mut usize,
) -> DecodeResult<f64> {
    let mut bytes = [0u8; 10];
    *end += 10;
    let mut i = 0;
    for byte in *start..*end {
        let b = match reader.get(byte) {
            Some(val) => val,
            None => return Err(DecodeError::UnexpectedEof),
        };
        bytes[i] = *b;
        i += 1;
    }
    *start = *end;

    let sign = (bytes[0] & 0x80) != 0;
    let exp = (((bytes[0] & 0x7F) as u16) << 8) | bytes[1] as u16;

    // 64-bit mantissa (explicit integer bit at bit 63)
    let mut mant: u64 = 0;
    for &b in &bytes[2..] {
        mant = (mant << 8) | b as u64;
    }

    // Zero
    if exp == 0 && mant == 0 {
        return Ok(0.0);
    }

    // Inf/NaN
    if exp == 0x7FFF {
        return if mant == 0 {
            if sign {
                Ok(f64::NEG_INFINITY)
            } else {
                Ok(f64::INFINITY)
            }
        } else {
            Ok(f64::NAN)
        };
    }

    // value = mantissa * 2^(exp - 16383 - 63)
    let e = (exp as i32) - 16383 - 63;
    let mut val = (mant as f64) * 2f64.powi(e);
    if sign {
        val = -val;
    }

    Ok(val)
}

// only care about COMM and SSND chunks,
// so adjust this to search only for those and
// extract the relevant information
pub fn parse(path: &str) -> DecodeResult<AudioFile> {
    let mut f = File::open(path)?;
    let mut reader = Vec::new();
    f.read_to_end(&mut reader)?;

    let mut start = 0;
    let mut end = 0;

    // FORM
    let form_id = read_id(&mut reader, &mut start, &mut end)?;

    // form size is often wrong in the wild, so chunks are bounded by the file instead
    parse_bytes(&mut reader, &mut start, &mut end, 4)?;

    // AIFF
    let aiff_id = read_id(&mut reader, &mut start, &mut end)?;

    if &form_id != b"FORM" {
        return Err(DecodeError::InvalidData("Not an IFF FORM file".to_string()));
    }
    match &aiff_id {
        b"AIFF" => {}
        b"AIFC" => return Err(DecodeError::UnsupportedFormat("AIFF-C".to_string())),
        _ => return Err(DecodeError::InvalidData("Not an AIFF file".to_string())),
    }

    // walk chunks until both COMM and SSND are found;
    // each is (start of chunk data, chunk size)
    let mut comm_chunk: Option<(usize, u32)> = None;
    let mut ssnd_chunk: Option<(usize, u32)> = None;

    while start + 8 <= reader.len() && (comm_chunk.is_none() || ssnd_chunk.is_none()) {
        let id = read_id(&mut reader, &mut start, &mut end)?;
        let size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

        match &id {
            b"COMM" => comm_chunk = Some((start, size)),
            b"SSND" => ssnd_chunk = Some((start, size)),
            // MARK, INST, APPL, COMT, ...
            _ => {}
        }

        if (&id == b"COMM" || &id == b"SSND") && start + size as usize > reader.len() {
            return Err(DecodeError::UnexpectedEof);
        }

        // chunks are padded to an even size
        end = start + size as usize + (size & 1) as usize;
        start = end;
    }

    let Some((comm_start, comm_size)) = comm_chunk else {
        return Err(DecodeError::InvalidData("Missing COMM chunk".to_string()));
    };
    let Some((ssnd_start, ssnd_chunk_size)) = ssnd_chunk else {
        return Err(DecodeError::InvalidData("Missing SSND chunk".to_string()));
    };

    // anything past the first 18 bytes is skipped
    if comm_size < 18 {
        return Err(DecodeError::InvalidData(format!(
            "COMM chunk too small ({comm_size} bytes)"
        )));
    }

    // COMM
    start = comm_start;
    end = comm_start;

    let num_channels: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

    let num_frames: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

    let sample_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

    let sample_rate: f64 = parse_ieee_extended(&mut reader, &mut start, &mut end)?;

    // SSND
    start = ssnd_start;
    end = ssnd_start;

    // size of the sample data after offset and block_size
    let Some(ssnd_size) = ssnd_chunk_size.checked_sub(8) else {
        return Err(DecodeError::InvalidData(format!(
            "SSND chunk too small ({ssnd_chunk_size} bytes)"
        )));
    };

    // bytes to skip before the first sample; typically 0
    let offset: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;
    // also typically 0
    let _block_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

    if offset > ssnd_size {
        return Err(DecodeError::InvalidData(format!(
            "SSND offset {offset} exceeds data size {ssnd_size}"
        )));
    }

    start += offset as usize;
    end += ssnd_size as usize;
    let data: &[u8] = match reader.get(start..end) {
        Some(val) => val,
        None => return Err(DecodeError::UnexpectedEof),
    };

    // never read past the frames COMM declares
    let frame_bytes: usize = (num_frames as usize)
        .saturating_mul(num_channels as usize)
        .saturating_mul(sample_size.div_ceil(8) as usize);
    let data: &[u8] = &data[..data.len().min(frame_bytes)];

    // 8-bit AIFF is signed
    let samples: Vec<i16> = pcm_to_i16(data, sample_size, true, false)?;

    let file_name = file_stem(path)?;

    Ok(AudioFile::new(
        file_name,
        "aiff",
        sample_rate as u32,
        num_channels,
        sample_size,
        samples,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // big-endian chunk: id + size + body (+ pad byte if odd)
    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(body.len() as u32).to_be_bytes());
        v.extend_from_slice(body);
        if body.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    fn form(kind: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = chunks.concat();
        let mut v = b"FORM".to_vec();
        v.extend_from_slice(&(body.len() as u32 + 4).to_be_bytes());
        v.extend_from_slice(kind);
        v.extend_from_slice(&body);
        v
    }

    // 80-bit IEEE extended for a positive integer rate
    fn extended(rate: u32) -> [u8; 10] {
        let mant = rate as u64;
        let shift = mant.leading_zeros();
        let exp = 16383 + 63 - shift as u16;
        let mut v = [0u8; 10];
        v[..2].copy_from_slice(&exp.to_be_bytes());
        v[2..].copy_from_slice(&(mant << shift).to_be_bytes());
        v
    }

    fn comm_body(channels: u16, frames: u32, bits: u16, rate: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&channels.to_be_bytes());
        v.extend_from_slice(&frames.to_be_bytes());
        v.extend_from_slice(&bits.to_be_bytes());
        v.extend_from_slice(&extended(rate));
        v
    }

    fn ssnd_body(offset: u32, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&offset.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    fn parse_temp(name: &str, bytes: &[u8]) -> DecodeResult<AudioFile> {
        let path =
            std::env::temp_dir().join(format!("blast_aiff_{}_{}.aif", name, std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        let result = parse(path.to_str().unwrap());
        std::fs::remove_file(&path).unwrap();
        result
    }

    fn depth_file(name: &str, channels: u16, bits: u16, data: &[u8]) -> AudioFile {
        let width = (bits as usize).div_ceil(8);
        let frames = (data.len() / width / channels as usize) as u32;
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &comm_body(channels, frames, bits, 44100)),
                chunk(b"SSND", &ssnd_body(0, data)),
            ],
        );
        parse_temp(name, &bytes).unwrap()
    }

    // full-scale positive, full-scale negative, zero
    const EXPECTED: [i16; 3] = [i16::MAX, i16::MIN, 0];

    #[test]
    fn parses_16_bit_aiff() {
        let samples = [0i16, 1, -1, i16::MAX, i16::MIN, 1234];
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_be_bytes()).collect();
        let file = depth_file("s16", 2, 16, &data);
        assert_eq!(file.sample_rate, 44100);
        assert_eq!(file.num_channels, 2);
        assert_eq!(file.bits_per_sample, 16);
        assert_eq!(file.format, "aiff");
        assert_eq!(file.samples, samples.to_vec());
    }

    #[test]
    fn converts_8_bit_signed() {
        let file = depth_file("s8", 1, 8, &[0x7F, 0x80, 0x00]);
        assert_eq!(file.samples, vec![32512, i16::MIN, 0]);
    }

    #[test]
    fn converts_24_bit() {
        let file = depth_file(
            "s24",
            1,
            24,
            &[0x7F, 0xFF, 0xFF, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00],
        );
        assert_eq!(file.samples, EXPECTED.to_vec());
        assert_eq!(file.bits_per_sample, 24);
    }

    #[test]
    fn converts_32_bit() {
        let data: Vec<u8> = [i32::MAX, i32::MIN, 0]
            .iter()
            .flat_map(|s| s.to_be_bytes())
            .collect();
        let file = depth_file("s32", 1, 32, &data);
        assert_eq!(file.samples, EXPECTED.to_vec());
    }

    #[test]
    fn skips_chunks_before_ssnd() {
        let samples = [5i16, -5, i16::MAX, i16::MIN];
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_be_bytes()).collect();
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &comm_body(2, 2, 16, 48000)),
                chunk(b"MARK", &[0, 0]),
                chunk(b"COMT", &[1, 2, 3]),
                chunk(b"SSND", &ssnd_body(0, &data)),
            ],
        );
        let file = parse_temp("mark", &bytes).unwrap();
        assert_eq!(file.sample_rate, 48000);
        assert_eq!(file.samples, samples.to_vec());
    }

    #[test]
    fn ssnd_before_comm_and_larger_comm() {
        let samples = [7i16, -7];
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_be_bytes()).collect();
        let mut comm = comm_body(1, 2, 16, 44100);
        comm.extend_from_slice(&[0xAA; 4]);
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"SSND", &ssnd_body(0, &data)),
                chunk(b"APPL", &[0; 6]),
                chunk(b"COMM", &comm),
            ],
        );
        let file = parse_temp("ssndfirst", &bytes).unwrap();
        assert_eq!(file.num_channels, 1);
        assert_eq!(file.samples, samples.to_vec());
    }

    #[test]
    fn wrong_magic_is_rejected() {
        let chunks = [
            chunk(b"COMM", &comm_body(1, 1, 16, 44100)),
            chunk(b"SSND", &ssnd_body(0, &[0, 1])),
        ];
        assert!(matches!(
            parse_temp("aifc", &form(b"AIFC", &chunks)),
            Err(DecodeError::UnsupportedFormat(_))
        ));
        assert!(matches!(
            parse_temp("8svx", &form(b"8SVX", &chunks)),
            Err(DecodeError::InvalidData(_))
        ));
        let mut bytes = form(b"AIFF", &chunks);
        bytes[0..4].copy_from_slice(b"RIFF");
        assert!(matches!(
            parse_temp("riff", &bytes),
            Err(DecodeError::InvalidData(_))
        ));
    }

    #[test]
    fn missing_chunks_are_errors() {
        let bytes = form(b"AIFF", &[chunk(b"COMM", &comm_body(1, 1, 16, 44100))]);
        assert!(matches!(
            parse_temp("nossnd", &bytes),
            Err(DecodeError::InvalidData(_))
        ));
        let bytes = form(b"AIFF", &[chunk(b"SSND", &ssnd_body(0, &[0, 1]))]);
        assert!(matches!(
            parse_temp("nocomm", &bytes),
            Err(DecodeError::InvalidData(_))
        ));
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &[0, 1, 0, 0]),
                chunk(b"SSND", &ssnd_body(0, &[0, 1])),
            ],
        );
        assert!(matches!(
            parse_temp("shortcomm", &bytes),
            Err(DecodeError::InvalidData(_))
        ));
    }

    #[test]
    fn truncated_file_is_eof() {
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &comm_body(1, 4, 16, 44100)),
                chunk(b"SSND", &ssnd_body(0, &[0; 8])),
            ],
        );
        assert!(matches!(
            parse_temp("trunc", &bytes[..bytes.len() - 3]),
            Err(DecodeError::UnexpectedEof)
        ));
        assert!(matches!(
            parse_temp("trunc_hdr", &bytes[..6]),
            Err(DecodeError::UnexpectedEof)
        ));
    }

    #[test]
    fn tiny_ssnd_is_error_not_panic() {
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &comm_body(1, 1, 16, 44100)),
                chunk(b"SSND", &[0; 4]),
            ],
        );
        assert!(matches!(
            parse_temp("ssnd4", &bytes),
            Err(DecodeError::InvalidData(_))
        ));
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &comm_body(1, 1, 16, 44100)),
                chunk(b"SSND", &[]),
            ],
        );
        assert!(matches!(
            parse_temp("ssnd0", &bytes),
            Err(DecodeError::InvalidData(_))
        ));
    }

    #[test]
    fn ssnd_offset_is_skipped() {
        let mut data = vec![0xEE; 4];
        data.extend_from_slice(&[0x12, 0x34, 0x80, 0x00]);
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &comm_body(1, 2, 16, 44100)),
                chunk(b"SSND", &ssnd_body(4, &data)),
            ],
        );
        assert_eq!(
            parse_temp("offset", &bytes).unwrap().samples,
            vec![0x1234, i16::MIN]
        );

        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &comm_body(1, 2, 16, 44100)),
                chunk(b"SSND", &ssnd_body(9, &data)),
            ],
        );
        assert!(matches!(
            parse_temp("bigoffset", &bytes),
            Err(DecodeError::InvalidData(_))
        ));
    }

    #[test]
    fn reads_only_num_frames() {
        let samples = [1i16, 2, 3, 4, 5, 6];
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_be_bytes()).collect();
        // 2 stereo frames declared, 3 in the chunk
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &comm_body(2, 2, 16, 44100)),
                chunk(b"SSND", &ssnd_body(0, &data)),
            ],
        );
        assert_eq!(
            parse_temp("frames", &bytes).unwrap().samples,
            vec![1, 2, 3, 4]
        );
        // more frames declared than present: read what is there
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &comm_body(2, 10, 16, 44100)),
                chunk(b"SSND", &ssnd_body(0, &data)),
            ],
        );
        assert_eq!(
            parse_temp("frames_over", &bytes).unwrap().samples,
            samples.to_vec()
        );
    }

    #[test]
    fn ieee_extended_rates() {
        let known: [(f64, [u8; 10]); 3] = [
            (44100.0, [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]),
            (48000.0, [0x40, 0x0E, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]),
            (96000.0, [0x40, 0x0F, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]),
        ];
        for (rate, bytes) in known {
            assert_eq!(extended(rate as u32), bytes);
            let mut v = bytes.to_vec();
            let (mut start, mut end) = (0, 0);
            assert_eq!(
                parse_ieee_extended(&mut v, &mut start, &mut end).unwrap(),
                rate
            );
            assert_eq!((start, end), (10, 10));
        }
        let mut short = vec![0x40, 0x0E, 0xAC];
        assert!(matches!(
            parse_ieee_extended(&mut short, &mut 0, &mut 0),
            Err(DecodeError::UnexpectedEof)
        ));
    }

    #[test]
    fn parse_bytes_big_endian() {
        let mut bytes = vec![0x12, 0x34, 0x12, 0x34, 0x56, 0x78, 0xFF];
        let (mut start, mut end) = (0, 0);
        assert_eq!(
            parse_bytes(&mut bytes, &mut start, &mut end, 2).unwrap(),
            0x1234
        );
        assert_eq!(
            parse_bytes(&mut bytes, &mut start, &mut end, 4).unwrap(),
            0x12345678
        );
        assert_eq!((start, end), (6, 6));
        assert!(matches!(
            parse_bytes(&mut bytes, &mut start, &mut end, 2),
            Err(DecodeError::UnexpectedEof)
        ));
    }

    #[test]
    fn unsupported_sample_size_is_error() {
        let bytes = form(
            b"AIFF",
            &[
                chunk(b"COMM", &comm_body(1, 1, 40, 44100)),
                chunk(b"SSND", &ssnd_body(0, &[0; 5])),
            ],
        );
        assert!(matches!(
            parse_temp("s40", &bytes),
            Err(DecodeError::UnsupportedFormat(_))
        ));
    }
}
