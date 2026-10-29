//! Ordered exact-position quads: shared sampling attributes and 48-byte instances.
use super::{
    GpuError,
    mesh::{self, Allocation, ImageSampling, Mesh, Vertex},
    precision::BoundedColor,
};
use reel_core::{image_coverage::ImageCoverage, texture::TextureLayout};
use reel_ui::{
    frame::{FrameSprites, RecordLocation},
    kurbo::{Affine, Rect},
    peniko::{Extend, ImageQuality},
};

pub(crate) const INDICES: [u16; 6] = [0, 1, 2, 0, 2, 3];
pub(crate) const MAX_INSTANCES: usize = u16::MAX as usize;
// Amortize the shared template/index payload to at most half of ordinary u16
// quad geometry. Smaller source calls can still join an existing instance run.
pub(crate) const MIN_INSTANCES: usize = 3;
pub(crate) struct Attribute {
    pub location: u32,
    pub components: u32,
    pub offset: u64,
}
pub(crate) const VERTEX_ATTRIBUTES: [Attribute; 3] = [
    Attribute {
        location: 0,
        components: 1,
        offset: 0,
    },
    Attribute {
        location: 1,
        components: 2,
        offset: 4,
    },
    Attribute {
        location: 3,
        components: 4,
        offset: 12,
    },
];
pub(crate) const INSTANCE_ATTRIBUTES: [Attribute; 3] = [
    Attribute {
        location: 4,
        components: 4,
        offset: 0,
    },
    Attribute {
        location: 5,
        components: 4,
        offset: 16,
    },
    Attribute {
        location: 2,
        components: 4,
        offset: 32,
    },
];
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct QuadVertex {
    pub corner: f32,
    pub uv: [f32; 2],
    pub window: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Template {
    pub vertices: [QuadVertex; 4],
}
impl Template {
    pub fn same(&self, other: &Self) -> bool {
        bytemuck::bytes_of(self) == bytemuck::bytes_of(other)
    }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Instance {
    pub positions: [[f32; 4]; 2],
    pub color: [f32; 4],
}
const _: [(); 48] = [(); size_of::<Instance>()];
impl Instance {
    pub fn position(&self, corner: usize) -> [f32; 2] {
        let pair = &self.positions[corner / 2];
        let offset = corner % 2 * 2;
        [pair[offset], pair[offset + 1]]
    }
}
/// Validated stack-only input. Both backends use the original filtering order,
/// f64 affine operations and local f32 conversion from ordinary mesh expansion.
pub(crate) struct Input<'a> {
    batch: &'a FrameSprites,
    pub template: Template,
    local: [[f32; 2]; 4],
    outer: Affine,
    size: [u32; 2],
    at: RecordLocation,
}
impl<'a> Input<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn sprites(
        batch: &'a FrameSprites,
        outer: Affine,
        layout: TextureLayout,
        uv: Affine,
        coverage: Option<&ImageCoverage>,
        size: [u32; 2],
        atlas: bool,
        at: RecordLocation,
    ) -> Result<Option<Self>, GpuError> {
        if batch.brush.sampler.quality == ImageQuality::High
            || batch.brush.sampler.x_extend == Extend::Reflect
            || batch.brush.sampler.y_extend == Extend::Reflect
        {
            return Err(GpuError::Unsupported {
                at,
                record: "sprite image sampling",
            });
        }
        if batch.composite.blend != Default::default()
            || coverage.is_some_and(|coverage| !coverage.is_full())
        {
            return Ok(None);
        }
        let local = mesh::quad_local(batch.bounds);
        let [_, _, _, _, x, y] = uv.as_coeffs();
        let window = ImageSampling::for_image(
            batch.brush.image.source_rect,
            layout,
            [x, y],
            [
                batch.brush.sampler.x_extend == Extend::Repeat,
                batch.brush.sampler.y_extend == Extend::Repeat,
            ],
            atlas,
        )
        .window();
        let vertices = std::array::from_fn(|corner| QuadVertex {
            corner: corner as f32,
            uv: mesh::project_uv(local[corner], uv),
            window,
        });
        if vertices
            .iter()
            .flat_map(|vertex| vertex.uv)
            .any(|value| !value.is_finite())
        {
            // The ordinary path validates UV only for visible quads.
            return Ok(None);
        }
        Ok(Some(Self {
            batch,
            template: Template { vertices },
            local,
            outer: outer * batch.transform,
            size,
            at,
        }))
    }
}
pub(crate) struct Batch {
    pub template: Template,
    pub instances: Vec<Instance>,
    pub bounded_color: BoundedColor,
    pub opaque: bool,
    allocation: Allocation,
}
impl Batch {
    pub fn new(input: &Input<'_>, allocation: Allocation) -> Result<Self, GpuError> {
        let mut batch = Self {
            template: input.template,
            instances: Vec::new(),
            bounded_color: BoundedColor::UNKNOWN,
            opaque: true,
            allocation,
        };
        batch.append(input)?;
        Ok(batch)
    }
    pub fn append(&mut self, input: &Input<'_>) -> Result<(), GpuError> {
        assert!(
            self.template.same(&input.template),
            "instance run shares one sampling template"
        );
        let viewport = Rect::new(0., 0., f64::from(input.size[0]), f64::from(input.size[1]));
        for sprite in &input.batch.instances {
            if sprite.alpha <= 0. || !sprite.has_area() {
                continue;
            }
            let transform = input.outer * sprite.transform(input.batch.bounds);
            let visible = transform
                .transform_rect_bbox(input.batch.bounds)
                .inflate(1., 1.)
                .intersect(viewport);
            if visible.width() <= 0. || visible.height() <= 0. {
                continue;
            }
            let positions = input
                .local
                .map(|position| mesh::project_position(position, transform, input.size));
            if positions.iter().flatten().any(|value| !value.is_finite()) {
                return Err(GpuError::Unsupported {
                    at: input.at,
                    record: "transformed geometry outside float32 range",
                });
            }
            let alpha =
                sprite.alpha * input.batch.composite.alpha * input.batch.brush.sampler.alpha;
            let color = input.batch.brush.image.color.map(|value| value * alpha);
            let bounded = BoundedColor::new(color);
            self.bounded_color = if self.instances.is_empty() {
                bounded
            } else {
                self.bounded_color.intersect(bounded)
            };
            self.opaque &= color[3] == 1.;
            self.allocation.grow(&mut self.instances, 1)?;
            self.instances.push(Instance {
                positions: [
                    [
                        positions[0][0],
                        positions[0][1],
                        positions[1][0],
                        positions[1][1],
                    ],
                    [
                        positions[2][0],
                        positions[2][1],
                        positions[3][0],
                        positions[3][1],
                    ],
                ],
                color,
            });
        }
        Ok(())
    }
    pub fn bytes(&self) -> u64 {
        size_of::<Template>() as u64
            + size_of_val(&INDICES) as u64
            + self.instances.len() as u64 * size_of::<Instance>() as u64
    }
    pub fn expanded_bytes(&self) -> usize {
        self.instances
            .len()
            .saturating_mul(4 * size_of::<Vertex>() + 6 * size_of::<u32>())
    }
    pub fn direct_paint(
        &self,
        source: mesh::PaintSource,
        repeat: [bool; 2],
        appearance: Option<&[[f32; 4]; 10]>,
        format: Option<reel_core::texture::TextureFormat>,
    ) -> Option<mesh::DirectPaint> {
        mesh::direct_paint(
            source,
            repeat,
            appearance,
            format,
            std::iter::once(self.template.vertices[0].window),
        )
    }
    #[cfg(all(feature = "gxm", target_os = "vita"))]
    pub fn direct_coverage(
        &self,
        appearance: Option<&[[f32; 4]; 10]>,
    ) -> Option<mesh::DirectPaint> {
        (self.direct_paint(mesh::PaintSource::Texture, [false; 2], appearance, None)
            == Some(mesh::DirectPaint::Rgba))
        .then_some(mesh::DirectPaint::Coverage)
    }
    pub fn opaque_paint(&self, direct: Option<mesh::DirectPaint>) -> Option<mesh::OpaquePaint> {
        mesh::opaque_paint(direct, self.opaque)
    }
    pub fn scissor(
        &self,
        scissor: super::scissor::Scissor,
        size: [u32; 2],
    ) -> super::scissor::Scissor {
        scissor.for_positions(
            self.instances
                .iter()
                .flat_map(|instance| (0..4).map(|corner| instance.position(corner))),
            size,
        )
    }
    pub fn worthwhile(&self) -> bool {
        self.bytes() * 2
            <= self.instances.len() as u64 * (4 * size_of::<Vertex>() + 6 * size_of::<u16>()) as u64
    }
    pub fn instanced(&self, merges_before: bool, merges_after: bool) -> bool {
        self.worthwhile() && !merges_before && !merges_after
    }
    /// Preserve the old upload boundary if this run later needs mesh fallback.
    pub fn can_merge(&self, other: &Self) -> bool {
        self.expanded_bytes().saturating_add(other.expanded_bytes()) <= super::draw_batch::MAX_BYTES
    }
    pub fn merge(&mut self, other: Self) -> Result<(), GpuError> {
        assert!(
            self.template.same(&other.template),
            "instance run shares one sampling template"
        );
        self.allocation
            .grow(&mut self.instances, other.instances.len())?;
        if self.instances.is_empty() {
            self.bounded_color = other.bounded_color;
        } else {
            self.bounded_color = self.bounded_color.intersect(other.bounded_color);
        }
        self.opaque &= other.opaque;
        self.instances.extend_from_slice(&other.instances);
        Ok(())
    }
    /// Reuse the charged 48-byte records when a neighbouring ordinary draw can
    /// merge with this run. Safe same-layout casting avoids retaining a second
    /// copy of the instance allocation during the expanded fallback.
    pub fn into_mesh(mut self) -> Result<Mesh, GpuError> {
        let count = self.instances.len();
        if count < MIN_INSTANCES {
            // A one-quad frame needs only the original 216-byte CPU payload.
            // Keep the at-most-two records on the stack while releasing their
            // temporary ticket, so fallback preserves that admission boundary.
            let mut records = [bytemuck::Zeroable::zeroed(); MIN_INSTANCES];
            records[..count].copy_from_slice(&self.instances);
            let template = self.template;
            let allocation = self.allocation.fresh();
            drop(self);
            let mut mesh = Mesh::new(allocation);
            for instance in &records[..count] {
                let vertices: [Vertex; 4] = std::array::from_fn(|corner| Vertex {
                    position: instance.position(corner),
                    uv: template.vertices[corner].uv,
                    color: instance.color,
                    source_window: template.vertices[corner].window,
                });
                mesh.indexed_triangles(&vertices, INDICES.map(u32::from).into_iter())?;
            }
            return Ok(mesh);
        }
        let vertices_len = count
            .checked_mul(4)
            .filter(|&n| n <= u32::MAX as usize)
            .ok_or(GpuError::InvalidSize)?;
        let indices_len = count.checked_mul(6).ok_or(GpuError::InvalidSize)?;
        let mut vertices: Vec<Vertex> = bytemuck::allocation::cast_vec(self.instances);
        self.allocation.grow(&mut vertices, vertices_len - count)?;
        vertices.resize(vertices_len, bytemuck::Zeroable::zeroed());
        for index in (0..count).rev() {
            let instance: Instance = bytemuck::cast(vertices[index]);
            for corner in 0..4 {
                vertices[index * 4 + corner] = Vertex {
                    position: instance.position(corner),
                    uv: self.template.vertices[corner].uv,
                    color: instance.color,
                    source_window: self.template.vertices[corner].window,
                };
            }
        }
        let mut indices = Vec::new();
        self.allocation.grow(&mut indices, indices_len)?;
        for index in 0..count {
            indices.extend(INDICES.map(|vertex| index as u32 * 4 + u32::from(vertex)));
        }
        Ok(Mesh::from_geometry(
            vertices,
            indices,
            self.bounded_color,
            self.allocation,
        ))
    }
}

#[cfg(test)]
#[path = "quad_instances_tests.rs"]
mod tests;
