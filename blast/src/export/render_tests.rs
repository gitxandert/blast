//! PKc's tests: `--export` renders, through scanned WAV assets in a temp
//! folder, with the real `Conductor` and runner.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::render::{BLOCK_FRAMES, ExportErr, ExportOpts, render_script};
use crate::audio_processing::blast_rand::{clear_session_seed, set_session_seed};
use crate::audio_processing::blast_time::sample_rate;
use crate::audio_processing::commands::{CmdProcessor, EngineState, cmd_queue};
use crate::audio_processing::engine::{Conductor, Event};
use crate::cli::Length;
use crate::file_parsing::decode_helpers::AudioFile;
use crate::file_parsing::{load_assets, preferred_config, scan_assets};

const RATE: u32 = 48_000;

// a folder of its own per test, with `assets/kick.wav` (half a second of
// noise, mono) and the scripts written into it
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("blast-pkc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(dir.join("assets/kick.wav"), spec).unwrap();
        let mut x: u32 = 12345;
        for _ in 0..RATE / 2 {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            w.write_sample(((x >> 16) as i16) / 4).unwrap();
        }
        w.finalize().unwrap();
        Fixture { dir }
    }

    fn script(&self, text: &str) -> PathBuf {
        let path = self.dir.join("t.blast");
        std::fs::write(&path, text).unwrap();
        path
    }

    fn opts(&self, script: &str) -> ExportOpts {
        ExportOpts {
            assets_dir: self.dir.join("assets"),
            sources_dir: self.dir.join("sources"),
            script: self.script(script),
            out: Some(self.dir.join("out.wav")),
            length: None,
            bits: 32,
            rate: Some(RATE),
            seed: 0,
            progress: false,
        }
    }

    fn out(&self) -> PathBuf {
        self.dir.join("out.wav")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn floats(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).unwrap();
    r.samples::<f32>().map(Result::unwrap).collect()
}

// a script with jitter and chance, so the seed matters
const RANDOM: &str = "voice kick from \"kick\"\n\
    kick { seq s steps=8 grid=16 chance=50 jitter=0.2 in 4 | trig }\n\
    start kick\n\
    wait 4\n";

#[test]
fn export_is_bit_identical_across_runs() {
    let fx = Fixture::new("ident");
    let opts = fx.opts(RANDOM);
    render_script(&opts).unwrap();
    let first = std::fs::read(fx.out()).unwrap();
    std::fs::remove_file(fx.out()).unwrap();
    render_script(&opts).unwrap();
    let second = std::fs::read(fx.out()).unwrap();
    assert!(first.len() > 44 + 4 * RATE as usize, "should be 2 s long");
    assert!(
        floats(&fx.out()).iter().any(|x| x.abs() > 0.01),
        "should not be silent"
    );
    assert_eq!(first, second);
}

#[test]
fn export_seed_changes_output() {
    let fx = Fixture::new("seed");
    let mut opts = fx.opts(RANDOM);
    render_script(&opts).unwrap();
    let a = floats(&fx.out());
    opts.seed = 1;
    render_script(&opts).unwrap();
    let b = floats(&fx.out());
    assert_eq!(a.len(), b.len());
    assert_ne!(a, b);
}

#[test]
fn export_length_flag() {
    let fx = Fixture::new("length");
    let mut opts = fx.opts("voice kick from \"kick\"\nstart kick\n");
    opts.length = Some(Length::Beats(8.0));
    let s = render_script(&opts).unwrap();
    // 8 beats at 120 bpm
    assert_eq!(s.frames, 4 * u64::from(RATE));
    assert_eq!(floats(&fx.out()).len(), 4 * RATE as usize);
    opts.length = Some(Length::Secs(2.0));
    let s = render_script(&opts).unwrap();
    assert_eq!(s.frames, 2 * u64::from(RATE));
    assert_eq!(floats(&fx.out()).len(), 2 * RATE as usize);
    // a length that is not a whole number of blocks is cut to the frame
    opts.length = Some(Length::Secs(0.001));
    assert_eq!(render_script(&opts).unwrap().frames, 48);
}

#[test]
fn export_zero_length_e207() {
    let fx = Fixture::new("zero");
    let opts = fx.opts("voice kick from \"kick\"\nstart kick\n");
    let Err(ExportErr::Script(text)) = render_script(&opts) else {
        panic!("should be refused");
    };
    assert!(
        text.starts_with("E207 export: the script ends at once"),
        "{text}"
    );
    assert!(text.contains("--length"));
    assert!(!fx.out().exists(), "nothing is written");
}

#[test]
fn export_script_errors_write_nothing() {
    let fx = Fixture::new("errors");
    let opts = fx.opts("voice kick from \"kick\"\nstart nope\n");
    let Err(ExportErr::Script(text)) = render_script(&opts) else {
        panic!("should fail");
    };
    assert!(text.contains("E301"), "{text}");
    assert!(!fx.out().exists());
}

#[test]
fn export_ends_after_glides() {
    let fx = Fixture::new("glides");
    let opts = fx.opts("voice kick from \"kick\"\nstart kick\nkick gain=0 over 4b then end\n");
    let s = render_script(&opts).unwrap();
    // 4 beats at 120 bpm, cut to the frame the end fired on
    assert_eq!(s.frames, 2 * u64::from(RATE));
    assert_eq!(floats(&fx.out()).len(), 2 * RATE as usize);

    // a glide with no end of its own still runs out
    let opts = fx.opts("voice kick from \"kick\"\nstart kick\nkick gain=0 over 3b\n");
    let s = render_script(&opts).unwrap();
    assert_eq!(s.frames, 72_000);

    // the end waits for a glide that is still going: the script ends at 1b,
    // the glide at 4b
    let opts = fx.opts("voice kick from \"kick\"\nstart kick\nkick gain=0 over 4b\nwait 1\nend\n");
    let s = render_script(&opts).unwrap();
    assert_eq!(s.frames, 2 * u64::from(RATE));
}

#[test]
fn export_last_wait_is_the_end() {
    let fx = Fixture::new("wait");
    let opts = fx.opts("voice kick from \"kick\"\nstart kick\nwait 2\nstop kick\nwait 1\n");
    let s = render_script(&opts).unwrap();
    assert_eq!(s.frames, 72_000);
    let samples = floats(&fx.out());
    // the kick sounds, then stops at 2 beats
    assert!(samples[..24_000].iter().any(|x| x.abs() > 0.01));
    assert!(samples[49_000..60_000].iter().all(|x| *x == 0.0));
}

#[test]
fn export_bits_16_24() {
    let fx = Fixture::new("bits");
    let mut opts = fx.opts(RANDOM);
    opts.length = Some(Length::Secs(1.0));
    render_script(&opts).unwrap();
    let float = floats(&fx.out());

    for bits in [16u32, 24] {
        opts.bits = bits;
        render_script(&opts).unwrap();
        let mut r = hound::WavReader::open(fx.out()).unwrap();
        let spec = r.spec();
        assert_eq!(spec.bits_per_sample, bits as u16);
        assert_eq!(spec.sample_format, hound::SampleFormat::Int);
        assert_eq!(spec.sample_rate, RATE);
        assert_eq!(spec.channels, 1);
        let ints: Vec<i32> = r.samples::<i32>().map(Result::unwrap).collect();
        assert_eq!(ints.len(), float.len());
        let scale = (1u32 << (bits - 1)) as f32;
        // within the dither (one LSB) of the float render, and not all
        // quantised the same way
        let mut dithered = 0;
        for (i, f) in ints.iter().zip(&float) {
            let want = f.clamp(-1.0, 1.0) * scale;
            assert!((*i as f32 - want).abs() <= 2.0, "{i} vs {want}");
            if (*i as f32 - want.round()).abs() >= 1.0 {
                dithered += 1;
            }
        }
        assert!(dithered > 0, "{bits} bits should be dithered");
        // dither is seeded too: the same render repeats
        let first = std::fs::read(fx.out()).unwrap();
        render_script(&opts).unwrap();
        assert_eq!(first, std::fs::read(fx.out()).unwrap());
    }
}

#[test]
fn export_default_rate_is_the_assets() {
    let fx = Fixture::new("rate");
    let mut opts = fx.opts(RANDOM);
    opts.length = Some(Length::Secs(1.0));
    opts.rate = None;
    let s = render_script(&opts).unwrap();
    // the assets' own rate
    assert_eq!(s.sample_rate, RATE);
    // an explicit rate that matches is the same render
    opts.rate = Some(RATE);
    assert_eq!(render_script(&opts).unwrap().frames, u64::from(RATE));
}

#[test]
fn export_writes_buffer_takes() {
    let fx = Fixture::new("takes");
    let ended = fx.dir.join("ended.wav");
    let open = fx.dir.join("open.wav");
    let script = format!(
        "voice kick from \"kick\"\n\
         start kick\n\
         buffer a length=4800f to=\"{}\"\n\
         buffer b to=\"{}\"\n\
         start a\n\
         start b\n\
         wait 1\n",
        ended.display(),
        open.display()
    );
    let opts = fx.opts(&script);
    let s = render_script(&opts).unwrap();
    assert_eq!(s.frames, 24_000);
    // a take that ended by itself
    assert_eq!(hound::WavReader::open(&ended).unwrap().len(), 4800);
    // one still running when the script ended is stopped there and written
    // (up to a few blocks more: the engine renders whole blocks, and the
    // end is known a little late)
    let n = hound::WavReader::open(&open).unwrap().len() as u64;
    assert!(
        (24_000..24_000 + 8 * BLOCK_FRAMES as u64).contains(&n),
        "{n}"
    );
}

// the same script through the shared step by hand, the way a session drives
// it, in 64-frame blocks
fn live_render(fx: &Fixture, text: &str, frames: usize) -> Vec<f32> {
    set_session_seed(0);
    let assets = scan_assets(&fx.dir.join("assets")).unwrap();
    let (rate, channels) = preferred_config(&assets).unwrap();
    let files = load_assets(&assets, rate, channels).unwrap();
    sample_rate::set(rate);
    let mut state = EngineState::new(files.clone(), channels as usize);
    state.set_assets_dir(Some(fx.dir.join("assets")));
    let tracks: HashMap<String, AudioFile> = files
        .into_iter()
        .map(|f| (f.file_name.clone(), f))
        .collect();
    let mut conductor = Conductor::prepare(channels as usize, tracks);
    let mut erx = conductor.take_rx().unwrap();
    let (mut tx, mut rx) = cmd_queue(256);
    let mut cp = CmdProcessor::new(state);
    cp.set_sources_dir(fx.dir.join("sources"));
    tx.try_push(cp.boot()).ok().unwrap();
    cp.run_script_text(&fx.script(text), &mut tx).unwrap();

    let mut out = Vec::new();
    let mut block = vec![0.0f32; BLOCK_FRAMES * channels as usize];
    while out.len() < frames * channels as usize {
        conductor.run_block(&mut rx, &mut block, channels as usize);
        out.extend_from_slice(&block);
        for event in erx.events() {
            if let Event::ScheduleFired {
                seq_no,
                total,
                applied,
            } = event
            {
                cp.engine_state.note_schedule_fired(seq_no, total, applied);
            }
        }
        erx.drain_saves();
        let _ = cp.pump(&mut tx);
    }
    out.truncate(frames * channels as usize);
    clear_session_seed();
    out
}

#[test]
fn export_matches_live_render() {
    let fx = Fixture::new("live");
    let script = "voice kick from \"kick\"\n\
        kick { seq s steps=8 grid=16 chance=60 jitter=0.1 in 4 | trig }\n\
        start kick\n\
        wait 1\n\
        kick pan=-1 over 1b\n\
        wait 1\n\
        kick gain=0.5\n\
        wait 1\n";
    let opts = fx.opts(script);
    let s = render_script(&opts).unwrap();
    assert_eq!(s.frames, 3 * 24_000);
    let exported = floats(&fx.out());
    let live = live_render(&fx, script, exported.len());
    assert_eq!(exported.len(), live.len());
    assert!(exported.iter().any(|x| x.abs() > 0.01));
    assert!(exported == live, "export and the live step differ");
}
