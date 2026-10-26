// Copyright 2026 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

use kurbo::{Affine, Rect};
use std::sync::Arc;

use crate::core::{PaintImageMask, WidgetId};
use crate::imaging::PaintSink;
use crate::imaging::record::{Scene, replay_transformed};

/// Actual painter ownership, independent of the widget requesting a layer boundary.
#[derive(Clone, Debug)]
pub struct CommandOwnerRange {
    /// Contiguous command range produced by one paint/append operation.
    pub commands: std::ops::Range<usize>,
    /// Widget that produced these commands, including ordinary Inline children.
    pub owner: WidgetId,
    /// Linear premultiplied multiplier for image brushes in this range only.
    pub image_color: [f32; 4],
    pub image_appearance: Option<crate::frame::ImageAppearance>,
    pub image_resource: Option<crate::resources::ResourceImageBrush>,
    /// Keeps a typed image draw's producer storage alive with the recording.
    pub image_lease: Option<crate::resources::ResourceLease>,
    /// Image brush matrices already map local coordinates to source pixels.
    pub image_sampling_transform: bool,
    /// Source-pixel window retained by this widget's cached image paint.
    pub image_source_rect: Option<Rect>,
}

/// Reliable provenance captured when a widget's cached paint is appended.
#[derive(Clone, Debug)]
pub struct GroupMetadata {
    /// Index of the `PushGroup` command in this fragment.
    pub command: usize,
    /// Widget whose paint callback produced this group.
    pub owner: WidgetId,
    /// Effect-local coordinates to layer-local coordinates.
    pub effect_transform: Affine,
    /// Optional image threshold, separate from the retained mask's scene content.
    pub image_mask: Option<PaintImageMask>,
    /// Typed prepared fragment applied before group coverage and compositing.
    pub effect: Option<crate::core::PaintEffect>,
    /// Scalar shader coverage applied after the subtree's color effect.
    pub mask_effect: Option<crate::core::PaintEffect>,
    /// sRGB inversion of the completed subtree before coverage and opacity.
    pub invert: f32,
    /// Subtree blend retained when the Imaging record cannot express it.
    pub blend: Option<crate::frame::BlendMode>,
}

/// A balanced widget paint range; coordinates identify gaps between commands.
#[derive(Clone, Debug)]
pub struct SubtreePaint {
    pub widget: WidgetId,
    pub commands: std::ops::Range<usize>,
    pub suppressed_by_ancestor: bool,
    /// Effective capture inclusion, including every ancestor.
    pub capture_included: bool,
}

/// Ordered UI paint output consumed by the portable frame compiler.
#[derive(Debug)]
pub struct VisualLayerPlan {
    /// Layers in painter order, back to front.
    pub layers: Vec<VisualLayer>,
    pub subtrees: Vec<SubtreePaint>,
    pub body_commands: usize,
    /// Sorted, disjoint balanced command ranges omitted only from captures.
    pub capture_exclusions: Vec<std::ops::Range<usize>>,
}

impl VisualLayerPlan {
    /// Replay all layers into a sink in window coordinate space.
    ///
    /// Return UnsupportedRecord before writing when Imaging cannot represent a
    /// subtree blend or a typed resource image draw.
    /// Production FrameBuild reads the original plan and preserves every Reel blend.
    pub fn replay_into<S>(&self, sink: &mut S) -> Result<(), crate::frame::BuildError>
    where
        S: PaintSink + ?Sized,
    {
        for (fragment, layer) in self.layers.iter().enumerate() {
            if let Some(range) = layer
                .command_owners
                .iter()
                .find(|range| range.image_resource.is_some())
            {
                return Err(crate::frame::BuildError {
                    kind: crate::frame::BuildErrorKind::UnsupportedRecord,
                    at: crate::frame::RecordLocation {
                        fragment,
                        command: range.commands.start,
                        mask_depth: 0,
                        owner: range.owner,
                    },
                });
            }
            if let Some(group) = layer.groups.iter().find(|group| {
                group.effect.is_some()
                    || group.mask_effect.is_some()
                    || group.blend.is_some_and(|blend| blend.imaging().is_none())
            }) {
                return Err(crate::frame::BuildError {
                    kind: crate::frame::BuildErrorKind::UnsupportedRecord,
                    at: crate::frame::RecordLocation {
                        fragment,
                        command: group.command,
                        mask_depth: 0,
                        owner: group.owner,
                    },
                });
            }
        }
        for layer in &self.layers {
            if let VisualLayerKind::Scene(scene) = &layer.kind {
                replay_transformed(scene, sink, layer.transform);
            }
        }
        Ok(())
    }
}

/// A single visual layer in UI paint output.
///
/// The retained scene is stored in layer-local coordinates. Apply [`transform`](Self::transform)
/// to composite it into window space, including for the base layer.
#[derive(Debug)]
pub struct VisualLayer {
    /// The visual content represented by this layer.
    pub kind: VisualLayerKind,
    /// Transform from layer-local space to window space.
    pub transform: Affine,
    /// Default painter identity for this fragment; explicit ranges may name other painters.
    pub widget_id: WidgetId,
    /// Group provenance; independent of optional context annotations or child draws.
    pub groups: Vec<GroupMetadata>,
    /// Sorted, nonoverlapping command ownership captured at paint/append boundaries.
    /// An empty list assigns the whole fragment to `widget_id` with default image paint.
    pub command_owners: Vec<CommandOwnerRange>,
}

/// The content represented by a visual layer.
#[derive(Debug)]
pub enum VisualLayerKind {
    /// Immutable widget paint shared by recordings in layer-local coordinates.
    /// Repainting replaces the producer's scene; existing candidates retain its old content.
    Scene(Arc<Scene>),
    /// A placeholder for externally realized content.
    ///
    /// Bounds are content-local; apply `content_transform` then the layer transform.
    External {
        /// Host-owned opaque content token, never a native handle.
        token: u64,
        /// Placeholder bounds in content-local coordinates.
        bounds: Rect,
        /// Content-local to layer-local coordinates.
        content_transform: Affine,
    },
}

#[cfg(test)]
mod tests {
    use super::{VisualLayer, VisualLayerKind, VisualLayerPlan};
    use crate::core::WidgetId;
    use crate::imaging::Painter;
    use crate::imaging::record::{Scene, replay_transformed};
    use kurbo::{Affine, Rect};
    use peniko::Color;

    fn filled_scene(rect: Rect, color: Color) -> Scene {
        let mut scene = Scene::new();
        Painter::new(&mut scene).fill_rect(rect, color);
        scene
    }

    #[test]
    fn replay_into_replays_layers_in_window_space() {
        let root_scene = filled_scene(Rect::new(0.0, 0.0, 10.0, 10.0), Color::from_rgb8(255, 0, 0));
        let overlay_scene =
            filled_scene(Rect::new(0.0, 0.0, 4.0, 4.0), Color::from_rgb8(0, 0, 255));

        let plan = VisualLayerPlan {
            capture_exclusions: Vec::new(),
            subtrees: Vec::new(),
            body_commands: 3,
            layers: vec![
                VisualLayer {
                    kind: VisualLayerKind::Scene(root_scene.clone().into()),
                    transform: Affine::IDENTITY,
                    widget_id: WidgetId::next(),
                    groups: Vec::new(),
                    command_owners: Vec::new(),
                },
                VisualLayer {
                    kind: VisualLayerKind::External {
                        token: 0,
                        content_transform: Affine::IDENTITY,
                        bounds: Rect::new(10.0, 0.0, 20.0, 10.0),
                    },
                    transform: Affine::IDENTITY,
                    widget_id: WidgetId::next(),
                    groups: Vec::new(),
                    command_owners: Vec::new(),
                },
                VisualLayer {
                    kind: VisualLayerKind::Scene(overlay_scene.clone().into()),
                    transform: Affine::translate((20.0, 5.0)),
                    widget_id: WidgetId::next(),
                    groups: Vec::new(),
                    command_owners: Vec::new(),
                },
            ],
        };

        let mut actual = Scene::new();
        plan.replay_into(&mut actual).unwrap();

        let mut expected = Scene::new();
        replay_transformed(&root_scene, &mut expected, Affine::IDENTITY);
        replay_transformed(
            &overlay_scene,
            &mut expected,
            Affine::translate((20.0, 5.0)),
        );

        assert_eq!(actual, expected);
    }
}
