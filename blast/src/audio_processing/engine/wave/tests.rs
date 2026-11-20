use super::*;
use crate::audio_processing::blast_rand::{clear_session_seed, set_session_seed};

const SR: f64 = 48_000.0;

fn one(shape: WaveShape, freq: f32) -> Waves {
    let mut w = Waves::new();
    let mut p = wave_defaults();
    p[WaveParam::Freq.index()] = freq;
    w.insert(0, shape, p, true);
    w
}

fn render(w: &mut Waves, n: usize, ratio: f64) -> Vec<f32> {
    (0..n)
        .map(|_| {
            w.now += 1;
            let s = w.render(0, ratio, SR, test_table());
            w.advance_ramps();
            s
        })
        .collect()
}

// the magnitude of the DFT of `x` (4-term Blackman-Harris windowed, whose
// side lobes are 92 dB down) at bin `k`
fn bin(x: &[f32], k: usize) -> f64 {
    let n = x.len();
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let t = std::f64::consts::TAU * i as f64 / n as f64;
        let w = 0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos();
        let a = std::f64::consts::TAU * (k * i % n) as f64 / n as f64;
        re += f64::from(*v) * w * a.cos();
        im -= f64::from(*v) * w * a.sin();
    }
    (re * re + im * im).sqrt()
}

// the frequency of a signal from its rising zero crossings
fn measured_hz(x: &[f32]) -> f64 {
    let ups: Vec<f64> = x
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f64 + f64::from(-w[0] / (w[1] - w[0])))
        .collect();
    let cycles = (ups.len() - 1) as f64;
    SR * cycles / (ups[ups.len() - 1] - ups[0])
}

#[test]
fn sine_zero_crossings_at_1khz() {
    let mut w = one(WaveShape::Sine, 1000.0);
    let x = render(&mut w, 480, 1.0);
    // 48 frames a cycle: a crossing every 24 frames
    let crossings: Vec<usize> = (1..x.len())
        .filter(|&i| (x[i - 1] < 0.0) != (x[i] < 0.0) || x[i] == 0.0)
        .collect();
    assert!(crossings.len() >= 18, "{crossings:?}");
    for pair in crossings.windows(2) {
        let gap = pair[1] as i64 - pair[0] as i64;
        assert!((gap - 24).abs() <= 1, "{crossings:?}");
    }
}

#[test]
fn ratio_scales_the_frequency() {
    let mut w = one(WaveShape::Sine, 440.0);
    let a = measured_hz(&render(&mut w, 9600, 1.0));
    let mut w = one(WaveShape::Sine, 440.0);
    let b = measured_hz(&render(&mut w, 9600, 2.0));
    assert!((a - 440.0).abs() < 0.5, "{a}");
    assert!((b - 880.0).abs() < 1.0, "{b}");
}

// the strongest bin from a quarter of the sample rate up to 19 kHz (the
// filter's band, whose 20 kHz edge is not held to the same floor) that is
// not near a harmonic of `f0`, in dB under the fundamental (`x` made at `sr`)
fn alias_db(x: &[f32], f0: f64, sr: f64) -> f64 {
    let hz_per_bin = sr / x.len() as f64;
    let fund_bin = (f0 / hz_per_bin).round() as usize;
    let fund = (fund_bin - 3..=fund_bin + 3)
        .map(|k| bin(x, k))
        .fold(0.0, f64::max);
    let near_harmonic = |k: usize| {
        let hz = k as f64 * hz_per_bin;
        (1..=24).any(|h| (hz - h as f64 * f0).abs() < 6.0 * hz_per_bin)
    };
    let worst = ((sr / 4.0 / hz_per_bin) as usize..(19_000.0 / hz_per_bin) as usize)
        .filter(|k| !near_harmonic(*k))
        .map(|k| bin(x, k))
        .fold(0.0, f64::max);
    20.0 * (worst / fund).log10()
}

fn render_at(shape: WaveShape, f0: f64, sr: f64) -> Vec<f32> {
    let table = BlepTable::new(sr);
    let mut w = one(shape, f0 as f32);
    // let the filter settle, then take 4096 frames (not a whole cycle)
    let next = |w: &mut Waves| {
        w.now += 1;
        w.render(0, 1.0, sr, &table)
    };
    for _ in 0..2000 {
        next(&mut w);
    }
    (0..4096).map(|_| next(&mut w)).collect()
}

#[test]
fn saw_and_square_alias_floor_off_the_grid() {
    // a pitch whose aliases fall between the harmonics
    let f0 = 1507.3;
    for (shape, sr) in [
        (WaveShape::Saw, 48_000.0),
        (WaveShape::Square, 48_000.0),
        (WaveShape::Saw, 44_100.0),
        (WaveShape::Square, 44_100.0),
    ] {
        let db = alias_db(&render_at(shape, f0, sr), f0, sr);
        println!("{shape:?} at {sr}: alias floor {db:.1} dB");
        assert!(db < -80.0, "{shape:?} at {sr}: {db} dB");
    }
}

// the worst bin between `lo` and `hi` Hz that no harmonic of `f0` sits on,
// against the fundamental
fn band_db(x: &[f32], f0: f64, sr: f64, lo: f64, hi: f64) -> f64 {
    let hz_per_bin = sr / x.len() as f64;
    let fund_bin = (f0 / hz_per_bin).round() as usize;
    let fund = (fund_bin - 3..=fund_bin + 3)
        .map(|k| bin(x, k))
        .fold(0.0, f64::max);
    let worst = ((lo / hz_per_bin) as usize..(hi / hz_per_bin) as usize)
        .filter(|k| {
            let hz = *k as f64 * hz_per_bin;
            !(1..=24).any(|h| (hz - h as f64 * f0).abs() < 6.0 * hz_per_bin)
        })
        .map(|k| bin(x, k))
        .fold(0.0, f64::max);
    20.0 * (worst / fund).log10()
}

#[test]
fn saw_images_above_20khz_are_far_down() {
    // the part of the folded images that lands between 20 kHz and Nyquist:
    // the pure elliptic residues put it near -100 dB (upstream's DC-safe set
    // left it at -55)
    let f0 = 1507.3;
    for shape in [WaveShape::Saw, WaveShape::Square, WaveShape::Tri] {
        let x = render_at(shape, f0, 48_000.0);
        let db = band_db(&x, f0, 48_000.0, 20_500.0, 23_900.0);
        println!("{shape:?}: 20k-Nyquist band {db:.1} dB");
        assert!(db < -90.0, "{shape:?}: {db} dB");
    }
}

#[test]
fn tri_alias_floor_off_the_grid() {
    let f0 = 1507.3;
    for sr in [48_000.0, 44_100.0] {
        let db = alias_db(&render_at(WaveShape::Tri, f0, sr), f0, sr);
        println!("Tri at {sr}: alias floor {db:.1} dB");
        assert!(db < -90.0, "tri at {sr}: {db} dB");
    }
}

#[test]
fn elliptic_blep_beats_a_naive_saw() {
    let f0 = 1507.3;
    let blep = alias_db(&render_at(WaveShape::Saw, f0, SR), f0, SR);
    let dt = f0 / SR;
    let naive: Vec<f32> = (0..4096)
        .map(|i| (2.0 * (i as f64 * dt).fract() - 1.0) as f32)
        .collect();
    let naive = alias_db(&naive, f0, SR);
    assert!(blep < naive - 40.0, "blep {blep} dB, naive {naive} dB");
}

// the largest step between neighbouring samples
fn biggest_step(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn a_retrigger_mid_cycle_is_smoothed() {
    for shape in [
        WaveShape::Saw,
        WaveShape::Sine,
        WaveShape::Square,
        WaveShape::Tri,
    ] {
        let mut w = one(shape, 500.0);
        render(&mut w, 1037, 1.0);
        // the cycle's position now, and the waveform there and at the start
        let at = w.osc[0].as_ref().unwrap().phase[0];
        let (old, new) = (naive(shape, at, 0.5, None), naive(shape, 0.0, 0.5, None));
        let jump = (new - old).abs() as f32;
        w.hit(0);
        let after = render(&mut w, 200, 1.0);
        // the first samples after the jump, from the last one before it
        // (which was `old`, to within the frames rendered in between)
        let mut x = vec![old as f32];
        x.extend_from_slice(&after[..8]);
        if jump > 0.3 {
            assert_spread(&x, jump, &format!("{shape:?}"));
        }
        // and nothing rings much past what the waveform reaches (a filtered
        // step overshoots by a few percent of its size)
        let top = peak_of(&after);
        assert!(
            f64::from(top) <= 1.05 + 0.25 * f64::from(jump),
            "{shape:?}: {top}"
        );
    }
}

#[test]
fn square_pw_duty() {
    let mut w = Waves::new();
    let mut p = wave_defaults();
    p[WaveParam::Freq.index()] = 100.0;
    p[WaveParam::Pw.index()] = 0.25;
    w.insert(0, WaveShape::Square, p, true);
    let x = render(&mut w, 4800, 1.0);
    let high = x.iter().filter(|v| **v > 0.0).count() as f64 / x.len() as f64;
    assert!((high - 0.25).abs() < 0.01, "{high}");
}

#[test]
fn tri_peaks_at_one() {
    let mut w = one(WaveShape::Tri, 200.0);
    // from well after the onset
    let x = render(&mut w, 9600, 1.0);
    let x = &x[4800..];
    let (lo, hi) = x
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    // the corners ring a little (the filter has a sharp edge)
    assert!(hi > 0.95 && hi < 1.06, "{hi}");
    assert!(lo < -0.95 && lo > -1.06, "{lo}");
    let mean = x.iter().sum::<f32>() / x.len() as f32;
    assert!(mean.abs() < 0.02, "{mean}");
}

#[test]
fn every_shape_stays_in_range() {
    for shape in WaveShape::ALL {
        let mut w = one(shape, 1000.0);
        let x = render(&mut w, 2000, 1.0);
        assert!(
            x.iter().all(|v| v.is_finite() && v.abs() <= 1.5),
            "{shape:?}: {}",
            x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
        );
    }
}

#[test]
fn retrig_restarts_phase() {
    let mut w = one(WaveShape::Saw, 100.0);
    let a = render(&mut w, 400, 1.0);
    w.hit(0);
    let b = render(&mut w, 400, 1.0);
    // the jump back is smoothed, and then the wave is where it began
    for i in 150..400 {
        assert!((a[i] - b[i]).abs() < 3e-2, "{i}: {} {}", a[i], b[i]);
    }
    assert!((a[0] - b[0]).abs() > 0.1);
}

#[test]
fn retrig_no_runs_free() {
    let mut w = one(WaveShape::Saw, 100.0);
    w.set_retrig(0, false);
    let a = render(&mut w, 100, 1.0);
    w.hit(0);
    let b = render(&mut w, 100, 1.0);
    assert_ne!(a, b);
}

#[test]
fn phase_setting_is_where_a_hit_starts() {
    let mut w = Waves::new();
    let mut p = wave_defaults();
    p[WaveParam::Phase.index()] = 0.25;
    w.insert(0, WaveShape::Sine, p, true);
    // a quarter in, a sine is at its peak: well into the onset, a wave of
    // 220 Hz is where the sine says
    let first = render(&mut w, 400, 1.0);
    let dt = 220.0 / SR;
    for (i, got) in first.iter().enumerate().skip(200) {
        let want = (std::f64::consts::TAU * (0.25 + i as f64 * dt)).sin() as f32;
        assert!((got - want).abs() < 3e-2, "{i}");
    }
    render(&mut w, 37, 1.0);
    w.hit(0);
    // the jump back is smoothed; once it has passed the wave runs where a
    // fresh one would
    let mut fresh = Waves::new();
    fresh.insert(0, WaveShape::Sine, p, true);
    let a = render(&mut fresh, 400, 1.0);
    let b = render(&mut w, 400, 1.0);
    for i in 200..400 {
        assert!((a[i] - b[i]).abs() < 3e-2, "{i}");
    }
}

#[test]
fn instances_keep_their_own_phase_and_ratio() {
    // two instances of one wave, each at its own ratio, in the same frames
    // (4817 frames: not a whole number of cycles)
    let mut w = one(WaveShape::Sine, 100.0);
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for _ in 0..4817 {
        w.now += 1;
        a.push(w.render(0, 1.0, SR, test_table()));
        b.push(w.render(1, 1.02, SR, test_table()));
    }
    assert!(measured_hz(&b) > measured_hz(&a) * 1.015);
    assert!((measured_hz(&a) - 100.0).abs() < 0.5);
    assert!((measured_hz(&b) - 102.0).abs() < 0.5);
    // a hit restarts one instance and leaves the other where it was going
    let (last_a, last_b) = (a[4816], b[4816]);
    assert!(
        last_b.abs() > 0.3,
        "{last_b}: a place with something to see"
    );
    w.hit(1);
    w.now += 1;
    let x = w.render(0, 1.0, SR, test_table());
    let y = w.render(1, 1.0, SR, test_table());
    let dt = 100.0 / SR;
    let want_a = last_a + (a[4816] - a[4815]);
    assert!((x - want_a).abs() < 0.02, "{x} against {want_a} ({dt})");
    // the restarted one glides from where it was (a hard restart would be 0)
    assert!((y - last_b).abs() < 0.5 * last_b.abs(), "{y} from {last_b}");
}

#[test]
fn wave_stop_resets_pause_holds() {
    let mut w = one(WaveShape::Saw, 100.0);
    let first = render(&mut w, 20, 1.0);
    // pause: silent, and the phase holds
    w.bypass(0, true, false);
    assert!(render(&mut w, 50, 1.0).iter().all(|v| *v == 0.0));
    w.bypass(0, false, false);
    // it comes back as an onset, and then it is the phase it held
    let held = render(&mut w, 400, 1.0);
    let mut cont = one(WaveShape::Saw, 100.0);
    let all = render(&mut cont, 420, 1.0);
    for i in 150..400 {
        assert!((held[i] - all[20 + i]).abs() < 3e-2, "{i}");
    }
    // stop: the phase starts over
    w.bypass(0, true, true);
    w.bypass(0, false, true);
    assert_eq!(render(&mut w, 20, 1.0), first);
}

#[test]
fn noise_deterministic_with_seed() {
    let run = || {
        set_session_seed(9);
        let mut w = one(WaveShape::Noise, 220.0);
        let x = render(&mut w, 256, 1.0);
        clear_session_seed();
        x
    };
    let (a, b) = (run(), run());
    assert_eq!(a, b);
    assert!(a.iter().any(|v| v.abs() > 0.1));
    assert!(a.iter().all(|v| v.abs() <= 1.0));
}

#[test]
fn glide_lands_on_target() {
    let mut w = one(WaveShape::Sine, 100.0);
    assert_eq!(w.set(0, WaveParam::Freq.index() as u8, 200.0, 10), 10);
    render(&mut w, 10, 1.0);
    assert_eq!(w.osc[0].as_ref().map(|o| o.params[0].base), Some(200.0));
}

#[test]
fn modulation_adds_octaves_to_freq_and_clears() {
    let mut w = one(WaveShape::Sine, 100.0);
    w.modulate(0, WaveParam::Freq.index() as u8, ModOp::Add(1.0));
    let osc = w.osc[0].as_ref().unwrap();
    assert!((osc.effective(WaveParam::Freq) - 200.0).abs() < 1e-3);
    w.advance_ramps();
    let osc = w.osc[0].as_ref().unwrap();
    assert_eq!(osc.effective(WaveParam::Freq), 100.0);
}

#[test]
fn a_frequency_past_nyquist_is_silent_for_saw_and_square() {
    for shape in [WaveShape::Saw, WaveShape::Square, WaveShape::Tri] {
        let mut w = one(shape, 15_000.0);
        let x = render(&mut w, 64, 2.0);
        assert!(x.iter().all(|v| *v == 0.0), "{shape:?}");
    }
}

#[test]
fn removed_and_unknown_waves_are_ignored() {
    let mut w = one(WaveShape::Sine, 100.0);
    w.remove(0);
    assert!(w.is_empty());
    assert_eq!(w.set(0, 0, 1.0, 5), 0);
    w.modulate(3, 0, ModOp::Add(1.0));
    w.bypass(7, true, true);
    w.hit(0);
    assert_eq!(w.render(0, 1.0, SR, test_table()), 0.0);
}

#[test]
fn switching_shape_while_playing_starts_clean() {
    for from in [WaveShape::Sine, WaveShape::Saw, WaveShape::Square] {
        let mut w = one(from, 200.0);
        // not a whole number of cycles (240 frames each)
        render(&mut w, 24_117, 1.0);
        w.set_shape(0, WaveShape::Tri);
        let x = render(&mut w, 2_399, 1.0);
        assert!(x.iter().all(|v| v.abs() <= 1.1), "{from:?}");
        let mean = x.iter().sum::<f32>() / x.len() as f32;
        assert!(mean.abs() < 0.05, "{from:?}: {mean}");
    }
}

#[test]
fn tri_is_right_after_a_stay_past_nyquist() {
    let mut w = one(WaveShape::Tri, 100.0);
    render(&mut w, 103, 1.0);
    // a ratio that puts it past Nyquist, then back
    render(&mut w, 1_001, 300.0);
    let x = render(&mut w, 2_399, 1.0);
    assert!(x.iter().all(|v| v.abs() <= 1.1));
    let mean = x.iter().sum::<f32>() / x.len() as f32;
    assert!(mean.abs() < 0.05, "{mean}");
}

#[test]
fn sine_past_nyquist_is_silent_but_noise_is_not() {
    let mut w = one(WaveShape::Sine, 20_000.0);
    assert!(render(&mut w, 64, 2.0).iter().all(|v| *v == 0.0));
    let mut w = one(WaveShape::Sine, 20_000.0);
    assert!(render(&mut w, 64, 1.0).iter().any(|v| *v != 0.0));
    let mut w = one(WaveShape::Noise, 20_000.0);
    assert!(render(&mut w, 64, 2.0).iter().any(|v| *v != 0.0));
}

// 64 saws (8 waves of 8 choir instances) for a second of audio
#[test]
#[ignore = "a timing: cargo test --release elliptic_cpu -- --ignored --nocapture"]
fn elliptic_cpu_against_polyblep() {
    use std::time::Instant;
    let table = BlepTable::new(SR);
    let mut w = Waves::new();
    for id in 0..8u8 {
        let mut p = wave_defaults();
        p[WaveParam::Freq.index()] = 110.0 + 13.0 * f32::from(id);
        w.insert(id, WaveShape::Saw, p, true);
    }
    let t = Instant::now();
    let mut sum = 0.0f32;
    for _ in 0..48_000 {
        w.now += 1;
        for inst in 0..8 {
            sum += w.render(inst, 1.0, SR, &table);
        }
    }
    let elliptic = t.elapsed();
    // the polyBLEP saw this replaced, 64 of them
    let poly = |t: f64, dt: f64| {
        if t < dt {
            let x = t / dt;
            x + x - x * x - 1.0
        } else if t > 1.0 - dt {
            let x = (t - 1.0) / dt;
            x * x + x + x + 1.0
        } else {
            0.0
        }
    };
    let mut phase = [0.0f64; 64];
    let t = Instant::now();
    let mut sum2 = 0.0f32;
    for _ in 0..48_000 {
        for (i, ph) in phase.iter_mut().enumerate() {
            let dt = f64::from(110.0 + 13.0 * (i / 8) as f32) / SR;
            sum2 += (2.0 * *ph - 1.0 - poly(*ph, dt)) as f32;
            *ph = (*ph + dt).fract();
        }
    }
    let polyblep = t.elapsed();
    println!("64 saws, 1 s of audio: elliptic {elliptic:?}, polyBLEP {polyblep:?} ({sum} {sum2})");
}

// the biggest |x| of a slice
fn peak_of(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

#[test]
fn an_onset_from_silence_is_band_limited() {
    // (shape, Hz): the first 50 ms stay near the steady waveform, and the
    // offset has gone by 10 ms
    for (shape, f) in [
        (WaveShape::Tri, 6000.0),
        (WaveShape::Tri, 1000.0),
        (WaveShape::Saw, 1000.0),
        (WaveShape::Square, 1000.0),
        (WaveShape::Sine, 1000.0),
    ] {
        let mut w = one(shape, f);
        let x = render(&mut w, 48_000, 1.0);
        let steady = peak_of(&x[24_000..]);
        let early = peak_of(&x[..2400]);
        let mean = x[480..2400].iter().sum::<f32>() / 1920.0;
        println!(
            "{shape:?} {f}: steady {steady:.3}, first 50 ms {early:.3}, mean 10-50 ms {mean:.4}"
        );
        assert!(
            early <= steady * 1.15,
            "{shape:?} {f}: {early} against {steady}"
        );
        assert!(mean.abs() < 0.02, "{shape:?} {f}: mean {mean}");
        // the start is spread, not a step
        let jump = naive(shape, 0.0, 0.5, None).abs() as f32;
        if jump > 0.3 {
            assert_spread(&from_silence(&x[..8]), jump, &format!("{shape:?} {f}"));
        }
    }
}

#[test]
fn an_onset_mid_cycle_is_spread() {
    // the waveform starts in the middle of its cycle: from 0 to where it is
    for (shape, phase) in [
        (WaveShape::Saw, 0.37),
        (WaveShape::Saw, 0.0),
        (WaveShape::Square, 0.2),
        (WaveShape::Tri, 0.1),
        (WaveShape::Sine, 0.25),
    ] {
        let mut p = wave_defaults();
        p[WaveParam::Freq.index()] = 500.0;
        p[WaveParam::Phase.index()] = phase as f32;
        let mut w = Waves::new();
        w.insert(0, shape, p, true);
        let x = render(&mut w, 400, 1.0);
        let jump = naive(shape, phase, 0.5, None).abs() as f32;
        assert_spread(&from_silence(&x[..8]), jump, &format!("{shape:?} {phase}"));
    }
}

#[test]
fn a_wave_that_was_silent_for_a_frame_starts_again_as_an_onset() {
    // stopped (a frame with no render) and started again mid-cycle
    for shape in [WaveShape::Saw, WaveShape::Square, WaveShape::Tri] {
        let mut w = one(shape, 500.0);
        render(&mut w, 658, 1.0);
        w.now += 7;
        w.bypass(0, true, false);
        w.bypass(0, false, false);
        let at = w.osc[0].as_ref().unwrap().phase[0];
        let jump = naive(shape, at, 0.5, None).abs() as f32;
        let b = render(&mut w, 300, 1.0);
        assert!(jump > 0.3, "{shape:?}: a test phase with something to see");
        assert_spread(&from_silence(&b[..8]), jump, &format!("{shape:?}"));
        assert!(peak_of(&b) < 1.5);
    }
}

// `x` with the silence before it (a wave that starts from nothing)
fn from_silence(x: &[f32]) -> Vec<f32> {
    std::iter::once(0.0).chain(x.iter().copied()).collect()
}

// an onset or a jump of `jump` is band-limited when no step of its first
// samples is as big as the jump itself (a hard jump is one step of `jump`;
// the filter spreads it to about half)
fn assert_spread(x: &[f32], jump: f32, what: &str) {
    let step = biggest_step(x);
    assert!(
        step < 0.7 * jump,
        "{what}: a step of {step} for a jump of {jump}"
    );
}

#[test]
fn a_change_of_frequency_or_ratio_does_not_jump() {
    // 100 Hz to 2000 Hz in one sample, by the frequency setting or by the
    // voice's ratio (`rate=`, `pitch=`, a `melo` note): the slope the filter
    // is told of moves with it, so the output has no step of its own
    for shape in [WaveShape::Saw, WaveShape::Tri] {
        for by_ratio in [false, true] {
            let mut w = one(shape, 100.0);
            let mut out = render(&mut w, 3_001, 1.0);
            let ratio = match by_ratio {
                true => 20.0,
                false => {
                    w.set(0, WaveParam::Freq.index() as u8, 2_000.0, 0);
                    1.0
                }
            };
            out.extend(render(&mut w, 2_000, ratio));
            let across = biggest_step(&out[2_995..3_004]);
            let mut steps: Vec<f32> = out[3_200..4_800]
                .windows(2)
                .map(|p| (p[1] - p[0]).abs())
                .collect();
            steps.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let typical = steps[steps.len() / 2];
            println!("{shape:?} by_ratio {by_ratio}: across {across}, typical {typical}");
            assert!(
                across < 0.5 * typical,
                "{shape:?} {by_ratio}: {across} vs {typical}"
            );
        }
    }
}

// ---- tables (WAVb) ----

fn sine_cycle(n: usize, h: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (std::f64::consts::TAU * (h * i) as f64 / n as f64).sin() as f32)
        .collect()
}

fn saw_cycle(n: usize) -> Vec<f32> {
    (0..n).map(|i| 2.0 * i as f32 / n as f32 - 1.0).collect()
}

fn table_wave(table: &Arc<WaveTable>, freq: f32) -> Waves {
    let mut w = Waves::new();
    let mut p = wave_defaults();
    p[WaveParam::Freq.index()] = freq;
    w.insert(0, WaveShape::Table, p, true);
    assert!(w.set_table(0, Arc::clone(table)).is_ok());
    w
}

fn pos_index() -> u8 {
    WaveParam::Pos.index() as u8
}

// the DFT magnitude of one table cycle at harmonic `h`, per sample
fn cycle_bin(x: &[f32], h: usize) -> f64 {
    let n = x.len();
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let a = std::f64::consts::TAU * (h * i % n) as f64 / n as f64;
        re += f64::from(*v) * a.cos();
        im -= f64::from(*v) * a.sin();
    }
    (re * re + im * im).sqrt() / n as f64
}

// the total harmonic distortion (harmonics 2..=8 under 20 kHz) and the worst
// partial that is no harmonic (up to 19 kHz), both in dB under the
// fundamental, of `x` (a rendering at `f0`)
fn thd_and_floor(x: &[f32], f0: f64) -> (f64, f64) {
    let hz_per_bin = SR / x.len() as f64;
    let at = |hz: f64| {
        let k = (hz / hz_per_bin).round() as usize;
        (k - 3..=k + 3).map(|k| bin(x, k)).fold(0.0, f64::max)
    };
    let fund = at(f0);
    let harmonics: f64 = (2..=8)
        .filter(|h| f64::from(*h) * f0 < 20_000.0)
        .map(|h| at(f64::from(h) * f0).powi(2))
        .sum::<f64>()
        .sqrt();
    let thd = 20.0 * (harmonics / fund).max(1e-12).log10();
    (thd, nonharmonic_db(x, f0))
}

// the worst bin from 100 Hz to 19 kHz that is not within 6 bins of a harmonic
// of `f0` (any harmonic under Nyquist: a table has many), in dB under the
// fundamental
fn nonharmonic_db(x: &[f32], f0: f64) -> f64 {
    let hz_per_bin = SR / x.len() as f64;
    let fund_bin = (f0 / hz_per_bin).round() as usize;
    let fund = (fund_bin - 3..=fund_bin + 3)
        .map(|k| bin(x, k))
        .fold(0.0, f64::max);
    let near_harmonic = |k: usize| {
        let hz = k as f64 * hz_per_bin;
        let h = (hz / f0).round().max(1.0);
        (hz - h * f0).abs() < 6.0 * hz_per_bin
    };
    let worst = ((100.0 / hz_per_bin) as usize..(19_000.0 / hz_per_bin) as usize)
        .filter(|k| !near_harmonic(*k))
        .map(|k| bin(x, k))
        .fold(0.0, f64::max);
    20.0 * (worst / fund).log10()
}

fn settled(w: &mut Waves, n: usize) -> Vec<f32> {
    render(w, 2000, 1.0);
    render(w, n, 1.0)
}

#[test]
fn table_from_one_cycle_sine_is_a_sine() {
    let table = Arc::new(WaveTable::build(&sine_cycle(2048, 1), 1, 1, 2048));
    let f0 = 440.0;
    let x = settled(&mut table_wave(&table, f0 as f32), 4096);
    let (thd, floor) = thd_and_floor(&x, f0);
    println!("table sine at 440 Hz: THD {thd:.1} dB, floor {floor:.1} dB");
    assert!(thd < -80.0, "THD {thd}");
    assert!(floor < -80.0, "floor {floor}");
    // the metric can fail: a nearest-sample read of the same cycle is a
    // staircase and is above the bound
    let cycle = table.cycle(0, 0);
    let stairs: Vec<f32> = (0..4096)
        .map(|i| cycle[((f64::from(i) * f0 / SR).fract() * 2048.0) as usize])
        .collect();
    let (_, stairs_floor) = thd_and_floor(&stairs, f0);
    println!("nearest read: floor {stairs_floor:.1} dB");
    assert!(stairs_floor > -80.0, "{stairs_floor}");
    // the amplitude is the source's, and the pitch is right
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!((peak - 1.0).abs() < 0.01, "{peak}");
    assert!((measured_hz(&x) - f0).abs() < 0.5);
}

#[test]
fn table_levels_have_no_partial_above_nyquist() {
    let n = 2048;
    let table = Arc::new(WaveTable::build(&saw_cycle(n), 1, 1, n));
    assert_eq!(table.levels(), 10);
    for k in 0..table.levels() {
        let cycle = table.cycle(0, k);
        let h = ((n / 2) >> k).saturating_sub(1).max(1);
        let fund = cycle_bin(cycle, 1);
        // the last kept harmonic is there (a saw has all of them) ...
        let last = cycle_bin(cycle, h) / fund;
        assert!(last > 0.3 / h as f64, "level {k}: harmonic {h} is {last}");
        // ... and nothing past it, up to the mirror half
        let worst = (h + 1..=n / 2)
            .map(|b| cycle_bin(cycle, b) / fund)
            .fold(0.0, f64::max);
        println!("level {k}: {h} harmonics, worst past them {worst:.2e}");
        assert!(worst < 1e-5, "level {k}: {worst}");
    }
    // at every frequency the level in use (the deeper of the two it fades
    // between) keeps only partials under 90 % of Nyquist
    for hz in [20.0, 100.0, 440.0, 1507.3, 5000.0, 10_000.0, 19_000.0] {
        let level = table.level_for(hz / SR);
        let used = (level.ceil() as usize).min(table.levels() - 1);
        let h = ((n / 2) >> used).saturating_sub(1).max(1);
        println!("{hz} Hz: level {level:.3} keeps {h} harmonics");
        assert!(
            hz * h as f64 <= 0.9 * SR / 2.0 + 1e-9,
            "{hz} Hz, level {used}: {h} harmonics"
        );
    }
    // the output has no partial the harmonics don't explain
    for f0 in [440.7, 1507.3, 5003.1, 10_000.0] {
        let x = settled(&mut table_wave(&table, f0 as f32), 4096);
        let (_, floor) = thd_and_floor(&x, f0);
        println!("saw table at {f0} Hz: non-harmonic floor {floor:.1} dB");
        assert!(floor < -60.0, "{f0} Hz: {floor} dB");
    }
}

fn biggest(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|p| (p[1] - p[0]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn pos_morph_is_continuous() {
    // 8 different cycles (harmonic i + 1 of a sine) from a source whose
    // length isn't a multiple of the slice count
    let src: Vec<f32> = (0..8usize)
        .flat_map(|i| sine_cycle(2003, i + 1))
        .take(16_003)
        .collect();
    let table = Arc::new(WaveTable::build(&src, 1, 8, 2048));
    let f0 = 300.0;
    // the waveform's own largest step: the busiest cycle, held still
    let own = (0..8)
        .map(|s| {
            let mut w = table_wave(&table, f0);
            w.set(0, pos_index(), s as f32 / 7.0, 0);
            biggest(&settled(&mut w, 2000))
        })
        .fold(0.0, f32::max);
    let mut w = table_wave(&table, f0);
    render(&mut w, 2000, 1.0);
    // a glide across all of them
    w.set(0, pos_index(), 1.0, 24_000);
    let glide = biggest(&render(&mut w, 24_000, 1.0));
    // a jump from the first to the last, a quarter of the way into a cycle
    // (where cycle 0 is 1 and cycle 7 is 0), is a step of its own
    let mut w = table_wave(&table, f0);
    let mut x = render(&mut w, 40, 1.0);
    w.set(0, pos_index(), 1.0, 0);
    x.extend(render(&mut w, 40, 1.0));
    let jump = biggest(&x);
    println!("own step {own:.4}, glide {glide:.4}, jump {jump:.4}");
    assert!(glide <= own * 1.02, "{glide} against {own}");
    assert!(
        jump > own * 1.2,
        "a jump must show as a step: {jump} against {own}"
    );
}

#[test]
fn level_crossfade_has_no_step() {
    let n = 2048;
    // every harmonic at the same size (a band-limited pulse) is the case in
    // which dropping a level changes the most
    let mut pulse = vec![0.0f32; n];
    pulse[0] = 1.0;
    let table = Arc::new(WaveTable::build(&pulse, 1, 1, n));
    let freqs = [
        100.0f32, 200.0, 400.0, 800.0, 1600.0, 3200.0, 6400.0, 8000.0,
    ];
    let own = freqs
        .iter()
        .map(|f| biggest(&settled(&mut table_wave(&table, *f), 2000)))
        .fold(0.0, f32::max);
    // 100 Hz up to 8 kHz in half a second
    let mut w = table_wave(&table, 100.0);
    render(&mut w, 2000, 1.0);
    w.set(0, WaveParam::Freq.index() as u8, 8000.0, 24_000);
    let step = biggest(&render(&mut w, 24_000, 1.0));
    println!("own step {own:.4}, glide {step:.4}");
    assert!(step <= own * 1.05, "{step} against {own}");
    // the level the reader asks for moves smoothly with the frequency (no
    // step to the next level), up and down; rounding it would step by 1
    let mut dt = 1e-4;
    let mut last = table.level_for(dt);
    let mut biggest_move = 0.0f32;
    while dt < 0.49 {
        dt *= 1.0005;
        let level = table.level_for(dt);
        assert!(level >= last, "level fell at {dt}: {last} then {level}");
        biggest_move = biggest_move.max(level - last);
        last = level;
    }
    println!("biggest move of the level between steps of 0.05 % in frequency: {biggest_move:.5}");
    assert!(biggest_move < 0.006, "{biggest_move}");
    assert_eq!(last, (table.levels() - 1) as f32);
    // and the sample is continuous in it
    for level in [0.0f32, 3.0, 5.5, 8.9] {
        let (a, b) = (
            table.sample(0.0, level, 0.0),
            table.sample(0.0, level + 0.001, 0.0),
        );
        assert!((a - b).abs() < 0.0007, "{a} {b}");
    }
}

#[test]
fn a_table_wave_without_a_table_is_silent() {
    let mut w = Waves::new();
    let mut p = wave_defaults();
    p[WaveParam::Freq.index()] = 440.0;
    w.insert(0, WaveShape::Table, p, true);
    assert!(render(&mut w, 500, 1.0).iter().all(|v| *v == 0.0));
    let table = Arc::new(WaveTable::build(&sine_cycle(256, 1), 1, 1, 256));
    assert!(w.set_table(0, table).unwrap().is_none());
    assert!(render(&mut w, 500, 1.0).iter().any(|v| v.abs() > 0.5));
}

#[test]
fn set_table_returns_the_old_one_and_remove_returns_the_last() {
    let a = Arc::new(WaveTable::build(&sine_cycle(256, 1), 1, 1, 256));
    let b = Arc::new(WaveTable::build(&sine_cycle(256, 2), 1, 1, 256));
    let mut w = table_wave(&a, 440.0);
    assert!(w.has_table(0));
    let old = w.set_table(0, Arc::clone(&b)).unwrap().unwrap();
    assert!(Arc::ptr_eq(&old, &a));
    // a wave that isn't there hands the table back
    assert!(w.set_table(3, Arc::clone(&a)).is_err());
    let last = w.remove(0).unwrap();
    assert!(Arc::ptr_eq(&last, &b));
    assert!(!w.has_table(0));
    assert!(w.remove(0).is_none());
}

#[test]
fn build_cuts_slices_and_averages_channels() {
    // two channels, the right the negative of the left: the mono mix is
    // silent; both the same is the sine
    let sine = sine_cycle(512, 1);
    let both: Vec<f32> = sine.iter().flat_map(|v| [*v, -*v]).collect();
    let t = WaveTable::build(&both, 2, 1, 256);
    assert!(t.cycle(0, 0).iter().all(|v| v.abs() < 1e-5));
    let same: Vec<f32> = sine.iter().flat_map(|v| [*v, *v]).collect();
    let t = WaveTable::build(&same, 2, 1, 256);
    let peak = t.cycle(0, 0).iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!((peak - 1.0).abs() < 0.02, "{peak}");
    // slices are cut at i * len / n: the first half a sine, the second a
    // sine twice as fast
    let mut src = sine_cycle(1000, 1);
    src.extend(sine_cycle(1000, 2));
    let t = WaveTable::build(&src, 1, 2, 1024);
    assert_eq!((t.slices(), t.frames(), t.levels()), (2, 1024, 9));
    let (a, b) = (t.cycle(0, 0), t.cycle(1, 0));
    assert!(cycle_bin(a, 1) > 0.49 && cycle_bin(a, 2) < 1e-4);
    assert!(cycle_bin(b, 2) > 0.49 && cycle_bin(b, 1) < 1e-4);
    // DC is removed
    let biased: Vec<f32> = sine_cycle(256, 1).iter().map(|v| v + 0.5).collect();
    let t = WaveTable::build(&biased, 1, 1, 256);
    let mean = t.cycle(0, 0).iter().sum::<f32>() / 256.0;
    assert!(mean.abs() < 1e-5, "{mean}");
}

#[test]
fn the_size_cap_counts_every_level() {
    assert_eq!(levels_for(2048), 10);
    assert_eq!(levels_for(256), 7);
    assert_eq!(levels_for(4096), 11);
    assert!(fits(8, 2048));
    // the language's own largest table fits
    assert!(fits(MAX_TABLE_SLICES, MAX_TABLE_FRAMES));
    // 16 M samples is the line
    assert!(fits(16 * 1024 * 1024 / (11 * 4096), 4096));
    assert!(!fits(1024, 4096));
    assert!(!fits(usize::MAX, 4096));
    assert!(frames_ok(256) && frames_ok(4096) && !frames_ok(1000) && !frames_ok(8192));
}

// ---- FM (WAVc) ----

// a carrier in slot 0 and its modulator (a sine, not heard) in slot 1
fn fm_pair(shape: WaveShape, carrier_hz: f32, mod_hz: f32, index: f32) -> Waves {
    let mut w = Waves::new();
    let mut p = wave_defaults();
    p[WaveParam::Freq.index()] = carrier_hz;
    p[WaveParam::Index.index()] = index;
    w.insert(0, shape, p, true);
    p[WaveParam::Freq.index()] = mod_hz;
    w.insert(1, WaveShape::Sine, p, true);
    w.set_out(1, false);
    w.set_fm(0, 1 << 1);
    w
}

// 1875 Hz at 48 kHz: 25.6 frames a cycle, so 4096 frames hold exactly 160
const F160: f32 = 1875.0;

fn settle_then(w: &mut Waves, warm: usize, n: usize) -> Vec<f32> {
    render(w, warm, 1.0);
    render(w, n, 1.0)
}

#[test]
fn index_zero_is_the_plain_carrier() {
    for shape in [
        WaveShape::Sine,
        WaveShape::Saw,
        WaveShape::Square,
        WaveShape::Tri,
    ] {
        let mut fm = fm_pair(shape, 777.0, 1234.0, 0.0);
        let mut plain = one(shape, 777.0);
        let (a, b) = (render(&mut fm, 3000, 1.0), render(&mut plain, 3000, 1.0));
        assert_eq!(a, b, "{shape:?}");
        // and with the index up it is not
        let mut fm = fm_pair(shape, 777.0, 1234.0, 1.0);
        assert_ne!(render(&mut fm, 3000, 1.0), b, "{shape:?}");
    }
}

// |J_n(x)|, the Bessel function of the first kind, by its power series
fn bessel_j(n: i32, x: f64) -> f64 {
    let n = n.unsigned_abs() as i32;
    let half = x / 2.0;
    let mut term = half.powi(n) / (1..=n).map(f64::from).product::<f64>();
    let mut sum = term;
    for k in 1..40 {
        term *= -half * half / (f64::from(k) * f64::from(k + n));
        sum += term;
    }
    sum.abs()
}

#[test]
fn bessel_sidebands_at_index_one() {
    // a carrier at bin 160 and a modulator at bin 400 (2.5 times it): the
    // sidebands (bins 160 + 400 n) don't sit on each other or on the carrier.
    // (At exactly 1:1 the sidebands land on the harmonics and interfere, so
    // the amplitudes are not the plain Bessel ones: see the next test.)
    let (cb, mb) = (160usize, 400usize);
    let bin_hz = 48_000.0 / 4096.0;
    let (carrier, modulator) = ((cb as f64 * bin_hz) as f32, (mb as f64 * bin_hz) as f32);
    let mut plain = one(WaveShape::Sine, carrier);
    let unmodulated = bin(&render(&mut plain, 4096, 1.0), cb);
    let mut w = fm_pair(WaveShape::Sine, carrier, modulator, 1.0);
    let x = render(&mut w, 4096, 1.0);
    let rel = |k: usize| bin(&x, k) / unmodulated;
    let at = |n: i32| (cb as i64 + mb as i64 * i64::from(n)).unsigned_abs() as usize;
    for (n, want) in [
        (0, 0.7652),
        (1, 0.4401),
        (-1, 0.4401),
        (2, 0.1149),
        (-2, 0.1149),
    ] {
        let got = rel(at(n));
        assert!(
            (got - want).abs() < 0.01,
            "sideband {n} at bin {}: {got} against {want}",
            at(n)
        );
        assert!((got - bessel_j(n, 1.0)).abs() < 0.01);
    }
    // the third pair is small but there
    assert!(
        (rel(at(3)) - bessel_j(3, 1.0)).abs() < 0.01,
        "{}",
        rel(at(3))
    );
}

#[test]
fn one_to_one_fm_is_the_modulated_sine_sample_for_sample() {
    // phase modulation by a sine of the same frequency, with the modulator
    // one frame early (the phase increment form), at an index where the
    // harmonics interfere
    let (f, index) = (1000.0f64, 2.0f64);
    let tau = std::f64::consts::TAU;
    let mut w = fm_pair(WaveShape::Sine, f as f32, f as f32, index as f32);
    let x = render(&mut w, 2000, 1.0);
    let dt = f / SR;
    for (n, got) in x.iter().enumerate().skip(1) {
        let m = (tau * (n - 1) as f64 * dt).sin();
        let want = (tau * (n as f64 * dt + index / tau * m)).sin();
        assert!(
            (f64::from(*got) - want).abs() < 2e-4,
            "{n}: {got} against {want}"
        );
    }
}

#[test]
fn modulator_with_out_no_is_not_heard() {
    // a modulator alone is silent
    let mut w = fm_pair(WaveShape::Sine, 500.0, 700.0, 2.0);
    w.bypass(0, true, true);
    assert!(render(&mut w, 2000, 1.0).iter().all(|s| *s == 0.0));
    // and beside a carrier that follows nothing, it adds nothing
    let mut w = fm_pair(WaveShape::Sine, 500.0, 700.0, 2.0);
    w.set_fm(0, 0);
    let mut plain = one(WaveShape::Sine, 500.0);
    assert_eq!(render(&mut w, 2000, 1.0), render(&mut plain, 2000, 1.0));
    // out=yes brings it into the mix
    w.set_out(1, true);
    let mixed = render(&mut w, 100, 1.0);
    let alone = render(&mut one(WaveShape::Sine, 500.0), 2100, 1.0);
    assert!(
        mixed
            .iter()
            .zip(&alone[2000..])
            .any(|(m, a)| (m - a).abs() > 0.1)
    );
}

#[test]
fn modulators_render_before_carriers() {
    let forward = render(&mut fm_pair(WaveShape::Saw, 600.0, 900.0, 3.0), 3000, 1.0);
    // the same pair with the ids the other way round
    let mut w = Waves::new();
    let mut p = wave_defaults();
    p[WaveParam::Index.index()] = 3.0;
    p[WaveParam::Freq.index()] = 900.0;
    w.insert(0, WaveShape::Sine, p, true);
    p[WaveParam::Freq.index()] = 600.0;
    w.insert(1, WaveShape::Saw, p, true);
    w.set_out(0, false);
    w.set_fm(1, 1 << 0);
    assert_eq!(render(&mut w, 3000, 1.0), forward);
    // the order has the modulator first, whichever slot holds it
    let mut w = fm_pair(WaveShape::Saw, 600.0, 900.0, 3.0);
    assert_eq!(&w.order[..2], &[1, 0]);
    // and a chain 0 <- 1 <- 2 is evaluated 2, 1, 0
    let mut p = wave_defaults();
    p[WaveParam::Freq.index()] = 300.0;
    w.insert(2, WaveShape::Sine, p, true);
    w.set_fm(1, 1 << 2);
    assert_eq!(&w.order[..3], &[2, 1, 0]);
    // a removed modulator frees what followed it
    w.remove(2);
    assert_eq!(w.modulators_of(1), 0);
    assert_eq!(usize::from(w.count), 2);
}

// the power of every bin up to 20 kHz that is not a multiple of
// `harmonic_bins` (where the signal's own partials sit), less five bins either
// side of each: the window's main lobe is wider than a bin
fn offgrid_power(x: &[f32], harmonic_bins: usize) -> f64 {
    (1..x.len() * 20 / 48)
        .filter(|k| (k + 5) % harmonic_bins > 10)
        .map(|k| bin(x, k).powi(2))
        .sum()
}

// a naive saw (no filter) read by the same phase increments the engine uses
fn naive_fm_saw(f: f64, mod_f: f64, index: f64, n: usize, warm: usize) -> Vec<f32> {
    let tau = std::f64::consts::TAU;
    let (dt, mut phase, mut pm_prev) = (f / SR, 0.0f64, 0.0f64);
    let mut out = Vec::new();
    for i in 0..warm + n {
        if i >= warm {
            out.push((2.0 * phase - 1.0) as f32);
        }
        let m = f64::from((tau * mod_f / SR * i as f64).sin() as f32);
        let pm = index / tau * m;
        phase += dt + pm - pm_prev;
        pm_prev = pm;
        phase -= phase.floor();
    }
    out
}

#[test]
fn fm_saw_is_band_limited() {
    // 1:1 at 1875 Hz, index 2: every partial is a multiple of 1875 Hz, so
    // whatever sits between them is an alias
    let mut w = fm_pair(WaveShape::Saw, F160, F160, 2.0);
    let x = settle_then(&mut w, 4096, 4096);
    let naive = naive_fm_saw(f64::from(F160), f64::from(F160), 2.0, 4096, 4096);
    let (engine, reference) = (offgrid_power(&x, 160), offgrid_power(&naive, 160));
    let db = 10.0 * (engine / reference).log10();
    println!("fm saw: aliases {db:.1} dB against the naive saw");
    assert!(x.iter().all(|s| s.is_finite()));
    assert!(db < -20.0, "{db} dB");
}

#[test]
fn backwards_phase_crossing_is_band_limited() {
    // a modulation so deep and fast that the read phase runs backwards
    let (f, mf, index) = (F160 / 2.0, F160, 6.0f32);
    let dt = f64::from(f) / SR;
    let steepest = f64::from(index) * f64::from(mf) / SR;
    assert!(
        dt - steepest < 0.0 && dt + steepest < 0.5,
        "dt_eff goes negative, never past 0.5"
    );
    let mut w = fm_pair(WaveShape::Saw, f, mf, index);
    let x = settle_then(&mut w, 4096, 4096);
    assert!(x.iter().all(|s| s.is_finite() && s.abs() < 3.0));
    let naive = naive_fm_saw(f64::from(f), f64::from(mf), f64::from(index), 4096, 4096);
    // every partial is a multiple of 937.5 Hz (80 bins)
    let (engine, reference) = (offgrid_power(&x, 80), offgrid_power(&naive, 80));
    let db = 10.0 * (engine / reference).log10();
    println!("backwards fm saw: aliases {db:.1} dB against the naive saw");
    assert!(db < -10.0, "{db} dB");
}

#[test]
fn ratio_mode_is_a_multiple_of_the_voices_note() {
    // ratio=2 sounds at 2 x 220 Hz, and the voice's ratio (a pitch of 12st
    // is 2) multiplies it: 880 Hz
    let mut w = one(WaveShape::Sine, 1234.0);
    w.set_ratio_mode(0, true);
    w.set(0, WaveParam::Ratio.index() as u8, 2.0, 0);
    let plain = measured_hz(&render(&mut w, 9600, 1.0));
    assert!((plain - 440.0).abs() < 1.0, "{plain}");
    let mut w = one(WaveShape::Sine, 1234.0);
    w.set_ratio_mode(0, true);
    w.set(0, WaveParam::Ratio.index() as u8, 2.0, 0);
    let up = measured_hz(&render(&mut w, 9600, 2.0));
    assert!((up - 880.0).abs() < 1.5, "{up}");
    // freq mode again: the absolute frequency
    w.set_ratio_mode(0, false);
    let abs = measured_hz(&render(&mut w, 9600, 1.0));
    assert!((abs - 1234.0).abs() < 2.0, "{abs}");
}

#[test]
fn ratio_and_freq_modulation_move_in_octaves() {
    let mut w = one(WaveShape::Sine, 100.0);
    w.set_ratio_mode(0, true);
    w.set(0, WaveParam::Ratio.index() as u8, 1.0, 0);
    // an lfo on ratio, one octave up
    w.modulate(0, WaveParam::Ratio.index() as u8, ModOp::Add(1.0));
    let a = {
        w.now += 1;
        w.render(0, 1.0, SR, test_table());
        w.osc[0].as_ref().map(Osc::base_hz)
    };
    assert_eq!(a, Some(440.0));
    w.advance_ramps();
    // and on freq, which still multiplies a ratio wave's frequency
    w.modulate(0, WaveParam::Freq.index() as u8, ModOp::Add(1.0));
    assert_eq!(w.osc[0].as_ref().map(Osc::base_hz), Some(440.0));
    w.advance_ramps();
    assert_eq!(w.osc[0].as_ref().map(Osc::base_hz), Some(220.0));
}

#[test]
fn stop_modulator_plays_plain_carrier() {
    let mut w = fm_pair(WaveShape::Sine, 500.0, 700.0, 3.0);
    let modulated = render(&mut w, 4800, 1.0);
    w.bypass(1, true, true);
    // the modulator is silent from here: one step of the phase when its
    // contribution goes, then a plain sine
    render(&mut w, 2, 1.0);
    let x = render(&mut w, 9600, 1.0);
    assert!((measured_hz(&x) - 500.0).abs() < 1.0);
    assert!(x.iter().all(|s| s.abs() <= 1.0 + 1e-6));
    assert!(x.iter().map(|s| s.abs()).fold(0.0, f32::max) > 0.99);
    let early = measured_hz(&modulated[..2400]);
    assert!(
        (early - 500.0).abs() > 5.0,
        "it was modulated before: {early}"
    );
}

#[test]
fn choir_instances_modulate_their_own() {
    // two instances at different ratios, rendered interleaved, each match
    // an instance rendered alone: a carrier reads its own modulator instance
    let ratios = [1.0, 1.37];
    let mut both = fm_pair(WaveShape::Sine, 400.0, 530.0, 2.5);
    let mut x = [Vec::new(), Vec::new()];
    for _ in 0..3000 {
        both.now += 1;
        for (i, r) in ratios.iter().enumerate() {
            x[i].push(both.render(i, *r, SR, test_table()));
        }
    }
    for (i, r) in ratios.iter().enumerate() {
        let mut alone = fm_pair(WaveShape::Sine, 400.0, 530.0, 2.5);
        assert_eq!(x[i], render(&mut alone, 3000, *r), "instance {i}");
    }
    assert_ne!(x[0], x[1]);
}

#[test]
fn a_retrigger_of_a_modulated_carrier_stays_finite_and_restarts() {
    let mut w = fm_pair(WaveShape::Saw, 300.0, 450.0, 4.0);
    render(&mut w, 1000, 1.0);
    w.hit(0);
    let x = render(&mut w, 1000, 1.0);
    assert!(x.iter().all(|s| s.is_finite() && s.abs() < 3.0));
}

#[test]
fn set_fm_refuses_a_loop_and_keeps_the_link() {
    let mut w = fm_pair(WaveShape::Sine, 500.0, 700.0, 2.0);
    // 0 follows 1; 1 following 0 would close a loop
    w.set_fm(1, 1 << 0);
    assert_eq!(w.modulators_of(1), 0);
    assert_eq!(w.modulators_of(0), 1 << 1);
    // and a longer one: 0 <- 1 <- 2, then 2 following 0
    let mut p = wave_defaults();
    p[WaveParam::Freq.index()] = 300.0;
    w.insert(2, WaveShape::Sine, p, true);
    w.set_fm(1, 1 << 2);
    w.set_fm(2, 1 << 0);
    assert_eq!(w.modulators_of(2), 0);
    assert_eq!(&w.order[..3], &[2, 1, 0]);
}

#[test]
fn a_retrigger_reads_the_table_at_the_same_level_whatever_the_sign_of_dt() {
    let table = Arc::new(WaveTable::build(&saw_cycle(2048), 1, 1, 2048));
    let run = |dt: f64| {
        let mut w = table_wave(&table, 300.0);
        render(&mut w, 200, 1.0);
        w.osc[0].as_mut().unwrap().dt[0] = dt;
        w.hit(0);
        render(&mut w, 8, 1.0)
    };
    assert_eq!(run(0.3), run(-0.3));
}

// ---- feedback and several modulators (WAVd) ----

fn with_fb(shape: WaveShape, freq: f32, fb: f32) -> Waves {
    let mut w = Waves::new();
    let mut p = wave_defaults();
    p[WaveParam::Freq.index()] = freq;
    p[WaveParam::Fb.index()] = fb;
    w.insert(0, shape, p, true);
    w
}

#[test]
fn fb_zero_is_the_plain_wave() {
    for shape in [
        WaveShape::Sine,
        WaveShape::Saw,
        WaveShape::Square,
        WaveShape::Tri,
    ] {
        let a = render(&mut with_fb(shape, 777.0, 0.0), 3000, 1.0);
        let b = render(&mut one(shape, 777.0), 3000, 1.0);
        assert_eq!(a, b, "{shape:?}");
        let c = render(&mut with_fb(shape, 777.0, 1.0), 3000, 1.0);
        assert_ne!(c, b, "{shape:?}");
    }
    // and the sine is the closed form
    let x = render(&mut with_fb(WaveShape::Sine, 777.0, 0.0), 2000, 1.0);
    for (n, got) in x.iter().enumerate() {
        let want = (std::f64::consts::TAU * n as f64 * 777.0 / SR).sin();
        assert!((f64::from(*got) - want).abs() < 2e-4, "{n}");
    }
}

#[test]
fn fb_brightens_a_sine() {
    let bin_hz = 48_000.0 / 4096.0;
    let f = (160.0 * bin_hz) as f32;
    let harmonics = |fb: f32| {
        let mut w = with_fb(WaveShape::Sine, f, fb);
        let x = settle_then(&mut w, 4096, 4096);
        [bin(&x, 160), bin(&x, 320), bin(&x, 480)]
    };
    let (low, high) = (harmonics(0.3), harmonics(1.0));
    let upper = |h: [f64; 3]| h[1] * h[1] + h[2] * h[2];
    assert!(upper(high) > 2.0 * upper(low), "{low:?} {high:?}");
    // saw-like: the harmonics fall with order
    assert!(high[0] > high[1] && high[1] > high[2], "{high:?}");
    // and the deepest feedback stays finite and bounded
    let mut w = with_fb(WaveShape::Sine, f, 4.0);
    let x = render(&mut w, 20_000, 1.0);
    assert!(x.iter().all(|s| s.is_finite() && s.abs() < 2.0));
}

// a carrier in slot 0 (500 Hz) and two sine modulators (slots 1 and 2, not
// heard) at `hz_a` and `hz_b`; the second at `gain_b`
fn two_mods(index: f32, hz_a: f32, hz_b: f32, gain_b: f32) -> Waves {
    let mut w = Waves::new();
    let mut p = wave_defaults();
    p[WaveParam::Freq.index()] = 500.0;
    p[WaveParam::Index.index()] = index;
    w.insert(0, WaveShape::Sine, p, true);
    p[WaveParam::Freq.index()] = hz_a;
    w.insert(1, WaveShape::Sine, p, true);
    p[WaveParam::Freq.index()] = hz_b;
    p[WaveParam::Gain.index()] = gain_b;
    w.insert(2, WaveShape::Sine, p, true);
    w.set_out(1, false);
    w.set_out(2, false);
    w
}

#[test]
fn two_modulators_sum() {
    let index = 1.5f64;
    let tau = std::f64::consts::TAU;
    let mut w = two_mods(index as f32, 700.0, 1100.0, 1.0);
    w.set_fm(0, 0b110);
    let x = render(&mut w, 2000, 1.0);
    for (n, got) in x.iter().enumerate().skip(1) {
        let t = (n - 1) as f64 / SR;
        let m = (tau * 700.0 * t).sin() + (tau * 1100.0 * t).sin();
        let want = (tau * (n as f64 * 500.0 / SR + index / tau * m)).sin();
        assert!(
            (f64::from(*got) - want).abs() < 2e-4,
            "{n}: {got} against {want}"
        );
    }
}

#[test]
fn modulator_gain_sets_its_share() {
    let mut both = two_mods(2.0, 700.0, 1100.0, 0.0);
    both.set_fm(0, 0b110);
    let mut one_mod = two_mods(2.0, 700.0, 1100.0, 0.0);
    one_mod.set_fm(0, 0b010);
    assert_eq!(
        render(&mut both, 3000, 1.0),
        render(&mut one_mod, 3000, 1.0)
    );
    // with gain it is not the same
    let mut loud = two_mods(2.0, 700.0, 1100.0, 1.0);
    loud.set_fm(0, 0b110);
    assert_ne!(
        render(&mut loud, 3000, 1.0),
        render(&mut one_mod, 3000, 1.0)
    );
}

#[test]
fn remove_one_of_two_modulators_keeps_the_other() {
    // removing wave 2 leaves the carrier exactly as taking it out of the set
    // would: the one remaining modulator keeps working
    let mut removed = two_mods(1.5, 700.0, 1100.0, 1.0);
    removed.set_fm(0, 0b110);
    let mut dropped = two_mods(1.5, 700.0, 1100.0, 1.0);
    dropped.set_fm(0, 0b110);
    render(&mut removed, 1000, 1.0);
    render(&mut dropped, 1000, 1.0);
    removed.remove(2);
    dropped.set_fm(0, 0b010);
    assert_eq!(removed.modulators_of(0), 0b010);
    let (x, y) = (
        render(&mut removed, 2000, 1.0),
        render(&mut dropped, 2000, 1.0),
    );
    assert_eq!(x, y);
    // and it is still modulated: not the plain carrier
    let mut plain = one(WaveShape::Sine, 500.0);
    render(&mut plain, 1000, 1.0);
    let p = render(&mut plain, 2000, 1.0);
    assert!(x.iter().zip(&p).any(|(a, b)| (a - b).abs() > 0.1));
    // the last one gone: a plain sine again
    removed.remove(1);
    assert_eq!(removed.modulators_of(0), 0);
    let z = render(&mut removed, 3000, 1.0);
    let hz = measured_hz(&z[500..]);
    assert!((hz - 500.0).abs() < 1.0, "{hz}");
    assert!(z.iter().all(|s| s.abs() <= 1.001));
}

#[test]
fn engine_refuses_mask_loop_from_waiting_line() {
    // 2 follows 0; then 0 following {1, 2} closes a loop through the second
    // bit of the set (1 follows nothing)
    let mut w = two_mods(1.0, 700.0, 1100.0, 1.0);
    w.set_fm(2, 0b001);
    w.set_fm(0, 0b110);
    assert_eq!(w.modulators_of(0), 0);
    assert_eq!(w.modulators_of(2), 0b001);
    // the same through a longer chain: 1 follows 2 follows 0, then 0 {1, 2}
    w.set_fm(1, 0b100);
    w.set_fm(0, 0b010);
    assert_eq!(w.modulators_of(0), 0);
    // a set that names the wave itself is refused whole
    w.set_fm(1, 0b110);
    assert_eq!(w.modulators_of(1), 0b100);
    // and a set with no loop is taken
    w.set_fm(0, 0);
    let mut ok = two_mods(1.0, 700.0, 1100.0, 1.0);
    ok.set_fm(0, 0b110);
    assert_eq!(ok.modulators_of(0), 0b110);
    assert_eq!(usize::from(ok.count), 3);
}

fn largest_step(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn removing_a_modulator_does_not_click() {
    for by_remove in [true, false] {
        let mut w = two_mods(2.0, 700.0, 300.0, 1.0);
        w.set_fm(0, 0b110);
        render(&mut w, 2000, 1.0);
        let before = render(&mut w, 1024, 1.0);
        // the sample before the change joins the steps across it
        let mut after = vec![*before.last().unwrap()];
        if by_remove {
            w.remove(2);
        } else {
            w.set_fm(0, 0b010);
        }
        after.extend(render(&mut w, 64, 1.0));
        assert!(
            largest_step(&after) <= 1.1 * largest_step(&before),
            "{by_remove}: {} against {}",
            largest_step(&after),
            largest_step(&before)
        );
    }
}
