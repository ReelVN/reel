//! Sample native state in memory; the session writes it after closing playback.
use crate::probe::Recording;
use reel_render::gxm::GxmBackend;

pub(super) fn record_device(
    recording: &mut Recording,
    backend: &GxmBackend,
    contiguous: &reel_core::budget::BudgetPool,
) {
    let Ok(native) = backend.native_memory_usage() else {
        return;
    };
    let renderer = backend.usage();
    let timing = native.submission_timing;
    let main = backend.native_memory_state().main;
    let contiguous = contiguous.usage();
    recording.record_device([
        renderer.resources,
        renderer.targets,
        renderer.transient,
        renderer.cached_images,
        renderer.retired_images,
        renderer.retired_image_readers,
        native.pixels.reserved_bytes,
        native.pixels.used_bytes,
        u64::from(native.pixels.blocks),
        native.pixels.idle_bytes,
        native.buffers.reserved_bytes,
        native.buffers.used_bytes,
        u64::from(native.buffers.blocks),
        native.buffers.idle_bytes,
        native.parameter_cdram_bytes,
        native.context_ring_bytes,
        native.patcher_pool_bytes,
        native.patcher_host_bytes,
        native.render_target_driver_bytes,
        native.imported_video_bytes,
        u64::from(native.render_targets),
        u64::from(native.display_sync_objects),
        native.scene_submissions,
        native.notification_capacity_waits,
        native.display_queue_waits,
        native.display_flips,
        timing.scene_begin.calls,
        timing.scene_begin.cpu_us,
        timing.scene_end.calls,
        timing.scene_end.cpu_us,
        timing.uniform_reserve.calls,
        timing.uniform_reserve.cpu_us,
        timing.draw.calls,
        timing.draw.cpu_us,
        timing.notification_capacity_wait.calls,
        timing.notification_capacity_wait.cpu_us,
        timing.upload_capacity_wait.calls,
        timing.upload_capacity_wait.cpu_us,
        timing.resource_replacement_wait.calls,
        timing.resource_replacement_wait.cpu_us,
        native.main_textures.reserved_bytes,
        native.main_textures.used_bytes,
        u64::from(native.main_textures.blocks),
        native.main_textures.idle_bytes,
        native.display_reserved_bytes,
        main.committed_bytes,
        main.capacity_bytes,
        contiguous.bytes,
        contiguous.peak_bytes,
    ]);
}
