//! Channel conversion: ITU-R BS.775 downmixes to stereo or mono, and simple
//! widening.

use std::f32::consts::FRAC_1_SQRT_2;

// downmixes read at most this many input channels; more are ignored
const MAX_MIX_CHANNELS: usize = 8;

const LEFT: [f32; 2] = [1.0, 0.0];
const RIGHT: [f32; 2] = [0.0, 1.0];
const CENTER: [f32; 2] = [FRAC_1_SQRT_2, FRAC_1_SQRT_2];
const SURROUND_LEFT: [f32; 2] = [FRAC_1_SQRT_2, 0.0];
const SURROUND_RIGHT: [f32; 2] = [0.0, FRAC_1_SQRT_2];
const SILENT: [f32; 2] = [0.0, 0.0];

// BS.775 (L, R) gains for each WAVE speaker bit (dwChannelMask), lowest first
fn speaker_gains(bit: u32) -> [f32; 2] {
    match bit {
        0 | 6 | 12 => LEFT,             // FL, FLC, TFL
        1 | 7 | 14 => RIGHT,            // FR, FRC, TFR
        2 | 8 | 11 | 13 | 16 => CENTER, // FC, BC, TC, TFC, TBC
        4 | 9 | 15 => SURROUND_LEFT,    // BL, SL, TBL
        5 | 10 | 17 => SURROUND_RIGHT,  // BR, SR, TBR
        3 => SILENT,                    // LFE
        _ => CENTER,                    // reserved bits: keep the content rather than drop it
    }
}

// the WAVE default layout for a channel count (1..=8)
fn default_mask(channels: usize) -> u32 {
    match channels {
        1 => 0x4,
        2 => 0x3,
        3 => 0x7,
        4 => 0x33,
        5 => 0x37,
        6 => 0x3F,
        7 => 0x13F,
        _ => 0x63F,
    }
}

// (L, R) gains per input channel, each output column normalised to sum to 1
fn stereo_matrix(from_ch: usize, layout: Option<u32>) -> [[f32; 2]; MAX_MIX_CHANNELS] {
    let mask = match layout {
        Some(mask) if mask.count_ones() as usize == from_ch => mask,
        _ => default_mask(from_ch.min(MAX_MIX_CHANNELS)),
    };
    let mut matrix = [SILENT; MAX_MIX_CHANNELS];
    let bits = (0..32).filter(|bit| mask & (1 << bit) != 0);
    for (row, bit) in matrix.iter_mut().zip(bits) {
        *row = speaker_gains(bit);
    }
    for side in 0..2 {
        let sum: f32 = matrix.iter().map(|row| row[side]).sum();
        if sum > 0.0 {
            for row in &mut matrix {
                row[side] /= sum;
            }
        }
    }
    matrix
}

/// Converts interleaved `samples` from `from_ch` to `to_ch` channels.
/// Narrowing to 1 or 2 channels is an ITU-R BS.775 downmix using `layout` (a
/// WAVE channel mask) or, if it doesn't match `from_ch`, the default WAVE
/// layout for the count; gains are normalised so correlated content can't
/// clip. Narrowing to 3 or more keeps the first `to_ch` channels. Mono to N
/// duplicates; other widenings copy and zero-fill. Same count or a zero
/// count returns a copy.
pub fn remap(samples: &[f32], from_ch: usize, layout: Option<u32>, to_ch: usize) -> Vec<f32> {
    if from_ch == to_ch || from_ch == 0 || to_ch == 0 {
        return samples.to_vec();
    }
    let frames = samples.chunks_exact(from_ch);
    let mut out = Vec::with_capacity(frames.len() * to_ch);
    if from_ch == 1 {
        for frame in frames {
            out.extend(std::iter::repeat_n(frame[0], to_ch));
        }
    } else if from_ch < to_ch {
        for frame in frames {
            out.extend_from_slice(frame);
            out.extend(std::iter::repeat_n(0.0, to_ch - from_ch));
        }
    } else if to_ch >= 3 {
        // no standard matrix for these, and such devices are rare
        for frame in frames {
            out.extend_from_slice(&frame[..to_ch]);
        }
    } else {
        let matrix = stereo_matrix(from_ch, layout);
        for frame in frames {
            let (mut left, mut right) = (0.0, 0.0);
            for (sample, gains) in frame.iter().zip(&matrix) {
                left += sample * gains[0];
                right += sample * gains[1];
            }
            if to_ch == 1 {
                out.push((left + right) / 2.0);
            } else {
                out.extend_from_slice(&[left, right]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[f32], b: &[f32]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
    }

    // an impulse on channel `ch` of `from_ch`, downmixed to stereo
    fn impulse(from_ch: usize, layout: Option<u32>, ch: usize) -> Vec<f32> {
        let mut frame = vec![0.0; from_ch];
        frame[ch] = 1.0;
        remap(&frame, from_ch, layout, 2)
    }

    #[test]
    fn remap_mono_duplicates() {
        assert_eq!(
            remap(&[0.5, -0.25], 1, None, 3),
            vec![0.5, 0.5, 0.5, -0.25, -0.25, -0.25]
        );
    }

    #[test]
    fn remap_same_count_copies() {
        let input = [0.1, 0.2, 0.3, 0.4];
        assert_eq!(remap(&input, 2, None, 2), input.to_vec());
        assert_eq!(remap(&input, 0, None, 2), input.to_vec());
        assert_eq!(remap(&input, 2, None, 0), input.to_vec());
    }

    #[test]
    fn remap_widen_zero_fills() {
        assert_eq!(
            remap(&[0.1, 0.2, 0.3, 0.4], 2, None, 4),
            vec![0.1, 0.2, 0.0, 0.0, 0.3, 0.4, 0.0, 0.0]
        );
    }

    #[test]
    fn remap_stereo_to_mono_averages() {
        assert!(close(
            &remap(&[1.0, 0.0, 0.2, 0.6], 2, None, 1),
            &[0.5, 0.4]
        ));
    }

    #[test]
    fn remap_51_identical_channels_is_unity() {
        let input: Vec<f32> = [0.3f32, -0.7, 0.9].iter().flat_map(|&v| [v; 6]).collect();
        let out = remap(&input, 6, Some(0x3F), 2);
        assert!(close(&out, &[0.3, 0.3, -0.7, -0.7, 0.9, 0.9]), "{out:?}");
    }

    #[test]
    fn remap_51_coefficients() {
        let sum = 1.0 + 2.0 * FRAC_1_SQRT_2;
        let h = FRAC_1_SQRT_2 / sum;
        let expected = [
            [1.0 / sum, 0.0], // FL
            [0.0, 1.0 / sum], // FR
            [h, h],           // FC
            [0.0, 0.0],       // LFE
            [h, 0.0],         // BL
            [0.0, h],         // BR
        ];
        for (ch, gains) in expected.iter().enumerate() {
            assert!(close(&impulse(6, Some(0x3F), ch), gains), "channel {ch}");
        }
    }

    #[test]
    fn remap_unknown_or_bad_layout_uses_default() {
        for ch in 0..6 {
            let default = impulse(6, Some(0x3F), ch);
            assert!(close(&impulse(6, None, ch), &default));
            assert!(close(&impulse(6, Some(0x3), ch), &default));
        }
    }

    #[test]
    fn remap_side_vs_back() {
        // 7.1: FL FR FC LFE BL BR SL SR
        for (back, side) in [(4, 6), (5, 7)] {
            assert!(close(
                &impulse(8, Some(0x63F), back),
                &impulse(8, Some(0x63F), side)
            ));
        }
        // and the sides land on their own side
        let sl = impulse(8, Some(0x63F), 6);
        assert!(sl[0] > 0.0 && sl[1] == 0.0);
    }

    #[test]
    fn remap_to_four_keeps_first() {
        let input: Vec<f32> = (0..12).map(|v| v as f32).collect();
        assert_eq!(
            remap(&input, 6, None, 4),
            vec![0.0, 1.0, 2.0, 3.0, 6.0, 7.0, 8.0, 9.0]
        );
    }
}
