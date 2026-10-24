use std::fs::File;
use std::io::{self, Read, SeekFrom};
use std::ops::{Shl, BitOr, AddAssign};
use super::decode_helpers::{AudioFile, DecodeError, DecodeResult};

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

pub fn print_id(vec: &mut Vec<u8>, start: &mut usize, end: &mut usize) -> DecodeResult<()> {
    *end += 4;

    for i in *start..*end {
        let c = match vec.get(i) {
            Some(val)   => val,
            None    => return Err(DecodeError::UnexpectedEof),
        };
    }

    *start = *end;


    Ok(())
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
    // (print_id always increments end by four before printing
    //  and sets start to end afterward)
    print_id(&mut reader, &mut start, &mut end)?;

    // (parse_bytes increments end by the integer argument
    //  before decoding the reader from start to end
    //  and sets start to end afterward))
    let riff_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

    // WAVE
    print_id(&mut reader, &mut start, &mut end)?;

    // "fmt "
    print_id(&mut reader, &mut start,&mut end)?;        

    let fmt_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;
    // fmt data begins here; used to skip to the end of the chunk
    let fmt_start: usize = start;

    let Some(fmt_tag) = FormatCode::from_u16(parse_bytes(&mut reader, &mut start, &mut end, 2)?.try_into().unwrap())
    else {
        return Err(DecodeError::UnsupportedFormat(String::from("Unrecognized format tag")));
    };
    
    let num_channels: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;
    
    let sample_rate: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

    let data_rate: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

    let data_blk_sz: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

    let bits_per_sample: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

    // if !WaveFormatPcm (i.e. is extensible)
    if fmt_size >= 18 {
        // extension is either 0 or 22
        let cb_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

        if cb_size > 0 {
            let valid_bits: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

            let dw_channel_mask: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;

            let old_fmt: u32 = parse_bytes(&mut reader, &mut start, &mut end, 2)?;

            // rest of the SubFormat GUID is skipped below
            // TODO: compare against audio media subtype
        }
    }

    // skip whatever is left of the fmt chunk
    end = fmt_start + fmt_size as usize;
    start = end;


    //
    // TODO: parse "fact" chunk if non-PCM (and if exists)
    //

    // "data"
    print_id(&mut reader, &mut start, &mut end)?;
    let data_size: u32 = parse_bytes(&mut reader, &mut start, &mut end, 4)?;
   
    let mut samples: Vec<i16> = Vec::new();
    
    end += data_size as usize;
    for i in (start..end).step_by(2) {
        let s1 = match reader.get(i) {
            Some(val) => *val,
            None => return Err(DecodeError::UnexpectedEof),
        };
        let s2 = match reader.get(i + 1) {
            Some(val) => *val,
            None => return Err(DecodeError::UnexpectedEof),
        };
 
        samples.push(i16::from_le_bytes([s1, s2]));
    }

    let file_name: &str = match path.rsplit_once(|b: char| b == '.') {
        Some((before, after)) if !before.is_empty() && !after.is_empty() => {
            match before.rsplit_once(|b: char| b == '/') {
                Some((assets, name)) => name,
                None => return Err(DecodeError::InvalidData("File is not nested".to_string())),
            }
        }
        _ => return Err(DecodeError::InvalidData("File has no name".to_string())),
    };

    Ok(AudioFile::new(file_name, "wav", sample_rate, num_channels, bits_per_sample, samples))
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
