#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    UnsupportedFormat(String),
    UnexpectedEof,
    InvalidData(String),
}

pub type DecodeResult<T> = Result<T, DecodeError>;

impl From<std::io::Error> for DecodeError {
    fn from(err: std::io::Error) -> Self {
        DecodeError::Io(err)
    }
}

#[derive(Clone, Debug)]
pub struct AudioFile {
    pub file_name: String,
    pub format: String,
    pub sample_rate: u32,
    pub num_channels: u32,
    pub bits_per_sample: u32,
    pub samples: Vec<i16>,
}

impl AudioFile {
    pub fn new(
        file_name: &str,
        format: &str,
        sample_rate: u32,
        num_channels: u32,
        bits_per_sample: u32,
        samples: Vec<i16>,
    ) -> Self {
        Self {
            file_name: file_name.to_string(),
            format: format.to_string(),
            sample_rate,
            num_channels,
            bits_per_sample,
            samples,
        }
    }
}

// a track's name is its file name without directory or extension
pub fn file_stem(path: &str) -> DecodeResult<&str> {
    match std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
    {
        Some(name) if !name.is_empty() => Ok(name),
        _ => Err(DecodeError::InvalidData(format!(
            "File has no name: '{path}'"
        ))),
    }
}

// convert integer PCM of any width up to 32 bits to i16;
// samples narrower than their container are left-justified,
// so only the container width (bits rounded up to bytes) matters
pub fn pcm_to_i16(
    data: &[u8],
    bits: u32,
    big_endian: bool,
    unsigned_8: bool,
) -> DecodeResult<Vec<i16>> {
    let width = bits.div_ceil(8) as usize;
    if width == 0 || width > 4 {
        return Err(DecodeError::UnsupportedFormat(format!("{bits}-bit PCM")));
    }

    let samples = data
        .chunks_exact(width)
        .map(|c| {
            if width == 1 {
                return if unsigned_8 {
                    (c[0] as i16 - 128) << 8
                } else {
                    (c[0] as i8 as i16) << 8
                };
            }

            let mut value: u32 = 0;
            for k in 0..width {
                let b = if big_endian { c[k] } else { c[width - 1 - k] };
                value = (value << 8) | b as u32;
            }
            // move to the top of an i32 and keep the 16 most significant bits
            ((value << (8 * (4 - width))) as i32 >> 16) as i16
        })
        .collect();

    Ok(samples)
}

// convert 32- or 64-bit IEEE float (full scale -1.0..1.0) to i16
pub fn float_to_i16(data: &[u8], bits: u32, big_endian: bool) -> DecodeResult<Vec<i16>> {
    let to_i16 = |f: f64| (f * 32768.0).clamp(-32768.0, 32767.0) as i16;

    match bits {
        32 => Ok(data
            .chunks_exact(4)
            .map(|c| {
                let b = [c[0], c[1], c[2], c[3]];
                to_i16(if big_endian {
                    f32::from_be_bytes(b)
                } else {
                    f32::from_le_bytes(b)
                } as f64)
            })
            .collect()),
        64 => Ok(data
            .chunks_exact(8)
            .map(|c| {
                let b = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
                to_i16(if big_endian {
                    f64::from_be_bytes(b)
                } else {
                    f64::from_le_bytes(b)
                })
            })
            .collect()),
        _ => Err(DecodeError::UnsupportedFormat(format!("{bits}-bit float"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_8_bit_unsigned_and_signed() {
        assert_eq!(
            pcm_to_i16(&[0x80, 0xFF, 0x00], 8, false, true).unwrap(),
            vec![0, 32512, -32768]
        );
        assert_eq!(
            pcm_to_i16(&[0x00, 0x7F, 0x80], 8, true, false).unwrap(),
            vec![0, 32512, -32768]
        );
    }

    #[test]
    fn pcm_16_bit_both_endians() {
        assert_eq!(
            pcm_to_i16(&[0x34, 0x12, 0xFF, 0x7F, 0x00, 0x80], 16, false, true).unwrap(),
            vec![0x1234, i16::MAX, i16::MIN]
        );
        assert_eq!(
            pcm_to_i16(&[0x12, 0x34, 0x7F, 0xFF, 0x80, 0x00], 16, true, false).unwrap(),
            vec![0x1234, i16::MAX, i16::MIN]
        );
    }

    #[test]
    fn pcm_24_bit() {
        let le = [
            0x00, 0x00, 0x00, 0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x80, 0xFF, 0xFF, 0xFF,
        ];
        assert_eq!(
            pcm_to_i16(&le, 24, false, true).unwrap(),
            vec![0, i16::MAX, i16::MIN, -1]
        );
        let be = [
            0x00, 0x00, 0x00, 0x7F, 0xFF, 0xFF, 0x80, 0x00, 0x00, 0x12, 0x34, 0x56,
        ];
        assert_eq!(
            pcm_to_i16(&be, 24, true, false).unwrap(),
            vec![0, i16::MAX, i16::MIN, 0x1234]
        );
        // 20 valid bits in a 24-bit container decode the same way
        assert_eq!(
            pcm_to_i16(&be, 20, true, false).unwrap(),
            vec![0, i16::MAX, i16::MIN, 0x1234]
        );
    }

    #[test]
    fn pcm_32_bit() {
        let le = [
            0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x7F, 0, 0, 0, 0x80, 0, 0, 0, 0x40,
        ];
        assert_eq!(
            pcm_to_i16(&le, 32, false, true).unwrap(),
            vec![0, i16::MAX, i16::MIN, 16384]
        );
    }

    #[test]
    fn pcm_ignores_trailing_partial_sample() {
        assert_eq!(
            pcm_to_i16(&[0x01, 0x00, 0x02], 16, false, true).unwrap(),
            vec![1]
        );
    }

    #[test]
    fn pcm_unsupported_width() {
        assert!(matches!(
            pcm_to_i16(&[0; 8], 40, false, true),
            Err(DecodeError::UnsupportedFormat(_))
        ));
        assert!(matches!(
            pcm_to_i16(&[0; 8], 0, false, true),
            Err(DecodeError::UnsupportedFormat(_))
        ));
    }

    #[test]
    fn float_32_and_64() {
        let le: Vec<u8> = [0.0f32, 1.0, -1.0, 0.5, 2.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        assert_eq!(
            float_to_i16(&le, 32, false).unwrap(),
            vec![0, i16::MAX, i16::MIN, 16384, i16::MAX]
        );
        let be: Vec<u8> = [0.0f64, 1.0, -1.0, -0.5]
            .iter()
            .flat_map(|f| f.to_be_bytes())
            .collect();
        assert_eq!(
            float_to_i16(&be, 64, true).unwrap(),
            vec![0, i16::MAX, i16::MIN, -16384]
        );
        assert!(matches!(
            float_to_i16(&le, 16, false),
            Err(DecodeError::UnsupportedFormat(_))
        ));
    }
}
