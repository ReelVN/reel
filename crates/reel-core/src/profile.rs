//! Optional runtime profiling for host and device measurements.
//! Nested phases overlap. Worker durations are attributed when work completes;
//! their totals can exceed the main thread's elapsed time. Normal builds erase
//! scopes and counters; profiling builds use fixed atomic arrays without IO.

macro_rules! profile_phases {
    ($( $(#[$meta:meta])* $phase:ident => $name:literal, )+) => {
        /// Stages recorded by the shared runtime and device backend.
        #[derive(Clone, Copy)]
        #[repr(usize)]
        pub enum Phase { $( $(#[$meta])* $phase, )+ }

        /// Report column names in phase index order.
        pub const NAMES: [&str; [$($name),+].len()] = [$($name),+];
    };
}

profile_phases! {
    Input => "input",
    Tick => "tick",
    /// Session scheduling, interpreter slices and native host work.
    Session => "session",
    UiSync => "ui_sync",
    Record => "record",
    Compile => "compile",
    ImagePlan => "image_plan",
    Upload => "upload",
    Replay => "replay",
    GlyphRaster => "glyph_raster",
    Draw => "draw",
    Present => "present",
    Read => "read",
    Decode => "decode",
    TargetCreate => "target_create",
    BufferCreate => "buffer_create",
    Poll => "poll",
    Load => "load",
    Mount => "mount",
    Sampler => "sampler",
    Composite => "composite",
    Screen => "screen",
    Clear => "clear",
    SourceComposite => "source_composite",
    CoverageComposite => "coverage_composite",
    EffectComposite => "effect_composite",
    AudioMix => "audio_mix",
    AudioDecode => "audio_decode",
    VideoOpen => "video_open",
    VideoDecode => "video_decode",
    OffscreenPlan => "offscreen_plan",
    CompositeBatch => "composite_batch",
    RetainedPlan => "retained_plan",
    /// Interpreter slices, including VM builtins; excludes external host dispatch.
    VmSlice => "vm_slice",
    /// External native dispatch, including continuation resumes.
    HostCall => "host_call",
    /// Ordered copies between GPU-resident textures; includes submission CPU time.
    TextureCopy => "texture_copy",
    /// Sampling native controller and touch state, before input dispatch.
    ControlPoll => "control_poll",
    /// Host event wait, including timeout and scheduler wake latency.
    HostWait => "host_wait",
    /// Offscreen payload admission, nested within TargetCreate.
    TargetReserve => "target_reserve",
    /// Native offscreen pixel allocation, nested within TargetCreate.
    TargetTexture => "target_texture",
    /// Native surface and scheduling owner creation, nested within TargetCreate.
    TargetFramebuffer => "target_framebuffer",
    /// Successfully constructed targets; instantaneous count, zero duration.
    TargetCreated => "target_created",
    /// Cross-frame scratch cache hits; instantaneous count, zero duration.
    TargetReused => "target_reused",
    /// Native decoded pictures returned before timestamp lookahead.
    VideoDecoded => "video_decoded",
    /// Pictures published into the playback queue.
    VideoQueued => "video_queued",
    /// Pictures selected for the current playback position.
    VideoSelected => "video_selected",
    /// Queued pictures superseded by another already-due picture.
    VideoDropped => "video_dropped",
    /// Video pictures installed into the stage model.
    VideoStage => "video_stage",
    /// New stage revisions captured by the player UI.
    ProjectionChanged => "projection_changed",
    /// UI revisions published after resource preparation.
    ProjectionPublished => "projection_published",
    /// Model invalidations delivered to presentation scheduling.
    PresentationInvalidated => "presentation_invalidated",
    /// Project committed VM properties into the shared stage.
    UiStageSync => "ui_stage_sync",
    /// Build the player's immutable stage view and resource demand identity.
    UiStageView => "ui_stage_view",
    /// Resolve visible image requests, including candidate layout when needed.
    UiImageRequests => "ui_image_requests",
    /// Plan visible tiles and prepare their resource leases.
    UiImageTiles => "ui_image_tiles",
    /// Apply a stage view to the retained widget tree.
    UiWidgetSync => "ui_widget_sync",
    /// Incremental VM collection driven by the session scheduler.
    SessionGc => "session_gc",
    /// Session activity/time updates, including bounded animation graph work.
    SessionActivity => "session_activity",
    /// Task selection/maintenance and VM slices; excludes later host dispatch.
    SessionSchedule => "session_schedule",
    /// Project VM node properties into the authored stage; nested in its caller.
    StageProperties => "stage_properties",
    /// Synchronous session diagnostic formatting and stderr output, nested in Session.
    SessionDiagnostics => "session_diagnostics",
    /// Complete backend render/present call, including its nested preparation and submission.
    RenderSubmission => "render_submission",
    /// Session host maintenance before clock and interpreter work; may contain nested services.
    SessionMaintenance => "session_maintenance",
    /// Begin a VM collection; incremental collection slices use SessionGc.
    SessionGcBegin => "session_gc_begin",
    /// Poll media and deliver its session completions, nested in SessionMaintenance.
    SessionMedia => "session_media",
    /// Advance save requests and storage completions, nested in SessionMaintenance.
    SessionSaves => "session_saves",
    /// Poll localization and script asset reads, nested in SessionMaintenance.
    SessionAssets => "session_assets",
    /// Finish native GPU encoding and submit work; may wait for driver capacity.
    RenderFence => "render_fence",
    /// Native draw call wall time, including driver backpressure.
    DriverDraw => "driver_draw",
    /// Native clear call wall time, including driver backpressure.
    DriverClear => "driver_clear",
    /// Main-thread diagnostic collection and recording.
    Observer => "observer",
    /// Successfully admitted ordered effect chains; instantaneous count.
    MacrotileChain => "macrotile_chain",
    /// Effect outputs selected from ordered macrotile cells; instantaneous count.
    MacrotilePass => "macrotile_pass",
    /// Error recovery waiting for all native graphics, transfer and display work.
    RenderDrain => "render_drain",
    /// Physical pressure before drawing begins; instantaneous count.
    RenderPreparationPressure => "render_preparation_pressure",
    /// Physical pressure after drawing begins; instantaneous count.
    RenderStreamPressure => "render_stream_pressure",
    /// Scratch acquisitions during drawing beyond the prepared set; instantaneous count.
    TargetUnplanned => "target_unplanned",
    /// Rebuilding an equal-capacity scratch view; includes scheduling metadata,
    /// without allocating color storage. Counts attempts, including failures.
    TargetReshape => "target_reshape",
    /// Settle widget mutations, layout, transforms and input state.
    UiRewrite => "ui_rewrite",
    /// Measure and arrange dirty widget subtrees, nested in UiRewrite.
    UiLayout => "ui_layout",
    /// Update widget transforms and clipping, nested in UiRewrite.
    UiCompose => "ui_compose",
    /// Record retained widget paint commands.
    UiPaint => "ui_paint",
    /// Capture or reuse candidate input/layout geometry.
    UiGeometry => "ui_geometry",
    /// Resolve image metadata and resource leases in a stage projection.
    UiImageResolve => "ui_image_resolve",
    /// Prepare glyph residency and measured group ink before replay.
    GlyphPrepare => "glyph_prepare",
    /// Look up or construct drawable geometry during replay, including nested draws.
    GeometryPrepare => "geometry_prepare",
    /// Prepare image storage and metadata, including nested pixel transfers.
    ImageAllocate => "image_allocate",
    /// Transfer prepared image pixels into native texture storage.
    ImageTransfer => "image_transfer",
    /// Convert source pixels to the backend's upload encoding.
    ImageConvert => "image_convert",
    /// Select reusable geometry buffers and upload changed vertex/index ranges.
    GeometryUpload => "geometry_upload",
    /// Pack ordered geometry segments into a backend upload layout.
    GeometryPack => "geometry_pack",
    /// Resolve paint programs, uniforms and state, including nested driver calls.
    DrawPrepare => "draw_prepare",
    /// Record invalidated widget paint; excludes traversal of cached widgets.
    UiPaintRecord => "ui_paint_record",
    /// Construct and validate a native shader or render pipeline on a cache miss.
    PipelineCreate => "pipeline_create",
    /// Acquire a native presentation image, including display or driver waits.
    SurfaceAcquire => "surface_acquire",
    /// Check the active desktop audio device and its negotiated format.
    AudioDeviceCheck => "audio_device_check",
    /// Enumerate native audio format capabilities.
    AudioDeviceFormats => "audio_device_formats",
    /// Create, start, suspend or close the native audio stream.
    AudioStreamControl => "audio_stream_control",
    /// Finish recording a native GPU command buffer before queue submission.
    CommandFinish => "command_finish",
    /// Player frame preparation and acceptance, including nested backend work.
    PlayerRender => "player_render",
    /// Publish accepted geometry and presentation completion to the session.
    FrameAccept => "frame_accept",
    /// Native window callbacks and event-loop maintenance outside nested phases.
    HostEvent => "host_event",
    /// Configure native presentation images after geometry or surface changes.
    SurfaceConfigure => "surface_configure",
    /// Scale and transform a font outline before rasterization.
    GlyphOutline => "glyph_outline",
    /// Expand a glyph stroke into a fillable path.
    GlyphStroke => "glyph_stroke",
    /// Allocate and rasterize the glyph's alpha coverage.
    GlyphCoverage => "glyph_coverage",
    /// Desktop preparations that selected RGBA16F with four samples; instantaneous count.
    RenderTier16f4x => "render_tier_16f_4x",
    /// Desktop preparations that selected RGBA16F with one sample; instantaneous count.
    RenderTier16f1x => "render_tier_16f_1x",
    /// Desktop preparations that selected RGBA8 with one sample; instantaneous count.
    RenderTier8u1x => "render_tier_8u_1x",
    /// Create a sampled desktop image's native texture and view.
    ImageTextureCreate => "image_texture_create",
    /// Allocate a sampled desktop image before creating its view.
    ImageNativeAllocate => "image_native_allocate",
    /// Create the shader-visible view of a sampled desktop image.
    ImageNativeView => "image_native_view",
    /// Shape styled text into positioned font runs.
    UiTextShape => "ui_text_shape",
    /// Break shaped text into lines.
    UiTextWrap => "ui_text_wrap",
    /// Index glyph bounds for viewport culling.
    UiTextPaintBounds => "ui_text_paint_bounds",
    /// Advance and route shared asset IO and decode completions.
    AssetPump => "asset_pump",
    /// Attach atlas views and publish their resolved backing pages.
    AssetAtlas => "asset_atlas",
    /// Publish audio decoder completions to pending segments.
    MediaDecoded => "media_decoded",
    /// Advance loaded audio activities and submit ready segments.
    MediaActivities => "media_activities",
    /// Reclaim mixer activity state and deliver terminal events.
    MediaAudioService => "media_audio_service",
    /// Measure alpha support and opacity when registering immutable image pixels.
    ImageCoverage => "image_coverage",
    /// Resolve a key and version to a file or archive entry, including cache misses.
    ReadOpen => "read_open",
    /// Read the requested bytes, including archive decompression when applicable.
    ReadPayload => "read_payload",
    /// Open a loose file on a descriptor cache miss, nested within ReadOpen.
    FileOpen => "file_open",
    /// Query a newly opened loose file's metadata, nested within ReadOpen.
    FileMetadata => "file_metadata",
}

/// Record an instantaneous event in the phase's call count without a duration.
#[inline]
pub fn count(phase: Phase) {
    #[cfg(feature = "profiling")]
    enabled::count(phase);
    #[cfg(not(feature = "profiling"))]
    let _ = phase;
}

/// Record retained-result reuse and the dependency bytes encoded for its key.
#[inline]
pub fn retained_cache(hit: bool, bytes: u64) {
    #[cfg(feature = "profiling")]
    enabled::retained_cache(hit, bytes);
    #[cfg(not(feature = "profiling"))]
    let _ = (hit, bytes);
}

/// Record bytes transferred into a geometry buffer by one upload operation.
/// Builds without profiling erase this counter alongside timing scopes.
#[inline]
pub fn geometry_upload(bytes: u64) {
    #[cfg(feature = "profiling")]
    enabled::geometry_upload(bytes);
    #[cfg(not(feature = "profiling"))]
    let _ = bytes;
}

/// Times one phase. Repeated calls contribute their duration and count.
pub struct Scope {
    #[cfg(feature = "profiling")]
    start: std::time::Instant,
    #[cfg(feature = "profiling")]
    phase: Phase,
    #[cfg(feature = "profiling")]
    thread: Option<thread::Probe>,
    #[cfg(feature = "profiling")]
    wall: Option<wall::Token>,
    #[cfg(feature = "profiling")]
    counted: bool,
    #[cfg(feature = "profiling")]
    owner: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Scope {
    #[inline]
    pub fn new(phase: Phase) -> Self {
        #[cfg(feature = "profiling")]
        let start = std::time::Instant::now();
        #[cfg(not(feature = "profiling"))]
        let _ = phase;
        let scope = Self {
            #[cfg(feature = "profiling")]
            start,
            #[cfg(feature = "profiling")]
            phase,
            #[cfg(feature = "profiling")]
            thread: thread::Probe::begin(phase),
            #[cfg(feature = "profiling")]
            wall: wall::begin(phase, start),
            #[cfg(feature = "profiling")]
            counted: true,
            #[cfg(feature = "profiling")]
            owner: std::marker::PhantomData,
        };
        #[cfg(feature = "profiling")]
        enabled::begin(phase, scope.start);
        scope
    }

    /// Finish this interval, adding a call only when `completed` is true.
    /// Both outcomes retain elapsed duration and close thread/wall accounting;
    /// ordinary scope destruction continues to count one call unconditionally.
    #[inline]
    pub fn finish_counted(mut self, completed: bool) {
        #[cfg(feature = "profiling")]
        {
            self.counted = completed;
        }
        #[cfg(not(feature = "profiling"))]
        let _ = (&mut self, completed);
    }
}
#[cfg(not(feature = "profiling"))]
impl Drop for Scope {
    #[inline]
    fn drop(&mut self) {}
}
#[cfg(feature = "profiling")]
mod enabled {
    use super::*;
    use std::sync::{
        OnceLock,
        atomic::{AtomicU32, AtomicU64, Ordering},
    };
    static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
    static MICROS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
    static CALLS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
    static LAST_MICROS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
    static LAST_CALLS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
    static ACTIVE: [AtomicU32; NAMES.len()] = [const { AtomicU32::new(0) }; NAMES.len()];
    static SINCE: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
    static GEOMETRY_CALLS: AtomicU64 = AtomicU64::new(0);
    static GEOMETRY_BYTES: AtomicU64 = AtomicU64::new(0);
    static RETAINED_HITS: AtomicU64 = AtomicU64::new(0);
    static RETAINED_MISSES: AtomicU64 = AtomicU64::new(0);
    static RETAINED_KEY_BYTES: AtomicU64 = AtomicU64::new(0);

    pub(super) fn count(phase: Phase) {
        CALLS[phase as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn retained_cache(hit: bool, bytes: u64) {
        if hit {
            RETAINED_HITS.fetch_add(1, Ordering::Relaxed);
        } else {
            RETAINED_MISSES.fetch_add(1, Ordering::Relaxed);
        }
        RETAINED_KEY_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
    /// Cumulative retained-result lookups. Key bytes contain paint dependencies.
    #[derive(Clone, Copy, Default)]
    pub struct RetainedCache {
        pub hits: u64,
        pub misses: u64,
        pub key_bytes: u64,
    }
    impl RetainedCache {
        /// Counters are independent observations of completed lookup work.
        pub fn snapshot() -> Self {
            Self {
                hits: RETAINED_HITS.load(Ordering::Relaxed),
                misses: RETAINED_MISSES.load(Ordering::Relaxed),
                key_bytes: RETAINED_KEY_BYTES.load(Ordering::Relaxed),
            }
        }
        /// Lookup work between two observations.
        pub fn delta(self, previous: Self) -> Self {
            Self {
                hits: self.hits.wrapping_sub(previous.hits),
                misses: self.misses.wrapping_sub(previous.misses),
                key_bytes: self.key_bytes.wrapping_sub(previous.key_bytes),
            }
        }
    }

    pub(super) fn geometry_upload(bytes: u64) {
        if bytes != 0 {
            GEOMETRY_BYTES.fetch_add(bytes, Ordering::Relaxed);
            GEOMETRY_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Cumulative geometry transfers. Cache hits contribute zero bytes.
    #[derive(Clone, Copy, Default)]
    pub struct GeometryUploads {
        pub calls: u64,
        pub bytes: u64,
    }
    impl GeometryUploads {
        /// Counters are sampled independently; concurrent transfers may cross a row.
        pub fn snapshot() -> Self {
            Self {
                calls: GEOMETRY_CALLS.load(Ordering::Relaxed),
                bytes: GEOMETRY_BYTES.load(Ordering::Relaxed),
            }
        }

        /// Transfers recorded between two observations.
        pub fn delta(self, previous: Self) -> Self {
            Self {
                calls: self.calls.wrapping_sub(previous.calls),
                bytes: self.bytes.wrapping_sub(previous.bytes),
            }
        }
    }

    fn tracked(phase: Phase) -> bool {
        !matches!(
            phase,
            Phase::Draw
                | Phase::TargetCreate
                | Phase::TargetReserve
                | Phase::TargetTexture
                | Phase::TargetFramebuffer
                | Phase::BufferCreate
                | Phase::GlyphRaster
                | Phase::VmSlice
                | Phase::HostCall
        )
    }
    pub(super) fn begin(phase: Phase, start: std::time::Instant) {
        if tracked(phase) && ACTIVE[phase as usize].fetch_add(1, Ordering::Relaxed) == 0 {
            let epoch = EPOCH.get_or_init(|| start);
            SINCE[phase as usize].store(
                start.saturating_duration_since(*epoch).as_micros() as u64,
                Ordering::Relaxed,
            );
        }
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            if let Some(probe) = self.thread.take() {
                probe.finish();
            }
            let index = self.phase as usize;
            let end = std::time::Instant::now();
            wall::end(self.wall.take(), end);
            MICROS[index].fetch_add(
                end.duration_since(self.start).as_micros() as u64,
                Ordering::Relaxed,
            );
            if self.counted {
                CALLS[index].fetch_add(1, Ordering::Relaxed);
            }
            if tracked(self.phase) {
                ACTIVE[index].fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
    /// Completed phase work since the previous sample, across all threads.
    #[derive(Clone, Copy)]
    pub struct Sample {
        pub micros: [u64; NAMES.len()],
        pub calls: [u64; NAMES.len()],
    }
    impl Default for Sample {
        fn default() -> Self {
            Self {
                micros: [0; NAMES.len()],
                calls: [0; NAMES.len()],
            }
        }
    }
    impl Sample {
        /// The main-thread collector is the sole consumer. Durations and counts
        /// are sampled independently; concurrent completions may cross a row.
        pub fn take() -> Self {
            let current = Self::snapshot();
            Self {
                micros: std::array::from_fn(|i| {
                    current.micros[i]
                        .wrapping_sub(LAST_MICROS[i].swap(current.micros[i], Ordering::Relaxed))
                }),
                calls: std::array::from_fn(|i| {
                    current.calls[i]
                        .wrapping_sub(LAST_CALLS[i].swap(current.calls[i], Ordering::Relaxed))
                }),
            }
        }
        /// Independent readers observe cumulative counters without consuming them.
        pub fn snapshot() -> Self {
            Self {
                micros: std::array::from_fn(|i| MICROS[i].load(Ordering::Relaxed)),
                calls: std::array::from_fn(|i| CALLS[i].load(Ordering::Relaxed)),
            }
        }
        pub fn delta(self, previous: Self) -> Self {
            Self {
                micros: std::array::from_fn(|i| self.micros[i].wrapping_sub(previous.micros[i])),
                calls: std::array::from_fn(|i| self.calls[i].wrapping_sub(previous.calls[i])),
            }
        }
        /// Elapsed microseconds in phases that have entered and not returned yet.
        pub fn active() -> [Option<u64>; NAMES.len()] {
            let now = EPOCH
                .get_or_init(std::time::Instant::now)
                .elapsed()
                .as_micros() as u64;
            std::array::from_fn(|i| {
                (ACTIVE[i].load(Ordering::Relaxed) != 0)
                    .then(|| now.saturating_sub(SINCE[i].load(Ordering::Relaxed)))
            })
        }
    }
}
#[cfg(feature = "profiling")]
pub use enabled::{GeometryUploads, RetainedCache, Sample};

#[cfg(feature = "profiling")]
mod wall;
#[cfg(feature = "profiling")]
pub use wall::{WallRecording, WallSnapshot};

#[cfg(feature = "profiling")]
mod thread;
#[cfg(feature = "profiling")]
pub use thread::{
    METRICS as THREAD_METRICS, PHASES as THREAD_PHASES, ThreadCost, ThreadSnapshot, ThreadTime,
    install_thread_clock,
};

#[cfg(all(test, feature = "profiling"))]
mod counted_scope_tests {
    use super::*;

    #[test]
    fn failed_interval_keeps_time_without_counting_success_or_leaking_wall_scope() {
        let _recording = WallRecording::start().unwrap();
        let before_wall = WallSnapshot::capture().unwrap();
        let before = Sample::snapshot();
        let phase = Phase::TargetReshape;
        let failed = Scope::new(phase);
        std::thread::sleep(std::time::Duration::from_millis(1));
        failed.finish_counted(false);
        Scope::new(phase).finish_counted(true);
        drop(Scope::new(phase));
        let delta = Sample::snapshot().delta(before);
        assert_eq!(delta.calls[phase as usize], 2);
        assert!(delta.micros[phase as usize] > 0);
        assert!(Sample::active()[phase as usize].is_none());
        let wall = WallSnapshot::capture()
            .unwrap()
            .checked_delta(&before_wall)
            .unwrap();
        assert_eq!(wall.calls[phase as usize], 3);
        assert_eq!(wall.errors, 0);
        assert_eq!(
            wall.wall_ns,
            wall.nanos.iter().sum::<u64>() + wall.unscoped_ns + wall.lost_ns
        );
    }
}
