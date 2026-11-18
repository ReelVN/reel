// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// An asset name, encoded data or immutable decoded resource. Clones share the input.
#[derive(Clone, Debug)]
pub struct ImageSource {
    key: Arc<str>,
    encoded: Option<Arc<[u8]>>,
    resource: Option<crate::resources::ResourceLease>,
    /// Distinct asset paths available in this image's interaction skin.
    pub skin_paths: Option<Arc<[String]>>,
}
impl ImageSource {
    /// No picture has been supplied yet. This never requests an asset.
    pub fn empty() -> Self {
        Self {
            key: Arc::from(""),
            encoded: None,
            resource: None,
            skin_paths: None,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.key.is_empty()
    }
    pub fn encoded(bytes: impl Into<Arc<[u8]>>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            key: format!("memory:image/{}", NEXT.fetch_add(1, Ordering::Relaxed)).into(),
            encoded: Some(bytes.into()),
            resource: None,
            skin_paths: None,
        }
    }
    /// A decoded frame or host image. Existing snapshots/accepted frames retain
    /// their own immutable lease when the producer publishes a replacement.
    pub fn resource(lease: crate::resources::ResourceLease) -> Self {
        let handle = lease.handle();
        Self {
            key: format!(
                "memory:resource/{}/{}/{}",
                handle.registry, handle.slot, handle.generation
            )
            .into(),
            encoded: None,
            resource: Some(lease),
            skin_paths: None,
        }
    }
    pub fn lease(&self) -> Option<&crate::resources::ResourceLease> {
        self.resource.as_ref()
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn bytes(&self) -> Option<&Arc<[u8]>> {
        self.encoded.as_ref()
    }
}
impl PartialEq for ImageSource {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.skin_paths == other.skin_paths
    }
}
impl From<String> for ImageSource {
    fn from(key: String) -> Self {
        Self {
            key: key.into(),
            encoded: None,
            resource: None,
            skin_paths: None,
        }
    }
}
impl From<&str> for ImageSource {
    fn from(key: &str) -> Self {
        Self {
            key: key.into(),
            encoded: None,
            resource: None,
            skin_paths: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_clones_share_asset_names_and_encoded_identity() {
        for source in [
            ImageSource::from("assets/background.png"),
            ImageSource::from(String::from("assets/foreground.png")),
            ImageSource::encoded(Arc::<[u8]>::from([1, 2, 3])),
        ] {
            let cloned = source.clone();
            assert!(Arc::ptr_eq(&source.key, &cloned.key));
            assert_eq!(source, cloned);
            if let Some(bytes) = source.bytes() {
                assert!(Arc::ptr_eq(bytes, cloned.bytes().unwrap()));
            }
        }
    }
}
