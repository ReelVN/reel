// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0
//! Cached paint and typed image bindings. Ordinary paints allocate no binding storage.
use crate::{
    app::{CommandOwnerRange, GroupMetadata, LayerMetadata},
    core::WidgetId,
    frame::ImagePaint,
    imaging::{self, PaintSink, record::Scene},
    peniko::ImageSampler,
    resources::ResourceLease,
};
use std::{cell::Cell, sync::Arc};

#[derive(Default)]
pub(crate) struct CachedPaint {
    pub scene: Arc<Scene>,
    pub images: Vec<ResourceDraw>,
    metadata: Option<Arc<LayerMetadata>>,
    metadata_valid: bool,
}
pub(crate) struct ResourceDraw {
    pub command: usize,
    pub resource: ResourceLease,
    pub sampler: ImageSampler,
    pub source_rect: Option<crate::kurbo::Rect>,
}
impl CachedPaint {
    pub fn clear(&mut self) {
        if let Some(scene) = Arc::get_mut(&mut self.scene) {
            scene.clear();
        } else {
            self.scene = Arc::new(Scene::new());
        }
        self.images.clear();
        self.metadata = None;
        self.metadata_valid = false;
    }
    pub fn metadata(&mut self, owner: WidgetId, image: ImagePaint) -> Option<Arc<LayerMetadata>> {
        if self.metadata_valid
            && self
                .metadata
                .as_ref()
                .map_or_else(|| image.is_default(), |metadata| metadata.image == image)
        {
            return self.metadata.clone();
        }
        let groups = self
            .scene
            .commands()
            .iter()
            .enumerate()
            .filter_map(|(command, op)| {
                matches!(op, imaging::record::Command::PushGroup(_)).then_some(GroupMetadata {
                    command,
                    owner,
                    effect_transform: crate::kurbo::Affine::IDENTITY,
                    image_mask: None,
                    effect: None,
                    mask_effect: None,
                    invert: 0.,
                    blend: None,
                })
            })
            .collect::<Vec<_>>();
        let mut command_owners = Vec::new();
        let mut next = 0;
        for binding in &self.images {
            record_owner_range(
                &mut command_owners,
                next,
                binding.command,
                owner,
                image,
                None,
            );
            record_owner_range(
                &mut command_owners,
                binding.command,
                binding.command + 1,
                owner,
                ImagePaint {
                    resource: Some(crate::resources::ResourceImageBrush {
                        resource: binding.resource.handle(),
                        sampler: binding.sampler,
                    }),
                    sampling_transform: true,
                    source_rect: binding.source_rect,
                    ..image
                },
                Some(&binding.resource),
            );
            next = binding.command + 1;
        }
        if !self.images.is_empty() || !image.is_default() {
            record_owner_range(
                &mut command_owners,
                next,
                self.scene.commands().len(),
                owner,
                image,
                None,
            );
        }
        self.metadata = (!groups.is_empty() || !command_owners.is_empty()).then(|| {
            Arc::new(LayerMetadata {
                groups,
                command_owners,
                image,
            })
        });
        self.metadata_valid = true;
        self.metadata.clone()
    }
}
pub(crate) fn record_owner_range(
    ranges: &mut Vec<CommandOwnerRange>,
    start: usize,
    end: usize,
    owner: WidgetId,
    image: ImagePaint,
    lease: Option<&ResourceLease>,
) {
    let crate::frame::ImagePaint {
        color: image_color,
        appearance: image_appearance,
        resource: image_resource,
        sampling_transform: image_sampling_transform,
        source_rect: image_source_rect,
    } = image;
    if start == end {
        return;
    }
    if let Some(last) = ranges.last_mut()
        && last.owner == owner
        && last.image_color == image_color
        && last.image_appearance == image_appearance
        && last.image_resource == image_resource
        && last.image_source_rect == image_source_rect
        && last.image_sampling_transform == image_sampling_transform
        && last.commands.end == start
    {
        last.commands.end = end;
    } else {
        ranges.push(CommandOwnerRange {
            commands: start..end,
            owner,
            image_color,
            image_appearance,
            image_resource,
            image_lease: lease.cloned(),
            image_sampling_transform,
            image_source_rect,
        });
    }
}
/// The scene sink emits exactly one top-level command per forwarded operation.
/// Its cursor is shared by reference with PaintCtx; no Rc or per-command lookup.
pub(crate) struct RecordingSink<'a> {
    pub scene: &'a mut Scene,
    pub cursor: &'a Cell<usize>,
}
impl RecordingSink<'_> {
    fn advance(&self) {
        self.cursor.set(self.cursor.get() + 1);
    }
}
impl PaintSink for RecordingSink<'_> {
    fn push_context(&mut self, c: imaging::ContextRef<'_>) {
        PaintSink::push_context(self.scene, c);
        self.advance();
    }
    fn pop_context(&mut self) {
        PaintSink::pop_context(self.scene);
        self.advance();
    }
    fn push_clip(&mut self, c: imaging::ClipRef<'_>) {
        PaintSink::push_clip(self.scene, c);
        self.advance();
    }
    fn pop_clip(&mut self) {
        PaintSink::pop_clip(self.scene);
        self.advance();
    }
    fn push_group(&mut self, g: imaging::GroupRef<'_>) {
        PaintSink::push_group(self.scene, g);
        self.advance();
    }
    fn pop_group(&mut self) {
        PaintSink::pop_group(self.scene);
        self.advance();
    }
    fn fill(&mut self, d: imaging::FillRef<'_>) {
        PaintSink::fill(self.scene, d);
        self.advance();
    }
    fn stroke(&mut self, d: imaging::StrokeRef<'_>) {
        PaintSink::stroke(self.scene, d);
        self.advance();
    }
    fn glyph_run(
        &mut self,
        d: imaging::GlyphRunRef<'_>,
        glyphs: &mut dyn Iterator<Item = imaging::record::Glyph>,
    ) {
        PaintSink::glyph_run(self.scene, d, glyphs);
        self.advance();
    }
    fn blurred_rounded_rect(&mut self, d: imaging::BlurredRoundedRect) {
        PaintSink::blurred_rounded_rect(self.scene, d);
        self.advance();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repaint_replaces_shared_scene_and_preserves_pending_recordings() {
        let mut cached = CachedPaint::default();
        let rect = crate::kurbo::Rect::new(0., 0., 10., 10.);
        imaging::Painter::new(Arc::get_mut(&mut cached.scene).unwrap())
            .fill_rect(rect, crate::peniko::Color::WHITE);
        let old = cached.scene.clone();
        let old_lease = Arc::downgrade(&old);
        cached.clear();
        imaging::Painter::new(Arc::get_mut(&mut cached.scene).unwrap())
            .fill_rect(rect, crate::peniko::Color::BLACK);
        assert!(!Arc::ptr_eq(&old, &cached.scene));
        let imaging::record::Command::Draw(id) = old.commands()[0] else {
            panic!()
        };
        let imaging::record::Draw::Fill { brush, .. } = old.draw_op(id) else {
            panic!()
        };
        assert_eq!(
            brush,
            &crate::peniko::Brush::Solid(crate::peniko::Color::WHITE)
        );
        drop(cached);
        assert!(
            old_lease.upgrade().is_some(),
            "recording survives producer retirement"
        );
        drop(old);
        assert!(
            old_lease.upgrade().is_none(),
            "last recording releases its scene"
        );
    }
    #[test]
    fn ordinary_paint_counts_commands_without_allocating_resource_bindings() {
        let mut cached = CachedPaint::default();
        let cursor = Cell::new(0);
        {
            let mut sink = RecordingSink {
                scene: Arc::get_mut(&mut cached.scene).unwrap(),
                cursor: &cursor,
            };
            let mut painter = imaging::Painter::new(&mut sink);
            painter.with_fill_clip(crate::kurbo::Rect::new(0., 0., 10., 10.), |p| {
                p.fill_rect(
                    crate::kurbo::Rect::new(1., 1., 2., 2.),
                    crate::peniko::Color::WHITE,
                )
            });
        }
        assert_eq!(cursor.get(), cached.scene.commands().len());
        assert_eq!(cached.images.capacity(), 0);
    }
}
