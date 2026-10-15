use super::{
    GpuError,
    renderer::{Budgets, Reservation, reserve},
};
use lyon_tessellation::{
    FillOptions, FillTessellator, FillVertex, GeometryBuilderError, StrokeOptions,
    StrokeTessellator, StrokeVertex, VertexId,
    geometry_builder::{FillGeometryBuilder, GeometryBuilder, StrokeGeometryBuilder},
    path::{PathEvent, math::point},
};
use reel_core::budget::BudgetPool;
use reel_ui::frame::RecordLocation;
use reel_ui::{
    imaging::record::Geometry,
    kurbo::{Affine, PathEl, Point, Rect, Shape, Stroke},
    peniko::Fill,
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    allocation: Allocation,
    error: Option<GpuError>,
}
pub(super) struct Allocation {
    shared: BudgetPool,
    local: BudgetPool,
    limit: u64,
    max_buffer: u64,
    _tickets: Vec<Reservation>,
}
impl Allocation {
    pub(super) fn new(
        budgets: &Budgets,
        budget_scope: &Budgets,
        limit: u64,
        max_buffer: u64,
    ) -> Self {
        Self {
            shared: budgets.transient.clone(),
            local: budget_scope.transient.clone(),
            limit,
            max_buffer,
            _tickets: Vec::new(),
        }
    }
    fn grow<T>(&mut self, storage: &mut Vec<T>, additional: usize) -> Result<(), GpuError> {
        let required = storage
            .len()
            .checked_add(additional)
            .ok_or(GpuError::InvalidSize)?;
        if required <= storage.capacity() {
            return Ok(());
        }
        let stride = size_of::<T>() as u64;
        let max_elements = (self.max_buffer / stride).min(u64::from(u32::MAX)) as usize;
        if required > max_elements {
            return Err(GpuError::InvalidSize);
        }
        let available = self
            .limit
            .saturating_sub(self.local.usage().bytes)
            .min(self.limit.saturating_sub(self.shared.usage().bytes));
        let growth = storage
            .capacity()
            .max(additional)
            .min((available / stride) as usize)
            .min(max_elements - storage.capacity())
            .max(required - storage.capacity());
        self._tickets.push(reserve(
            &self.shared,
            &self.local,
            growth as u64 * stride,
            self.limit,
            "mesh scratch",
        )?);
        storage.reserve_exact(storage.capacity() + growth - storage.len());
        Ok(())
    }
}
impl Mesh {
    pub(super) fn new(allocation: Allocation) -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            allocation,
            error: None,
        }
    }
    pub(super) fn quad(&mut self, rect: Rect, uv: Rect) -> Result<(), GpuError> {
        self.allocation.grow(&mut self.vertices, 4)?;
        self.allocation.grow(&mut self.indices, 6)?;
        let start = self.vertices.len() as u32;
        for (p, t) in [
            ((rect.x0, rect.y0), (uv.x0, uv.y0)),
            ((rect.x1, rect.y0), (uv.x1, uv.y0)),
            ((rect.x1, rect.y1), (uv.x1, uv.y1)),
            ((rect.x0, rect.y1), (uv.x0, uv.y1)),
        ] {
            self.vertices.push(Vertex {
                position: [p.0 as f32, p.1 as f32],
                uv: [t.0 as f32, t.1 as f32],
                color: [1.; 4],
            });
        }
        self.indices
            .extend([start, start + 1, start + 2, start, start + 2, start + 3]);
        Ok(())
    }
    pub(super) fn image_quad(
        &mut self,
        rect: Rect,
        transform: Affine,
        uv: Affine,
        size: [u32; 2],
        alpha: f32,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        let start = self.vertices.len();
        self.quad(rect, rect)?;
        transform_vertices(
            &mut self.vertices[start..],
            transform,
            size,
            Some(uv),
            [alpha; 4],
            at,
        )
    }
    fn push(
        &mut self,
        p: lyon_tessellation::math::Point,
    ) -> Result<VertexId, GeometryBuilderError> {
        if self.error.is_some() {
            return Err(GeometryBuilderError::TooManyVertices);
        }
        if let Err(error) = self.allocation.grow(&mut self.vertices, 1) {
            self.error = Some(error);
            return Err(GeometryBuilderError::TooManyVertices);
        }
        let id = VertexId(self.vertices.len() as u32);
        self.vertices.push(Vertex {
            position: [p.x, p.y],
            uv: [0.; 2],
            color: [1.; 4],
        });
        Ok(id)
    }
}
impl GeometryBuilder for Mesh {
    fn add_triangle(&mut self, a: VertexId, b: VertexId, c: VertexId) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = self.allocation.grow(&mut self.indices, 3) {
            self.error = Some(error);
            return;
        }
        self.indices.extend([a.0, b.0, c.0]);
    }
}
impl FillGeometryBuilder for Mesh {
    fn add_fill_vertex(&mut self, v: FillVertex<'_>) -> Result<VertexId, GeometryBuilderError> {
        self.push(v.position())
    }
}
impl StrokeGeometryBuilder for Mesh {
    fn add_stroke_vertex(
        &mut self,
        v: StrokeVertex<'_, '_>,
    ) -> Result<VertexId, GeometryBuilderError> {
        self.push(v.position())
    }
}
pub fn tessellate(
    shape: &Geometry,
    fill: Fill,
    stroke: Option<&Stroke>,
    at: RecordLocation,
    allocation: Allocation,
) -> Result<Mesh, GpuError> {
    let stroke_options = stroke.map(|s| stroke_options(s, at)).transpose()?;
    let mut mesh = Mesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        allocation,
        error: None,
    };
    for e in elements(shape) {
        let valid = |p: Point| (p.x as f32).is_finite() && (p.y as f32).is_finite();
        let finite = match e {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => valid(p),
            PathEl::QuadTo(a, b) => valid(a) && valid(b),
            PathEl::CurveTo(a, b, c) => valid(a) && valid(b) && valid(c),
            PathEl::ClosePath => true,
        };
        if !finite {
            return Err(GpuError::Unsupported {
                at,
                record: "geometry outside float32 range",
            });
        }
    }
    let mut path = elements(shape);
    if let Some(stroke) = stroke {
        let period = stroke.dash_pattern.iter().sum::<f64>()
            * if stroke.dash_pattern.len().is_multiple_of(2) {
                1.
            } else {
                2.
            };
        if !period.is_finite() {
            return Err(GpuError::Unsupported {
                at,
                record: "dash period outside finite float64 range",
            });
        }
        if period > 0. {
            path = Box::new(reel_ui::kurbo::dash(
                path,
                stroke.dash_offset,
                &stroke.dash_pattern,
            ));
        }
    }
    let path = events(path);
    let result = if let Some(options) = stroke_options {
        StrokeTessellator::new().tessellate(path, &options, &mut mesh)
    } else {
        FillTessellator::new().tessellate(
            path,
            &FillOptions::default().with_fill_rule(match fill {
                Fill::NonZero => lyon_tessellation::FillRule::NonZero,
                Fill::EvenOdd => lyon_tessellation::FillRule::EvenOdd,
            }),
            &mut mesh,
        )
    };
    if let Some(error) = mesh.error.take() {
        return Err(error);
    }
    result.map_err(|e| GpuError::Geometry {
        at,
        reason: e.to_string(),
    })?;
    Ok(mesh)
}
fn elements(shape: &Geometry) -> Box<dyn Iterator<Item = PathEl> + '_> {
    match shape {
        Geometry::Path(p) => Box::new(p.elements().iter().copied()),
        Geometry::Rect(r) => Box::new(r.path_elements(0.1)),
        Geometry::RoundedRect(r) => Box::new(r.path_elements(0.1)),
    }
}
fn events(elements: impl Iterator<Item = PathEl>) -> impl Iterator<Item = PathEvent> {
    let mut first = point(0., 0.);
    let mut last = first;
    let mut open = false;
    elements
        .map(Some)
        .chain(std::iter::once(None))
        .flat_map(move |element| {
            let p = |p: Point| point(p.x as f32, p.y as f32);
            let mut preceding = None;
            if !open
                && matches!(
                    element,
                    Some(PathEl::LineTo(_) | PathEl::QuadTo(..) | PathEl::CurveTo(..))
                )
            {
                preceding = Some(PathEvent::Begin { at: first });
                last = first;
                open = true;
            }
            let event = match element {
                Some(PathEl::MoveTo(at)) => {
                    if open {
                        preceding = Some(PathEvent::End {
                            last,
                            first,
                            close: false,
                        });
                    }
                    first = p(at);
                    last = first;
                    open = true;
                    Some(PathEvent::Begin { at: first })
                }
                Some(PathEl::LineTo(to)) => {
                    let event = PathEvent::Line {
                        from: last,
                        to: p(to),
                    };
                    last = p(to);
                    Some(event)
                }
                Some(PathEl::QuadTo(ctrl, to)) => {
                    let event = PathEvent::Quadratic {
                        from: last,
                        ctrl: p(ctrl),
                        to: p(to),
                    };
                    last = p(to);
                    Some(event)
                }
                Some(PathEl::CurveTo(ctrl1, ctrl2, to)) => {
                    let event = PathEvent::Cubic {
                        from: last,
                        ctrl1: p(ctrl1),
                        ctrl2: p(ctrl2),
                        to: p(to),
                    };
                    last = p(to);
                    Some(event)
                }
                Some(PathEl::ClosePath) | None => {
                    let event = open.then_some(PathEvent::End {
                        last,
                        first,
                        close: element.is_some(),
                    });
                    open = false;
                    last = first;
                    event
                }
            };
            [preceding, event].into_iter().flatten()
        })
}
fn stroke_options(stroke: &Stroke, at: RecordLocation) -> Result<StrokeOptions, GpuError> {
    let width = stroke.width as f32;
    // A miter is at least one half-width long. Below that limit every join
    // bevels. Bevel/Round do not use this parameter, including large f64 values.
    let join = match stroke.join {
        reel_ui::kurbo::Join::Miter if stroke.miter_limit >= 1. => {
            lyon_tessellation::LineJoin::Miter
        }
        reel_ui::kurbo::Join::Miter | reel_ui::kurbo::Join::Bevel => {
            lyon_tessellation::LineJoin::Bevel
        }
        reel_ui::kurbo::Join::Round => lyon_tessellation::LineJoin::Round,
    };
    let miter_limit = if join == lyon_tessellation::LineJoin::Miter {
        stroke.miter_limit as f32
    } else {
        1.
    };
    if !width.is_finite() || stroke.width < 0. || (stroke.width != 0. && width == 0.) {
        return Err(GpuError::Unsupported {
            at,
            record: "stroke width outside finite nonnegative float32 range",
        });
    }
    if !miter_limit.is_finite() {
        return Err(GpuError::Unsupported {
            at,
            record: "miter limit outside finite float32 range",
        });
    }
    Ok(StrokeOptions::default()
        .with_line_width(width)
        .with_miter_limit(miter_limit)
        .with_line_join(join)
        .with_start_cap(cap(stroke.start_cap))
        .with_end_cap(cap(stroke.end_cap)))
}

fn cap(c: reel_ui::kurbo::Cap) -> lyon_tessellation::LineCap {
    match c {
        reel_ui::kurbo::Cap::Butt => lyon_tessellation::LineCap::Butt,
        reel_ui::kurbo::Cap::Square => lyon_tessellation::LineCap::Square,
        reel_ui::kurbo::Cap::Round => lyon_tessellation::LineCap::Round,
    }
}
pub fn transform(
    mesh: &mut Mesh,
    transform: Affine,
    size: [u32; 2],
    uv: Affine,
    color: [f32; 4],
    at: RecordLocation,
) -> Result<(), GpuError> {
    transform_mesh(mesh, transform, size, Some(uv), color, at)
}
pub(super) fn transform_glyphs(
    mesh: &mut Mesh,
    transform: Affine,
    size: [u32; 2],
    color: [f32; 4],
    at: RecordLocation,
) -> Result<(), GpuError> {
    transform_mesh(mesh, transform, size, None, color, at)
}
fn transform_mesh(
    mesh: &mut Mesh,
    transform: Affine,
    size: [u32; 2],
    uv: Option<Affine>,
    color: [f32; 4],
    at: RecordLocation,
) -> Result<(), GpuError> {
    transform_vertices(&mut mesh.vertices, transform, size, uv, color, at)
}
fn transform_vertices(
    vertices: &mut [Vertex],
    transform: Affine,
    size: [u32; 2],
    uv: Option<Affine>,
    color: [f32; 4],
    at: RecordLocation,
) -> Result<(), GpuError> {
    for v in vertices {
        let local = Point::new(f64::from(v.position[0]), f64::from(v.position[1]));
        let p = transform * local;
        v.position = [
            (p.x * 2. / f64::from(size[0]) - 1.) as f32,
            (1. - p.y * 2. / f64::from(size[1])) as f32,
        ];
        if let Some(uv) = uv {
            let p = uv * local;
            v.uv = [p.x as f32, p.y as f32];
        }
        v.color = color;
        if v.position.iter().chain(&v.uv).any(|n| !n.is_finite()) {
            return Err(GpuError::Unsupported {
                at,
                record: "transformed geometry outside float32 range",
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_ui::core::Widget;
    #[test]
    fn image_quads_preserve_order_crop_transforms_and_alpha() {
        let limits = super::super::GpuLimits::default();
        let budgets = Budgets::new(limits);
        let local = Budgets::new(limits);
        let mut mesh = Mesh::new(Allocation::new(
            &budgets,
            &local,
            limits.transient_bytes,
            1 << 20,
        ));
        let at = RecordLocation {
            fragment: 0,
            command: 0,
            mask_depth: 0,
            owner: reel_ui::widgets::Label::new("").prepare().id(),
        };
        let bounds = Rect::new(2., 4., 6., 8.);
        let uv = Affine::scale_non_uniform(0.1, 0.2) * Affine::translate((-2., -4.));
        mesh.image_quad(
            bounds,
            Affine::translate((10., 20.)),
            uv,
            [100, 100],
            0.25,
            at,
        )
        .unwrap();
        mesh.image_quad(bounds, Affine::scale(2.), uv, [100, 100], 0.75, at)
            .unwrap();
        assert_eq!(mesh.indices, [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]);
        assert_eq!(mesh.vertices[0].position, [-0.76, 0.52]);
        assert_eq!(mesh.vertices[4].position, [-0.92, 0.84]);
        assert_eq!(mesh.vertices[0].uv, [0., 0.]);
        assert_eq!(mesh.vertices[2].uv, [0.4, 0.8]);
        assert!(mesh.vertices[..4].iter().all(|v| v.color == [0.25; 4]));
        assert!(mesh.vertices[4..].iter().all(|v| v.color == [0.75; 4]));
        drop(mesh);
        assert_eq!(budgets.transient.usage().bytes, 0);
        assert_eq!(local.transient.usage().bytes, 0);
    }

    #[test]
    fn stroke_float32_boundary_has_no_assertion_or_silent_clamp() {
        let at = RecordLocation {
            fragment: 7,
            command: 3,
            mask_depth: 2,
            owner: reel_ui::widgets::Label::new("").prepare().id(),
        };
        for join in [
            reel_ui::kurbo::Join::Miter,
            reel_ui::kurbo::Join::Bevel,
            reel_ui::kurbo::Join::Round,
        ] {
            for miter in [0., 0.5, 1. - f64::EPSILON, 1., 1e300] {
                let stroke = Stroke::new(5.).with_join(join).with_miter_limit(miter);
                let result = std::panic::catch_unwind(|| stroke_options(&stroke, at));
                let result = result.expect("GPU boundary must never panic");
                if join == reel_ui::kurbo::Join::Miter && miter == 1e300 {
                    assert!(
                        matches!(result, Err(GpuError::Unsupported { at: location, .. }) if location == at)
                    );
                } else {
                    let options = result.unwrap();
                    assert_eq!(options.miter_limit, 1.);
                    if join == reel_ui::kurbo::Join::Miter && miter < 1. {
                        assert_eq!(options.line_join, lyon_tessellation::LineJoin::Bevel);
                    }
                }
            }
        }
        for width in [-1., -f64::MIN_POSITIVE, f64::MIN_POSITIVE, 1e300, f64::NAN] {
            assert!(
                matches!(stroke_options(&Stroke::new(width), at), Err(GpuError::Unsupported { at: location, .. }) if location == at)
            );
        }
        assert!(stroke_options(&Stroke::new(0.), at).is_ok());
    }
}
