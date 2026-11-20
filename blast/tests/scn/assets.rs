//! Synthetic assets for scenarios: small deterministic mono 16-bit WAVs at
//! 48 kHz, written into a temp folder before the render. Each asset is a
//! small function; a plan that needs another one adds it here.

use std::f32::consts::TAU;
use std::path::Path;

/// The rate every scenario asset is made at (and rendered at).
pub const RATE: u32 = 48_000;

/// Frames at which `burst` starts a decaying 1 kHz burst.
pub const BURST_FRAMES: [usize; 4] = [0, 12_000, 24_000, 36_000];

/// The names `make` knows.
pub const NAMES: [&str; 5] = ["click", "sine440", "noise", "saw110", "burst"];

/// The samples of the named asset, or `None` for an unknown name.
pub fn samples(name: &str) -> Option<Vec<f32>> {
    let secs = RATE as usize;
    Some(match name {
        // one frame of an impulse: every play is an exact onset
        "click" => vec![0.9],
        "sine440" => (0..secs)
            .map(|i| 0.5 * (TAU * 440.0 * i as f32 / RATE as f32).sin())
            .collect(),
        "noise" => {
            let mut x: u32 = 12_345;
            (0..secs)
                .map(|_| {
                    x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    0.5 * ((x >> 8) as f32 / (1u32 << 23) as f32 - 1.0)
                })
                .collect()
        }
        // rising ramp of 110 Hz, from -0.5 to 0.5 each period. It has a step
        // every period of its own, so `no clicks` does not apply to it.
        "saw110" => (0..secs)
            .map(|i| (i as f32 * 110.0 / RATE as f32).fract() - 0.5)
            .collect(),
        // four 1 kHz bursts that decay to nothing (e^-8) over 50 ms
        "burst" => {
            let mut out = vec![0.0; secs];
            for start in BURST_FRAMES {
                for k in 0..2_400usize {
                    let env = (-(k as f32) / 300.0).exp();
                    out[start + k] = 0.8 * env * (TAU * 1_000.0 * k as f32 / RATE as f32).sin();
                }
            }
            out
        }
        _ => return None,
    })
}

/// Writes `<dir>/<name>.wav` for each name; an unknown name is an error.
pub fn make(dir: &Path, names: &[String]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    for name in names {
        let data = samples(name)
            .ok_or_else(|| format!("unknown asset `{name}` (known: {})", NAMES.join(", ")))?;
        let path = dir.join(format!("{name}.wav"));
        let mut w = hound::WavWriter::create(&path, spec).map_err(|e| e.to_string())?;
        for x in data {
            w.write_sample((x * 32_767.0).round() as i16)
                .map_err(|e| e.to_string())?;
        }
        w.finalize().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_is_made() {
        for name in NAMES {
            let s = samples(name).unwrap();
            assert!(!s.is_empty());
            assert!(s.iter().all(|x| x.abs() <= 1.0), "{name} clips");
        }
        assert!(samples("nope").is_none());
    }

    #[test]
    fn burst_starts_where_it_says() {
        let s = samples("burst").unwrap();
        for f in BURST_FRAMES {
            assert!(s[f + 1..f + 40].iter().any(|x| x.abs() > 0.1));
            assert!(f == 0 || s[f - 10..f].iter().all(|x| *x == 0.0));
        }
    }
}
