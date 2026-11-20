//! Command-line parsing (README §3.6).

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use lexopt::ValueExt;
use lexopt::prelude::*;

/// What `blast --help` prints.
pub const USAGE: &str = "\
Usage: blast [OPTIONS] [ASSETS_DIR]
       blast server [COMMAND] [OPTIONS]   serve the web UI (blast server --help)

Arguments:
  ASSETS_DIR        folder of audio files to load (default: the crate's assets/)

Options:
  --device <NAME>   output device: an id or a name from --list-devices
  --list-devices    print the output devices and exit
  --script <NAME>   run a script before the prompt (checked whole first;
                    nothing applies if it has errors); a file in the
                    working directory, else a name in the sources folder
  --card            print the language card and exit
  --agent           no terminal UI: read one JSON request per line on stdin,
                    answer one JSON line on stdout (see the README)
  --sources <DIR>   folder of source files for `source` and `state to`
                    (default: the crate's sources/)
  --export <NAME> [OUT.wav]
                    render a script to a WAV file, faster than real time,
                    with no audio device and no prompt (--agent, --device,
                    --list-devices and --card don't combine with it). NAME
                    is found like --script. OUT defaults to the script's
                    name with .wav in the working directory; an existing
                    file is overwritten. It renders until the script ends
                    (the implicit or written `end`, the glides still running
                    then, and every waiting line) and refuses a script that
                    ends at once (E207); buffers with to= are written
      --length <T>  with --export: render exactly T and cut there:
                    64b (beats of main, at its tempo when the render
                    starts), 90s (seconds), or a bare number (beats)
      --bits <N>    with --export: 32 (float, the default), 24 or 16
                    (integer, dithered)
      --rate <HZ>   with --export: sample rate (default: the assets' rate)
      --seed <N>    seed for the random features (default 0 in an export,
                    so a render repeats exactly; a live session picks its
                    own and ignores it)
  -h, --help        print this help";

/// The parsed command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// Positional; defaults to `CARGO_MANIFEST_DIR/assets`.
    pub assets_dir: PathBuf,
    /// `--device <name>`.
    pub device: Option<String>,
    /// `--list-devices`.
    pub list_devices: bool,
    /// `--script <path>`: run before the prompt, checked whole first.
    pub script: Option<PathBuf>,
    /// `--sources <dir>`; defaults to `CARGO_MANIFEST_DIR/sources`.
    pub sources_dir: PathBuf,
    /// `--card`: print the language card and exit.
    pub card: bool,
    /// `--agent`: JSON lines on stdin and stdout instead of the terminal.
    pub agent: bool,
    /// `--export <script>`: render it to a WAV file and exit.
    pub export: Option<PathBuf>,
    /// The `.wav` after the script name; `None` means the script's name.
    pub export_out: Option<PathBuf>,
    /// `--length`.
    pub length: Option<Length>,
    /// `--bits`: 16, 24 or 32 (float); `None` is 32.
    pub bits: Option<u32>,
    /// `--rate`.
    pub rate: Option<u32>,
    /// `--seed`.
    pub seed: Option<u64>,
}

/// How long an export renders (`--length`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// Beats of `main`.
    Beats(f64),
    /// Seconds.
    Secs(f64),
}

impl Length {
    /// `64b`, `90s` or a bare number of beats.
    fn parse(text: &str) -> Option<Length> {
        let (num, secs) = match text.strip_suffix('s') {
            Some(n) => (n, true),
            None => (text.strip_suffix('b').unwrap_or(text), false),
        };
        let n: f64 = num
            .parse()
            .ok()
            .filter(|n: &f64| n.is_finite() && *n > 0.0)?;
        Some(if secs {
            Length::Secs(n)
        } else {
            Length::Beats(n)
        })
    }
}

fn custom(text: &str) -> CliErr {
    CliErr::Lexopt(lexopt::Error::Custom(text.into()))
}

/// Why `parse_args` didn't produce `Options`.
#[derive(Debug)]
pub enum CliErr {
    Lexopt(lexopt::Error),
    /// `-h` or `--help`: print `USAGE` and exit 0.
    Help,
}

impl fmt::Display for CliErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliErr::Lexopt(e) => write!(f, "{e}"),
            CliErr::Help => f.write_str(USAGE),
        }
    }
}

impl std::error::Error for CliErr {}

impl From<lexopt::Error> for CliErr {
    fn from(e: lexopt::Error) -> Self {
        CliErr::Lexopt(e)
    }
}

/// Parses the arguments after the program name.
pub fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> Result<Options, CliErr> {
    let mut assets_dir = None;
    let mut device = None;
    let mut list_devices = false;
    let mut script = None;
    let mut sources_dir = None;
    let mut card = false;
    let mut agent = false;
    let mut export = None;
    let mut export_out = None;
    let mut length = None;
    let mut bits = None;
    let mut rate = None;
    let mut seed = None;

    let mut parser = lexopt::Parser::from_args(args);
    while let Some(arg) = parser.next()? {
        match arg {
            Long("device") => device = Some(parser.value()?.string()?),
            Long("list-devices") => list_devices = true,
            Long("script") => script = Some(PathBuf::from(parser.value()?)),
            Long("sources") => sources_dir = Some(PathBuf::from(parser.value()?)),
            Long("card") => card = true,
            Long("agent") => agent = true,
            Long("export") => export = Some(PathBuf::from(parser.value()?)),
            Long("length") => {
                let v = parser.value()?.string()?;
                length = Some(Length::parse(&v).ok_or_else(|| {
                    custom(&format!("--length {v}: expected 64b, 90s or a number"))
                })?);
            }
            Long("bits") => {
                let v = parser.value()?.string()?;
                bits = match v.as_str() {
                    "16" => Some(16),
                    "24" => Some(24),
                    "32" => Some(32),
                    _ => return Err(custom(&format!("--bits {v}: expected 16, 24 or 32"))),
                };
            }
            Long("rate") => {
                let v = parser.value()?.string()?;
                rate = Some(
                    v.parse::<u32>()
                        .ok()
                        .filter(|r| *r > 0)
                        .ok_or_else(|| custom(&format!("--rate {v}: expected a rate in Hz")))?,
                );
            }
            Long("seed") => {
                let v = parser.value()?.string()?;
                seed = Some(
                    v.parse::<u64>()
                        .map_err(|_| custom(&format!("--seed {v}: expected a whole number")))?,
                );
            }
            Short('h') | Long("help") => return Err(CliErr::Help),
            // the file after `--export NAME`
            Value(out)
                if export.is_some()
                    && export_out.is_none()
                    && Path::new(&out)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("wav")) =>
            {
                export_out = Some(PathBuf::from(out));
            }
            Value(dir) if assets_dir.is_none() => assets_dir = Some(PathBuf::from(dir)),
            arg => return Err(arg.unexpected().into()),
        }
    }

    if export.is_some() {
        if agent || device.is_some() || list_devices || card {
            return Err(custom("--export renders without a device or a prompt"));
        }
    } else if length.is_some() || bits.is_some() || rate.is_some() {
        return Err(custom("--length, --bits and --rate need --export"));
    }

    Ok(Options {
        assets_dir: assets_dir
            .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets"))),
        device,
        list_devices,
        script,
        sources_dir: sources_dir
            .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/sources"))),
        card,
        agent,
        export,
        export_out,
        length,
        bits,
        rate,
        seed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, CliErr> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn cli_defaults() {
        let o = parse(&[]).unwrap();
        assert_eq!(
            o.assets_dir,
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets"))
        );
        assert_eq!(o.device, None);
        assert!(!o.list_devices);
        assert_eq!(o.script, None);
    }

    #[test]
    fn cli_sources_default_and_flag() {
        assert_eq!(
            parse(&[]).unwrap().sources_dir,
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/sources"))
        );
        let o = parse(&["--sources", "mine/", "assets"]).unwrap();
        assert_eq!(o.sources_dir, PathBuf::from("mine/"));
        assert_eq!(o.assets_dir, PathBuf::from("assets"));
    }

    #[test]
    fn cli_positional_dir() {
        assert_eq!(
            parse(&["samples/"]).unwrap().assets_dir,
            PathBuf::from("samples/")
        );
    }

    #[test]
    fn cli_device_and_script() {
        let o = parse(&["--device", "alsa:hw:0,0", "--script", "a.blast", "dir"]).unwrap();
        assert_eq!(o.device.as_deref(), Some("alsa:hw:0,0"));
        assert_eq!(o.script, Some(PathBuf::from("a.blast")));
        assert_eq!(o.assets_dir, PathBuf::from("dir"));
    }

    #[test]
    fn cli_card_and_agent_flags() {
        let o = parse(&[]).unwrap();
        assert!(!o.card && !o.agent);
        assert!(parse(&["--card"]).unwrap().card);
        let o = parse(&["--agent", "assets/"]).unwrap();
        assert!(o.agent && !o.card);
        assert_eq!(o.assets_dir, PathBuf::from("assets/"));
    }

    #[test]
    fn cli_list_devices() {
        assert!(parse(&["--list-devices"]).unwrap().list_devices);
    }

    #[test]
    fn cli_rejects_second_positional() {
        assert!(matches!(
            parse(&["a", "b"]),
            Err(CliErr::Lexopt(lexopt::Error::UnexpectedArgument(_)))
        ));
    }

    #[test]
    fn cli_rejects_unknown_flag() {
        assert!(matches!(
            parse(&["--nope"]),
            Err(CliErr::Lexopt(lexopt::Error::UnexpectedOption(_)))
        ));
        assert!(matches!(
            parse(&["--device"]),
            Err(CliErr::Lexopt(lexopt::Error::MissingValue { .. }))
        ));
    }

    #[test]
    fn cli_help() {
        assert!(matches!(parse(&["-h"]), Err(CliErr::Help)));
        assert!(matches!(parse(&["--help", "x"]), Err(CliErr::Help)));
    }

    #[test]
    fn cli_export_defaults() {
        let o = parse(&[]).unwrap();
        assert_eq!(o.export, None);
        assert_eq!(o.export_out, None);
        assert!(o.length.is_none() && o.bits.is_none() && o.rate.is_none() && o.seed.is_none());
    }

    #[test]
    fn cli_export_script_and_output() {
        let o = parse(&["--export", "finished.blast"]).unwrap();
        assert_eq!(o.export, Some(PathBuf::from("finished.blast")));
        assert_eq!(o.export_out, None);
        let o = parse(&["--export", "finished.blast", "out.wav"]).unwrap();
        assert_eq!(o.export_out, Some(PathBuf::from("out.wav")));
        assert_eq!(
            o.assets_dir,
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets"))
        );
        // the assets folder may come first or last
        let o = parse(&["samples/", "--export", "a.blast", "a.WAV"]).unwrap();
        assert_eq!(o.assets_dir, PathBuf::from("samples/"));
        assert_eq!(o.export_out, Some(PathBuf::from("a.WAV")));
        let o = parse(&["--export", "a.blast", "out.wav", "samples/"]).unwrap();
        assert_eq!(o.assets_dir, PathBuf::from("samples/"));
        let o = parse(&["--export", "a.blast", "samples/"]).unwrap();
        assert_eq!(o.assets_dir, PathBuf::from("samples/"));
        assert_eq!(o.export_out, None);
        assert!(matches!(
            parse(&["--export"]),
            Err(CliErr::Lexopt(lexopt::Error::MissingValue { .. }))
        ));
    }

    #[test]
    fn cli_export_length() {
        let len = |t: &str| parse(&["--export", "a.blast", "--length", t]).map(|o| o.length);
        assert_eq!(len("64b").unwrap(), Some(Length::Beats(64.0)));
        assert_eq!(len("8").unwrap(), Some(Length::Beats(8.0)));
        assert_eq!(len("1.5s").unwrap(), Some(Length::Secs(1.5)));
        for bad in ["0", "-2b", "b", "xs", "nan", "inf"] {
            assert!(len(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cli_export_bits_rate_seed() {
        let o = parse(&[
            "--export", "a.blast", "--bits", "24", "--rate", "44100", "--seed", "9",
        ])
        .unwrap();
        assert_eq!((o.bits, o.rate, o.seed), (Some(24), Some(44_100), Some(9)));
        assert_eq!(
            parse(&["--export", "a", "--bits", "32"]).unwrap().bits,
            Some(32)
        );
        assert!(parse(&["--export", "a", "--bits", "8"]).is_err());
        assert!(parse(&["--export", "a", "--rate", "0"]).is_err());
        assert!(parse(&["--export", "a", "--seed", "x"]).is_err());
        // the seed is harmless without --export; the rest isn't
        assert_eq!(parse(&["--seed", "3"]).unwrap().seed, Some(3));
        assert!(parse(&["--length", "4b"]).is_err());
        assert!(parse(&["--bits", "16"]).is_err());
        assert!(parse(&["--rate", "44100"]).is_err());
    }

    #[test]
    fn export_flags_conflict() {
        for other in [
            &["--agent"][..],
            &["--device", "x"],
            &["--list-devices"],
            &["--card"],
        ] {
            let mut args = vec!["--export", "a.blast"];
            args.extend_from_slice(other);
            let e = parse(&args).unwrap_err().to_string();
            assert!(e.contains("renders without a device or a prompt"), "{e}");
        }
    }
}
