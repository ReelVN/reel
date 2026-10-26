// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0
use crate::{resources::ResourceLease, stage::effect::EffectInstance};
use kurbo::Rect;
use std::sync::Arc;

/// Prepared subtree effect. The immutable shader lease travels with paint and
/// frame ownership; bounds are in this widget's local logical coordinates.
#[derive(Clone, Debug)]
pub struct PaintEffect {
    pub instance: Arc<EffectInstance>,
    pub shader: ResourceLease,
    /// Immutable whole-image leases in the definition's texture binding order.
    pub images: Vec<ResourceLease>,
    pub bounds: Rect,
    /// Invert clamped scalar output when this effect supplies subtree coverage.
    pub coverage_invert: bool,
}
