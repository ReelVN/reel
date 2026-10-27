//! GLES rendering of the common immutable frame and resource contracts.
mod draw;
mod effect;
mod geometry;
mod mask;
mod ownership;
mod programs;
mod replay;
mod resources;
mod target_cache;
mod targets;
pub use crate::common::GpuError as GlesError;
use crate::common::renderer::Budgets;
use crate::*;
use pvr_psp2_sys::graphics as native;
use reel_core::budget::BudgetGroup;
use resources::{Image, Images};
use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
};

struct CacheRetry {
    cause: GlesError,
    released: bool,
}

/// Device references and their shared resource admission limits.
#[derive(Clone, Copy)]
struct DeviceResources<'a> {
    gpu: &'a ownership::Device,
    budgets: &'a Budgets,
    limits: crate::common::GpuLimits,
}

/// Device resource ceilings, all sharing one simultaneous-memory limit.
#[derive(Clone, Copy, Debug)]
pub struct GlesLimits {
    /// Cached image, glyph atlas and persistent draw resources, in bytes.
    pub resource_bytes: u64,
    /// Simultaneous physical RGBA8 render targets, in bytes. Defaults to 48 MiB;
    /// storage is charged on demand within the shared total ceiling.
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
impl Default for GlesLimits {
    fn default() -> Self {
        Self {
            resource_bytes: 64 << 20,
            target_bytes: 48 << 20,
            transient_bytes: 8 << 20,
            total_bytes: 96 << 20,
            max_inflight: 2,
            image_upload_bytes_per_prepare: 2 << 20,
        }
    }
}
impl GlesLimits {
    fn common(self) -> crate::common::GpuLimits {
        crate::common::GpuLimits {
            resource_bytes: self.resource_bytes,
            target_bytes: self.target_bytes,
            transient_bytes: self.transient_bytes,
            max_inflight: self.max_inflight,
            image_upload_bytes_per_prepare: self.image_upload_bytes_per_prepare,
        }
    }
}
/// Native assembly options. The module directory contains one matching driver build.
#[derive(Clone, Debug)]
pub struct GlesOptions {
    pub module_directory: String,
    pub vsync: bool,
    pub limits: GlesLimits,
}
impl Default for GlesOptions {
    fn default() -> Self {
        Self {
            module_directory: "app0:module".into(),
            vsync: true,
            limits: Default::default(),
        }
    }
}
struct Flight {
    fence: Rc<native::Fence>,
    submission: Option<(SubmissionId, FrameId)>,
    _frame: RenderFrame,
    _prepared: Option<Rc<replay::Prepared>>,
    _images: Vec<Rc<Image>>,
    upload: bool,
    retained: Option<usize>,
}
/// The renderer owns its native context; the host supplies only portable lifecycle
/// and presentation callbacks. Submitted resources survive until their GPU fence.
pub struct GlesBackend {
    context: ownership::Device,
    programs: Rc<programs::Programs>,
    options: GlesOptions,
    budgets: Budgets,
    _group: BudgetGroup,
    images: Images,
    glyphs: draw::GlyphCache,
    effects: effect::Cache,
    masks: crate::common::mask::split::Cache,
    targets: target_cache::Cache,
    retained: crate::common::retained::Cache<resources::Target, u8>,
    geometry: geometry::Cache,
    info: Option<SurfaceInfo>,
    suspended: bool,
    next_submission: u64,
    flights: VecDeque<Flight>,
    events: VecDeque<RenderEvent<GlesError>>,
    failed_work: Vec<replay::Work>,
    failed_prepared: Vec<Rc<replay::Prepared>>,
    failed_images: Vec<Rc<Image>>,
    drain_failed: bool,
}
impl GlesBackend {
    /// Create the thread-bound native context and load offline SGX programs.
    #[cfg(target_os = "vita")]
    pub fn new(options: GlesOptions) -> Result<Self, GlesError> {
        let context =
            native::Context::new(&options.module_directory, options.vsync).map_err(native_error)?;
        Self::with_context(context, options)
    }

    /// Create an offscreen desktop EGL context with the same rendering and budget
    /// paths as the device backend. Shaders use their shared GLSL sources.
    #[cfg(not(target_os = "vita"))]
    pub fn headless(size: [u32; 2], limits: GlesLimits) -> Result<Self, GlesError> {
        let context = native::Context::headless(size, false).map_err(native_error)?;
        Self::with_context(
            context,
            GlesOptions {
                limits,
                ..Default::default()
            },
        )
    }

    fn with_context(context: native::Context, options: GlesOptions) -> Result<Self, GlesError> {
        if options.limits.max_inflight == 0 || options.limits.image_upload_bytes_per_prepare == 0 {
            return Err(GlesError::InvalidSize);
        }
        let context = ownership::Device::new(context);
        let group = BudgetGroup::new(options.limits.total_bytes);
        let pool = |category, bytes| {
            group.pool(
                category,
                QueueLimits {
                    max_requests: u32::MAX,
                    max_bytes: bytes,
                },
            )
        };
        let budgets = Budgets {
            resources: pool(BudgetCategory::GpuResource, options.limits.resource_bytes),
            targets: pool(BudgetCategory::Offscreen, options.limits.target_bytes),
            transient: pool(BudgetCategory::Temporary, options.limits.transient_bytes),
        };
        let programs = Rc::new(programs::Programs::new(&context, &budgets.resources)?);
        Ok(Self {
            context,
            programs,
            options,
            budgets,
            _group: group,
            images: Default::default(),
            glyphs: Default::default(),
            effects: Default::default(),
            masks: Default::default(),
            targets: Default::default(),
            retained: Default::default(),
            geometry: Default::default(),
            info: None,
            suspended: false,
            next_submission: 1,
            flights: VecDeque::new(),
            events: VecDeque::new(),
            failed_work: Vec::new(),
            failed_prepared: Vec::new(),
            failed_images: Vec::new(),
            drain_failed: false,
        })
    }
    fn check_native(&self) -> Result<(), GlesError> {
        self.context.check()
    }
    /// Physical output dimensions used when configuring the common surface.
    pub fn output_size(&self) -> [u32; 2] {
        self.context.output_size()
    }
    /// Renderer-owned domains and their shared simultaneous budget peak.
    /// Native driver allocations have separate ownership from these budgets.
    pub fn usage(&self) -> crate::Usage {
        crate::Usage {
            targets: self.budgets.targets.usage().bytes,
            resources: self.budgets.resources.usage().bytes,
            transient: self.budgets.transient.usage().bytes,
            cached_geometry: self.geometry.bytes(),
            shared: Some(self._group.usage()),
        }
    }
    /// Explicitly sample native allocator domains, separately from renderer budgets.
    /// Heap snapshots are adjacent observations; registered storage, its device
    /// suballocations and independent historical peaks must not be added together.
    /// Host EGL has no matching native allocator query and returns `None`.
    pub fn native_memory_usage(&self) -> Result<Option<native::DriverMemoryUsage>, GlesError> {
        self.context.memory_usage().map_err(native_error)
    }
    /// Completed geometry capacity, including allocation metadata, retained as
    /// an evictable subset of `usage().transient` in the transient budget.
    pub fn cached_geometry_bytes(&self) -> u64 {
        self.geometry.bytes()
    }
    /// Drain all submitted work before releasing a departed player's resources.
    /// Call only after that player and its captures have been dropped. The native
    /// device and offline programs remain owned by this backend for the next player.
    /// A failed drain retains all owners and prevents mounting another session.
    pub fn reset_session(&mut self) -> Result<(), GlesError> {
        self.context.refresh();
        self.check_native()?;
        self.suspended = true;
        self.info = None;
        self.context.finish().map_err(native_error)?;
        self.flights.clear();
        self.check_native()?;
        self.failed_work.clear();
        self.check_native()?;
        self.failed_prepared.clear();
        self.check_native()?;
        self.failed_images.clear();
        self.check_native()?;
        self.events.clear();
        self.images = Images::default();
        self.check_native()?;
        self.glyphs = draw::GlyphCache::default();
        self.check_native()?;
        self.masks = Default::default();
        self.targets.clear();
        self.check_native()?;
        self.retained.clear();
        self.check_native()?;
        self.geometry.clear();
        self.check_native()?;
        self.drain_failed = false;
        Ok(())
    }
    /// Release device resources, then explicitly close EGL and its native modules.
    /// The underlying close rejects any outstanding resource or capture owner and
    /// reports the actual EGL/module failure instead of creating another context.
    pub fn close(mut self) -> Result<(), GlesError> {
        self.reset_session()?;
        let context = self.context.clone();
        drop(self);
        context.close().map_err(|(_, error)| native_error(error))
    }
    // A failed command can already have reached the device. Retire its owners
    // only after a successful drain; a failed drain makes this backend terminal.
    fn drain_error(&mut self) -> bool {
        if self.context.refresh() || self.context.finish().is_err() {
            self.drain_failed = true;
            return false;
        }
        self.images.after_drain();
        self.glyphs.after_drain();
        self.check_native().is_ok()
    }
    fn prepare(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        capture: bool,
    ) -> Result<replay::Prepared, GlesError> {
        if capture {
            self.images.release_idle();
            self.check_native()?;
        }
        self.geometry.cpu_disabled = false;
        self.geometry.optional_used = false;
        let result = self.prepare_cached(frame, size, capture, None);
        let fallback = self.geometry.optional_used
            && matches!(
                result,
                Err(GlesError::Budget {
                    domain: "transient" | "mesh scratch" | "geometry" | "index packing",
                    ..
                })
            );
        if (fallback || matches!(result, Err(GlesError::Pressure { .. }))) && !self.drain_failed {
            let cause = result.err().unwrap();
            let resident = self._group.usage().bytes;
            self.targets.release_idle();
            self.check_native()?;
            self.geometry.clear();
            self.check_native()?;
            self.images.release_idle();
            self.check_native()?;
            // Rebuild image demand once before replay. A shared-pressure retry
            // must release real capacity; flights keep all active owners alive.
            self.geometry.cpu_disabled = true;
            let result = self.prepare_cached(
                frame,
                size,
                capture,
                Some(CacheRetry {
                    cause,
                    released: fallback || self._group.usage().bytes < resident,
                }),
            );
            self.geometry.cpu_disabled = false;
            return result;
        }
        result
    }
    fn prepare_cached(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        capture: bool,
        retry: Option<CacheRetry>,
    ) -> Result<replay::Prepared, GlesError> {
        self.check_native()?;
        self.masks.prune();
        if self.drain_failed {
            return Err(GlesError::NativeDeviceLost(
                "GPU failure could not be drained".into(),
            ));
        }
        let limits = self.options.limits.common();
        if !capture && self.retained.pending(&frame.key()) {
            return Err(GlesError::Preparing);
        }
        if self.flights.len() >= limits.max_inflight {
            return Err(GlesError::Busy);
        }
        self.images.prune(&[]);
        self.glyphs.prune(&[]);
        self.effects.prune();
        self.check_native()?;
        let local = Budgets::new(limits);
        let mut plan_tickets = Vec::new();
        let selection = std::sync::Arc::new(crate::common::retained::Plan::collect(
            std::iter::once(frame),
            size,
            capture,
            &self.budgets,
            &local,
            limits.transient_bytes,
            |key| self.retained.lookup(key, 0, &frame.key()),
            |bytes| {
                plan_tickets.push(crate::common::renderer::reserve(
                    &self.budgets.transient,
                    &local.transient,
                    bytes,
                    limits.transient_bytes,
                    "retained plan",
                )?);
                Ok(())
            },
        )?);
        self.retained.retain(&selection);
        let warm = selection
            .warm
            .map(|index| {
                self.retained.start(
                    selection.entries[index].key.clone(),
                    0,
                    frame.key(),
                    &self.budgets.transient,
                    limits.transient_bytes,
                )
            })
            .transpose()?;
        let plan = {
            let _planning = reel_core::profile::Scope::new(reel_core::profile::Phase::ImagePlan);
            crate::common::image_plan::collect(
                std::iter::once(frame),
                self.context.capabilities().max_texture_size,
                capture,
                &selection,
                |bytes| {
                    plan_tickets.push(crate::common::renderer::reserve(
                        &self.budgets.transient,
                        &local.transient,
                        bytes,
                        limits.transient_bytes,
                        "image plan",
                    )?);
                    Ok(())
                },
            )?
        };
        if let Some(retry) = retry
            && matches!(retry.cause, GlesError::Pressure { .. })
        {
            let resident = self._group.usage().bytes;
            self.images.retain_demand(&plan.images);
            self.check_native()?;
            self.glyphs.prune_idle();
            self.check_native()?;
            if !retry.released && self._group.usage().bytes >= resident {
                return Err(retry.cause);
            }
        }
        let image_growth = self.images.validate(
            &plan.images,
            self.context.capabilities(),
            limits,
            self.options.limits.total_bytes,
        )?;
        // Captures need independent outputs; release the ordinary scratch cache
        // before reserving them. New images also take priority under pressure.
        // Flights keep any targets that the device still owns alive.
        if capture
            || self
                .targets
                .output_size()
                .is_some_and(|cached| cached != size)
            || image_growth
                > self.options.limits.total_bytes.saturating_sub(
                    self.budgets
                        .resources
                        .usage()
                        .bytes
                        .saturating_add(self.budgets.targets.usage().bytes)
                        .saturating_add(self.budgets.transient.usage().bytes),
                )
        {
            self.targets.clear();
            self.check_native()?;
        }
        for mask in &plan.masks {
            let caps = self.context.capabilities();
            if mask.definition.program.operations().len() > mask::OPERATION_CAPACITY {
                return Err(GlesError::Unsupported {
                    at: mask.at,
                    record: "mask graph exceeds the device interpreter capacity",
                });
            }
            if mask.definition.images.len() > 4
                || caps.max_texture_units < 4
                || caps.max_fragment_uniform_vectors < mask::UNIFORM_VECTORS
            {
                return Err(GlesError::Unsupported {
                    at: mask.at,
                    record: "mask exceeds device uniform or texture-unit capacity",
                });
            }
            for handle in &mask.definition.images {
                if plan.images.iter().any(|image| {
                    image.source.handle() == *handle
                        && matches!(image.source.data(), ResourceData::Yuv420(_))
                }) {
                    return Err(GlesError::Unsupported {
                        at: mask.at,
                        record: "mask requires an image source",
                    });
                }
            }
        }
        let mut images = HashMap::new();
        let mut touched = Vec::new();
        let mut quota = limits.image_upload_bytes_per_prepare;
        let upload = {
            let _uploading = reel_core::profile::Scope::new(reel_core::profile::Phase::Upload);
            self.images.prepare(
                DeviceResources {
                    gpu: &self.context,
                    budgets: &self.budgets,
                    limits,
                },
                &plan.images,
                &mut quota,
                &mut images,
                &mut touched,
            )
        };
        drop(plan);
        drop(plan_tickets);
        let ready = match upload {
            Ok(ready) => ready,
            Err(error) => {
                self.images.release_idle();
                if !self.drain_error() {
                    self.failed_images.extend(touched);
                }
                return Err(error);
            }
        };
        if !ready {
            let fence = match self.context.fence() {
                Ok(fence) => Rc::new(fence),
                Err(error) => {
                    if !self.drain_error() {
                        self.failed_images.extend(touched);
                    }
                    return Err(error.into());
                }
            };
            if let Err(error) = self.context.flush() {
                if !self.drain_error() {
                    self.failed_images.extend(touched);
                }
                return Err(error.into());
            }
            self.flights.push_back(Flight {
                fence,
                submission: None,
                _frame: frame.clone(),
                _prepared: None,
                _images: touched,
                upload: true,
                retained: None,
            });
            return Err(GlesError::Busy);
        }
        // Captures retain their output until readback, independently of later frames.
        let mut capture_targets = target_cache::Cache::default();
        if capture {
            capture_targets.configure(size);
        } else {
            self.targets.configure(size);
        }
        self.check_native()?;
        let targets = if capture {
            &mut capture_targets
        } else {
            &mut self.targets
        };
        match replay::prepare(
            DeviceResources {
                gpu: &self.context,
                budgets: &self.budgets,
                limits: self.options.limits.common(),
            },
            &self.programs,
            replay::Caches {
                images: &mut self.images,
                glyphs: &mut self.glyphs,
                effects: &mut self.effects,
                masks: &mut self.masks,
                targets,
                geometry: &mut self.geometry,
                retained: &self.retained,
            },
            images,
            frame,
            size,
            capture,
            selection,
            warm,
        ) {
            Ok(prepared) => {
                if let Some(token) = prepared.retained {
                    let storage = prepared.output.view.size();
                    self.retained.stage(
                        token,
                        prepared.output.clone(),
                        self.context.target_layout(storage)?.texture_bytes,
                    );
                    self.targets.clear();
                    self.check_native()?;
                    if let Err(error) = self.submit(frame, prepared, None) {
                        self.retained.cancel(&frame.key());
                        return Err(error);
                    }
                    Err(GlesError::Preparing)
                } else {
                    Ok(prepared)
                }
            }
            Err((error, work)) => {
                self.images.release_idle();
                if !self.drain_error() {
                    self.failed_work.push(work);
                }
                Err(error)
            }
        }
    }
    fn submit(
        &mut self,
        frame: &RenderFrame,
        prepared: replay::Prepared,
        submission: Option<(SubmissionId, FrameId)>,
    ) -> Result<(Rc<replay::Prepared>, Rc<native::Fence>), GlesError> {
        let prepared = Rc::new(prepared);
        let fence = match self.context.fence() {
            Ok(fence) => Rc::new(fence),
            Err(error) => {
                if !self.drain_error() {
                    self.failed_prepared.push(prepared);
                }
                return Err(error.into());
            }
        };
        if let Err(error) = self.context.flush() {
            if !self.drain_error() {
                self.failed_prepared.push(prepared);
            }
            return Err(error.into());
        }
        self.flights.push_back(Flight {
            fence: fence.clone(),
            submission,
            _frame: frame.clone(),
            _prepared: Some(prepared.clone()),
            _images: Vec::new(),
            upload: false,
            retained: prepared.retained,
        });
        self.check_native()?;
        self.targets.submitted(prepared.targets());
        Ok((prepared, fence))
    }
}
impl RenderBackend for GlesBackend {
    type Error = GlesError;
    fn vsync(&self) -> Option<bool> {
        Some(self.context.vsync())
    }
    fn set_vsync(&mut self, enabled: bool) -> Result<Option<bool>, GlesError> {
        self.context.set_vsync(enabled).map_err(native_error)?;
        Ok(Some(enabled))
    }
    fn capabilities(&self) -> RenderCaps {
        RenderCaps {
            consumer: ConsumerKind::Device,
            rasterizes: true,
            formats: match (
                self.context.capabilities().bc,
                self.context.capabilities().bgra,
                self.context.capabilities().half_float,
            ) {
                (true, true, true) => &[
                    PixelFormat::Rgba8,
                    PixelFormat::Bgra8,
                    PixelFormat::LinearRgba16Float,
                    PixelFormat::Bc1Rgb,
                    PixelFormat::Bc3Rgba,
                ],
                (true, true, false) => &[
                    PixelFormat::Rgba8,
                    PixelFormat::Bgra8,
                    PixelFormat::Bc1Rgb,
                    PixelFormat::Bc3Rgba,
                ],
                (true, false, true) => &[
                    PixelFormat::Rgba8,
                    PixelFormat::LinearRgba16Float,
                    PixelFormat::Bc1Rgb,
                    PixelFormat::Bc3Rgba,
                ],
                (true, false, false) => &[
                    PixelFormat::Rgba8,
                    PixelFormat::Bc1Rgb,
                    PixelFormat::Bc3Rgba,
                ],
                (false, true, true) => &[
                    PixelFormat::Rgba8,
                    PixelFormat::Bgra8,
                    PixelFormat::LinearRgba16Float,
                ],
                (false, true, false) => &[PixelFormat::Rgba8, PixelFormat::Bgra8],
                (false, false, true) => &[PixelFormat::Rgba8, PixelFormat::LinearRgba16Float],
                (false, false, false) => &[PixelFormat::Rgba8],
            },
            effects: &[
                RenderEffect::Groups,
                RenderEffect::Clips,
                RenderEffect::AlphaMask,
                RenderEffect::LuminanceMask,
                RenderEffect::ImageMask,
                RenderEffect::ShaderEffect,
            ],
            max_texture_size: self.context.capabilities().max_texture_size,
            max_offscreen_size: self.context.capabilities().max_texture_size,
            upload_limits: QueueLimits {
                max_requests: self.options.limits.max_inflight as u32,
                max_bytes: self.options.limits.transient_bytes,
            },
        }
    }
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), GlesError> {
        self.check_native()?;
        self.retained.clear();
        self.check_native()?;
        if info.physical_size != self.output_size()
            || info.surface_epoch.0 == 0
            || !info.scale_factor.is_finite()
            || info.scale_factor <= 0.
            || info.color_encoding != ColorEncoding::Srgb
            || info.alpha_mode != AlphaMode::Opaque
        {
            return Err(GlesError::InvalidSurface);
        }
        self.info = Some(info);
        self.suspended = false;
        Ok(())
    }
    fn upload(&mut self, batch: UploadBatch) -> Result<UploadId, Rejected<UploadBatch>> {
        Err(Rejected {
            request: batch,
            reason: RejectReason::new(
                RejectCode::Unsupported,
                "GLES prepares immutable frame resources directly",
            ),
        })
    }
    fn render_present(
        &mut self,
        frame: &RenderFrame,
        before_present: impl FnOnce(),
    ) -> Result<PresentOutcome, GlesError> {
        self.check_native()?;
        if self.suspended {
            return Ok(PresentOutcome::Skipped(SkipReason::Suspended));
        }
        let info = self.info.ok_or(GlesError::InvalidSurface)?;
        if frame.surface_epoch() != info.surface_epoch
            || frame.info().physical_size != info.physical_size
        {
            return Err(GlesError::WrongSurface);
        }
        let id = SubmissionId(self.next_submission);
        let next = self
            .next_submission
            .checked_add(1)
            .ok_or(GlesError::IdentityExhausted)?;
        let prepared = match self.prepare(frame, info.physical_size, false) {
            Err(GlesError::Preparing) => return Ok(PresentOutcome::Preparing),
            result => result?,
        };
        if let Err(error) =
            self.programs
                .present(&self.context, &prepared.output, info.physical_size)
        {
            if !self.drain_error() {
                self.failed_prepared.push(Rc::new(prepared));
            }
            return Err(error);
        }
        self.next_submission = next;
        self.submit(frame, prepared, Some((id, frame.frame_id())))?;
        before_present();
        {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Present);
            self.context.present().map_err(native_error)?;
        }
        Ok(PresentOutcome::PresentAccepted(frame.frame_id()))
    }
    fn retire(&mut self, resources: &[ResourceVersion]) {
        self.images.prune(resources);
        if self.check_native().is_err() {
            return;
        }
        self.glyphs.prune(resources);
        if self.check_native().is_err() {
            return;
        }
        self.effects.prune();
    }
    fn cancel_preparation(&mut self, frame: &RenderFrame) {
        self.retained.cancel(&frame.key());
    }
    fn poll(
        &mut self,
        out: &mut Vec<RenderEvent<GlesError>>,
        limit: usize,
    ) -> Result<(), GlesError> {
        self.check_native()?;
        while self.events.len() < limit {
            let Some(flight) = self.flights.front() else {
                break;
            };
            if !flight.fence.poll().map_err(native_error)? {
                break;
            }
            let flight = self.flights.pop_front().unwrap();
            if let Some(prepared) = &flight._prepared {
                prepared.recycle_geometry(&mut self.geometry);
            }
            if let Some(token) = flight.retained {
                self.retained.complete(token);
            }
            if flight.upload || flight.retained.is_some() {
                self.events.push_back(RenderEvent::ResourcesAvailable);
            }
            if let Some((id, frame)) = flight.submission {
                self.events
                    .push_back(RenderEvent::RenderFinished { id, frame });
            }
        }
        out.extend(self.events.drain(..limit.min(self.events.len())));
        self.images.prune(&[]);
        self.glyphs.prune(&[]);
        self.effects.prune();
        self.check_native()?;
        self.targets.prune_cold();
        self.check_native()?;
        Ok(())
    }
    fn has_pending(&self) -> bool {
        !self.flights.is_empty() || !self.events.is_empty()
    }
    fn suspend(&mut self) {
        self.retained.clear();
        self.suspended = true;
        self.info = None;
    }
}
/// Requested readback owns the exact frame and its GPU result until completion.
pub struct GlesCapture {
    prepared: Option<Rc<replay::Prepared>>,
    fence: Rc<native::Fence>,
    size: [u32; 2],
    output: BudgetPool,
    _frame: RenderFrame,
}
impl crate::capture::CaptureBackend for GlesBackend {
    type Error = GlesError;
    type Capture = GlesCapture;
    fn capture(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        output: &BudgetPool,
        wake: reel_core::wake::WakeSignal,
    ) -> Result<GlesCapture, GlesError> {
        if size.contains(&0)
            || size
                .iter()
                .any(|&n| n > self.context.capabilities().max_texture_size)
        {
            return Err(GlesError::InvalidSize);
        }
        let prepared = self.prepare(frame, size, true)?;
        let (prepared, fence) = self.submit(frame, prepared, None)?;
        wake.wake();
        Ok(GlesCapture {
            prepared: Some(prepared),
            fence,
            size,
            output: output.clone(),
            _frame: frame.clone(),
        })
    }
    fn poll_capture(
        &mut self,
        capture: &mut GlesCapture,
    ) -> Result<Option<crate::capture::CapturedPixels>, GlesError> {
        self.check_native()?;
        if !capture.fence.poll().map_err(native_error)? {
            return Ok(None);
        }
        let Some(prepared) = capture.prepared.take() else {
            return Ok(None);
        };
        prepared.recycle_geometry(&mut self.geometry);
        let bytes = u64::from(capture.size[0]) * u64::from(capture.size[1]) * 4;
        let ticket = capture
            .output
            .reserve(bytes)
            .map_err(|e| GlesError::Readback(e.message))?;
        let mut rgba = vec![0u8; usize::try_from(bytes).map_err(|_| GlesError::InvalidSize)?];
        self.context
            .read_rgba(&prepared.output.framebuffer, &mut rgba)
            .map_err(native_error)?;
        let row = capture.size[0] as usize * 4;
        for y in 0..capture.size[1] as usize / 2 {
            let opposite = capture.size[1] as usize - 1 - y;
            let (top, bottom) = rgba.split_at_mut(opposite * row);
            top[y * row..(y + 1) * row].swap_with_slice(&mut bottom[..row]);
        }
        for pixel in rgba.as_chunks_mut::<4>().0 {
            for channel in &mut pixel[..3] {
                let x = f32::from(*channel) / 255.;
                let s = if x <= 0.0031308 {
                    12.92 * x
                } else {
                    1.055 * x.powf(1. / 2.4) - 0.055
                };
                *channel = (s.clamp(0., 1.) * 255.).round() as u8;
            }
            pixel[3] = 255;
        }
        let rgba =
            OwnedBytes::new(rgba, ticket).map_err(|e| GlesError::Readback(e.reason.message))?;
        Ok(Some(crate::capture::CapturedPixels {
            size: capture.size,
            rgba,
        }))
    }
}
impl Drop for GlesBackend {
    fn drop(&mut self) {
        if !self.context.refresh() && self.context.finish().is_ok() {
            self.flights.clear();
            self.failed_work.clear();
            self.failed_prepared.clear();
            self.failed_images.clear();
            self.images.after_drain();
            self.glyphs.after_drain();
            self.targets.clear();
        } else {
            std::mem::forget(self.programs.clone());
            std::mem::forget(std::mem::take(&mut self.flights));
            std::mem::forget(std::mem::take(&mut self.failed_work));
            std::mem::forget(std::mem::take(&mut self.failed_prepared));
            std::mem::forget(std::mem::take(&mut self.failed_images));
            std::mem::forget(std::mem::take(&mut self.images));
            std::mem::forget(std::mem::take(&mut self.glyphs));
            std::mem::forget(std::mem::take(&mut self.targets));
        }
    }
}
pub(super) fn native_error(error: native::Error) -> GlesError {
    match error {
        native::Error::Terminal(_) | native::Error::Egl(_, 0x300e) => {
            GlesError::NativeDeviceLost(error.to_string())
        }
        native::Error::Egl(_, 0x300d) => GlesError::NativeSurface(error.to_string()),
        _ => GlesError::Device(error.to_string()),
    }
}
impl From<native::Error> for GlesError {
    fn from(error: native::Error) -> Self {
        native_error(error)
    }
}
