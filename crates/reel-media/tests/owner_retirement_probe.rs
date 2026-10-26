//! Build only reel-media, then compile an isolated unsafe allocator observer.
//! No VM, Aside, reel-dev, native device or workspace test execution.
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn rlib(messages: &str, crate_name: &str) -> PathBuf {
    let prefix = format!("lib{crate_name}-");
    let mut found = None;
    for line in messages.lines() {
        if !line.contains("\"reason\":\"compiler-artifact\"") {
            continue;
        }
        let Some((_, filenames)) = line.split_once("\"filenames\":[") else {
            continue;
        };
        let filenames = filenames.split(']').next().unwrap();
        for value in filenames.split(',') {
            let path = value.trim().trim_matches('"').replace("\\\\", "\\");
            let path = PathBuf::from(path);
            if path
                .extension()
                .is_some_and(|extension| extension == "rlib")
                && path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&prefix)
                || path
                    .file_name()
                    .is_some_and(|name| name == format!("lib{crate_name}.rlib").as_str())
            {
                assert!(
                    found.replace(path).is_none(),
                    "ambiguous Cargo library artifact"
                );
            }
        }
    }
    found.unwrap_or_else(|| panic!("Cargo did not report {crate_name} rlib"))
}

#[test]
fn physical_pcm_deallocation_precedes_owner_retirement_evidence() {
    run_probe("owner_retirement_probe", &[]);
}

#[test]
fn pcm_controls_and_rate_resampling_remain_allocation_free() {
    run_probe("pcm_probe", &["--features", "decode"]);
}

fn run_probe(name: &str, features: &[&str]) {
    // Cargo publishes one un-hashed reel_media.rlib. Keep it unchanged until
    // the isolated observer has linked, including on Windows with open-file locks.
    static BUILD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _build = BUILD.lock().unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let artifacts = root.join("target/reel-artifacts").join(name);
    std::fs::create_dir_all(&artifacts).unwrap();
    let build = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(root)
        .args([
            "build",
            "-p",
            "reel-media",
            "--no-default-features",
            "--locked",
            "--offline",
            "--message-format=json",
        ])
        .args(features)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "pure Rust reel-media build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let messages = String::from_utf8(build.stdout).unwrap();
    let media = rlib(&messages, "reel_media");
    let core = rlib(&messages, "reel_core");
    let executable = artifacts.join(format!(
        "{name}-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let compile = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .current_dir(root)
        .args(["--edition", "2024"])
        .arg(manifest.join(format!("tests/support/{name}.rs")))
        .arg("--extern")
        .arg(format!("reel_media={}", media.display()))
        .arg("--extern")
        .arg(format!("reel_core={}", core.display()))
        .arg("-L")
        .arg(format!("dependency={}", core.parent().unwrap().display()))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "isolated allocator observer compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&executable)
        .current_dir(root)
        .output()
        .unwrap();
    std::fs::write(artifacts.join("latest.log"), &run.stdout).unwrap();
    assert!(
        run.status.success(),
        "allocator probe failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    print!("{}", String::from_utf8_lossy(&run.stdout));
}
