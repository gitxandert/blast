//! The scenario language: `# scn:` lines set the render, `# expect:` lines
//! are checks on the result (see `plans/detailed/SCN.md`). Parsing is
//! separate from checking so that a typo fails with its line number before
//! anything renders.
//!
//! ```text
//! # scn: length 8b, seed 0, assets click sine440
//! # expect: peak <= -1dbfs
//! # expect: rms 0b..4b >= -30dbfs
//! # expect: onsets on main every 1b within 64f
//! # expect: onsets at 0b 2b within 10ms
//! # expect: no clicks
//! # expect: pitch 2b..3b 880hz within 5c
//! # expect: silent after 7b
//! # expect: equals other-scenario within -90dbfs
//! # expect: equals golden
//! ```
//!
//! Times are beats of `main` (`b`), `ms`, `s` or frames (`f`). Beats assume
//! a constant `main` tempo: one `clock main Nb|Nms|Nf` line at most (120 bpm
//! without one); anything else is a parse error. `onsets` measures the mix;
//! `onsets left on main ...` / `right` picks one channel. Levels (`peak`,
//! `rms`, `silent after`) and `no clicks` look at every channel and take the
//! worst.

use std::collections::HashMap;

use super::measure::{self, Cmp};

/// A moment or a length, in the unit it was written in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Time {
    /// Beats of `main`.
    Beats(f64),
    /// Seconds.
    Secs(f64),
    /// Frames.
    Frames(f64),
}

/// A time window `a..b`.
pub type Window = (Time, Time);

/// How to read the render: its rate and the length of a beat of `main`.
#[derive(Clone, Copy, Debug)]
pub struct Units {
    /// Sample rate of the render.
    pub rate: u32,
    /// Seconds in one beat of `main`.
    pub beat_secs: f64,
}

impl Units {
    /// The time in frames (fractional).
    pub fn frames(&self, t: Time) -> f64 {
        match t {
            Time::Beats(b) => b * self.beat_secs * f64::from(self.rate),
            Time::Secs(s) => s * f64::from(self.rate),
            Time::Frames(f) => f,
        }
    }

    /// Frames as `1.50b (72000f)`.
    pub fn show(&self, frames: f64) -> String {
        let beats = frames / (self.beat_secs * f64::from(self.rate));
        format!("{beats:.3}b ({frames:.0}f)")
    }
}

/// What a `# scn:` line asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Header {
    /// `length T`: as `--length`.
    pub length: Option<Time>,
    /// `seed N` (default 0).
    pub seed: u64,
    /// `assets a b c`: synthetic assets to make.
    pub assets: Vec<String>,
}

/// One check.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    /// `peak [a..b] <= X` or `>= X`.
    Peak(Option<Window>, Cmp, f64),
    /// `rms [a..b] <= X` or `>= X`.
    Rms(Option<Window>, Cmp, f64),
    /// `onsets [left|right] on main every D [in a..b] within T`.
    OnsetGrid {
        /// `Some(0)` for left, `Some(1)` for right.
        channel: Option<usize>,
        /// The grid step.
        every: Time,
        /// Where the grid applies (default: the whole render).
        window: Option<Window>,
        /// The tolerance.
        within: Time,
    },
    /// `onsets at t1 t2 ... within T`.
    OnsetsAt(Vec<Time>, Time),
    /// `no clicks`.
    NoClicks,
    /// `pitch [a..b] F within C`: F in Hz, C in cents.
    Pitch(Option<Window>, f64, f64),
    /// `silent after T`.
    SilentAfter(Time),
    /// `equals NAME within X` (X in dBFS); `golden` when `NAME` is `golden`.
    Equals(String, f64),
}

/// A check with where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Expect {
    /// 1-based line in the scenario file.
    pub line: usize,
    /// The text after `# expect:`.
    pub text: String,
    /// The check.
    pub check: Check,
}

/// A parsed scenario file.
#[derive(Clone, Debug)]
pub struct Scenario {
    /// File name, e.g. `scn-basics.blast`.
    pub file: String,
    /// Name without the extension, e.g. `scn-basics`.
    pub name: String,
    /// The render settings.
    pub header: Header,
    /// The checks, in file order.
    pub expects: Vec<Expect>,
    /// Seconds in one beat of `main` (from its `clock main …` line).
    pub beat_secs: f64,
}

// ---- parsing ----

fn num(s: &str) -> Result<f64, String> {
    s.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("`{s}` is not a number"))
}

/// `1.5b`, `250ms`, `2s`, `64f`.
pub fn parse_time(s: &str) -> Result<Time, String> {
    let (digits, make): (&str, fn(f64) -> Time) = if let Some(d) = s.strip_suffix("ms") {
        (d, |v| Time::Secs(v / 1000.0))
    } else if let Some(d) = s.strip_suffix('s') {
        (d, Time::Secs)
    } else if let Some(d) = s.strip_suffix('b') {
        (d, Time::Beats)
    } else if let Some(d) = s.strip_suffix('f') {
        (d, Time::Frames)
    } else {
        return Err(format!("`{s}` needs a unit: b, ms, s or f"));
    };
    Ok(make(num(digits)?))
}

fn parse_window(s: &str) -> Result<Window, String> {
    let (a, b) = s
        .split_once("..")
        .ok_or_else(|| format!("`{s}` is not a window like 1b..3b"))?;
    Ok((parse_time(a)?, parse_time(b)?))
}

fn parse_db(s: &str) -> Result<f64, String> {
    let digits = s
        .strip_suffix("dbfs")
        .or_else(|| s.strip_suffix("db"))
        .unwrap_or(s);
    num(digits).map_err(|_| format!("`{s}` is not a level like -6dbfs"))
}

/// A frequency: `880hz`, `440`, or a note name (`a4`, `c#3`, `eb2`; A4 = 440).
pub fn parse_freq(s: &str) -> Result<f64, String> {
    if let Some(d) = s.strip_suffix("hz") {
        return num(d);
    }
    if let Ok(v) = s.parse::<f64>() {
        return Ok(v);
    }
    let lower = s.to_ascii_lowercase();
    let mut chars = lower.chars();
    let base = match chars.next() {
        Some('c') => 0,
        Some('d') => 2,
        Some('e') => 4,
        Some('f') => 5,
        Some('g') => 7,
        Some('a') => 9,
        Some('b') => 11,
        _ => return Err(format!("`{s}` is not a frequency or a note name")),
    };
    let rest: String = chars.collect();
    let (shift, octave) = match rest.chars().next() {
        Some('#') => (1, &rest[1..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, &rest[..]),
    };
    let octave: i32 = octave
        .parse()
        .map_err(|_| format!("`{s}` is not a frequency or a note name"))?;
    let midi = 12 * (octave + 1) + base + shift;
    Ok(440.0 * 2f64.powf(f64::from(midi - 69) / 12.0))
}

fn parse_cmp(s: &str) -> Result<Cmp, String> {
    match s {
        "<=" | "<" => Ok(Cmp::AtMost),
        ">=" | ">" => Ok(Cmp::AtLeast),
        _ => Err(format!("expected <= or >=, found `{s}`")),
    }
}

/// An optional leading window, then the rest.
fn window_first<'a>(words: &'a [&'a str]) -> Result<(Option<Window>, &'a [&'a str]), String> {
    match words.first() {
        Some(w) if w.contains("..") => Ok((Some(parse_window(w)?), &words[1..])),
        _ => Ok((None, words)),
    }
}

fn value_after<'a>(words: &[&'a str], key: &str) -> Result<&'a str, String> {
    let at = words
        .iter()
        .position(|w| *w == key)
        .ok_or_else(|| format!("missing `{key}`"))?;
    words
        .get(at + 1)
        .copied()
        .ok_or_else(|| format!("`{key}` needs a value"))
}

fn parse_check(text: &str) -> Result<Check, String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let Some((&head, rest)) = words.split_first() else {
        return Err("empty expectation".into());
    };
    match head {
        "peak" | "rms" => {
            let (win, rest) = window_first(rest)?;
            let [cmp, level] = rest else {
                return Err(format!("`{head}` takes [a..b] <= X or >= X"));
            };
            let (cmp, level) = (parse_cmp(cmp)?, parse_db(level)?);
            Ok(if head == "peak" {
                Check::Peak(win, cmp, level)
            } else {
                Check::Rms(win, cmp, level)
            })
        }
        "no" if rest == ["clicks"] => Ok(Check::NoClicks),
        "silent" => match rest {
            ["after", t] => Ok(Check::SilentAfter(parse_time(t)?)),
            _ => Err("`silent` takes `after T`".into()),
        },
        "pitch" => {
            let (win, rest) = window_first(rest)?;
            let [freq, "within", cents] = rest else {
                return Err("`pitch` takes [a..b] F within C".into());
            };
            let cents = cents.strip_suffix('c').unwrap_or(cents);
            Ok(Check::Pitch(win, parse_freq(freq)?, num(cents)?))
        }
        "equals" => match rest {
            ["golden"] => Ok(Check::Equals("golden".into(), 0.0)),
            [name, "within", x] => Ok(Check::Equals((*name).into(), parse_db(x)?)),
            _ => Err("`equals` takes NAME within X, or `golden`".into()),
        },
        "onsets" if rest.first() == Some(&"at") => {
            let at = rest
                .iter()
                .position(|w| *w == "within")
                .ok_or("`onsets at` needs `within T`")?;
            let times = rest[1..at]
                .iter()
                .map(|t| parse_time(t))
                .collect::<Result<Vec<_>, _>>()?;
            if times.is_empty() {
                return Err("`onsets at` needs at least one time".into());
            }
            let within = parse_time(rest.get(at + 1).ok_or("`within` needs a time")?)?;
            Ok(Check::OnsetsAt(times, within))
        }
        "onsets" => {
            let on = rest
                .iter()
                .position(|w| *w == "on")
                .ok_or("`onsets` takes [left|right] on main every D within T")?;
            let channel = match &rest[..on] {
                [] => None,
                ["left"] => Some(0),
                ["right"] => Some(1),
                other => {
                    return Err(format!(
                        "`onsets {} on`: the only labels are `left` and `right`",
                        other.join(" ")
                    ));
                }
            };
            if rest.get(on + 1) != Some(&"main") {
                return Err("`onsets … on` only knows the clock `main`".into());
            }
            let every = parse_time(value_after(rest, "every")?)?;
            let within = parse_time(value_after(rest, "within")?)?;
            let window = match rest.iter().position(|w| *w == "in") {
                Some(i) => Some(parse_window(rest.get(i + 1).ok_or("`in` needs a window")?)?),
                None => None,
            };
            Ok(Check::OnsetGrid {
                channel,
                every,
                window,
                within,
            })
        }
        other => Err(format!("unknown expectation `{other}`")),
    }
}

fn parse_header_line(h: &mut Header, text: &str) -> Result<(), String> {
    for part in text.split(',') {
        let words: Vec<&str> = part.split_whitespace().collect();
        match words.as_slice() {
            [] => {}
            ["length", t] => h.length = Some(parse_time(t)?),
            ["seed", n] => {
                h.seed = n.parse().map_err(|_| format!("`{n}` is not a seed"))?;
            }
            ["assets", names @ ..] if !names.is_empty() => {
                h.assets.extend(names.iter().map(|n| n.to_string()));
            }
            _ => return Err(format!("unknown setting `{}`", part.trim())),
        }
    }
    Ok(())
}

/// Seconds per beat of `main`: its one `clock main Xb`, `Xms` or `Xf` line
/// (frames at the scenario rate), or 120 bpm with no such line. Times in
/// beats assume the tempo never changes, so a `clock main` line the parser
/// can't read as a constant, with `at` / `over` / `by`, or a second one, is
/// an error.
fn main_beat_secs(script: &str) -> Result<f64, String> {
    let mut found: Option<(usize, f64)> = None;
    for (i, line) in script.lines().enumerate() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let ["clock", "main", rest @ ..] = words.as_slice() else {
            continue;
        };
        let at = i + 1;
        if let Some((first, _)) = found {
            return Err(format!(
                "{at}: scenario times assume a constant main tempo (`clock main` is already set on line {first})"
            ));
        }
        let [rate] = rest else {
            return Err(format!(
                "{at}: scenario times assume a constant main tempo; use one `clock main Nb|Nms|Nf` line"
            ));
        };
        let bad = || format!("{at}: `clock main {rate}` is not a constant Nb, Nms or Nf");
        let parsed = if let Some(d) = rate.strip_suffix("ms") {
            d.parse::<f64>().map(|v| v / 1000.0)
        } else if let Some(d) = rate.strip_suffix('f') {
            d.parse::<f64>().map(|v| v / f64::from(super::assets::RATE))
        } else if let Some(d) = rate.strip_suffix('b') {
            d.parse::<f64>().map(|v| 60.0 / v)
        } else {
            return Err(bad());
        };
        match parsed {
            Ok(v) if v.is_finite() && v > 0.0 => found = Some((at, v)),
            _ => return Err(bad()),
        }
    }
    Ok(found.map_or(0.5, |(_, v)| v))
}

/// Parses a scenario file; every problem is returned as `file:line: why`.
pub fn parse_scenario(file: &str, text: &str) -> Result<Scenario, Vec<String>> {
    let mut header = Header::default();
    let mut expects = Vec::new();
    let mut errors = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let Some(comment) = raw.trim().strip_prefix('#') else {
            continue;
        };
        let comment = comment.trim_start();
        if let Some(rest) = comment.strip_prefix("scn:") {
            if let Err(e) = parse_header_line(&mut header, rest) {
                errors.push(format!("{file}:{line}: {e}"));
            }
        } else if let Some(rest) = comment.strip_prefix("expect:") {
            match parse_check(rest.trim()) {
                Ok(check) => expects.push(Expect {
                    line,
                    text: rest.trim().to_string(),
                    check,
                }),
                Err(e) => errors.push(format!("{file}:{line}: {e}")),
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let beat_secs = match main_beat_secs(text) {
        Ok(v) => v,
        Err(e) => return Err(vec![format!("{file}:{e}")]),
    };
    Ok(Scenario {
        file: file.to_string(),
        name: file.strip_suffix(".blast").unwrap_or(file).to_string(),
        header,
        expects,
        beat_secs,
    })
}

// ---- checking ----

/// What a check can see besides the render itself.
pub struct Env<'a> {
    /// Rate and beat length.
    pub units: Units,
    /// Channels in the render (interleaved in `samples`).
    pub channels: usize,
    /// The interleaved render.
    pub samples: &'a [f32],
    /// Other scenarios' renders by name, for `equals NAME`.
    pub others: &'a HashMap<String, Vec<f32>>,
    /// `equals golden`: compares the render with the blessed hash.
    pub golden: &'a dyn Fn(&[f32]) -> Result<(), String>,
}

impl Env<'_> {
    /// The mix of all channels, or one channel (`Some(0)` left, `Some(1)`
    /// right); asking for one the render doesn't have is an error.
    fn mono(&self, channel: Option<usize>) -> Result<Vec<f32>, String> {
        let ch = self.channels.max(1);
        match channel {
            Some(c) if c >= ch => Err(format!(
                "the render has {ch} channel(s), no channel {} for `{}`",
                c + 1,
                if c == 0 { "left" } else { "right" }
            )),
            Some(c) => Ok(self.samples.chunks(ch).map(|f| f[c]).collect()),
            None => Ok(self
                .samples
                .chunks(ch)
                .map(|f| f.iter().sum::<f32>() / ch as f32)
                .collect()),
        }
    }

    /// Each channel on its own, for the checks that must not let one
    /// channel's problem average away.
    fn per_channel(&self) -> Vec<Vec<f32>> {
        let ch = self.channels.max(1);
        (0..ch)
            .map(|c| self.samples.chunks(ch).map(|f| f[c]).collect())
            .collect()
    }

    /// The window as frame indices, clamped to the render.
    fn span(&self, win: Option<Window>, len: usize) -> (usize, usize) {
        let Some((a, b)) = win else { return (0, len) };
        let clamp = |t| (self.units.frames(t).round().max(0.0) as usize).min(len);
        let (a, b) = (clamp(a), clamp(b));
        (a.min(b), b)
    }
}

fn cmp_ok(cmp: Cmp, got: f64, want: f64) -> bool {
    match cmp {
        Cmp::AtMost => got <= want,
        Cmp::AtLeast => got >= want,
    }
}

fn level_check(
    name: &str,
    measure: impl Fn(&[f32]) -> f64,
    env: &Env,
    win: Option<Window>,
    cmp: Cmp,
    want: f64,
) -> Result<(), String> {
    // the loudest channel: a level check on a mix would hide a hot side
    let got = env
        .per_channel()
        .iter()
        .map(|c| {
            let (a, b) = env.span(win, c.len());
            measure(&c[a..b])
        })
        .fold(measure::FLOOR_DB, f64::max);
    if cmp_ok(cmp, got, want) {
        Ok(())
    } else {
        Err(format!("{name} measured {got:.2} dBFS"))
    }
}

/// Runs one check against a render; `Err` says what was measured.
pub fn check(c: &Check, env: &Env) -> Result<(), String> {
    let u = env.units;
    match c {
        Check::Peak(win, cmp, want) => {
            level_check("peak", measure::peak_db, env, *win, *cmp, *want)
        }
        Check::Rms(win, cmp, want) => level_check("rms", measure::rms_db, env, *win, *cmp, *want),
        Check::NoClicks => {
            // a click in any channel counts
            let mut found: Vec<usize> = env
                .per_channel()
                .iter()
                .flat_map(|c| measure::clicks(c, u.rate))
                .collect();
            found.sort_unstable();
            found.dedup();
            if found.is_empty() {
                return Ok(());
            }
            let shown: Vec<String> = found.iter().take(4).map(|&f| u.show(f as f64)).collect();
            Err(format!(
                "{} click(s), first at {}{}",
                found.len(),
                shown.join(", "),
                if found.len() > 4 { ", …" } else { "" }
            ))
        }
        Check::SilentAfter(t) => {
            let chans = env.per_channel();
            let len = chans[0].len();
            let from = (u.frames(*t).round().max(0.0) as usize).min(len);
            let peak = chans
                .iter()
                .map(|c| measure::peak_db(&c[from..]))
                .fold(measure::FLOOR_DB, f64::max);
            if peak < -90.0 {
                return Ok(());
            }
            let loud = measure::lin(-90.0);
            let at = (from..len)
                .find(|&i| chans.iter().any(|c| f64::from(c[i].abs()) >= loud))
                .unwrap_or(from);
            Err(format!(
                "peak after is {peak:.1} dBFS, first at {}",
                u.show(at as f64)
            ))
        }
        Check::Pitch(win, want_hz, cents) => {
            let mono = env.mono(None)?;
            let (a, b) = env.span(*win, mono.len());
            let Some(got) = measure::pitch_hz(&mono[a..b], u.rate) else {
                return Err("no pitch found (silent or too short)".into());
            };
            let off = measure::cents(got, *want_hz);
            if off.abs() <= *cents {
                Ok(())
            } else {
                Err(format!(
                    "found {got:.2} Hz, {off:+.1} c from {want_hz:.2} Hz"
                ))
            }
        }
        Check::OnsetsAt(times, within) => {
            let found = measure::onsets(&env.mono(None)?, u.rate);
            let tol = u.frames(*within);
            let mut problems = Vec::new();
            for &t in times {
                let want = u.frames(t);
                let best = found
                    .iter()
                    .map(|&f| f as f64 - want)
                    .min_by(|a, b| a.abs().total_cmp(&b.abs()));
                match best {
                    Some(d) if d.abs() <= tol => {}
                    Some(d) => problems.push(format!("{} off by {d:+.0}f", u.show(want))),
                    None => problems.push(format!("no onset near {}", u.show(want))),
                }
            }
            for &f in &found {
                let near = times.iter().any(|&t| (f as f64 - u.frames(t)).abs() <= tol);
                if !near {
                    problems.push(format!("extra onset at {}", u.show(f as f64)));
                }
            }
            if problems.is_empty() {
                Ok(())
            } else {
                Err(problems.join("; "))
            }
        }
        Check::OnsetGrid {
            channel,
            every,
            window,
            within,
        } => {
            let mono = env.mono(*channel)?;
            let found = measure::onsets(&mono, u.rate);
            let step = u.frames(*every);
            let tol = u.frames(*within);
            if step < 1.0 {
                return Err("the grid step is under one frame".into());
            }
            let (a, b) = env.span(*window, mono.len());
            let mut problems = Vec::new();
            for &f in found.iter().filter(|&&f| f >= a && f < b) {
                let grid = (f as f64 / step).round() * step;
                let off = f as f64 - grid;
                if off.abs() > tol {
                    problems.push(format!(
                        "onset at {} is {} from the grid ({:+.0}f)",
                        u.show(f as f64),
                        u.show(grid),
                        off
                    ));
                }
            }
            let mut k = (a as f64 / step).ceil();
            while k * step < b as f64 {
                let g = k * step;
                if !found.iter().any(|&f| (f as f64 - g).abs() <= tol) {
                    problems.push(format!("no onset near {}", u.show(g)));
                }
                k += 1.0;
            }
            if problems.is_empty() {
                Ok(())
            } else {
                let total = problems.len();
                problems.truncate(4);
                Err(format!(
                    "{}{}",
                    problems.join("; "),
                    if total > 4 { "; …" } else { "" }
                ))
            }
        }
        Check::Equals(name, _) if name == "golden" => (env.golden)(env.samples),
        Check::Equals(name, tol) => {
            let Some(other) = env.others.get(name) else {
                return Err(format!("no scenario named `{name}` rendered"));
            };
            if other.len() != env.samples.len() {
                return Err(format!(
                    "lengths differ: {} vs {} samples",
                    env.samples.len(),
                    other.len()
                ));
            }
            let (at, worst) = env
                .samples
                .iter()
                .zip(other)
                .map(|(a, b)| (a - b).abs())
                .enumerate()
                .fold((0, 0.0f32), |m, (i, d)| if d > m.1 { (i, d) } else { m });
            let worst_db = measure::db(f64::from(worst));
            if worst_db <= *tol {
                Ok(())
            } else {
                let frame = at / env.channels.max(1);
                Err(format!(
                    "differs from `{name}` by {worst_db:.1} dBFS at {}",
                    u.show(frame as f64)
                ))
            }
        }
    }
}

/// Runs every expectation; returns `file:line: text: what was measured` for
/// each that fails.
pub fn check_all(sc: &Scenario, env: &Env) -> Vec<String> {
    sc.expects
        .iter()
        .filter_map(|e| {
            check(&e.check, env)
                .err()
                .map(|why| format!("{}:{}: {}: {why}", sc.file, e.line, e.text))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn units() -> Units {
        Units {
            rate: 48_000,
            beat_secs: 0.5,
        }
    }

    fn run(text: &str, samples: &[f32]) -> Result<(), String> {
        let sc = parse_scenario("t.blast", &format!("# expect: {text}\n")).unwrap();
        let others = HashMap::new();
        let env = Env {
            units: units(),
            channels: 1,
            samples,
            others: &others,
            golden: &|_| Err("no golden".into()),
        };
        check(&sc.expects[0].check, &env)
    }

    #[test]
    fn times_notes_and_levels_parse() {
        assert_eq!(parse_time("1.5b"), Ok(Time::Beats(1.5)));
        assert_eq!(parse_time("250ms"), Ok(Time::Secs(0.25)));
        assert_eq!(parse_time("2s"), Ok(Time::Secs(2.0)));
        assert_eq!(parse_time("64f"), Ok(Time::Frames(64.0)));
        assert!(parse_time("5").is_err());
        assert!((parse_freq("a4").unwrap() - 440.0).abs() < 1e-9);
        assert!((parse_freq("a5").unwrap() - 880.0).abs() < 1e-9);
        assert!((parse_freq("c4").unwrap() - 261.6256).abs() < 1e-3);
        assert!((parse_freq("eb2").unwrap() - parse_freq("d#2").unwrap()).abs() < 1e-9);
        assert_eq!(parse_freq("880hz"), Ok(880.0));
        assert_eq!(parse_db("-1dbfs"), Ok(-1.0));
        assert!(parse_freq("h4").is_err());
    }

    #[test]
    fn a_scenario_file_parses() {
        let text = "# scn: length 8b, seed 3, assets click noise\n\
                    # expect: onsets on main every 1b within 64f\n\
                    # expect: no clicks\n\
                    # expect: silent after 7b\n\
                    clock main 60b\n";
        let sc = parse_scenario("scn-x.blast", text).unwrap();
        assert_eq!(sc.name, "scn-x");
        assert_eq!(sc.header.length, Some(Time::Beats(8.0)));
        assert_eq!(sc.header.seed, 3);
        assert_eq!(sc.header.assets, ["click", "noise"]);
        assert_eq!(sc.expects.len(), 3);
        assert_eq!(sc.expects[1].line, 3);
        assert_eq!(sc.beat_secs, 1.0);
    }

    #[test]
    fn mistakes_name_their_line() {
        let text = "# scn: lenth 8b\n# expect: peak 3\n# expect: bogus\n# expect: no clicks\n";
        let errs = parse_scenario("bad.blast", text).unwrap_err();
        assert_eq!(errs.len(), 3, "{errs:?}");
        assert!(errs[0].starts_with("bad.blast:1:"));
        assert!(errs[1].starts_with("bad.blast:2:"));
        assert!(errs[2].starts_with("bad.blast:3:"));
        // no expectations is fine here: `tools/ab.sh` renders without any
        let bare = parse_scenario("bare.blast", "voice a from \"b\"\n").unwrap();
        assert!(bare.expects.is_empty());
    }

    #[test]
    fn level_checks() {
        let mut x = vec![0.0f32; 48_000];
        for (i, v) in x.iter_mut().enumerate().take(24_000) {
            *v = 0.5 * (TAU * 440.0 * i as f32 / 48_000.0).sin();
        }
        assert!(run("peak <= -5dbfs", &x).is_ok());
        assert!(run("peak >= -7dbfs", &x).is_ok());
        let why = run("peak <= -7dbfs", &x).unwrap_err();
        assert!(why.contains("-6.0"), "{why}");
        assert!(run("rms 0s..0.5s >= -10dbfs", &x).is_ok());
        assert!(run("rms 0.5s..1s <= -90dbfs", &x).is_ok());
        assert!(run("silent after 0.6s", &x).is_ok());
        assert!(run("silent after 0.4s", &x).is_err());
    }

    #[test]
    fn onset_checks_on_a_grid() {
        let mut x = vec![0.0f32; 96_000];
        for f in [0usize, 24_000, 48_000, 72_000] {
            x[f] = 0.9;
        }
        assert!(run("onsets on main every 1b within 10f", &x).is_ok());
        assert!(run("onsets at 0b 1b 2b 3b within 10f", &x).is_ok());
        // every half beat: the halves are missing
        let why = run("onsets on main every 0.5b within 10f", &x).unwrap_err();
        assert!(why.contains("no onset near 0.500b"), "{why}");
        // a hit pushed late by 100 frames
        x[48_000] = 0.0;
        x[48_100] = 0.9;
        let why = run("onsets on main every 1b within 64f", &x).unwrap_err();
        assert!(why.contains("+100f"), "{why}");
        assert!(run("onsets on main every 1b within 128f", &x).is_ok());
        assert!(run("onsets at 0b 1b 2b within 128f", &x).is_err());
    }

    #[test]
    fn pitch_and_clicks() {
        let x: Vec<f32> = (0..48_000)
            .map(|i| 0.5 * (TAU * 880.0 * i as f32 / 48_000.0).sin())
            .collect();
        assert!(run("pitch 0b..1b 880hz within 1c", &x).is_ok());
        assert!(run("pitch a5 within 1c", &x).is_ok());
        let why = run("pitch a4 within 5c", &x).unwrap_err();
        assert!(why.contains("+1200"), "{why}");
        assert!(run("no clicks", &x).is_ok());
        let mut y = x.clone();
        y[20_000] += 0.4;
        assert!(run("no clicks", &y).unwrap_err().contains("click"));
    }

    #[test]
    fn equals_compares_renders() {
        let a = vec![0.1f32, 0.2, 0.3];
        let mut others = HashMap::new();
        others.insert("other".to_string(), vec![0.1f32, 0.2, 0.3001]);
        let sc = parse_scenario(
            "t.blast",
            "# expect: equals other within -70dbfs\n# expect: equals other within -90dbfs\n",
        )
        .unwrap();
        let env = Env {
            units: units(),
            channels: 1,
            samples: &a,
            others: &others,
            golden: &|_| Ok(()),
        };
        assert!(check(&sc.expects[0].check, &env).is_ok());
        assert!(check(&sc.expects[1].check, &env).is_err());
        assert_eq!(check_all(&sc, &env).len(), 1);
    }

    #[test]
    fn main_tempo_must_be_a_constant() {
        let ok =
            |script: &str| parse_scenario("t.blast", &format!("# expect: no clicks\n{script}"));
        assert_eq!(ok("clock main 60b\n").unwrap().beat_secs, 1.0);
        assert_eq!(ok("clock main 250ms\n").unwrap().beat_secs, 0.25);
        assert_eq!(ok("clock main 24000f\n").unwrap().beat_secs, 0.5);
        assert_eq!(ok("voice a from \"b\"\n").unwrap().beat_secs, 0.5);
        for bad in [
            "clock main $t\n",
            "clock main 90b over 8\n",
            "clock main 90b at 4\n",
            "clock main 90b\nclock main 100b\n",
            "clock main fast\n",
        ] {
            let errs = ok(bad).unwrap_err();
            assert!(errs[0].starts_with("t.blast:"), "{bad}: {errs:?}");
        }
    }

    #[test]
    fn labels_and_channels() {
        let parse = |t: &str| parse_scenario("t.blast", &format!("# expect: {t}\n"));
        assert!(parse("onsets kick on main every 1b within 1f").is_err());
        assert!(parse("onsets left on main every 1b within 1f").is_ok());
        assert!(parse("onsets on main every 1b within 1f").is_ok());
        // `right` on a mono render is an error, not channel 0
        let x = vec![0.0f32; 48_000];
        let why = run("onsets right on main every 1b within 1f", &x).unwrap_err();
        assert!(why.contains("channel"), "{why}");
    }

    #[test]
    fn levels_and_clicks_look_at_every_channel() {
        // left silent, right loud with a click: a mix would halve both
        let mut samples = Vec::new();
        for i in 0..48_000usize {
            let v = 0.5 * (TAU * 440.0 * i as f32 / 48_000.0).sin();
            samples.push(0.0);
            samples.push(if i == 20_000 { v + 0.4 } else { v });
        }
        let others = HashMap::new();
        let env = Env {
            units: units(),
            channels: 2,
            samples: &samples,
            others: &others,
            golden: &|_| Ok(()),
        };
        let go = |t: &str| {
            let sc = parse_scenario("t.blast", &format!("# expect: {t}\n")).unwrap();
            check(&sc.expects[0].check, &env)
        };
        assert!(go("peak >= -3dbfs").is_ok());
        assert!(go("no clicks").is_err());
        assert!(go("silent after 0.5s").is_err());
        assert!(go("rms >= -10dbfs").is_ok());
    }
}
