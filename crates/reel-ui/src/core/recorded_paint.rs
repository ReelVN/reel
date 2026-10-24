// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0
//! Cached paint and typed image bindings. Ordinary paints allocate no binding storage.
use crate::{
    imaging::{self, PaintSink, record::Scene},
    peniko::ImageSampler,
    resources::ResourceLease,
};
use std::cell::Cell;

#[derive(Default)]
pub(crate) struct CachedPaint {
    pub scene: Scene,
    pub images: Vec<ResourceDraw>,
}
pub(crate) struct ResourceDraw {
    pub command: usize,
    pub resource: ResourceLease,
    pub sampler: ImageSampler,
}
impl CachedPaint {
    pub fn clear(&mut self) {
        self.scene.clear();
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
    fn ordinary_paint_counts_commands_without_allocating_resource_bindings() {
        let mut cached = CachedPaint::default();
        let cursor = Cell::new(0);
        {
            let mut sink = RecordingSink {
                scene: &mut cached.scene,
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
