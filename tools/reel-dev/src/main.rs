use anyhow::{Result, bail};
use reel_dev::{fixtures, maintenance, root};
use std::{env, fs, path::Path};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let command = args.iter().map(String::as_str).collect::<Vec<_>>();
    match command.as_slice() {
        ["check"] => {
            let assertions = fixtures::conversions(false)?;
            fixtures::immutable_t01()?;
            let inventory = fixtures::inventory()?;
            println!(
                "{} fixtures; {assertions} preserved assertion calls; 10 immutable T01 sources/maps; no source execution",
                inventory["fixtures"].as_array().unwrap().len()
            );
        }
        ["convert", "--write"] => {
            fixtures::conversions(true)?;
        }
        ["inventory"] => {
            let path = maintenance::artifacts()?.join("quirrel-inventory.json");
            fs::write(
                &path,
                serde_json::to_string_pretty(&fixtures::inventory()?)? + "\n",
            )?;
            println!("{}", path.display());
        }
        ["verify-results", dir] => {
            let failures = fixtures::verify_results(Path::new(dir))?;
            println!(
                "Verified original raw hashes, line comparisons, seeds, universe and frontend hashes; mismatches={failures}"
            );
        }
        ["verify-results", dir, "--t01-baseline"] => {
            let failures = fixtures::verify_results(Path::new(dir))?;
            anyhow::ensure!(
                failures == 8,
                "T01 baseline must contain exactly eight mismatches and runner exit 1"
            );
            println!(
                "T01 baseline verified: every raw SHA and comparison, 8 mismatches, runner_exit_code=1"
            );
        }
        ["deps"] => maintenance::dependencies(None)?,
        ["deps", manifest] => maintenance::dependencies(Some(Path::new(manifest)))?,
        ["audit-masonry", upstream] => maintenance::audit_masonry(Path::new(upstream))?,
        ["prepare-masonry", upstream] => maintenance::prepare_experiment(Path::new(upstream))?,
        ["root-negative"] => maintenance::negative_controls()?,
        ["compiler-probe", probe, mode] => maintenance::compiler_probe(probe, Some(mode))?,
        ["compiler-probe", probe] => maintenance::compiler_probe(probe, None)?,
        ["measure", build, target, ..] => {
            maintenance::measure(Path::new(build), target, &args[3..])?
        }
        _ => bail!(
            "Usage: cargo run -p reel-dev -- <check|convert --write|inventory|verify-results DIR [--t01-baseline]|deps [MANIFEST]|audit-masonry UPSTREAM|prepare-masonry UPSTREAM|root-negative|measure BUILD_JSON contracts/r01_measurements [TEST_ARGS]>\nRepository: {}",
            root().display()
        ),
    }
    Ok(())
}
