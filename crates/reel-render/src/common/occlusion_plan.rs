//! Bounded, backend-independent removal of completely overwritten draws.
use super::{
    GpuError,
    renderer::{Budgets, Reservation, reserve},
    scissor::Scissor,
    scopes::GroupPlan,
};
use reel_core::texture::TextureFormat;
use reel_ui::{
    frame::{FrameCommand, FrameDraw, FrameDrawing, FrameGroup, FrameOp, RenderFrame},
    imaging::record::Geometry,
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend},
    resources::ResourceHandle,
};

const RECT_LIMIT: usize = 32;
const EMPTY: Scissor = Scissor {
    x: 0,
    y: 0,
    width: 0,
    height: 0,
};

struct Row {
    drawing: usize,
    hidden: Vec<usize>,
    _storage: Reservation,
}
#[derive(Default)]
pub(crate) struct Plan {
    rows: Vec<Row>,
    _storage: Option<Reservation>,
}
impl Plan {
    pub(crate) fn collect<'a>(
        frames: impl Iterator<Item = &'a RenderFrame> + Clone,
        size: [u32; 2],
        capture: bool,
        budgets: &Budgets,
        local: &Budgets,
        limit: u64,
    ) -> Result<Self, GpuError> {
        if capture {
            return Ok(Self::default());
        }
        // Only metadata admission is optional. No rendering error is swallowed.
        match Self::try_collect(frames, size, budgets, local, limit) {
            Err(GpuError::Budget { .. } | GpuError::Pressure { .. }) => Ok(Self::default()),
            result => result,
        }
    }
    fn try_collect<'a>(
        frames: impl Iterator<Item = &'a RenderFrame> + Clone,
        size: [u32; 2],
        budgets: &Budgets,
        local: &Budgets,
        limit: u64,
    ) -> Result<Self, GpuError> {
        let Some(first) = frames.clone().next() else {
            return Ok(Self::default());
        };
        // Replay uses the candidate's first-frame transform for both crossfade
        // streams. Demand selection must prove coverage in that same grid.
        let outer = Affine::scale_non_uniform(
            f64::from(size[0]) / first.info().logical_size[0],
            f64::from(size[1]) / first.info().logical_size[1],
        );
        let capacity = frames.clone().count();
        let storage = reserve(
            &budgets.transient,
            &local.transient,
            (capacity * size_of::<Row>()) as u64,
            limit,
            "occlusion plan",
        )?;
        let mut rows = Vec::with_capacity(capacity);
        for frame in frames {
            let drawing = frame.drawing();
            if !drawing.transitions().is_empty()
                || !drawing.capture_exclusions().is_empty()
                || rows
                    .iter()
                    .any(|row: &Row| row.drawing == drawing as *const _ as usize)
            {
                continue;
            }
            if let Some(row) = plan_drawing(frame, outer, size, budgets, local, limit)? {
                rows.push(row);
            }
        }
        if rows.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self {
            rows,
            _storage: Some(storage),
        })
    }
    pub(crate) fn selected(&self, drawing: &FrameDrawing, command: usize) -> bool {
        self.rows
            .iter()
            .find(|row| row.drawing == drawing as *const _ as usize)
            .is_none_or(|row| row.hidden.binary_search(&command).is_err())
    }
    pub(crate) fn visible_end(&self, drawing: &FrameDrawing, start: usize, end: usize) -> usize {
        self.rows
            .iter()
            .find(|row| row.drawing == drawing as *const _ as usize)
            .and_then(|row| {
                row.hidden
                    .get(row.hidden.partition_point(|&index| index < start))
                    .copied()
            })
            .map_or(end, |next| next.min(end))
    }
}

// One growable ticket owns all temporary vectors and GroupPlan stack capacity.
// GroupPlan's reserve callback reports each allocation increment.
fn grow(
    storage: &mut Reservation,
    bytes: &mut u64,
    additional: u64,
    limit: u64,
) -> Result<(), GpuError> {
    *bytes = bytes.checked_add(additional).ok_or(GpuError::InvalidSize)?;
    storage
        ._local
        .resize(*bytes)
        .map_err(|_| GpuError::Budget {
            domain: "occlusion scratch",
            requested: *bytes,
            available: limit,
        })?;
    storage
        ._shared
        .as_mut()
        .unwrap()
        .resize(*bytes)
        .map_err(|_| GpuError::Pressure {
            domain: "occlusion scratch",
            requested: *bytes,
            available: limit,
        })
}
fn plan_drawing(
    frame: &RenderFrame,
    outer: Affine,
    size: [u32; 2],
    budgets: &Budgets,
    local: &Budgets,
    limit: u64,
) -> Result<Option<Row>, GpuError> {
    let drawing = frame.drawing();
    let commands = drawing.commands();
    if commands.is_empty() {
        return Ok(None);
    }
    let mut bytes = (commands.len()
        * (size_of::<Option<(usize, Scissor)>>() + size_of::<Scissor>() + size_of::<bool>()))
        as u64;
    let mut scratch = reserve(
        &budgets.transient,
        &local.transient,
        bytes,
        limit,
        "occlusion scratch",
    )?;
    let leases = drawing.resources().leases();
    grow(
        &mut scratch,
        &mut bytes,
        (leases.len() * size_of::<ResourceHandle>()) as u64,
        limit,
    )?;
    let mut opaque_images = Vec::with_capacity(leases.len());
    for lease in leases {
        if lease
            .image_coverage()
            .is_some_and(|coverage| coverage.is_opaque())
            || lease
                .data()
                .texture_descriptor()
                .is_some_and(|desc| desc.format == TextureFormat::Bc1Rgb)
        {
            opaque_images.push(lease.handle());
        }
    }
    opaque_images.sort_unstable();
    let groups = GroupPlan::new(
        commands,
        Some(0),
        std::iter::empty(),
        std::iter::empty(),
        |additional| grow(&mut scratch, &mut bytes, additional, limit),
    )?;
    grow(
        &mut scratch,
        &mut bytes,
        commands.len().div_ceil(8) as u64,
        limit,
    )?;
    let mut hidden = vec![0u8; commands.len().div_ceil(8)];
    let mut events = Vec::with_capacity(commands.len());
    let mut clips = Vec::with_capacity(commands.len());
    let mut isolated = Vec::with_capacity(commands.len());
    let mut scissor = Scissor::full(size);
    let mut next = 0;
    while let Some(command) = commands.get(next) {
        let at = next;
        next += 1;
        match &command.op {
            FrameOp::Draw(draw) if ordinary(draw) => events.push(Some((at, scissor))),
            FrameOp::PushClip(clip) => {
                if let Some(rect) = Scissor::from_clip(clip, outer, size, 4) {
                    clips.push(scissor);
                    scissor = scissor.intersect(rect);
                } else {
                    events.push(None);
                    next = scope_end(commands, at).unwrap_or(commands.len());
                }
            }
            FrameOp::PopClip => scissor = clips.pop().unwrap_or(Scissor::full(size)),
            FrameOp::PushGroup(group) if !groups.can_inline(at) => {
                // Replay folds this exact single draw and its scissors into the
                // parent. Reuse that proof for ordinary clipped image widgets.
                if group.composite.alpha == 1.
                    && let Some(fill) =
                        super::scopes::inline_group_fill(group, &commands[next..], outer, size, 4)
                {
                    events.push(Some((next + fill.command, scissor.intersect(fill.scissor))));
                    next += fill.consumed;
                } else if parent_pixel_grid(group) {
                    // Ordinary isolation only translates to a cropped pixel
                    // grid (or keeps the full parent grid). Analyze its body
                    // independently; no coverage crosses either composite.
                    events.push(None);
                    isolated.push(true);
                } else {
                    events.push(None);
                    next = scope_end(commands, at).unwrap_or(commands.len());
                }
            }
            FrameOp::PushGroup(_) => isolated.push(false),
            FrameOp::PopGroup => {
                if isolated.pop().unwrap_or(false) {
                    events.push(None);
                }
            }
            FrameOp::PushContext(_) | FrameOp::PopContext => {}
            _ => events.push(None),
        }
    }
    let mut coverage = Coverage::default();
    let mut any = false;
    for event in events.into_iter().rev() {
        let Some((index, clip)) = event else {
            coverage.len = 0;
            continue;
        };
        let FrameOp::Draw(draw) = &commands[index].op else {
            unreachable!()
        };
        let bounds = draw_rect(draw, outer)
            .and_then(|(rect, aligned)| pixels(rect, size, false, aligned))
            .unwrap_or(clip)
            .intersect(clip);
        if coverage.covers(bounds) {
            hidden[index / 8] |= 1 << (index % 8);
            any = true;
        } else if opaque(draw, &opaque_images)
            && let Some((rect, true)) = draw_rect(draw, outer)
            && let Some(rect) = pixels(rect, size, true, true)
        {
            coverage.insert(rect.intersect(clip));
        }
    }
    if !any {
        return Ok(None);
    }
    let count: usize = hidden.iter().map(|bits| bits.count_ones() as usize).sum();
    let storage = reserve(
        &budgets.transient,
        &local.transient,
        (count * size_of::<usize>()) as u64,
        limit,
        "occlusion selection",
    )?;
    let mut selected = Vec::with_capacity(count);
    for index in 0..commands.len() {
        if hidden[index / 8] & (1 << (index % 8)) != 0 {
            selected.push(index);
        }
    }
    Ok(Some(Row {
        drawing: drawing as *const _ as usize,
        hidden: selected,
        _storage: storage,
    }))
}
fn parent_pixel_grid(group: &FrameGroup) -> bool {
    // These sources use SourceView::group's integer crop or the unchanged
    // parent extent in both replay backends; no shader-local resampling occurs.
    group.composite.blend == Default::default()
        && group.clip.is_none()
        && group.mask.is_none()
        && group.image_mask.is_none()
        && group.effect.is_none()
        && group.mask_effect.is_none()
        && group.filters.is_empty()
        && group.invert == 0.
}
fn ordinary(draw: &FrameDraw) -> bool {
    match draw {
        FrameDraw::Fill { composite, .. } | FrameDraw::Stroke { composite, .. } => {
            composite.blend == Default::default()
        }
        FrameDraw::GlyphRun(run) => run.composite.blend == Default::default(),
        FrameDraw::Sprites(sprites) => sprites.composite.blend == Default::default(),
        FrameDraw::BlurredRoundedRect(_) => false,
    }
}
fn opaque(draw: &FrameDraw, images: &[ResourceHandle]) -> bool {
    let FrameDraw::Fill {
        composite,
        brush,
        brush_transform,
        shape: Geometry::Rect(rect),
        ..
    } = draw
    else {
        return false;
    };
    if composite.alpha != 1. {
        return false;
    }
    match brush {
        Brush::Solid(color) => color.components[3] == 1.,
        Brush::Image(brush) => {
            if brush.image.color[3] != 1.
                || brush.sampler.alpha != 1.
                || brush.image.appearance.is_some()
                || brush.image.source_rect.is_some()
                || brush.sampler.x_extend != Extend::Pad
                || brush.sampler.y_extend != Extend::Pad
            {
                return false;
            }
            // Keep author sampling arithmetic finite after conversion to GPU f32.
            let source = brush_transform
                .unwrap_or(Affine::IDENTITY)
                .transform_rect_bbox(*rect);
            if ![source.x0, source.y0, source.x1, source.y1]
                .into_iter()
                .all(|x| (x as f32).is_finite())
            {
                return false;
            }
            images.binary_search(&brush.image.resource).is_ok()
        }
        _ => false,
    }
}
fn draw_rect(draw: &FrameDraw, outer: Affine) -> Option<(Rect, bool)> {
    let FrameDraw::Fill {
        shape: Geometry::Rect(rect),
        transform,
        ..
    } = draw
    else {
        return None;
    };
    let transform = outer * *transform;
    let [a, b, c, d, _, _] = transform.as_coeffs();
    let rect = transform.transform_rect_bbox(*rect);
    Some((rect, (b == 0. && c == 0.) || (a == 0. && d == 0.)))
}
// Integer edges preserve the same complete pixels for 1x and 4x. Fractional
// occluder edges discard the boundary pixels; queried bounds enclose them.
fn pixels(rect: Rect, size: [u32; 2], inner: bool, aligned: bool) -> Option<Scissor> {
    if ![rect.x0, rect.y0, rect.x1, rect.y1]
        .into_iter()
        .all(|x| x.is_finite() && x.abs() < 1_048_576.)
    {
        return None;
    }
    let edge = |x: f64, extent: u32, start: bool| {
        let x = x.clamp(0., f64::from(extent));
        if aligned && x.fract() == 0. {
            return x as u32;
        }
        let inset = if aligned { 1. / 16. } else { 1. };
        let x = if inner == start {
            (x + inset).ceil()
        } else {
            (x - inset).floor()
        };
        x.clamp(0., f64::from(extent)) as u32
    };
    let [x, y, x1, y1] = [
        edge(rect.x0, size[0], true),
        edge(rect.y0, size[1], true),
        edge(rect.x1, size[0], false),
        edge(rect.y1, size[1], false),
    ];
    Some(Scissor {
        x,
        y,
        width: x1.saturating_sub(x),
        height: y1.saturating_sub(y),
    })
}
fn scope_end(commands: &[FrameCommand], start: usize) -> Option<usize> {
    let mut depth = 1;
    for (index, command) in commands.iter().enumerate().skip(start + 1) {
        match command.op {
            FrameOp::PushGroup(_) | FrameOp::PushClip(_) => depth += 1,
            FrameOp::PopGroup | FrameOp::PopClip => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

struct Coverage {
    rectangles: [Scissor; RECT_LIMIT],
    len: usize,
}
impl Default for Coverage {
    fn default() -> Self {
        Self {
            rectangles: [EMPTY; RECT_LIMIT],
            len: 0,
        }
    }
}
impl Coverage {
    fn insert(&mut self, rect: Scissor) {
        if rect.empty()
            || self.rectangles[..self.len]
                .iter()
                .any(|&old| old.intersect(rect) == rect)
        {
            return;
        }
        let mut next = 0;
        for index in 0..self.len {
            let old = self.rectangles[index];
            if old.intersect(rect) != old {
                self.rectangles[next] = old;
                next += 1;
            }
        }
        // Dropping coverage loses only optimization, never invents opaque area.
        self.len = if next == RECT_LIMIT { 0 } else { next };
        self.rectangles[self.len] = rect;
        self.len += 1;
    }
    fn covers(&self, query: Scissor) -> bool {
        if query.empty() {
            return true;
        }
        let mut overlap = false;
        for &rect in &self.rectangles[..self.len] {
            let hit = rect.intersect(query);
            if hit == query {
                return true;
            }
            overlap |= !hit.empty();
        }
        // Most ordinary UI rectangles are disjoint. Avoid fragment scratch
        // initialization unless more than one opaque rectangle can contribute.
        if !overlap {
            return false;
        }
        let mut remaining = [EMPTY; RECT_LIMIT];
        remaining[0] = query;
        let mut count = 1;
        for &rect in &self.rectangles[..self.len] {
            if rect.intersect(query).empty() {
                continue;
            }
            let mut next = [EMPTY; RECT_LIMIT];
            let mut used = 0;
            for &part in &remaining[..count] {
                let hit = part.intersect(rect);
                let pieces = if hit.empty() {
                    [part, EMPTY, EMPTY, EMPTY]
                } else {
                    [
                        Scissor {
                            width: hit.x - part.x,
                            ..part
                        },
                        Scissor {
                            x: hit.x + hit.width,
                            width: part.x + part.width - hit.x - hit.width,
                            ..part
                        },
                        Scissor {
                            x: hit.x,
                            width: hit.width,
                            height: hit.y - part.y,
                            ..part
                        },
                        Scissor {
                            x: hit.x,
                            y: hit.y + hit.height,
                            width: hit.width,
                            height: part.y + part.height - hit.y - hit.height,
                        },
                    ]
                };
                for piece in pieces.into_iter().filter(|p| !p.empty()) {
                    if used == RECT_LIMIT {
                        return false;
                    }
                    next[used] = piece;
                    used += 1;
                }
            }
            if used == 0 {
                return true;
            }
            remaining = next;
            count = used;
        }
        false
    }
}

#[cfg(test)]
#[path = "occlusion_plan_tests.rs"]
mod tests;
