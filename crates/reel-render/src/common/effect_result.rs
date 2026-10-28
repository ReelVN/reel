#![allow(
    dead_code,
    reason = "effect replay integration is being developed in the next slice"
)]
//! Content identity for already-materialized ordinary effect outputs.
use reel_core::budget::{BudgetPool, BudgetTicket};
#[cfg(test)]
mod tests;
use reel_ui::{
    frame::{FrameDraw, FrameDrawing, FrameEffect, FrameOp, LocalId},
    imaging::record::Scene,
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend, ImageQuality},
    resources::ResourceHandle,
    stage::effect::EffectValue,
};
use std::{ops::Range, sync::Arc};

fn affine(value: Affine) -> [u64; 6] {
    value.as_coeffs().map(f64::to_bits)
}
fn rect(value: Rect) -> [u64; 4] {
    [
        value.x0.to_bits(),
        value.y0.to_bits(),
        value.x1.to_bits(),
        value.y1.to_bits(),
    ]
}

/// Exact raster grid. Values must describe the actual effect invocation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Grid {
    pub source_transform: [u64; 6],
    pub effect_transform: [u64; 6],
    pub source_size: [u32; 2],
    pub source_storage: [u32; 2],
    pub output_size: [u32; 2],
    pub output_storage: [u32; 2],
    pub scissor: [u32; 4],
    pub quality: [u32; 2],
}
impl Grid {
    pub(crate) fn transforms(source: Affine, effect: Affine) -> ([u64; 6], [u64; 6]) {
        (affine(source), affine(effect))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ImageProjection {
    resource: ResourceHandle,
    color: [u32; 4],
    source_rect: Option<[u64; 4]>,
    x_extend: Extend,
    y_extend: Extend,
    quality: ImageQuality,
    alpha: u32,
    transform: [u64; 6],
    brush_transform: Option<[u64; 6]>,
}
struct Source {
    scene: Arc<Scene>,
    local: LocalId,
    image: ImageProjection,
}
impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.scene, &other.scene)
            && self.local == other.local
            && self.image == other.image
    }
}
impl Eq for Source {}

/// Borrowed probe. No serialized command or copied parameter/texture vectors are
/// built on a hit. The scene is upgraded to prevent identity reuse during lookup.
pub(crate) struct Probe<'a> {
    source: Source,
    effect: &'a FrameEffect,
    grid: Grid,
}
impl<'a> Probe<'a> {
    pub(crate) fn new(
        drawing: &FrameDrawing,
        commands: Range<usize>,
        effect: &'a FrameEffect,
        grid: Grid,
    ) -> Option<Self> {
        if !drawing.transitions().is_empty() || !drawing.masks().is_empty() {
            return None;
        }
        let commands = drawing.commands().get(commands)?;
        if !matches!(commands.first()?.op, FrameOp::PushGroup(_))
            || !matches!(commands.last()?.op, FrameOp::PopGroup)
        {
            return None;
        }
        let mut source = None;
        for command in &commands[1..commands.len() - 1] {
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => {}
                FrameOp::Draw(FrameDraw::Fill {
                    transform,
                    brush_transform,
                    brush: Brush::Image(brush),
                    ..
                }) if source.is_none() => {
                    if brush.image.appearance.is_some() {
                        return None;
                    }
                    let (recording, local) = drawing.recording(command.origin)?;
                    if !recording.mask_path().is_empty() {
                        return None;
                    }
                    source = Some(Source {
                        scene: recording.root().upgrade()?,
                        local,
                        image: ImageProjection {
                            resource: brush.image.resource,
                            color: brush.image.color.map(f32::to_bits),
                            source_rect: brush.image.source_rect.map(rect),
                            x_extend: brush.sampler.x_extend,
                            y_extend: brush.sampler.y_extend,
                            quality: brush.sampler.quality,
                            alpha: brush.sampler.alpha.to_bits(),
                            transform: affine(*transform),
                            brush_transform: brush_transform.map(affine),
                        },
                    });
                }
                _ => return None,
            }
        }
        Some(Self {
            source: source?,
            effect,
            grid,
        })
    }
}

pub(crate) struct Key {
    source: Source,
    definition: [u8; 32],
    shader: ResourceHandle,
    values: Vec<EffectValue>,
    images: Vec<ResourceHandle>,
    bounds: [u64; 4],
    coverage_invert: bool,
    grid: Grid,
    _storage: BudgetTicket,
}
impl Key {
    pub(crate) fn matches(&self, probe: &Probe<'_>) -> bool {
        self.source == probe.source
            && self.definition == probe.effect.definition
            && self.shader == probe.effect.shader
            && self.values == probe.effect.values
            && self.images == probe.effect.images
            && self.bounds == rect(probe.effect.bounds)
            && self.coverage_invert == probe.effect.coverage_invert
            && self.grid == probe.grid
    }
    /// Optional cache admission: failure leaves the ordinary render path intact.
    pub(crate) fn new(probe: Probe<'_>, pool: &BudgetPool) -> Option<Self> {
        let bytes = size_of::<Self>()
            .checked_add(
                probe
                    .effect
                    .values
                    .len()
                    .checked_mul(size_of::<EffectValue>())?,
            )?
            .checked_add(
                probe
                    .effect
                    .images
                    .len()
                    .checked_mul(size_of::<ResourceHandle>())?,
            )?;
        let storage = pool.reserve(bytes as u64).ok()?;
        Some(Self {
            source: probe.source,
            definition: probe.effect.definition,
            shader: probe.effect.shader,
            values: probe.effect.values.clone(),
            images: probe.effect.images.clone(),
            bounds: rect(probe.effect.bounds),
            coverage_invert: probe.effect.coverage_invert,
            grid: probe.grid,
            _storage: storage,
        })
    }
}
