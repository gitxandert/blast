//! HPR's tests: the `resample` setting and the Elliptic-BLEP read (`iir`)
//! against Hermite (`cubic`). Every figure is measured on a rendered track,
//! with test lengths that are not whole numbers of cycles.

use std::f64::consts::TAU;

use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::ResampleMode;
use crate::audio_processing::commands::lang::Code;
use crate::audio_processing::commands::{CmdErr, CmdProcessor};
use crate::audio_processing::engine::{Conductor, Voice};
use crate::audio_processing::test_util::{render, run, setup};
use crate::file_parsing::decode_helpers::AudioFile;
use std::sync::Arc;

const SR: f64 = 48_000.0;
// the level of the test tracks (of full scale)
const LEVEL: f64 = 20_000.0 / 32_768.0;

fn track_of(
    name: &str,
    channels: usize,
    frames: usize,
    f: impl Fn(usize, usize) -> f64,
) -> AudioFile {
    let samples: Vec<i16> = (0..frames)
        .flat_map(|n| (0..channels).map(move |c| (n, c)))
        .map(|(n, c)| (f(n, c) * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16)
        .collect();
    AudioFile::new(
        name,
        "wav",
        48000,
        channels as u32,
        16,
        crate::audio_processing::test_util::pcm(&samples),
    )
}

fn sine_track(freq: f64, frames: usize) -> AudioFile {
    track_of("t", 1, frames, |n, _| {
        LEVEL * (TAU * freq * n as f64 / SR).sin()
    })
}

// a voice `v` on `track`, `settings` after its name, started
fn rig(track: AudioFile, channels: usize, settings: &str) -> (CmdProcessor, Conductor) {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(channels, vec![track]);
    run(
        &mut cp,
        &mut c,
        &format!("voice v from \"t\" on 1000ms {settings}"),
    );
    run(&mut cp, &mut c, "start v");
    (cp, c)
}

fn play(track: AudioFile, settings: &str, frames: usize) -> Vec<f32> {
    let (_cp, mut c) = rig(track, 1, settings);
    render(&mut c, 1, frames)
}

// the amplitude of `f` Hz in `x` (Hann window), as a fraction of `LEVEL`
fn level_at(x: &[f32], f: f64) -> f64 {
    let n = x.len();
    let (mut re, mut im, mut sum) = (0.0, 0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * (TAU * (i as f64 + 0.5) / n as f64).cos();
        let ph = TAU * f * i as f64 / SR;
        re += w * f64::from(*v) * ph.cos();
        im += w * f64::from(*v) * ph.sin();
        sum += w;
    }
    2.0 * (re * re + im * im).sqrt() / sum / LEVEL
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

// what is left of `x` when the best-fitting sinusoid of `f` Hz is taken out,
// as an rms against the sinusoid's amplitude
fn residual_db(x: &[f32], f: f64) -> f64 {
    let (mut ss, mut cc, mut sc, mut xs, mut xc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let (s, c) = (TAU * f * i as f64 / SR).sin_cos();
        ss += s * s;
        cc += c * c;
        sc += s * c;
        xs += f64::from(*v) * s;
        xc += f64::from(*v) * c;
    }
    let det = ss * cc - sc * sc;
    let (a, b) = ((xs * cc - xc * sc) / det, (xc * ss - xs * sc) / det);
    let mut err = 0.0;
    for (i, v) in x.iter().enumerate() {
        let (s, c) = (TAU * f * i as f64 / SR).sin_cos();
        let e = f64::from(*v) - a * s - b * c;
        err += e * e;
    }
    let amp = (a * a + b * b).sqrt();
    db((err / x.len() as f64).sqrt() / amp)
}

// the steady part of a render: past the filter's onset, off a whole cycle
fn steady(x: &[f32]) -> &[f32] {
    &x[2_000..]
}

#[test]
fn iir_rate2_removes_alias() {
    // 15 kHz played at 2x is 30 kHz, which folds to 18 kHz
    let out = |mode: &str| {
        play(
            sine_track(15_000.0, 40_000),
            &format!("rate=2 resample={mode}"),
            17_001,
        )
    };
    let iir = db(level_at(steady(&out("iir")), 18_000.0));
    let cubic = db(level_at(steady(&out("cubic")), 18_000.0));
    println!("rate 2, 15 kHz: 18 kHz bin iir {iir:.1} dB, cubic {cubic:.1} dB");
    assert!(iir < -70.0, "iir: {iir} dB");
    // the measure can fail: Hermite lets it through
    assert!(cubic > -30.0, "cubic: {cubic} dB");
}

#[test]
fn iir_rate3_removes_alias_and_keeps_the_level_below() {
    // 10 kHz at 3x is 30 kHz (folds to 18 kHz); 3 kHz at 3x is 9 kHz, kept
    let alias = play(sine_track(10_000.0, 60_000), "rate=3 resample=iir", 17_001);
    let a = db(level_at(steady(&alias), 18_000.0));
    let kept = play(sine_track(3_000.0, 60_000), "rate=3 resample=iir", 17_001);
    let k = db(level_at(steady(&kept), 9_000.0));
    println!("rate 3: alias {a:.1} dB, 9 kHz level {k:.2} dB");
    assert!(a < -70.0, "alias {a} dB");
    assert!(k.abs() < 0.25, "level {k} dB");
}

#[test]
fn iir_rate1_transparent() {
    // within 0.25 dB (0.5 at 18 kHz) of the source, and nothing else added
    for (f, tol) in [
        (1_000.0, 0.25),
        (5_000.0, 0.25),
        (10_000.0, 0.25),
        (15_000.0, 0.25),
        (18_000.0, 0.5),
    ] {
        let out = play(sine_track(f, 20_000), "rate=1 resample=iir", 17_001);
        let x = steady(&out);
        let level = db(level_at(x, f));
        let rest = residual_db(x, f);
        println!("rate 1, {f} Hz: {level:+.3} dB, rest {rest:.1} dB");
        assert!(level.abs() < tol, "{f} Hz: {level} dB");
        assert!(rest < -80.0, "{f} Hz: rest {rest} dB");
    }
}

#[test]
fn rate1_without_the_pre_emphasis_would_droop() {
    // the shape the figures above rest on: Hermite at 1x is exact, so the
    // droop of the line through the samples (-1.4 dB at 10 kHz) is the
    // resampler's own to cancel
    let out = play(
        sine_track(10_000.0, 20_000),
        "rate=1 resample=cubic",
        17_001,
    );
    let level = db(level_at(steady(&out), 10_000.0));
    assert!(level.abs() < 0.05, "cubic: {level} dB");
}

#[test]
fn iir_backwards_is_as_transparent() {
    // rate -1 from the end of the track
    let (_cp, mut c) = rig(sine_track(5_000.0, 20_000), 1, "rate=-1 resample=iir");
    // let it run past the start of the loop of frames it reads
    let out = render(&mut c, 1, 17_001);
    let x = steady(&out);
    let level = db(level_at(x, 5_000.0));
    println!("rate -1, 5 kHz: {level:+.3} dB");
    assert!(level.abs() < 0.25, "{level} dB");
}

#[test]
fn iir_glide_through_one_is_smooth() {
    // rate 0.5 -> 3 over a second on a 2 kHz sine: no step between samples
    // bigger than the steepest the signal gets at the fastest rate
    let (mut cp, mut c) = rig(sine_track(2_000.0, 200_000), 1, "rate=0.5 resample=iir");
    run(&mut cp, &mut c, "v rate=3 over 1s");
    let out = render(&mut c, 1, 47_001);
    let biggest = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    // the source's own largest step per source frame, times the top speed
    let limit = (LEVEL * TAU * 2_000.0 / SR * 3.0) as f32;
    println!("glide: biggest step {biggest}, limit {limit}");
    assert!(biggest <= limit * 1.05, "{biggest} > {limit}");
    // it did go through the range: the pitch rose
    let early = zero_crossings(&out[2_000..8_000]);
    let late = zero_crossings(&out[40_000..46_000]);
    assert!(late > early * 3 / 2, "{early} -> {late}");
}

fn zero_crossings(x: &[f32]) -> usize {
    x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
}

// the worst bin of `x` between `lo` and `hi` that no harmonic of `f0` sits on,
// against the strongest harmonic, in dB
fn floor_db(x: &[f32], f0: f64, lo: f64, hi: f64) -> f64 {
    let n = x.len();
    let bin = |f: f64| {
        let (mut re, mut im, mut sum) = (0.0, 0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (TAU * (i as f64 + 0.5) / n as f64).cos();
            let ph = TAU * f * i as f64 / SR;
            re += w * f64::from(*v) * ph.cos();
            im += w * f64::from(*v) * ph.sin();
            sum += w;
        }
        2.0 * (re * re + im * im).sqrt() / sum
    };
    let strongest = (1..=12).map(|h| bin(h as f64 * f0)).fold(0.0, f64::max);
    let hz_per_bin = SR / n as f64;
    let mut worst = 0.0f64;
    let mut f = lo;
    while f < hi {
        // a harmonic of f0 within a few bins is the signal's own
        let h = (f / f0).round();
        if (f - h * f0).abs() > 8.0 * hz_per_bin {
            worst = worst.max(bin(f));
        }
        f += 4.0 * hz_per_bin;
    }
    db(worst / strongest)
}

// a saw of `period` frames: one period is the whole track
fn saw_track(period: usize) -> AudioFile {
    track_of("t", 1, period, |n, _| {
        LEVEL * (2.0 * n as f64 / period as f64 - 1.0)
    })
}

#[test]
fn iir_loop_and_pingpong_band_limited() {
    // a periodic saw looped at 1.7x: everything but resampling artefacts is
    // a harmonic of the loop's own pitch
    let period = 61;
    for (mode, f0) in [
        ("forward", 1.7 * SR / period as f64),
        ("pingpong", 1.7 * SR / (2.0 * (period - 1) as f64)),
    ] {
        let settings = |m: &str| format!("rate=1.7 loop={mode} bounce=0 resample={m}");
        let iir = floor_db(
            steady(&play(saw_track(period), &settings("iir"), 17_001)),
            f0,
            SR / 4.0,
            19_000.0,
        );
        let cubic = floor_db(
            steady(&play(saw_track(period), &settings("cubic"), 17_001)),
            f0,
            SR / 4.0,
            19_000.0,
        );
        println!("{mode}: floor iir {iir:.1} dB, cubic {cubic:.1} dB");
        assert!(iir < -60.0, "{mode}: iir {iir} dB");
        assert!(cubic > iir + 20.0, "{mode}: cubic {cubic} dB");
    }
}

#[test]
fn iir_loops_are_transparent_through_every_wrap_and_turn() {
    // 10 kHz in a loop that is whole cycles (forward: 240 frames) or that
    // turns on two peaks (ping-pong: 241 frames, so each turn is on a crest),
    // read at 1x, is a clean 10 kHz forever: any mis-folded knot shows in the
    // rest
    let cos = |frames: usize| {
        track_of("t", 1, frames, |n, _| {
            LEVEL * (TAU * 10_000.0 * n as f64 / SR).cos()
        })
    };
    for (mode, frames) in [("forward", 240), ("pingpong", 241)] {
        let out = play(
            cos(frames),
            &format!("rate=1 loop={mode} bounce=0 resample=iir"),
            17_001,
        );
        let x = steady(&out);
        let level = db(level_at(x, 10_000.0));
        let rest = residual_db(x, 10_000.0);
        println!("{mode}: {level:+.3} dB, rest {rest:.1} dB");
        assert!(level.abs() < 0.25, "{mode}: {level} dB");
        assert!(rest < -80.0, "{mode}: rest {rest} dB");
    }
}

#[test]
fn stereo_channels_are_read_alike() {
    // right is minus left
    let t = track_of("t", 2, 20_000, |n, c| {
        let s = LEVEL * (TAU * 7_000.0 * n as f64 / SR).sin();
        if c == 0 { s } else { -s }
    });
    let (_cp, mut c) = rig(t, 2, "rate=1.3 resample=iir pan=0");
    let out = render(&mut c, 2, 6_001);
    let worst = out
        .chunks(2)
        .map(|f| (f[0] + f[1]).abs())
        .fold(0.0, f32::max);
    assert!(worst < 1e-5, "{worst}");
    assert!(out.iter().any(|v| v.abs() > 0.1));
}

#[test]
fn a_retrigger_starts_from_the_last_output() {
    // `start` again mid-note: the read jumps back to the top of the track,
    // and the resampler carries the output across, where Hermite steps
    let step_across = |mode: &str| {
        let (mut cp, mut c) = rig(sine_track(1_000.0, 20_000), 1, &format!("resample={mode}"));
        let mut out = render(&mut c, 1, 1_037);
        run(&mut cp, &mut c, "start v");
        out.extend(render(&mut c, 1, 200));
        let at = 1_037;
        (out[at] - out[at - 1])
            .abs()
            .max((out[at + 1] - out[at]).abs())
    };
    let iir = step_across("iir");
    let cubic = step_across("cubic");
    println!("retrigger: step iir {iir}, cubic {cubic}");
    // a 1 kHz sine's largest natural step is LEVEL * 2 pi / 48
    let natural = (LEVEL * TAU * 1_000.0 / SR) as f32;
    assert!(iir < natural * 1.2, "iir {iir} vs {natural}");
    assert!(
        cubic > natural * 1.5,
        "cubic {cubic} vs {natural}: the retrigger should step"
    );
}

#[test]
fn resample_switch_waits_for_hit() {
    // a note that is sounding keeps its method when `resample` is restated;
    // the next hit takes the new one
    let (mut cp, mut c) = rig(sine_track(3_000.0, 40_000), 1, "rate=1.5 resample=iir");
    let first = render(&mut c, 1, 2_001);
    run(&mut cp, &mut c, "v resample=cubic");
    let second = render(&mut c, 1, 2_001);
    // the same render, with no switch
    let (_cp2, mut c2) = rig(sine_track(3_000.0, 40_000), 1, "rate=1.5 resample=iir");
    let want_first = render(&mut c2, 1, 2_001);
    let want_second = render(&mut c2, 1, 2_001);
    assert_eq!(first, want_first);
    assert_eq!(second, want_second, "the switch must wait for the hit");
    // after a hit, Hermite: the render equals one that was cubic from the start
    run(&mut cp, &mut c, "start v");
    let after = render(&mut c, 1, 1_001);
    let (_cp3, mut c3) = rig(sine_track(3_000.0, 40_000), 1, "rate=1.5 resample=cubic");
    let cubic = render(&mut c3, 1, 1_001);
    assert_eq!(after, cubic);
    assert_ne!(
        after,
        render(
            &mut rig(sine_track(3_000.0, 40_000), 1, "rate=1.5 resample=iir").1,
            1,
            1_001
        )
    );
}

#[test]
fn state_round_trip_and_default() {
    sample_rate::set(48000);
    let (mut cp, mut c) = setup(1, vec![sine_track(1_000.0, 1_000)]);
    // the schema says iir is the default (the unit tests start from cubic,
    // see `ResampleMode::default`)
    let spec = crate::audio_processing::commands::lang::schema::schema(
        crate::audio_processing::commands::lang::ast::Kind::Voice,
    )
    .settings
    .iter()
    .find(|s| s.name == "resample")
    .expect("a resample setting");
    assert_eq!(spec.default, "iir");
    run(
        &mut cp,
        &mut c,
        "voice v from \"t\" on 1000ms resample=cubic",
    );
    let text = state_text(&mut cp);
    let other = match crate::audio_processing::commands::ResampleMode::initial() {
        crate::audio_processing::commands::ResampleMode::Cubic => "iir",
        _ => "cubic",
    };
    // a word other than the default is printed; the default is not
    assert_eq!(text.contains("resample=cubic"), other == "cubic", "{text}");
    run(&mut cp, &mut c, &format!("v resample={other}"));
    let text = state_text(&mut cp);
    assert!(text.contains(&format!("resample={other}")), "{text}");
    // the printed state replays to the same state
    let (mut cp2, mut c2) = setup(1, vec![sine_track(1_000.0, 1_000)]);
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        run(&mut cp2, &mut c2, line);
    }
    assert_eq!(state_text(&mut cp2), text);
    // and restating the default drops it from the state
    let default_word = match other {
        "iir" => "cubic",
        _ => "iir",
    };
    run(&mut cp, &mut c, &format!("v resample={default_word}"));
    assert!(!state_text(&mut cp).contains("resample"));
}

fn state_text(cp: &mut CmdProcessor) -> String {
    match cp.parse("state") {
        Ok(crate::audio_processing::commands::Parsed::Local(t)) => t,
        _ => panic!("state should print"),
    }
}

#[test]
fn a_bad_resample_word_is_e402() {
    sample_rate::set(48000);
    let (mut cp, _c) = setup(1, vec![sine_track(1_000.0, 1_000)]);
    match cp.parse("voice v from \"t\" on 1000ms resample=sinc") {
        Err(CmdErr::Lang(errs)) => assert_eq!(errs[0].code, Code::WrongType),
        other => panic!("expected E402, got {:?}", other.map(|_| ())),
    }
}

// ---- round 1 of the review ----

// a ramp from 0 to LEVEL over the track: slow, so the resampler and Hermite
// agree to a hair wherever nothing happens
fn ramp_track(frames: usize) -> AudioFile {
    track_of("t", 1, frames, |n, _| LEVEL * n as f64 / frames as f64)
}

// the most `a` and `b` differ, outside the `skip` windows (centre, radius),
// and how many frames were compared
fn worst_difference(a: &[f32], b: &[f32], skip: &[(usize, usize)]) -> (f32, usize) {
    let (mut worst, mut n) = (0.0f32, 0);
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if skip.iter().any(|&(c, r)| i + r >= c && i <= c + r) {
            continue;
        }
        worst = worst.max((x - y).abs());
        n += 1;
    }
    (worst, n)
}

fn iir_and_cubic(track: impl Fn() -> AudioFile, settings: &str, n: usize) -> (Vec<f32>, Vec<f32>) {
    (
        play(track(), &format!("{settings} resample=iir"), n),
        play(track(), &format!("{settings} resample=cubic"), n),
    )
}

#[test]
fn iir_reverse_with_a_loop_that_ends_early_plays_the_tail_first() {
    // backwards from the end of the track, with the loop in its first half:
    // the head crosses 10000 frames of tail before it meets the loop, and
    // those frames are the track's own, not the loop's
    for mode in ["forward", "pingpong"] {
        let settings = format!("rate=-1 loop={mode} bounce=0 loopstart=0.1 loopend=0.5");
        let (iir, cubic) = iir_and_cubic(|| ramp_track(20_000), &settings, 600);
        // a ramp read backwards falls from the top: near LEVEL, not half
        assert!(iir[300] > 0.95 * LEVEL as f32, "{mode}: {}", iir[300]);
        let (worst, n) = worst_difference(&iir, &cubic, &[(0, 100)]);
        println!("reverse lead-in {mode}: worst {worst} over {n}");
        assert!(worst < 0.02 * LEVEL as f32, "{mode}: {worst}");
    }
}

#[test]
fn iir_follows_cubic_through_loops_slices_and_one_shots() {
    // (settings, frames played, windows to leave out: wraps and ends)
    type Case = (&'static str, usize, Vec<(usize, usize)>);
    let cases: [Case; 13] = [
        // fractional and backwards speeds, and ping-pong with its fade
        (
            "loop=forward rate=-1.37",
            9_000,
            vec![(4_380, 150), (8_760, 150)],
        ),
        ("rate=-1.37 start=0.2 end=0.7", 2_100, vec![(2_190, 150)]),
        (
            "loop=pingpong rate=1.37",
            9_000,
            vec![(4_379, 250), (8_758, 250)],
        ),
        (
            "loop=pingpong rate=-1.37",
            9_000,
            vec![(4_380, 250), (8_760, 250)],
        ),
        ("loop=forward", 9_000, vec![(6_000, 150)]),
        // (the knots at the loop's start see the loop's wrap in their taps)
        (
            "loop=forward loopstart=0.5",
            9_000,
            // (the third trip wraps on the last frame: the virtual head is
            // there `delay()` frames early, and its pre-ringing is heard)
            vec![(6_000, 150), (3_000, 10), (9_000, 150)],
        ),
        ("loop=forward rate=-1", 9_000, vec![(6_000, 150)]),
        (
            "loop=forward loopend=0.5 rate=-1",
            9_000,
            vec![(3_000, 150), (6_000, 150), (9_000, 150)],
        ),
        ("loop=pingpong bounce=0", 9_000, vec![(6_000, 80)]),
        ("loop=pingpong bounce=0 rate=-1", 9_000, vec![(6_000, 80)]),
        ("slices=4 slice=2", 1_450, vec![]),
        ("rate=-1 start=0.2 end=0.7", 2_900, vec![(3_000, 150)]),
        ("loop=forward rate=2.5", 4_000, vec![(2_400, 150)]),
    ];
    for (settings, n, skip) in cases {
        let mut skip = skip;
        skip.push((0, 120));
        let (iir, cubic) = iir_and_cubic(|| ramp_track(6_000), settings, n);
        let (worst, compared) = worst_difference(&iir, &cubic, &skip);
        println!("{settings}: worst {worst} over {compared}");
        assert!(
            compared > n / 2,
            "{settings}: only {compared} frames compared"
        );
        assert!(worst < 0.02 * LEVEL as f32, "{settings}: {worst}");
    }
}

#[test]
fn a_retrigger_in_a_region_follows_cubic_afterwards() {
    let run_it = |mode: &str| {
        let (mut cp, mut c) = rig(
            ramp_track(6_000),
            1,
            &format!("resample={mode} slices=2 slice=2"),
        );
        let mut out = render(&mut c, 1, 2_000);
        run(&mut cp, &mut c, "start v");
        out.extend(render(&mut c, 1, 1_000));
        out
    };
    let (iir, cubic) = (run_it("iir"), run_it("cubic"));
    let (worst, n) = worst_difference(&iir, &cubic, &[(0, 120), (2_000, 150)]);
    println!("retrigger in a slice: worst {worst} over {n}");
    assert!(worst < 0.02 * LEVEL as f32, "{worst}");
}

// the biggest step between neighbouring frames of `x`
fn biggest(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn an_instant_change_of_speed_or_direction_does_not_jump() {
    // (frequency, the speed it runs at first, the speed it is set to)
    for (f, first, then) in [
        (5_000.0, 1.0, -1.0),
        (5_000.0, 1.0, 2.0),
        (1_000.0, 1.0, 0.25),
        (3_000.0, 3.0, -0.5),
    ] {
        let (mut cp, mut c) = rig(
            sine_track(f, 60_000),
            1,
            &format!("resample=iir rate={first}"),
        );
        let mut out = render(&mut c, 1, 3_000);
        let at = out.len();
        run(&mut cp, &mut c, &format!("v rate={then}"));
        out.extend(render(&mut c, 1, 3_000));
        let before = biggest(&out[at - 900..at - 10]);
        let after = biggest(&out[at + 400..at + 2_000]);
        let across = biggest(&out[at - 6..at + 14]);
        println!("{first} -> {then} at {f} Hz: before {before}, across {across}, after {after}");
        assert!(
            across < 1.3 * before.max(after),
            "{first} -> {then}: {across} vs {before}/{after}"
        );
    }
}

#[test]
fn an_instant_stretch_past_the_corner_loop_hands_over_cleanly() {
    // rate 4, then stretch=16: for a frame the head moves 64 frames per
    // output frame, past what the resampler follows; that frame is read the
    // old way, and the sound must not stick, jump or blow up
    let (mut cp, mut c) = rig(sine_track(3_000.0, 200_000), 1, "rate=4 resample=iir");
    let mut out = render(&mut c, 1, 2_000);
    run(&mut cp, &mut c, "v stretch=16");
    out.extend(render(&mut c, 1, 2_000));
    assert!(out.iter().all(|v| v.is_finite()));
    let peak = out[1_990..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    println!("rate 4 then stretch 16: peak {peak}");
    assert!(peak < 1.5 * LEVEL as f32, "{peak}");
    // and not stuck on one value
    let moving = out[1_990..2_100]
        .windows(2)
        .filter(|w| w[0] != w[1])
        .count();
    assert!(moving > 50, "{moving}");
}

#[test]
fn stop_then_start_begins_from_silence() {
    for mode in ["iir", "cubic"] {
        let (mut cp, mut c) = rig(sine_track(1_000.0, 40_000), 1, &format!("resample={mode}"));
        let _ = render(&mut c, 1, 1_023);
        run(&mut cp, &mut c, "stop v");
        let _ = render(&mut c, 1, 5_000);
        run(&mut cp, &mut c, "start v");
        let out = render(&mut c, 1, 12);
        println!("stop/start {mode}: {:?}", &out[..4]);
        // a sine starts at 0: the first frames are near it, not at the level
        // the note was stopped on
        assert!(
            out[0].abs() < 0.05 && out[1].abs() < 0.12,
            "{mode}: {:?}",
            &out[..3]
        );
    }
}

#[test]
fn a_region_end_rings_out_instead_of_cutting() {
    // a 1 kHz cosine that ends mid-cycle (20000 frames is 416.67 cycles, so
    // the last frame is at -0.5): the step to silence goes through the filter
    let track = || {
        track_of("t", 1, 20_000, |n, _| {
            LEVEL * (TAU * 1_000.0 * n as f64 / SR).cos()
        })
    };
    let out = play(track(), "resample=iir", 22_000);
    let level = out[19_990].abs().max(0.25 * LEVEL as f32);
    let across = biggest(&out[19_980..20_300]);
    println!("region end: level {level}, biggest step across {across}");
    // a hard cut would step by the whole level at once
    assert!(across < 0.75 * level, "{across} vs {level}");
    // it rings out and stops: exact silence well after
    assert!(out[21_500..].iter().all(|v| *v == 0.0));
    // and the track is at its level before the end
    let peak = out[10_000..19_900]
        .iter()
        .fold(0.0f32, |m, v| m.max(v.abs()));
    assert!((peak / LEVEL as f32 - 1.0).abs() < 0.02, "{peak}");
}

#[test]
fn pause_is_not_stop_for_the_method() {
    // a paused note keeps its method through a restated `resample`
    let play_to = |switch: bool| {
        let (mut cp, mut c) = rig(sine_track(3_000.0, 40_000), 1, "rate=1.5 resample=iir");
        let mut out = render(&mut c, 1, 1_001);
        run(&mut cp, &mut c, "pause v");
        if switch {
            run(&mut cp, &mut c, "v resample=cubic");
        }
        run(&mut cp, &mut c, "resume v");
        out.extend(render(&mut c, 1, 1_001));
        out
    };
    assert_eq!(play_to(true), play_to(false));
}

#[test]
fn a_silent_source_lets_the_filter_die_to_exact_zero() {
    // 2000 frames of a sine, then silence: the filter's tail is cut off
    // once nothing has changed for a while (it would run down to denormals)
    let track = track_of("t", 1, 30_000, |n, _| {
        if n < 2_000 {
            LEVEL * (TAU * 7_000.0 * n as f64 / SR).sin()
        } else {
            0.0
        }
    });
    let (_cp, mut c) = rig(track, 1, "resample=iir");
    let out = render(&mut c, 1, 2_000);
    assert!(out.iter().any(|v| v.abs() > 0.3));
    assert!(!c.voices.get(0).unwrap().iir.borrow()[0].state[0].is_silent());
    let out = render(&mut c, 1, 4_000);
    assert!(out[2_300..].iter().all(|v| *v == 0.0));
    // the state itself is zero, not merely too small for an f32
    assert!(c.voices.get(0).unwrap().iir.borrow()[0].state[0].is_silent());
}

#[test]
fn a_growing_buffer_is_read_the_old_way_near_its_edge() {
    // `rec` is recorded live from the track and `re` plays it at the same
    // moment: its head sits on the newest frame all along, where the source
    // isn't settled, so it reads like cubic; started later, it is far from
    // the edge and uses the resampler
    let run_script = |mode: &str, late: bool| {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![sine_track(2_000.0, 30_000)]);
        run(&mut cp, &mut c, "voice src from \"t\" on 1000ms");
        run(
            &mut cp,
            &mut c,
            "buffer rec from=src length=20000f live=yes",
        );
        run(
            &mut cp,
            &mut c,
            &format!("voice re from rec on 1000ms resample={mode}"),
        );
        run(&mut cp, &mut c, "start src");
        run(&mut cp, &mut c, "start rec");
        if late {
            let _ = render(&mut c, 1, 6_000);
        }
        run(&mut cp, &mut c, "start re");
        render(&mut c, 1, 1_500)
    };
    let (edge_iir, edge_cubic) = (run_script("iir", false), run_script("cubic", false));
    assert_eq!(edge_iir, edge_cubic, "at the edge it must read like cubic");
    let (late_iir, late_cubic) = (run_script("iir", true), run_script("cubic", true));
    assert_ne!(
        late_iir, late_cubic,
        "far from the edge it must use the resampler"
    );
}

// ---- round 2 of the review ----

#[test]
fn a_region_that_moves_off_the_head_never_spikes() {
    // `end` dropping below the head, `start` rising above it, a different
    // slice: the instance has no region to play in any more, and what it
    // leaves is a step to silence through the filter, of the size of the note
    // (not of a line read thousands of frames outside the region)
    for (settings, change, played) in [
        ("", "v end=0.3", 24_000),
        (" slices=4 slice=4", "v slice=1", 4_000),
        ("", "v start=0.9", 24_000),
        (" rate=3", "v end=0.2", 6_000),
    ] {
        let (mut cp, mut c) = rig(
            sine_track(1_000.0, 40_000),
            1,
            &format!("resample=iir{settings}"),
        );
        let _ = render(&mut c, 1, played);
        run(&mut cp, &mut c, change);
        let out = render(&mut c, 1, 3_000);
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        println!(
            "{settings} {change}: peak {peak} (level {LEVEL}), biggest step {}",
            biggest(&out)
        );
        assert!(
            peak < 1.4 * LEVEL as f32,
            "{settings} {change}: peak {peak}"
        );
        assert!(biggest(&out) < 1.4 * LEVEL as f32, "{settings} {change}");
        // and it ends: the tail rings out to silence
        assert!(
            out[2_500..].iter().all(|v| *v == 0.0),
            "{settings} {change}"
        );
    }
}

#[test]
fn a_loop_seam_is_seamless_at_fractional_speeds_either_way() {
    // 15 whole periods of 120 Hz, then DC: a loop on the first half is
    // seamless, and what lies past its end is different from what lies in it
    let track = || {
        track_of("t", 1, 12_000, |n, _| match n < 6_000 {
            true => LEVEL * (TAU * 120.0 * n as f64 / SR).sin(),
            false => 0.9 * LEVEL,
        })
    };
    // (settings, where the head leaves the tail and meets the loop: that
    // step is the source's own, so only it is left out)
    // (the intonation case seeds the choir, so both renders draw the same
    // detunes and delays; each instance meets the loop up to 0.020 * sr *
    // spread = 576 frames late and its rate_mul moves the entry by ~110 more,
    // so the window around the entry is wider than that)
    for (settings, entry, radius, speed) in [
        ("loop=forward loopend=0.5 rate=-1.37", 4_380, 150, 1.4),
        ("loop=forward loopend=0.5 rate=1.37", 0, 0, 1.4),
        ("loop=forward loopend=0.5 rate=-1", 6_000, 150, 1.0),
        (
            "loop=forward loopend=0.5 rate=-1 intonation=0.4 choir=2",
            6_000,
            800,
            1.0,
        ),
        ("loop=forward loopend=0.5 rate=-2.6", 2_308, 150, 2.6),
    ] {
        let seeded = |mode: &str| {
            sample_rate::set(48000);
            let (mut cp, mut c) = setup(1, vec![track()]);
            let line = format!("voice v from \"t\" on 1000ms {settings} resample={mode}");
            run(&mut cp, &mut c, &line);
            c.voices.get_mut(0).unwrap().state.seed(11);
            run(&mut cp, &mut c, "start v");
            render(&mut c, 1, 30_000)
        };
        let (iir, cubic) = (seeded("iir"), seeded("cubic"));
        let skip = [(0, 150), (entry, radius)];
        let (worst, n) = worst_difference(&iir, &cubic, &skip);
        println!("{settings}: worst {worst} over {n}");
        let (si, sc) = (
            biggest(&iir[entry + radius + 1..]),
            biggest(&cubic[entry + radius + 1..]),
        );
        println!("   biggest step after the entry: iir {si} cubic {sc}");
        assert!(si < 1.3 * sc, "{settings}: step {si} vs cubic {sc}");
        if !settings.contains("intonation") {
            assert!(worst < 0.07 * speed * LEVEL as f32, "{settings}: {worst}");
        }
    }
    // a ping-pong loop that turns on crests (with the default fade)
    let cos = || {
        track_of("t", 1, 6_001, |n, _| {
            LEVEL * (TAU * 120.0 * n as f64 / SR).cos()
        })
    };
    for settings in [
        "loop=pingpong rate=1.37",
        "loop=pingpong rate=-1.37",
        "loop=pingpong bounce=0 rate=-1.37",
        "loop=pingpong bounce=0 rate=1.37",
    ] {
        let (iir, cubic) = iir_and_cubic(cos, settings, 30_000);
        let (worst, _) = worst_difference(&iir, &cubic, &[(0, 150)]);
        println!("{settings}: worst {worst}");
        assert!(worst < 0.07 * LEVEL as f32, "{settings}: {worst}");
    }
}

#[test]
fn park_forgets_the_line() {
    // a voice that is parked (a seq's voice waiting for its step, after a
    // restart) and hit before it renders again must not carry the old note
    use crate::audio_processing::commands::TempoId;
    use crate::audio_processing::engine::track::Track;
    let samples: Arc<[f32]> = (0..4_000)
        .map(|n| (LEVEL * (TAU * 1_000.0 * n as f64 / SR).sin()) as f32)
        .collect::<Vec<f32>>()
        .into();
    let track = Track {
        samples,
        channels: 1,
        frames: 4_000,
    };
    sample_rate::set(48000);
    let mut v = Voice::new(&track, TempoId(0), false);
    v.state.set_resample(ResampleMode::Iir);
    v.state.active = true;
    v.hit();
    let table = crate::audio_processing::engine::wave::test_table();
    let mut frame = [0.0f32; 1];
    for now in 0..1_023 {
        frame[0] = 0.0;
        v.render_frame(&mut frame, table, now);
        v.advance();
    }
    assert!(v.iir.borrow()[0].on && v.iir.borrow()[0].last[0].abs() > 0.3);
    v.park();
    {
        let bank = v.iir.borrow();
        let q = &bank[0];
        assert!(!q.on && q.last[0] == 0.0 && q.slope[0] == 0.0);
    }
    // and the first frame of the next note starts from silence
    v.hit();
    frame[0] = 0.0;
    v.render_frame(&mut frame, table, 2_000);
    assert!(frame[0].abs() < 0.05, "{}", frame[0]);
}

// cost of the reader: `cargo test --release bench_resample -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_resample_cost() {
    let t = || {
        track_of("t", 2, 400_000, |n, c| {
            LEVEL * (TAU * (3_000.0 + c as f64) * n as f64 / SR).sin()
        })
    };
    for settings in [
        "rate=1 resample=cubic",
        "rate=1 resample=iir",
        "rate=4 resample=cubic",
        "rate=4 resample=iir",
        "rate=4 resample=iir loop=forward loopstart=0.1 loopend=0.9",
        "rate=1 resample=iir loop=pingpong loopstart=0.1 loopend=0.9",
    ] {
        let (_cp, mut c) = rig(t(), 2, settings);
        let _ = render(&mut c, 2, 4_800);
        let t0 = std::time::Instant::now();
        let out = render(&mut c, 2, 48_000);
        println!(
            "BENCH {settings}: {:?} per second of audio ({})",
            t0.elapsed(),
            out.len()
        );
    }
}

// ---- HPRL: the resampler without the filter's delay ----

// the complex amplitude of `f` Hz in `x[from..to]` (Hann window), the phase
// in radians (x = A sin(w (n - d)) reads -w d), and the amplitude
fn projection(x: &[f32], f: f64, from: usize, to: usize) -> (f64, f64) {
    let n = to - from;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x[from..to].iter().enumerate() {
        let w = 0.5 - 0.5 * (TAU * (i as f64 + 0.5) / n as f64).cos();
        let ph = TAU * f * (from + i) as f64 / SR;
        re += w * f64::from(*v) * ph.cos();
        im += w * f64::from(*v) * ph.sin();
    }
    // x = A sin(w n + phi) projects onto (cos, sin) as (A sin phi, A cos phi)/2
    (im.atan2(re), (re * re + im * im).sqrt())
}

// how many output frames `iir` plays behind `cubic` (positive: later) and by
// how many dB it is louder, for a sine of `f` Hz read with `settings`
fn lag_and_level(f: f64, settings: &str) -> (f64, f64) {
    let n = 30_000;
    let (iir, cubic) = iir_and_cubic(|| sine_track(f, 120_000), settings, n);
    let out_f = f * rate_of(settings).abs();
    let (pi, ai) = projection(&iir, out_f, 4_800, 28_800);
    let (pc, ac) = projection(&cubic, out_f, 4_800, 28_800);
    let dphi = (pi - pc + 3.0 * std::f64::consts::PI).rem_euclid(TAU) - std::f64::consts::PI;
    (dphi / (TAU * out_f / SR), db(ai / ac))
}

fn rate_of(settings: &str) -> f64 {
    settings
        .split_whitespace()
        .find_map(|w| w.strip_prefix("rate="))
        .and_then(|r| r.parse().ok())
        .unwrap_or(1.0)
}

#[test]
fn iir_lines_up_with_cubic() {
    let mut worst = (0.0f64, 0.0f64);
    for f in [50.0, 120.0, 1_000.0, 5_000.0] {
        for settings in [
            "rate=1",
            "rate=0.5",
            "rate=2",
            "rate=-1",
            "rate=-1.37",
            "rate=1 loop=forward loopstart=0 loopend=0.2",
        ] {
            // (the filter's own phase takes over from about 10 kHz: 5 kHz
            // at twice the speed is out of what the lead cancels)
            if f * rate_of(settings).abs() > 9_000.0 {
                continue;
            }
            let (lag, level) = lag_and_level(f, settings);
            println!("LAG {f} Hz {settings}: {lag:+.3} frames, {level:+.3} dB");
            worst = (worst.0.max(lag.abs()), worst.1.max(level.abs()));
        }
    }
    assert!(worst.0 <= 0.05, "lag {} frames", worst.0);
    assert!(worst.1 <= 0.15, "level {} dB", worst.1);
}

// the time at which `x` first crosses half of its settled value (the mean of
// the last 100 frames), by a line between the frames
fn half_crossing(x: &[f32]) -> f64 {
    let settled = x[x.len() - 100..]
        .iter()
        .map(|v| f64::from(*v))
        .sum::<f64>()
        / 100.0;
    let n = x
        .iter()
        .position(|v| f64::from(*v) >= settled / 2.0)
        .unwrap();
    let (a, b) = (f64::from(x[n - 1]), f64::from(x[n]));
    (n - 1) as f64 + (settled / 2.0 - a) / (b - a)
}

#[test]
fn iir_onset_lines_up() {
    // a note that starts on a level, 100 frames in: the output steps from 0
    // to 0.5 at the hit. cubic steps on the frame; the resampler's step is
    // centred on it
    let level = |_n: usize, _c: usize| 0.5;
    for rate in ["1", "0.5", "2", "-1", "1.37"] {
        let play_late = |mode: &str| {
            sample_rate::set(48000);
            let (mut cp, mut c) = setup(1, vec![track_of("t", 1, 20_000, level)]);
            run(
                &mut cp,
                &mut c,
                &format!("voice v from \"t\" on 1000ms rate={rate} resample={mode}"),
            );
            let mut out = render(&mut c, 1, 100);
            run(&mut cp, &mut c, "start v");
            out.extend(render(&mut c, 1, 400));
            out
        };
        let (iir, cubic) = (play_late("iir"), play_late("cubic"));
        let hit = cubic.iter().position(|v| *v != 0.0).unwrap();
        let crossing = half_crossing(&iir);
        println!("ONSET rate {rate}: hit frame {hit}, iir 50 % at {crossing:.3}");
        assert_eq!(hit, 100);
        // (the first frame already carries the onset's first half: the
        // pre-roll ran before it)
        assert!(
            iir[hit] > 0.2,
            "rate {rate}: the first frame is {}",
            iir[hit]
        );
        assert!(
            (crossing - hit as f64).abs() < 0.1,
            "rate {rate}: the onset is at {crossing}, the hit at {hit}"
        );
    }
}

#[test]
fn instant_speed_change_with_lead_has_no_step() {
    // the virtual head leads by `delay() * speed`: a change of speed that is
    // instant must slew the lead, not move the head. The largest step across
    // the changes stays within the steady ones (HPR round 1's rule)
    for (f, speeds) in [
        (1_000.0, vec![1.0, 3.0, 0.5, -1.0, 2.0, -0.25, 1.0]),
        (5_000.0, vec![1.0, 3.0, 0.5, -1.0, 2.0]),
        (300.0, vec![0.5, 4.0, 1.0, -3.0, 8.0, 0.1, 1.0]),
    ] {
        let (mut cp, mut c) = rig(
            sine_track(f, 400_000),
            1,
            &format!("resample=iir rate={}", speeds[0]),
        );
        let mut out = render(&mut c, 1, 2_000);
        // (where each stretch begins and ends)
        let mut stretches = vec![(500, out.len())];
        for s in &speeds[1..] {
            run(&mut cp, &mut c, &format!("v rate={s}"));
            let from = out.len();
            out.extend(render(&mut c, 1, 2_000));
            stretches.push((from + 500, out.len()));
        }
        let steady = stretches
            .iter()
            .map(|&(a, b)| biggest(&out[a..b]))
            .fold(0.0, f32::max);
        let across = (1..speeds.len())
            .map(|k| {
                let at = stretches[k].0 - 500;
                let local = biggest(&out[stretches[k - 1].0..at - 10])
                    .max(biggest(&out[at + 500..stretches[k].1]));
                println!(
                    "STEPK {f} {k}: {} -> {} across {} local {local}",
                    speeds[k - 1],
                    speeds[k],
                    biggest(&out[at - 4..at + 60])
                );
                biggest(&out[at - 4..at + 60])
            })
            .fold(0.0, f32::max);
        println!("STEP {f} Hz {speeds:?}: across {across}, steady {steady}");
        // (an instant change takes the filter its delay to show, so a step
        // a little over the steady one is told; HPR round 1 allowed 1.3)
        assert!(across <= steady * 1.15, "{f}: {across} vs {steady}");
    }
}

// the lag and level of `iir` against `cubic` on a track, for a tone of `f` Hz
// in the output
fn lag_on(track: impl Fn() -> AudioFile, f: f64, settings: &str) -> (f64, f64) {
    let (iir, cubic) = iir_and_cubic(track, settings, 30_000);
    let (pi, ai) = projection(&iir, f, 4_800, 28_800);
    let (pc, ac) = projection(&cubic, f, 4_800, 28_800);
    let dphi = (pi - pc + 3.0 * std::f64::consts::PI).rem_euclid(TAU) - std::f64::consts::PI;
    (dphi / (TAU * f / SR), db(ai / ac))
}

#[test]
fn lead_folds_through_loops() {
    // 1 kHz cosines whose loops are whole cycles (forward: 4800 frames) or
    // that turn on crests (ping-pong: 0..=4800), so the tone is clean through
    // every wrap and turn, and the lead has to fold through them: the lag
    // stays nil at fractional and negative speeds
    let cos_track = |frames: usize| {
        move || {
            track_of("t", 1, frames, |n, _| {
                LEVEL * (TAU * 1_000.0 * n as f64 / SR).cos()
            })
        }
    };
    for rate in [1.0, 1.37, -1.37, 0.8, -0.8, 2.5, -1.0] {
        for (mode, frames) in [("forward", 9_600), ("pingpong", 9_602)] {
            let settings = format!("rate={rate} loop={mode} loopstart=0 loopend=0.5 bounce=0");
            let (lag, level) = lag_on(cos_track(frames), 1_000.0 * f64::abs(rate), &settings);
            println!("FOLD {mode} rate {rate}: {lag:+.3} frames, {level:+.3} dB");
            assert!(lag.abs() <= 0.05, "{mode} {rate}: lag {lag}");
            assert!(level.abs() <= 0.15, "{mode} {rate}: level {level} dB");
        }
    }
}

#[test]
fn growing_buffer_keeps_its_margin() {
    // `re` reads a buffer that `rec` is still recording, started so late and
    // faster than the write head that its head reaches the newest frame 300
    // frames in. The lead reads `delay() * speed` frames
    // further ahead than the head, where the source isn't there yet: a
    // reader that looked at it would see the unwritten silence, which on a
    // level is a dip against cubic (the 7-tap pre-emphasis weighs the
    // neighbours). It never does
    for rate in [1.5, 3.0, 8.0, 12.0] {
        let run_script = |mode: &str| {
            sample_rate::set(48000);
            let (mut cp, mut c) = setup(1, vec![track_of("t", 1, 30_000, |_, _| 0.5)]);
            run(&mut cp, &mut c, "voice src from \"t\" on 1000ms");
            run(
                &mut cp,
                &mut c,
                "buffer rec from=src length=20000f live=yes",
            );
            run(
                &mut cp,
                &mut c,
                &format!("voice re from rec on 1000ms rate={rate} resample={mode}"),
            );
            run(&mut cp, &mut c, "start src");
            run(&mut cp, &mut c, "start rec");
            let late = (300.0 * (rate - 1.0)) as usize;
            let mut out = render(&mut c, 1, late);
            run(&mut cp, &mut c, "start re");
            out.extend(render(&mut c, 1, 700));
            out.split_off(late + 64)
        };
        let (iir, cubic) = (run_script("iir"), run_script("cubic"));
        let (worst, _) = worst_difference(&iir, &cubic, &[]);
        println!("MARGIN rate {rate}: worst {worst}");
        // (the level's own 0.55 % of pre-emphasis gain is 0.003)
        assert!(worst < 0.006, "rate {rate}: {worst} against cubic");
    }
}

// cost of a hit on an iir voice: `cargo test --release bench_hit -- --ignored --nocapture`.
// A note that is hit again after every frame against a steady note, in
// ns per frame; the difference is what a hit costs (its pre-roll included)
#[test]
#[ignore]
fn bench_hit_cost() {
    use crate::audio_processing::commands::TempoId;
    use crate::audio_processing::engine::track::Track;
    let samples: Arc<[f32]> = (0..400_000)
        .map(|n| (LEVEL * (TAU * 3_000.0 * n as f64 / SR).sin()) as f32)
        .collect::<Vec<f32>>()
        .into();
    let track = Track {
        samples,
        channels: 1,
        frames: 400_000,
    };
    sample_rate::set(48000);
    let table = crate::audio_processing::engine::wave::test_table();
    for rate in [1.0f32, 3.0] {
        let mut per = [0.0f64; 2];
        for (k, hits) in [false, true].into_iter().enumerate() {
            let mut v = Voice::new(&track, TempoId(0), false);
            v.state.set_resample(ResampleMode::Iir);
            v.state.active = true;
            v.hit();
            let mut best = f64::MAX;
            for _round in 0..5 {
                let n = 100_000;
                let t0 = std::time::Instant::now();
                let mut frame = [0.0f32; 1];
                for now in 0..n {
                    if hits {
                        v.park();
                        v.hit();
                    }
                    frame[0] = 0.0;
                    v.render_frame(&mut frame, table, now);
                    v.advance();
                }
                best = best.min(t0.elapsed().as_nanos() as f64 / n as f64);
            }
            per[k] = best;
        }
        println!(
            "BENCHHIT rate {rate}: steady {:.1} ns/frame, hit every frame {:.1} ns/frame, a hit costs {:.1} ns",
            per[0],
            per[1],
            per[1] - per[0]
        );
    }
}

#[test]
fn a_big_instant_drop_of_speed_never_runs_the_head_backwards() {
    // 8 -> 0.25 and the like: the lead (`delay() * speed`) has to shrink, and
    // if it did so faster than the head moves, the virtual head would run
    // backwards for tens of frames. It shrinks by at most `LEAD_SHRINK` of the
    // head's step a frame, and once it is eaten up, iir and cubic agree again.
    // 1 kHz cosines; the ping-pong turns on crests
    let cos = |frames: usize| {
        track_of("t", 1, frames, |n, _| {
            LEVEL * (TAU * 1_000.0 * n as f64 / SR).cos()
        })
    };
    for (first, then) in [(8.0, 0.25), (12.0, 0.5), (8.0, 0.1), (4.0, 0.05)] {
        for (mode, frames) in [
            ("", 400_000),
            (" loop=pingpong loopstart=0 loopend=0.5", 9_602),
        ] {
            let play_drop = |m: &str| {
                let (mut cp, mut c) =
                    rig(cos(frames), 1, &format!("rate={first} resample={m}{mode}"));
                let mut out = render(&mut c, 1, 1_000);
                run(&mut cp, &mut c, &format!("v rate={then}"));
                let mut worst_shrink = 0.0f64;
                let mut lead = f64::NAN;
                for _ in 0..3_000 {
                    out.extend(render(&mut c, 1, 1));
                    let now = c.voices.get(0).unwrap().iir.borrow()[0].lead;
                    if lead.is_finite() {
                        worst_shrink = worst_shrink.max(lead - now);
                    }
                    lead = now;
                }
                (out.split_off(1_000), worst_shrink)
            };
            let ((iir, shrink), (cubic, _)) = (play_drop("iir"), play_drop("cubic"));
            // the settled lead is `delay() * then`: well before 1_500 frames
            let (worst, _) = worst_difference(&iir[1_500..], &cubic[1_500..], &[]);
            println!(
                "DROP {first} -> {then}{mode}: lead shrank {shrink:.3} a frame at most, worst {worst} after +1500"
            );
            assert!(
                shrink <= 0.75 * then + 1e-6,
                "{first} -> {then}{mode}: the lead shrank {shrink} in a frame"
            );
            assert!(
                worst < 0.02,
                "{first} -> {then}{mode}: {worst} against cubic"
            );
        }
    }
}
