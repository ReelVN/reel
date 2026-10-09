use super::GpuError;
use lyon_tessellation::{
    FillOptions, FillTessellator, FillVertex, GeometryBuilderError, StrokeOptions,
    StrokeTessellator, StrokeVertex, VertexId,
    geometry_builder::{FillGeometryBuilder, GeometryBuilder, StrokeGeometryBuilder},
    path::{Path, math::point},
};
use reel_ui::frame::RecordLocation;
use reel_ui::{
    imaging::record::Geometry,
    kurbo::{Affine, PathEl, Point, Shape, Stroke},
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
    // Stage accepts nonnegative f64 miter limits. Lyon's ordinary assertion
    // requires >= 1 even for Bevel/Round, so reject at this narrower boundary.
    let stroke_options = stroke.map(|s| stroke_options(s, at)).transpose()?;
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
    let result = if let Some(options) = stroke_options {
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
fn stroke_options(stroke: &Stroke, at: RecordLocation) -> Result<StrokeOptions, GpuError> {
    let width = stroke.width as f32;
    let miter_limit = stroke.miter_limit as f32;
    if !width.is_finite() || stroke.width < 0. || (stroke.width != 0. && width == 0.) {
        return Err(GpuError::Unsupported {
            at,
            record: "stroke width outside finite nonnegative float32 range",
        });
    }
    if !miter_limit.is_finite() || stroke.miter_limit < 1. {
        return Err(GpuError::Unsupported {
            at,
            record: "Lyon stroke miter limit must be finite float32 and at least 1",
        });
    }
    if !stroke.dash_pattern.is_empty() {
        return Err(GpuError::Unsupported {
            at,
            record: "dashed stroke",
        });
    }
    Ok(StrokeOptions::default()
        .with_line_width(width)
        .with_miter_limit(miter_limit)
        .with_line_join(match stroke.join {
            reel_ui::kurbo::Join::Bevel => lyon_tessellation::LineJoin::Bevel,
            reel_ui::kurbo::Join::Miter => lyon_tessellation::LineJoin::Miter,
            reel_ui::kurbo::Join::Round => lyon_tessellation::LineJoin::Round,
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use reel_ui::core::Widget;
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
            for miter in [0., 0.5, 1. - f64::EPSILON, 1., 1e300, f64::NAN] {
                let stroke = Stroke::new(5.).with_join(join).with_miter_limit(miter);
                let result = std::panic::catch_unwind(|| stroke_options(&stroke, at));
                let result = result.expect("GPU boundary must never panic");
                if miter == 1. {
                    let options = result.unwrap();
                    assert_eq!(options.miter_limit, 1.);
                } else {
                    assert!(
                        matches!(result, Err(GpuError::Unsupported { at: location, .. }) if location == at)
                    );
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
