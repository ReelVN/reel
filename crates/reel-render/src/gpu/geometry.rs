//! Reusable geometry and its bounded, encoder-owned upload storage.
use super::{Gpu, GpuError, renderer::Budgets};
use crate::common::{
    geometry::{Allocation, Capacity, Pool},
    mesh,
};
use reel_core::budget::BudgetTicket;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
};
use wgpu::util::DeviceExt;

const MAPPED: u8 = 0;
const PENDING: u8 = 1;
const FAILED: u8 = 2;

pub(super) struct Buffers {
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    staging: Option<wgpu::Buffer>,
    mapping: Option<Arc<AtomicU8>>,
}
pub(super) struct UploadPolicy {
    pub reuse: bool,
    pub staged: std::cell::Cell<bool>,
}
impl UploadPolicy {
    pub fn new(reuse: bool) -> Self {
        Self {
            reuse,
            staged: std::cell::Cell::new(false),
        }
    }
}
pub(super) type Storage = Box<Allocation<Buffers>>;
#[derive(Default)]
pub(super) struct Cache(Mutex<Pool<Buffers>>);
impl Cache {
    pub fn bytes(&self) -> u64 {
        self.0.lock().unwrap().bytes()
    }
    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
    pub fn recycle(&self, storage: &mut Vec<Storage>) {
        let mut pool = self.0.lock().unwrap();
        for slot in storage.drain(..) {
            if slot.storage.staging.is_some() {
                pool.put(slot);
            }
        }
    }
    pub fn acquire(
        &self,
        gpu: &Gpu,
        budgets: &Budgets,
        local: &Budgets,
        mesh: &mesh::Mesh,
        policy: &UploadPolicy,
    ) -> Result<(Storage, BudgetTicket), GpuError> {
        let requested = Capacity {
            vertices: std::mem::size_of_val(mesh.vertices.as_slice()) as u64,
            indices: std::mem::size_of_val(mesh.indices.as_slice()) as u64,
        };
        let mut pool = self.0.lock().unwrap();
        if policy.reuse
            && let Some(storage) = pool.take(requested)
            && let Ok(ticket) = local.transient.reserve(storage.ticket.bytes())
        {
            return Ok((storage, ticket));
        }
        let staging_bytes = requested.bytes().div_ceil(wgpu::MAP_ALIGNMENT) * wgpu::MAP_ALIGNMENT;
        let limit = gpu.limits.transient_bytes;
        // A mesh which only fits the original immutable upload must remain
        // admissible; its one-off buffers simply do not enter the reusable pool.
        // Include the Arc's atomic owner counts and aligned state allocation.
        let state_bytes = (2 * size_of::<usize>() + size_of::<AtomicU8>())
            .div_ceil(align_of::<usize>())
            * align_of::<usize>();
        let reusable = policy.reuse
            && requested.bytes()
                + staging_bytes
                + size_of::<Allocation<Buffers>>() as u64
                + state_bytes as u64
                <= limit.saturating_sub(local.transient.usage().bytes);
        let bytes = requested.bytes()
            + if reusable {
                staging_bytes + state_bytes as u64
            } else {
                0
            }
            + size_of::<Allocation<Buffers>>() as u64;
        let local_ticket = local
            .transient
            .reserve(bytes)
            .map_err(|_| GpuError::Budget {
                domain: "geometry",
                requested: bytes,
                available: limit.saturating_sub(local.transient.usage().bytes),
            })?;
        if bytes > limit.saturating_sub(budgets.transient.usage().bytes) {
            pool.clear();
        }
        let ticket = budgets
            .transient
            .reserve(bytes)
            .map_err(|_| GpuError::Pressure {
                domain: "geometry",
                requested: bytes,
                available: limit.saturating_sub(budgets.transient.usage().bytes),
            })?;
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::BufferCreate);
        let create = |label, size, usage, mapped_at_creation| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation,
            })
        };
        let (vertices, indices, staging) = if reusable {
            (
                create(
                    "reel reusable vertices",
                    requested.vertices,
                    wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    false,
                ),
                create(
                    "reel reusable indices",
                    requested.indices,
                    wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                    false,
                ),
                Some(create(
                    "reel geometry upload",
                    staging_bytes,
                    wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::MAP_WRITE,
                    true,
                )),
            )
        } else {
            (
                gpu.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("reel vertices"),
                        contents: bytemuck::cast_slice(&mesh.vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                gpu.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("reel indices"),
                        contents: bytemuck::cast_slice(&mesh.indices),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                None,
            )
        };
        gpu.check()?;
        #[cfg(test)]
        {
            gpu.upload_creations
                .mesh_buffers
                .fetch_add(2, Ordering::Relaxed);
            gpu.upload_creations
                .geometry_staging
                .fetch_add(usize::from(reusable), Ordering::Relaxed);
        }
        Ok((
            Box::new(Allocation {
                capacity: requested,
                ticket,
                storage: Buffers {
                    vertices,
                    indices,
                    staging,
                    mapping: reusable.then(|| Arc::new(AtomicU8::new(MAPPED))),
                },
            }),
            local_ticket,
        ))
    }
}
impl Buffers {
    pub fn has_staging(&self) -> bool {
        self.staging.is_some()
    }
    pub fn ready(&self) -> Result<bool, GpuError> {
        let Some(mapping) = &self.mapping else {
            return Ok(true);
        };
        match mapping.load(Ordering::Acquire) {
            MAPPED => Ok(true),
            FAILED => Err(GpuError::Device("geometry upload remapping failed".into())),
            _ => Ok(false),
        }
    }
    pub fn upload(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        capacity: Capacity,
        mesh: &mesh::Mesh,
    ) -> Result<(), GpuError> {
        let Some(staging) = &self.staging else {
            return Ok(());
        };
        let vertices = bytemuck::cast_slice(&mesh.vertices);
        let indices = bytemuck::cast_slice(&mesh.indices);
        {
            let mut mapping = staging
                .slice(..)
                .get_mapped_range_mut()
                .map_err(|error| GpuError::Device(error.to_string()))?;
            mapping.slice(..vertices.len()).copy_from_slice(vertices);
            let offset = capacity.vertices as usize;
            mapping
                .slice(offset..offset + indices.len())
                .copy_from_slice(indices);
        }
        staging.unmap();
        encoder.copy_buffer_to_buffer(staging, 0, &self.vertices, 0, vertices.len() as u64);
        encoder.copy_buffer_to_buffer(
            staging,
            capacity.vertices,
            &self.indices,
            0,
            indices.len() as u64,
        );
        let state = self
            .mapping
            .as_ref()
            .expect("staging owns mapping state")
            .clone();
        state.store(PENDING, Ordering::Release);
        // WGPU schedules this only when this exact encoder is submitted. Dropping
        // an unsubmitted Prepared drops both its copy commands and these owners.
        encoder.map_buffer_on_submit(staging, wgpu::MapMode::Write, .., move |result| {
            state.store(
                if result.is_ok() { MAPPED } else { FAILED },
                Ordering::Release,
            );
        });
        Ok(())
    }
}
