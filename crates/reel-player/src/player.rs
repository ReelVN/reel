//! Shared player lifecycle, presentation and input. Platform assemblies supply
//! a host and renderer; this module contains no native window or device types.
use crate::{
    hal::{DisplayMetrics, Host},
    presentation::Presentation,
    session::RecordedProjection,
};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    clock::MonoTime,
    identity::{FrameId, SurfaceEpoch},
};
use reel_render::{RenderBackend, RenderEvent, SurfaceInfo, capture::CaptureBackend};
use reel_ui::{
    bridge::PresentationBridge,
    core::pointer::PointerButtons,
    core::*,
    dpi::PhysicalSize,
    frame::{BuildLimits, FrameBuild, FrameInfo, RenderFrame, StepBudget},
};
use std::sync::Arc;
type Error = Box<dyn std::error::Error>;
mod display;
pub(crate) mod pixel_recovery;
pub(crate) mod session;
mod ui_animation;
use session::PlayerSession;

/// Shared player configuration. Display size and author canvas remain independent.
pub struct PlayerOptions {
    pub assets: crate::assets::AssetOptions,
    /// Some selects archive resolution; None retains ordinary directory assets.
    pub asset_packages: Option<Vec<std::path::PathBuf>>,
    /// Explicit loose overrides for an archive-backed work.
    pub asset_overlay: Option<std::path::PathBuf>,
    /// Resource directory overlay, defaulting to the host OS name.
    /// This selects assets independently of the player platform.
    pub resource_target: String,
    /// Prepared logical-to-physical resources supplied by authoring tools.
    /// The player opens these files directly and leaves archive-only entries available.
    pub resource_files: Option<std::collections::BTreeMap<String, std::path::PathBuf>>,
    /// Canonical language tags resolved through the same asset service as images and media.
    pub catalogs: crate::localization::CatalogIndex,
    pub media: crate::media::MediaOptions,
    pub title: String,
    pub locale: String,
    pub logical_size: Option<[u32; 2]>,
    pub canvas_size: Option<[u32; 2]>,
    /// Host-selected user data, independent of read-only game assets.
    pub save_directory: Option<std::path::PathBuf>,
    pub save_bytes: u64,
    /// CPU frame preparation and metadata admission limit, charged by actual
    /// capacity. The desktop default is 64 MiB; smaller hosts select their quota.
    pub frame_bytes: u64,
    /// Maximum capture readback bytes copied per poll; a complete row can exceed it.
    /// The default is 256 KiB.
    pub capture_readback_bytes_per_poll: u64,
}
impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            assets: Default::default(),
            asset_packages: None,
            asset_overlay: None,
            resource_target: std::env::consts::OS.into(),
            resource_files: None,
            catalogs: Default::default(),
            media: Default::default(),
            title: "Reel".into(),
            locale: "en".into(),
            logical_size: None,
            canvas_size: None,
            save_directory: None,
            save_bytes: 256 << 20,
            frame_bytes: 64 << 20,
            capture_readback_bytes_per_poll: 256 << 10,
        }
    }
}

impl PlayerOptions {
    /// Open one shared asset service for rendering, localization and media.
    /// Prepared files take precedence over archives; otherwise the configured
    /// directory or archive overlay is resolved for the selected resource target.
    /// Transfers prepared file mappings to the service without duplicating them.
    pub fn open_assets(&mut self) -> Result<crate::assets::Assets, reel_core::io::IoError> {
        use crate::assets::Assets;
        if let Some(files) = self.resource_files.take() {
            Assets::selected(self.assets.clone(), files, self.asset_packages.clone())
        } else if let Some(packages) = &self.asset_packages {
            Assets::packaged_for_target(
                self.assets.clone(),
                packages.clone(),
                self.asset_overlay.clone(),
                self.resource_target.clone(),
            )
        } else {
            Assets::directory_for_target(self.assets.clone(), self.resource_target.clone())
        }
    }
}

/// A running player assembled from a renderer and portable host services.
/// The platform owns device recovery and its event loop; this type owns the
/// session, accepted view, resource readiness and logical time suspension.
pub struct Player<B: RenderBackend + CaptureBackend> {
    pub(crate) backend: B,
    pub(crate) presentation: Presentation,
    pub(crate) size: PhysicalSize<u32>,
    pub(crate) pool: BudgetPool,
    pub(crate) resources: Vec<reel_ui::resources::ResourceLease>,
    pub(crate) fonts: std::collections::BTreeMap<String, usize>,
    pub(crate) active_fonts: Vec<String>,
    pub(crate) font_requests: std::collections::BTreeMap<String, crate::assets::LoadId>,
    pub(crate) canvas_id: Option<WidgetId>,
    pub(crate) game: Option<Arc<reel_engine::aside::GameSettings>>,
    pub(crate) fit_initial_canvas: bool,
    pub(crate) epoch: u64,
    pub(crate) pointer: reel_ui::dpi::PhysicalPosition<f64>,
    pub(crate) pointer_buttons: PointerButtons,
    pub(crate) root_id: WidgetId,
    pub(crate) session: Option<PlayerSession>,
    pub(crate) recorded_projection: Option<RecordedProjection>,
    pub(crate) closing: bool,
    pub(crate) first_frame_pending: bool,
    pub(crate) ui_animation: ui_animation::UiAnimation,
    pub(crate) completions: Vec<RenderEvent<<B as RenderBackend>::Error>>,
    #[cfg(all(feature = "performance", feature = "window"))]
    pub(crate) desktop_trace: Option<crate::performance::recording::desktop::Trace>,
    pub(crate) pixel_recovery: pixel_recovery::PixelRecovery,
    pub(crate) captures: crate::capture::CaptureService<B::Capture>,
    pub(crate) window_state: Arc<reel_core::window::WindowInfo>,
    pub(crate) window_request: Option<reel_engine::aside::window::WindowRequestId>,
    pub(crate) input_hints: reel_ui::input::InputTracker,
}
impl<B> Player<B>
where
    B: RenderBackend + CaptureBackend<Error = <B as RenderBackend>::Error>,
{
    /// Mount an already loaded story. The driver carries its asset and media
    /// services; the renderer must belong to the supplied host's output surface.
    pub fn new(
        driver: crate::aside_session::AsideSessionUi,
        backend: B,
        host: &mut impl Host,
        options: &PlayerOptions,
    ) -> Result<Self, Error> {
        Self::mount(PlayerSession::aside(driver), backend, host, options)
    }

    pub(crate) fn mount(
        mut session: PlayerSession,
        mut backend: B,
        host: &mut impl Host,
        options: &PlayerOptions,
    ) -> Result<Self, Error> {
        use reel_ui::app::{RenderRoot, RenderRootOptions, WindowSizePolicy};
        let metrics = host.display_metrics();
        let [width, height] = metrics.physical_size;
        let info = SurfaceInfo {
            physical_size: metrics.physical_size,
            scale_factor: metrics.scale_factor,
            surface_epoch: SurfaceEpoch(1),
            color_encoding: reel_ui::resources::ColorEncoding::Srgb,
            alpha_mode: reel_render::AlphaMode::Opaque,
        };
        backend.configure(info)?;
        if let Some(story) = session.story_mut() {
            story.set_frame_capture_enabled(true);
            story.set_window_control_enabled(true);
        }
        session.ui_mut()?.set_resource_waker(host.wake_callback());
        let widget = session.ui().widget().prepare().erased();
        let root_id = widget.id();
        let canvas = if let Some(size) = options.canvas_size {
            reel_ui::widgets::Canvas::new(widget, size)
        } else {
            reel_ui::widgets::Canvas::viewport(widget)
        }
        .prepare();
        let canvas_id = canvas.id();
        let root = RenderRoot::new(
            canvas,
            host.ui_signal_handler(),
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(width, height),
                scale_factor: metrics.scale_factor,
                default_font: None,
            },
        );
        let bridge = PresentationBridge::new(
            root,
            info.surface_epoch,
            reel_ui::bridge::BridgeLimits {
                geometry: reel_ui::app::GeometryLimits { max_bytes: 2 << 20 },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 16 << 20,
            },
        )
        .map_err(|error| format!("bridge: {error:?}"))?;
        let notify = host.wake_callback();
        let mut player = Self {
            backend,
            presentation: Presentation::new(bridge, info),
            size: PhysicalSize::new(width, height),
            pool: BudgetPool::new(
                BudgetCategory::CpuOutput,
                QueueLimits {
                    max_requests: u32::MAX,
                    max_bytes: options.frame_bytes,
                },
            ),
            resources: Vec::new(),
            fonts: Default::default(),
            active_fonts: Vec::new(),
            font_requests: Default::default(),
            canvas_id: Some(canvas_id),
            game: None,
            fit_initial_canvas: options.logical_size.is_none() && options.canvas_size.is_none(),
            epoch: 1,
            pointer: Default::default(),
            pointer_buttons: Default::default(),
            root_id,
            session: Some(session),
            recorded_projection: None,
            closing: false,
            first_frame_pending: true,
            ui_animation: Default::default(),
            completions: Vec::new(),
            #[cfg(all(feature = "performance", feature = "window"))]
            desktop_trace: None,
            pixel_recovery: pixel_recovery::PixelRecovery::default(),
            captures: crate::capture::CaptureService::new(
                options.assets.input_bytes,
                options.capture_readback_bytes_per_poll,
                reel_core::wake::WakeSignal::new(move || notify()),
            ),
            window_state: Arc::new(Default::default()),
            window_request: None,
            input_hints: Default::default(),
        };
        player.publish_window_info(host)?;
        Ok(player)
    }

    /// Borrow the device adapter for native recovery or surface replacement.
    pub fn renderer_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Access the active story to install save storage and a media factory used
    /// by session replacement and snapshot restoration.
    pub fn story_mut(&mut self) -> Option<&mut crate::story_runtime::StoryRuntime> {
        self.session.as_mut()?.story_mut()
    }

    /// Whether rendering or a frozen exposure repaint is currently eligible.
    pub fn needs_redraw(&self) -> bool {
        self.presentation.needs_redraw() && self.frame_inputs_ready()
    }

    fn frame_inputs_ready(&self) -> bool {
        if self.presentation.is_preparing() {
            return true;
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.ui().images_block_presentation())
        {
            return false;
        }
        if !self.is_diagnostic() && self.active_fonts.is_empty() {
            return false;
        }
        !self.first_frame_pending
            || (!self.settings_pending()
                && self
                    .session
                    .as_ref()
                    .is_some_and(PlayerSession::initial_frame_ready))
    }

    /// Apply the geometry carried by a display event. Suspended displays retain
    /// the new geometry and wait for resume before configuring their device.
    /// Repeated geometry preserves the surface and its accepted input view.
    pub fn resize(&mut self, host: &mut impl Host, metrics: DisplayMetrics) -> Result<(), Error> {
        let current = self.presentation.surface_info();
        if current.physical_size != metrics.physical_size
            || current.scale_factor != metrics.scale_factor
        {
            let suspended = self.presentation.is_suspended();
            self.invalidate_surface(metrics)?;
            if suspended {
                self.presentation.suspend();
            } else {
                self.backend.configure(self.presentation.surface_info())?;
            }
        }
        let complete = host.window_resized();
        self.publish_window_info(host)?;
        if complete {
            self.complete_window_change(Ok(self.window_state.clone()))?;
        }
        host.request_redraw();
        Ok(())
    }
    /// Cancel active input, retire presentation and freeze the session clock.
    pub fn suspend(&mut self, host: &mut impl Host) -> Result<(), Error> {
        self.ui_animation.reset();
        self.presentation
            .bridge
            .handle_text_event(TextEvent::WindowFocusChange(false));
        self.presentation.bridge.cancel_pointer_inputs();
        self.pointer_event(PointerKind::Cancel)?;
        self.reset_pixel_recovery();
        self.presentation.cancel_preparation(&mut self.backend);
        self.presentation.suspend();
        self.backend.suspend();
        if let Some(session) = &mut self.session {
            session.ui_mut()?.clear_presented();
            session.suspend(host.monotonic_now().0)?;
        }
        self.publish_pointer_info()
    }
    /// Configure a restored native surface before resuming session time.
    pub fn resume(&mut self, host: &mut impl Host) -> Result<(), Error> {
        self.invalidate_surface(host.display_metrics())?;
        self.backend.configure(self.presentation.surface_info())?;
        let complete = host.window_resized();
        self.publish_window_info(host)?;
        if complete {
            self.complete_window_change(Ok(self.window_state.clone()))?;
        }
        if let Some(session) = &mut self.session {
            session.resume(host.monotonic_now().0);
        }
        host.request_redraw();
        Ok(())
    }
    pub(crate) fn invalidate_surface(&mut self, metrics: DisplayMetrics) -> Result<(), Error> {
        self.reset_pixel_recovery();
        self.presentation.cancel_preparation(&mut self.backend);
        let [width, height] = metrics.physical_size;
        self.size = PhysicalSize::new(width, height);
        self.epoch = self.epoch.checked_add(1).ok_or("surface epoch exhausted")?;
        self.presentation.replace_surface(SurfaceInfo {
            physical_size: [width, height],
            scale_factor: metrics.scale_factor,
            surface_epoch: SurfaceEpoch(self.epoch),
            ..self.presentation.surface_info()
        });
        // Invalidates candidates even if fresh negotiation subsequently fails.
        if let Some(session) = &mut self.session {
            session.ui_mut()?.clear_presented();
            session.sync(&mut self.presentation.bridge, self.root_id)?;
        }
        self.publish_pointer_info()?;
        Ok(())
    }

    /// Mark model changes for preparation at the next rendering opportunity.
    pub fn invalidate(&mut self) {
        self.presentation.invalidate();
    }

    pub(crate) fn is_diagnostic(&self) -> bool {
        match self.session {
            Some(PlayerSession::Aside { .. }) => false,
            #[cfg(any(feature = "vita", test))]
            Some(PlayerSession::Launcher(_)) => false,
            _ => true,
        }
    }

    /// Next story or capture wake in the host's monotonic time domain.
    pub fn story_deadline(&self, now: MonoTime) -> Option<MonoTime> {
        if self.captures.busy() {
            return Some(MonoTime(now.0.saturating_add(8_000_000)));
        }
        let story = self
            .session
            .as_ref()
            .and_then(|session| session.wake_deadline(now, self.frame_production()));
        let animation = self.ui_animation_deadline(now);
        match (story, animation) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn frame_production(&self) -> crate::story_runtime::FrameProduction {
        use crate::{demand::Model, story_runtime::FrameProduction};
        if self.presentation.is_preparing() {
            FrameProduction::CandidateRetained
        } else if matches!(
            self.presentation.demand.model,
            Model::Pending | Model::Preparing | Model::Waiting
        ) {
            FrameProduction::ProjectionQueued
        } else {
            FrameProduction::Ready
        }
    }

    /// Advance ready work within one scheduling slice and apply game settings.
    pub fn tick(&mut self, host: &mut impl Host) -> Result<(), Error> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Tick);
        self.publish_pointer_info()?;
        self.maintain_window_requests(host)?;
        self.maintain_captures()?;
        self.maintain_pixel_recovery()?;
        let production = self.frame_production();
        self.request_ui_frame(host);
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        if !session.needs_tick() && !session.ui().images_need_poll() {
            return Ok(());
        }
        let now = || host.monotonic_now().0;
        let budget = reel_engine::aside::SliceBudget {
            deadline_ns: Some(now().saturating_add(2_000_000)),
            ..Default::default()
        };
        let before = session.ui().projection_stamp();
        let identity = session.ui().session().identity();
        let frame_was_ready = session.frame_ready();
        {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Session);
            session.tick(budget, production, now)?;
        }
        if identity != session.ui().session().identity() {
            self.ui_animation.reset();
            self.pixel_recovery.cancel(&session.ui().assets());
            self.presentation.cancel_preparation(&mut self.backend);
            self.recorded_projection = None;
        }
        if !self.closing && session.exit_requested() {
            return self.close();
        }
        self.maintain_captures()?;
        let settings_changed = if self.closing {
            false
        } else {
            self.apply_game_settings(host)?
        };
        self.maintain_window_requests(host)?;
        if !self.closing {
            let session = self.session.as_mut().unwrap();
            session.check_fonts()?;
            if self.settings_pending() {
                return Ok(());
            }
            let session = self.session.as_mut().unwrap();
            session.sync(&mut self.presentation.bridge, self.root_id)?;
            if settings_changed
                || before != session.ui().projection_stamp()
                || (!frame_was_ready && session.frame_ready())
            {
                self.invalidate();
            }
        }
        Ok(())
    }

    fn maintain_captures(&mut self) -> Result<(), Error> {
        if let Some(PlayerSession::Aside { story, .. }) = &mut self.session
            && !story.capturing()
        {
            let changed = self.captures.drive(
                &mut self.backend,
                story.driver_mut()?,
                self.presentation.accepted_frame.as_ref(),
            )?;
            if changed {
                story.wake_input();
            }
        }
        if let Some((id, key, demand)) = self.captures.take_pixel_demand() {
            self.request_pixels(Some(id), key, demand)?;
        }
        Ok(())
    }

    fn reset_pixel_recovery(&mut self) {
        if let Some(session) = &self.session {
            self.pixel_recovery.cancel(&session.ui().assets());
        }
        if let Some((id, key)) = self.captures.waiting_pixels() {
            self.captures.pixels_ready(id, &key);
        }
    }

    fn request_pixels(
        &mut self,
        capture_id: Option<reel_engine::aside::capture::CaptureId>,
        key: reel_ui::bridge::CandidateKey,
        demand: reel_render::PixelDemand,
    ) -> Result<(), Error> {
        if self.session.is_none() {
            return Err("pixel restore requires an active session".into());
        }
        if let Err(error) = self.pixel_recovery.request(
            capture_id,
            key.clone(),
            self.backend.device_generation(),
            demand,
            &self.pool,
        ) {
            let assets = self.session.as_ref().unwrap().ui().assets();
            if let Some(id) = capture_id {
                self.pixel_recovery.cancel_capture(&assets);
                self.captures.pixels_failed(id, &key, error);
            } else {
                self.pixel_recovery.cancel_frame(&assets);
                self.presentation.pixels_failed(&key, &mut self.backend);
                eprintln!("image restore admission failed; keeping last accepted view: {error}");
            }
            return Ok(());
        }
        self.maintain_pixel_recovery()
    }

    fn maintain_pixel_recovery(&mut self) -> Result<(), Error> {
        if !self.pixel_recovery.active() {
            return Ok(());
        }
        let Some(session) = self.session.as_ref() else {
            return Ok(());
        };
        let assets = session.ui().assets();
        let frame = self.presentation.preparing_key();
        let capture = self.captures.waiting_pixels();
        let ready = match self.pixel_recovery.poll(
            &assets,
            self.backend.device_generation(),
            frame.as_ref(),
            capture.as_ref().map(|(id, key)| (*id, key)),
        ) {
            Ok(ready) => ready,
            Err(failure) => {
                let error = session
                    .ui()
                    .image_restore_diagnostic(&failure.source, &failure.error);
                if let Some((id, key)) = failure.capture {
                    self.captures.pixels_failed(id, &key, error.clone());
                }
                if let Some(key) = failure.frame {
                    self.presentation.pixels_failed(&key, &mut self.backend);
                    eprintln!("image restore failed; keeping last accepted view: {error}");
                }
                return Ok(());
            }
        };
        if ready.cancel_frame {
            self.presentation.cancel_preparation(&mut self.backend);
        }
        if let Some(key) = ready.frame {
            self.presentation.pixels_ready(&key);
        }
        if let Some((id, key)) = ready.capture {
            self.captures.pixels_ready(id, &key);
        }
        Ok(())
    }

    pub(crate) fn settings_pending(&self) -> bool {
        self.session
            .as_ref()
            .and_then(PlayerSession::game_settings)
            .is_some_and(|settings| {
                self.game
                    .as_ref()
                    .is_none_or(|applied| !Arc::ptr_eq(applied, settings))
            })
    }

    pub(crate) fn apply_game_settings(&mut self, host: &mut impl Host) -> Result<bool, Error> {
        let Some(settings) = self
            .session
            .as_ref()
            .and_then(PlayerSession::game_settings)
            .filter(|settings| {
                self.game
                    .as_ref()
                    .is_none_or(|old| !Arc::ptr_eq(old, settings))
            })
            .cloned()
        else {
            return Ok(false);
        };
        let layout_changed = self.game.as_ref().is_none_or(|old| {
            old.fonts != settings.fonts || old.canvas_size != settings.canvas_size
        });
        let paths = &settings.fonts;
        let assets = self.session.as_ref().unwrap().ui().assets();
        self.font_requests.retain(|path, id| {
            if paths.contains(path) {
                true
            } else {
                assets.cancel(*id);
                false
            }
        });
        if *paths != self.active_fonts {
            for path in paths {
                if !self.fonts.contains_key(path) && !self.font_requests.contains_key(path) {
                    let id = assets.request(path.clone(), crate::assets::AssetKind::Font);
                    self.font_requests.insert(path.clone(), id);
                }
            }
            if !self.font_requests.is_empty() {
                assets.maintain();
                let source = settings.source.as_deref().unwrap_or("game.configure");
                for path in paths {
                    let Some(&request) = self.font_requests.get(path) else {
                        continue;
                    };
                    let lease = assets
                        .take_ready(request)
                        .map_err(|e| format!("{source}: font {path}: {e}"))?;
                    if let Some(lease) = lease {
                        self.font_requests.remove(path);
                        self.fonts.insert(path.clone(), self.resources.len());
                        self.resources.push(lease);
                    }
                }
                if !self.font_requests.is_empty() {
                    return Ok(false);
                }
            }
            let data = paths
                .iter()
                .map(|path| {
                    let reel_ui::resources::ResourceView::Font(font) =
                        self.resources[self.fonts[path]].view()
                    else {
                        unreachable!()
                    };
                    font.clone()
                })
                .collect::<Vec<_>>();
            if !self.presentation.bridge.set_default_fonts(&data) {
                return Err(format!(
                    "{}: configured font files contain no usable font",
                    settings.source.as_deref().unwrap_or("game.configure")
                )
                .into());
            }
            self.active_fonts.clone_from(paths);
        }
        if layout_changed {
            self.pixel_recovery.cancel_frame(&assets);
            self.presentation.cancel_preparation(&mut self.backend);
            self.session.as_mut().unwrap().ui_mut()?.clear_presented();
        }
        host.set_title(&settings.title);
        if self.fit_initial_canvas
            && let Some([width, height]) = settings.canvas_size
        {
            self.fit_initial_canvas = false;
            if self.window_state.capabilities.resize {
                let applied = host.configure_window(reel_core::window::WindowConfiguration {
                    width: Some(f64::from(width)),
                    height: Some(f64::from(height)),
                    ..Default::default()
                })?;
                if applied {
                    self.resize(host, host.display_metrics())?;
                }
            }
        }

        if layout_changed && let Some(id) = self.canvas_id {
            self.presentation.bridge.edit_widget(id, |mut widget| {
                let mut canvas = widget.downcast::<reel_ui::widgets::Canvas>();
                reel_ui::widgets::Canvas::set_size(&mut canvas, settings.canvas_size);
            });
        }
        self.game = Some(settings);
        Ok(true)
    }

    /// Stop presentation and begin cancellation and resource retirement.
    pub fn close(&mut self) -> Result<(), Error> {
        self.closing = true;
        self.presentation.cancel_preparation(&mut self.backend);
        self.presentation.suspend();
        if let Some(session) = &mut self.session {
            self.pixel_recovery.cancel(&session.ui().assets());
            session.ui_mut()?.close_resources();
            session.close()?;
        }
        Ok(())
    }

    /// Whether cancellation and background retirement have completed.
    pub fn close_complete(&self) -> bool {
        self.closing
            && self
                .session
                .as_ref()
                .is_none_or(PlayerSession::close_complete)
    }

    /// Whether author code requested termination of the current player.
    pub fn exit_requested(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(PlayerSession::exit_requested)
    }

    /// Release completed submissions and wake deferred frame preparation.
    pub fn poll_completions(&mut self) -> Result<(), <B as RenderBackend>::Error> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Poll);
        self.completions.clear();
        self.presentation
            .poll(&mut self.backend, &mut self.completions, 64)?;
        #[cfg(all(feature = "performance", feature = "window"))]
        if let Some(trace) = &self.desktop_trace {
            trace.completions(&self.completions);
        }
        Ok(())
    }

    /// Prepare and present ready content. Only acceptance publishes input geometry.
    pub fn render(&mut self, host: &mut impl Host) -> Result<(), Error> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::PlayerRender);
        if !self.frame_inputs_ready() {
            return Ok(());
        }
        self.poll_completions()?;
        if self.presentation.needs_redraw() && !self.presentation.is_preparing() {
            self.animate_ui(host);
        }
        let Self {
            presentation,
            backend,
            pool,
            resources,
            session,
            recorded_projection,
            root_id,
            ..
        } = self;
        let result = presentation.render(backend, host, |bridge, id, info| {
            build_frame(
                bridge,
                *root_id,
                id,
                info,
                pool,
                resources,
                session.as_ref(),
                recorded_projection,
            )
        });
        let result = result?;
        if let Some((key, demand)) = result.pixels {
            self.request_pixels(None, key, demand)?;
        }
        if result.accepted
            && !self.settings_pending()
            && let (Some(session), Some(stamp)) = (&mut self.session, &self.recorded_projection)
        {
            session.presented(
                &self.presentation.bridge,
                stamp,
                self.presentation.accepted_frame.as_ref().unwrap(),
                host.monotonic_now().0,
            )?;
        }
        if result.accepted {
            self.publish_pointer_info()?;
        }
        if result.accepted && self.first_frame_pending {
            self.first_frame_pending = false;
            host.set_visible(true);
        }
        if result.accepted && std::env::var_os("REEL_DIAGNOSTIC_TRACE").is_some() {
            let key = self
                .presentation
                .bridge
                .accepted_key()
                .expect("accepted frame");
            eprintln!(
                "present frame={} epoch={}",
                key.frame_id().0,
                key.surface_epoch().0
            );
        }
        if let Some(error) = result.refused {
            if self.first_frame_pending
                && self.presentation.demand.model == crate::demand::Model::Blocked
            {
                return Err(error);
            }
            if self.presentation.demand.model == crate::demand::Model::Blocked
                || std::env::var_os("REEL_DIAGNOSTIC_TRACE").is_some()
            {
                eprintln!("frame deferred; keeping last accepted view: {error}");
            }
        }
        Ok(())
    }

    /// Respond to exposure, preserving an accepted frame when no model changed.
    pub fn redraw_requested(&mut self, host: &mut impl Host) -> Result<(), Error> {
        self.prepare_redraw(host)?;
        if self.closing {
            return Ok(());
        }
        self.render(host)
    }

    /// Preserve a queued settled projection or retained GPU candidate through
    /// recording. Hosts that tick before requesting redraw must not advance the
    /// VM again and invalidate that projection before it can be submitted.
    pub(crate) fn prepare_redraw(&mut self, host: &mut impl Host) -> Result<(), Error> {
        let settled_projection = self.presentation.demand.model == crate::demand::Model::Pending
            && self.session.as_ref().is_none_or(PlayerSession::frame_ready);
        if !self.presentation.is_preparing() && !settled_projection {
            self.tick(host)?;
        }
        if !self.closing {
            self.presentation.exposed();
        }
        Ok(())
    }
    /// Hidden native windows need not receive redraw events. The event loop
    /// still advances startup using VM deadlines and resource completion wakes.
    pub fn prepare_initial_frame(&mut self, host: &mut impl Host) -> Result<(), Error> {
        if self.first_frame_pending && !self.closing {
            self.render(host)?;
        }
        Ok(())
    }

    /// Dispatch translated text, keyboard or action input to the accepted view.
    pub fn text_event(&mut self, event: TextEvent) -> Result<Handled, Error> {
        self.input_hints.text(&event);
        self.publish_input_hints()?;
        let lost_focus = matches!(event, TextEvent::WindowFocusChange(false));
        let cancelling = lost_focus
            || matches!(&event, TextEvent::Navigation(e) if e.phase == reel_ui::input::ActionPhase::Cancelled)
            || matches!(
                &event,
                TextEvent::Input(
                    reel_ui::input::InputEvent::Cancel
                        | reel_ui::input::InputEvent::Disconnected(_)
                )
            );
        if self.settings_pending() && !cancelling {
            return Ok(Handled::Yes);
        }
        let before = self.session.as_ref().map(|s| s.ui().projection_stamp());
        let handled = if let Some(session) = &mut self.session {
            let handled = session.handle_text(&mut self.presentation.bridge, event)?;
            session.after_input(&mut self.presentation.bridge, self.root_id)?;
            handled
        } else {
            self.presentation.bridge.handle_text_event(event)
        };
        self.invalidate_input_changes(before);
        Ok(handled)
    }

    fn invalidate_input_changes(&mut self, before: Option<crate::session::ProjectionStamp>) {
        if before != self.session.as_ref().map(|s| s.ui().projection_stamp())
            || self.presentation.bridge.root().needs_paint()
        {
            self.invalidate();
        }
    }

    pub(crate) fn pointer_event(&mut self, kind: PointerKind) -> Result<(), Error> {
        match kind {
            PointerKind::Down(button) => self.pointer_buttons.insert(button),
            PointerKind::Up(button) => self.pointer_buttons.remove(button),
            PointerKind::Cancel => self.pointer_buttons = Default::default(),
            _ => {}
        }
        let pointer = PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: PointerType::Mouse,
        };
        let state = PointerState {
            position: self.pointer,
            buttons: self.pointer_buttons,
            ..Default::default()
        };
        let event = match kind {
            PointerKind::Move => PointerEvent::Move(PointerUpdate {
                pointer,
                current: state,
                coalesced: vec![],
                predicted: vec![],
            }),
            PointerKind::Down(button) => PointerEvent::Down(PointerButtonEvent {
                pointer,
                state,
                button: Some(button),
            }),
            PointerKind::Up(button) => PointerEvent::Up(PointerButtonEvent {
                pointer,
                state,
                button: Some(button),
            }),
            PointerKind::Cancel => PointerEvent::Cancel(pointer),
            PointerKind::Leave => PointerEvent::Leave(pointer),
            PointerKind::Scroll(delta) => PointerEvent::Scroll(PointerScrollEvent {
                pointer,
                state,
                delta,
            }),
        };
        self.dispatch_pointer(event)
    }

    /// Dispatch a translated pointer event using frozen accepted geometry.
    pub fn dispatch_pointer(&mut self, event: PointerEvent) -> Result<(), Error> {
        self.input_hints.pointer(&event);
        self.publish_input_hints()?;
        if self.settings_pending()
            && !matches!(event, PointerEvent::Cancel(_) | PointerEvent::Leave(_))
        {
            return Ok(());
        }
        let released = matches!(event, PointerEvent::Up(_));
        let before = self.session.as_ref().map(|s| s.ui().projection_stamp());
        if let Some(session) = &mut self.session {
            session.handle_pointer(&mut self.presentation.bridge, event)?;
            if released {
                session.after_input(&mut self.presentation.bridge, self.root_id)?;
            }
        } else {
            self.presentation.bridge.handle_pointer_event(event)?;
        }
        self.invalidate_input_changes(before);
        Ok(())
    }
}
/// Mouse action after the platform has updated the physical pointer position.
#[derive(Clone, Copy)]
pub enum PointerKind {
    Leave,
    Move,
    Down(PointerButton),
    Up(PointerButton),
    Cancel,
    Scroll(ScrollDelta),
}

impl<B: RenderBackend + CaptureBackend> Drop for Player<B> {
    fn drop(&mut self) {
        if let Some(session) = &mut self.session {
            self.pixel_recovery.cancel(&session.ui().assets());
            let _ = session.close();
        }
        self.presentation.suspend();
        self.backend.suspend();
    }
}
#[allow(
    clippy::too_many_arguments,
    reason = "assembly borrows disjoint frame inputs without copying resource payloads"
)]
pub(crate) fn build_frame(
    bridge: &mut PresentationBridge,
    root: WidgetId,
    id: FrameId,
    info: SurfaceInfo,
    pool: &BudgetPool,
    resources: &[reel_ui::resources::ResourceLease],
    session: Option<&PlayerSession>,
    recorded_projection: &mut Option<RecordedProjection>,
) -> Result<Option<RenderFrame>, Error> {
    let source = {
        let _recording = reel_core::profile::Scope::new(reel_core::profile::Phase::Record);
        if let Some(session) = session {
            let Some((source, receipt)) = session.record(bridge, root, id)? else {
                *recorded_projection = None;
                return Ok(None);
            };
            *recorded_projection = Some(receipt);
            source
        } else {
            *recorded_projection = None;
            bridge
                .record(id)
                .map_err(|error| format!("record: {error:?}"))?
        }
    };
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Compile);
    let images = session.map_or(&[][..], |session| session.ui().image_resources());
    let transitions = session.map_or(&[][..], PlayerSession::transition_resources);
    let snapshot = reel_ui::resources::ResourceSnapshot::capture(
        (0..resources.len() + images.len() + transitions.len()).map(|index| {
            if index < resources.len() {
                Ok(resources[index].clone())
            } else if index < resources.len() + images.len() {
                Ok(images[index - resources.len()].clone())
            } else {
                Ok(transitions[index - resources.len() - images.len()].clone())
            }
        }),
        pool,
    )
    .map_err(|e| format!("snapshot: {e:?}"))?;
    let mut build = FrameBuild::new(
        source,
        snapshot,
        FrameInfo {
            logical_size: info
                .physical_size
                .map(|size| f64::from(size) / info.scale_factor),
            physical_size: info.physical_size,
        },
        BuildLimits { max_bytes: 8 << 20 },
        pool,
    )
    .map_err(|e| format!("build: {e:?}"))?;
    if let Some(receipt) = recorded_projection {
        build
            .set_transitions(std::mem::take(&mut receipt.frame_transitions))
            .map_err(|error| format!("transition frame: {error:?}"))?;
    }
    let mut budget = StepBudget {
        max_commands: 64,
        max_units: 8192,
        max_bytes: 64 << 10,
    };
    loop {
        let progress = build.step(budget).map_err(|e| format!("compile: {e:?}"))?;
        if progress.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
    Ok(Some(build.finish().map_err(|e| format!("finish: {e:?}"))?))
}
