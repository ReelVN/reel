// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! A two-dimensional view of an ordinary stage subtree.
use crate::kurbo::{Affine, Size};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraView {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
    /// Clockwise camera angle in degrees; the viewed scene rotates oppositely.
    pub rotation: f64,
}
impl Default for CameraView {
    fn default() -> Self {
        Self {
            x: 0.,
            y: 0.,
            zoom: 1.,
            rotation: 0.,
        }
    }
}
impl CameraView {
    /// Map the world focus point to the inner viewport center. The caller places
    /// children at the viewport's padding origin using normal layout placement.
    pub fn transform(self, inner: Size) -> Affine {
        Affine::translate((inner.width * 0.5, inner.height * 0.5))
            * Affine::scale(self.zoom)
            * Affine::rotate(-self.rotation.to_radians())
            * Affine::translate((-self.x, -self.y))
    }
}
