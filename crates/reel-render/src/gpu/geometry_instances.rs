//! Instance streams use the ordinary bounded pool and completion/remap lifecycle.
use super::*;
use crate::common::quad_instances::{Batch, INDICES, Template};
use wgpu::util::DeviceExt;

impl Cache {
    pub fn upload_instances(
        &self,
        gpu: &Gpu,
        budgets: &Budgets,
        local: &Budgets,
        batch: &Batch,
        policy: &UploadPolicy,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(Storage, BudgetTicket), GpuError> {
        let requested = Capacity {
            vertices: (size_of::<Template>() + size_of_val(batch.instances.as_slice())) as u64,
            indices: size_of_val(&INDICES) as u64,
            index_format: crate::common::indices::Format::U16,
        };
        let mut pool = self.pool.lock().unwrap();
        if policy.reuse
            && let Some(storage) = pool.take_eligible(requested, |b| b.instances.is_some(), |_| 0)
            && let Ok(ticket) = local.transient.reserve(storage.ticket.bytes())
        {
            storage.storage.upload_instances(encoder, batch)?;
            policy.optional.set(true);
            #[cfg(test)]
            self.uploaded_bytes
                .fetch_add(requested.vertices, Ordering::Relaxed);
            return Ok((storage, ticket));
        }
        let stage_bytes = requested.vertices.div_ceil(wgpu::MAP_ALIGNMENT) * wgpu::MAP_ALIGNMENT;
        let state_bytes = (2 * size_of::<usize>() + size_of::<AtomicU8>())
            .div_ceil(align_of::<usize>())
            * align_of::<usize>();
        let metadata = (size_of::<Allocation<Buffers>>() + size_of::<wgpu::Buffer>()) as u64;
        let limit = gpu.limits.transient_bytes;
        let reusable = policy.reuse
            && requested.bytes() + metadata + stage_bytes + state_bytes as u64
                <= limit.saturating_sub(local.transient.usage().bytes);
        let bytes = requested.bytes()
            + metadata
            + if reusable {
                stage_bytes + state_bytes as u64
            } else {
                0
            };
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
        let create = |label, contents, usage| {
            gpu.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents,
                    usage,
                })
        };
        let usage = wgpu::BufferUsages::VERTEX
            | if reusable {
                wgpu::BufferUsages::COPY_DST
            } else {
                wgpu::BufferUsages::empty()
            };
        let vertices = create(
            "reel quad template",
            bytemuck::bytes_of(&batch.template),
            usage,
        );
        let instances = Box::new(create(
            "reel quad instances",
            bytemuck::cast_slice(&batch.instances),
            usage,
        ));
        let indices = create(
            "reel unit quad indices",
            bytemuck::cast_slice(&INDICES),
            wgpu::BufferUsages::INDEX,
        );
        let staging = reusable.then(|| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reel instance upload"),
                size: stage_bytes,
                usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: true,
            })
        });
        gpu.check()?;
        reel_core::profile::geometry_upload(requested.bytes());
        #[cfg(test)]
        {
            self.uploaded_bytes
                .fetch_add(requested.bytes(), Ordering::Relaxed);
            gpu.upload_creations
                .mesh_buffers
                .fetch_add(3, Ordering::Relaxed);
            gpu.upload_creations
                .geometry_staging
                .fetch_add(usize::from(reusable), Ordering::Relaxed);
        }
        if reusable {
            policy.optional.set(true);
        }
        Ok((
            Box::new(Allocation {
                capacity: requested,
                ticket,
                storage: Buffers {
                    vertices,
                    indices,
                    instances: Some(instances),
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
    fn upload_instances(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        batch: &Batch,
    ) -> Result<(), GpuError> {
        let staging = self
            .staging
            .as_ref()
            .expect("only reusable instance buffers enter pool");
        let template_bytes = size_of::<Template>() as u64;
        let instance_bytes = size_of_val(batch.instances.as_slice()) as u64;
        {
            let mut mapped = staging
                .slice(..)
                .get_mapped_range_mut()
                .map_err(|e| GpuError::Device(e.to_string()))?;
            mapped
                .slice(0..template_bytes as usize)
                .copy_from_slice(bytemuck::bytes_of(&batch.template));
            mapped
                .slice(template_bytes as usize..(template_bytes + instance_bytes) as usize)
                .copy_from_slice(bytemuck::cast_slice(&batch.instances));
        }
        staging.unmap();
        encoder.copy_buffer_to_buffer(staging, 0, &self.vertices, 0, template_bytes);
        encoder.copy_buffer_to_buffer(
            staging,
            template_bytes,
            self.instances.as_deref().unwrap(),
            0,
            instance_bytes,
        );
        self.remap(encoder, staging);
        reel_core::profile::geometry_upload(template_bytes + instance_bytes);
        Ok(())
    }
}
