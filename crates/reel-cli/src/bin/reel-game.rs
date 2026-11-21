#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use clap::Parser;
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(about = "Play the game in this release")]
struct Options {
    /// Override the release's default save directory.
    #[arg(long, value_name = "DIRECTORY")]
    save_directory: Option<PathBuf>,
}

fn main() -> ExitCode {
    let options = Options::parse();
    let result = std::env::current_exe()
        .map_err(|error| error.to_string())
        .and_then(|executable| reel_cli::playback::standalone(&executable, options.save_directory));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("game: {error}");
            ExitCode::FAILURE
        }
    }
}
