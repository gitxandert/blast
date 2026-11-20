//! Offline sample-rate conversion for whole files (rubato's sinc resampler).

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

// offline, so quality over speed (see plans/detailed/LOAD.md step 1)
const PARAMS: SincInterpolationParameters = SincInterpolationParameters {
    sinc_len: 256,
    f_cutoff: None,
    oversampling_factor: 128,
    interpolation: SincInterpolationType::Cubic,
    window: WindowFunction::BlackmanHarris2,
};
const CHUNK_FRAMES: usize = 1024;

/// Offline sinc resampling of interleaved `samples` from `from` Hz to `to` Hz.
/// The result has `ceil(frames * to / from)` frames with the resampler's
/// startup delay trimmed. Returns a copy if `from == to`, and the input
/// unchanged if any argument is 0 or there is less than one frame (callers
/// validate first). A trailing partial frame is ignored.
pub fn resample(samples: &[f32], channels: usize, from: u32, to: u32) -> Vec<f32> {
    if from == to || channels == 0 || from == 0 || to == 0 || samples.len() < channels {
        return samples.to_vec();
    }
    let frames = samples.len() / channels;
    let input = &samples[..frames * channels];
    match run(input, channels, frames, from, to) {
        Ok(out) => out,
        Err(error) => {
            // can't happen with validated inputs and rubato's own buffers
            debug_assert!(false, "resample failed: {error}");
            samples.to_vec()
        }
    }
}

fn run(
    input: &[f32],
    channels: usize,
    frames: usize,
    from: u32,
    to: u32,
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let ratio = f64::from(to) / f64::from(from);
    let mut resampler = Async::<f32>::new_sinc(
        ratio,
        1.0,
        &PARAMS,
        CHUNK_FRAMES,
        channels,
        FixedAsync::Input,
    )?;
    let adapter = InterleavedSlice::new(input, channels, frames)?;
    let mut out = resampler.process_all(&adapter, frames, None)?.take_data();
    // rubato rounds `ratio * frames` in floating point, which can land one
    // frame past the exact count (44100 * (48000 / 44100) > 48000)
    let exact = (frames as u64 * u64::from(to)).div_ceil(u64::from(from));
    out.truncate(exact as usize * channels);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn frames_of(samples: &[f32], channels: usize) -> usize {
        samples.len() / channels
    }

    #[test]
    fn resample_identity_and_degenerate() {
        let input = vec![0.1, -0.2, 0.3, -0.4];
        assert_eq!(resample(&input, 2, 48000, 48000), input);
        assert_eq!(resample(&input, 0, 44100, 48000), input);
        assert_eq!(resample(&input, 2, 0, 48000), input);
        assert_eq!(resample(&input, 2, 44100, 0), input);
        assert_eq!(resample(&[0.5], 2, 44100, 48000), vec![0.5]);
        // one frame works
        let one = resample(&[0.5, -0.5], 2, 24000, 48000);
        assert_eq!(frames_of(&one, 2), 2);
    }

    #[test]
    fn resample_length() {
        let up = resample(&vec![0.0; 44100], 1, 44100, 48000);
        assert_eq!(up.len(), 48000);
        let down = resample(&vec![0.0; 48000], 1, 48000, 44100);
        assert_eq!(down.len(), 44100);
        let odd = resample(&vec![0.0; 1000], 1, 22050, 48000);
        assert_eq!(odd.len(), (1000.0f64 * 48000.0 / 22050.0).ceil() as usize);
        let stereo = resample(&vec![0.0; 2 * 44100], 2, 44100, 48000);
        assert_eq!(stereo.len(), 2 * 48000);
    }

    #[test]
    fn resample_sine_error_bound() {
        let (from, to, freq) = (44100.0, 48000.0, 1000.0);
        let input: Vec<f32> = (0..44100)
            .flat_map(|n| {
                let v = (0.5 * (TAU * freq * n as f64 / from).sin()) as f32;
                [v, -v]
            })
            .collect();
        let out = resample(&input, 2, 44100, 48000);
        let frames = frames_of(&out, 2);
        assert_eq!(frames, 48000);
        let range = 512..frames - 512;
        let rms_at = |d: f64| {
            let sum: f64 = range
                .clone()
                .map(|n| {
                    let ideal = 0.5 * (TAU * freq * (n as f64 - d) / to).sin();
                    (f64::from(out[2 * n]) - ideal).powi(2)
                })
                .sum();
            (sum / range.len() as f64).sqrt()
        };
        let best = (-150..=150)
            .map(|i| rms_at(f64::from(i) / 100.0))
            .fold(f64::INFINITY, f64::min);
        assert!(best < 1e-3, "rms error {best}");
        for n in range {
            assert!((out[2 * n] + out[2 * n + 1]).abs() < 1e-6);
        }
    }

    #[test]
    fn resample_downsample_rejects_alias() {
        let input: Vec<f32> = (0..48000)
            .map(|n| (0.5 * (TAU * 20000.0 * n as f64 / 48000.0).sin()) as f32)
            .collect();
        let out = resample(&input, 1, 48000, 22050);
        let inner = &out[512..out.len() - 512];
        let rms =
            (inner.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / inner.len() as f64).sqrt();
        assert!(rms < 1e-3, "alias rms {rms}");
    }

    #[test]
    fn resample_dc_stays_dc() {
        let out = resample(&vec![0.25; 48000], 1, 48000, 44100);
        for &v in &out[512..out.len() - 512] {
            assert!((v - 0.25).abs() < 1e-3, "{v}");
        }
    }
}
