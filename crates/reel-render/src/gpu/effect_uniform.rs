//! Exclusive, budgeted effect-uniform slots recycled after GPU completion.
use super::{
    Gpu, GpuError, effect::Compiled, renderer::Budgets, resources::Image, targets::Target,
};
use reel_core::budget::{BudgetPool, BudgetTicket};
use std::{
    num::NonZeroU64,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Layout {
    context: u64,
    params: u64,
    metadata: u64,
    parameter_bytes: u64,
    metadata_bytes: u64,
    capacity: u64,
}
impl Layout {
    fn new(gpu: &Gpu, parameters: usize, images: usize) -> Result<Self, GpuError> {
        let align = u64::from(gpu.device.limits().min_uniform_buffer_offset_alignment).max(1);
        let round = |v: u64| {
            v.checked_add(align - 1)
                .map(|v| v / align * align)
                .ok_or(GpuError::InvalidSize)
        };
        let parameter_bytes = (parameters.max(1) as u64)
            .checked_mul(16)
            .ok_or(GpuError::InvalidSize)?;
        let metadata_bytes = (images as u64)
            .checked_mul(32)
            .ok_or(GpuError::InvalidSize)?;
        let params = round(160)?;
        let metadata = round(
            params
                .checked_add(parameter_bytes)
                .ok_or(GpuError::InvalidSize)?,
        )?;
        let capacity = metadata
            .checked_add(metadata_bytes)
            .ok_or(GpuError::InvalidSize)?
            .max(params + parameter_bytes);
        if capacity > gpu.device.limits().max_buffer_size {
            return Err(GpuError::InvalidSize);
        }
        Ok(Self {
            context: 0,
            params,
            metadata,
            parameter_bytes,
            metadata_bytes,
            capacity,
        })
    }
}

pub(super) type Storage = Box<Slot>;
pub(super) struct Slot {
    layout: Layout,
    buffer: wgpu::Buffer,
    previous: Vec<u8>,
    pending: Vec<u8>,
    initialized: bool,
    group: Option<wgpu::BindGroup>,
    compiled: Option<Arc<Compiled>>,
    source: Option<Arc<super::target_cache::Allocation>>,
    coverage: Option<Arc<super::target_cache::Allocation>>,
    images: Vec<Arc<Image>>,
    reusable: bool,
    idle_since: u64,
    generation: u64,
    _ticket: BudgetTicket,
}
impl Slot {
    fn binding(&self, offset: u64, size: u64) -> wgpu::BindingResource<'_> {
        wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: &self.buffer,
            offset,
            size: NonZeroU64::new(size),
        })
    }
    fn matches(
        &self,
        compiled: &Arc<Compiled>,
        source: Option<&Target>,
        coverage: Option<&Target>,
        images: &[Arc<Image>],
    ) -> bool {
        self.compiled
            .as_ref()
            .is_some_and(|v| Arc::ptr_eq(v, compiled))
            && self.source.as_ref().map(|v| &v.target.view) == source.map(|v| &v.view)
            && self.coverage.as_ref().map(|v| &v.target.view) == coverage.map(|v| &v.view)
            && self.images.len() == images.len()
            && self
                .images
                .iter()
                .zip(images)
                .all(|(a, b)| a.view == b.view)
    }
    pub(super) fn group(&self) -> &wgpu::BindGroup {
        self.group.as_ref().expect("effect group prepared")
    }
    pub(super) fn upload(&mut self, gpu: &Gpu) {
        for (offset, len) in [
            (self.layout.context, 160),
            (self.layout.params, self.layout.parameter_bytes),
            (self.layout.metadata, self.layout.metadata_bytes),
        ] {
            if len == 0 {
                continue;
            }
            let range = offset as usize..(offset + len) as usize;
            if !self.initialized || self.previous[range.clone()] != self.pending[range.clone()] {
                gpu.queue
                    .write_buffer(&self.buffer, offset, &self.pending[range.clone()]);
                self.previous[range.clone()].copy_from_slice(&self.pending[range]);
                #[cfg(test)]
                gpu.upload_creations
                    .effect_uniform_bytes
                    .fetch_add(len, std::sync::atomic::Ordering::Relaxed);
            }
        }
        self.initialized = true;
    }
}

#[derive(Default)]
struct State {
    slots: Vec<Storage>,
    storage: Option<BudgetTicket>,
    submitted: u64,
    cursor: usize,
    generation: u64,
}
impl State {
    fn clear(&mut self) {
        self.slots = Vec::new();
        self.storage = None;
        self.cursor = 0;
        self.generation = self.generation.wrapping_add(1);
    }
}
#[derive(Default)]
pub(super) struct Cache(Mutex<State>, #[cfg(test)] std::sync::atomic::AtomicBool);
impl Cache {
    #[cfg(test)]
    pub(super) fn disable_for_test(&self, value: bool) {
        self.1.store(value, std::sync::atomic::Ordering::Relaxed);
        self.clear();
    }
    pub(super) fn clear(&self) {
        let mut state = self.0.lock().unwrap();
        state.clear();
    }
    #[cfg(test)]
    pub(super) fn test_len(&self) -> usize {
        self.0.lock().unwrap().slots.len()
    }
    pub(super) fn submitted(&self) {
        let mut state = self.0.lock().unwrap();
        state.submitted = state.submitted.saturating_add(1);
        if state.slots.is_empty() {
            state.cursor = 0;
            return;
        }
        let index = state.cursor % state.slots.len();
        if state
            .submitted
            .saturating_sub(state.slots[index].idle_since)
            >= u64::from(crate::common::target_cache::MAX_IDLE_SUBMISSIONS)
        {
            state.slots.swap_remove(index);
            if state.slots.is_empty() {
                state.slots = Vec::new();
                state.storage = None;
                state.cursor = 0;
            } else {
                state.cursor = index % state.slots.len();
            }
        } else {
            state.cursor = (index + 1) % state.slots.len();
        }
    }
    pub(super) fn recycle(&self, slots: &mut Vec<Storage>, pool: &BudgetPool) {
        let mut state = self.0.lock().unwrap();
        for mut slot in slots.drain(..) {
            if !slot.reusable || slot.generation != state.generation {
                continue;
            }
            if state.slots.len() == state.slots.capacity() {
                let old = state.slots.capacity();
                let Some(capacity) = old.max(1).checked_mul(2) else {
                    continue;
                };
                let Some(peak) = old
                    .checked_add(capacity)
                    .and_then(|n| n.checked_mul(size_of::<Box<Slot>>()))
                else {
                    continue;
                };
                if let Some(ticket) = &mut state.storage {
                    if ticket.resize(peak as u64).is_err() {
                        continue;
                    }
                } else {
                    let Ok(ticket) = pool.reserve(peak as u64) else {
                        continue;
                    };
                    state.storage = Some(ticket);
                }
                let additional = capacity - state.slots.len();
                state.slots.reserve_exact(additional);
                let bytes = (state.slots.capacity() * size_of::<Box<Slot>>()) as u64;
                state
                    .storage
                    .as_mut()
                    .unwrap()
                    .resize(bytes)
                    .expect("admitted slot metadata");
            }
            slot.idle_since = state.submitted;
            state.slots.push(slot);
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "effect binding and its exact owned dependencies"
    )]
    pub(super) fn prepare(
        &self,
        gpu: &Gpu,
        budgets: &Budgets,
        local: &Budgets,
        compiled: Arc<Compiled>,
        source: Option<&Target>,
        coverage: Option<&Target>,
        images: Vec<Arc<Image>>,
        context: &[[f32; 4]; 10],
        parameters: &[reel_ui::stage::effect::EffectValue],
        reuse: bool,
        sampler: &wgpu::Sampler,
    ) -> Result<(Storage, BudgetTicket), GpuError> {
        #[cfg(test)]
        if self.1.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(GpuError::Budget {
                domain: "transient",
                requested: 0,
                available: 0,
            });
        }
        let layout = Layout::new(gpu, parameters.len(), images.len())?;
        let mut state = self.0.lock().unwrap();
        let position = reuse
            .then(|| {
                state
                    .slots
                    .iter()
                    .position(|slot| {
                        slot.layout == layout && slot.matches(&compiled, source, coverage, &images)
                    })
                    .or_else(|| state.slots.iter().position(|slot| slot.layout == layout))
            })
            .flatten();
        let mut slot = if let Some(index) = position {
            state.slots.swap_remove(index)
        } else {
            // One aligned GPU buffer, two same-capacity CPU snapshots, and
            // the maximum actual unpadded staging payload for one submission.
            let bytes = layout
                .capacity
                .checked_mul(3)
                .and_then(|n| n.checked_add(160))
                .and_then(|n| n.checked_add(layout.parameter_bytes))
                .and_then(|n| n.checked_add(layout.metadata_bytes))
                .and_then(|n| n.checked_add(size_of::<Slot>() as u64))
                .and_then(|n| n.checked_add((images.len() * size_of::<Arc<Image>>()) as u64))
                .ok_or(GpuError::InvalidSize)?;
            let ticket = match budgets.transient.reserve(bytes) {
                Ok(ticket) => ticket,
                Err(_) => {
                    state.clear();
                    budgets
                        .transient
                        .reserve(bytes)
                        .map_err(|_| GpuError::Pressure {
                            domain: "transient",
                            requested: bytes,
                            available: gpu
                                .limits
                                .transient_bytes
                                .saturating_sub(budgets.transient.usage().bytes),
                        })?
                }
            };
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("effect reusable uniforms"),
                size: layout.capacity,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            #[cfg(test)]
            gpu.upload_creations
                .effect_uniform_buffers
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::new(Slot {
                layout,
                buffer,
                previous: vec![0; layout.capacity as usize],
                pending: vec![0; layout.capacity as usize],
                initialized: false,
                group: None,
                compiled: None,
                source: None,
                coverage: None,
                images: Vec::with_capacity(images.len()),
                reusable: reuse,
                idle_since: 0,
                generation: state.generation,
                _ticket: ticket,
            })
        };
        slot.generation = state.generation;
        let local_ticket =
            local
                .transient
                .reserve(slot._ticket.bytes())
                .map_err(|_| GpuError::Budget {
                    domain: "transient",
                    requested: slot._ticket.bytes(),
                    available: gpu
                        .limits
                        .transient_bytes
                        .saturating_sub(local.transient.usage().bytes),
                })?;
        drop(state);
        let allocation = |target: Option<&Target>| {
            target.and_then(|target| target.allocation.as_ref()?.upgrade())
        };
        let source_owner = allocation(source);
        let coverage_owner = allocation(coverage);
        slot.reusable = reuse
            && (source.is_none() || source_owner.is_some())
            && (coverage.is_none() || coverage_owner.is_some());
        let same = slot.matches(&compiled, source, coverage, &images);
        if !same {
            let mut entries = Vec::with_capacity(
                3 + usize::from(source.is_some())
                    + usize::from(coverage.is_some())
                    + images.len()
                    + usize::from(!images.is_empty()),
            );
            entries.extend([
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: slot.binding(layout.context, 160),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: slot.binding(layout.params, layout.parameter_bytes),
                },
            ]);
            if let Some(source) = source {
                entries.push(wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source.view),
                });
            }
            if let Some(coverage) = coverage {
                entries.push(wgpu::BindGroupEntry {
                    binding: 5 + images.len() as u32,
                    resource: wgpu::BindingResource::TextureView(&coverage.view),
                });
            }
            for (index, image) in images.iter().enumerate() {
                entries.push(wgpu::BindGroupEntry {
                    binding: 5 + index as u32,
                    resource: wgpu::BindingResource::TextureView(&image.view),
                });
            }
            if !images.is_empty() {
                entries.push(wgpu::BindGroupEntry {
                    binding: 4,
                    resource: slot.binding(layout.metadata, layout.metadata_bytes),
                });
            }
            slot.group = Some(gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("reusable effect bindings"),
                layout: &compiled.layout,
                entries: &entries,
            }));
            #[cfg(test)]
            gpu.upload_creations
                .effect_bind_groups
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            slot.compiled = Some(compiled);
            slot.source = source_owner;
            slot.coverage = coverage_owner;
            slot.images.clear();
            slot.images.extend(images.iter().cloned());
        }
        slot.pending.fill(0);
        slot.pending[..160].copy_from_slice(bytemuck::cast_slice(context));
        for (index, value) in parameters.iter().enumerate() {
            let start = layout.params as usize + index * 16;
            slot.pending[start..start + 16].copy_from_slice(bytemuck::cast_slice(
                &crate::common::effect_plan::slot(*value),
            ));
        }
        for (index, image) in images.iter().enumerate() {
            let start = layout.metadata as usize + index * 32;
            let metadata = crate::common::effect_plan::texture_metadata(
                image.layout,
                image.origin,
                image.encoding,
                image.texture_format,
            );
            slot.pending[start..start + 32].copy_from_slice(bytemuck::cast_slice(&metadata));
        }
        Ok((slot, local_ticket))
    }
}

/// One-off path preserves admission when the aligned reusable slot cannot fit.
#[expect(clippy::too_many_arguments, reason = "one complete effect binding")]
pub(super) fn fallback(
    gpu: &Gpu,
    compiled: &Compiled,
    source: Option<&Target>,
    coverage: Option<&Target>,
    images: &[Arc<Image>],
    context: &[[f32; 4]; 10],
    parameters: &[reel_ui::stage::effect::EffectValue],
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    use wgpu::util::DeviceExt;
    #[cfg(test)]
    {
        use std::sync::atomic::Ordering;
        gpu.upload_creations
            .effect_uniform_buffers
            .fetch_add(2 + usize::from(!images.is_empty()), Ordering::Relaxed);
        gpu.upload_creations
            .effect_bind_groups
            .fetch_add(1, Ordering::Relaxed);
        gpu.upload_creations.effect_uniform_bytes.fetch_add(
            (160 + parameters.len().max(1) * 16 + images.len() * 32) as u64,
            Ordering::Relaxed,
        );
    }
    let parameter_slots: Vec<_> = if parameters.is_empty() {
        vec![[0.; 4]]
    } else {
        parameters
            .iter()
            .copied()
            .map(crate::common::effect_plan::slot)
            .collect()
    };
    let make = |label, data: &[u8]| {
        gpu.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: data,
                usage: wgpu::BufferUsages::UNIFORM,
            })
    };
    let context = make("effect coordinates", bytemuck::cast_slice(context));
    let params = make("effect parameters", bytemuck::cast_slice(&parameter_slots));
    let mut entries = Vec::with_capacity(
        3 + usize::from(source.is_some())
            + usize::from(coverage.is_some())
            + images.len()
            + usize::from(!images.is_empty()),
    );
    entries.extend([
        wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::Sampler(sampler),
        },
        wgpu::BindGroupEntry {
            binding: 2,
            resource: context.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 3,
            resource: params.as_entire_binding(),
        },
    ]);
    if let Some(source) = source {
        entries.push(wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&source.view),
        });
    }
    if let Some(coverage) = coverage {
        entries.push(wgpu::BindGroupEntry {
            binding: 5 + images.len() as u32,
            resource: wgpu::BindingResource::TextureView(&coverage.view),
        });
    }
    let mut metadata = Vec::with_capacity(images.len() * 2);
    for (index, image) in images.iter().enumerate() {
        entries.push(wgpu::BindGroupEntry {
            binding: 5 + index as u32,
            resource: wgpu::BindingResource::TextureView(&image.view),
        });
        metadata.extend(crate::common::effect_plan::texture_metadata(
            image.layout,
            image.origin,
            image.encoding,
            image.texture_format,
        ));
    }
    let metadata = (!metadata.is_empty())
        .then(|| make("effect image metadata", bytemuck::cast_slice(&metadata)));
    if let Some(metadata) = &metadata {
        entries.push(wgpu::BindGroupEntry {
            binding: 4,
            resource: metadata.as_entire_binding(),
        });
    }
    gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("one-off effect bindings"),
        layout: &compiled.layout,
        entries: &entries,
    })
}
