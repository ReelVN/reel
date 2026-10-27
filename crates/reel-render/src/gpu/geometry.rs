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

#[cfg(test)]
fn upload_bytes<T>(
    layout: Option<&crate::common::draw_batch::Layout>,
    batch: &crate::common::draw_batch::Batch<T>,
) -> u64 {
    let Some(layout) = layout.filter(|layout| layout.compatible(batch)) else {
        return batch.capacity().bytes();
    };
    let width = if batch.capacity().index_format == crate::common::indices::Format::U16 {
        2
    } else {
        4
    };
    if layout.score(batch) == 0 {
        return batch.capacity().bytes();
    }
    layout
        .ranges(batch)
        .map(|range| {
            (range.vertices.len() * size_of::<mesh::Vertex>()
                + (range.indices.end * width).div_ceil(4) * 4
                - range.indices.start * width / 4 * 4) as u64
        })
        .sum()
}

pub(super) struct Buffers {
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    staging: Option<wgpu::Buffer>,
    mapping: Option<Arc<AtomicU8>>,
    pub layout: Option<crate::common::draw_batch::Layout>,
}
pub(super) struct UploadPolicy {
    pub reuse: bool,
    pub optional: std::cell::Cell<bool>,
}
impl UploadPolicy {
    pub fn new(reuse: bool) -> Self {
        Self {
            reuse,
            optional: std::cell::Cell::new(false),
        }
    }
}
pub(super) type Storage = Box<Allocation<Buffers>>;
type GlyphGeometry = crate::common::recording_cache::Cache<
    crate::common::recording_cache::GlyphDependency,
    crate::common::recording_cache::Glyphs<
        Arc<super::glyph_cache::Page>,
        Arc<super::glyph_cache::Glyph>,
    >,
>;
#[derive(Default)]
pub(super) struct Cache {
    pool: Mutex<Pool<Buffers>>,
    pub shapes: Mutex<
        crate::common::recording_cache::Cache<
            crate::common::recording_cache::ShapeDependency,
            Arc<crate::common::recording_cache::Mesh>,
        >,
    >,
    pub glyphs: Mutex<GlyphGeometry>,
    #[cfg(test)]
    pub uploaded_bytes: std::sync::atomic::AtomicU64,
}
impl Cache {
    pub fn upload_batch<T>(
        &self,
        gpu: &Gpu,
        budgets: &Budgets,
        local: &Budgets,
        batch: &crate::common::draw_batch::Batch<T>,
        policy: &UploadPolicy,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(Storage, BudgetTicket, Option<BudgetTicket>), GpuError> {
        let mut reused = self.take(batch, local, policy.reuse);
        #[cfg(test)]
        self.uploaded_bytes.fetch_add(
            upload_bytes(
                reused
                    .as_ref()
                    .and_then(|(storage, _)| storage.storage.layout.as_ref()),
                batch,
            ),
            Ordering::Relaxed,
        );
        let partial = if let Some((storage, _)) = &reused {
            storage
                .storage
                .upload_segments(encoder, storage.capacity, batch)?
                .is_some()
        } else {
            false
        };
        let (mut storage, local_ticket) = if partial {
            reused.take().unwrap()
        } else {
            let materialized = batch.materialize()?;
            let mesh = &*materialized;
            let indices = crate::common::indices::Upload::new(mesh, |bytes| {
                super::renderer::reserve(
                    &budgets.transient,
                    &local.transient,
                    bytes,
                    gpu.limits.transient_bytes,
                    "index packing",
                )
            })?;
            let (storage, ticket) = if let Some(storage) = reused.take() {
                storage
            } else {
                self.acquire(gpu, budgets, local, mesh, &indices, policy)?
            };
            storage
                .storage
                .upload(encoder, storage.capacity, mesh, indices.bytes())?;
            (storage, ticket)
        };
        let metadata = if storage.storage.has_staging() {
            crate::common::draw_batch::Layout::record(
                &mut storage.storage.layout,
                batch,
                budgets,
                local,
            )
        } else {
            None
        };
        Ok((storage, local_ticket, metadata))
    }
    pub fn take<T>(
        &self,
        batch: &crate::common::draw_batch::Batch<T>,
        local: &Budgets,
        reuse: bool,
    ) -> Option<(Storage, BudgetTicket)> {
        if !reuse {
            return None;
        }
        let storage = self
            .pool
            .lock()
            .unwrap()
            .take_matching(batch.capacity(), |storage| {
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
        Some((storage, ticket))
    }
    pub fn bytes(&self) -> u64 {
        let pool = self.pool.lock().unwrap();
        pool.bytes()
            + pool.additional_bytes(|storage| {
                storage.layout.as_ref().map_or(0, |layout| layout.bytes())
            })
            + self.shapes.lock().unwrap().bytes()
            + self.glyphs.lock().unwrap().bytes()
    }
    pub fn clear(&self) {
        self.pool.lock().unwrap().clear();
        self.shapes.lock().unwrap().clear();
        self.glyphs.lock().unwrap().clear();
    }
    pub fn recycle(&self, storage: &mut Vec<Storage>) {
        let mut pool = self.pool.lock().unwrap();
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
        indices: &crate::common::indices::Upload<'_, super::renderer::Reservation>,
        policy: &UploadPolicy,
    ) -> Result<(Storage, BudgetTicket), GpuError> {
        let requested = Capacity {
            vertices: std::mem::size_of_val(mesh.vertices.as_slice()) as u64,
            indices: indices.bytes().len() as u64,
            index_format: indices.format(),
        };
        let mut pool = self.pool.lock().unwrap();
        if policy.reuse
            && let Some(storage) = pool.take(requested)
            && let Ok(ticket) = local.transient.reserve(
                storage.ticket.bytes()
                    + storage
                        .storage
                        .layout
                        .as_ref()
                        .map_or(0, |layout| layout.bytes()),
            )
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
            reel_core::profile::geometry_upload(requested.vertices);
            reel_core::profile::geometry_upload(requested.indices);
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
                        contents: indices.bytes(),
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
                    layout: None,
                },
            }),
            local_ticket,
        ))
    }
}
impl Buffers {
    /// Return None when the ordered extents changed and require a full upload.
    /// Four-byte index copies include their unchanged neighbour in mapped storage.
    pub fn upload_segments<T>(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        capacity: Capacity,
        batch: &crate::common::draw_batch::Batch<T>,
    ) -> Result<Option<bool>, GpuError> {
        let Some(layout) = self
            .layout
            .as_ref()
            .filter(|layout| layout.compatible(batch))
        else {
            return Ok(None);
        };
        if layout.score(batch) == 0 {
            return Ok(None);
        }
        if batch
            .segments
            .iter()
            .enumerate()
            .all(|(index, segment)| layout.matches(index, segment.key))
        {
            return Ok(Some(false));
        }
        let Some(staging) = &self.staging else {
            return Ok(None);
        };
        let width = if capacity.index_format == crate::common::indices::Format::U16 {
            2
        } else {
            4
        };
        {
            let mut mapping = staging
                .slice(..)
                .get_mapped_range_mut()
                .map_err(|error| GpuError::Device(error.to_string()))?;
            let mut vertex = 0;
            let mut index = 0;
            for (part, segment) in batch.segments.iter().enumerate() {
                if !layout.matches(part, segment.key) {
                    let bytes = bytemuck::cast_slice(&segment.mesh.vertices);
                    let start = vertex * size_of::<mesh::Vertex>();
                    mapping
                        .slice(start..start + bytes.len())
                        .copy_from_slice(bytes);
                    let start = capacity.vertices as usize + index * width;
                    let mut destination =
                        mapping.slice(start..start + segment.mesh.indices.len() * width);
                    for (index, &value) in segment.mesh.indices.iter().enumerate() {
                        let mut slot = destination.slice(index * width..(index + 1) * width);
                        let value = vertex as u32 + value;
                        if width == 2 {
                            slot.copy_from_slice(&(value as u16).to_le_bytes());
                        } else {
                            slot.copy_from_slice(&value.to_le_bytes());
                        }
                    }
                } else if width == 2 {
                    // Re-emit the retained half-word included by an aligned
                    // neighbour copy; MAP_WRITE needs no preserved CPU contents.
                    let first = index % 2 != 0
                        && part > 0
                        && !layout.matches(part - 1, batch.segments[part - 1].key);
                    let last = (index + segment.key.indices) % 2 != 0
                        && part + 1 < batch.segments.len()
                        && !layout.matches(part + 1, batch.segments[part + 1].key);
                    for (element, include) in
                        [(0, first), (segment.key.indices.saturating_sub(1), last)]
                    {
                        if include {
                            let value = (vertex as u32 + segment.mesh.indices[element]) as u16;
                            let offset = capacity.vertices as usize + (index + element) * 2;
                            mapping
                                .slice(offset..offset + 2)
                                .copy_from_slice(&value.to_le_bytes());
                        }
                    }
                }
                vertex += segment.key.vertices;
                index += segment.key.indices;
            }
            if width == 2 && index % 2 != 0 {
                let offset = capacity.vertices as usize + index * 2;
                mapping.slice(offset..offset + 2).copy_from_slice(&[0; 2]);
            }
        }
        staging.unmap();
        for range in layout.ranges(batch) {
            let offset = (range.vertices.start * size_of::<mesh::Vertex>()) as u64;
            let vertex_bytes = (range.vertices.len() * size_of::<mesh::Vertex>()) as u64;
            encoder.copy_buffer_to_buffer(staging, offset, &self.vertices, offset, vertex_bytes);
            reel_core::profile::geometry_upload(vertex_bytes);
            let start = (range.indices.start * width) as u64 / 4 * 4;
            let end = ((range.indices.end * width) as u64).div_ceil(4) * 4;
            encoder.copy_buffer_to_buffer(
                staging,
                capacity.vertices + start,
                &self.indices,
                start,
                end - start,
            );
            reel_core::profile::geometry_upload(end - start);
        }
        self.remap(encoder, staging);
        Ok(Some(true))
    }
    fn remap(&self, encoder: &mut wgpu::CommandEncoder, staging: &wgpu::Buffer) {
        let state = self
            .mapping
            .as_ref()
            .expect("staging owns mapping state")
            .clone();
        state.store(PENDING, Ordering::Release);
        encoder.map_buffer_on_submit(staging, wgpu::MapMode::Write, .., move |result| {
            state.store(
                if result.is_ok() { MAPPED } else { FAILED },
                Ordering::Release,
            );
        });
    }
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
        indices: &[u8],
    ) -> Result<(), GpuError> {
        let Some(staging) = &self.staging else {
            return Ok(());
        };
        let vertices = bytemuck::cast_slice(&mesh.vertices);
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
        reel_core::profile::geometry_upload(vertices.len() as u64);
        encoder.copy_buffer_to_buffer(
            staging,
            capacity.vertices,
            &self.indices,
            0,
            indices.len() as u64,
        );
        reel_core::profile::geometry_upload(indices.len() as u64);
        // WGPU schedules this only when this exact encoder is submitted. Dropping
        // an unsubmitted Prepared drops both its copy commands and these owners.
        self.remap(encoder, staging);
        Ok(())
    }
}
