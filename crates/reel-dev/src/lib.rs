//! Repository validation and reproducible measurement commands.
pub mod audio;
pub mod frame_cost;
pub mod maintenance;
pub mod performance;
pub mod process;
pub mod render_cost;

use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_owned()
}
