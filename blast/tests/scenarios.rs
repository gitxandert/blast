//! Rendered scenario checks (SCN). Every `tests/scenarios/*.blast` is a
//! script with `# scn:` settings and `# expect:` assertions in its comments.
//! It is rendered offline with `blast --export`'s renderer against synthetic
//! assets, and each expectation is measured on the result (see
//! `tests/scn/expect.rs` for the language and `plans/detailed/SCN.md`).
//!
//! Scenarios under `tests/scn/self_test/` are deliberately bad: every one of
//! their expectations must fail, which pins the detectors' thresholds.
//! `equals golden` is blessed with `BLAST_BLESS=1 cargo test --test scenarios`.

mod scn;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use blast::cli::Length;
use blast::export::render::{ExportOpts, render_script};
use scn::expect::{self, Env, Expect, Scenario, Time, Units};
use scn::{assets, sha256};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The `.blast` files directly inside `dir`, sorted.
fn blast_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "blast"))
        // `NAME.b.blast` is the B side of `tools/ab.sh NAME`, not a scenario
        .filter(|p| !p.to_string_lossy().ends_with(".b.blast"))
        .collect();
    files.sort();
    files
}

/// A scenario and what its render gave.
struct Rendered {
    sc: Scenario,
    /// Interleaved samples and the channel count, or why it failed.
    audio: Result<(Vec<f32>, usize), String>,
}

fn read_wav(path: &Path) -> Result<(Vec<f32>, usize), String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        }
    };
    Ok((samples, usize::from(spec.channels)))
}

/// Renders one scenario in its own temp folder.
///
/// `SCN_KEEP=DIR` keeps every render as `DIR/<scenario>.wav`, to listen to;
/// `copy_to` (the A/B test) names the file to leave the render in.
fn render(path: &Path, sc: &Scenario, copy_to: Option<&Path>) -> Result<(Vec<f32>, usize), String> {
    // tests run in parallel and may render the same file: one folder each
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let work = std::env::temp_dir().join(format!(
        "blast-scn-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        sc.name
    ));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let done = (|| {
        assets::make(&work, &sc.header.assets)?;
        let out = work.join("out.wav");
        let length = sc.header.length.map(|t| match t {
            Time::Beats(b) => Length::Beats(b),
            Time::Secs(s) => Length::Secs(s),
            Time::Frames(f) => Length::Secs(f / f64::from(assets::RATE)),
        });
        let opts = ExportOpts {
            assets_dir: work.clone(),
            sources_dir: work.join("sources"),
            script: path.to_path_buf(),
            out: Some(out.clone()),
            length,
            bits: 32,
            rate: Some(assets::RATE),
            seed: sc.header.seed,
            progress: false,
        };
        render_script(&opts).map_err(|e| format!("render failed: {e}"))?;
        if let Some(keep) = std::env::var_os("SCN_KEEP") {
            let _ = std::fs::create_dir_all(&keep);
            let _ = std::fs::copy(&out, Path::new(&keep).join(format!("{}.wav", sc.name)));
        }
        if let Some(dest) = copy_to {
            std::fs::copy(&out, dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        }
        let (samples, channels) = read_wav(&out)?;
        if samples.iter().any(|v| !v.is_finite()) {
            return Err("the render has NaN or infinite samples".into());
        }
        Ok((samples, channels))
    })();
    let _ = std::fs::remove_dir_all(&work);
    done
}

/// Parses and renders every file, one thread each. A parse error is an `Err`
/// with its messages; a panic in a render is that scenario's render error.
fn render_all(files: &[PathBuf]) -> Vec<Result<Rendered, Vec<String>>> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .iter()
            .map(|path| {
                std::thread::Builder::new()
                    .stack_size(16 << 20)
                    .spawn_scoped(scope, move || {
                        let file = path.file_name().unwrap().to_string_lossy().into_owned();
                        let text = std::fs::read_to_string(path)
                            .map_err(|e| vec![format!("{file}: {e}")])?;
                        let sc = expect::parse_scenario(&file, &text)?;
                        let audio = render(path, &sc, None);
                        Ok(Rendered { sc, audio })
                    })
                    .unwrap()
            })
            .collect();
        handles
            .into_iter()
            .zip(files)
            .map(|(h, path)| {
                h.join().unwrap_or_else(|_| {
                    Err(vec![format!("{}: the render panicked", path.display())])
                })
            })
            .collect()
    })
}

/// `equals golden`: the hash of the render against `scn/golden/<name>.sha256`.
fn golden_check(name: &str, samples: &[f32]) -> Result<(), String> {
    let hash = sha256::hash_samples(samples);
    let file = crate_dir().join(format!("tests/scn/golden/{name}.sha256"));
    if std::env::var_os("BLAST_BLESS").is_some() {
        std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
        return std::fs::write(&file, format!("{hash}\n")).map_err(|e| e.to_string());
    }
    let want = std::fs::read_to_string(&file)
        .map_err(|_| format!("no {}: run with BLAST_BLESS=1", file.display()))?;
    if hash == want.trim() {
        Ok(())
    } else {
        Err("the render changed; if that is intended, run with BLAST_BLESS=1".into())
    }
}

/// A scenario with each expectation's verdict, or why it did not render.
type Judged<'a> = (&'a Scenario, Result<Vec<Verdict<'a>>, String>);
type Verdict<'a> = (&'a Expect, Result<(), String>);

/// Every expectation of every rendered scenario with its verdict. A scenario
/// that did not render gives one `Err` with the reason.
fn judge(all: &[Rendered]) -> Vec<Judged<'_>> {
    let others: HashMap<String, Vec<f32>> = all
        .iter()
        .filter_map(|r| Some((r.sc.name.clone(), r.audio.as_ref().ok()?.0.clone())))
        .collect();
    all.iter()
        .map(|r| {
            let verdicts = r
                .audio
                .as_ref()
                .map_err(String::clone)
                .map(|(samples, channels)| {
                    let golden = |s: &[f32]| golden_check(&r.sc.name, s);
                    let env = Env {
                        units: Units {
                            rate: assets::RATE,
                            beat_secs: r.sc.beat_secs,
                        },
                        channels: *channels,
                        samples,
                        others: &others,
                        golden: &golden,
                    };
                    r.sc.expects
                        .iter()
                        .map(|e| (e, expect::check(&e.check, &env)))
                        .collect()
                });
            (&r.sc, verdicts)
        })
        .collect()
}

/// Renders the files and returns one message per failed expectation, parse
/// error or failed render.
fn failures(files: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    let mut rendered = Vec::new();
    for r in render_all(files) {
        match r {
            Ok(r) if r.sc.expects.is_empty() => {
                out.push(format!("{}: no `# expect:` lines", r.sc.file));
            }
            Ok(r) => rendered.push(r),
            Err(errors) => out.extend(errors),
        }
    }
    for (sc, verdicts) in judge(&rendered) {
        match verdicts {
            Err(why) => out.push(format!("{}: {why}", sc.file)),
            Ok(list) => {
                for (e, verdict) in list {
                    if let Err(why) = verdict {
                        out.push(format!("{}:{}: {}: {why}", sc.file, e.line, e.text));
                    }
                }
            }
        }
    }
    out
}

#[test]
fn scenarios_pass() {
    let files = blast_files(&crate_dir().join("tests/scenarios"));
    assert!(!files.is_empty(), "no scenarios found");
    let failed = failures(&files);
    assert!(
        failed.is_empty(),
        "{} scenario check(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// The files in `tests/scn/self_test/` are bad on purpose: each expectation
/// in them must fail, and for a reason a bug of that kind would give.
#[test]
fn self_test_scenarios_fail() {
    let files = blast_files(&crate_dir().join("tests/scn/self_test"));
    assert!(files.len() >= 2, "the self test scenarios are missing");
    let mut rendered = Vec::new();
    for r in render_all(&files) {
        match r {
            Ok(r) => rendered.push(r),
            Err(errors) => panic!("self test does not parse: {errors:?}"),
        }
    }
    let mut passed = Vec::new();
    for (sc, verdicts) in judge(&rendered) {
        let list = verdicts.unwrap_or_else(|why| panic!("{}: {why}", sc.file));
        for (e, verdict) in list {
            match verdict {
                Err(_) => {}
                Ok(()) => passed.push(format!("{}:{}: {} passed", sc.file, e.line, e.text)),
            }
        }
    }
    assert!(
        passed.is_empty(),
        "expectations that should have failed:\n{}",
        passed.join("\n")
    );
}

/// The renderer and the detector together: a ping-pong turnaround with
/// `bounce=0` clicks (caught by `no clicks`), and with the default fade it
/// does not (`scenarios/scn-ppf.blast`). This is the first half.
#[test]
fn ppf_without_bounce_is_a_click() {
    let file = crate_dir().join("tests/scn/self_test/ppf-bounce0.blast");
    let failed = failures(&[file]);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        failed[0].contains("no clicks") && failed[0].contains("click(s)"),
        "{failed:?}"
    );
}

/// `tools/ab.sh`: renders two scripts (each with its own `# scn:` assets)
/// into WAV files for listening. Not a check, so it only runs when asked:
/// `AB_A=a.blast AB_B=b.blast AB_OUT_A=a.wav AB_OUT_B=b.wav cargo test
/// --test scenarios -- --ignored ab_render`.
#[test]
#[ignore = "run by tools/ab.sh"]
fn ab_render() {
    let var = |k: &str| PathBuf::from(std::env::var_os(k).unwrap_or_else(|| panic!("{k} not set")));
    let sides = [("AB_A", "AB_OUT_A"), ("AB_B", "AB_OUT_B")].map(|(i, o)| (var(i), var(o)));
    std::thread::scope(|scope| {
        let jobs: Vec<_> = sides
            .iter()
            .map(|(input, out)| {
                scope.spawn(move || {
                    let file = input.file_name().unwrap().to_string_lossy().into_owned();
                    let text =
                        std::fs::read_to_string(input).map_err(|e| format!("{file}: {e}"))?;
                    let sc = expect::parse_scenario(&file, &text).map_err(|e| e.join("\n"))?;
                    render(input, &sc, Some(out)).map(|_| ())
                })
            })
            .collect();
        for job in jobs {
            job.join().unwrap().unwrap_or_else(|e| panic!("{e}"));
        }
    });
}

/// Engine problems found by scenarios and filed as requests live in
/// `tests/scn/known/`; each must still fail its `no clicks`. When one starts
/// passing, the engine was fixed: move the scenario into `tests/scenarios/`.
/// A missing or empty folder passes, so the task that fixes the engine can
/// move them all out.
#[test]
fn known_engine_issues_still_fail() {
    let dir = crate_dir().join("tests/scn/known");
    if !dir.is_dir() {
        return;
    }
    for file in blast_files(&dir) {
        let failed = failures(std::slice::from_ref(&file));
        assert!(
            failed.iter().any(|f| f.contains("click(s)")),
            "{} no longer clicks: the engine was fixed, so move it to tests/scenarios/ ({failed:?})",
            file.display()
        );
    }
}
