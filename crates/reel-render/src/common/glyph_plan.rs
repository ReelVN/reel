//! Prepare only recorded glyphs before replay and retain their actual group ink.
use super::{GpuError, glyph, scopes::push_stack};
use reel_ui::{
    frame::{
        FrameCommand, FrameDraw, FrameDrawing, FrameGlyphRun, FrameGroup, FrameOp, InkBounds,
        RecordLocation, RenderFrame,
    },
    imaging::record::Geometry,
    kurbo::{Affine, Rect, Shape},
};

/// Backend-owned atlas preparation. Returned ink is in the glyph run's local
/// coordinates, including positioned glyphs, synthesis and raster coverage.
pub(crate) trait Prepare {
    fn reserve(&mut self, bytes: u64) -> Result<(), GpuError>;
    fn run(
        &mut self,
        drawing: &FrameDrawing,
        run: &FrameGlyphRun,
        density: f64,
        at: RecordLocation,
    ) -> Result<InkBounds, GpuError>;
}

/// Per-preparation bounds borrow immutable group identities through their owning
/// frames. Repeated uses at different raster densities are conservatively united.
#[derive(Default)]
pub(crate) struct Plan {
    groups: Vec<(usize, InkBounds)>,
}

struct Stream<'a> {
    drawing: &'a FrameDrawing,
    commands: &'a [FrameCommand],
    outer: Affine,
    offset: Option<usize>,
    transitions: bool,
}
struct Scope<'a> {
    group: Option<&'a FrameGroup>,
    outer: Affine,
    ink: InkBounds,
    measure: bool,
}

impl Plan {
    /// Mirror replay's effect-local sampling domains, retained drawings, masks
    /// and capture exclusions without allocating a target or issuing a draw.
    pub(crate) fn collect<'a>(
        frames: impl Iterator<Item = &'a RenderFrame>,
        outer: Affine,
        max_dimension: u32,
        capture: bool,
        prepare: &mut impl Prepare,
    ) -> Result<Self, GpuError> {
        let mut plan = Self::default();
        let mut streams = Vec::new();
        for frame in frames {
            push_stack(
                &mut streams,
                Stream {
                    drawing: frame.drawing(),
                    commands: frame.commands(),
                    outer,
                    offset: Some(0),
                    transitions: true,
                },
                |bytes| prepare.reserve(bytes),
            )?;
        }
        while let Some(stream) = streams.pop() {
            let mut outer = stream.outer;
            let mut scopes: Vec<Scope<'_>> = Vec::new();
            let mut next = 0;
            let mut transition = stream.drawing.transitions().partition_point(|definition| {
                definition.commands.start < stream.offset.unwrap_or(0)
            });
            loop {
                if let Some(offset) = stream.offset {
                    let at = offset + next;
                    if stream.transitions
                        && let Some(definition) = stream.drawing.transitions().get(transition)
                        && definition.commands.start == at
                    {
                        transition += 1;
                        next = definition.commands.end - offset;
                        if capture && !definition.capture_included {
                            continue;
                        }
                        // The parent's retained ink includes an older drawing;
                        // its child streams retain their own exact group ink.
                        for scope in &mut scopes {
                            scope.ink = InkBounds::Unbounded;
                        }
                        push_stack(
                            &mut streams,
                            Stream {
                                drawing: definition.previous.drawing(),
                                commands: definition.previous.drawing().commands(),
                                outer: outer
                                    * definition.parent_to_window
                                    * definition.previous.to_parent(),
                                offset: Some(0),
                                transitions: true,
                            },
                            |bytes| prepare.reserve(bytes),
                        )?;
                        push_stack(
                            &mut streams,
                            Stream {
                                drawing: stream.drawing,
                                commands: &stream.drawing.commands()[definition.commands.clone()],
                                outer,
                                offset: Some(definition.commands.start),
                                transitions: false,
                            },
                            |bytes| prepare.reserve(bytes),
                        )?;
                        continue;
                    }
                    if capture
                        && next < stream.commands.len()
                        && let Some(end) = stream.drawing.capture_exclusion_end(at)
                    {
                        next = (end - offset).min(stream.commands.len());
                        transition = stream
                            .drawing
                            .transitions()
                            .partition_point(|definition| definition.commands.start < end);
                        continue;
                    }
                }
                let Some(command) = stream.commands.get(next) else {
                    break;
                };
                next += 1;
                match &command.op {
                    FrameOp::PushClip(_) => {
                        let measure = scopes.last().is_some_and(|scope| scope.measure);
                        push_stack(
                            &mut scopes,
                            Scope {
                                group: None,
                                outer,
                                ink: InkBounds::Empty,
                                measure,
                            },
                            |bytes| prepare.reserve(bytes),
                        )?;
                    }
                    FrameOp::PushGroup(group) => {
                        push_stack(
                            &mut scopes,
                            Scope {
                                group: Some(group),
                                outer,
                                ink: InkBounds::Empty,
                                measure: matches!(group.ink_bounds, InkBounds::Unbounded),
                            },
                            |bytes| prepare.reserve(bytes),
                        )?;
                        if let Some(effect) = group.effect.as_ref().or(group.mask_effect.as_ref()) {
                            outer = super::effect_plan::source_layout(
                                effect,
                                group.effect_to_target,
                                outer,
                                max_dimension,
                                command.origin.at,
                            )?
                            .frame_to_source;
                        }
                    }
                    FrameOp::PopClip | FrameOp::PopGroup => {
                        let Some(scope) = scopes.pop() else {
                            return Err(GpuError::InvalidRecord {
                                at: command.origin.at,
                                record: "glyph preparation scope",
                            });
                        };
                        outer = scope.outer;
                        let mut ink = scope.ink;
                        if let Some(group) = scope.group {
                            if !scope.measure {
                                ink = group.ink_bounds;
                            } else if let Some(effect) =
                                group.effect.as_ref().or(group.mask_effect.as_ref())
                            {
                                ink = transformed(
                                    InkBounds::Rect(effect.bounds),
                                    group.effect_to_target,
                                );
                            } else if !group.filters.is_empty() {
                                ink = InkBounds::Unbounded;
                            }
                            if scope.measure {
                                push_stack(
                                    &mut plan.groups,
                                    (group as *const _ as usize, ink),
                                    |bytes| prepare.reserve(bytes),
                                )?;
                            }
                            if let Some(mask) = group.mask {
                                let definition = stream.drawing.masks().get(&mask.mask).ok_or(
                                    GpuError::Unsupported {
                                        at: command.origin.at,
                                        record: "missing mask",
                                    },
                                )?;
                                push_stack(
                                    &mut streams,
                                    Stream {
                                        drawing: stream.drawing,
                                        commands: &definition.commands,
                                        outer: outer * mask.transform,
                                        offset: None,
                                        transitions: false,
                                    },
                                    |bytes| prepare.reserve(bytes),
                                )?;
                            }
                        }
                        if let Some(parent) = scopes.last_mut()
                            && parent.measure
                        {
                            parent.ink = union(parent.ink, ink);
                        }
                    }
                    FrameOp::Draw(draw) => {
                        let measure = scopes.last().is_some_and(|scope| scope.measure);
                        // FrameBuild already bounded ordinary paths. Revisit their
                        // geometry only while replacing an unknown enclosing ink.
                        if !measure && !matches!(draw, FrameDraw::GlyphRun(_)) {
                            continue;
                        }
                        let ink = match draw {
                            FrameDraw::GlyphRun(run) => {
                                let density = glyph::raster_density(outer * run.transform);
                                let ink =
                                    prepare.run(stream.drawing, run, density, command.origin.at)?;
                                padding(transformed(ink, run.transform), outer)
                            }
                            FrameDraw::Fill {
                                transform, shape, ..
                            } => padding(transformed(geometry(shape), *transform), outer),
                            FrameDraw::Stroke {
                                transform,
                                shape,
                                stroke,
                                ..
                            } => {
                                let ink = match geometry(shape) {
                                    InkBounds::Rect(rect) => InkBounds::Rect(rect.inflate(
                                        stroke.width * stroke.miter_limit.max(1.),
                                        stroke.width * stroke.miter_limit.max(1.),
                                    )),
                                    ink => ink,
                                };
                                padding(transformed(ink, *transform), outer)
                            }
                            FrameDraw::BlurredRoundedRect(_) => InkBounds::Unbounded,
                        };
                        if let Some(scope) = scopes.last_mut()
                            && scope.measure
                        {
                            scope.ink = union(scope.ink, ink);
                        }
                    }
                    FrameOp::External(_) => {
                        if let Some(scope) = scopes.last_mut() {
                            scope.ink = InkBounds::Unbounded;
                        }
                    }
                    FrameOp::PushContext(_) | FrameOp::PopContext => {}
                }
            }
        }
        plan.groups.sort_unstable_by_key(|entry| entry.0);
        let mut written = 0;
        for read in 0..plan.groups.len() {
            let (key, ink) = plan.groups[read];
            if written != 0 && plan.groups[written - 1].0 == key {
                plan.groups[written - 1].1 = union(plan.groups[written - 1].1, ink);
            } else {
                plan.groups[written] = (key, ink);
                written += 1;
            }
        }
        plan.groups.truncate(written);
        Ok(plan)
    }

    pub(crate) fn group_ink(&self, group: &FrameGroup) -> InkBounds {
        let key = group as *const _ as usize;
        self.groups
            .binary_search_by_key(&key, |entry| entry.0)
            .map_or(group.ink_bounds, |index| self.groups[index].1)
    }
}

/// The same quad bounds used by both backend glyph draws; glyph transforms and
/// synthetic strokes have already been applied by the shared rasterizer.
pub(crate) fn bitmap_bounds(
    position: [f32; 2],
    offset: [f64; 2],
    size: [u32; 2],
    density: f64,
) -> Rect {
    let x = f64::from(position[0]) + offset[0] / density;
    let y = f64::from(position[1]) + offset[1] / density;
    // Mesh::quad stores these coordinates in float32 before applying the outer
    // affine. Bound those actual vertices, including large local positions.
    Rect::new(
        x as f32 as f64,
        y as f32 as f64,
        (x + f64::from(size[0]) / density) as f32 as f64,
        (y + f64::from(size[1]) / density) as f32 as f64,
    )
}

pub(crate) fn union(a: InkBounds, b: InkBounds) -> InkBounds {
    match (a, b) {
        (InkBounds::Unbounded, _) | (_, InkBounds::Unbounded) => InkBounds::Unbounded,
        (InkBounds::Empty, b) => b,
        (a, InkBounds::Empty) => a,
        (InkBounds::Rect(a), InkBounds::Rect(b)) => InkBounds::Rect(a.union(b)),
    }
}
fn geometry(shape: &Geometry) -> InkBounds {
    let rect = match shape {
        Geometry::Rect(rect) => *rect,
        Geometry::RoundedRect(rect) => rect.rect(),
        Geometry::Path(path) if path.elements().is_empty() => return InkBounds::Empty,
        Geometry::Path(path) => path.bounding_box(),
    };
    if rect.is_finite() {
        InkBounds::Rect(rect)
    } else {
        InkBounds::Unbounded
    }
}
fn transformed(ink: InkBounds, transform: Affine) -> InkBounds {
    match ink {
        InkBounds::Rect(rect) => {
            let rect = transform.transform_rect_bbox(rect);
            if rect.is_finite() {
                InkBounds::Rect(rect)
            } else {
                InkBounds::Unbounded
            }
        }
        ink => ink,
    }
}
fn padding(ink: InkBounds, outer: Affine) -> InkBounds {
    let InkBounds::Rect(rect) = ink else {
        return ink;
    };
    if outer.determinant() == 0. {
        return InkBounds::Unbounded;
    }
    let [a, b, c, d, _, _] = outer.inverse().as_coeffs();
    let rect = rect.inflate(a.abs() + c.abs(), b.abs() + d.abs());
    if rect.is_finite() {
        InkBounds::Rect(rect)
    } else {
        InkBounds::Unbounded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_bitmap_ink_keeps_negative_bearings_and_density() {
        let rect = bitmap_bounds([10., 20.], [-4., -18.], [16, 22], 2.);
        assert_eq!(rect, Rect::new(8., 11., 16., 22.));
        let transform = Affine::translate((45., -20.))
            * Affine::rotate(0.3)
            * Affine::scale_non_uniform(1.5, 0.75);
        let InkBounds::Rect(padded) = padding(
            transformed(InkBounds::Rect(rect), transform),
            Affine::scale(0.75),
        ) else {
            panic!("finite raster ink");
        };
        let expected = transform
            .transform_rect_bbox(rect)
            .inflate(1. / 0.75, 1. / 0.75);
        assert_eq!(padded, expected);

        let large = bitmap_bounds([1_000_000_000., -1_000_000_000.], [1., -1.], [90, 90], 1.);
        assert_eq!(
            large,
            Rect::new(
                1_000_000_000.,
                -1_000_000_000.,
                1_000_000_064.,
                -999_999_936.
            )
        );
    }
}

#[cfg(test)]
#[path = "glyph_plan_tests.rs"]
mod frame_tests;
