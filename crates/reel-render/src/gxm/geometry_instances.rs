//! Reuse ordered instance geometry through the ordinary completion-owned pool.
use super::*;
use crate::common::quad_instances::{Batch, INDICES, Instance, Template};

pub(in crate::gxm) fn vertex_attributes() -> [native::VertexAttribute; 3] {
    crate::common::quad_instances::VERTEX_ATTRIBUTES.map(|a| native::VertexAttribute {
        location: a.location,
        components: a.components,
        offset: (a.offset / 4) as u32,
    })
}
pub(in crate::gxm) fn instance_attributes() -> [native::VertexAttribute; 3] {
    crate::common::quad_instances::INSTANCE_ATTRIBUTES.map(|a| native::VertexAttribute {
        location: a.location,
        components: a.components,
        offset: (a.offset / 4) as u32,
    })
}
pub(in crate::gxm) fn upload(
    cache: &mut Cache,
    gpu: &super::super::ownership::Device,
    budgets: &Budgets,
    local: &Budgets,
    limits: crate::common::GpuLimits,
    batch: &Batch,
) -> Result<(Storage, BudgetTicket), GpuError> {
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GeometryUpload);
    let requested = Capacity {
        vertices: (size_of::<Template>() + size_of_val(batch.instances.as_slice())) as u64,
        indices: size_of_val(&INDICES) as u64,
        index_format: crate::common::indices::Format::U16,
    };
    if let Some(mut storage) =
        cache
            .pool
            .take_eligible(requested, |storage| storage.instances.is_some(), |_| 0)
        && let Ok(ticket) = local.transient.reserve(storage.ticket.bytes())
    {
        cache.optional_used = true;
        gpu.update_vertices(
            &mut storage.storage.vertices,
            bytemuck::cast_slice(&batch.template.vertices),
        )?;
        gpu.update_vertices(
            storage.storage.instances.as_deref_mut().unwrap(),
            bytemuck::cast_slice(&batch.instances),
        )?;
        reel_core::profile::geometry_upload(requested.vertices);
        return Ok((storage, ticket));
    }
    let metadata = size_of::<Allocation<Buffers, super::super::ownership::NativeTicket>>()
        + size_of::<native::VertexBuffer>()
        + 6 * size_of::<native::VertexAttribute>();
    let bytes = requested.bytes() + metadata as u64;
    let local_ticket = local
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
    let ticket = gpu.ticket(resources::reserve(
        &budgets.transient,
        bytes,
        limits.transient_bytes,
        "geometry",
    )?);
    let vertices = gpu.vertex_buffer(
        bytemuck::cast_slice(&batch.template.vertices),
        (size_of::<crate::common::quad_instances::QuadVertex>() / 4) as u32,
        &vertex_attributes(),
    )?;
    let instances = Box::new(gpu.vertex_buffer(
        bytemuck::cast_slice(&batch.instances),
        (size_of::<Instance>() / 4) as u32,
        &instance_attributes(),
    )?);
    let indices = gpu.index_buffer_u16(&INDICES)?;
    reel_core::profile::geometry_upload(requested.bytes());
    Ok((
        Box::new(Allocation {
            capacity: requested,
            storage: Buffers {
                vertices,
                indices,
                instances: Some(instances),
                layout: None,
            },
            ticket,
        }),
        local_ticket,
    ))
}
