//! Reusable mapped texture uploads; a submitted buffer returns only after both
//! its GPU fence and map-on-submit callback have completed.
use super::{Gpu, GpuError, renderer::Budgets};
use reel_core::budget::BudgetTicket;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
const MAPPED: u8 = 0;
const PENDING: u8 = 1;
const FAILED: u8 = 2;
pub(super) struct Buffer {
    pub buffer: wgpu::Buffer,
    pub capacity: u64,
    state: Arc<AtomicU8>,
    _gpu: BudgetTicket,
    _metadata: BudgetTicket,
}
impl Buffer {
    pub fn settled(&self) -> bool {
        self.state.load(Ordering::Acquire) != PENDING
    }
    pub fn ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == MAPPED
    }
    pub fn remap(&self, encoder: &mut wgpu::CommandEncoder) {
        self.state.store(PENDING, Ordering::Release);
        let state = self.state.clone();
        encoder.map_buffer_on_submit(&self.buffer, wgpu::MapMode::Write, .., move |result| {
            state.store(
                if result.is_ok() { MAPPED } else { FAILED },
                Ordering::Release,
            );
        });
    }
}
pub(super) struct Cache {
    slots: [Option<Box<Buffer>>; 16],
    warmed: bool,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            warmed: false,
        }
    }
}
impl Cache {
    pub(super) fn warm(
        &mut self,
        gpu: &Gpu,
        budgets: &Budgets,
        viewport_upload: u64,
    ) -> Result<(), GpuError> {
        if self.warmed {
            return Ok(());
        }
        let alignment = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let bytes = gpu
            .limits
            .image_upload_bytes_per_prepare
            .min(viewport_upload)
            .min(gpu.limits.resource_bytes / 8)
            / alignment
            * alignment;
        if bytes == 0
            || self
                .slots
                .iter()
                .flatten()
                .filter(|slot| slot.capacity >= bytes)
                .count()
                >= 2
        {
            self.warmed = true;
            return Ok(());
        }
        let mut buffers = [None, None];
        for slot in &mut buffers {
            let allocating = self.capacity() < bytes;
            if allocating && bytes > budgets.resources.available_bytes() {
                break;
            }
            // WGPU initializes a fresh mapping when it creates the buffer.
            *slot = self.acquire(gpu, budgets, bytes)?;
        }
        for buffer in buffers.into_iter().flatten() {
            self.put(buffer);
        }
        self.warmed = true;
        Ok(())
    }
    pub fn capacity(&self) -> u64 {
        self.slots
            .iter()
            .flatten()
            .map(|b| b.capacity)
            .max()
            .unwrap_or(0)
    }
    pub fn clear(&mut self) {
        self.slots.iter_mut().for_each(|slot| *slot = None);
    }
    pub fn put(&mut self, buffer: Box<Buffer>) {
        if buffer.ready() {
            let index = if let Some(index) = self.slots.iter().position(Option::is_none) {
                index
            } else {
                let (index, smallest) = self
                    .slots
                    .iter()
                    .enumerate()
                    .map(|(index, slot)| (index, slot.as_ref().unwrap().capacity))
                    .min_by_key(|(_, capacity)| *capacity)
                    .unwrap();
                if buffer.capacity <= smallest {
                    return;
                }
                index
            };
            self.slots[index] = Some(buffer);
        }
    }
    pub fn acquire(
        &mut self,
        gpu: &Gpu,
        budgets: &Budgets,
        bytes: u64,
    ) -> Result<Option<Box<Buffer>>, GpuError> {
        if let Some(index) = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, b)| {
                let b = b.as_ref()?;
                (b.capacity >= bytes).then_some((i, b.capacity))
            })
            .min_by_key(|(_, n)| *n)
            .map(|(i, _)| i)
        {
            return Ok(self.slots[index].take());
        }
        let state_bytes = (2 * size_of::<usize>() + size_of::<AtomicU8>())
            .div_ceil(align_of::<usize>())
            * align_of::<usize>();
        let metadata = (size_of::<Buffer>() + state_bytes) as u64;
        // Exact-limit images retain the former one-off upload admission.
        let Ok(metadata) = budgets.transient.reserve(metadata) else {
            return Ok(None);
        };
        let ticket = budgets
            .resources
            .reserve(bytes)
            .map_err(|_| GpuError::Pressure {
                domain: "image upload",
                requested: bytes,
                available: gpu
                    .limits
                    .resource_bytes
                    .saturating_sub(budgets.resources.usage().bytes),
            })?;
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::BufferCreate);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("reusable image upload"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::MAP_WRITE,
            mapped_at_creation: true,
        });
        gpu.check()?;
        #[cfg(test)]
        gpu.upload_creations.staging.fetch_add(1, Ordering::Relaxed);
        Ok(Some(Box::new(Buffer {
            buffer,
            capacity: bytes,
            state: Arc::new(AtomicU8::new(MAPPED)),
            _gpu: ticket,
            _metadata: metadata,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "actual adapter; bounded capacity-aware upload cache"]
    fn small_upload_returns_preserve_large_reusable_buffers() {
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            super::super::DeviceGeneration(1),
            Default::default(),
        ))
        .unwrap();
        let budgets = Budgets::new(gpu.limits);
        let mut cache = Cache::default();
        let large = cache.acquire(&gpu, &budgets, 4 << 20).unwrap().unwrap();
        let address = (&*large as *const Buffer) as usize;
        let small: Vec<_> = (1..=16)
            .map(|index| cache.acquire(&gpu, &budgets, index * 256).unwrap().unwrap())
            .collect();
        cache.put(large);
        for buffer in small {
            cache.put(buffer);
        }
        let creations = gpu.upload_creations.staging.load(Ordering::Relaxed);
        let large = cache.acquire(&gpu, &budgets, 4 << 20).unwrap().unwrap();
        assert_eq!((&*large as *const Buffer) as usize, address);
        assert_eq!(
            gpu.upload_creations.staging.load(Ordering::Relaxed),
            creations
        );
        assert_eq!(
            budgets.resources.usage().bytes,
            (4 << 20) + (2..=16).sum::<u64>() * 256
        );
        cache.put(large);
        cache.clear();
        assert_eq!(budgets.resources.usage().bytes, 0);
        assert_eq!(budgets.transient.usage().bytes, 0);
        gpu.check().unwrap();
    }
    #[test]
    #[ignore = "requires actual graphics adapter; buffer submission only, no pixel readback"]
    fn completed_texture_upload_buffers_reuse_mapping_allocation_and_reservation() {
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            super::super::DeviceGeneration(1),
            Default::default(),
        ))
        .unwrap();
        let budgets = Budgets::new(gpu.limits);
        let target = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("upload cache test destination"),
            size: 4096,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut cache = Cache::default();
        let mut address = None;
        for index in 0..8 {
            let upload = cache.acquire(&gpu, &budgets, 4096).unwrap().unwrap();
            let actual = (&*upload as *const Buffer) as usize;
            if let Some(expected) = address {
                assert_eq!(actual, expected);
            } else {
                address = Some(actual);
            }
            upload
                .buffer
                .slice(..)
                .get_mapped_range_mut()
                .unwrap()
                .slice(..)
                .fill(index);
            upload.buffer.unmap();
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            encoder.copy_buffer_to_buffer(&upload.buffer, 0, &target, 0, 4096);
            upload.remap(&mut encoder);
            assert!(!upload.settled());
            gpu.queue.submit([encoder.finish()]);
            gpu.device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            assert!(upload.ready());
            cache.put(upload);
        }
        assert_eq!(gpu.upload_creations.staging.load(Ordering::Relaxed), 1);
        assert_eq!(budgets.resources.usage().bytes, 4096);
        cache.clear();
        assert_eq!(budgets.resources.usage().bytes, 0);
        assert_eq!(budgets.transient.usage().bytes, 0);
        eprintln!(
            "8 completed texture buffer uploads: staging_buffer_creations=1 (former=8), native_copy_calls=8, retained_capacity=4096"
        );
        let bytes = 4096;
        cache.warm(&gpu, &budgets, bytes).unwrap();
        assert_eq!(budgets.resources.usage().bytes, bytes * 2);
        let in_flight = cache.acquire(&gpu, &budgets, bytes).unwrap().unwrap();
        let creations = gpu.upload_creations.staging.load(Ordering::Relaxed);
        cache.warm(&gpu, &budgets, bytes * 2).unwrap();
        assert_eq!(
            gpu.upload_creations.staging.load(Ordering::Relaxed),
            creations
        );
        cache.put(in_flight);
        cache.clear();
        cache.warm(&gpu, &budgets, bytes).unwrap();
        assert_eq!(budgets.resources.usage().bytes, 0);
        assert_eq!(budgets.transient.usage().bytes, 0);
    }
}
