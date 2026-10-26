use super::{Budgets, GlesError, native, resources};
use crate::common::{
    geometry::{Allocation, Capacity, Pool},
    mesh,
};
use reel_core::budget::BudgetTicket;

pub(super) struct Buffers {
    pub vertices: native::VertexBuffer,
    pub indices: native::IndexBuffer,
}
pub(super) type Storage = Box<Allocation<Buffers>>;
pub(super) type Cache = Pool<Buffers>;

pub(super) fn acquire(
    cache: &mut Cache,
    gpu: &native::Context,
    budgets: &Budgets,
    local: &Budgets,
    limits: crate::common::GpuLimits,
    mesh: &mesh::Mesh,
) -> Result<(Storage, BudgetTicket, bool), GlesError> {
    let requested = Capacity {
        vertices: std::mem::size_of_val(mesh.vertices.as_slice()) as u64,
        indices: std::mem::size_of_val(mesh.indices.as_slice()) as u64,
    };
    if let Some(storage) = cache.take(requested) {
        // A large cached slot must not reject a frame which fits exact storage.
        if let Ok(ticket) = local.transient.reserve(storage.ticket.bytes()) {
            return Ok((storage, ticket, false));
        }
    }
    let bytes = requested.bytes()
        + size_of::<Allocation<Buffers>>() as u64
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
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::BufferCreate);
    let vertices = gpu.vertex_buffer(
        bytemuck::cast_slice(&mesh.vertices),
        12,
        &super::programs::attributes(),
    )?;
    let indices = gpu.index_buffer(&mesh.indices)?;
    Ok((
        Box::new(Allocation {
            capacity: requested,
            storage: Buffers { vertices, indices },
            ticket,
        }),
        local,
        true,
    ))
}
