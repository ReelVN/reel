/// Device resource ceilings, all sharing one simultaneous-memory limit.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Cached image, glyph atlas and persistent draw resources, in bytes.
    pub resource_bytes: u64,
    /// Simultaneous physical RGBA8 render targets, in bytes. Defaults to 64 MiB,
    /// leaving 24 MiB of the native CDRAM pixel arena for glyphs and other pixels.
    /// Storage is charged on demand within the shared total ceiling.
    pub target_bytes: u64,
    /// CPU staging, in-flight geometry and evictable completed geometry, in bytes.
    pub transient_bytes: u64,
    /// Shared ceiling across all three pools, in bytes.
    pub total_bytes: u64,
    /// Maximum outstanding render, capture and upload submissions combined.
    pub max_inflight: usize,
    /// Work slice in upload bytes. One complete row or compressed mip is the
    /// indivisible minimum, and may exceed this value while remaining budgeted.
    pub image_upload_bytes_per_prepare: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            resource_bytes: 64 << 20,
            target_bytes: 64 << 20,
            transient_bytes: 8 << 20,
            total_bytes: 96 << 20,
            max_inflight: 2,
            image_upload_bytes_per_prepare: 2 << 20,
        }
    }
}
impl Limits {
    pub(crate) fn common(self) -> crate::common::GpuLimits {
        crate::common::GpuLimits {
            resource_bytes: self.resource_bytes,
            target_bytes: self.target_bytes,
            transient_bytes: self.transient_bytes,
            max_inflight: self.max_inflight,
            image_upload_bytes_per_prepare: self.image_upload_bytes_per_prepare,
        }
    }
}
