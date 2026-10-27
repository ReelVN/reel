use super::{Budgets, GlesError, native, resources};
use crate::common::{
    geometry::{Allocation, Capacity, Pool},
    mesh,
};
use reel_core::budget::BudgetTicket;

pub(super) struct Buffers {
    pub vertices: native::VertexBuffer,
    pub indices: native::IndexBuffer,
    pub layout: Option<crate::common::draw_batch::Layout>,
}
pub(super) type Storage = Box<Allocation<Buffers, super::ownership::NativeTicket>>;
type GlyphGeometry = crate::common::recording_cache::Cache<
    crate::common::recording_cache::GlyphDependency,
    crate::common::recording_cache::Glyphs<
        std::rc::Rc<super::draw::Page>,
        std::rc::Rc<super::draw::Glyph>,
    >,
>;
impl Buffers {
    pub fn upload_segments<T>(
        &mut self,
        gpu: &super::ownership::Device,
        capacity: Capacity,
        batch: &crate::common::draw_batch::Batch<T>,
        budgets: &Budgets,
        local: &Budgets,
        limit: u64,
    ) -> Result<bool, GlesError> {
        let Some(layout) = self
            .layout
            .as_ref()
            .filter(|layout| layout.compatible(batch))
        else {
            return Ok(false);
        };
        if layout.score(batch) == 0 {
            return Ok(false);
        }
        let ranges = layout.ranges(batch);
        let maximum = ranges
            .clone()
            .map(|range| range.indices.len())
            .max()
            .unwrap_or(0);
        if maximum == 0 {
            return Ok(true);
        }
        let narrow = capacity.index_format == crate::common::indices::Format::U16;
        let _scratch = crate::common::renderer::reserve(
            &budgets.transient,
            &local.transient,
            (maximum * if narrow { 2 } else { 4 }) as u64,
            limit,
            "index packing",
        )?;
        let mut short = Vec::<u16>::with_capacity(if narrow { maximum } else { 0 });
        let mut wide = Vec::<u32>::with_capacity(if narrow { 0 } else { maximum });
        for range in ranges {
            let materialized = batch.materialize_range(range.segments)?;
            let mesh = &*materialized;
            gpu.update_vertex_range(
                &mut self.vertices,
                range.vertices.start,
                bytemuck::cast_slice(&mesh.vertices),
            )?;
            reel_core::profile::geometry_upload(
                std::mem::size_of_val(mesh.vertices.as_slice()) as u64
            );
            if narrow {
                short.clear();
                short.extend(
                    mesh.indices
                        .iter()
                        .map(|&value| (range.vertices.start as u32 + value) as u16),
                );
                gpu.update_index_range_u16(&mut self.indices, range.indices.start, &short)?;
                reel_core::profile::geometry_upload(std::mem::size_of_val(short.as_slice()) as u64);
            } else {
                wide.clear();
                wide.extend(
                    mesh.indices
                        .iter()
                        .map(|&value| range.vertices.start as u32 + value),
                );
                gpu.update_index_range(&mut self.indices, range.indices.start, &wide)?;
                reel_core::profile::geometry_upload(std::mem::size_of_val(wide.as_slice()) as u64);
            }
        }
        Ok(true)
    }
}
#[derive(Default)]
pub(super) struct Cache {
    pool: Pool<Buffers, super::ownership::NativeTicket>,
    pub cpu_disabled: bool,
    pub optional_used: bool,
    pub shapes: crate::common::recording_cache::Cache<
        crate::common::recording_cache::ShapeDependency,
        std::sync::Arc<crate::common::recording_cache::Mesh>,
    >,
    pub glyphs: GlyphGeometry,
}
impl Cache {
    pub fn take_batch<T>(
        &mut self,
        batch: &crate::common::draw_batch::Batch<T>,
        local: &Budgets,
    ) -> Option<(Storage, BudgetTicket)> {
        let storage = self.pool.take_matching(batch.capacity(), |storage| {
            storage
                .layout
                .as_ref()
                .map_or(0, |layout| layout.score(batch))
        })?;
        let bytes = storage.ticket.bytes()
            + storage
                .storage
                .layout
                .as_ref()
                .map_or(0, |layout| layout.bytes());
        let ticket = local.transient.reserve(bytes).ok()?;
        self.optional_used = true;
        Some((storage, ticket))
    }
    pub fn take(&mut self, capacity: Capacity) -> Option<Storage> {
        self.pool.take(capacity)
    }
    pub fn put(&mut self, storage: Storage) {
        self.pool.put(storage);
    }
    pub fn bytes(&self) -> u64 {
        self.pool.bytes()
            + self.pool.additional_bytes(|storage| {
                storage.layout.as_ref().map_or(0, |layout| layout.bytes())
            })
            + self.shapes.bytes()
            + self.glyphs.bytes()
    }
    pub fn clear(&mut self) {
        self.pool.clear();
        self.shapes.clear();
        self.glyphs.clear();
    }
}
pub(super) fn upload_batch<T>(
    cache: &mut Cache,
    gpu: &super::ownership::Device,
    budgets: &Budgets,
    local: &Budgets,
    limits: crate::common::GpuLimits,
    batch: &crate::common::draw_batch::Batch<T>,
) -> Result<(Storage, BudgetTicket, Option<BudgetTicket>), GlesError> {
    let mut reused = cache.take_batch(batch, local);
    let partial = if let Some((storage, _)) = &mut reused {
        storage.storage.upload_segments(
            gpu,
            storage.capacity,
            batch,
            budgets,
            local,
            limits.transient_bytes,
        )?
    } else {
        false
    };
    let (mut storage, local_ticket) = if partial {
        reused.take().unwrap()
    } else {
        let materialized = batch.materialize()?;
        let mesh = &*materialized;
        let indices = crate::common::indices::Upload::new(mesh, |bytes| {
            crate::common::renderer::reserve(
                &budgets.transient,
                &local.transient,
                bytes,
                limits.transient_bytes,
                "index packing",
            )
        })?;
        let (mut storage, ticket, created) = if let Some((storage, ticket)) = reused.take() {
            (storage, ticket, false)
        } else {
            acquire(cache, gpu, budgets, local, limits, mesh, &indices)?
        };
        if !created {
            gpu.update_vertices(
                &mut storage.storage.vertices,
                bytemuck::cast_slice(&mesh.vertices),
            )?;
            reel_core::profile::geometry_upload(
                std::mem::size_of_val(mesh.vertices.as_slice()) as u64
            );
            match &indices {
                crate::common::indices::Upload::Narrow { values, .. } => {
                    gpu.update_indices_u16(&mut storage.storage.indices, values)?
                }
                crate::common::indices::Upload::Wide(values) => {
                    gpu.update_indices(&mut storage.storage.indices, values)?
                }
            }
            reel_core::profile::geometry_upload(indices.bytes().len() as u64);
        }
        (storage, ticket)
    };
    let metadata = if cache.cpu_disabled {
        None
    } else {
        crate::common::draw_batch::Layout::record(
            &mut storage.storage.layout,
            batch,
            budgets,
            local,
        )
    };
    cache.optional_used |= metadata.is_some();
    Ok((storage, local_ticket, metadata))
}

pub(super) fn acquire(
    cache: &mut Cache,
    gpu: &super::ownership::Device,
    budgets: &Budgets,
    local: &Budgets,
    limits: crate::common::GpuLimits,
    mesh: &mesh::Mesh,
    indices: &crate::common::indices::Upload<'_, crate::common::renderer::Reservation>,
) -> Result<(Storage, BudgetTicket, bool), GlesError> {
    let requested = Capacity {
        vertices: std::mem::size_of_val(mesh.vertices.as_slice()) as u64,
        indices: indices.bytes().len() as u64,
        index_format: indices.format(),
    };
    if let Some(storage) = cache.take(requested) {
        // A large cached slot must not reject a frame which fits exact storage.
        if let Ok(ticket) = local.transient.reserve(
            storage.ticket.bytes()
                + storage
                    .storage
                    .layout
                    .as_ref()
                    .map_or(0, |layout| layout.bytes()),
        ) {
            cache.optional_used = true;
            return Ok((storage, ticket, false));
        }
    }
    let bytes = requested.bytes()
        + size_of::<Allocation<Buffers, super::ownership::NativeTicket>>() as u64
        + std::mem::size_of_val(&super::programs::attributes()) as u64;
    let local = local
        .transient
        .reserve(bytes)
        .map_err(|_| GlesError::Budget {
            domain: "geometry",
            requested: bytes,
            available: limits
                .transient_bytes
                .saturating_sub(local.transient.usage().bytes),
        })?;
    if bytes
        > limits
            .transient_bytes
            .saturating_sub(budgets.transient.usage().bytes)
    {
        cache.clear();
    }
    let ticket = resources::reserve(
        &budgets.transient,
        bytes,
        limits.transient_bytes,
        "geometry",
    )?;
    let ticket = gpu.ticket(ticket);
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::BufferCreate);
    let vertices = gpu.vertex_buffer(
        bytemuck::cast_slice(&mesh.vertices),
        12,
        &super::programs::attributes(),
    )?;
    reel_core::profile::geometry_upload(requested.vertices);
    let indices = match indices {
        crate::common::indices::Upload::Narrow { values, .. } => gpu.index_buffer_u16(values)?,
        crate::common::indices::Upload::Wide(values) => gpu.index_buffer(values)?,
    };
    reel_core::profile::geometry_upload(requested.indices);
    Ok((
        Box::new(Allocation {
            capacity: requested,
            storage: Buffers {
                vertices,
                indices,
                layout: None,
            },
            ticket,
        }),
        local,
        true,
    ))
}
