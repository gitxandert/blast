use super::kinds::biquad::{Biquad, Shape};
use super::*;
use crate::audio_processing::blast_rand::X128P;
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::engine::Conductor;
use crate::audio_processing::test_util::{pcm, render, run, setup};
use crate::file_parsing::decode_helpers::AudioFile;

const SR: f32 = 48_000.0;

const LOWPASS: FxKind = FxKind::new(FxType::Filter);
const HIGHPASS: FxKind = FxKind {
    ty: FxType::Filter,
    variant: 1,
};
const BANDPASS: FxKind = FxKind {
    ty: FxType::Filter,
    variant: 2,
};
const NOTCH: FxKind = FxKind {
    ty: FxType::Filter,
    variant: 3,
};
const DRIVE_FX: FxKind = FxKind::new(FxType::Drive);
const CRUSH_FX: FxKind = FxKind::new(FxType::Crush);

// parameter indices of the filter, drive and crush
const CUTOFF: usize = 0;
const MIX: usize = 3;
const GAIN: usize = 4;
const DRIVE: usize = 0;
const BITS: usize = 0;

// the defaults of `kind` with some settings, by name, set; a name the kind
// doesn't have is skipped
fn params(kind: FxKind, over: &[(&str, f32)]) -> [f32; FX_PARAMS] {
    let mut p = defaults(kind.ty);
    for &(name, v) in over {
        if let Some(i) = param_of(kind.ty, name) {
            p[i] = v;
        }
    }
    p
}

// a chain of one effect, inserted the way the mirror does: with room
fn chain_of(kind: FxKind, over: &[(&str, f32)]) -> FxChain {
    sample_rate::set(48_000);
    let mut chain = FxChain::new();
    chain.insert(0, kind, params(kind, over), Some(Vec::with_capacity(1)));
    chain
}

fn sine(freq: f32, i: usize) -> f32 {
    (std::f32::consts::TAU * freq * i as f32 / SR).sin()
}

// output RMS over input RMS for a mono sine, after the filter has settled
fn gain_at(chain: &mut FxChain, freq: f32) -> f32 {
    let (mut sum_in, mut sum_out) = (0.0f64, 0.0f64);
    for i in 0..9600 {
        let x = sine(freq, i);
        let mut frame = [x];
        chain.process(&mut frame);
        if i >= 4800 {
            sum_in += f64::from(x * x);
            sum_out += f64::from(frame[0] * frame[0]);
        }
    }
    (sum_out / sum_in).sqrt() as f32
}

fn assert_near(got: f32, want: f32, tol: f32) {
    assert!(
        (got - want).abs() <= tol,
        "got {got}, wanted {want} ± {tol}"
    );
}

fn ids(chain: &FxChain) -> Vec<u16> {
    chain.fx.iter().map(|s| s.id).collect()
}

#[test]
fn lowpass_passes_low_cuts_high() {
    let over = [("cutoff", 1000.0)];
    assert!(gain_at(&mut chain_of(LOWPASS, &over), 100.0) > 0.95);
    assert!(gain_at(&mut chain_of(LOWPASS, &over), 10_000.0) < 0.05);
}

#[test]
fn highpass_and_notch_and_bandpass_shapes() {
    let over = [("cutoff", 1000.0)];
    let hp = |f| gain_at(&mut chain_of(HIGHPASS, &over), f);
    assert!(hp(10_000.0) > 0.95);
    assert!(hp(100.0) < 0.05);
    let notch = |f| gain_at(&mut chain_of(NOTCH, &over), f);
    assert!(notch(1000.0) < 0.05);
    assert!(notch(100.0) > 0.95);
    let bp = |f| gain_at(&mut chain_of(BANDPASS, &over), f);
    // constant 0 dB peak at the cutoff
    assert_near(bp(1000.0), 1.0, 0.02);
    assert!(bp(100.0) < 0.2);
    assert!(bp(10_000.0) < 0.2);
}

#[test]
fn drive_bounded_and_full_scale_kept() {
    let mut chain = chain_of(DRIVE_FX, &[("amount", 12.0)]);
    for i in 0..=200 {
        let x = -1.0 + i as f32 / 100.0;
        let mut frame = [x];
        chain.process(&mut frame);
        assert!(frame[0].abs() <= 1.0 + 1e-6, "{x} -> {}", frame[0]);
    }
    let mut frame = [1.0];
    chain.process(&mut frame);
    assert_near(frame[0], 1.0, 1e-6);
    // the slope near 0 is g / tanh(g)
    let g = 10f32.powf(12.0 / 20.0);
    let x = 1e-4;
    let mut frame = [x];
    chain.process(&mut frame);
    assert_near(frame[0] / x, g / g.tanh(), 1e-3);
}

#[test]
fn crush_levels_and_hold() {
    let mut chain = chain_of(CRUSH_FX, &[("bits", 2.0)]);
    let mut seen: Vec<i32> = Vec::new();
    for i in 0..4800 {
        let mut frame = [0.9 * sine(137.0, i)];
        chain.process(&mut frame);
        let level = (frame[0] * 1000.0).round() as i32;
        if !seen.contains(&level) {
            seen.push(level);
        }
    }
    assert!(seen.len() <= 4, "{seen:?}");
    assert!(seen.len() >= 3, "{seen:?}");

    let mut chain = chain_of(CRUSH_FX, &[("bits", 24.0), ("down", 4.0)]);
    let out: Vec<f32> = (0..16)
        .map(|i| {
            let mut frame = [i as f32 / 32.0];
            chain.process(&mut frame);
            frame[0]
        })
        .collect();
    for group in out.chunks(4) {
        assert!(group.iter().all(|&v| v == group[0]), "{out:?}");
    }
    assert_ne!(out[0], out[4]);
    // the held value is the group's first input
    assert_near(out[4], 4.0 / 32.0, 1e-6);
}

#[test]
fn mix_zero_is_dry() {
    for kind in [LOWPASS, DRIVE_FX, CRUSH_FX] {
        let mut chain = chain_of(kind, &[("mix", 0.0), ("bits", 2.0)]);
        for i in 0..500 {
            let x = 0.7 * sine(440.0, i);
            let mut frame = [x];
            chain.process(&mut frame);
            assert_eq!(frame[0], x, "{kind:?}");
        }
    }
}

#[test]
fn base_makes_shelves() {
    let lp = |base, f| {
        gain_at(
            &mut chain_of(LOWPASS, &[("cutoff", 400.0), ("base", base)]),
            f,
        )
    };
    assert!(lp(0.25, 100.0) > 0.9);
    assert_near(lp(0.25, 8000.0), 0.25, 0.03);
    let hp = |base, f| {
        gain_at(
            &mut chain_of(HIGHPASS, &[("cutoff", 400.0), ("base", base)]),
            f,
        )
    };
    assert!(hp(0.25, 8000.0) > 0.9);
    assert_near(hp(0.25, 50.0), 0.25, 0.03);

    // base 1 is the input; base 0 is the plain filter
    let mut one = chain_of(LOWPASS, &[("cutoff", 400.0), ("base", 1.0)]);
    let mut zero = chain_of(LOWPASS, &[("cutoff", 400.0), ("base", 0.0)]);
    let mut plain = Biquad::new();
    plain.set(Shape::Lowpass, 400.0, 0.707, SR);
    for i in 0..2000 {
        let x = sine(300.0, i) + 0.5 * sine(5000.0, i);
        let (mut a, mut b) = ([x], [x]);
        one.process(&mut a);
        zero.process(&mut b);
        assert_near(a[0], x, 1e-6);
        assert_near(b[0], plain.run(0, x), 1e-6);
    }
}

#[test]
fn notch_base_limits_depth() {
    let mut chain = chain_of(NOTCH, &[("cutoff", 1000.0), ("base", 0.5)]);
    assert_near(gain_at(&mut chain, 1000.0), 0.5, 0.05);
}

#[test]
fn gain_scales_output() {
    for kind in [LOWPASS, DRIVE_FX, CRUSH_FX] {
        let base = [("mix", 0.5), ("bits", 3.0), ("cutoff", 800.0)];
        let mut one = chain_of(kind, &base);
        let mut two = chain_of(kind, &[base[0], base[1], base[2], ("gain", 2.0)]);
        for i in 0..1000 {
            let x = 0.6 * sine(220.0, i);
            let (mut a, mut b) = ([x], [x]);
            one.process(&mut a);
            two.process(&mut b);
            assert_near(b[0], 2.0 * a[0], 1e-5);
        }
    }
}

#[test]
fn chain_runs_in_written_order() {
    let drive = params(DRIVE_FX, &[("amount", 30.0)]);
    let lp = params(LOWPASS, &[("cutoff", 400.0)]);
    let build = |first: (FxKind, _), second: (FxKind, _)| {
        sample_rate::set(48_000);
        let mut chain = FxChain::new();
        chain.insert(0, first.0, first.1, Some(Vec::with_capacity(1)));
        chain.insert(1, second.0, second.1, Some(Vec::with_capacity(2)));
        chain
    };
    let mut a = build((DRIVE_FX, drive), (LOWPASS, lp));
    let mut b = build((LOWPASS, lp), (DRIVE_FX, drive));
    let mut diff = 0.0f32;
    for i in 0..4000 {
        let x = 0.5 * sine(150.0, i) + 0.3 * sine(3000.0, i);
        let (mut fa, mut fb) = ([x], [x]);
        a.process(&mut fa);
        b.process(&mut fb);
        diff = diff.max((fa[0] - fb[0]).abs());
    }
    assert!(diff > 0.05, "{diff}");
}

#[test]
fn remove_closes_order_and_slots_are_stable() {
    let mut chain = FxChain::new();
    for (id, room) in [(0, 1), (1, 2), (2, 4)] {
        chain.insert(
            id,
            LOWPASS,
            params(LOWPASS, &[("cutoff", 100.0 * (id + 1) as f32)]),
            Some(Vec::with_capacity(room)),
        );
    }
    assert_eq!(ids(&chain), [0, 1, 2]);
    chain.remove(1);
    assert_eq!(ids(&chain), [0, 2]);
    // the others keep their ids and settings
    assert_eq!(chain.fx[1].params[CUTOFF].base, 300.0);
    // a new effect goes last and takes the freed id
    chain.insert(1, DRIVE_FX, params(DRIVE_FX, &[]), None);
    assert_eq!(ids(&chain), [0, 2, 1]);
    // editing by id finds the right one after the shuffle
    chain.set(2, CUTOFF as u8, 5000.0, 0);
    assert_eq!(chain.fx[1].params[CUTOFF].base, 5000.0);
    assert_eq!(chain.fx[0].params[CUTOFF].base, 100.0);
    // a missing id is ignored
    chain.set(9, CUTOFF as u8, 1.0, 0);
    chain.remove(9);
    assert_eq!(chain.len(), 3);
}

#[test]
fn cutoff_mod_is_octaves() {
    let mut chain = chain_of(LOWPASS, &[("cutoff", 500.0)]);
    assert_near(chain.fx[0].effective(CUTOFF), 500.0, 1e-3);
    chain.modulate(0, CUTOFF as u8, ModOp::Add(1.0));
    assert_near(chain.fx[0].effective(CUTOFF), 1000.0, 1e-3);
    chain.modulate(0, CUTOFF as u8, ModOp::Mul(2.0));
    assert_near(chain.fx[0].effective(CUTOFF), 2000.0, 1e-2);
    // other settings add in their own units; the result is clamped
    chain.modulate(0, MIX as u8, ModOp::Add(-0.25));
    assert_near(chain.fx[0].effective(MIX), 0.75, 1e-6);
    chain.modulate(0, GAIN as u8, ModOp::Mul(100.0));
    assert_eq!(chain.fx[0].effective(GAIN), 8.0);
    // process clears the bank
    chain.process(&mut [0.0]);
    assert_near(chain.fx[0].effective(CUTOFF), 500.0, 1e-3);
    // an unknown effect or parameter is ignored
    chain.modulate(7, 0, ModOp::Add(1.0));
    chain.modulate(0, 99, ModOp::Add(1.0));
}

#[test]
fn glide_reaches_target_exactly() {
    let mut chain = chain_of(LOWPASS, &[("cutoff", 1000.0)]);
    chain.set(0, CUTOFF as u8, 4000.0, 100);
    for _ in 0..50 {
        chain.advance();
    }
    assert_near(chain.fx[0].params[CUTOFF].base, 2500.0, 1.0);
    for _ in 0..49 {
        chain.advance();
    }
    assert_ne!(chain.fx[0].params[CUTOFF].base, 4000.0);
    chain.advance();
    assert_eq!(chain.fx[0].params[CUTOFF].base, 4000.0);
    // a value past the range is clamped
    chain.set(0, CUTOFF as u8, 1.0e9, 0);
    assert_eq!(chain.fx[0].params[CUTOFF].base, 20_000.0);
    // a non-finite value is ignored
    chain.set(0, CUTOFF as u8, f32::NAN, 0);
    assert_eq!(chain.fx[0].params[CUTOFF].base, 20_000.0);
}

#[test]
fn insert_with_room_moves_without_allocating() {
    fn is_copy<T: Copy>() {}
    // a slot owns no heap, so moving or dropping one never frees
    is_copy::<FxSlot>();

    let mut chain = FxChain::new();
    assert_eq!(chain.capacity(), 0);
    assert!(
        chain
            .insert(
                0,
                DRIVE_FX,
                params(DRIVE_FX, &[]),
                Some(Vec::with_capacity(1))
            )
            .is_none(),
        "the empty chain owns no buffer"
    );
    assert_eq!(chain.capacity(), 1);

    let room = Vec::with_capacity(2);
    let new_buffer = room.as_ptr();
    let old = chain
        .insert(1, CRUSH_FX, params(CRUSH_FX, &[]), Some(room))
        .expect("the outgrown buffer comes back");
    assert_eq!(old.capacity(), 1);
    assert!(old.is_empty());
    assert_eq!(chain.capacity(), 2);
    assert_eq!(chain.fx.as_ptr(), new_buffer);
    assert_eq!(ids(&chain), [0, 1]);

    // running the chain, editing and gliding never touch the buffer
    let (cap, ptr) = (chain.capacity(), chain.fx.as_ptr());
    chain.set(0, DRIVE as u8, 20.0, 5000);
    chain.modulate(1, BITS as u8, ModOp::Add(1.0));
    for i in 0..10_000 {
        chain.process(&mut [sine(100.0, i), 0.0]);
        chain.advance();
    }
    chain.bypass(0, true, true);
    chain.retype(1, CRUSH_FX, params(CRUSH_FX, &[]));
    assert_eq!((chain.capacity(), chain.fx.as_ptr()), (cap, ptr));
}

#[test]
fn bypassed_slot_passes_dry() {
    let mut chain = chain_of(DRIVE_FX, &[("amount", 40.0)]);
    chain.bypass(0, true, false);
    for i in 0..100 {
        let x = 0.3 * sine(300.0, i);
        let mut frame = [x, -x];
        chain.process(&mut frame);
        assert_eq!(frame, [x, -x]);
    }
    chain.bypass(0, false, false);
    let mut frame = [0.3, 0.3];
    chain.process(&mut frame);
    assert_ne!(frame[0], 0.3);
}

#[test]
fn stop_clears_pause_keeps_filter_state() {
    let ring = |chain: &mut FxChain| {
        for i in 0..400 {
            chain.process(&mut [sine(300.0, i)]);
        }
    };
    let tail = |chain: &mut FxChain| {
        let mut frame = [0.0];
        chain.process(&mut frame);
        frame[0]
    };

    let mut paused = chain_of(LOWPASS, &[("cutoff", 500.0)]);
    ring(&mut paused);
    paused.bypass(0, true, false);
    paused.bypass(0, false, false);
    assert!(tail(&mut paused).abs() > 1e-4, "pause keeps the memory");

    let mut stopped = chain_of(LOWPASS, &[("cutoff", 500.0)]);
    ring(&mut stopped);
    stopped.bypass(0, true, true);
    stopped.bypass(0, false, true);
    assert_eq!(tail(&mut stopped), 0.0, "stop clears it");
}

// ---- on a voice, through the engine ----

fn noise_track() -> AudioFile {
    let mut rng = X128P::new(7);
    let samples: Vec<i16> = (0..4800)
        .map(|_| ((rng.next_f32() * 2.0 - 1.0) * 16000.0) as i16)
        .collect();
    AudioFile::new("n", "wav", 48000, 1, 16, pcm(&samples))
}

// energy of the first difference: high-band energy
fn high_energy(out: &[f32]) -> f32 {
    out.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum()
}

#[test]
fn filter_lowpass_on_voice_darkens_noise() {
    sample_rate::set(48_000);
    let play = |script: &[&str]| {
        let (mut cp, mut c) = setup(1, vec![noise_track()]);
        run(&mut cp, &mut c, "voice n from \"n\" on 1000ms");
        for line in script {
            run(&mut cp, &mut c, line);
        }
        run(&mut cp, &mut c, "start n");
        render(&mut c, 1, 4000)
    };
    let dry = play(&[]);
    let wet = play(&["n { filter lo type=lp cutoff=200 }"]);
    assert!(dry.iter().all(|s| s.is_finite()) && wet.iter().all(|s| s.is_finite()));
    assert!(high_energy(&wet[500..]) < 0.05 * high_energy(&dry[500..]));
}

#[test]
fn fx_glide_over() {
    sample_rate::set(48_000);
    let (mut cp, mut c) = setup(1, vec![noise_track()]);
    run(&mut cp, &mut c, "voice n from \"n\" on 1000ms");
    run(&mut cp, &mut c, "n { filter lo type=lp cutoff=200 }");
    run(&mut cp, &mut c, "start n");
    let cutoff = |c: &crate::audio_processing::engine::Conductor| {
        c.voices.get(0).unwrap().fx.fx[0].params[CUTOFF].base
    };
    assert_eq!(cutoff(&c), 200.0);
    run(&mut cp, &mut c, "n.lo cutoff=4000 over 1000ms");
    render(&mut c, 1, 24_000);
    let mid = cutoff(&c);
    assert!(mid > 1500.0 && mid < 2500.0, "{mid}");
    render(&mut c, 1, 24_000);
    assert_eq!(cutoff(&c), 4000.0);
}

#[test]
fn voice_modulate_routes_to_the_chain() {
    sample_rate::set(48_000);
    let (mut cp, mut c) = setup(1, vec![noise_track()]);
    run(&mut cp, &mut c, "voice n from \"n\" on 1000ms");
    run(&mut cp, &mut c, "n { filter lo type=lp cutoff=500 }");
    let voice = c.voices.get_mut(0).unwrap();
    voice.modulate(
        crate::audio_processing::engine::ModTarget::Fx { id: 0, param: 0 },
        ModOp::Add(1.0),
    );
    assert_near(voice.fx.fx[0].effective(CUTOFF), 1000.0, 1e-3);
}

// ---- on a group's mix (FXb) ----

fn noise(name: &str, seed: u64) -> AudioFile {
    let mut rng = X128P::new(seed);
    let samples: Vec<i16> = (0..4800)
        .map(|_| ((rng.next_f32() * 2.0 - 1.0) * 12000.0) as i16)
        .collect();
    AudioFile::new(name, "wav", 48000, 1, 16, pcm(&samples))
}

// a two-voice session, with `lines` run after the voices exist
fn pair(lines: &[&str]) -> (crate::audio_processing::commands::CmdProcessor, Conductor) {
    sample_rate::set(48_000);
    let (mut cp, mut c) = setup(1, vec![noise("a", 3), noise("b", 5)]);
    c.apply(cp.boot());
    run(&mut cp, &mut c, "voice a from \"a\" on 1000ms");
    run(&mut cp, &mut c, "voice b from \"b\" on 1000ms");
    for line in lines {
        run(&mut cp, &mut c, line);
    }
    (cp, c)
}

const N: usize = 1500;

// one voice on its own
fn solo(name: &str) -> Vec<f32> {
    let (mut cp, mut c) = pair(&[]);
    run(&mut cp, &mut c, &format!("start {name}"));
    render(&mut c, 1, N)
}

// a one-effect chain run over a signal
fn through(kind: FxKind, over: &[(&str, f32)], signal: &[f32]) -> Vec<f32> {
    let mut chain = chain_of(kind, over);
    signal
        .iter()
        .map(|&x| {
            let mut f = [x];
            chain.process(&mut f);
            f[0]
        })
        .collect()
}

fn sum(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

#[test]
fn group_without_fx_renders_identical() {
    let (a, b) = (solo("a"), solo("b"));
    let (_cp, mut c) = pair(&["group g { a; b }", "g gain=0.5", "start g"]);
    let got = render(&mut c, 1, N);
    // a power of two scales exactly, so the sum is bit for bit
    let want: Vec<f32> = sum(&a, &b).iter().map(|x| x * 0.5).collect();
    assert_eq!(got, want);
}

#[test]
fn group_fx_runs_once_on_the_mix() {
    let (a, b) = (solo("a"), solo("b"));
    let (_cp, mut c) = pair(&["group g { a; b }", "g { crush q bits=3 }", "start g"]);
    let got = render(&mut c, 1, N);
    let over = [("bits", 3.0)];
    assert_eq!(got, through(CRUSH_FX, &over, &sum(&a, &b)));
    // not the sum of two crushed signals
    let each = sum(&through(CRUSH_FX, &over, &a), &through(CRUSH_FX, &over, &b));
    assert_ne!(got, each);
}

#[test]
fn member_fx_then_group_fx_order() {
    let (a, b) = (solo("a"), solo("b"));
    let (_cp, mut c) = pair(&[
        "a { drive d amount=24 }",
        "group g { a; b }",
        "g { crush q bits=3 }",
        "start g",
    ]);
    let got = render(&mut c, 1, N);
    let driven = through(DRIVE_FX, &[("amount", 24.0)], &a);
    let want = through(CRUSH_FX, &[("bits", 3.0)], &sum(&driven, &b));
    assert_eq!(got, want);
}

#[test]
fn group_fx_gain_after_chain() {
    let (a, b) = (solo("a"), solo("b"));
    let (_cp, mut c) = pair(&[
        "group g { a; b }",
        "g { crush q bits=3 }",
        "g gain=0.5",
        "start g",
    ]);
    let got = render(&mut c, 1, N);
    let want: Vec<f32> = through(CRUSH_FX, &[("bits", 3.0)], &sum(&a, &b))
        .iter()
        .map(|x| x * 0.5)
        .collect();
    assert_eq!(got, want);
}

#[test]
fn group_fx_modulation_cleared_each_frame() {
    let (_cp, mut c) = pair(&[
        "group g { a; b }",
        "g { filter lo type=lp cutoff=500 }",
        "start g",
    ]);
    let g = c.groups.iter_mut().next().unwrap().1;
    g.modulate_fx(0, CUTOFF as u8, ModOp::Add(1.0));
    assert_near(g.fx.fx[0].effective(CUTOFF), 1000.0, 1e-3);
    render(&mut c, 1, 1);
    let g = c.groups.iter().next().unwrap().1;
    assert_near(g.fx.fx[0].effective(CUTOFF), 500.0, 1e-3);
}

#[test]
fn stopped_group_runs_no_chain_and_restarts_clean() {
    let lines = [
        "group g { a; b }",
        "g { filter lo type=lp cutoff=200 }",
        "start g",
    ];
    let (mut cp, mut c) = pair(&lines);
    render(&mut c, 1, 100);
    run(&mut cp, &mut c, "stop g");
    assert!(render(&mut c, 1, 10).iter().all(|s| *s == 0.0));
    run(&mut cp, &mut c, "start g");
    let first = render(&mut c, 1, N);
    let (_cp2, mut c2) = pair(&lines);
    assert_eq!(first, render(&mut c2, 1, N));
}
