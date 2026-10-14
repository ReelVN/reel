//! Explicit synchronous font preparation for Rust diagnostics and device tests.
//! Author fonts use the asynchronous shared service in `assets`.
use reel_core::{
    budget::{BudgetPool, OwnedBytes},
    identity::{AssetKey, ContentVersion, ResourceVersion},
};
use reel_ui::resources::{ResourceLease, ResourceRegistry};
use std::{io::Read, path::Path};

pub fn load(path: &Path, pool: &BudgetPool) -> Result<ResourceLease, Box<dyn std::error::Error>> {
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    let ticket = pool
        .reserve(length)
        .map_err(|e| format!("font quota: {e:?}"))?;
    let mut bytes = vec![0; usize::try_from(length)?];
    file.read_exact(&mut bytes)?;
    if file.read(&mut [0])? != 0 {
        return Err("font changed size during preparation".into());
    }
    let version = ResourceVersion {
        key: AssetKey(path.to_string_lossy().into()),
        version: ContentVersion(*blake3::hash(&bytes).as_bytes()),
    };
    let bytes = OwnedBytes::new(bytes, ticket).map_err(|e| format!("font bytes: {e:?}"))?;
    let mut registry =
        ResourceRegistry::new(1, 64 << 10).map_err(|e| format!("font registry: {e:?}"))?;
    let handle = registry
        .register_font(version, bytes)
        .map_err(|e| format!("font: {e:?}"))?;
    registry
        .acquire(handle)
        .map_err(|e| format!("font lease: {e:?}").into())
}
