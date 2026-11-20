//! Sample converters for the hand-written decoders (raw PCM and AU). The
//! scaling matches symphonia's, so a file decodes to the same samples
//! whichever path reads it.

use super::raw::SampleEncoding;

fn int(c: &[u8], big_endian: bool) -> i32 {
    // assemble into the top of an i32, then shift down to sign-extend
    let mut value: u32 = 0;
    for k in 0..c.len() {
        let b = if big_endian { c[k] } else { c[c.len() - 1 - k] };
        value = (value << 8) | u32::from(b);
    }
    let unused = 32 - 8 * c.len() as u32;
    ((value << unused) as i32) >> unused
}

fn float32(c: &[u8], big_endian: bool) -> f32 {
    let b = [c[0], c[1], c[2], c[3]];
    if big_endian {
        f32::from_be_bytes(b)
    } else {
        f32::from_le_bytes(b)
    }
}

fn float64(c: &[u8], big_endian: bool) -> f32 {
    let b = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
    (if big_endian {
        f64::from_be_bytes(b)
    } else {
        f64::from_le_bytes(b)
    }) as f32
}

/// Converts interleaved PCM bytes to f32 at full scale (`x / 2^(bits-1)`;
/// u8 is offset by 128). A trailing partial sample is ignored.
pub(super) fn to_f32(bytes: &[u8], encoding: SampleEncoding) -> Vec<f32> {
    use SampleEncoding::*;
    let samples = bytes.chunks_exact(encoding.bytes());
    match encoding {
        U8 => samples.map(|c| f32::from(c[0]) / 128.0 - 1.0).collect(),
        S8 => samples.map(|c| f32::from(c[0] as i8) / 128.0).collect(),
        S16Le | S16Be => samples
            .map(|c| int(c, encoding == S16Be) as f32 / 32_768.0)
            .collect(),
        S24Le | S24Be => samples
            .map(|c| int(c, encoding == S24Be) as f32 / 8_388_608.0)
            .collect(),
        S32Le | S32Be => samples
            .map(|c| (f64::from(int(c, encoding == S32Be)) / 2_147_483_648.0) as f32)
            .collect(),
        F32Le | F32Be => samples.map(|c| float32(c, encoding == F32Be)).collect(),
        F64Le | F64Be => samples.map(|c| float64(c, encoding == F64Be)).collect(),
    }
}

/// G.711 µ-law to f32, through the standard 16-bit expansion.
pub(super) fn ulaw_to_f32(byte: u8) -> f32 {
    let u = i32::from(!byte);
    let t = (((u & 0x0F) << 3) + 0x84) << ((u >> 4) & 7);
    let s = if u & 0x80 != 0 { 0x84 - t } else { t - 0x84 };
    s as f32 / 32_768.0
}

/// G.711 A-law to f32, through the standard 16-bit expansion.
pub(super) fn alaw_to_f32(byte: u8) -> f32 {
    let a = i32::from(byte ^ 0x55);
    let mut t = (a & 0x0F) << 4;
    let seg = (a >> 4) & 7;
    if seg == 0 {
        t += 8;
    } else {
        t = (t + 0x108) << (seg - 1);
    }
    let s = if a & 0x80 != 0 { t } else { -t };
    s as f32 / 32_768.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use SampleEncoding::*;

    #[test]
    fn pcm_8_bit_signed_and_unsigned() {
        assert_eq!(
            to_f32(&[0x80, 0xFF, 0x00], U8),
            vec![0.0, 127.0 / 128.0, -1.0]
        );
        assert_eq!(
            to_f32(&[0x00, 0x7F, 0x80], S8),
            vec![0.0, 127.0 / 128.0, -1.0]
        );
    }

    #[test]
    fn pcm_16_both_endians() {
        let expected = vec![4660.0 / 32768.0, 32767.0 / 32768.0, -1.0];
        assert_eq!(
            to_f32(&[0x34, 0x12, 0xFF, 0x7F, 0x00, 0x80], S16Le),
            expected
        );
        assert_eq!(
            to_f32(&[0x12, 0x34, 0x7F, 0xFF, 0x80, 0x00], S16Be),
            expected
        );
    }

    #[test]
    fn pcm_24_both_endians() {
        let expected = vec![0.0, 8_388_607.0 / 8_388_608.0, -1.0, -1.0 / 8_388_608.0];
        let le = [
            0x00, 0x00, 0x00, 0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x80, 0xFF, 0xFF, 0xFF,
        ];
        assert_eq!(to_f32(&le, S24Le), expected);
        let be = [
            0x00, 0x00, 0x00, 0x7F, 0xFF, 0xFF, 0x80, 0x00, 0x00, 0xFF, 0xFF, 0xFF,
        ];
        assert_eq!(to_f32(&be, S24Be), expected);
    }

    #[test]
    fn pcm_32() {
        let le = [
            0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x7F, 0, 0, 0, 0x80, 0, 0, 0, 0x40,
        ];
        // i32::MAX / 2^31 rounds to 1.0 in f32
        assert_eq!(to_f32(&le, S32Le), vec![0.0, 1.0, -1.0, 0.5]);
        let be = [0x40, 0, 0, 0];
        assert_eq!(to_f32(&be, S32Be), vec![0.5]);
    }

    #[test]
    fn pcm_float_32_and_64() {
        let le: Vec<u8> = [0.0f32, 1.0, -1.0, 0.5, 2.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        // no clamping: the backend clamps
        assert_eq!(to_f32(&le, F32Le), vec![0.0, 1.0, -1.0, 0.5, 2.0]);
        let be: Vec<u8> = [0.0f64, 1.0, -1.0, -0.5]
            .iter()
            .flat_map(|f| f.to_be_bytes())
            .collect();
        assert_eq!(to_f32(&be, F64Be), vec![0.0, 1.0, -1.0, -0.5]);
    }

    #[test]
    fn pcm_ignores_trailing_partial_sample() {
        assert_eq!(to_f32(&[0x01, 0x00, 0x02], S16Le), vec![1.0 / 32768.0]);
        assert!(to_f32(&[0; 7], F64Le).is_empty());
    }

    #[test]
    fn ulaw_known_values() {
        assert_eq!(ulaw_to_f32(0xFF), 0.0);
        assert_eq!(ulaw_to_f32(0x00), -32124.0 / 32768.0);
        assert_eq!(ulaw_to_f32(0x80), 32124.0 / 32768.0);
    }

    #[test]
    fn alaw_known_values() {
        assert_eq!(alaw_to_f32(0xD5), 8.0 / 32768.0);
        assert_eq!(alaw_to_f32(0x55), -8.0 / 32768.0);
        assert_eq!(alaw_to_f32(0xAA), 32256.0 / 32768.0);
        assert_eq!(alaw_to_f32(0x2A), -32256.0 / 32768.0);
    }
}
