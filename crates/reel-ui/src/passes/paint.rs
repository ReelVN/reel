// Copyright 2024 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use crate::tree_arena::ArenaMut;
use kurbo::{Affine, Line, Point, Rect, Stroke};
use peniko::{Color, Fill};
use tracing::{info_span, trace};

use crate::app::{
    CommandOwnerRange, GroupMetadata, RenderRoot, RenderRootState, SubtreePaint, VisualLayer,
    VisualLayerKind, VisualLayerMetadata, VisualLayerPlan,
};
use crate::core::recorded_paint::{CachedPaint, RecordingSink, record_owner_range};
use crate::core::{
    DefaultProperties, PaintCtx, PaintLayerMode, PropertiesRef, PropertyArena, WidgetArenaNode,
    WidgetId, WidgetState,
};
use crate::imaging::record::{Clip, Geometry, Group, Scene};
use crate::imaging::{Composite, PaintSink, Painter};
use crate::passes::{enter_span_if, recurse_on_children};
use crate::util::get_debug_color;
use std::{cell::Cell, sync::Arc};

struct CursorPaint {
    layers: Vec<VisualLayer>,
    subtrees: Vec<SubtreePaint>,
    capture_exclusions: Vec<std::ops::Range<usize>>,
    commands: usize,
}

struct LayerCollector {
    current_scene: Scene,
    groups: Vec<GroupMetadata>,
    command_owners: Vec<CommandOwnerRange>,
    layers: Vec<VisualLayer>,
    subtrees: Vec<SubtreePaint>,
    commands_before_layer: usize,
    current_owner_id: WidgetId,
    transform: Affine,
    suppress_output: bool,
    capture_included: bool,
    capture_exclusions: Vec<std::ops::Range<usize>>,
}

impl LayerCollector {
    fn new(root_id: WidgetId, transform: Affine) -> Self {
        Self {
            current_scene: Scene::new(),
            groups: Vec::new(),
            command_owners: Vec::new(),
            layers: Vec::new(),
            subtrees: Vec::new(),
            commands_before_layer: 0,
            current_owner_id: root_id,
            transform,
            suppress_output: false,
            capture_included: true,
            capture_exclusions: Vec::new(),
        }
    }

    fn begin_subtree(&mut self, widget: WidgetId, suppressed_by_ancestor: bool) -> usize {
        let position = self.commands_before_layer + self.current_scene.commands().len();
        let index = self.subtrees.len();
        self.subtrees.push(SubtreePaint {
            widget,
            commands: position..position,
            suppressed_by_ancestor,
            capture_included: self.capture_included,
        });
        index
    }

    fn end_subtree(&mut self, index: usize) {
        self.subtrees[index].commands.end =
            self.commands_before_layer + self.current_scene.commands().len();
    }

    fn append(
        &mut self,
        cached: &mut CachedPaint,
        transform: Affine,
        owner: WidgetId,
        image: crate::frame::ImagePaint,
    ) {
        if cached.scene.is_empty() {
            return;
        }
        self.finish_current_layer(false);
        let end = cached.scene.commands().len();
        let metadata = cached.metadata(owner, image);
        self.commands_before_layer += end;
        self.layers.push(VisualLayer {
            kind: VisualLayerKind::Scene(cached.scene.clone()),
            transform: self.transform * transform,
            widget_id: owner,
            metadata: metadata.map(VisualLayerMetadata::Shared),
        });
    }

    fn push_subtree_composite(
        &mut self,
        state: &WidgetState,
        transform: Affine,
        owner: WidgetId,
    ) -> bool {
        let mask = state
            .subtree_image_mask
            .as_deref()
            .filter(|mask| !mask.is_identity());
        let effect = state.subtree_effect.as_deref();
        let mask_effect = state.subtree_mask_effect.as_deref();
        let opacity = state.subtree_opacity;
        let invert = state.subtree_invert;
        let blend = state.subtree_blend;
        if mask.is_none()
            && effect.is_none()
            && mask_effect.is_none()
            && opacity == 1.
            && invert == 0.
            && blend == Default::default()
        {
            return false;
        }
        let group = Group {
            composite: Composite {
                alpha: opacity,
                // FrameBuild reads the actual mode from group provenance.
                blend: blend.imaging().unwrap_or_default(),
            },
            ..Default::default()
        };
        let command = self.current_scene.commands().len();
        self.current_scene.push_group(group);
        self.groups.push(GroupMetadata {
            command,
            owner,
            effect_transform: transform,
            image_mask: mask.cloned(),
            effect: effect.cloned(),
            mask_effect: mask_effect.cloned(),
            invert,
            blend: Some(blend),
        });
        self.record_owner(command, owner, crate::frame::ImagePaint::default());
        true
    }

    fn record_owner(&mut self, start: usize, owner: WidgetId, image: crate::frame::ImagePaint) {
        self.record_owner_range(
            start,
            self.current_scene.commands().len(),
            owner,
            image,
            None,
        );
    }
    fn record_owner_range(
        &mut self,
        start: usize,
        end: usize,
        owner: WidgetId,
        image: crate::frame::ImagePaint,
        lease: Option<&crate::resources::ResourceLease>,
    ) {
        record_owner_range(
            &mut self.command_owners,
            start,
            end,
            owner,
            image,
            lease,
            None,
        );
    }

    fn paint(&mut self, owner: WidgetId, draw: impl FnOnce(&mut Scene)) {
        let start = self.current_scene.commands().len();
        draw(&mut self.current_scene);
        self.record_owner(start, owner, crate::frame::ImagePaint::default());
    }

    fn paint_window(&mut self, owner: WidgetId, draw: impl FnOnce(&mut Scene)) {
        // Bounding boxes already use window space. Preserve painter scopes across
        // fragments instead of inverting a possibly singular layer transform.
        self.finish_current_layer(false);
        let transform = std::mem::replace(&mut self.transform, Affine::IDENTITY);
        let previous_owner = std::mem::replace(&mut self.current_owner_id, owner);
        self.paint(owner, draw);
        self.finish_current_layer(false);
        self.transform = transform;
        self.current_owner_id = previous_owner;
    }

    fn finish_current_layer(&mut self, allow_empty: bool) {
        let empty_scene = Scene::new();
        if !allow_empty && self.current_scene == empty_scene {
            return;
        }

        let scene = std::mem::replace(&mut self.current_scene, empty_scene);
        self.commands_before_layer += scene.commands().len();
        self.layers.push(VisualLayer {
            kind: VisualLayerKind::Scene(scene.into()),
            transform: self.transform,
            widget_id: self.current_owner_id,
            metadata: (!self.groups.is_empty() || !self.command_owners.is_empty()).then(|| {
                VisualLayerMetadata::Inline {
                    groups: std::mem::take(&mut self.groups),
                    command_owners: std::mem::take(&mut self.command_owners),
                }
            }),
        });
    }

    fn push_external_layer(
        &mut self,
        widget_id: WidgetId,
        token: u64,
        bounds: kurbo::Rect,
        content_transform: Affine,
    ) {
        self.commands_before_layer += 1;
        self.layers.push(VisualLayer {
            kind: VisualLayerKind::External {
                token,
                bounds,
                content_transform,
            },
            transform: self.transform,
            widget_id,
            metadata: None,
        });
    }

    fn into_layers(mut self) -> Vec<VisualLayer> {
        self.finish_current_layer(self.layers.is_empty());
        self.layers
    }
}

// --- MARK: PAINT WIDGET
fn paint_viewport(
    state: &WidgetState,
    parent: Rect,
    border_to_window: Affine,
    content_to_window: Affine,
) -> Rect {
    if let Some(effect) = state
        .subtree_effect
        .as_ref()
        .or(state.subtree_mask_effect.as_ref())
    {
        content_to_window.transform_rect_bbox(effect.bounds)
    } else {
        state.clip_path.map_or(parent, |clip| {
            parent.intersect(border_to_window.transform_rect_bbox(clip))
        })
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "recursive paint pass borrows disjoint tree and cache state"
)]
fn paint_widget(
    global_state: &mut RenderRootState,
    default_properties: &DefaultProperties,
    property_arena: &PropertyArena,
    layer_collector: &mut LayerCollector,
    scene_cache: &mut HashMap<WidgetId, (CachedPaint, CachedPaint, CachedPaint)>,
    cursor_layers: &mut Vec<CursorPaint>,
    in_cursor: bool,
    border_box_to_layer_transform: Affine,
    parent_viewport: Rect,
    node: ArenaMut<'_, WidgetArenaNode>,
) {
    if node.item.widget.is_cursor() && !in_cursor {
        let id = node.item.state.id;
        let active = global_state
            .cursor
            .overlay
            .filter(|(cursor, _)| *cursor == id);
        let position = active.map_or(Point::ORIGIN, |(_, point)| point);
        let mut collector = LayerCollector::new(id, Affine::translate(position.to_vec2()));
        collector.suppress_output = active.is_none();
        let size = global_state
            .size
            .to_logical::<f64>(global_state.scale_factor);
        let transform = node.item.state.transform;
        paint_widget(
            global_state,
            default_properties,
            property_arena,
            &mut collector,
            scene_cache,
            cursor_layers,
            true,
            transform,
            Rect::new(0., 0., size.width, size.height),
            node,
        );
        if active.is_some() {
            let commands =
                collector.commands_before_layer + collector.current_scene.commands().len();
            if !layer_collector.capture_included {
                collector.capture_exclusions.clear();
                if commands != 0 {
                    collector.capture_exclusions.push(0..commands);
                }
                for subtree in &mut collector.subtrees {
                    subtree.capture_included = false;
                }
            }
            let subtrees = std::mem::take(&mut collector.subtrees);
            let capture_exclusions = std::mem::take(&mut collector.capture_exclusions);
            cursor_layers.push(CursorPaint {
                layers: collector.into_layers(),
                subtrees,
                capture_exclusions,
                commands,
            });
        }
        return;
    }
    let mut children = node.children;
    let widget = &mut *node.item.widget;
    let state = &mut node.item.state;
    let properties = &mut node.item.properties;
    let class_set = &node.item.class_set;
    let id = state.id;

    let trace = global_state.trace.paint;
    let _span = enter_span_if(trace, state);

    // Note: At this point we could short-circuit if is_stashed is true,
    // but we deliberately avoid doing that to avoid creating zombie flags.
    // (See WidgetState doc.)
    let is_stashed = state.is_stashed;

    let border_box_to_window = layer_collector.transform * border_box_to_layer_transform;
    let content_to_window = border_box_to_window.pre_translate(state.border_box_translation());
    let mut viewport = paint_viewport(
        state,
        parent_viewport,
        border_box_to_window,
        content_to_window,
    );
    if !is_stashed {
        state.request_paint |= widget.set_paint_viewport(viewport, content_to_window);
    }

    // TODO - Handle damage regions
    // https://github.com/linebender/xilem/issues/789

    if is_stashed && (state.request_pre_paint || state.request_paint || state.request_post_paint) {
        // Invalidated hidden content must release cached brushes and resources.
        // Already recorded frames retain their own immutable drawing leases.
        scene_cache.remove(&id);
    }

    if (state.request_pre_paint || state.request_paint || state.request_post_paint) && !is_stashed {
        if trace {
            trace!("Painting widget '{}' {}", widget.short_type_name(), id);
        }

        let stack = property_arena.get(state.property_stack_id, widget.type_id());
        let props = PropertiesRef {
            local: properties,
            default_map: default_properties.for_widget(widget.type_id()),
            stack,
            class_set,
        };

        // TODO - Reserve scene
        // https://github.com/linebender/xilem/issues/524
        let (pre_scene, scene, post_scene) = scene_cache.entry(id).or_insert_with(|| {
            // Restoring an evicted drawing also rebuilds decorations on either side.
            state.request_pre_paint = true;
            state.request_paint = true;
            state.request_post_paint = true;
            Default::default()
        });
        for (phase, cached) in [pre_scene, scene, post_scene].into_iter().enumerate() {
            let requested = match phase {
                0 => state.request_pre_paint,
                1 => state.request_paint,
                _ => state.request_post_paint,
            };
            if !requested {
                continue;
            }
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::UiPaintRecord);
            cached.clear();
            let cursor = Cell::new(0);
            let mut ctx = PaintCtx {
                geometry: crate::app::ContextGeometry::Live,
                global_state,
                widget_state: state,
                children: children.reborrow_mut(),
                image_draws: &mut cached.images,
                command_cursor: &cursor,
            };
            let mut sink = RecordingSink {
                scene: Arc::get_mut(&mut cached.scene).expect("fresh paint has unique ownership"),
                cursor: &cursor,
            };
            let mut painter = Painter::new(&mut sink as &mut dyn PaintSink);
            match phase {
                0 => widget.pre_paint(&mut ctx, &props, &mut painter),
                1 => widget.paint(&mut ctx, &props, &mut painter),
                _ => widget.post_paint(&mut ctx, &props, &mut painter),
            }
            if phase == 0 {
                // Pre-paint can install or remove an effect in this frame. Its
                // complete source domain must reach text painting and children.
                let next = paint_viewport(
                    state,
                    parent_viewport,
                    border_box_to_window,
                    content_to_window,
                );
                if next != viewport {
                    viewport = next;
                    state.request_paint |= widget.set_paint_viewport(viewport, content_to_window);
                }
            }
        }
    }

    state.request_pre_paint = false;
    state.request_paint = false;
    state.request_post_paint = false;
    state.needs_paint = false;

    let previous_suppression = layer_collector.suppress_output;
    let suppress_output = previous_suppression
        || is_stashed
        || (state.subtree_opacity == 0.
            && state
                .subtree_blend
                .transparent_source_preserves_destination())
        || state
            .subtree_image_mask
            .as_ref()
            .is_some_and(|mask| mask.hides_all());
    layer_collector.suppress_output = suppress_output;

    let paint_layer_mode = if suppress_output {
        PaintLayerMode::Inline
    } else {
        state.paint_layer_mode
    };

    if matches!(
        paint_layer_mode,
        PaintLayerMode::IsolatedScene | PaintLayerMode::External
    ) {
        layer_collector.finish_current_layer(false);
    }

    let previous_owner_id = layer_collector.current_owner_id;
    layer_collector.current_owner_id = match paint_layer_mode {
        PaintLayerMode::Inline => previous_owner_id,
        PaintLayerMode::IsolatedScene | PaintLayerMode::External => id,
    };

    let content_box_to_layer_transform =
        border_box_to_layer_transform.pre_translate(state.border_box_translation());
    let has_clip = state.clip_path.is_some();
    let paint_as_external = paint_layer_mode == PaintLayerMode::External;
    let previous_capture = layer_collector.capture_included;
    layer_collector.capture_included &= widget.capture_included();
    let subtree = layer_collector.begin_subtree(id, previous_suppression);
    let effect_clip =
        has_clip && (state.subtree_effect.is_some() || state.subtree_mask_effect.is_some());
    if effect_clip && !suppress_output {
        layer_collector.paint(id, |scene| {
            scene.push_clip(Clip::Fill {
                transform: border_box_to_layer_transform,
                shape: Geometry::Rect(state.clip_path.unwrap()),
                fill_rule: Fill::NonZero,
            });
        });
    }
    let has_composite_group = !suppress_output
        && layer_collector.push_subtree_composite(state, content_box_to_layer_transform, id);

    if !suppress_output {
        let Some((pre_scene, scene, _)) = scene_cache.get_mut(&id) else {
            debug_panic!(
                "Error in paint pass: scene should have been cached earlier in this function."
            );
            return;
        };

        layer_collector.append(
            pre_scene,
            content_box_to_layer_transform,
            id,
            crate::frame::ImagePaint {
                color: state.image_color,
                appearance: state.image_appearance,
                resource: None,
                sampling_transform: state.image_sampling_transform,
                source_rect: state.image_source_rect,
            },
        );

        if let Some(clip) = state.clip_path.filter(|_| !effect_clip) {
            // The clip path is stored in border-box space, so need to use that transform.
            layer_collector.paint(id, |scene| {
                scene.push_clip(Clip::Fill {
                    transform: border_box_to_layer_transform,
                    shape: Geometry::Rect(clip),
                    fill_rule: Fill::NonZero,
                });
            });
        }

        if paint_as_external {
            layer_collector.finish_current_layer(false);
            layer_collector.push_external_layer(
                id,
                state.external_paint_token,
                state.content_box().size().to_rect(),
                content_box_to_layer_transform,
            );
        } else {
            layer_collector.append(
                scene,
                content_box_to_layer_transform,
                id,
                crate::frame::ImagePaint {
                    color: state.image_color,
                    appearance: state.image_appearance,
                    resource: None,
                    sampling_transform: state.image_sampling_transform,
                    source_rect: state.image_source_rect,
                },
            );
        }
    }

    let parent_state = &mut *state;
    recurse_on_children(id, widget, children, |mut node| {
        // TODO: We could skip painting children outside the parent clip path.
        // There's a few things to consider if we do:
        // - Some widgets can paint outside of their border-box.
        // - Once we implement compositor layers, we may want to paint outside of the clip path anyway in anticipation of user scrolling.
        // - We still want to reset needs_paint and request_paint flags.
        paint_widget(
            global_state,
            default_properties,
            property_arena,
            layer_collector,
            scene_cache,
            cursor_layers,
            in_cursor,
            border_box_to_layer_transform * node.item.state.compose_local_transform(),
            viewport,
            node.reborrow_mut(),
        );
        parent_state.merge_up(&mut node.item.state);
    });

    if !suppress_output {
        if global_state.debug_paint {
            let border_box_to_window = layer_collector.transform * border_box_to_layer_transform;
            layer_collector.paint_window(id, |scene| {
                // Draw the global axis aligned bounding rect of the widget
                const BORDER_WIDTH: f64 = 1.0;
                let color = get_debug_color(id.to_raw());
                let rect = state.bounding_box.inset(BORDER_WIDTH / -2.0);
                let border_style = Stroke::new(BORDER_WIDTH);
                let mut painter = Painter::new(scene);
                painter.stroke(rect, &border_style, color).draw();

                // Draw the widget's explicit baselines
                let mut draw_baseline = |baseline| {
                    let border_box = state.border_box();
                    let line = Line::new((border_box.x0, baseline), (border_box.x1, baseline));
                    let baseline_style = Stroke::new(1.0).with_dashes(0., [4.0, 4.0]);
                    painter
                        .stroke(line, &baseline_style, color)
                        .transform(border_box_to_window)
                        .draw();
                };
                if !state.first_baseline.is_nan() {
                    draw_baseline(state.first_baseline);
                }
                if !state.last_baseline.is_nan() && state.last_baseline != state.first_baseline {
                    draw_baseline(state.last_baseline);
                }
            });
        }

        if has_clip && !effect_clip {
            layer_collector.paint(id, |scene| scene.pop_clip());
        }

        let Some((_, _, post_scene)) = scene_cache.get_mut(&id) else {
            debug_panic!(
                "Error in paint pass: scene should have been cached earlier in this function."
            );
            return;
        };

        layer_collector.append(
            post_scene,
            content_box_to_layer_transform,
            id,
            crate::frame::ImagePaint {
                color: state.image_color,
                appearance: state.image_appearance,
                resource: None,
                sampling_transform: state.image_sampling_transform,
                source_rect: state.image_source_rect,
            },
        );

        if global_state.inspector_state.hovered_widget == Some(id) {
            const HOVER_FILL_COLOR: Color = Color::from_rgba8(60, 60, 250, 100);
            let rect = state.border_box();
            layer_collector.paint(id, |scene| {
                Painter::new(scene)
                    .fill(rect, HOVER_FILL_COLOR)
                    .transform(border_box_to_layer_transform)
                    .draw();
            });
        }
    }

    if has_composite_group {
        layer_collector.paint(id, |scene| scene.pop_group());
    }
    if effect_clip && !suppress_output {
        layer_collector.paint(id, |scene| scene.pop_clip());
    }

    layer_collector.end_subtree(subtree);
    if previous_capture && !layer_collector.capture_included {
        let range = layer_collector.subtrees[subtree].commands.clone();
        if !range.is_empty() {
            if let Some(last) = layer_collector.capture_exclusions.last_mut()
                && last.end == range.start
            {
                last.end = range.end;
            } else {
                layer_collector.capture_exclusions.push(range);
            }
        }
    }
    layer_collector.capture_included = previous_capture;

    if matches!(
        paint_layer_mode,
        PaintLayerMode::IsolatedScene | PaintLayerMode::External
    ) {
        layer_collector.finish_current_layer(false);
    }

    layer_collector.current_owner_id = previous_owner_id;
    layer_collector.suppress_output = previous_suppression;
}

// --- MARK: ROOT
/// See the [passes documentation](crate::doc::pass_system#render-passes).
pub(crate) fn run_paint_pass(root: &mut RenderRoot) -> VisualLayerPlan {
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::UiPaint);
    let _span = info_span!("paint").entered();

    // TODO - This is a bit of a hack until we refactor widget tree mutation.
    // This should be removed once remove_child is exclusive to MutateCtx.
    let mut scene_cache = std::mem::take(&mut root.global_state.scene_cache);

    let root_id = root.root_id();
    let layer_root_ids = root.layer_root_ids();

    let mut layers = Vec::new();
    let mut subtrees = Vec::new();
    let mut body_commands = 0;
    let mut capture_exclusions: Vec<std::ops::Range<usize>> = Vec::new();
    let mut cursor_layers = Vec::new();

    for &layer_widget_id in &layer_root_ids {
        let transform = root
            .widget_arena
            .get_state(layer_widget_id)
            .window_transform;

        let mut collector = LayerCollector::new(layer_widget_id, transform);
        paint_layer(
            root,
            &mut collector,
            &mut scene_cache,
            &mut cursor_layers,
            root_id,
            layer_widget_id,
        );
        let offset = body_commands;
        body_commands += collector.commands_before_layer + collector.current_scene.commands().len();
        for mut subtree in std::mem::take(&mut collector.subtrees) {
            subtree.commands.start += offset;
            subtree.commands.end += offset;
            subtrees.push(subtree);
        }
        for range in std::mem::take(&mut collector.capture_exclusions) {
            let range = range.start + offset..range.end + offset;
            if let Some(last) = capture_exclusions.last_mut()
                && last.end == range.start
            {
                last.end = range.end;
            } else {
                capture_exclusions.push(range);
            }
        }
        layers.extend(collector.into_layers());
    }
    let mut cursor_offset = body_commands;
    for cursor in cursor_layers {
        layers.extend(cursor.layers);
        subtrees.extend(cursor.subtrees.into_iter().map(|mut subtree| {
            subtree.commands.start += cursor_offset;
            subtree.commands.end += cursor_offset;
            subtree
        }));
        for range in cursor.capture_exclusions {
            let range = range.start + cursor_offset..range.end + cursor_offset;
            if let Some(last) = capture_exclusions.last_mut()
                && last.end == range.start
            {
                last.end = range.end;
            } else {
                capture_exclusions.push(range);
            }
        }
        cursor_offset += cursor.commands;
    }

    root.global_state.scene_cache = scene_cache;

    VisualLayerPlan {
        layers,
        subtrees,
        body_commands,
        capture_exclusions,
    }
}

/// Paint a single layer's widget subtree into `target_scene`.
///
/// This is a helper that handles the split borrows needed to access
/// `global_state`, `default_properties`, and `widget_arena` simultaneously.
fn paint_layer(
    root: &mut RenderRoot,
    layer_collector: &mut LayerCollector,
    scene_cache: &mut HashMap<WidgetId, (CachedPaint, CachedPaint, CachedPaint)>,
    cursor_layers: &mut Vec<CursorPaint>,
    root_id: WidgetId,
    layer_widget_id: WidgetId,
) {
    // Clear the LayerStack's own paint flags (its paint is a no-op).
    // This is idempotent so safe to call per-layer.
    {
        let root_node = root.widget_arena.get_node_mut(root_id);
        let state = &mut root_node.item.state;
        state.request_pre_paint = false;
        state.request_paint = false;
        state.request_post_paint = false;
        state.needs_paint = false;
    }

    // Get the layer child from the arena, then pass split borrows to paint_widget.
    let root_node = root.widget_arena.get_node_mut(root_id);
    let Some(layer_node) = root_node.children.into_item_mut(layer_widget_id) else {
        debug_panic!(
            "Error in paint pass: cannot find layer child {layer_widget_id:?} in LayerStack"
        );
        return;
    };

    let size = root
        .global_state
        .size
        .to_logical::<f64>(root.global_state.scale_factor);
    paint_widget(
        &mut root.global_state,
        &root.property_arena.default_properties,
        &root.property_arena,
        layer_collector,
        scene_cache,
        cursor_layers,
        false,
        Affine::IDENTITY,
        Rect::new(0., 0., size.width, size.height),
        layer_node,
    );
}
