//! Optional runtime profiling for host and device measurements.
//! Nested phases overlap. Worker durations are attributed when work completes;
//! their totals can exceed the main thread's elapsed time. Normal builds erase
//! scopes and counters; profiling builds use fixed atomic arrays without IO.

/// Stages recorded by the shared runtime and device backend.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Phase {
    Input,
    Tick,
    /// Session scheduling, interpreter slices and native host work.
    Session,
    UiSync,
    Record,
    Compile,
    ImagePlan,
    Upload,
    Replay,
    GlyphRaster,
    Draw,
    Present,
    Read,
    Decode,
    TargetCreate,
    BufferCreate,
    Poll,
    Load,
    Mount,
    Sampler,
    Composite,
    Screen,
    Clear,
    SourceComposite,
    CoverageComposite,
    EffectComposite,
    AudioMix,
    AudioDecode,
    VideoOpen,
    VideoDecode,
    OffscreenPlan,
    CompositeBatch,
    RetainedPlan,
    /// Interpreter slices, including VM builtins; excludes external host dispatch.
    VmSlice,
    /// External native dispatch, including continuation resumes.
    HostCall,
    /// Ordered copies between GPU-resident textures; includes submission CPU time.
    TextureCopy,
    /// Sampling native controller and touch state, before input dispatch.
    ControlPoll,
    /// Host event wait, including timeout and scheduler wake latency.
    HostWait,
    /// Offscreen payload admission, nested within TargetCreate.
    TargetReserve,
    /// Native offscreen pixel allocation, nested within TargetCreate.
    TargetTexture,
    /// Native surface and scheduling owner creation, nested within TargetCreate.
    TargetFramebuffer,
    /// Successfully constructed targets; instantaneous count, zero duration.
    TargetCreated,
    /// Cross-frame scratch cache hits; instantaneous count, zero duration.
    TargetReused,
    /// Native decoded pictures returned before timestamp lookahead.
    VideoDecoded,
    /// Pictures published into the playback queue.
    VideoQueued,
    /// Pictures selected for the current playback position.
    VideoSelected,
    /// Queued pictures superseded by another already-due picture.
    VideoDropped,
    /// Video pictures installed into the stage model.
    VideoStage,
    /// New stage revisions captured by the player UI.
    ProjectionChanged,
    /// UI revisions published after resource preparation.
    ProjectionPublished,
    /// Model invalidations delivered to presentation scheduling.
    PresentationInvalidated,
    /// Project committed VM properties into the shared stage.
    UiStageSync,
    /// Build the player's immutable stage view and resource demand identity.
    UiStageView,
    /// Resolve visible image requests, including candidate layout when needed.
    UiImageRequests,
    /// Plan visible tiles and prepare their resource leases.
    UiImageTiles,
    /// Apply a stage view to the retained widget tree.
    UiWidgetSync,
    /// Incremental VM collection driven by the session scheduler.
    SessionGc,
    /// Session activity/time updates, including bounded animation graph work.
    SessionActivity,
    /// Task selection/maintenance and VM slices; excludes later host dispatch.
    SessionSchedule,
    /// Project VM node properties into the authored stage; nested in its caller.
    StageProperties,
    /// Synchronous session diagnostic formatting and stderr output, nested in Session.
    SessionDiagnostics,
    /// Complete backend render/present call, including its nested preparation and submission.
    RenderSubmission,
    /// Session host maintenance before clock and interpreter work; may contain nested services.
    SessionMaintenance,
    /// Begin a VM collection; incremental collection slices use SessionGc.
    SessionGcBegin,
    /// Poll media and deliver its session completions, nested in SessionMaintenance.
    SessionMedia,
    /// Advance save requests and storage completions, nested in SessionMaintenance.
    SessionSaves,
    /// Poll localization and script asset reads, nested in SessionMaintenance.
    SessionAssets,
    /// Finish native GPU encoding and submit work; may wait for driver capacity.
    RenderFence,
    /// Native draw call wall time, including driver backpressure.
    DriverDraw,
    /// Native clear call wall time, including driver backpressure.
    DriverClear,
    /// Main-thread diagnostic collection and recording.
    Observer,
    /// Successfully admitted ordered effect chains; instantaneous count.
    MacrotileChain,
    /// Effect outputs selected from ordered macrotile cells; instantaneous count.
    MacrotilePass,
    /// Error recovery waiting for all native graphics, transfer and display work.
    RenderDrain,
    /// Physical pressure before drawing begins; instantaneous count.
    RenderPreparationPressure,
    /// Physical pressure after drawing begins; instantaneous count.
    RenderStreamPressure,
    /// Scratch acquisitions during drawing beyond the prepared set; instantaneous count.
    TargetUnplanned,
    /// Rebuilding an equal-capacity scratch view; includes scheduling metadata,
    /// without allocating color storage. Counts attempts, including failures.
    TargetReshape,
    /// Settle widget mutations, layout, transforms and input state.
    UiRewrite,
    /// Measure and arrange dirty widget subtrees, nested in UiRewrite.
    UiLayout,
    /// Update widget transforms and clipping, nested in UiRewrite.
    UiCompose,
    /// Record retained widget paint commands.
    UiPaint,
    /// Capture or reuse candidate input/layout geometry.
    UiGeometry,
    /// Resolve image metadata and resource leases in a stage projection.
    UiImageResolve,
    /// Prepare glyph residency and measured group ink before replay.
    GlyphPrepare,
    /// Look up or construct drawable geometry during replay, including nested draws.
    GeometryPrepare,
    /// Prepare image storage and metadata, including nested pixel transfers.
    ImageAllocate,
    /// Transfer prepared image pixels into native texture storage.
    ImageTransfer,
    /// Convert source pixels to the backend's upload encoding.
    ImageConvert,
    /// Select reusable geometry buffers and upload changed vertex/index ranges.
    GeometryUpload,
    /// Pack ordered geometry segments into a backend upload layout.
    GeometryPack,
    /// Resolve paint programs, uniforms and state, including nested driver calls.
    DrawPrepare,
    /// Record invalidated widget paint; excludes traversal of cached widgets.
    UiPaintRecord,
    /// Construct and validate a native shader or render pipeline on a cache miss.
    PipelineCreate,
    /// Acquire a native presentation image, including display or driver waits.
    SurfaceAcquire,
    /// Check the active desktop audio device and its negotiated format.
    AudioDeviceCheck,
    /// Enumerate native audio format capabilities.
    AudioDeviceFormats,
    /// Create, start, suspend or close the native audio stream.
    AudioStreamControl,
    /// Finish recording a native GPU command buffer before queue submission.
    CommandFinish,
    /// Player frame preparation and acceptance, including nested backend work.
    PlayerRender,
    /// Publish accepted geometry and presentation completion to the session.
    FrameAccept,
    /// Native window callbacks and event-loop maintenance outside nested phases.
    HostEvent,
    /// Configure native presentation images after geometry or surface changes.
    SurfaceConfigure,
    /// Scale and transform a font outline before rasterization.
    GlyphOutline,
    /// Expand a glyph stroke into a fillable path.
    GlyphStroke,
    /// Allocate and rasterize the glyph's alpha coverage.
    GlyphCoverage,
}
pub const NAMES: [&str; 106] = [
    "input",
    "tick",
    "session",
    "ui_sync",
    "record",
    "compile",
    "image_plan",
    "upload",
    "replay",
    "glyph_raster",
    "draw",
    "present",
    "read",
    "decode",
    "target_create",
    "buffer_create",
    "poll",
    "load",
    "mount",
    "sampler",
    "composite",
    "screen",
    "clear",
    "source_composite",
    "coverage_composite",
    "effect_composite",
    "audio_mix",
    "audio_decode",
    "video_open",
    "video_decode",
    "offscreen_plan",
    "composite_batch",
    "retained_plan",
    "vm_slice",
    "host_call",
    "texture_copy",
    "control_poll",
    "host_wait",
    "target_reserve",
    "target_texture",
    "target_framebuffer",
    "target_created",
    "target_reused",
    "video_decoded",
    "video_queued",
    "video_selected",
    "video_dropped",
    "video_stage",
    "projection_changed",
    "projection_published",
    "presentation_invalidated",
    "ui_stage_sync",
    "ui_stage_view",
    "ui_image_requests",
    "ui_image_tiles",
    "ui_widget_sync",
    "session_gc",
    "session_activity",
    "session_schedule",
    "stage_properties",
    "session_diagnostics",
    "render_submission",
    "session_maintenance",
    "session_gc_begin",
    "session_media",
    "session_saves",
    "session_assets",
    "render_fence",
    "driver_draw",
    "driver_clear",
    "observer",
    "macrotile_chain",
    "macrotile_pass",
    "render_drain",
    "render_preparation_pressure",
    "render_stream_pressure",
    "target_unplanned",
    "target_reshape",
    "ui_rewrite",
    "ui_layout",
    "ui_compose",
    "ui_paint",
    "ui_geometry",
    "ui_image_resolve",
    "glyph_prepare",
    "geometry_prepare",
    "image_allocate",
    "image_transfer",
    "image_convert",
    "geometry_upload",
    "geometry_pack",
    "draw_prepare",
    "ui_paint_record",
    "pipeline_create",
    "surface_acquire",
    "audio_device_check",
    "audio_device_formats",
    "audio_stream_control",
    "command_finish",
    "player_render",
    "frame_accept",
    "host_event",
    "surface_configure",
    "glyph_outline",
    "glyph_stroke",
    "glyph_coverage",
];

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
