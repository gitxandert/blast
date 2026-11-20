//! Measurements on a rendered signal (mono, `f32`): levels, onsets, clicks
//! and pitch. Thresholds are pinned by the unit tests below and by
//! `tests/scenarios.rs`'s self test.

/// The floor `db` returns for silence.
pub const FLOOR_DB: f64 = -200.0;

/// Length of the onset detector's window, in seconds (2 ms).
const ONSET_WINDOW_SECS: f64 = 0.002;
/// An onset needs the window's mean level above this (about -60 dBFS)...
const ONSET_FLOOR: f32 = 0.001;
/// ...and this many times the level of the window before it.
const ONSET_RISE: f32 = 3.0;
/// After an onset, no other for this long (seconds).
const ONSET_REFRACTORY_SECS: f64 = 0.010;

/// A click's slope change must exceed this multiple of the local mean...
const CLICK_RATIO: f32 = 8.0;
/// ...and happen where the signal is above this level (-40 dBFS), so a
/// quiet kink on a quiet sound still counts but dither-level noise does not.
const CLICK_FLOOR: f32 = 0.01;
/// A slope change under this is float noise whatever the level.
const BEND_EPS: f32 = 1e-6;
/// Frames on each side of a point that make its "local" neighbourhood.
const CLICK_SPAN: usize = 16;
/// Frames next to the point itself that the local mean leaves out, so a
/// click can't hide in its own average.
const CLICK_GAP: usize = 2;

/// A level comparison.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmp {
    /// `<=`
    AtMost,
    /// `>=`
    AtLeast,
}

/// Linear amplitude in dBFS (never below `FLOOR_DB`).
pub fn db(x: f64) -> f64 {
    if x <= 0.0 {
        FLOOR_DB
    } else {
        (20.0 * x.log10()).max(FLOOR_DB)
    }
}

/// Linear amplitude of `d` dBFS.
pub fn lin(d: f64) -> f64 {
    10f64.powf(d / 20.0)
}

/// Peak level of the samples, in dBFS.
pub fn peak_db(x: &[f32]) -> f64 {
    db(x.iter().fold(0.0f32, |m, v| m.max(v.abs())) as f64)
}

/// RMS level of the samples, in dBFS.
pub fn rms_db(x: &[f32]) -> f64 {
    if x.is_empty() {
        return FLOOR_DB;
    }
    let sum: f64 = x.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
    db((sum / x.len() as f64).sqrt())
}

/// Running sums of `|x|` (so any window's mean is two lookups).
fn abs_prefix(x: &[f32]) -> Vec<f64> {
    let mut p = Vec::with_capacity(x.len() + 1);
    p.push(0.0);
    for &v in x {
        p.push(p[p.len() - 1] + f64::from(v.abs()));
    }
    p
}

/// Frames where a sound starts: the rectified level over a 2 ms window
/// (stepped by a quarter window) rises past 3 times the level of the window
/// before it and past -60 dBFS. The frame is refined to the first one in the
/// window above a quarter of the window's peak.
pub fn onsets(x: &[f32], rate: u32) -> Vec<usize> {
    let w = ((ONSET_WINDOW_SECS * f64::from(rate)) as usize).max(4);
    let hop = (w / 4).max(1);
    let refractory = (ONSET_REFRACTORY_SECS * f64::from(rate)) as usize;
    let prefix = abs_prefix(x);
    let mean = |a: usize, b: usize| {
        let b = b.min(x.len());
        if a >= b {
            0.0
        } else {
            ((prefix[b] - prefix[a]) / (b - a) as f64) as f32
        }
    };
    let mut found: Vec<usize> = Vec::new();
    let mut p = 0;
    while p < x.len() {
        let cur = mean(p, p + w);
        let prev = if p >= w { mean(p - w, p) } else { 0.0 };
        let quiet_since_last = found.last().is_none_or(|&f| p >= f + refractory);
        if cur > ONSET_FLOOR && cur > ONSET_RISE * prev && quiet_since_last {
            let end = (p + w).min(x.len());
            let top = x[p..end].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let at = x[p..end]
                .iter()
                .position(|v| v.abs() >= 0.25 * top)
                .map_or(p, |i| p + i);
            found.push(at);
        }
        p += hop;
    }
    found
}

/// Frames of clicks: points where the slope changes by more than 8 times
/// the local mean slope change (over the 32 frames around, without the two
/// either side), where the signal is above -40 dBFS. That finds a step in the signal
/// (a splice) and a kink (the slope reversing at a loop edge), and not a
/// smooth waveform. Points within 2 ms of a detected onset are skipped, and
/// neighbouring hits are merged into one.
pub fn clicks(x: &[f32], rate: u32) -> Vec<usize> {
    let n = x.len();
    if n < 3 {
        return Vec::new();
    }
    // bend[i]: how much the slope changes at i
    let mut bend = vec![0.0f32; n];
    for i in 2..n {
        bend[i] = (x[i] - 2.0 * x[i - 1] + x[i - 2]).abs();
    }
    let mut prefix = Vec::with_capacity(n + 1);
    prefix.push(0.0f64);
    for &b in &bend {
        prefix.push(prefix[prefix.len() - 1] + f64::from(b));
    }
    let sum = |a: usize, b: usize| prefix[b.min(n)] - prefix[a.min(n)];

    let w = ((ONSET_WINDOW_SECS * f64::from(rate)) as usize).max(4);
    let starts = onsets(x, rate);
    let mut near = starts.iter().peekable();
    let mut hits: Vec<usize> = Vec::new();
    for (i, &b) in bend.iter().enumerate().skip(2) {
        if b < BEND_EPS {
            continue;
        }
        // the signal's level around the point, not the size of the change
        let near_lo = i.saturating_sub(CLICK_SPAN);
        let level = x[near_lo..n.min(i + CLICK_SPAN + 1)]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()));
        if level < CLICK_FLOOR {
            continue;
        }
        // the onsets are in order, so one cursor walks along with `i`
        while near.next_if(|&&s| s + 2 * w < i).is_some() {}
        if near.peek().is_some_and(|&&s| i + w >= s) {
            continue;
        }
        let lo = i.saturating_sub(CLICK_SPAN);
        let left = sum(lo, i.saturating_sub(CLICK_GAP));
        let right = sum(i + 1 + CLICK_GAP, i + 1 + CLICK_SPAN);
        let count = (i.saturating_sub(CLICK_GAP) - lo)
            + (n.min(i + 1 + CLICK_SPAN) - n.min(i + 1 + CLICK_GAP));
        let local = if count == 0 {
            0.0
        } else {
            ((left + right) / count as f64) as f32
        };
        if b > CLICK_RATIO * local {
            match hits.last() {
                Some(&last) if i - last <= 2 * CLICK_SPAN => {}
                _ => hits.push(i),
            }
        }
    }
    hits
}

// ---- pitch ----

/// In-place radix-2 FFT; `re.len()` is a power of two.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -std::f64::consts::TAU / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
        }
        len <<= 1;
    }
}

/// The strongest frequency in the samples, in Hz: a Hann-windowed DFT,
/// zero-padded 4 times, with parabolic interpolation on the log magnitudes
/// around the peak. `None` for a signal too short or silent.
pub fn pitch_hz(x: &[f32], rate: u32) -> Option<f64> {
    if x.len() < 64 {
        return None;
    }
    let size = (x.len() * 4).next_power_of_two();
    let mut re = vec![0.0f64; size];
    let mut im = vec![0.0f64; size];
    let last = (x.len() - 1) as f64;
    for (i, &v) in x.iter().enumerate() {
        let hann = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / last).cos();
        re[i] = f64::from(v) * hann;
    }
    fft(&mut re, &mut im);
    let mag: Vec<f64> = (0..size / 2)
        .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt())
        .collect();
    // skip the DC end: below about 20 Hz is a drift, not a pitch
    let first = ((20.0 * size as f64 / f64::from(rate)) as usize).max(1);
    let (peak, &top) = mag
        .iter()
        .enumerate()
        .skip(first)
        .max_by(|a, b| a.1.total_cmp(b.1))?;
    if top <= 1e-9 || peak + 1 >= mag.len() {
        return None;
    }
    let (a, b, c) = (
        (mag[peak - 1] + 1e-12).ln(),
        (mag[peak] + 1e-12).ln(),
        (mag[peak + 1] + 1e-12).ln(),
    );
    let denom = a - 2.0 * b + c;
    let shift = if denom.abs() < 1e-12 {
        0.0
    } else {
        0.5 * (a - c) / denom
    };
    Some((peak as f64 + shift) * f64::from(rate) / size as f64)
}

/// `a` against `b` in cents (1200 per octave).
pub fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const R: u32 = 48_000;

    fn sine(hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| 0.5 * (TAU * hz * i as f32 / R as f32).sin())
            .collect()
    }

    #[test]
    fn levels() {
        let s = sine(440.0, 48_000);
        assert!((peak_db(&s) - db(0.5)).abs() < 0.01);
        // a sine's RMS is 3 dB under its peak
        assert!((rms_db(&s) - (peak_db(&s) - 3.0103)).abs() < 0.01);
        assert_eq!(peak_db(&[0.0; 10]), FLOOR_DB);
        assert_eq!(rms_db(&[]), FLOOR_DB);
        assert!((lin(-6.0206) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn onsets_of_a_click_train() {
        let mut x = vec![0.0f32; 48_000];
        let at = [0usize, 11_999, 24_000, 36_037];
        for &f in &at {
            x[f] = 0.9;
        }
        assert_eq!(onsets(&x, R), at);
    }

    #[test]
    fn onsets_of_bursts_and_a_sustained_sound() {
        let x = super::super::assets::samples("burst").unwrap();
        let found = onsets(&x, R);
        assert_eq!(found.len(), 4, "{found:?}");
        for (f, want) in found.iter().zip(super::super::assets::BURST_FRAMES) {
            assert!(f.abs_diff(want) <= 24, "{f} vs {want}");
        }
        // a sine that starts at frame 1000 and goes on: one onset
        let mut s = vec![0.0f32; 1000];
        s.extend(sine(440.0, 20_000));
        assert_eq!(onsets(&s, R).len(), 1);
        assert!(onsets(&[0.0; 5000], R).is_empty());
    }

    #[test]
    fn a_splice_is_a_click_and_a_clean_sine_is_not() {
        let clean = sine(440.0, 24_000);
        assert!(clicks(&clean, R).is_empty());
        // a phase jump halfway: the second half comes from a different place
        let mut spliced = clean.clone();
        for (i, v) in spliced.iter_mut().enumerate().skip(12_000) {
            *v = 0.5 * (TAU * 440.0 * (i as f32 + 33.0) / R as f32).sin();
        }
        let found = clicks(&spliced, R);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].abs_diff(12_000) <= 3);
    }

    #[test]
    fn a_kink_is_a_click() {
        // a sine played forward, then backward from its zero crossing: the
        // value is continuous and the slope reverses
        let fwd = sine(440.0, 24_000);
        let mut x = fwd.clone();
        x.extend(fwd.iter().rev().skip(1));
        // the turn lands at a sample where the sine is near zero
        let turn = fwd.len() - 1;
        let found = clicks(&x, R);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].abs_diff(turn) <= 3);
    }

    #[test]
    fn a_quiet_kink_on_a_loud_enough_sound_is_a_click() {
        // 110 Hz at -10 dBFS played forward then backward: the slope change
        // is 2 * w * A = 0.009, under the old absolute floor of 0.01
        let fwd: Vec<f32> = (0..48_000)
            .map(|i| 0.316 * (TAU * 110.0 * i as f32 / R as f32).sin())
            .collect();
        let mut x = fwd.clone();
        x.extend(fwd.iter().rev().skip(1));
        let found = clicks(&x, R);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].abs_diff(fwd.len() - 1) <= 3);
        // the same kink 30 times quieter (-40 dBFS peak is the floor) is
        // below the level floor
        let quiet: Vec<f32> = x.iter().map(|v| v * 0.03).collect();
        assert!(clicks(&quiet, R).is_empty());
    }

    #[test]
    fn noise_and_onsets_are_not_clicks() {
        let n = super::super::assets::samples("noise").unwrap();
        assert!(clicks(&n, R).is_empty(), "{:?}", clicks(&n, R));
        let b = super::super::assets::samples("burst").unwrap();
        assert!(clicks(&b, R).is_empty(), "{:?}", clicks(&b, R));
        let mut c = vec![0.0f32; 4800];
        c[100] = 0.9;
        assert!(clicks(&c, R).is_empty());
    }

    #[test]
    fn pitch_to_half_a_cent() {
        for hz in [110.0f32, 440.0, 880.0, 1234.5] {
            let found = pitch_hz(&sine(hz, 24_000), R).unwrap();
            let off = cents(found, f64::from(hz)).abs();
            assert!(off < 0.5, "{hz} Hz found as {found} ({off} c)");
        }
        assert_eq!(pitch_hz(&[0.0; 4800], R), None);
        assert_eq!(pitch_hz(&[0.1; 10], R), None);
    }
}
