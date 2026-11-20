#![forbid(unsafe_code)]
use blast::{
    audio_processing::runtime::run_blast,
    cli::{CliErr, USAGE, parse_args},
};
use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args_os().nth(1).is_some_and(|a| a == "server") {
        return server();
    }
    let options = match parse_args(std::env::args_os().skip(1)) {
        Ok(options) => options,
        Err(CliErr::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("Error: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    match run_blast(&options) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `blast server ARGS`: runs `blast-server ARGS`, the web UI's server. It is
/// its own binary (the `blast_server` crate) so the web stack stays out of
/// blast's build; it is looked for next to this binary, then on `PATH`.
fn server() -> ExitCode {
    use std::os::unix::process::CommandExt;
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("blast-server")))
        .filter(|p| p.is_file());
    let program = sibling.unwrap_or_else(|| "blast-server".into());
    // only returns if the exec failed
    let error = std::process::Command::new(&program)
        .args(std::env::args_os().skip(2))
        .exec();
    eprintln!(
        "Error: could not run {}: {error}\n\
         `blast server` needs the blast-server binary: build it with\n\
         `cargo build --release -p blast_server` (or a workspace `cargo build --release`)",
        program.display()
    );
    ExitCode::FAILURE
}
