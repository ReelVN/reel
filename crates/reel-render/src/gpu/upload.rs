//! Reusable mapped texture uploads; a submitted buffer returns only after both
//! its GPU fence and map-on-submit callback have completed.
use super::{Gpu, GpuError, renderer::Budgets};
use reel_core::budget::BudgetTicket;
use std::ops::Range;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
};
const MAPPED: u8 = 0;
const PENDING: u8 = 1;
const FAILED: u8 = 2;
pub(super) struct Buffer {
    pub buffer: wgpu::Buffer,
    pub capacity: u64,
    used: u64,
    state: Arc<AtomicU8>,
    _gpu: BudgetTicket,
    _metadata: BudgetTicket,
}
#[derive(Clone, Copy)]
pub(super) struct Span<'a> {
    pub buffer: &'a wgpu::Buffer,
    pub offset: u64,
}

impl Buffer {
    pub fn settled(&self) -> bool {
        self.state.load(Ordering::Acquire) != PENDING
    }
    pub fn ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == MAPPED
    }
    pub fn remaining(&self) -> u64 {
        if self.ready() {
            self.capacity - self.used
        } else {
            0
        }
    }
    /// Image spans are whole padded rows, so every offset stays copy-aligned.
    pub fn allocate_span(&mut self, bytes: u64) -> Option<Span<'_>> {
        if bytes > self.remaining() {
            return None;
        }
        let offset = self.used;
        self.used += bytes;
        Some(Span {
            buffer: &self.buffer,
            offset,
        })
    }
    pub fn finish_spans(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.used != 0 {
            self.buffer.unmap();
            self.remap(encoder);
        }
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

/// Owns all staging charged to one submission, including its in-flight mappings.
pub(super) struct Batch {
    #[allow(
        clippy::vec_box,
        reason = "the cache and submissions move the same buffer boxes"
    )]
    pub buffers: Vec<Box<Buffer>>,
    /// The bounded metadata reservation may only cover one additional buffer per image.
    split: bool,
    _one_off: Vec<BudgetTicket>,
}

impl Batch {
    pub fn new(images: usize, slots: usize) -> Self {
        Self {
            buffers: Vec::with_capacity(slots),
            split: slots > images,
            _one_off: Vec::with_capacity(images),
        }
    }

    pub fn one_off(&mut self, ticket: BudgetTicket) {
        self._one_off.push(ticket);
    }

    pub fn reusable_row_bytes(&self, cache: &Cache, stride: u64) -> u64 {
        let open = self
            .buffers
            .iter()
            .map(|buffer| buffer.remaining() / stride);
        let rows = if self.split {
            open.sum::<u64>() + cache.rows_available(stride)
        } else {
            self.largest_span(cache) / stride
        };
        rows * stride
    }

    fn largest_span(&self, cache: &Cache) -> u64 {
        self.buffers
            .iter()
            .map(|buffer| buffer.remaining())
            .max()
            .unwrap_or(0)
            .max(cache.capacity())
    }

    /// A whole image still becomes several copies when only shorter spans
    /// are mapped and the batch can reuse them.
    pub fn splits_rows(&self, cache: &Cache, stride: u64, bytes: u64) -> bool {
        self.split && (stride..bytes).contains(&self.largest_span(cache))
    }

    pub fn new_bytes(&self, requested: u64, reusable: u64) -> u64 {
        if self.split {
            requested.saturating_sub(reusable)
        } else if requested <= reusable {
            0
        } else {
            requested
        }
    }

    pub fn row_capacity(&self, cache: &Cache, stride: u64, unallocated: u64) -> u64 {
        let reusable = self.reusable_row_bytes(cache, stride);
        if self.split {
            reusable.saturating_add(unallocated)
        } else {
            reusable.max(unallocated)
        }
    }

    pub fn copy_rows(
        &mut self,
        gpu: &Gpu,
        budgets: &Budgets,
        cache: &Mutex<Cache>,
        (rows, stride): (Range<u32>, u64),
        mut copy: impl FnMut(Range<u32>, Option<Span<'_>>) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        let bytes = u64::from(rows.end - rows.start) * stride;
        if let Some(span) = self
            .buffers
            .iter_mut()
            .find_map(|buffer| buffer.allocate_span(bytes))
        {
            return copy(rows, Some(span));
        }
        let mut cache = cache.lock().unwrap();
        if let Some(mut buffer) = cache.take(bytes) {
            drop(cache);
            copy(rows, buffer.allocate_span(bytes))?;
            self.buffers.push(buffer);
            return Ok(());
        }

        // Admit the uncovered rows before using any reusable ranges. If this
        // fails, the caller can still submit the preceding complete copies.
        let missing = self.new_bytes(bytes, self.reusable_row_bytes(&cache, stride));
        let mut tail = if missing == 0 {
            None
        } else {
            Buffer::allocate(gpu, budgets, missing)?
        };
        if missing != 0 && tail.is_none() {
            self.one_off(reserve(gpu, budgets, missing)?);
        }
        let mapped = bytes - missing;
        let mut open = self
            .buffers
            .iter()
            .map(|buffer| buffer.remaining() / stride * stride)
            .sum::<u64>();
        while open < mapped {
            let buffer = cache.take_largest();
            open += buffer.capacity / stride * stride;
            self.buffers.push(buffer);
        }
        drop(cache);

        let mapped_end = rows.start + (mapped / stride) as u32;
        let mut row = rows.start;
        for buffer in &mut self.buffers {
            let count = (buffer.remaining() / stride).min(u64::from(mapped_end - row)) as u32;
            if count != 0 {
                copy(
                    row..row + count,
                    buffer.allocate_span(u64::from(count) * stride),
                )?;
                row += count;
            }
            if row == mapped_end {
                break;
            }
        }
        debug_assert_eq!(row, mapped_end);
        if missing != 0 {
            copy(
                row..rows.end,
                tail.as_mut()
                    .and_then(|buffer| buffer.allocate_span(missing)),
            )?;
            self.buffers.extend(tail);
        }
        Ok(())
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
    pub fn mapped_count(&self) -> usize {
        self.slots.iter().flatten().count()
    }
    fn rows_available(&self, row_bytes: u64) -> u64 {
        self.slots
            .iter()
            .flatten()
            .map(|buffer| buffer.capacity / row_bytes)
            .sum()
    }
    fn take_largest(&mut self) -> Box<Buffer> {
        let (index, _) = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| Some((index, slot.as_ref()?.capacity)))
            .max_by_key(|&(_, capacity)| capacity)
            .expect("enough mapped image rows");
        self.slots[index].take().unwrap()
    }
    pub fn clear(&mut self) {
        self.slots.iter_mut().for_each(|slot| *slot = None);
    }
    pub fn put(&mut self, mut buffer: Box<Buffer>) {
        if buffer.ready() {
            buffer.used = 0;
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
        if let Some(buffer) = self.take(bytes) {
            return Ok(Some(buffer));
        }
        Buffer::allocate(gpu, budgets, bytes)
    }

    fn take(&mut self, bytes: u64) -> Option<Box<Buffer>> {
        let index = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, b)| {
                let b = b.as_ref()?;
                (b.capacity >= bytes).then_some((i, b.capacity))
            })
            .min_by_key(|(_, n)| *n)
            .map(|(i, _)| i)?;
        self.slots[index].take()
    }
}

pub(super) fn reserve(gpu: &Gpu, budgets: &Budgets, bytes: u64) -> Result<BudgetTicket, GpuError> {
    budgets
        .resources
        .reserve(bytes)
        .map_err(|_| GpuError::Pressure {
            domain: "image upload",
            requested: bytes,
            available: gpu
                .limits
                .resource_bytes
                .saturating_sub(budgets.resources.usage().bytes),
        })
}

impl Buffer {
    fn allocate(gpu: &Gpu, budgets: &Budgets, bytes: u64) -> Result<Option<Box<Self>>, GpuError> {
        let state_bytes = (2 * size_of::<usize>() + size_of::<AtomicU8>())
            .div_ceil(align_of::<usize>())
            * align_of::<usize>();
        let metadata = (size_of::<Buffer>() + state_bytes) as u64;
        // Exact-limit images retain the former one-off upload admission.
        let Ok(metadata) = budgets.transient.reserve(metadata) else {
            return Ok(None);
        };
        let ticket = reserve(gpu, budgets, bytes)?;
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::ImageStagingCreate);
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
            used: 0,
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
