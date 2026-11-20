//! Native GXM execution of the common retained-frame plans.
#[path = "draw.rs"]
mod draw;
#[path = "effect.rs"]
mod effect;
#[path = "geometry.rs"]
mod geometry;
#[path = "mask.rs"]
mod mask;
#[path = "ownership.rs"]
mod ownership;
#[path = "programs.rs"]
mod programs;
#[path = "replay.rs"]
mod replay;
#[path = "resources.rs"]
mod resources;
#[path = "target_cache.rs"]
mod target_cache;
#[path = "targets.rs"]
mod targets;
use super::{Options, native, native_error};
use crate::common::GpuError;
use crate::common::renderer::Budgets;
use crate::*;
use reel_core::budget::BudgetGroup;
use resources::{Image, Images, UploadImage};
use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
};

struct CacheRetry {
    cause: GpuError,
    released: bool,
}

/// These errors name the shared temporary ledger that owns cache metadata.
/// Native MAIN/CDRAM and target capacity failures retain dependency evidence.
fn effect_metadata_pressure(error: &GpuError) -> bool {
    match error {
        GpuError::Shader { source, .. } => effect_metadata_pressure(source),
        GpuError::Budget { domain, .. } | GpuError::Pressure { domain, .. } => matches!(
            *domain,
            "transient"
                | "retained plan"
                | "image plan"
                | "target capacities"
                | "geometry packing"
                | "geometry"
                | "mesh scratch"
        ),
        _ => false,
    }
}

/// Device references and their shared resource admission limits.
#[derive(Clone, Copy)]
struct DeviceResources<'a> {
    gpu: &'a ownership::Device,
    budgets: &'a Budgets,
    limits: crate::common::GpuLimits,
}

struct Flight {
    fence: Rc<native::Fence>,
    submission: Option<(SubmissionId, FrameId)>,
    _frame: RenderFrame,
    _prepared: Option<Rc<replay::Prepared>>,
    uploads: Vec<UploadImage>,
    upload: bool,
    retained: Option<usize>,
    #[cfg(feature = "profiling")]
    trace: Option<crate::profiling::TraceLink>,
}
/// The renderer owns its native context; the host supplies only portable lifecycle
/// and presentation callbacks. Submitted resources survive until their GPU fence.
pub struct Backend {
    context: ownership::Device,
    programs: Rc<programs::Programs>,
    options: Options,
    limits: crate::common::GpuLimits,
    total_bytes: u64,
    budgets: Budgets,
    _group: BudgetGroup,
    images: Images,
    glyphs: draw::GlyphCache,
    effects: effect::Cache,
    masks: crate::common::mask::split::Cache,
    targets: target_cache::Cache,
    coverage_targets: target_cache::Cache,
    retained: crate::common::retained::Cache<resources::Target, u8>,
    effect_results: crate::common::effect_result::Cache<resources::Target>,
    geometry: geometry::Cache,
    info: Option<SurfaceInfo>,
    suspended: bool,
    next_submission: u64,
    device_generation: u64,
    flights: VecDeque<Flight>,
    events: VecDeque<RenderEvent<GpuError>>,
    failed_work: Vec<replay::Work>,
    failed_prepared: Vec<Rc<replay::Prepared>>,
    failed_images: Vec<UploadImage>,
    drain_failed: bool,
}
impl Backend {
    /// Clear immutable effect results for a controlled fixed-frame replay.
    /// The probe drains submissions first; ordinary target and image pools stay
    /// resident. This diagnostic changes cache reuse and is not a gameplay mode.
    #[cfg(feature = "profiling")]
    pub fn clear_effect_results_for_probe(&mut self) -> Result<(), GpuError> {
        self.check_native()?;
        if !self.flights.is_empty() || !self.events.is_empty() {
            return Err(GpuError::Busy);
        }
        self.effect_results.clear();
        self.check_native()?;
        crate::profiling::record_trace(
            crate::profiling::TraceKind::EffectCache,
            crate::profiling::TraceData {
                label: "probe-clear",
                status: crate::profiling::TraceStatus::Ok,
                ..Default::default()
            },
        );
        Ok(())
    }
    /// Create the device and notify the host after display completion.
    /// The wake callback only signals the host; graphics work stays on this thread.
    /// Physical admission shares MAIN with CPU arenas and accounts CDRAM separately.
    #[cfg(target_os = "vita")]
    pub fn new(
        options: Options,
        wake: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<Self, GpuError> {
        let context = super::create_context(&options, wake).map_err(native_error)?;
        Self::with_context(context, options)
    }

    fn with_context(context: native::Context, options: Options) -> Result<Self, GpuError> {
        if options.limits.max_inflight == 0 || options.limits.image_upload_bytes_per_prepare == 0 {
            return Err(GpuError::InvalidSize);
        }
        static NEXT_DEVICE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let device_generation = NEXT_DEVICE
            .try_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |generation| generation.checked_add(1),
            )
            .map_err(|_| GpuError::IdentityExhausted)?;
        let limits = options.limits.common(context.memory_state());
        let total_bytes = limits.resource_bytes;
        let context = ownership::Device::new(context);
        let group = BudgetGroup::new(total_bytes);
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
            resources: pool(BudgetCategory::GpuResource, limits.resource_bytes),
            targets: pool(BudgetCategory::Offscreen, limits.target_bytes),
            transient: pool(BudgetCategory::Temporary, limits.transient_bytes),
        };
        let programs = Rc::new(programs::Programs::new(&context, &budgets.resources)?);
        Ok(Self {
            context,
            programs,
            options,
            limits,
            total_bytes,
            budgets,
            _group: group,
            images: Default::default(),
            glyphs: Default::default(),
            effects: Default::default(),
            masks: Default::default(),
            targets: Default::default(),
            coverage_targets: Default::default(),
            retained: Default::default(),
            effect_results: Default::default(),
            geometry: Default::default(),
            info: None,
            suspended: false,
            next_submission: 1,
            device_generation,
            flights: VecDeque::new(),
            events: VecDeque::new(),
            failed_work: Vec::new(),
            failed_prepared: Vec::new(),
            failed_images: Vec::new(),
            drain_failed: false,
        })
    }
    fn check_native(&self) -> Result<(), GpuError> {
        self.context.check()
    }
    /// Physical output dimensions used when configuring the common surface.
    pub fn output_size(&self) -> [u32; 2] {
        self.context.output_size()
    }
    /// Renderer-owned domains and their shared simultaneous budget peak.
    /// Native driver allocations have separate ownership from these budgets.
    pub fn usage(&self) -> crate::Usage {
        let [cached_images, retired_images, retired_image_readers] = self.images.usage();
        crate::Usage {
            targets: self.budgets.targets.usage().bytes,
            resources: self.budgets.resources.usage().bytes,
            cached_images,
            retired_images,
            retired_image_readers,
            transient: self.budgets.transient.usage().bytes,
            cached_geometry: self.geometry.bytes(),
            shared: Some(self._group.usage()),
        }
    }
    /// Physical page commitment and capacity, read in constant time.
    pub fn native_memory_state(&self) -> native::MemoryState {
        self.context.memory_state()
    }
    /// Explicitly sample native allocator domains, separately from renderer budgets.
    /// Heap snapshots are adjacent observations; registered storage, its device
    /// suballocations and independent historical peaks must not be added together.
    pub fn native_memory_usage(&self) -> Result<super::MemoryUsage, GpuError> {
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
    pub fn reset_session(&mut self) -> Result<(), GpuError> {
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
        self.effect_results.clear();
        self.check_native()?;
        self.targets.clear();
        self.coverage_targets.clear();
        self.check_native()?;
        self.retained.clear();
        self.check_native()?;
        self.geometry.clear();
        self.check_native()?;
        self.drain_failed = false;
        Ok(())
    }
    /// Release session resources, drain display work, and close the native device.
    /// The underlying close rejects any outstanding resource or capture owner and
    /// reports the actual device failure.
    pub fn close(mut self) -> Result<(), GpuError> {
        self.reset_session()?;
        let context = self.context.clone();
        drop(self);
        context.close().map_err(|(_, error)| native_error(error))
    }
    // A failed command can already have reached the device. Retire its owners
    // only after a successful drain; a failed drain makes this backend terminal.
    fn drain_error(&mut self) -> Result<(), GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::RenderDrain);
        let result = (|| {
            self.context.refresh();
            self.check_native()?;
            self.context.finish().map_err(native_error)?;
            self.images.after_drain();
            self.glyphs.after_drain();
            self.context.refresh();
            self.check_native()
        })();
        self.drain_failed |= result.is_err();
        result
    }
    fn prepare(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        capture: bool,
    ) -> Result<replay::Prepared, GpuError> {
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
                Err(GpuError::Budget {
                    domain: "transient" | "mesh scratch" | "geometry" | "index packing",
                    ..
                })
            );
        if (fallback
            || result
                .as_ref()
                .is_err_and(|error| error.is_memory_pressure()))
            && !self.drain_failed
        {
            let cause = result.err().unwrap();
            let resident = self._group.usage().bytes;
            let metadata_pressure = effect_metadata_pressure(&cause);
            if metadata_pressure {
                self.effect_results.clear();
                self.targets.groups.disable();
            } else {
                self.effect_results.release_outputs();
            }
            self.targets.release_idle();
            self.coverage_targets.release_idle();
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
            if metadata_pressure {
                self.targets.groups.clear();
            }
            return self.resolve_pressure(result);
        }
        self.resolve_pressure(result)
    }

    fn resolve_pressure<T>(&self, result: Result<T, GpuError>) -> Result<T, GpuError> {
        // Completed work has been polled and idle caches reclaimed. Only live
        // submissions can justify waiting for another release notification.
        result.map_err(|error| error.after_reclamation(!self.flights.is_empty()))
    }
    fn prepare_cached(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        capture: bool,
        retry: Option<CacheRetry>,
    ) -> Result<replay::Prepared, GpuError> {
        let effect_results_enabled = retry.is_none();
        self.check_native()?;
        self.masks.prune();
        if self.drain_failed {
            return Err(GpuError::NativeDeviceLost(
                "GPU failure could not be drained".into(),
            ));
        }
        let limits = self.limits;
        if !capture && self.retained.pending(&frame.key()) {
            return Err(GpuError::Preparing);
        }
        if self.flights.len() >= limits.max_inflight {
            return Err(GpuError::Busy);
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
            && retry.cause.is_memory_pressure()
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
            self.total_bytes,
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
                > self.total_bytes.saturating_sub(
                    self.budgets
                        .resources
                        .usage()
                        .bytes
                        .saturating_add(self.budgets.targets.usage().bytes)
                        .saturating_add(self.budgets.transient.usage().bytes),
                )
        {
            self.targets.clear();
            self.coverage_targets.clear();
            self.check_native()?;
        }
        for mask in &plan.masks {
            let caps = self.context.capabilities();
            if mask.definition.program.operations().len() > mask::OPERATION_CAPACITY {
                return Err(GpuError::Unsupported {
                    at: mask.at,
                    record: "mask graph exceeds the device interpreter capacity",
                });
            }
            if mask.definition.images.len() > 4 || caps.max_texture_units < 4 {
                return Err(GpuError::Unsupported {
                    at: mask.at,
                    record: "mask exceeds device texture-unit capacity",
                });
            }
            for handle in &mask.definition.images {
                if plan.images.iter().any(|image| {
                    image.source.handle() == *handle
                        && matches!(
                            image.source.view(),
                            reel_ui::resources::ResourceView::Yuv420(_)
                        )
                }) {
                    return Err(GpuError::Unsupported {
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
        let upload = upload.and_then(|ready| {
            if !ready || plan.aliases.is_empty() {
                return Ok(ready);
            }
            images.reserve(plan.aliases.len());
            for alias in &plan.aliases {
                let backing = images
                    .get(&alias.backing.handle())
                    .ok_or(GpuError::InvalidSize)?
                    .clone();
                let storage = crate::common::renderer::reserve(
                    &self.budgets.transient,
                    &local.transient,
                    (size_of::<resources::Image>() + 2 * size_of::<usize>() + 128) as u64,
                    limits.transient_bytes,
                    "image plan",
                )?;
                let image = resources::Image::atlas(backing, alias.source, storage)?;
                images.insert(alias.source.handle(), Rc::new(image));
            }
            Ok(ready)
        });
        drop(plan);
        drop(plan_tickets);
        let ready = match upload {
            Ok(ready) => ready,
            Err(error) => {
                if matches!(error, GpuError::PixelsNeeded(_)) && touched.is_empty() {
                    return Err(error);
                }
                self.images.release_idle();
                if let Err(fault) = self.drain_error() {
                    self.failed_images.extend(touched);
                    return Err(fault);
                }
                return Err(error);
            }
        };
        if !ready {
            let fence = match self.context.fence() {
                Ok(fence) => Rc::new(fence),
                Err(error) => {
                    if let Err(fault) = self.drain_error() {
                        self.failed_images.extend(touched);
                        return Err(fault);
                    }
                    return Err(error.into());
                }
            };
            if let Err(error) = self.context.flush() {
                if let Err(fault) = self.drain_error() {
                    self.failed_images.extend(touched);
                    return Err(fault);
                }
                return Err(error.into());
            }
            self.flights.push_back(Flight {
                fence,
                submission: None,
                _frame: frame.clone(),
                _prepared: None,
                uploads: touched,
                upload: true,
                retained: None,
                #[cfg(feature = "profiling")]
                trace: crate::profiling::trace_link(),
            });
            return Err(GpuError::Busy);
        }
        // Captures retain their output until readback, independently of later frames.
        let mut capture_targets = target_cache::Cache::default();
        let mut capture_coverage_targets = target_cache::Cache::default();
        if capture {
            capture_targets.configure(size);
            capture_coverage_targets.configure(size);
        } else {
            self.targets.configure(size);
            self.coverage_targets.configure(size);
        }
        self.check_native()?;
        let targets = if capture {
            &mut capture_targets
        } else {
            &mut self.targets
        };
        let coverage_targets = if capture {
            &mut capture_coverage_targets
        } else {
            &mut self.coverage_targets
        };
        match replay::prepare(
            DeviceResources {
                gpu: &self.context,
                budgets: &self.budgets,
                limits: self.limits,
            },
            &self.programs,
            replay::Caches {
                images: &mut self.images,
                glyphs: &mut self.glyphs,
                effects: &mut self.effects,
                masks: &mut self.masks,
                targets,
                coverage_targets,
                geometry: &mut self.geometry,
                retained: &self.retained,
                effect_results: &self.effect_results,
                effect_results_enabled,
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
                    self.retained.stage(
                        token,
                        prepared.output.clone(),
                        prepared.output.view.allocated_bytes() as u64,
                    );
                    self.targets.exclude_retained(&prepared.output);
                    self.coverage_targets.exclude_retained(&prepared.output);
                    self.check_native()?;
                    if let Err(error) = self.submit(frame, prepared, None) {
                        self.retained.cancel(&frame.key());
                        return Err(error);
                    }
                    Err(GpuError::Preparing)
                } else {
                    Ok(prepared)
                }
            }
            Err((error, work)) => {
                self.images.release_idle();
                if error.is_memory_pressure() {
                    reel_core::profile::count(if work.stream_started {
                        reel_core::profile::Phase::RenderStreamPressure
                    } else {
                        reel_core::profile::Phase::RenderPreparationPressure
                    });
                    if !work.stream_started {
                        self.context.refresh();
                        if let Err(fault) = self.check_native() {
                            self.failed_work.push(work);
                            return Err(fault);
                        }
                        // Preparation has issued no draws. Native transfer leases
                        // retain any glyph uploads independently of this work owner.
                        drop(work);
                        self.check_native()?;
                        let fence = self.context.fence().map_err(native_error)?;
                        if fence.poll().map_err(native_error)? {
                            self.images.complete_uploads(&touched);
                        } else {
                            // Give pending uploads the normal completion wakeup
                            // needed for pressure reclamation and another attempt.
                            self.flights.push_back(Flight {
                                fence: Rc::new(fence),
                                submission: None,
                                _frame: frame.clone(),
                                _prepared: None,
                                uploads: touched,
                                upload: true,
                                retained: None,
                                #[cfg(feature = "profiling")]
                                trace: crate::profiling::trace_link(),
                            });
                        }
                        return Err(error);
                    }
                }
                if let Err(fault) = self.drain_error() {
                    self.failed_work.push(work);
                    return Err(fault);
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
    ) -> Result<(Rc<replay::Prepared>, Rc<native::Fence>), GpuError> {
        let prepared = Rc::new(prepared);
        let fence_timing = reel_core::profile::Scope::new(reel_core::profile::Phase::RenderFence);
        let fence = match self.context.fence() {
            Ok(fence) => Rc::new(fence),
            Err(error) => {
                if let Err(fault) = self.drain_error() {
                    self.failed_prepared.push(prepared);
                    return Err(fault);
                }
                return Err(error.into());
            }
        };
        if let Err(error) = self.context.flush() {
            if let Err(fault) = self.drain_error() {
                self.failed_prepared.push(prepared);
                return Err(fault);
            }
            return Err(error.into());
        }
        drop(fence_timing);
        self.effect_results.submitted();
        for pending in prepared.take_effect_pending() {
            self.effect_results
                .publish(pending, &self.budgets.transient);
        }
        self.flights.push_back(Flight {
            fence: fence.clone(),
            submission,
            _frame: frame.clone(),
            _prepared: Some(prepared.clone()),
            uploads: Vec::new(),
            upload: false,
            retained: prepared.retained,
            #[cfg(feature = "profiling")]
            trace: crate::profiling::trace_link(),
        });
        self.check_native()?;
        self.images.submitted();
        self.targets.submitted(prepared.targets());
        self.coverage_targets.submitted(prepared.targets());
        Ok((prepared, fence))
    }
}
impl RenderBackend for Backend {
    fn device_generation(&self) -> u64 {
        self.device_generation
    }
    type Error = GpuError;
    fn vsync(&self) -> Option<bool> {
        Some(self.context.vsync())
    }
    fn set_vsync(&mut self, enabled: bool) -> Result<Option<bool>, GpuError> {
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
                max_bytes: self.limits.transient_bytes,
            },
        }
    }
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), GpuError> {
        self.check_native()?;
        self.retained.clear();
        self.effect_results.clear();
        self.check_native()?;
        if info.physical_size != self.output_size()
            || info.surface_epoch.0 == 0
            || !info.scale_factor.is_finite()
            || info.scale_factor <= 0.
            || info.color_encoding != ColorEncoding::Srgb
            || info.alpha_mode != AlphaMode::Opaque
        {
            return Err(GpuError::InvalidSurface);
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
                "GXM prepares immutable frame resources directly",
            ),
        })
    }
    fn render_present(
        &mut self,
        frame: &RenderFrame,
        before_present: impl FnOnce(),
    ) -> Result<PresentOutcome, GpuError> {
        #[cfg(feature = "profiling")]
        let trace = crate::profiling::TraceAttempt::begin(frame.frame_id());
        let result = (|| {
            self.check_native()?;
            if self.suspended {
                return Ok(PresentOutcome::Skipped(SkipReason::Suspended));
            }
            let info = self.info.ok_or(GpuError::InvalidSurface)?;
            if frame.surface_epoch() != info.surface_epoch
                || frame.info().physical_size != info.physical_size
            {
                return Err(GpuError::WrongSurface);
            }
            let id = SubmissionId(self.next_submission);
            let next = self
                .next_submission
                .checked_add(1)
                .ok_or(GpuError::IdentityExhausted)?;
            #[cfg(feature = "profiling")]
            let completion = crate::profiling::FrameCompletion::start();
            #[cfg(feature = "profiling")]
            let prepare_trace = crate::profiling::TracePass::begin("prepare", Default::default());
            let preparation = self.prepare(frame, info.physical_size, false);
            #[cfg(feature = "profiling")]
            if let Some(trace) = prepare_trace {
                use crate::profiling::{TraceData, TraceStatus};
                let (status, data) = match &preparation {
                    Ok(prepared) => (TraceStatus::Ok, prepared.output.trace_data()),
                    Err(GpuError::Preparing | GpuError::Busy) => {
                        (TraceStatus::Preparing, TraceData::default())
                    }
                    Err(_) => (TraceStatus::Error, TraceData::default()),
                };
                trace.finish(status, data);
            }
            let prepared = match preparation {
                Err(GpuError::Preparing) => return Ok(PresentOutcome::Preparing),
                result => result?,
            };
            if let Err(error) =
                self.programs
                    .present(&self.context, &prepared.output, info.physical_size)
            {
                if let Err(fault) = self.drain_error() {
                    self.failed_prepared.push(Rc::new(prepared));
                    return Err(fault);
                }
                return Err(error);
            }
            self.next_submission = next;
            #[cfg(feature = "profiling")]
            crate::profiling::trace_submission(id.0);
            let (_, fence) = self.submit(frame, prepared, Some((id, frame.frame_id())))?;
            #[cfg(feature = "profiling")]
            if let Some(completion) = completion {
                completion
                    .complete(|timeout| fence.wait(timeout))
                    .map_err(native_error)?;
            }
            #[cfg(not(feature = "profiling"))]
            let _ = fence;
            before_present();
            {
                let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Present);
                self.context.present().map_err(native_error)?;
            }
            Ok(PresentOutcome::PresentAccepted(frame.frame_id()))
        })();
        #[cfg(feature = "profiling")]
        if let Some(trace) = trace {
            use crate::profiling::{TraceData, TraceStatus};
            let status = match &result {
                Ok(PresentOutcome::PresentAccepted(_)) => TraceStatus::Ok,
                Ok(PresentOutcome::Preparing) => TraceStatus::Preparing,
                Ok(_) => TraceStatus::Skipped,
                Err(GpuError::Preparing | GpuError::Busy) => TraceStatus::Preparing,
                Err(_) => TraceStatus::Error,
            };
            trace.finish(status, TraceData::default());
        }
        result
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
    fn poll(&mut self, out: &mut Vec<RenderEvent<GpuError>>, limit: usize) -> Result<(), GpuError> {
        self.check_native()?;
        while self.events.len() < limit {
            let Some(flight) = self.flights.front() else {
                break;
            };
            if !flight.fence.poll().map_err(native_error)? {
                break;
            }
            let flight = self.flights.pop_front().unwrap();
            #[cfg(feature = "profiling")]
            if let Some(link) = flight.trace {
                let mut data = flight
                    ._prepared
                    .as_ref()
                    .map_or_else(crate::profiling::TraceData::default, |prepared| {
                        prepared.output.trace_data()
                    });
                data.label = if flight.submission.is_some() {
                    "final-flight-observed"
                } else if flight.upload {
                    "upload-flight-observed"
                } else {
                    "retained-flight-observed"
                };
                data.count = 1;
                crate::profiling::trace_completed(link, data);
            }
            if let Some(prepared) = &flight._prepared {
                for image in prepared.images().values() {
                    self.images.complete_upload(image);
                }
                prepared.recycle_geometry(&mut self.geometry);
            }
            self.images.complete_uploads(&flight.uploads);
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
        // A frame fence represents its last scene. Retire every completed native
        // scene and transfer before pruning pools or admitting the next frame.
        self.context.poll().map_err(native_error)?;
        out.extend(self.events.drain(..limit.min(self.events.len())));
        self.images.prune(&[]);
        self.glyphs.prune(&[]);
        self.effects.prune();
        self.check_native()?;
        self.check_native()?;
        Ok(())
    }
    fn has_pending(&self) -> bool {
        !self.flights.is_empty() || !self.events.is_empty()
    }
    fn suspend(&mut self) {
        self.retained.clear();
        self.effect_results.clear();
        self.suspended = true;
        self.info = None;
    }
}
/// Requested readback owns the exact frame and its GPU result until completion.
pub struct Capture {
    prepared: Option<Rc<replay::Prepared>>,
    fence: Rc<native::Fence>,
    size: [u32; 2],
    output: BudgetPool,
    _frame: RenderFrame,
    pixels: Option<(Vec<u8>, reel_core::budget::BudgetTicket)>,
    wake: reel_core::wake::WakeSignal,
}
impl crate::capture::CaptureBackend for Backend {
    type Error = GpuError;
    type Capture = Capture;
    fn capture(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        output: &BudgetPool,
        wake: reel_core::wake::WakeSignal,
    ) -> Result<Capture, GpuError> {
        if size.contains(&0)
            || size
                .iter()
                .any(|&n| n > self.context.capabilities().max_texture_size)
        {
            return Err(GpuError::InvalidSize);
        }
        let prepared = self.prepare(frame, size, true)?;
        let (prepared, fence) = self.submit(frame, prepared, None)?;
        wake.wake();
        Ok(Capture {
            prepared: Some(prepared),
            fence,
            size,
            output: output.clone(),
            _frame: frame.clone(),
            pixels: None,
            wake,
        })
    }
    fn poll_capture(
        &mut self,
        capture: &mut Capture,
        max_bytes: u64,
    ) -> Result<Option<crate::capture::CapturedPixels>, GpuError> {
        self.check_native()?;
        if !capture.fence.poll().map_err(native_error)? {
            return Ok(None);
        }
        let Some(prepared) = capture.prepared.as_ref() else {
            return Ok(None);
        };
        let row = capture.size[0] as usize * 4;
        let total = row
            .checked_mul(capture.size[1] as usize)
            .ok_or(GpuError::InvalidSize)?;
        if capture.pixels.is_none() {
            let mut ticket = capture
                .output
                .reserve(total as u64)
                .map_err(|e| GpuError::Readback(e.message))?;
            let pixels = Vec::with_capacity(total);
            ticket
                .resize(pixels.capacity() as u64)
                .map_err(|e| GpuError::Readback(e.message))?;
            capture.pixels = Some((pixels, ticket));
        }
        let (rgba, _) = capture.pixels.as_mut().unwrap();
        let first_row = rgba.len() / row;
        let rows =
            crate::capture::readback_rows(row, capture.size[1] as usize - first_row, max_bytes);
        let start = rgba.len();
        rgba.resize(start + rows * row, 0);
        self.context
            .read_rgba(
                &prepared.output.framebuffer,
                first_row as u32,
                &mut rgba[start..],
            )
            .map_err(native_error)?;
        if rgba.len() < total {
            capture.wake.wake();
            return Ok(None);
        }
        let prepared = capture.prepared.take().unwrap();
        prepared.recycle_geometry(&mut self.geometry);
        let (rgba, ticket) = capture.pixels.take().unwrap();
        let rgba =
            OwnedBytes::new(rgba, ticket).map_err(|e| GpuError::Readback(e.reason.message))?;
        Ok(Some(crate::capture::CapturedPixels {
            size: capture.size,
            rgba,
            encoding: crate::capture::CaptureEncoding::Linear,
        }))
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        if !self.context.refresh() && self.context.finish().is_ok() {
            self.flights.clear();
            self.failed_work.clear();
            self.failed_prepared.clear();
            self.failed_images.clear();
            self.images.after_drain();
            self.glyphs.after_drain();
            self.targets.clear();
            self.coverage_targets.clear();
        } else {
            std::mem::forget(self.programs.clone());
            std::mem::forget(std::mem::take(&mut self.flights));
            std::mem::forget(std::mem::take(&mut self.failed_work));
            std::mem::forget(std::mem::take(&mut self.failed_prepared));
            std::mem::forget(std::mem::take(&mut self.failed_images));
            std::mem::forget(std::mem::take(&mut self.images));
            std::mem::forget(std::mem::take(&mut self.glyphs));
            std::mem::forget(std::mem::take(&mut self.targets));
            std::mem::forget(std::mem::take(&mut self.coverage_targets));
        }
    }
}
