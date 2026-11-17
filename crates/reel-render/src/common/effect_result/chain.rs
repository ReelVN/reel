//! Resolve an existing same-grid effect chain without materializing its inputs.
use super::*;
use crate::common::{effect_plan, scissor::Scissor, scopes::GroupPlan};
use reel_ui::frame::FrameGroup;
use reel_ui::stage::effect::EffectType;

#[cfg(test)]
mod tests;

struct Layer<'a> {
    effect: &'a FrameEffect,
    grid: Grid,
    footprint: Option<reel_ui::resources::EffectSourceFootprint>,
}

// Author nesting uses an explicit, accounted stack rather than the Rust stack.
struct Layers<'a> {
    values: Vec<Layer<'a>>,
    storage: BudgetTicket,
}
impl<'a> Layers<'a> {
    fn new(pool: &BudgetPool) -> Option<Self> {
        Some(Self {
            values: Vec::new(),
            storage: pool.reserve(0).ok()?,
        })
    }
    fn push(&mut self, layer: Layer<'a>) -> Option<()> {
        if self.values.len() == self.values.capacity() {
            let old = self.values.capacity();
            let next = old.max(1).checked_mul(2)?;
            let bytes = old.checked_add(next)?.checked_mul(size_of::<Layer<'a>>())?;
            self.storage.resize(bytes as u64).ok()?;
            self.values
                .try_reserve_exact(next - self.values.len())
                .ok()?;
            self.storage
                .resize(self.values.capacity().checked_mul(size_of::<Layer<'a>>())? as u64)
                .ok()?;
        }
        self.values.push(layer);
        Some(())
    }
}

fn group<'a>(drawing: &'a FrameDrawing, range: &Range<usize>) -> Option<&'a FrameGroup> {
    let commands = drawing.commands().get(range.clone())?;
    let FrameOp::PushGroup(group) = &commands.first()?.op else {
        return None;
    };
    matches!(commands.last()?.op, FrameOp::PopGroup).then_some(group)
}

fn sole_child(
    drawing: &FrameDrawing,
    range: &Range<usize>,
    groups: &GroupPlan,
) -> Option<Range<usize>> {
    let mut child = range.start.checked_add(1)?;
    while matches!(
        drawing.commands().get(child)?.op,
        FrameOp::PushContext(_) | FrameOp::PopContext
    ) {
        child += 1;
    }
    if !groups.is_source_child(child)
        || !matches!(drawing.commands().get(child)?.op, FrameOp::PushGroup(_))
    {
        return None;
    }
    // The source-child proof leaves only contexts between this child's closing
    // group and its parent's close. Walking that suffix avoids rescanning every
    // nested subtree at each layer of a deep chain.
    let mut end = range.end.checked_sub(1)?;
    while matches!(
        drawing.commands().get(end.checked_sub(1)?)?.op,
        FrameOp::PushContext(_) | FrameOp::PopContext
    ) {
        end -= 1;
    }
    if end <= child || !matches!(drawing.commands().get(end - 1)?.op, FrameOp::PopGroup) {
        return None;
    }
    Some(child..end)
}

fn invocation(
    drawing: &FrameDrawing,
    at: usize,
    effect: &FrameEffect,
    grid: Grid,
) -> Option<(Grid, Option<reel_ui::resources::EffectSourceFootprint>)> {
    let shader =
        effect_plan::shader(drawing, effect, drawing.commands().get(at)?.origin.at).ok()?;
    Some((
        if shader.descriptor.output == EffectType::Vec4 && grid.effect_context.is_none() {
            grid.color_context(effect)
        } else {
            grid
        },
        shader.descriptor.source_footprint,
    ))
}

/// Prove the root effect's source structure before looking up inner identities.
///
/// The caller has already proved a forwarded source on its original coordinate
/// grid. A physical backing window keeps the canonical raster identity. Only direct,
/// exclusive effect children on the inherited raster qualify; other bodies keep
/// ordinary replay. Each resolved identity names an existing RGBA8 pass, while
/// all pixel ownership and terminal compositing remain with the caller.
pub(crate) fn source<'a>(
    drawing: &'a FrameDrawing,
    commands: Range<usize>,
    grid: Grid,
    groups: &GroupPlan,
    storage_size: &dyn Fn([u32; 2]) -> [u32; 2],
    pool: &BudgetPool,
) -> Option<SourceProof<'a>> {
    if crate::common::scopes::intersects_transition(drawing, commands.clone()) {
        return None;
    }
    let mut range = commands;
    let mut grid = grid;
    let mut layers = Layers::new(pool)?;
    let result = loop {
        let parent = group(drawing, &range)?;
        let effect = parent.effect.as_ref()?;
        let (invocation, footprint) = invocation(drawing, range.start, effect, grid)?;
        grid = invocation;
        if let Some(raw) = Probe::new(drawing, range.clone(), effect, grid, true) {
            break raw.with_source_demand(footprint);
        }
        let child_range = sole_child(drawing, &range, groups)?;
        let child = group(drawing, &child_range)?;
        if !effect_plan::same_source_domain(child, Some(parent)) {
            return None;
        }
        let transform = Affine::new(grid.source_transform.map(f64::from_bits));
        let source = effect_plan::SourceLayout {
            size: grid.source_size,
            frame_to_source: transform,
            effect_to_source: effect_plan::local_transform(
                effect,
                parent.effect_to_target,
                grid.source_size,
            ),
        };
        if !effect_plan::forwards_source(child, &source, transform, grid.source_size, false) {
            return None;
        }
        let storage = storage_size(grid.source_size);
        if storage != grid.source_storage {
            return None;
        }
        let [x, y, width, height] = grid.source_scissor;
        let inherited = Scissor {
            x,
            y,
            width,
            height,
        };
        let output = Scissor::group_bounds(child, transform, grid.source_size, false)
            .map_or(inherited, |bounds| inherited.intersect(bounds));
        let effect_transform = effect_plan::output_transform(
            child,
            Some((parent, source.effect_to_source)),
            transform,
        );
        let input = effect_plan::roi::source_scissor(
            drawing,
            child,
            effect_transform,
            grid.source_size,
            output,
            source.size,
            storage,
        );
        let (source_transform, effect_transform) = Grid::transforms(transform, effect_transform);
        let inner = Grid {
            source_transform,
            effect_transform,
            source_size: source.size,
            source_storage: storage,
            source_scissor: [input.x, input.y, input.width, input.height],
            output_size: grid.source_size,
            output_storage: storage,
            scissor: [output.x, output.y, output.width, output.height],
            quality: grid.quality,
            effect_context: None,
        };
        layers.push(Layer {
            effect,
            grid,
            footprint,
        })?;
        range = child_range;
        grid = inner;
    };
    Some(SourceProof { result, layers })
}

/// One completed structural/raster proof, independent of submitted metadata.
pub(crate) struct SourceProof<'a> {
    result: Probe<'a>,
    layers: Layers<'a>,
}
impl<'a> SourceProof<'a> {
    pub(crate) fn grid(&self) -> Grid {
        let mut grid = self
            .layers
            .values
            .first()
            .map_or(self.result.grid, |layer| layer.grid);
        grid.source_transform = Affine::IDENTITY.as_coeffs().map(f64::to_bits);
        grid
    }
    pub(crate) fn resolve<T: Clone>(mut self, cache: &Cache<T>) -> Option<Probe<'a>> {
        while let Some(layer) = self.layers.values.pop() {
            let token = cache.lookup_content(&self.result)?;
            self.result = Probe::from_output(token, layer.effect, layer.grid)
                .with_source_demand(layer.footprint);
        }
        Some(self.result)
    }
}

/// A proved source can miss submitted content while retaining its reuse boundary.
#[derive(Default)]
pub(crate) struct Lookup {
    pub hit: bool,
    pub early_reusable: bool,
}

#[cfg(test)]
pub(crate) fn probe<'a, T: Clone>(
    cache: &Cache<T>,
    drawing: &'a FrameDrawing,
    commands: Range<usize>,
    grid: Grid,
    groups: &GroupPlan,
    storage_size: &dyn Fn([u32; 2]) -> [u32; 2],
    pool: &BudgetPool,
) -> Option<Probe<'a>> {
    source(drawing, commands, grid, groups, storage_size, pool)?.resolve(cache)
}
