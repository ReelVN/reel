use super::{Budgets, GpuError, native, resources};
use crate::common::geometry::{Allocation, Capacity, Pool};
use reel_core::budget::BudgetTicket;

pub(super) struct Buffers {
    pub vertices: native::VertexBuffer,
    pub indices: native::IndexBuffer,
    pub instances: Option<Box<native::VertexBuffer>>,
    pub layout: Option<crate::common::draw_batch::Layout>,
}
pub(super) type Storage = Box<Allocation<Buffers, super::ownership::NativeTicket>>;
#[path = "geometry_instances.rs"]
mod instances;
pub(super) use instances::{instance_attributes, upload as upload_instances, vertex_attributes};
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
    ) -> Result<bool, GpuError> {
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
        let narrow = capacity.index_format == crate::common::indices::Format::U16;
        for range in layout.ranges(batch) {
            let packed = batch.view(range.clone()).pack(|bytes| {
                crate::common::renderer::reserve(
                    &budgets.transient,
                    &local.transient,
                    bytes,
                    limit,
                    "geometry packing",
                )
            })?;
            gpu.update_vertex_range(
                &mut self.vertices,
                range.vertices.start,
                bytemuck::cast_slice(&packed.vertices),
            )?;
            reel_core::profile::geometry_upload(
                std::mem::size_of_val(packed.vertices.as_ref()) as u64
            );
            if narrow {
                let values =
                    &bytemuck::cast_slice::<u32, u16>(&packed.indices)[..range.indices.len()];
                gpu.update_index_range_u16(&mut self.indices, range.indices.start, values)?;
                reel_core::profile::geometry_upload(std::mem::size_of_val(values) as u64);
            } else {
                gpu.update_index_range(&mut self.indices, range.indices.start, &packed.indices)?;
                reel_core::profile::geometry_upload(
                    std::mem::size_of_val(packed.indices.as_slice()) as u64,
                );
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
        let storage = self.pool.take_eligible(
            batch.capacity(),
            |storage| storage.instances.is_none(),
            |storage| {
                storage
                    .layout
                    .as_ref()
                    .map_or(0, |layout| layout.score(batch))
            },
        )?;
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
        self.pool
            .take_eligible(capacity, |storage| storage.instances.is_none(), |_| 0)
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
    pub fn begin_replay(&mut self) {
        self.shapes.begin_replay();
        self.glyphs.begin_replay();
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
) -> Result<(Storage, BudgetTicket, Option<BudgetTicket>), GpuError> {
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GeometryUpload);
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
        let packed = batch.full_view().pack(|bytes| {
            crate::common::renderer::reserve(
                &budgets.transient,
                &local.transient,
                bytes,
                limits.transient_bytes,
                "geometry packing",
            )
        })?;
        let (mut storage, ticket, created) = if let Some((storage, ticket)) = reused.take() {
            (storage, ticket, false)
        } else {
            acquire(
                cache,
                gpu,
                budgets,
                local,
                limits,
                batch.capacity(),
                &packed,
            )?
        };
        if !created {
            gpu.update_vertices(
                &mut storage.storage.vertices,
                bytemuck::cast_slice(&packed.vertices),
            )?;
            reel_core::profile::geometry_upload(
                std::mem::size_of_val(packed.vertices.as_ref()) as u64
            );
            match packed.format {
                crate::common::indices::Format::U16 => gpu.update_indices_u16(
                    &mut storage.storage.indices,
                    bytemuck::cast_slice(&packed.indices),
                )?,
                crate::common::indices::Format::U32 => {
                    gpu.update_indices(&mut storage.storage.indices, &packed.indices)?
                }
            }
            reel_core::profile::geometry_upload(
                std::mem::size_of_val(packed.indices.as_slice()) as u64
            );
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
    requested: Capacity,
    packed: &crate::common::draw_batch::Packed<'_, crate::common::renderer::Reservation>,
) -> Result<(Storage, BudgetTicket, bool), GpuError> {
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
        .map_err(|_| GpuError::Budget {
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
        bytemuck::cast_slice(&packed.vertices),
        12,
        &super::programs::attributes(),
    )?;
    reel_core::profile::geometry_upload(requested.vertices);
    let indices = match packed.format {
        crate::common::indices::Format::U16 => {
            gpu.index_buffer_u16(bytemuck::cast_slice(&packed.indices))?
        }
        crate::common::indices::Format::U32 => gpu.index_buffer(&packed.indices)?,
    };
    reel_core::profile::geometry_upload(requested.indices);
    Ok((
        Box::new(Allocation {
            capacity: requested,
            storage: Buffers {
                vertices,
                indices,
                instances: None,
                layout: None,
            },
            ticket,
        }),
        local,
        true,
    ))
}
