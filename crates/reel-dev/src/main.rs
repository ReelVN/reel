use anyhow::{Result, bail};
use reel_dev::{audio, maintenance, root, vita};
use std::{env, path::Path, process::Command};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let command = args.iter().map(String::as_str).collect::<Vec<_>>();
    match command.as_slice() {
        ["check"] => {
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
        ["deps"] => maintenance::dependencies()?,
        ["audio-regression"] => audio::regression(false)?,
        ["audio-regression", "--release"] => audio::regression(true)?,
        ["audio-probe"] => audio::probes()?,
        ["vita-assets", ..] => vita::assets(&args[1..])?,
        ["root-negative"] => maintenance::negative_controls()?,
        ["compiler-probe", probe, mode] => maintenance::compiler_probe(probe, Some(mode))?,
        ["compiler-probe", probe] => maintenance::compiler_probe(probe, None)?,
        ["measure", build, target, ..] => {
            maintenance::measure(Path::new(build), target, &args[3..])?
        }
        _ => bail!(
            "Usage: cargo run -p reel-dev -- <check|deps|audio-regression [--release]|audio-probe|vita-assets --modules DIR --font FILE --font-license FILE|root-negative|compiler-probe frontend/semantic/lowering [MODE]|measure BUILD_JSON contracts/r01_measurements [TEST_ARGS]>\nRepository: {}",
            root().display()
        ),
    }
    Ok(())
}
