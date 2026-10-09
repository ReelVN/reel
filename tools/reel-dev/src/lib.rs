//! Repository maintenance checks. These tools never execute Aside programs.
pub mod audio;
pub mod fixtures;
pub mod maintenance;
#[path = "../../test-support/process.rs"]
pub mod process;
pub mod upstream;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

pub fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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
