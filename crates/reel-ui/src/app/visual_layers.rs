// Copyright 2026 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

use kurbo::{Affine, Rect};

use crate::core::{MaskProgress, WidgetId};
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
    pub image_gradient: Option<crate::frame::ImageColorGradient>,
}

/// Reliable provenance captured when a widget's cached paint is appended.
#[derive(Clone, Copy, Debug)]
pub struct GroupMetadata {
    /// Index of the `PushGroup` command in this fragment.
    pub command: usize,
    /// Widget whose paint callback produced this group.
    pub owner: WidgetId,
    /// Effect-local coordinates to layer-local coordinates.
    pub effect_transform: Affine,
    /// Optional image threshold, separate from the retained mask's scene content.
    pub mask_progress: Option<MaskProgress>,
}

/// Snapshot of Masonry's current visual layers in painter order.
///
/// This is the semantic paint output of Masonry. Current hosts still consume it through
/// compatibility helpers that flatten or reinterpret it, but the plan is the source of truth.
#[derive(Debug)]
pub struct VisualLayerPlan {
    /// Layers in painter order, back to front.
    pub layers: Vec<VisualLayer>,
}

impl VisualLayerPlan {
    /// Replay all layers into a sink in window coordinate space.
    ///
    /// This preserves current flattened rendering behavior for hosts that do not yet realize
    /// layers differently.
    pub fn replay_into<S>(&self, sink: &mut S)
    where
        S: PaintSink + ?Sized,
    {
        for layer in &self.layers {
            if let VisualLayerKind::Scene(scene) = &layer.kind {
                replay_transformed(scene, sink, layer.transform);
            }
        }
    }

    /// The first scene layer, if one exists.
    ///
    /// In the current compatibility model, this is the first scene layer that
    /// flattened consumers treat as the base scene.
    pub fn root_layer(&self) -> Option<&VisualLayer> {
        self.layers
            .iter()
            .find(|layer| matches!(layer.kind, VisualLayerKind::Scene(_)))
    }

    /// All scene layers after the first one, in painter order.
    ///
    /// In the current compatibility model, these are replayed after the root layer
    /// in painter order. External placeholders are skipped.
    pub fn overlay_layers(&self) -> impl Iterator<Item = &VisualLayer> {
        let mut saw_root_scene = false;
        self.layers.iter().filter(move |layer| match layer.kind {
            VisualLayerKind::Scene(_) if !saw_root_scene => {
                saw_root_scene = true;
                false
            }
            VisualLayerKind::Scene(_) => true,
            VisualLayerKind::External { .. } => false,
        })
    }
}

/// A single visual layer in Masonry's paint output.
///
/// The retained scene is stored in layer-local coordinates. Apply [`transform`](Self::transform)
/// to composite it into window space, including for the base layer.
#[derive(Debug)]
pub struct VisualLayer {
    /// The visual content represented by this layer.
    pub kind: VisualLayerKind,
    /// Transform from layer-local space to window space.
    pub transform: Affine,
    /// The widget that requested this layer boundary.
    pub widget_id: WidgetId,
    /// Group provenance; independent of optional context annotations or child draws.
    pub groups: Vec<GroupMetadata>,
    /// Sorted, nonoverlapping command ownership captured at paint/append boundaries.
    pub command_owners: Vec<CommandOwnerRange>,
}

/// The content represented by a visual layer.
#[derive(Debug)]
pub enum VisualLayerKind {
    /// Retained Masonry scene content in layer-local coordinates.
    Scene(Scene),
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
            layers: vec![
                VisualLayer {
                    kind: VisualLayerKind::Scene(root_scene.clone()),
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
                    kind: VisualLayerKind::Scene(overlay_scene.clone()),
                    transform: Affine::translate((20.0, 5.0)),
                    widget_id: WidgetId::next(),
                    groups: Vec::new(),
                    command_owners: Vec::new(),
                },
            ],
        };

        let mut actual = Scene::new();
        plan.replay_into(&mut actual);

        let mut expected = Scene::new();
        replay_transformed(&root_scene, &mut expected, Affine::IDENTITY);
        replay_transformed(
            &overlay_scene,
            &mut expected,
            Affine::translate((20.0, 5.0)),
        );

        assert_eq!(actual, expected);
    }

    #[test]
    fn scene_layer_helpers_skip_external_placeholders() {
        let root_scene = filled_scene(Rect::new(0.0, 0.0, 10.0, 10.0), Color::from_rgb8(255, 0, 0));
        let overlay_scene =
            filled_scene(Rect::new(0.0, 0.0, 4.0, 4.0), Color::from_rgb8(0, 0, 255));

        let plan = VisualLayerPlan {
            layers: vec![
                VisualLayer {
                    kind: VisualLayerKind::Scene(root_scene),
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
                    kind: VisualLayerKind::Scene(overlay_scene),
                    transform: Affine::translate((20.0, 5.0)),
                    widget_id: WidgetId::next(),
                    groups: Vec::new(),
                    command_owners: Vec::new(),
                },
            ],
        };

        assert!(matches!(
            plan.root_layer().map(|layer| &layer.kind),
            Some(VisualLayerKind::Scene(_))
        ));
        let overlays: Vec<_> = plan.overlay_layers().collect();
        assert_eq!(overlays.len(), 1);
        assert!(matches!(overlays[0].kind, VisualLayerKind::Scene(_)));
    }
}
