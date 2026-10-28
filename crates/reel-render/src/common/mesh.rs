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
    /// Normalized origin and signed span; a negative span repeats that axis.
    /// Zero span selects the ordinary whole-texture sampler.
    pub source_window: [f32; 4],
}
/// Axis-aligned geometry clip in the same float32 clip space as draw vertices.
/// Single-sample rasterization keeps this binary, so ordinary draws can be
/// clipped individually without changing their source-over composition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RectClip {
    bounds: [f32; 4],
}
impl RectClip {
    pub(crate) fn from_clip(
        clip: &reel_ui::imaging::record::Clip,
        outer: Affine,
        size: [u32; 2],
    ) -> Option<Self> {
        let reel_ui::imaging::record::Clip::Fill {
            transform,
            shape: Geometry::Rect(rect),
            ..
        } = clip
        else {
            return None;
        };
        let transform = outer * *transform;
        let [_, b, c, _, _, _] = transform.as_coeffs();
        if b != 0. || c != 0. {
            return None;
        }
        // Tessellation stores local coordinates in float32 before the common
        // transform; use that same conversion for the rectangle's own edges.
        let rect = Rect::new(
            rect.x0 as f32 as f64,
            rect.y0 as f32 as f64,
            rect.x1 as f32 as f64,
            rect.y1 as f32 as f64,
        );
        let rect = transform.transform_rect_bbox(rect);
        let bounds = [
            (rect.x0 * 2. / f64::from(size[0]) - 1.) as f32,
            (1. - rect.y1 * 2. / f64::from(size[1])) as f32,
            (rect.x1 * 2. / f64::from(size[0]) - 1.) as f32,
            (1. - rect.y0 * 2. / f64::from(size[1])) as f32,
        ];
        bounds
            .iter()
            .all(|v| v.is_finite())
            .then_some(Self { bounds })
    }
    fn contains(self, vertex: &Vertex) -> bool {
        (0..2).all(|axis| {
            vertex.position[axis] >= self.bounds[axis]
                && vertex.position[axis] <= self.bounds[axis + 2]
        })
    }
}
/// Per-quad sampling data keeps different windows in the same texture batch.
#[derive(Clone, Copy, Default, PartialEq)]
pub(crate) struct ImageSampling {
    window: [f32; 4],
}
impl ImageSampling {
    pub(crate) fn with_layout(
        rect: Option<Rect>,
        layout: reel_core::texture::TextureLayout,
        repeat: [bool; 2],
    ) -> Self {
        let rect = rect.or_else(|| {
            (layout.content_size != layout.storage_size).then(|| {
                Rect::from_origin_size(
                    (0., 0.),
                    (
                        f64::from(layout.logical_size[0]),
                        f64::from(layout.logical_size[1]),
                    ),
                )
            })
        });
        let Some(rect) = rect else {
            return Self::default();
        };
        let scale = layout.uv_scale();
        Self {
            window: [
                (rect.x0 * scale[0]) as f32,
                (rect.y0 * scale[1]) as f32,
                (rect.width() * scale[0]) as f32 * if repeat[0] { -1. } else { 1. },
                (rect.height() * scale[1]) as f32 * if repeat[1] { -1. } else { 1. },
            ],
        }
    }
    #[cfg(test)]
    pub(crate) fn new(rect: Option<Rect>, size: [u32; 2], repeat: [bool; 2]) -> Self {
        let Some(rect) = rect else {
            return Self::default();
        };
        Self {
            window: [
                (rect.x0 / f64::from(size[0])) as f32,
                (rect.y0 / f64::from(size[1])) as f32,
                (rect.width() / f64::from(size[0])) as f32 * if repeat[0] { -1. } else { 1. },
                (rect.height() / f64::from(size[1])) as f32 * if repeat[1] { -1. } else { 1. },
            ],
        }
    }
    fn apply(self, vertices: &mut [Vertex]) {
        for vertex in vertices {
            vertex.source_window = self.window;
        }
    }
}
pub(crate) struct ImagePaint {
    pub(crate) color: [f32; 4],
    pub(crate) sampling: ImageSampling,
}
/// Sampling follows the actual paint source; a solid-colored glyph still samples
/// its atlas and therefore uses `Texture`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaintSource {
    Solid,
    Texture,
}
/// Fixed paint programs preserve solid color, coverage and block-alpha semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum DirectPaint {
    Rgba,
    Bc1,
    Bc3,
    RegionalRgba,
    RegionalBc1,
    RegionalBc3,
    Coverage,
    Solid,
}
/// Fixed paint shaders whose source and interpolated vertex alpha are opaque.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum OpaquePaint {
    Bc1,
    RegionalBc1,
    Solid,
}
pub struct Mesh {
    pub(crate) bounded_color: super::precision::BoundedColor,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    allocation: Allocation,
    error: Option<GpuError>,
    pub(crate) identity: Option<u64>,
    pub(crate) clip_identity: Option<RectClip>,
}
pub(crate) struct Allocation {
    shared: BudgetPool,
    local: BudgetPool,
    limit: u64,
    max_buffer: u64,
    ticket: Option<Reservation>,
}
impl Allocation {
    pub(crate) fn fresh(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            local: self.local.clone(),
            limit: self.limit,
            max_buffer: self.max_buffer,
            ticket: None,
        }
    }
    pub(crate) fn borrow_cached(
        &self,
        bytes: u64,
    ) -> Result<reel_core::budget::BudgetTicket, GpuError> {
        self.local.reserve(bytes).map_err(|_| GpuError::Budget {
            domain: "mesh scratch",
            requested: bytes,
            available: self.limit.saturating_sub(self.local.usage().bytes),
        })
    }
    pub(crate) fn new(
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
            ticket: None,
        }
    }
    fn resize(&mut self, bytes: u64) -> Result<(), GpuError> {
        let Some(ticket) = &mut self.ticket else {
            self.ticket = Some(reserve(
                &self.shared,
                &self.local,
                bytes,
                self.limit,
                "mesh scratch",
            )?);
            return Ok(());
        };
        let previous = ticket._local.bytes();
        if bytes == previous {
            return Ok(());
        }
        let requested = bytes.saturating_sub(previous);
        ticket._local.resize(bytes).map_err(|_| GpuError::Budget {
            domain: "mesh scratch",
            requested,
            available: self.limit.saturating_sub(self.local.usage().bytes),
        })?;
        if ticket
            ._shared
            .as_mut()
            .expect("mesh retains both reservations")
            .resize(bytes)
            .is_err()
        {
            ticket
                ._local
                .resize(previous)
                .expect("restoring a smaller reservation cannot exhaust its budget");
            return Err(GpuError::Pressure {
                domain: "mesh scratch",
                requested,
                available: self.limit.saturating_sub(self.shared.usage().bytes),
            });
        }
        Ok(())
    }
    pub(crate) fn grow<T>(
        &mut self,
        storage: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), GpuError> {
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
            .min(((available / stride) as usize).saturating_sub(storage.capacity()))
            .min(max_elements - storage.capacity())
            .max(required - storage.capacity());
        let previous = self
            .ticket
            .as_ref()
            .map_or(0, |ticket| ticket._local.bytes());
        let old_bytes = storage.capacity() as u64 * stride;
        let capacity = storage.capacity() + growth;
        // A relocating reserve can keep the old and new allocation alive at
        // once. Retain that peak before allocation, then charge actual capacity.
        self.resize(
            previous
                .checked_add(capacity as u64 * stride)
                .ok_or(GpuError::InvalidSize)?,
        )?;
        storage.reserve_exact(capacity - storage.len());
        self.resize(previous - old_bytes + storage.capacity() as u64 * stride)?;
        Ok(())
    }
}
impl Mesh {
    pub(crate) fn cache_copy(&self) -> Self {
        Self {
            bounded_color: self.bounded_color,
            vertices: self.vertices.clone(),
            indices: self.indices.clone(),
            error: None,
            identity: self.identity,
            clip_identity: self.clip_identity,
            allocation: Allocation {
                shared: self.allocation.shared.clone(),
                local: self.allocation.shared.clone(),
                limit: self.allocation.limit,
                max_buffer: self.allocation.max_buffer,
                ticket: None,
            },
        }
    }
    pub(crate) fn scratch_allocation(&self) -> Allocation {
        self.allocation.fresh()
    }
    /// Solid glyph color uses the atlas's linear coverage channel directly.
    /// Gradient or adjusted brushes retain their existing color operations.
    pub(crate) fn direct_coverage(
        &self,
        appearance: Option<&[[f32; 4]; 9]>,
    ) -> Option<DirectPaint> {
        (self.direct_paint(PaintSource::Texture, [false; 2], appearance, None)
            == Some(DirectPaint::Rgba))
        .then_some(DirectPaint::Coverage)
    }
    /// Prove opacity after clipping and before batching. Vertex alpha includes
    /// authored opacity and analytic edge coverage; multisample coverage stays
    /// with the rasterizer and remains equivalent under per-sample replacement.
    pub(crate) fn opaque_paint(&self, direct: Option<DirectPaint>) -> Option<OpaquePaint> {
        let paint = match direct? {
            DirectPaint::Bc1 => OpaquePaint::Bc1,
            DirectPaint::RegionalBc1 => OpaquePaint::RegionalBc1,
            DirectPaint::Solid => OpaquePaint::Solid,
            _ => return None,
        };
        self.vertices
            .iter()
            .all(|vertex| vertex.color[3] == 1.)
            .then_some(paint)
    }
    /// Classify before batching: solid paint uses its prepared vertex color;
    /// texture draws retain their own source windows. Zero windows delegate edge
    /// clamping to the sampler; regional windows keep the half-texel clamp.
    pub(crate) fn direct_paint(
        &self,
        source: PaintSource,
        repeat: [bool; 2],
        appearance: Option<&[[f32; 4]; 9]>,
        format: Option<reel_core::texture::TextureFormat>,
    ) -> Option<DirectPaint> {
        if repeat != [false; 2]
            || appearance.is_some_and(|values| {
                !matches!(values[4][3], 0. | 1.) || values[5][3] != 0. || values[7][3] != 0.
            })
        {
            return None;
        }
        if source == PaintSource::Solid {
            return Some(DirectPaint::Solid);
        }
        let regional = self
            .vertices
            .first()
            .is_some_and(|v| v.source_window != [0.; 4]);
        if self.vertices.iter().any(|v| {
            if regional {
                v.source_window[2] <= 0. || v.source_window[3] <= 0.
            } else {
                v.source_window != [0.; 4]
            }
        }) {
            return None;
        }
        use reel_core::texture::TextureFormat::{Bc1Rgb, Bc3Rgba};
        Some(match (regional, format) {
            (false, Some(Bc1Rgb)) => DirectPaint::Bc1,
            (false, Some(Bc3Rgba)) => DirectPaint::Bc3,
            (false, _) => DirectPaint::Rgba,
            (true, Some(Bc1Rgb)) => DirectPaint::RegionalBc1,
            (true, Some(Bc3Rgba)) => DirectPaint::RegionalBc3,
            (true, _) => DirectPaint::RegionalRgba,
        })
    }
    /// Append already transformed geometry without changing primitive order.
    #[cfg(test)]
    pub(crate) fn append(&mut self, mesh: &Self) -> Result<std::ops::Range<u32>, GpuError> {
        let vertex_start = u32::try_from(self.vertices.len()).map_err(|_| GpuError::InvalidSize)?;
        let start = u32::try_from(self.indices.len()).map_err(|_| GpuError::InvalidSize)?;
        let end = self
            .indices
            .len()
            .checked_add(mesh.indices.len())
            .filter(|&n| n <= i32::MAX as usize)
            .ok_or(GpuError::InvalidSize)? as u32;
        if self
            .vertices
            .len()
            .checked_add(mesh.vertices.len())
            .is_none_or(|n| n > u32::MAX as usize)
        {
            return Err(GpuError::InvalidSize);
        }
        self.allocation
            .grow(&mut self.vertices, mesh.vertices.len())?;
        self.allocation
            .grow(&mut self.indices, mesh.indices.len())?;
        self.bounded_color = if vertex_start == 0 {
            mesh.bounded_color
        } else {
            self.bounded_color.intersect(mesh.bounded_color)
        };
        self.vertices.extend_from_slice(&mesh.vertices);
        self.indices
            .extend(mesh.indices.iter().map(|&index| vertex_start + index));
        Ok(start..end)
    }
    pub(crate) fn bytes(&self) -> usize {
        self.vertices.len() * size_of::<Vertex>() + self.indices.len() * size_of::<u32>()
    }
    /// Clip triangles and linearly interpolate every authored vertex attribute.
    /// The fixed polygon scratch has at most seven vertices; output growth is
    /// reserved through the same transient ledger as ordinary tessellation.
    pub(crate) fn clipped(
        &self,
        clip: RectClip,
        allocation: Allocation,
    ) -> Result<Option<Self>, GpuError> {
        if self.vertices.iter().all(|vertex| clip.contains(vertex)) {
            return Ok(None);
        }
        let mut output = Self::new(allocation);
        output.bounded_color = self.bounded_color;
        output.identity = self.identity;
        output.clip_identity = Some(clip);
        for triangle in self.indices.as_chunks::<3>().0 {
            let first = self.vertices[triangle[0] as usize];
            let mut polygon = [first; 8];
            for (vertex, index) in polygon.iter_mut().zip(triangle) {
                *vertex = self.vertices[*index as usize];
            }
            let mut count = 3;
            for (edge, &bound) in clip.bounds.iter().enumerate() {
                if count == 0 {
                    break;
                }
                let axis = edge % 2;
                let inside = |vertex: Vertex| {
                    if edge < 2 {
                        vertex.position[axis] >= bound
                    } else {
                        vertex.position[axis] <= bound
                    }
                };
                let input = polygon;
                let mut previous = input[count - 1];
                let mut written = 0;
                for current in input.into_iter().take(count) {
                    if inside(previous) != inside(current)
                        && previous.position[axis] != bound
                        && current.position[axis] != bound
                    {
                        // Canonical endpoint order makes adjacent triangles
                        // produce bit-identical shared-edge intersections.
                        let (a, b) = if previous.position[axis] < current.position[axis] {
                            (previous, current)
                        } else {
                            (current, previous)
                        };
                        // Finite float32 endpoints can still have an overflowing
                        // float32 difference when geometry extends far offscreen.
                        let t = (f64::from(bound) - f64::from(a.position[axis]))
                            / (f64::from(b.position[axis]) - f64::from(a.position[axis]));
                        let lerp = |a: f32, b: f32| {
                            (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as f32
                        };
                        let mut vertex = Vertex {
                            position: std::array::from_fn(|i| lerp(a.position[i], b.position[i])),
                            uv: std::array::from_fn(|i| lerp(a.uv[i], b.uv[i])),
                            color: std::array::from_fn(|i| lerp(a.color[i], b.color[i])),
                            source_window: std::array::from_fn(|i| {
                                lerp(a.source_window[i], b.source_window[i])
                            }),
                        };
                        vertex.position[axis] = bound;
                        polygon[written] = vertex;
                        written += 1;
                    }
                    if inside(current) {
                        polygon[written] = current;
                        written += 1;
                    }
                    previous = current;
                }
                count = written;
            }
            if count < 3 {
                continue;
            }
            output.allocation.grow(&mut output.vertices, count)?;
            output
                .allocation
                .grow(&mut output.indices, (count - 2) * 3)?;
            let start = output.vertices.len() as u32;
            output.vertices.extend_from_slice(&polygon[..count]);
            for index in 1..count - 1 {
                output
                    .indices
                    .extend([start, start + index as u32, start + index as u32 + 1]);
            }
        }
        Ok(Some(output))
    }
    pub(crate) fn new(allocation: Allocation) -> Self {
        Self {
            bounded_color: super::precision::BoundedColor::UNKNOWN,
            vertices: Vec::new(),
            indices: Vec::new(),
            allocation,
            error: None,
            identity: None,
            clip_identity: None,
        }
    }
    pub(crate) fn triangles(&mut self, vertices: &[Vertex]) -> Result<(), GpuError> {
        self.indexed_triangles(vertices, 0..vertices.len() as u32)
    }
    /// Append trusted triangle indices with shared vertices from an internal planner.
    pub(crate) fn indexed_triangles(
        &mut self,
        vertices: &[Vertex],
        indices: impl ExactSizeIterator<Item = u32>,
    ) -> Result<(), GpuError> {
        self.allocation.grow(&mut self.vertices, vertices.len())?;
        self.allocation.grow(&mut self.indices, indices.len())?;
        let start = self.vertices.len() as u32;
        let mut proof = if self.vertices.is_empty() {
            super::precision::BoundedColor::new([1.; 4])
        } else {
            self.bounded_color
        };
        for vertex in vertices {
            proof = proof.intersect(super::precision::BoundedColor::new(vertex.color));
        }
        self.bounded_color = proof;
        self.vertices.extend_from_slice(vertices);
        self.indices.extend(indices.map(|index| start + index));
        Ok(())
    }
    pub(crate) fn quad(&mut self, rect: Rect, uv: Rect) -> Result<(), GpuError> {
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
                source_window: [0.; 4],
            });
        }
        self.indices
            .extend([start, start + 1, start + 2, start, start + 2, start + 3]);
        Ok(())
    }
    pub(crate) fn image_quad(
        &mut self,
        rect: Rect,
        transform: Affine,
        uv: Affine,
        size: [u32; 2],
        paint: ImagePaint,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        let start = self.vertices.len();
        self.quad(rect, rect)?;
        let proof = super::precision::BoundedColor::new(paint.color);
        self.bounded_color = if start == 0 {
            proof
        } else {
            self.bounded_color.intersect(proof)
        };
        paint.sampling.apply(&mut self.vertices[start..]);
        transform_vertices(
            &mut self.vertices[start..],
            transform,
            size,
            Some(uv),
            paint.color,
            at,
        )
    }
    pub(crate) fn set_image_sampling(&mut self, sampling: ImageSampling) {
        sampling.apply(&mut self.vertices);
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
            source_window: [0.; 4],
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
    let mut mesh = Mesh::new(allocation);
    if let (Geometry::Rect(rect), None) = (shape, stroke) {
        let [mut x0, mut y0, mut x1, mut y1] =
            [rect.x0, rect.y0, rect.x1, rect.y1].map(|v| v as f32);
        if ![x0, y0, x1, y1].into_iter().all(f32::is_finite) {
            return Err(GpuError::Unsupported {
                at,
                record: "geometry outside float32 range",
            });
        }
        // Keep Lyon's exact vertex order, diagonal and winding so transformed
        // UVs and rectangle clipping see the same mesh as the path route.
        // Degenerate rectangles retain that route's vertex/error behavior.
        if x0 != x1 && y0 != y1 {
            if x0 > x1 {
                std::mem::swap(&mut x0, &mut x1);
            }
            if y0 > y1 {
                std::mem::swap(&mut y0, &mut y1);
            }
            mesh.allocation.grow(&mut mesh.vertices, 4)?;
            mesh.allocation.grow(&mut mesh.indices, 6)?;
            mesh.vertices.extend(
                [[x0, y0], [x1, y0], [x0, y1], [x1, y1]].map(|position| Vertex {
                    position,
                    uv: [0.; 2],
                    color: [1.; 4],
                    source_window: [0.; 4],
                }),
            );
            mesh.indices.extend([1, 0, 2, 1, 2, 3]);
            return Ok(mesh);
        }
    }
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
pub(crate) fn transform_glyphs(
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
    mesh.bounded_color = super::precision::BoundedColor::new(color);
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

    #[test]
    fn bounded_color_proof_follows_images_cache_clip_and_mixed_batches() {
        let limits = super::super::GpuLimits::default();
        let shared = Budgets::new(limits);
        let local = Budgets::new(limits);
        let allocation = || Allocation::new(&shared, &local, limits.transient_bytes, 1 << 20);
        let at = RecordLocation {
            fragment: 0,
            command: 0,
            mask_depth: 0,
            owner: reel_ui::widgets::Label::new("").prepare().id(),
        };
        let mut mesh = Mesh::new(allocation());
        let bounded = |mesh: &Mesh| {
            mesh.bounded_color.paint(
                Some(DirectPaint::Rgba),
                Some(reel_core::texture::TextureFormat::Rgba8),
            )
        };
        for color in [[0.5; 4], [1.; 4], [1.1; 4]] {
            mesh.image_quad(
                Rect::new(0., 0., 10., 10.),
                Affine::IDENTITY,
                Affine::IDENTITY,
                [10; 2],
                ImagePaint {
                    color,
                    sampling: ImageSampling::new(None, [10; 2], [false; 2]),
                },
                at,
            )
            .unwrap();
            assert_eq!(bounded(&mesh), color[0] <= 1.);
            assert_eq!(bounded(&mesh.cache_copy()), bounded(&mesh));
        }
        transform(
            &mut mesh,
            Affine::IDENTITY,
            [10; 2],
            Affine::IDENTITY,
            [0.5; 4],
            at,
        )
        .unwrap();
        assert!(bounded(&mesh));
        let clipped = mesh
            .clipped(
                RectClip {
                    bounds: [-0.95, -0.95, -0.85, -0.85],
                },
                allocation(),
            )
            .unwrap()
            .unwrap();
        assert!(bounded(&clipped));
        let mut other = Mesh::new(allocation());
        other
            .triangles(
                &[Vertex {
                    color: [2.; 4],
                    ..mesh.vertices[0]
                }; 3],
            )
            .unwrap();
        mesh.append(&other).unwrap();
        assert!(!bounded(&mesh));
    }

    #[test]
    fn mesh_growth_charges_relocation_peak_and_rolls_back_both_ledgers() {
        let limits = super::super::GpuLimits {
            transient_bytes: 600,
            ..Default::default()
        };
        let shared = Budgets::new(limits);
        let local = Budgets::new(limits);
        let mut mesh = Mesh::new(Allocation::new(&shared, &local, 600, 1 << 20));
        mesh.quad(Rect::new(0., 0., 4., 3.), Rect::ZERO).unwrap();
        let original = bytemuck::cast_slice::<_, u8>(&mesh.vertices).to_vec();
        let pointer = mesh.vertices.as_ptr();
        assert_eq!(shared.transient.usage().bytes, 216);
        assert_eq!(local.transient.usage().tickets, 1);

        // Other scratch in this preparation is a local admission failure.
        let local_pressure = local.transient.reserve(1).unwrap();
        assert!(matches!(
            mesh.quad(Rect::new(5., 0., 9., 3.), Rect::ZERO),
            Err(GpuError::Budget { requested: 384, .. })
        ));
        assert_eq!(shared.transient.usage().bytes, 216);
        assert_eq!(local.transient.usage().bytes, 217);
        assert_eq!(mesh.vertices.as_ptr(), pointer);
        assert_eq!(bytemuck::cast_slice::<_, u8>(&mesh.vertices), original);
        drop(local_pressure);

        let pressure = shared.transient.reserve(1).unwrap();
        assert!(matches!(
            mesh.quad(Rect::new(5., 0., 9., 3.), Rect::ZERO),
            Err(GpuError::Pressure { requested: 384, .. })
        ));
        assert_eq!(shared.transient.usage().bytes, 217);
        assert_eq!(local.transient.usage().bytes, 216);
        assert_eq!(local.transient.usage().tickets, 1);
        assert_eq!(mesh.vertices.as_ptr(), pointer);
        assert_eq!(bytemuck::cast_slice::<_, u8>(&mesh.vertices), original);
        drop(pressure);

        mesh.quad(Rect::new(5., 0., 9., 3.), Rect::ZERO).unwrap();
        for usage in [shared.transient.usage(), local.transient.usage()] {
            assert_eq!(usage.bytes, 432);
            assert_eq!(usage.peak_bytes, 600, "216 old + 384 replacement vertices");
            assert_eq!(usage.tickets, 1);
        }
        drop(mesh);
        assert_eq!(shared.transient.usage().bytes, 0);
        assert_eq!(local.transient.usage().bytes, 0);
    }

    #[test]
    fn rectangle_fill_matches_path_mesh_through_transforms_and_clipping() {
        let limits = super::super::GpuLimits::default();
        let shared = Budgets::new(limits);
        let local = Budgets::new(limits);
        let allocation = || Allocation::new(&shared, &local, limits.transient_bytes, 1 << 20);
        let at = RecordLocation {
            fragment: 2,
            command: 7,
            mask_depth: 0,
            owner: reel_ui::widgets::Label::new("").prepare().id(),
        };
        let equal = |a: &Mesh, b: &Mesh| {
            assert_eq!(a.indices, b.indices);
            assert_eq!(
                bytemuck::cast_slice::<_, u8>(&a.vertices),
                bytemuck::cast_slice::<_, u8>(&b.vertices),
            );
        };
        for rect in [
            Rect::new(0., 0., 30., 20.),
            Rect::new(30., 0., 0., 20.),
            Rect::new(0., 20., 30., 0.),
            Rect::new(30., 20., 0., 0.),
            Rect::new(-0., -0., 30.25, 20.5),
            Rect::new(-11.25, -5.5, 2.125, 7.75),
            Rect::new(2., 3., 2., 20.),
            Rect::new(2., 3., 30., 3.),
            Rect::new(2., 3., 2., 3.),
            Rect::new(1., 2., 1.0000000001, 3.),
            Rect::new(0., 0., f64::from(f32::MIN_POSITIVE), 1.),
        ] {
            for fill in [Fill::NonZero, Fill::EvenOdd] {
                for outer in [
                    Affine::IDENTITY,
                    Affine::translate((13.5, 7.25)) * Affine::scale(0.6666667),
                    Affine::translate((40., 30.)) * Affine::scale_non_uniform(-1.25, 0.75),
                    Affine::translate((20., 10.)) * Affine::rotate(0.17),
                    Affine::new([1., 0.3, -0.2, 1., 10., 20.]),
                ] {
                    let mut direct =
                        tessellate(&Geometry::Rect(rect), fill, None, at, allocation()).unwrap();
                    let mut path = tessellate(
                        &Geometry::Path(rect.to_path(0.1)),
                        fill,
                        None,
                        at,
                        allocation(),
                    )
                    .unwrap();
                    equal(&direct, &path);
                    let uv = Affine::scale_non_uniform(0.05, -0.03) * Affine::translate((-4., 6.));
                    for mesh in [&mut direct, &mut path] {
                        transform(mesh, outer, [96, 64], uv, [0.2, 0.1, 0.3, 0.4], at).unwrap();
                    }
                    equal(&direct, &path);
                    let clip = RectClip {
                        bounds: [-0.65, -0.25, 0.1, 0.8],
                    };
                    match (
                        direct.clipped(clip, allocation()).unwrap(),
                        path.clipped(clip, allocation()).unwrap(),
                    ) {
                        (None, None) => (),
                        (Some(a), Some(b)) => equal(&a, &b),
                        _ => panic!("identical meshes must take the same clip path"),
                    }
                }
            }
        }
        for x in [f64::NAN, f64::INFINITY, -f64::INFINITY, 1e100] {
            assert!(matches!(
                tessellate(
                    &Geometry::Rect(Rect::new(x, 0., 1., 1.)),
                    Fill::NonZero, None, at, allocation(),
                ),
                Err(GpuError::Unsupported { at: location, .. }) if location == at
            ));
        }
        assert_eq!(shared.transient.usage().bytes, 0);
        assert_eq!(local.transient.usage().bytes, 0);
    }

    #[test]
    fn direct_paint_keeps_windows_repetition_styles_and_block_alpha_distinct() {
        use reel_core::texture::{TextureFormat, TextureLayout};
        let limits = super::super::GpuLimits::default();
        let shared = Budgets::new(limits);
        let local = Budgets::new(limits);
        let mut mesh = Mesh::new(Allocation::new(
            &shared,
            &local,
            limits.transient_bytes,
            1 << 20,
        ));
        mesh.quad(Rect::new(0., 0., 4., 4.), Rect::new(-1., -1., 2., 2.))
            .unwrap();
        let solid = mesh.direct_paint(PaintSource::Solid, [false; 2], None, None);
        assert_eq!(solid, Some(DirectPaint::Solid));
        assert_eq!(mesh.opaque_paint(solid), Some(OpaquePaint::Solid));
        assert_eq!(mesh.direct_coverage(None), Some(DirectPaint::Coverage));
        for (format, expected) in [
            (None, DirectPaint::Rgba),
            (Some(TextureFormat::Bc1Rgb), DirectPaint::Bc1),
            (Some(TextureFormat::Bc3Rgba), DirectPaint::Bc3),
        ] {
            assert_eq!(
                mesh.direct_paint(PaintSource::Texture, [false; 2], None, format),
                Some(expected)
            );
            assert_eq!(
                mesh.opaque_paint(Some(expected)),
                (expected == DirectPaint::Bc1).then_some(OpaquePaint::Bc1)
            );
        }
        // A partially covered edge or authored alpha prevents replacement for
        // the whole draw, even when its first vertex and texture are opaque.
        for alpha in [0., 0.5, f32::NAN] {
            mesh.vertices[3].color[3] = alpha;
            assert_eq!(mesh.opaque_paint(Some(DirectPaint::Bc1)), None);
            assert_eq!(mesh.opaque_paint(solid), None);
        }
        mesh.vertices[3].color[3] = 1.;
        assert_eq!(mesh.opaque_paint(None), None);
        assert_eq!(
            mesh.direct_paint(PaintSource::Texture, [true, false], None, None),
            None
        );
        assert_eq!(
            mesh.direct_paint(PaintSource::Texture, [false, true], None, None),
            None
        );
        let mut appearance = [[0.; 4]; 9];
        appearance[4][3] = 1.;
        assert_eq!(
            mesh.direct_paint(PaintSource::Texture, [false; 2], Some(&appearance), None),
            Some(DirectPaint::Rgba)
        );
        for (field, value) in [
            ((4, 3), 2.),
            ((4, 3), 3.),
            ((4, 3), 4.),
            ((5, 3), 1.),
            ((7, 3), 1.),
        ] {
            let mut styled = appearance;
            styled[field.0][field.1] = value;
            assert_eq!(
                mesh.direct_paint(PaintSource::Texture, [false; 2], Some(&styled), None),
                None
            );
            let direct = mesh.direct_paint(
                PaintSource::Texture,
                [false; 2],
                Some(&styled),
                Some(TextureFormat::Bc1Rgb),
            );
            assert_eq!(mesh.opaque_paint(direct), None);
        }
        // One regional vertex is sufficient to reject the whole original draw.
        // In particular, do not infer eligibility from the first vertex.
        mesh.vertices[3].source_window = [0.25, 0.25, 0.5, 0.5];
        assert_eq!(
            mesh.direct_paint(PaintSource::Texture, [false; 2], None, None),
            None
        );
        let layout = TextureLayout {
            logical_size: [3, 2],
            content_size: [3, 2],
            storage_size: [4, 4],
        };
        ImageSampling::with_layout(None, layout, [false; 2]).apply(&mut mesh.vertices);
        for (format, expected) in [
            (None, DirectPaint::RegionalRgba),
            (Some(TextureFormat::Bc1Rgb), DirectPaint::RegionalBc1),
            (Some(TextureFormat::Bc3Rgba), DirectPaint::RegionalBc3),
        ] {
            assert_eq!(
                mesh.direct_paint(PaintSource::Texture, [false; 2], None, format),
                Some(expected)
            );
            assert_eq!(
                mesh.opaque_paint(Some(expected)),
                (expected == DirectPaint::RegionalBc1).then_some(OpaquePaint::RegionalBc1)
            );
        }
        ImageSampling::with_layout(None, layout, [true, false]).apply(&mut mesh.vertices);
        assert_eq!(
            mesh.direct_paint(PaintSource::Texture, [false; 2], None, None),
            None
        );
    }
    use reel_ui::core::Widget;
    #[test]
    fn rectangle_clip_preserves_triangle_area_and_affine_attributes() {
        let limits = super::super::GpuLimits::default();
        let budgets = Budgets::new(limits);
        let local = Budgets::new(limits);
        let allocation = || Allocation::new(&budgets, &local, limits.transient_bytes, 1 << 20);
        let mut mesh = Mesh::new(allocation());
        mesh.quad(Rect::new(-2., -2., 2., 2.), Rect::new(0., 0., 1., 1.))
            .unwrap();
        for vertex in &mut mesh.vertices {
            vertex.color = [vertex.uv[0], vertex.uv[1], 0.25, 0.5];
            vertex.source_window = [0.1, 0.2, -0.5, 0.75];
        }
        let clip = RectClip {
            bounds: [-0.5, -0.25, 1., 0.75],
        };
        let output = mesh.clipped(clip, allocation()).unwrap().unwrap();
        let mut area = 0.;
        for triangle in output.indices.as_chunks::<3>().0 {
            let p: [[f32; 2]; 3] =
                std::array::from_fn(|i| output.vertices[triangle[i] as usize].position);
            area += ((p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]))
                .abs()
                * 0.5;
        }
        assert!((area - 1.5).abs() < 1e-6);
        for vertex in &output.vertices {
            assert!(clip.contains(vertex));
            for axis in 0..2 {
                let expected = (vertex.position[axis] + 2.) / 4.;
                assert!((vertex.uv[axis] - expected).abs() < 1e-6);
                assert!((vertex.color[axis] - expected).abs() < 1e-6);
            }
            assert_eq!(vertex.color[2..], [0.25, 0.5]);
            assert_eq!(vertex.source_window, [0.1, 0.2, -0.5, 0.75]);
        }
        assert!(output.clipped(clip, allocation()).unwrap().is_none());
        // Both original triangles cross their shared diagonal. Its intersection
        // must carry identical attributes in both clipped output polygons.
        for point in [[-0.25, -0.25], [0.75, 0.75]] {
            let shared: Vec<_> = output
                .vertices
                .iter()
                .filter(|vertex| vertex.position == point)
                .collect();
            assert_eq!(shared.len(), 2);
            assert_eq!(bytemuck::bytes_of(shared[0]), bytemuck::bytes_of(shared[1]));
        }
        assert!(
            mesh.clipped(
                RectClip {
                    bounds: [3., 3., 4., 4.]
                },
                allocation()
            )
            .unwrap()
            .unwrap()
            .indices
            .is_empty()
        );
        assert_eq!(mesh.vertices.len(), 4);
        drop(output);
        drop(mesh);
        assert_eq!(budgets.transient.usage().bytes, 0);
    }
    #[test]
    fn rectangle_clip_boundary_vertices_do_not_duplicate_intersections() {
        let limits = super::super::GpuLimits::default();
        let budgets = Budgets::new(limits);
        let local = Budgets::new(limits);
        let allocation = || Allocation::new(&budgets, &local, limits.transient_bytes, 1 << 20);
        let mut mesh = Mesh::new(allocation());
        let clip = RectClip {
            bounds: [-1., -1., 1., 1.],
        };
        mesh.quad(Rect::new(-2., -2., 1., 1.), Rect::new(0., 0., 1., 1.))
            .unwrap();
        let clipped = mesh.clipped(clip, allocation()).unwrap().unwrap();
        assert_eq!(clipped.vertices.len(), 6);
        assert_eq!(clipped.indices.len(), 6);
        for triangle in clipped.indices.as_chunks::<3>().0 {
            let positions = triangle.map(|i| clipped.vertices[i as usize].position);
            assert_ne!(positions[0], positions[1]);
            assert_ne!(positions[1], positions[2]);
            assert_ne!(positions[0], positions[2]);
        }
        let mut large = Mesh::new(allocation());
        let extent = f64::from(f32::MAX);
        large
            .quad(Rect::new(-extent, -extent, extent, extent), Rect::ZERO)
            .unwrap();
        let clipped = large.clipped(clip, allocation()).unwrap().unwrap();
        assert!(clipped.vertices.iter().all(|vertex| {
            vertex
                .position
                .iter()
                .chain(&vertex.uv)
                .all(|value| value.is_finite())
        }));
    }
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
            ImagePaint {
                color: [0.25; 4],
                sampling: ImageSampling::new(Some(Rect::new(0., 0., 4., 4.)), [10, 10], [false; 2]),
            },
            at,
        )
        .unwrap();
        mesh.image_quad(
            bounds,
            Affine::scale(2.),
            uv,
            [100, 100],
            ImagePaint {
                color: [0.75; 4],
                sampling: ImageSampling::new(
                    Some(Rect::new(4., 0., 8., 4.)),
                    [10, 10],
                    [true, false],
                ),
            },
            at,
        )
        .unwrap();
        assert_eq!(mesh.indices, [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]);
        assert_eq!(mesh.vertices[0].position, [-0.76, 0.52]);
        assert_eq!(mesh.vertices[4].position, [-0.92, 0.84]);
        assert_eq!(mesh.vertices[0].uv, [0., 0.]);
        assert_eq!(mesh.vertices[2].uv, [0.4, 0.8]);
        assert!(
            mesh.vertices[..4]
                .iter()
                .all(|v| v.source_window == [0., 0., 0.4, 0.4])
        );
        assert!(
            mesh.vertices[4..]
                .iter()
                .all(|v| v.source_window == [0.4, 0., -0.4, 0.4])
        );
        assert_eq!(
            size_of::<Vertex>(),
            48,
            "sampling windows have fixed per-vertex cost"
        );
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
