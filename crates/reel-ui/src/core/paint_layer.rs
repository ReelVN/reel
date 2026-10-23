// Copyright 2026 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

/// How the current widget subtree should be represented in the current paint pass.
///
/// Controls how the widget subtree enters the ordered
/// [`VisualLayerPlan`](crate::app::VisualLayerPlan) consumed by frame compilation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaintLayerMode {
    /// Paint into the current scene layer.
    #[default]
    Inline,
    /// Record this widget subtree as an isolated scene layer.
    ///
    /// The subtree still paints in normal painter order. If nested isolated scene
    /// layers occur, recording splits the surrounding scene as needed to preserve
    /// that order in the flattened visual-layer plan.
    IsolatedScene,
    /// Record this widget subtree as an external placeholder layer.
    ///
    /// Records the host token, bounds and transform at this position in painter order.
    External,
}
