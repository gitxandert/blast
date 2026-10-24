//! `seq`: retrigger a voice on steps.

use crate::audio_processing::blast_rand::{X128P, fast_seed};
use crate::audio_processing::blast_time::blast_time::{TempoMode, TempoUnit};
use crate::audio_processing::commands::errors::{CmdErr, CmdResult};
use crate::audio_processing::commands::process::ProcRepr;
use crate::audio_processing::commands::reprs::Idx;
use crate::audio_processing::commands::tempo::{
    TempoRepr, TempoSpec, has_two_parts, parse_tempo_spec,
};
use crate::audio_processing::commands::voice::VoiceRepr;
use crate::audio_processing::commands::{CmdProcessor, Command, Parsed};

/// One row per command: (name, usage and summary).
pub const HELP: &[(&str, &str)] = &[(
    "seq",
    "seq <voice> -s <steps> [-n name] [-p period] [-t s|m|b:<n> | v | c:<ctx> | g:<group>] [-c chance] [-j jitter]: retrigger a voice on steps",
)];

/// Arguments for `Command::Seq`.
pub struct SeqArgs {
    pub idx: Idx,
    pub tempo: TempoRepr,
    pub period: usize,
    pub steps: Vec<f32>,
    pub chance: Vec<f32>,
    /// Four values per step, at `jit[step * 4..]`:
    /// `[early_min, early_max, late_min, late_max]` (not applied by the
    /// engine yet).
    pub jit: Vec<f32>,
    pub rng: X128P,
}

const STEPS_FIRST: &str = "Must provide arguments to -s/--steps before -c/--chance or -j/--jitter";

fn number(s: &str, cmd: &str) -> CmdResult<f32> {
    s.parse().map_err(|_| CmdErr::invalid(s, cmd))
}

fn parse_steps(arg: &str) -> CmdResult<Vec<f32>> {
    arg.split(',').map(|step| number(step, "seq -s")).collect()
}

// the first of `_`, `:` and `-` in a value
fn special_char(s: &str) -> Option<char> {
    s.chars().find(|c| matches!(c, '_' | ':' | '-'))
}

/// `-c`: a value sets the chance of the step at its position (only the
/// first `min(values, steps)` values count, even indexed ones); `_` is 100;
/// `n:val` sets step n; `a:val` sets every step; `n1-n2:val` sets the steps
/// in a range.
fn apply_chance(arg: &str, steps: &[f32], chance: &mut [f32]) -> CmdResult<()> {
    let values: Vec<&str> = arg.split(',').collect();
    let len = values.len().min(chance.len());

    for (i, value) in values.iter().take(len).enumerate() {
        match special_char(value) {
            Some('_') => chance[i] = 100.0,
            Some(':') => {
                let Some((index, val)) = value.split_once(':').filter(|(_, v)| !v.contains(':'))
                else {
                    return Err(CmdErr::formatting(
                        "Indexed chance arguments must be formatted beat:chance",
                    ));
                };
                // the chance first, in case the index is 'a'
                let val = number(val, "seq -c")?;
                if index == "a" {
                    chance.fill(val);
                    continue;
                }
                let index = number(index, "seq -c")?;
                let Some(at) = steps.iter().position(|&step| step == index) else {
                    return Err(CmdErr::formatting("Invalid index for seq -c"));
                };
                chance[at] = val;
            }
            Some(_) => {
                let Some((range, val)) = value.split_once(':').filter(|(_, v)| !v.contains(':'))
                else {
                    return Err(CmdErr::formatting(
                        "Ranged chance arguments must be formatted range:chance",
                    ));
                };
                let val = number(val, "seq -c")?;
                let Some((lower, upper)) = range.split_once('-').filter(|(_, u)| !u.contains('-'))
                else {
                    return Err(CmdErr::formatting("Ranges must be formatted lower-upper"));
                };
                let (a, b) = (number(lower, "seq -c")?, number(upper, "seq -c")?);
                let (lower, upper) = if a > b { (b, a) } else { (a, b) };

                // steps can be in any order, so check them all
                let mut found = false;
                for (at, &step) in steps.iter().enumerate() {
                    if step >= lower && step <= upper {
                        chance[at] = val;
                        found = true;
                    }
                }
                if !found {
                    return Err(CmdErr::formatting("seq -c range applies to nothing"));
                }
            }
            // no special chars: the value for this position
            None => chance[i] = number(value, "seq -c")?,
        }
    }
    Ok(())
}

/// One jitter value: `_` (none), `v` (shorthand for `v|v`), `e|l` (before
/// and after the beat, as ranges 0-e and 0-l), or `e1-e2|l1-l2`.
fn parse_jitter_value(spec: &str) -> CmdResult<[f32; 4]> {
    if spec == "_" {
        return Ok([0f32; 4]);
    }

    let sides: Vec<&str> = spec.split('|').collect();
    let (early, late) = match sides[..] {
        [both] => (both, both),
        [early, late] => (early, late),
        _ => return Err(CmdErr::formatting("Jitter must be formatted early|late")),
    };

    let mut out = [0f32; 4];
    for (i, side) in [early, late].iter().enumerate() {
        let vals = side
            .split('-')
            .map(|b| {
                b.parse::<f32>()
                    .map_err(|_| CmdErr::invalid(spec, "seq -j"))
            })
            .collect::<CmdResult<Vec<f32>>>()?;
        let (min, max) = match vals[..] {
            [e] => (0f32, e),
            [a, b] => (a.min(b), a.max(b)),
            _ => {
                return Err(CmdErr::formatting(
                    "Jitter ranges must be formatted lower-upper",
                ));
            }
        };
        out[i * 2] = min;
        out[i * 2 + 1] = max;
    }

    Ok(out)
}

/// `-j`: like `-c`, with jitter values; `n:j`, `a:j` and `n1-n2:j` address
/// steps, and extra positional values are ignored.
fn apply_jitter(arg: &str, steps: &[f32], jit: &mut [f32]) -> CmdResult<()> {
    for (i, value) in arg.split(',').enumerate() {
        let parts: Vec<&str> = value.split(':').collect();
        let (index, vals) = match parts[..] {
            [only] => {
                let vals = parse_jitter_value(only)?;
                if let Some(slot) = jit.get_mut(i * 4..i * 4 + 4) {
                    slot.copy_from_slice(&vals);
                }
                continue;
            }
            [index, vals] => (index, parse_jitter_value(vals)?),
            _ => {
                return Err(CmdErr::formatting(
                    "Indexed jitter arguments must be formatted beat:jitter",
                ));
            }
        };

        // a: all steps; n1-n2: the steps in the range; n: the step equal to n
        let (lower, upper) = if index == "a" {
            (f32::NEG_INFINITY, f32::INFINITY)
        } else if index.contains('-') {
            let Some((a, b)) = index.split_once('-').filter(|(_, b)| !b.contains('-')) else {
                return Err(CmdErr::formatting("Ranges must be formatted lower-upper"));
            };
            let (a, b) = (number(a, "seq -j")?, number(b, "seq -j")?);
            (a.min(b), a.max(b))
        } else {
            let at = number(index, "seq -j")?;
            (at, at)
        };

        let mut found = false;
        for (at, &step) in steps.iter().enumerate() {
            if step >= lower && step <= upper {
                jit[at * 4..at * 4 + 4].copy_from_slice(&vals);
                found = true;
            }
        }
        if !found {
            return Err(CmdErr::formatting("seq -j index applies to nothing"));
        }
    }
    Ok(())
}

// the first free key of seq, seq1, seq2...
fn default_proc_name(voice: &VoiceRepr) -> String {
    let mut name = "seq".to_string();
    let mut i = 1;
    while voice.processes.contains_key(&name) {
        name = format!("seq{i}");
        i += 1;
    }
    name
}

impl CmdProcessor {
    // TODO: make able to apply to Group
    // TODO: implement naming Processes
    //       and replace insert("seq".to_string(), ...) with
    //       insert(name, ...)
    pub(in crate::audio_processing::commands) fn try_seq(
        &mut self,
        args: &str,
    ) -> CmdResult<Parsed> {
        let mut args = args.split_whitespace();
        let name = args.next().ok_or_else(|| CmdErr::missing("name", "seq"))?;

        // without -t, the seq owns a Process TempoState of 120 BPM (one step
        // per beat)
        let mut tempo: TempoRepr = {
            // TODO: find object? needs to be more generic
            let voice = self.engine_state.voice(name)?;
            TempoRepr::new(voice.proc_tempi.len())
        };
        tempo.init(TempoMode::Process, TempoUnit::Bpm, 120f32);
        let mut period: usize = 4;
        let mut steps: Vec<f32> = Vec::new();
        let mut chance: Vec<f32> = Vec::new();
        let mut jit: Vec<f32> = Vec::new();
        // implement user-defined seed l8r
        let rng = X128P::new(fast_seed());
        // key in the Voice's processes
        let mut p_name: Option<String> = None;

        while let Some(arg) = args.next() {
            match arg {
                "-n" | "--name" => {
                    let n = args
                        .next()
                        .ok_or_else(|| CmdErr::missing("name", "seq -n"))?;
                    p_name = Some(n.to_string());
                }
                "-t" | "--tempo" => {
                    let spec = args
                        .next()
                        .ok_or_else(|| CmdErr::missing("unit:interval", "seq -t"))?;
                    if spec != "v" && !has_two_parts(spec) {
                        return Err(CmdErr::TempoFormatting);
                    }
                    match parse_tempo_spec(spec, "seq")? {
                        TempoSpec::Voice => {
                            let voice = self.engine_state.voice(name)?;
                            tempo = TempoRepr::following(&voice.tempo, voice.idx);
                        }
                        TempoSpec::Context(tc) => {
                            tempo = TempoRepr::shared(self.engine_state.context(tc)?);
                        }
                        TempoSpec::Group(g) => {
                            let g = self.engine_state.group(g)?;
                            tempo = TempoRepr::following(&g.tempo, g.idx);
                        }
                        TempoSpec::New { unit, interval } => {
                            tempo.init(TempoMode::Process, unit, interval);
                        }
                    }
                }
                "-p" | "--period" => {
                    let raw = args
                        .next()
                        .ok_or_else(|| CmdErr::missing("value", "seq -p"))?;
                    period = raw.parse().map_err(|_| CmdErr::invalid(raw, "seq -p"))?;
                    // a period of 0 has no room for steps
                    if period == 0 {
                        return Err(CmdErr::invalid("0", "seq -p"));
                    }
                }
                "-s" | "--steps" => {
                    let arg = args
                        .next()
                        .ok_or_else(|| CmdErr::missing("value", "seq -s"))?;
                    // a repeated -s replaces the steps (and the chance and
                    // jitter sized to them) rather than appending
                    steps = parse_steps(arg)?;
                    chance = vec![100f32; steps.len()];
                    jit = vec![0f32; steps.len() * 4];
                }
                "-c" | "--chance" => {
                    if steps.is_empty() {
                        return Err(CmdErr::formatting(STEPS_FIRST));
                    }
                    let arg = args
                        .next()
                        .ok_or_else(|| CmdErr::missing("value", "seq -c"))?;
                    apply_chance(arg, &steps, &mut chance)?;
                }
                "-j" | "--jitter" => {
                    if steps.is_empty() {
                        return Err(CmdErr::formatting(STEPS_FIRST));
                    }
                    let arg = args
                        .next()
                        .ok_or_else(|| CmdErr::missing("value", "seq -j"))?;
                    apply_jitter(arg, &steps, &mut jit)?;
                }
                _ => return Err(CmdErr::invalid(arg, "seq")),
            }
        }

        // a seq must have steps (no default)
        if steps.is_empty() {
            return Err(CmdErr::missing("-s/--steps", "seq"));
        }

        // TODO: allow for Idx::Group
        let owner = self.engine_state.voice_idx(name)?;
        let voice = self.engine_state.voice_mut(name)?;

        // each Process needs its own key, so that processes.len() (and so
        // ProcRepr.idx) matches the engine's Vec<Process>
        let p_name = match p_name {
            Some(n) if voice.processes.contains_key(&n) => {
                return Err(CmdErr::AlreadyIs {
                    ty: "Process".to_string(),
                    name: n,
                });
            }
            Some(n) => n,
            None => default_proc_name(voice),
        };

        let repr = ProcRepr::new(voice.processes.len(), owner, Some(tempo.clone()));
        voice.processes.insert(p_name, repr);
        // proc_tempi only holds tempos the Process owns
        if tempo.mode == TempoMode::Process {
            voice
                .proc_tempi
                .insert(voice.proc_tempi.len(), tempo.clone());
        }

        Ok(Parsed::Engine(Command::Seq(SeqArgs {
            idx: owner,
            tempo,
            period,
            steps,
            chance,
            jit,
            rng,
        })))
    }
}

#[cfg(test)]
mod tests {
    use crate::audio_processing::blast_time::blast_time::TempoMode;
    use crate::audio_processing::commands::testing::*;
    use crate::audio_processing::commands::*;

    #[test]
    fn seq_voice_tempo_points_at_its_voice() {
        let mut cp = processor(&["kick", "snare"]);
        ok(&mut cp, "load kick");
        ok(&mut cp, "load snare -t b:120");
        let t = seq_tempo(ok(&mut cp, "seq snare -t v -s 0"));
        assert!(!t.owned);
        assert_eq!(t.mode, TempoMode::Voice);
        assert_eq!(t.idx, v_idx(&cp, "snare"));
        assert_eq!(t.idx, 1);
    }

    #[test]
    fn seq_voice_tempo_follows_index_shift() {
        let mut cp = processor(&["kick", "snare"]);
        ok(&mut cp, "load kick");
        ok(&mut cp, "load snare -t b:120");
        ok(&mut cp, "unload kick");
        let t = seq_tempo(ok(&mut cp, "seq snare -t v -s 0"));
        assert_eq!(t.idx, 0);
    }

    #[test]
    fn seq_tbd_voice_tempo_points_at_its_voice() {
        let mut cp = processor(&["kick", "snare"]);
        ok(&mut cp, "load kick");
        ok(&mut cp, "load snare");
        let t = seq_tempo(ok(&mut cp, "seq snare -t v -s 0"));
        assert!(!t.owned);
        assert_eq!(t.mode, TempoMode::TBD);
        assert_eq!(t.idx, 1);
    }

    #[test]
    fn seq_context_tempo_points_at_context() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "tc c1 b:120");
        ok(&mut cp, "tc c2 b:90");
        ok(&mut cp, "load a");
        let t = seq_tempo(ok(&mut cp, "seq a -t c:c2 -s 0"));
        assert!(!t.owned);
        assert_eq!(t.mode, TempoMode::Context);
        assert_eq!(t.idx, 1);
    }

    #[test]
    fn group_tempo_refs_point_at_their_group() {
        let mut cp = processor(&["a", "b", "c", "d"]);
        ok(&mut cp, "load a");
        ok(&mut cp, "load b");
        ok(&mut cp, "load c");
        ok(&mut cp, "group g1 -v a");
        ok(&mut cp, "group g2 -v b");

        let t = seq_tempo(ok(&mut cp, "seq c -t g:g2 -s 0"));
        assert!(!t.owned);
        assert_eq!(t.mode, TempoMode::Group);
        assert_eq!(t.idx, 1);

        let t = load_tempo(ok(&mut cp, "load d -t g:g1"));
        assert!(!t.owned);
        assert_eq!(t.mode, TempoMode::Group);
        assert_eq!(t.idx, 0);
    }

    #[test]
    fn seq_targets_group_voice() {
        let mut cp = grouped(&["a", "b"], "group g -v a,b");
        match ok(&mut cp, "seq g.b -s 0") {
            Command::Seq(args) => assert_eq!(args.idx, Idx::GroupVoice(0, 1)),
            _ => panic!("not a Seq"),
        }
        let gv = cp
            .engine_state
            .groups
            .get("g")
            .unwrap()
            .voices
            .get("b")
            .unwrap();
        assert_eq!(gv.processes.len(), 1);
    }
}

#[cfg(test)]
mod seq_tests {
    use super::*;
    use crate::audio_processing::commands::{CmdErr, CmdResult, Command, EngineState, Parsed};
    use crate::file_parsing::decode_helpers::AudioFile;

    fn processor() -> CmdProcessor {
        let kick = AudioFile::new("kick", "wav", 48000, 1, 16, vec![0.0; 16]);
        let mut cp = CmdProcessor::new(EngineState::new(vec![kick], 2));
        assert!(cp.parse("load kick").is_ok());
        cp
    }

    fn seq(cp: &mut CmdProcessor, cmd: &str) -> CmdResult<SeqArgs> {
        match cp.parse(cmd)? {
            Parsed::Engine(Command::Seq(args)) => Ok(args),
            _ => panic!("'{}' didn't parse to Command::Seq", cmd),
        }
    }

    fn seq_ok(cmd: &str) -> SeqArgs {
        let mut cp = processor();
        match seq(&mut cp, cmd) {
            Ok(args) => args,
            Err(e) => panic!("'{}' failed: {}", cmd, e),
        }
    }

    fn seq_err(cmd: &str) -> CmdErr {
        let mut cp = processor();
        match seq(&mut cp, cmd) {
            Ok(_) => panic!("'{}' should have failed", cmd),
            Err(e) => e,
        }
    }

    #[test]
    fn seq_default_tempo_is_owned_process() {
        let args = seq_ok("seq kick -s 0,1");
        assert!(args.tempo.owned);
        assert_eq!(args.tempo.mode, TempoMode::Process);
        assert_eq!(args.tempo.unit, TempoUnit::Bpm);
        assert_eq!(args.tempo.interval, 120.0);
    }

    #[test]
    fn seq_tempo_flag() {
        let args = seq_ok("seq kick -t m:250 -s 0");
        assert!(args.tempo.owned);
        assert_eq!(args.tempo.mode, TempoMode::Process);
        assert_eq!(args.tempo.unit, TempoUnit::Millis);
        assert_eq!(args.tempo.interval, 250.0);

        let args = seq_ok("seq kick -t v -s 0");
        assert!(!args.tempo.owned);

        assert!(matches!(
            seq_err("seq kick -t x:1 -s 0"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -t b:fast -s 0"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -t b -s 0"),
            CmdErr::TempoFormatting
        ));
    }

    #[test]
    fn seq_period_and_steps() {
        let args = seq_ok("seq kick -p 8 -s 0,1.5,3");
        assert_eq!(args.period, 8);
        assert_eq!(args.steps, vec![0.0, 1.5, 3.0]);
        assert_eq!(args.chance.len(), 3);
        assert_eq!(args.jit.len(), 3 * 4);

        let args = seq_ok("seq kick -s 0");
        assert_eq!(args.period, 4);

        assert!(matches!(
            seq_err("seq kick -p x -s 0"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,x"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(seq_err("seq kick -s"), CmdErr::MissingArg { .. }));
    }

    #[test]
    fn seq_unknown_flag_and_voice() {
        assert!(matches!(
            seq_err("seq kick -s 0 -z"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(seq_err("seq snare -s 0"), CmdErr::NoVoice { .. }));
        assert!(matches!(seq_err("seq"), CmdErr::MissingArg { .. }));
    }

    #[test]
    fn seq_requires_steps() {
        assert!(matches!(
            seq_err("seq kick -t b:120"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(seq_err("seq kick"), CmdErr::MissingArg { .. }));

        // a rejected seq leaves the mirror untouched
        let mut cp = processor();
        assert!(seq(&mut cp, "seq kick -t b:120").is_err());
        let voice = cp.engine_state.voice("kick").unwrap();
        assert_eq!(voice.processes.len(), 0);
        assert_eq!(voice.proc_tempi.len(), 0);
    }

    #[test]
    fn seq_rejects_zero_period() {
        assert!(matches!(
            seq_err("seq kick -p 0 -s 0"),
            CmdErr::InvalidArg { .. }
        ));
        assert_eq!(seq_ok("seq kick -p 1 -s 0").period, 1);
    }

    #[test]
    fn seq_chance_at_index() {
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c 2:50").chance,
            vec![100.0, 100.0, 50.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c 1:50").chance,
            vec![100.0, 50.0, 100.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c 0:50").chance,
            vec![50.0, 100.0, 100.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1.5 -c 1.5:25").chance,
            vec![100.0, 25.0]
        );
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -c 5:50"),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -c x:50"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -c 1:x"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -c 1:2:3"),
            CmdErr::Formatting { .. }
        ));
    }

    #[test]
    fn seq_chance_all_positional_and_default() {
        assert_eq!(
            seq_ok("seq kick -s 0,1,2").chance,
            vec![100.0, 100.0, 100.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c a:30").chance,
            vec![30.0, 30.0, 30.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c 10,20").chance,
            vec![10.0, 20.0, 100.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c 10,_,30").chance,
            vec![10.0, 100.0, 30.0]
        );
        // extra values are ignored
        assert_eq!(
            seq_ok("seq kick -s 0,1 -c 10,20,30").chance,
            vec![10.0, 20.0]
        );
        assert!(matches!(
            seq_err("seq kick -s 0,1 -c x"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1 -c"),
            CmdErr::MissingArg { .. }
        ));
        // -c needs steps first
        assert!(matches!(
            seq_err("seq kick -c 50 -s 0,1"),
            CmdErr::Formatting { .. }
        ));
    }

    #[test]
    fn seq_chance_range() {
        let s = "seq kick -s 0,1,2,3 -c";
        assert_eq!(
            seq_ok(&format!("{} 1-3:20", s)).chance,
            vec![100.0, 20.0, 20.0, 20.0]
        );
        assert_eq!(
            seq_ok(&format!("{} 3-1:20", s)).chance,
            vec![100.0, 20.0, 20.0, 20.0]
        );
        assert_eq!(
            seq_ok(&format!("{} 0-1:20", s)).chance,
            vec![20.0, 20.0, 100.0, 100.0]
        );
        assert_eq!(
            seq_ok(&format!("{} 2-9:20", s)).chance,
            vec![100.0, 100.0, 20.0, 20.0]
        );
        assert_eq!(
            seq_ok(&format!("{} 1-1:20", s)).chance,
            vec![100.0, 20.0, 100.0, 100.0]
        );
        assert!(matches!(
            seq_err(&format!("{} 5-9:20", s)),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err(&format!("{} 1-x:20", s)),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err(&format!("{} 1-3", s)),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err(&format!("{} 1-2-3:20", s)),
            CmdErr::Formatting { .. }
        ));
    }

    #[test]
    fn seq_jitter_default_and_none() {
        assert_eq!(seq_ok("seq kick -s 0,1").jit, vec![0.0; 8]);
        assert_eq!(seq_ok("seq kick -s 0,1 -j _").jit, vec![0.0; 8]);
        assert_eq!(seq_ok("seq kick -s 0,1 -j 5|5 -j _,_").jit, vec![0.0; 8]);
    }

    #[test]
    fn seq_jitter_values() {
        // jit[step * 4..] = [early_min, early_max, late_min, late_max]
        assert_eq!(
            seq_ok("seq kick -s 0,1 -j 5|10").jit,
            vec![0.0, 5.0, 0.0, 10.0, 0.0, 0.0, 0.0, 0.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1 -j 3,2-4|8-6").jit,
            vec![0.0, 3.0, 0.0, 3.0, 2.0, 4.0, 6.0, 8.0]
        );
        // extra values are ignored
        assert_eq!(seq_ok("seq kick -s 0 -j 1,2").jit, vec![0.0, 1.0, 0.0, 1.0]);
        // -j consumes its value, so later flags still parse
        let args = seq_ok("seq kick -s 0,1 -j 5|5 -c 1:50");
        assert_eq!(args.chance, vec![100.0, 50.0]);
    }

    #[test]
    fn seq_jitter_indexed() {
        let j = |s: &str| seq_ok(&format!("seq kick -s 0,1,2 -j {}", s)).jit;
        let none = [0.0f32; 4];
        let v = [0.0f32, 5.0, 0.0, 10.0];
        let cat = |a: [[f32; 4]; 3]| a.concat();

        assert_eq!(j("1:5|10"), cat([none, v, none]));
        assert_eq!(j("a:5|10"), cat([v, v, v]));
        assert_eq!(j("1-2:5|10"), cat([none, v, v]));
        assert_eq!(j("2-1:5|10"), cat([none, v, v]));
        assert_eq!(j("0:5|10,2:5|10"), cat([v, none, v]));

        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -j 7:5|10"),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -j 5-9:5|10"),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -j x:5|10"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -j 1:2:3"),
            CmdErr::Formatting { .. }
        ));
    }

    #[test]
    fn seq_jitter_errors() {
        assert!(matches!(
            seq_err("seq kick -s 0,1 -j"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -j 5|5 -s 0,1"),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1 -j x"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1 -j 1|2|3"),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1 -j 1-2-3"),
            CmdErr::Formatting { .. }
        ));
    }

    fn proc_idx(cp: &mut CmdProcessor, p_name: &str) -> usize {
        let voice = cp.engine_state.voice("kick").unwrap();
        voice.processes.get(p_name).unwrap().idx
    }

    #[test]
    fn seq_default_names_are_unique() {
        let mut cp = processor();
        for _ in 0..3 {
            assert!(seq(&mut cp, "seq kick -s 0").is_ok());
        }
        assert_eq!(cp.engine_state.voice("kick").unwrap().processes.len(), 3);
        assert_eq!(proc_idx(&mut cp, "seq"), 0);
        assert_eq!(proc_idx(&mut cp, "seq1"), 1);
        assert_eq!(proc_idx(&mut cp, "seq2"), 2);
        // each owns a Process tempo
        assert_eq!(cp.engine_state.voice("kick").unwrap().proc_tempi.len(), 3);
    }

    #[test]
    fn seq_named() {
        let mut cp = processor();
        assert!(seq(&mut cp, "seq kick -n hats -s 0").is_ok());
        assert!(seq(&mut cp, "seq kick --name seq -s 0").is_ok());
        // default skips names already taken
        assert!(seq(&mut cp, "seq kick -s 0").is_ok());
        assert_eq!(proc_idx(&mut cp, "hats"), 0);
        assert_eq!(proc_idx(&mut cp, "seq"), 1);
        assert_eq!(proc_idx(&mut cp, "seq1"), 2);

        match seq(&mut cp, "seq kick -n hats -s 0") {
            Err(CmdErr::AlreadyIs { ty, name }) => {
                assert_eq!(ty, "Process");
                assert_eq!(name, "hats");
            }
            _ => panic!("duplicate seq name should fail"),
        }
        // the failed seq didn't touch the mirror
        assert_eq!(cp.engine_state.voice("kick").unwrap().processes.len(), 3);
        assert_eq!(cp.engine_state.voice("kick").unwrap().proc_tempi.len(), 3);

        assert!(matches!(
            seq_err("seq kick -s 0 -n"),
            CmdErr::MissingArg { .. }
        ));
    }

    #[test]
    fn repeated_steps_flag_replaces() {
        let args = seq_ok("seq kick -s 0,1,2 -c 50 -s 0,2");
        assert_eq!(args.steps, vec![0.0, 2.0]);
        assert_eq!(args.chance, vec![100.0, 100.0]);
        assert_eq!(args.jit.len(), 8);
    }

    #[test]
    fn chance_range_with_unsorted_steps() {
        let args = seq_ok("seq kick -s 3,0 -c 2-5:40");
        assert_eq!(args.chance, vec![40.0, 100.0]);
        assert!(matches!(
            seq_err("seq kick -s 3,0 -c 4-5:40"),
            CmdErr::Formatting { .. }
        ));
    }
}
