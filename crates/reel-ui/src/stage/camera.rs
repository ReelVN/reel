// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Camera placement for an ordinary stage subtree and parallel depth planes.
use crate::kurbo::{Affine, Size};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraView {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
    /// Clockwise camera angle in degrees; the viewed scene rotates oppositely.
    pub rotation: f64,
    /// Positive movement brings the camera closer to its depth planes.
    pub z: f64,
    /// Distance at which a plane has unit perspective scale before zoom.
    pub focal_length: f64,
}
impl Default for CameraView {
    fn default() -> Self {
        Self {
            x: 0.,
            y: 0.,
            zoom: 1.,
            rotation: 0.,
            z: 0.,
            focal_length: 600.,
        }
    }
}
impl CameraView {
    /// Map the world focus point to the inner viewport center. The caller places
    /// children at the viewport's padding origin using normal layout placement.
    pub fn transform(self, inner: Size) -> Affine {
        self.transform_at_scale(inner, self.zoom)
    }

    /// Map a plane parallel to the viewport. Near-clipped planes have no transform.
    pub fn plane_transform(self, inner: Size, distance: f64) -> Option<Affine> {
        (!self.clips(distance)).then(|| {
            self.transform_at_scale(inner, self.zoom * (self.focal_length / (distance - self.z)))
        })
    }

    /// Whether the plane lies on or behind the near clipping distance.
    pub fn clips(self, distance: f64) -> bool {
        distance - self.z <= 1.
    }

    fn transform_at_scale(self, inner: Size, zoom: f64) -> Affine {
        Affine::translate((inner.width * 0.5, inner.height * 0.5))
            * Affine::scale(zoom)
            * Affine::rotate(-self.rotation.to_radians())
            * Affine::translate((-self.x, -self.y))
    }
}
