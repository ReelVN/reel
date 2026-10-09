use super::GpuError;
use lyon_tessellation::{
    FillOptions, FillTessellator, FillVertex, GeometryBuilderError, StrokeOptions,
    StrokeTessellator, StrokeVertex, VertexId,
    geometry_builder::{FillGeometryBuilder, GeometryBuilder, StrokeGeometryBuilder},
    path::{Path, math::point},
};
use masonry_core::{
    imaging::record::Geometry,
    kurbo::{Affine, PathEl, Point, Shape, Stroke},
    peniko::Fill,
};
use reel_stage::RecordLocation;

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
    max: usize,
    overflow: bool,
}
impl Mesh {
    fn push(
        &mut self,
        p: lyon_tessellation::math::Point,
    ) -> Result<VertexId, GeometryBuilderError> {
        if self.vertices.len() >= self.max {
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
        if self.indices.len() + 3 > self.max * 3 {
            self.overflow = true;
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
    max_bytes: u64,
) -> Result<Mesh, GpuError> {
    let max = (max_bytes / (size_of::<Vertex>() as u64 + 12)).min(u64::from(u32::MAX)) as usize;
    let mut mesh = Mesh {
        vertices: Vec::with_capacity(max),
        indices: Vec::with_capacity(max * 3),
        max,
        overflow: false,
    };
    let mut builder = Path::builder();
    let mut open = false;
    let elements: Box<dyn Iterator<Item = PathEl> + '_> = match shape {
        Geometry::Path(p) => Box::new(p.elements().iter().copied()),
        Geometry::Rect(r) => Box::new(r.path_elements(0.1)),
        Geometry::RoundedRect(r) => Box::new(r.path_elements(0.1)),
    };
    for e in elements {
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
        match e {
            PathEl::MoveTo(p) => {
                if open {
                    builder.end(false);
                }
                builder.begin(point(p.x as f32, p.y as f32));
                open = true;
            }
            PathEl::LineTo(p) => {
                builder.line_to(point(p.x as f32, p.y as f32));
            }
            PathEl::QuadTo(a, b) => {
                builder.quadratic_bezier_to(
                    point(a.x as f32, a.y as f32),
                    point(b.x as f32, b.y as f32),
                );
            }
            PathEl::CurveTo(a, b, c) => {
                builder.cubic_bezier_to(
                    point(a.x as f32, a.y as f32),
                    point(b.x as f32, b.y as f32),
                    point(c.x as f32, c.y as f32),
                );
            }
            PathEl::ClosePath => {
                builder.end(true);
                open = false;
            }
        }
    }
    if open {
        builder.end(false);
    }
    let path = builder.build();
    let result = if let Some(s) = stroke {
        if !s.dash_pattern.is_empty() {
            return Err(GpuError::Unsupported {
                at,
                record: "dashed stroke",
            });
        }
        let options = StrokeOptions::default()
            .with_line_width(s.width as f32)
            .with_miter_limit(s.miter_limit as f32)
            .with_line_join(match s.join {
                masonry_core::kurbo::Join::Bevel => lyon_tessellation::LineJoin::Bevel,
                masonry_core::kurbo::Join::Miter => lyon_tessellation::LineJoin::Miter,
                masonry_core::kurbo::Join::Round => lyon_tessellation::LineJoin::Round,
            })
            .with_start_cap(cap(s.start_cap))
            .with_end_cap(cap(s.end_cap));
        StrokeTessellator::new().tessellate_path(&path, &options, &mut mesh)
    } else {
        FillTessellator::new().tessellate_path(
            &path,
            &FillOptions::default().with_fill_rule(match fill {
                Fill::NonZero => lyon_tessellation::FillRule::NonZero,
                Fill::EvenOdd => lyon_tessellation::FillRule::EvenOdd,
            }),
            &mut mesh,
        )
    };
    result.map_err(|e| GpuError::Geometry {
        at,
        reason: e.to_string(),
    })?;
    if mesh.overflow {
        return Err(GpuError::Budget {
            domain: "mesh",
            requested: max_bytes + 1,
            available: max_bytes,
        });
    }
    Ok(mesh)
}
fn cap(c: masonry_core::kurbo::Cap) -> lyon_tessellation::LineCap {
    match c {
        masonry_core::kurbo::Cap::Butt => lyon_tessellation::LineCap::Butt,
        masonry_core::kurbo::Cap::Square => lyon_tessellation::LineCap::Square,
        masonry_core::kurbo::Cap::Round => lyon_tessellation::LineCap::Round,
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
    for v in &mut mesh.vertices {
        let local = Point::new(f64::from(v.position[0]), f64::from(v.position[1]));
        let p = transform * local;
        v.position = [
            (p.x * 2. / f64::from(size[0]) - 1.) as f32,
            (1. - p.y * 2. / f64::from(size[1])) as f32,
        ];
        let p = uv * local;
        v.uv = [p.x as f32, p.y as f32];
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
