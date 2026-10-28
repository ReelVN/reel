/// Work concurrency and upload scheduling for a native device.
/// Storage admission follows its measured physical MAIN and CDRAM domains.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum outstanding render, capture and upload submissions combined.
    pub max_inflight: usize,
    /// Work slice in upload bytes. One complete row or compressed mip is the
    /// indivisible minimum, and may exceed this value while remaining budgeted.
    pub image_upload_bytes_per_prepare: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_inflight: 2,
            image_upload_bytes_per_prepare: 2 << 20,
        }
    }
}
impl Limits {
    pub(crate) fn common(
        self,
        memory: vita_native::graphics::MemoryState,
    ) -> crate::common::GpuLimits {
        let targets = memory
            .cdram
            .capacity_bytes
            .saturating_sub(memory.scanout_bytes);
        crate::common::GpuLimits {
            resource_bytes: memory.main.capacity_bytes.saturating_add(targets),
            target_bytes: targets,
            transient_bytes: memory.main.capacity_bytes,
            max_inflight: self.max_inflight,
            image_upload_bytes_per_prepare: self.image_upload_bytes_per_prepare,
        }
    }
}
