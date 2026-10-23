//! Repository validation and reproducible measurement commands.
pub mod audio;
pub mod maintenance;
pub mod process;

use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_owned()
}
