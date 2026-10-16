// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

use kurbo::Rect;
use peniko::ImageData;

/// A retained image mask applied to a widget's complete painted subtree.
///
/// Bounds are in the widget's content coordinates. The image shares its pixel
/// storage with the normal resource registry; changing progress does not upload it.
#[derive(Clone, Debug)]
pub struct PaintImageMask {
    pub image: ImageData,
    pub bounds: Rect,
    pub progress: f32,
    pub softness: f32,
    pub invert: bool,
    /// Subtree opacity, composed together with the mask coverage.
    pub opacity: f32,
}

/// Threshold parameters at a particular use of a retained image mask.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskProgress {
    /// Zero hides the subtree; one reveals it completely.
    pub progress: f32,
    /// Nonnegative transition width; zero selects a hard edge.
    pub softness: f32,
    /// Reverse encoded image gray before applying the threshold.
    pub invert: bool,
}
