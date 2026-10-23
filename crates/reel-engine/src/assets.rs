//! Owned raw resource data supplied through the host's ordinary asset service.
use reel_core::{budget::OwnedBytes, identity::ContentVersion};
use std::sync::Arc;

/// An immutable input buffer keeps its resource budget ticket until VM copying
/// finishes or its owner cancels the request.
pub struct AssetData {
    pub version: ContentVersion,
    pub bytes: Arc<OwnedBytes>,
}

/// Nonblocking access to the same logical assets used by images and media.
/// Restored requests may pin a content version already read before capture.
pub trait AssetLoader {
    /// Pump the shared service once before polling this producer round.
    fn maintain(&mut self) {}
    fn read(&mut self, path: &str, version: Option<ContentVersion>) -> Result<u64, String>;
    fn poll(&mut self, id: u64) -> Option<Result<AssetData, String>>;
    fn cancel(&mut self, id: u64);
}
