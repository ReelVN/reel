// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lazily allocated display text and source grapheme boundaries for password editing.
use alloc::{string::String, vec::Vec};
use icu_segmenter::GraphemeClusterSegmenterBorrowed;

const BULLET: char = '•';
const BULLET_BYTES: usize = BULLET.len_utf8();

#[derive(Clone)]
pub(super) struct MaskedDisplay {
    pub text: String,
    boundaries: Vec<usize>,
    pub dirty: bool,
}
impl Default for MaskedDisplay {
    fn default() -> Self {
        Self {
            text: String::new(),
            boundaries: Vec::new(),
            dirty: true,
        }
    }
}
impl MaskedDisplay {
    pub fn rebuild(&mut self, source: &str, segmenter: GraphemeClusterSegmenterBorrowed<'_>) {
        if !self.dirty {
            return;
        }
        self.boundaries.clear();
        self.boundaries.extend(segmenter.segment_str(source));
        self.text.clear();
        for _ in 1..self.boundaries.len() {
            self.text.push(BULLET);
        }
        self.dirty = false;
    }
    pub fn display_index(&self, source_index: usize) -> usize {
        self.boundaries
            .partition_point(|index| *index <= source_index)
            .saturating_sub(1)
            * BULLET_BYTES
    }
    pub fn source_index(&self, display_index: usize) -> usize {
        self.boundaries
            .get(display_index / BULLET_BYTES)
            .copied()
            .unwrap_or_else(|| self.boundaries.last().copied().unwrap_or(0))
    }
}
