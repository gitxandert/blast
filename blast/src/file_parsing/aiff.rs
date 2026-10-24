use std::fs::File;
use std::io::{self, Read, SeekFrom};
use std::ops::{Shl, BitOr, AddAssign};
use super::decode_helpers::{AudioFile, DecodeResult, DecodeError, pcm_to_i16};

fn print_id(vec: &mut Vec<u8>, start: &mut usize, end: &mut usize) -> DecodeResult<()> {
    *end += 4;

    for i in *start..*end {
        let c = match vec.get(i) {
            Some(val) => val,
            None => return Err(DecodeError::UnexpectedEof),
        };

        print!("{}", char::from(*c));
    }

    *start = *end;

    println!("");

    Ok(())
}

fn parse_bytes(bytes: &mut Vec<u8>, start: &mut usize, end: &mut usize, inc: usize) -> DecodeResult<u32> {
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
fn parse_ieee_extended(reader: &mut Vec<u8>, start: &mut usize, end: &mut usize) -> DecodeResult<f64> {
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
            if sign { Ok(f64::NEG_INFINITY) } else { Ok(f64::INFINITY) }
        } else {
            Ok(f64::NAN)
        };
    }

    // value = mantissa * 2^(exp - 16383 - 63)
    let e = (exp as i32) - 16383 - 63;
    let mut val = (mant as f64) * 2f64.powi(e);
    if sign { val = -val; }

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
    print_id(&mut reader, &mut start, &mut end)?;

    let form_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;
    println!("Form size: {form_size}");

    // AIFF
    print_id(&mut reader, &mut start, &mut end)?;
    
    println!("");

    // COMM
    print_id(&mut reader, &mut start, &mut end)?;

    let comm_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;
    if comm_size == 18 {
        println!("Comm size: {comm_size}");
    } else {
        return Err(DecodeError::InvalidData("Comm size should be 18".to_string()));
    }

    let num_channels: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;
    println!("Num channels: {num_channels}");

    let num_frames: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;
    println!("Num sample frames: {num_frames}");

    let sample_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;
    println!("Sample size: {sample_size}");


    let sample_rate: f64 = parse_ieee_extended(&mut reader, &mut start, &mut end)?;
    println!("Sample rate: {sample_rate}");
    
    println!("");

    // SSND
    print_id(&mut reader, &mut start, &mut end)?;

    let ssnd_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)? - 8;
    println!("Data size: {ssnd_size}");

    // typically 0
    let offset: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;
    println!("Offset: {offset}");
    // also typically 0
    let block_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;
    println!("Block size: {block_size}");

    end += ssnd_size as usize;
    let data: &[u8] = match reader.get(start..end) {
        Some(val) => val,
        None => return Err(DecodeError::UnexpectedEof),
    };

    // 8-bit AIFF is signed
    let samples: Vec<i16> = pcm_to_i16(data, sample_size, true, false)?;

    let file_name: &str = match path.rsplit_once(|b: char| b == '.') {
        Some((before, after)) if !before.is_empty() && !after.is_empty() => {
            match before.rsplit_once(|b: char| b == '/') {
                Some((assets, name)) => name,
                None => return Err(DecodeError::InvalidData("File is not nested".to_string())),
            }
        }
        _ => return Err(DecodeError::InvalidData("File has no name".to_string())),
    };

    Ok(AudioFile::new(file_name, "aiff", sample_rate as u32, num_channels, sample_size, samples))
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
        let path = std::env::temp_dir().join(format!("blast_aiff_{}_{}.aif", name, std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        let result = parse(path.to_str().unwrap());
        std::fs::remove_file(&path).unwrap();
        result
    }

    fn depth_file(name: &str, channels: u16, bits: u16, data: &[u8]) -> AudioFile {
        let width = (bits as usize).div_ceil(8);
        let frames = (data.len() / width / channels as usize) as u32;
        let bytes = form(b"AIFF", &[
            chunk(b"COMM", &comm_body(channels, frames, bits, 44100)),
            chunk(b"SSND", &ssnd_body(0, data)),
        ]);
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
        let file = depth_file("s24", 1, 24, &[0x7F, 0xFF, 0xFF, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(file.samples, EXPECTED.to_vec());
        assert_eq!(file.bits_per_sample, 24);
    }

    #[test]
    fn converts_32_bit() {
        let data: Vec<u8> = [i32::MAX, i32::MIN, 0].iter().flat_map(|s| s.to_be_bytes()).collect();
        let file = depth_file("s32", 1, 32, &data);
        assert_eq!(file.samples, EXPECTED.to_vec());
    }

    #[test]
    fn unsupported_sample_size_is_error() {
        let bytes = form(b"AIFF", &[
            chunk(b"COMM", &comm_body(1, 1, 40, 44100)),
            chunk(b"SSND", &ssnd_body(0, &[0; 5])),
        ]);
        assert!(matches!(parse_temp("s40", &bytes), Err(DecodeError::UnsupportedFormat(_))));
    }
}
