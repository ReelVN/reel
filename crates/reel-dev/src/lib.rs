//! Repository validation and reproducible measurement commands.
pub mod audio;
pub mod headless;
pub mod maintenance;
pub mod process;
pub mod testing;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_owned()
}

pub fn sha(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(encoded, "{byte:02x}").expect("formatting into a String");
    }
    encoded
}

pub fn json(path: &Path) -> Result<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).with_context(|| path.display().to_string())?)
        .with_context(|| path.display().to_string())
}

pub fn files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        anyhow::ensure!(entry.file_name() != ".git", "nested Git entry");
        let ty = entry.file_type()?;
        anyhow::ensure!(
            !ty.is_symlink(),
            "unexpected symlink: {}",
            entry.path().display()
        );
        if ty.is_dir() {
            anyhow::ensure!(entry.file_name() != ".git", "nested Git directory");
            found.extend(files(&entry.path())?);
        } else {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}
