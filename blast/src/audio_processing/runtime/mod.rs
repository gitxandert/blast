use std::{
    fmt,
    process::ExitCode,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

pub mod agent;
pub mod audio;
pub mod history;
pub mod line_editor;
pub mod repl;
pub mod terminal;

use crate::audio_processing::{
    blast_time::sample_rate,
    commands::{CmdProcessor, EngineState, cmd_queue},
    engine::Conductor,
};
use crate::cli::Options;
use crate::file_parsing::{
    decode_helpers::DecodeError, load_assets, preferred_config, scan_assets,
};
use agent::Agent;
use audio::{AudioErr, StreamStatus};
use repl::{Quit, Repl};
use terminal::Term;

/// Why `run_blast` gave up.
#[derive(Debug)]
pub enum RunErr {
    /// The assets directory couldn't be scanned or loaded.
    Assets {
        dir: String,
        err: DecodeError,
    },
    /// Nothing in the assets directory can be played.
    NoAssets {
        dir: String,
    },
    Audio(AudioErr),
    /// The terminal couldn't enter raw mode (stdin isn't a terminal?).
    Terminal(std::io::Error),
    /// `--export` gave up.
    Export(crate::export::render::ExportErr),
}

impl fmt::Display for RunErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunErr::Assets { dir, err } => {
                write!(f, "could not load assets from '{dir}': {err}")
            }
            RunErr::NoAssets { dir } => write!(f, "No playable audio files in '{dir}'"),
            RunErr::Audio(e) => write!(f, "{e}"),
            RunErr::Terminal(e) => write!(f, "could not set up the terminal: {e}"),
            RunErr::Export(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RunErr {}

impl From<AudioErr> for RunErr {
    fn from(e: AudioErr) -> Self {
        RunErr::Audio(e)
    }
}

/// Set in the child that `--list-devices` runs to do the probing.
const PROBE_ENV: &str = "BLAST_LIST_DEVICES_CHILD";

/// `--list-devices`. Probing every device's configs makes JACK and ALSA
/// print to stderr ("jack server is not running", "Cannot open device
/// /dev/dsp"), and without `libc` stderr can't be redirected in-process, so
/// the probe runs in a child whose stderr is shown only if it fails.
fn list_devices_quietly() -> Result<ExitCode, RunErr> {
    let direct = || -> Result<ExitCode, RunErr> {
        print!("{}", audio::list_devices()?);
        Ok(ExitCode::SUCCESS)
    };
    if std::env::var_os(PROBE_ENV).is_some() {
        return direct();
    }
    let child = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .arg("--list-devices")
            .env(PROBE_ENV, "1")
            .output()
    });
    // no child: probe here, noise and all
    let Ok(out) = child else {
        return direct();
    };
    let mut stdout = std::io::stdout();
    let _ = std::io::Write::write_all(&mut stdout, &out.stdout);
    if out.status.success() {
        return Ok(ExitCode::SUCCESS);
    }
    let _ = std::io::Write::write_all(&mut std::io::stderr(), &out.stderr);
    Ok(ExitCode::FAILURE)
}

/// Runs blast; returns the process exit code.
pub fn run_blast(options: &Options) -> Result<ExitCode, RunErr> {
    if options.list_devices {
        return list_devices_quietly();
    }

    if options.card {
        print!("{}", crate::audio_processing::commands::lang::card());
        return Ok(ExitCode::SUCCESS);
    }

    // a headless render: no device, no prompt
    if let Some(script) = &options.export {
        let opts = crate::export::render::ExportOpts {
            assets_dir: options.assets_dir.clone(),
            sources_dir: options.sources_dir.clone(),
            script: script.clone(),
            out: options.export_out.clone(),
            length: options.length,
            bits: options.bits.unwrap_or(32),
            rate: options.rate,
            seed: options.seed.unwrap_or(0),
            progress: true,
        };
        crate::export::render::render_script(&opts).map_err(RunErr::Export)?;
        return Ok(ExitCode::SUCCESS);
    }
    let agent = options.agent;
    // in agent mode stdout is only JSON lines: the rest goes to stderr
    let say = |text: &str| {
        if agent {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    };

    // README §3.3: scan, choose a config, open the device, load at what the
    // device accepted
    let dir = options.assets_dir.display().to_string();
    let assets_err = |err| RunErr::Assets {
        dir: dir.clone(),
        err,
    };
    let assets = scan_assets(&options.assets_dir).map_err(assets_err)?;
    let Some((want_rate, want_channels)) = preferred_config(&assets) else {
        return Err(RunErr::NoAssets { dir });
    };
    let device = audio::open_device(options.device.as_deref())?;
    let out = audio::choose_config(&device, want_rate, want_channels)?;
    let (rate, num_channels) = (out.config.sample_rate, u32::from(out.config.channels));
    let files = load_assets(&assets, rate, num_channels).map_err(assets_err)?;

    say("Loaded tracks [");
    for file in &files {
        say(&format!("\t{}", file.file_name));
    }
    say("]");
    say(&format!(
        "Output: {} · {} Hz · {} ch · {:?} · buffer {}",
        out.device_name,
        rate,
        num_channels,
        out.format,
        out.buffer_text()
    ));

    // initialize audio engine and engine state
    sample_rate::set(rate);
    let mut engine_state = EngineState::new(files, num_channels as usize);
    // `rescan` and relative `track … from` paths start here
    engine_state.set_assets_dir(Some(options.assets_dir.clone()));
    // the engine shares the mirror's samples: a startup asset lives once
    let mut conductor =
        Conductor::prepare_shared(num_channels as usize, engine_state.shared_tracks());
    let engine_rx = conductor.take_rx().expect("prepare keeps the REPL ends");

    // the command queue between the REPL and the audio callback
    let (mut cmd_tx, cmd_rx) = cmd_queue(256);
    let status = Arc::new(StreamStatus::new());
    // the callback owns the Conductor and the command receiver from here on
    let stream = audio::start_stream(&device, &out, conductor, cmd_rx, status.clone())?;

    // take over the terminal (not in agent mode: stdin is a pipe); every
    // line from here on goes through `Term`
    let term = if agent {
        None
    } else {
        let term = Term::enter().map_err(RunErr::Terminal)?;
        term.edit(|_| ());

        // the marker's timer; `print_line` and `edit` redraw by themselves
        let ticker = term.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(100));
                ticker.tick();
            }
        });
        Some(term)
    };

    // initialize the command processor with engine state
    // (just tracks for now)
    let mut cmd_processor = CmdProcessor::new(engine_state);
    cmd_processor.set_sources_dir(options.sources_dir.clone());
    // the session's clock `main`, queued before anything the user types; the
    // queue is empty, so the push can't fail
    let pushed = cmd_tx.try_push(cmd_processor.boot());
    debug_assert!(pushed.is_ok(), "boot command refused by an empty queue");

    // `--script`: checked whole, then applied, before the first prompt; an
    // error leaves the session as it was, and the REPL starts anyway
    if let Some(path) = &options.script {
        let path = cmd_processor.resolve_script_arg(path);
        let result = cmd_processor.run_script_text(&path, &mut cmd_tx);
        match &term {
            None => {
                // there is no history in agent mode
                cmd_processor.history_out.clear();
                println!("{}", agent::script_event(result));
            }
            Some(term) => match result {
                Ok(text) if text.is_empty() => {}
                Ok(text) => term.print_line(&text),
                Err(e) => term.print_line(&format!("Err: {e}")),
            },
        }
    }

    // the front end tells the main thread why it ended
    let (quit_tx, quit_rx) = mpsc::channel::<Quit>();
    match &term {
        Some(term) => {
            let repl = Repl::new(
                cmd_processor,
                cmd_tx,
                engine_rx,
                term.clone(),
                status.clone(),
                quit_tx,
            );
            thread::spawn(move || repl.run());
        }
        None => {
            let agent = Agent::new(cmd_processor, cmd_tx, engine_rx, status.clone(), quit_tx);
            thread::spawn(move || agent.run());
        }
    }

    // a REPL thread that died without a word (a panic) counts as a failure
    let why = quit_rx.recv().ok();

    // stop the audio first, then give the terminal back
    drop(stream);
    if let Some(term) = &term {
        term.restore();
    }
    Ok(match why {
        Some(Quit::Command) => ExitCode::SUCCESS,
        Some(Quit::Interrupt) => {
            say("Interrupted.");
            ExitCode::from(130)
        }
        Some(Quit::StreamFailed) => {
            if let Some(kind) = status.last_error() {
                eprintln!("Error: audio stream failed: {kind}");
            }
            ExitCode::FAILURE
        }
        None => ExitCode::FAILURE,
    })
}
