// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0
//! Cached paint and typed image bindings. Ordinary paints allocate no binding storage.
use crate::{
    imaging::{self, PaintSink, record::Scene},
    peniko::ImageSampler,
    resources::ResourceLease,
};
use std::{cell::Cell, sync::Arc};

#[derive(Default)]
pub(crate) struct CachedPaint {
    pub scene: Arc<Scene>,
    pub images: Vec<ResourceDraw>,
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
