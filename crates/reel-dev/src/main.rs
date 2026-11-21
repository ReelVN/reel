use anyhow::Result;
use clap::{Parser, Subcommand};
use reel_dev::{audio, frame_cost, maintenance, performance, render_cost, root};
use std::{path::PathBuf, process::Command};

#[derive(Parser)]
#[command(
    name = "reel-dev",
    about = "Reel repository checks and performance tools"
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Check the preserved compiler source fixtures.
    Check,
    /// Check workspace dependencies and platform boundaries.
    Deps,
    AudioRegression {
        #[arg(long)]
        release: bool,
    },
    AudioProbe,
    /// Open a source project for manual diagnosis and save its logs and frame timings.
    Debug(performance::DebugOptions),
    Perf(performance::Options),
    FrameCost {
        scope: String,
        input: PathBuf,
        output: PathBuf,
        budget: Option<PathBuf>,
    },
    RenderCost {
        scope: String,
        input: PathBuf,
        output: PathBuf,
    },
    RootNegative,
    CompilerProbe {
        probe: String,
        mode: Option<String>,
    },
    Measure {
        build: PathBuf,
        target: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            error.print()?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    match cli.command {
        Action::Check => {
            let status = Command::new("cargo")
                .current_dir(root())
                .args([
                    "test",
                    "--locked",
                    "-p",
                    "aside-compiler",
                    "--test",
                    "semantic_quirrel",
                    "--test",
                    "quirrel_vm",
                ])
                .status()?;
            anyhow::ensure!(status.success(), "compiler source fixture contract failed");
        }
        Action::Deps => maintenance::dependencies()?,
        Action::AudioRegression { release } => audio::regression(release)?,
        Action::AudioProbe => audio::probes()?,
        Action::Debug(options) => performance::debug(options)?,
        Action::Perf(options) => performance::execute(options)?,
        Action::FrameCost {
            scope,
            input,
            output,
            budget,
        } => {
            frame_cost::report(&scope, &input, &output, budget.as_deref())?;
        }
        Action::RenderCost {
            scope,
            input,
            output,
        } => {
            render_cost::report(&scope, &input, &output)?;
        }
        Action::RootNegative => maintenance::negative_controls()?,
        Action::CompilerProbe { probe, mode } => {
            maintenance::compiler_probe(&probe, mode.as_deref())?
        }
        Action::Measure {
            build,
            target,
            arguments,
        } => maintenance::measure(&build, &target, &arguments)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_tree_and_forwarded_arguments() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        let parsed = Cli::try_parse_from([
            "reel-dev",
            "measure",
            "build.json",
            "contracts",
            "--ignored",
            "--nocapture",
        ])
        .unwrap();
        let Action::Measure { arguments, .. } = parsed.command else {
            panic!("measure");
        };
        assert_eq!(arguments, ["--ignored", "--nocapture"]);
        assert!(
            Cli::try_parse_from(["reel-dev", "perf", "--help"])
                .is_err_and(|e| e.kind() == clap::error::ErrorKind::DisplayHelp)
        );
    }
}
