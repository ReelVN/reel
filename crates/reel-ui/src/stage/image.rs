// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// An asset name or immutable encoded PNG/JPEG data. Clones share the input.
#[derive(Clone, Debug)]
pub struct ImageSource {
    key: String,
    encoded: Option<Arc<[u8]>>,
}
impl ImageSource {
    pub fn encoded(bytes: impl Into<Arc<[u8]>>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            key: format!("memory:image/{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            encoded: Some(bytes.into()),
        }
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
        self.key == other.key
    }
}
impl From<String> for ImageSource {
    fn from(key: String) -> Self {
        Self { key, encoded: None }
    }
}
impl From<&str> for ImageSource {
    fn from(key: &str) -> Self {
        key.to_owned().into()
    }
}
