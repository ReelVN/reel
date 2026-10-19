// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0
use crate::stage::MaskProgram;
use kurbo::Rect;
use peniko::ImageData;
use std::sync::Arc;

/// Retained scalar coverage applied once to a widget's painted subtree.
#[derive(Clone, Debug)]
pub struct PaintImageMask {
    pub program: Arc<MaskProgram>,
    pub images: Arc<[ImageData]>,
    pub bounds: Rect,
    /// progress, softness, x, y, and the string shorthand's invert flag.
    pub parameters: [f32; 5],
}
impl PaintImageMask {
    pub fn is_identity(&self) -> bool {
        self.program.is_image_threshold() && self.parameters[0] >= 1.
    }
    pub fn hides_all(&self) -> bool {
        self.program.is_image_threshold() && self.parameters[0] <= 0.
    }
}
