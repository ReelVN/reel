use crate::diagnostic::Diagnostic;
use crate::session::{ProjectionStamp, SessionHostDiagnostic};
use crate::{
    aside_session::AsideSessionUi, shell::StoryWindowOptions, window_session::WindowSession,
};
use crate::{
    demand::{Demand, Draw},
    surface::negotiate,
};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion, SurfaceEpoch},
};
use reel_render::{
    PresentOutcome,
    gpu::{DeviceGeneration, Gpu, GpuError, GpuLimits, Renderer, SurfacePass},
    presentation_notice,
};
use reel_ui::bridge::{BridgeLimits, PresentationBridge};
use reel_ui::frame::{BuildLimits, FrameBuild, FrameInfo, RenderFrame, StepBudget};
use reel_ui::resources::ResourceRegistry;
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    core::*,
    dpi::PhysicalSize,
};
use std::{
    io::Read,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{event_loop::ActiveEventLoop, window::Window};

type Error = Box<dyn std::error::Error>;
// Surface is declared first and explicitly dropped before Window on shutdown/suspend.
pub(crate) struct State {
    surface: Option<wgpu::Surface<'static>>,
    instance: wgpu::Instance,
    pub(crate) renderer: Renderer,
    output: SurfacePass,
    pub(crate) window: Arc<Window>,
    pub(crate) bridge: PresentationBridge,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    pool: BudgetPool,
    registry: ResourceRegistry,
    resources: Vec<reel_ui::resources::ResourceHandle>,
    epoch: u64,
    frame: u64,
    // One budgeted immutable accepted CPU frame, for expose after model refusal.
    accepted_frame: Option<RenderFrame>,
    pub(crate) pointer: reel_ui::dpi::PhysicalPosition<f64>,
    pub(crate) modifiers: winit::keyboard::ModifiersState,
    pub(crate) demand: Demand,
    pub(crate) root_id: WidgetId,
    pub(crate) zoom: f64,
    pub(crate) session: Option<WindowSession>,
    recorded_projection: Option<ProjectionStamp>,
    next_reveal: Option<Instant>,
    clock_origin: Instant,
    closing: bool,
}
impl State {
    pub(crate) fn new(event_loop: &ActiveEventLoop, generation: u64) -> Result<Self, Error> {
        Self::with_limits(event_loop, generation, GpuLimits::default())
    }
    pub(crate) fn new_session(
        event_loop: &ActiveEventLoop,
        generation: u64,
    ) -> Result<Self, Error> {
        Self::with_mode(
            event_loop,
            generation,
            GpuLimits::default(),
            Some(WindowSession::Diagnostic(Box::new(
                SessionHostDiagnostic::new()?,
            ))),
            None,
        )
    }
    pub(crate) fn new_story(
        event_loop: &ActiveEventLoop,
        generation: u64,
        driver: AsideSessionUi,
        options: StoryWindowOptions,
    ) -> Result<Self, Error> {
        Self::with_mode(
            event_loop,
            generation,
            GpuLimits::default(),
            Some(WindowSession::aside(driver)),
            Some(options),
        )
    }
    fn with_limits(
        event_loop: &ActiveEventLoop,
        generation: u64,
        limits: GpuLimits,
    ) -> Result<Self, Error> {
        Self::with_mode(event_loop, generation, limits, None, None)
    }
    fn with_mode(
        event_loop: &ActiveEventLoop,
        generation: u64,
        limits: GpuLimits,
        mut session: Option<WindowSession>,
        options: Option<StoryWindowOptions>,
    ) -> Result<Self, Error> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title(options.as_ref().map_or_else(
                        || {
                            if session.is_some() {
                                "Reel Rust session host diagnostic — no Aside execution"
                            } else {
                                "Reel renderer diagnostic — drag shapes; no Aside session"
                            }
                        },
                        |options| options.title.as_str(),
                    ))
                    .with_inner_size({
                        let [width, height] = options
                            .as_ref()
                            .map_or([640, 420], |options| options.logical_size);
                        winit::dpi::LogicalSize::new(width, height)
                    }),
            )?,
        );
        let instance = wgpu::Instance::default();
        if let Some(session) = &mut session {
            let window = Arc::downgrade(&window);
            session.ui_mut().set_image_waker(Arc::new(move || {
                if let Some(window) = window.upgrade() {
                    window.request_redraw();
                }
            }));
        }
        let surface = instance.create_surface(window.clone())?;
        let gpu = pollster::block_on(Gpu::request(
            &instance,
            Some(&surface),
            DeviceGeneration(generation),
            limits,
        ))?;
        let size = window.inner_size();
        let (config, output) = negotiate(&gpu, &surface, [size.width, size.height])?;
        eprintln!(
            "adapter={:?}; quality={:?}",
            gpu.adapter.get_info(),
            gpu.quality([config.width, config.height])?
        );
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: u32::MAX,
                max_bytes: 64 << 20,
            },
        );
        let font_path = if let Some(options) = &options {
            Some(options.font.clone().into_os_string())
        } else if session.is_some() {
            Some(std::env::var_os("REEL_SESSION_FONT").or_else(|| std::env::var_os("REEL_DIAGNOSTIC_FONT"))
                .ok_or("set REEL_SESSION_FONT to a TTF/OTF file for the Rust session host diagnostic")?)
        } else {
            std::env::var_os("REEL_DIAGNOSTIC_FONT")
        };
        let prepared_resources = u32::from(session.is_none()) + u32::from(font_path.is_some());
        let mut registry = ResourceRegistry::new(prepared_resources, 64 << 10)
            .map_err(|e| format!("registry: {e:?}"))?;
        let mut resources = Vec::new();
        let version = |name: &str, bytes: &[u8]| ResourceVersion {
            key: AssetKey(name.into()),
            version: ContentVersion(*blake3::hash(bytes).as_bytes()),
        };
        let image = if session.is_none() {
            let pixels = vec![
                255, 255, 255, 255, 255, 100, 0, 255, 20, 100, 255, 255, 255, 255, 255, 255,
            ];
            let bytes = OwnedBytes::new(
                pixels,
                pool.reserve(16)
                    .map_err(|e| format!("image quota: {e:?}"))?,
            )
            .map_err(|e| format!("pixels: {e:?}"))?;
            let image_handle = registry
                .register_image(
                    version("diagnostic/checker-v1", bytes.as_slice()),
                    bytes,
                    [2, 2],
                    reel_ui::peniko::ImageFormat::Rgba8,
                    reel_ui::peniko::ImageAlphaType::Alpha,
                    reel_ui::resources::ColorEncoding::Srgb,
                )
                .map_err(|e| format!("image: {e:?}"))?;
            let lease = registry
                .acquire(image_handle)
                .map_err(|e| format!("image lease: {e:?}"))?;
            let reel_ui::resources::ResourceData::Image { image, .. } = lease.data() else {
                unreachable!()
            };
            resources.push(image_handle);
            Some(image.clone())
        } else {
            None
        };
        let font = if let Some(path) = font_path {
            let mut file = std::fs::File::open(path)?;
            let length = file.metadata()?.len();
            let ticket = pool
                .reserve(length)
                .map_err(|e| format!("font quota: {e:?}"))?;
            let mut bytes = vec![0; length as usize];
            file.read_exact(&mut bytes)?;
            if file.read(&mut [0])? != 0 {
                return Err("font changed size during preparation".into());
            }
            let bytes = OwnedBytes::new(bytes, ticket).map_err(|e| format!("font bytes: {e:?}"))?;
            let handle = registry
                .register_font(version("diagnostic/font-v1", bytes.as_slice()), bytes)
                .map_err(|e| format!("font: {e:?}"))?;
            resources.push(handle);
            let lease = registry
                .acquire(handle)
                .map_err(|e| format!("font lease: {e:?}"))?;
            let reel_ui::resources::ResourceData::Font(font) = lease.data() else {
                unreachable!()
            };
            Some(font.clone())
        } else {
            eprintln!("Set REEL_DIAGNOSTIC_FONT to a TTF/OTF file to include real Parley text.");
            None
        };
        let widget = if let Some(session) = &session {
            session.ui().widget().prepare().erased()
        } else {
            Diagnostic::new(image.expect("diagnostic image prepared"), font.is_some())
                .prepare()
                .erased()
        };
        let root_id = widget.id();
        let root = RenderRoot::new(
            widget,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(size.width, size.height),
                scale_factor: window.scale_factor(),
                default_font: font,
            },
        );
        let bridge = PresentationBridge::new(
            root,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: reel_ui::app::GeometryLimits { max_bytes: 2 << 20 },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 16 << 20,
            },
        )
        .map_err(|e| format!("bridge: {e:?}"))?;
        let mut state = Self {
            surface: Some(surface),
            instance,
            renderer: Renderer::new(gpu),
            output,
            window,
            bridge,
            config,
            size: PhysicalSize::new(size.width, size.height),
            pool,
            registry,
            resources,
            epoch: 1,
            frame: 0,
            accepted_frame: None,
            pointer: Default::default(),
            modifiers: Default::default(),
            demand: Demand::default(),
            root_id,
            zoom: 1.,
            session,
            recorded_projection: None,
            next_reveal: None,
            clock_origin: Instant::now(),
            closing: false,
        };
        state.schedule_reveal();
        Ok(state)
    }
    pub(crate) fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) -> Result<(), Error> {
        self.invalidate_surface(size)?;
        if let Some(surface) = &self.surface {
            let (config, output) =
                negotiate(self.renderer.gpu(), surface, [size.width, size.height])?;
            self.config = config;
            self.output = output;
        }
        self.demand = Demand::default();
        self.schedule_reveal();
        self.window.request_redraw();
        Ok(())
    }
    fn invalidate_surface(&mut self, size: winit::dpi::PhysicalSize<u32>) -> Result<(), Error> {
        self.size = PhysicalSize::new(size.width, size.height);
        self.epoch = self.epoch.checked_add(1).ok_or("surface epoch exhausted")?;
        self.bridge.replace_surface(SurfaceEpoch(self.epoch));
        self.bridge
            .handle_window_event(reel_ui::core::WindowEvent::Rescale(
                self.window.scale_factor(),
            ));
        self.bridge
            .handle_window_event(reel_ui::core::WindowEvent::Resize(self.size));
        // Invalidates candidates even if fresh negotiation subsequently fails.
        self.demand = Demand::idle();
        self.next_reveal = None;
        self.accepted_frame = None;
        if let Some(session) = &mut self.session {
            session.ui_mut().clear_presented();
        }
        Ok(())
    }
    pub(crate) fn invalidate(&mut self) {
        self.demand.invalidate();
        if self.demand.has_work() {
            self.window.request_redraw();
        }
    }
    pub(crate) fn is_diagnostic(&self) -> bool {
        !matches!(self.session, Some(WindowSession::Aside(_)))
    }
    pub(crate) fn story_deadline(&self) -> Option<Instant> {
        self.session
            .as_ref()
            .filter(|session| session.needs_tick())
            .map(|session| Instant::now() + session.tick_interval())
    }
    pub(crate) fn tick_story(&mut self) -> Result<(), Error> {
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        if !session.needs_tick() && !session.ui().images_need_poll() {
            return Ok(());
        }
        let origin = self.clock_origin;
        let now = || u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let budget = reel_engine::aside::SliceBudget {
            deadline_ns: Some(now().saturating_add(2_000_000)),
            ..Default::default()
        };
        let before = session.ui().projection_stamp();
        session.tick(budget, now)?;
        if !self.closing {
            session.sync(&mut self.bridge, self.root_id)?;
            if before != session.ui().projection_stamp() {
                self.schedule_reveal();
                self.invalidate();
            }
        }
        Ok(())
    }
    pub(crate) fn close(&mut self) -> Result<(), Error> {
        self.closing = true;
        self.next_reveal = None;
        self.demand = Demand::idle();
        self.bridge.clear();
        if let Some(session) = &mut self.session {
            session.ui_mut().close_images();
            session.close()?;
        }
        Ok(())
    }
    pub(crate) fn close_complete(&self) -> bool {
        self.closing
            && self
                .session
                .as_ref()
                .is_none_or(WindowSession::close_complete)
    }
    pub(crate) fn poll_completions(&mut self) -> Result<(), GpuError> {
        let completed = self.renderer.poll()?;
        self.demand.completed(!completed.is_empty());
        Ok(())
    }
    pub(crate) fn render(&mut self) -> Result<(), Error> {
        self.poll_completions()?;
        let Some(draw) = self.demand.draw() else {
            return Ok(());
        };
        if self.size.width == 0 || self.size.height == 0 {
            self.demand = Demand::idle();
            return Ok(());
        }
        let Some(surface) = &self.surface else {
            return Ok(());
        };
        let surface_texture = match surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.resize(self.window.inner_size())?;
                return Ok(());
            }
            Err(wgpu::SurfaceError::Timeout) => {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        let frame = match draw {
            Draw::Current => self.build_frame()?,
            Draw::Accepted => {
                let Some(frame) = &self.accepted_frame else {
                    self.demand.presented(draw);
                    return Ok(());
                };
                // A frozen repaint cannot install stale or unaccepted geometry.
                if !self.bridge.accepted_key().is_some_and(|key| {
                    key.frame_id() == frame.frame_id()
                        && key.surface_epoch() == frame.surface_epoch()
                }) {
                    self.accepted_frame = None;
                    if let Some(session) = &mut self.session {
                        session.ui_mut().clear_presented();
                    }
                    self.demand.presented(draw);
                    return Ok(());
                }
                frame.clone()
            }
        };
        let mut prepared = match self.renderer.prepare(&frame) {
            Ok(p) => p,
            Err(error) if error.is_retryable() => {
                self.demand.refused(draw, &error);
                return Ok(());
            }
            Err(
                error @ (GpuError::Budget { .. }
                | GpuError::Unsupported { .. }
                | GpuError::Geometry { .. }
                | GpuError::InvalidRecord { .. }),
            ) => {
                eprintln!("frame refused; keeping last accepted view: {error}");
                self.demand.refused(draw, &error);
                if draw == Draw::Current && self.accepted_frame.is_some() {
                    self.demand.exposed();
                }
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        self.output.encode(
            &self.renderer.gpu().device,
            &mut prepared,
            &surface_texture.texture.create_view(&Default::default()),
        );
        self.renderer.submit(prepared)?;
        self.window.pre_present_notify();
        surface_texture.present();
        let frame_id = frame.frame_id();
        let notice_result = if draw == Draw::Current {
            let notice = presentation_notice(
                &frame,
                SurfaceEpoch(self.epoch),
                Ok(PresentOutcome::PresentAccepted(frame.frame_id())),
            );
            let result = self.bridge.resolve(notice);
            if result == reel_ui::bridge::NoticeResult::Accepted {
                if let (Some(session), Some(stamp)) = (&mut self.session, self.recorded_projection)
                {
                    session.ui_mut().presented(stamp);
                }
                self.accepted_frame = Some(frame);
            }
            Some(result)
        } else {
            // This is the exact already accepted frame; repaint has no new
            // candidate and cannot accept or change the latest model's geometry.
            None
        };
        if std::env::var_os("REEL_DIAGNOSTIC_TRACE").is_some() {
            eprintln!(
                "present frame={} epoch={} draw={draw:?} notice={notice_result:?}",
                frame_id.0, self.epoch
            );
        }
        self.demand.presented(draw);
        Ok(())
    }
    pub(crate) fn redraw_requested(&mut self) -> Result<(), Error> {
        self.tick_story()?;
        self.demand.exposed();
        self.render()
    }
    fn build_frame(&mut self) -> Result<RenderFrame, Error> {
        self.frame = self
            .frame
            .checked_add(1)
            .ok_or("frame identity exhausted")?;
        let source = self
            .bridge
            .record(FrameId(self.frame))
            .map_err(|e| format!("record: {e:?}"))?;
        // Recording lays out the viewport and may clamp its scroll offset.
        self.recorded_projection = self
            .session
            .as_ref()
            .map(|session| session.ui().projection_stamp());
        let images = self
            .session
            .as_ref()
            .map_or(&[][..], |session| session.ui().image_resources());
        let snapshot = reel_ui::resources::ResourceSnapshot::capture(
            (0..self.resources.len() + images.len()).map(|index| {
                if index < self.resources.len() {
                    self.registry.acquire(self.resources[index])
                } else {
                    Ok(images[index - self.resources.len()].clone())
                }
            }),
            &self.pool,
        )
        .map_err(|e| format!("snapshot: {e:?}"))?;
        let scale = self.window.scale_factor();
        let mut build = FrameBuild::new(
            source,
            snapshot,
            FrameInfo {
                logical_size: [
                    f64::from(self.size.width) / scale,
                    f64::from(self.size.height) / scale,
                ],
                physical_size: [self.size.width, self.size.height],
            },
            BuildLimits { max_bytes: 8 << 20 },
            &self.pool,
        )
        .map_err(|e| format!("build: {e:?}"))?;
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
        Ok(build.finish().map_err(|e| format!("finish: {e:?}"))?)
    }
    pub(crate) fn recover_device(&mut self) -> Result<(), Error> {
        self.pointer_event(PointerKind::Cancel)?;
        let size = self.window.inner_size();
        self.invalidate_surface(size)?;
        let generation = self
            .renderer
            .gpu()
            .generation()
            .0
            .checked_add(1)
            .ok_or("device generation exhausted")?;
        let gpu = pollster::block_on(Gpu::request(
            &self.instance,
            self.surface.as_ref(),
            DeviceGeneration(generation),
            self.renderer.gpu().limits(),
        ))?;
        let surface = self
            .surface
            .as_ref()
            .ok_or("device recovery requires a resumed surface")?;
        let (config, output) = negotiate(&gpu, surface, [size.width, size.height])?;
        self.output = output;
        self.config = config;
        self.renderer = Renderer::new(gpu);
        self.demand = Demand::default();
        self.schedule_reveal();
        self.window.request_redraw();
        eprintln!(
            "device recovered generation={generation}, epoch={}",
            self.epoch
        );
        Ok(())
    }
    pub(crate) fn suspend(&mut self) {
        let _ = self.pointer_event(PointerKind::Cancel);
        self.bridge.clear();
        let _ = self.renderer.gpu().device.poll(wgpu::PollType::Wait);
        let _ = self.renderer.poll();
        self.surface = None;
        self.demand = Demand::idle();
        self.next_reveal = None;
        self.accepted_frame = None;
        if let Some(session) = &mut self.session {
            session.ui_mut().clear_presented();
        }
    }
    pub(crate) fn is_suspended(&self) -> bool {
        self.surface.is_none()
    }
    pub(crate) fn resume(&mut self) -> Result<(), Error> {
        self.surface = Some(self.instance.create_surface(self.window.clone())?);
        if self.renderer.gpu().check().is_err() {
            self.recover_device()
        } else {
            self.resize(self.window.inner_size())
        }
    }
    pub(crate) fn text_event(&mut self, event: TextEvent) -> Result<(), Error> {
        if let Some(session) = &mut self.session {
            session.handle_text(&mut self.bridge, event)?;
            session.after_input(&mut self.bridge, self.root_id)?;
        } else {
            self.bridge.handle_text_event(event);
        }
        self.schedule_reveal();
        self.invalidate();
        Ok(())
    }
    pub(crate) fn pointer_event(&mut self, kind: PointerKind) -> Result<(), Error> {
        let pointer = PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: PointerType::Mouse,
        };
        let state = PointerState {
            position: self.pointer,
            ..Default::default()
        };
        let event = match kind {
            PointerKind::Move => PointerEvent::Move(PointerUpdate {
                pointer,
                current: state,
                coalesced: vec![],
                predicted: vec![],
            }),
            PointerKind::Down => PointerEvent::Down(PointerButtonEvent {
                pointer,
                state,
                button: Some(PointerButton::Primary),
            }),
            PointerKind::Up => PointerEvent::Up(PointerButtonEvent {
                pointer,
                state,
                button: Some(PointerButton::Primary),
            }),
            PointerKind::Cancel => PointerEvent::Cancel(pointer),
            PointerKind::Scroll(delta) => PointerEvent::Scroll(PointerScrollEvent {
                pointer,
                state,
                delta,
            }),
        };
        if std::env::var_os("REEL_DIAGNOSTIC_TRACE").is_some() {
            eprintln!(
                "pointer accepted={:?} position={:?}",
                self.bridge
                    .accepted_key()
                    .map(|key| (key.frame_id(), key.surface_epoch())),
                self.pointer
            );
        }
        if let Some(session) = &mut self.session {
            session.handle_pointer(&mut self.bridge, event)?;
            if matches!(kind, PointerKind::Up) {
                session.after_input(&mut self.bridge, self.root_id)?;
            }
        } else {
            self.bridge.handle_pointer_event(event);
        }
        if matches!(kind, PointerKind::Up) {
            self.schedule_reveal();
        }
        self.invalidate();
        Ok(())
    }
    fn schedule_reveal(&mut self) {
        if self.surface.is_none() || self.size.width == 0 || self.size.height == 0 {
            self.next_reveal = None;
            return;
        }
        self.next_reveal = self.session.as_ref().and_then(|session| {
            if session.ui().images_pending() {
                return None;
            }
            match session.ui().session().view().interaction {
                Some(reel_engine::InteractionView::Dialogue(dialogue))
                    if dialogue.lines().iter().any(|line| {
                        let reveal = line.reveal();
                        reveal.revealed_count < reveal.total_count
                    }) =>
                {
                    Some(Instant::now() + Duration::from_millis(35))
                }
                _ => None,
            }
        });
    }
    pub(crate) fn reveal_deadline(&self) -> Option<Instant> {
        self.next_reveal
    }
    pub(crate) fn tick_reveal(&mut self) -> Result<(), Error> {
        if self
            .next_reveal
            .is_none_or(|deadline| Instant::now() < deadline)
        {
            return Ok(());
        }
        if let Some(session) = &mut self.session {
            if session.ui().gesture_active() || session.ui().images_pending() {
                // Preserve the presented content bound at Down until Up/Cancel.
                self.schedule_reveal();
                return Ok(());
            }
            let interaction = session
                .ui()
                .session()
                .view()
                .interaction
                .map(|interaction| interaction.id());
            if let Some(interaction) = interaction {
                let owner = session.ui().session().root_owner();
                if session
                    .ui_mut()
                    .session_mut()
                    .reveal_more(owner, interaction, 1)?
                {
                    session.sync(&mut self.bridge, self.root_id)?;
                    self.invalidate();
                }
            }
        }
        // Advance at most one grapheme per timer wake; no unbounded catch-up after a sleep.
        self.schedule_reveal();
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub(crate) enum PointerKind {
    Move,
    Down,
    Up,
    Cancel,
    Scroll(ScrollDelta),
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(session) = &mut self.session {
            let _ = session.close();
        }
        self.bridge.clear();
        let _ = self.renderer.gpu().device.poll(wgpu::PollType::Wait);
        let _ = self.renderer.poll();
        self.surface = None;
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::demand::Model;
    use reel_ui::app::InputGeometrySource;
    use winit::{
        application::ApplicationHandler, event_loop::EventLoop,
        platform::x11::EventLoopBuilderExtX11,
    };

    fn accepted_transform(state: &State) -> reel_ui::kurbo::Affine {
        let InputGeometrySource::Presented(Some(view)) =
            state.bridge.root().input_geometry_source()
        else {
            panic!("input requires an actually accepted surface frame");
        };
        view.get(state.root_id).unwrap().transform
    }
    fn exercise(event_loop: &ActiveEventLoop) {
        // Derive pressure from the current profile rather than assuming 4x support.
        let mut probe = State::new(event_loop, 1).unwrap();
        let probe_frame = probe.build_frame().unwrap();
        let prepared = probe.renderer.prepare(&probe_frame).unwrap();
        let single_targets = probe.renderer.usage().targets;
        drop(prepared);
        drop(probe_frame);
        drop(probe);
        let mut state = State::with_limits(
            event_loop,
            1,
            GpuLimits {
                target_bytes: single_targets * 3 / 2,
                ..Default::default()
            },
        )
        .unwrap();
        state.render().unwrap();
        assert!(state.bridge.accepted_key().is_some());
        state
            .renderer
            .gpu()
            .device
            .poll(wgpu::PollType::Wait)
            .unwrap();
        state.poll_completions().unwrap();
        let before_expose = state.frame;
        state.redraw_requested().unwrap();
        assert!(state.frame > before_expose);
        let accepted = state.bridge.accepted_key().unwrap().frame_id();
        assert_eq!(state.demand.model, Model::Idle);
        state
            .renderer
            .gpu()
            .device
            .poll(wgpu::PollType::Wait)
            .unwrap();
        state.poll_completions().unwrap();
        // Successful idle never creates a timer-driven model preparation.
        let after_expose = state.frame;
        state.render().unwrap();
        assert_eq!(state.frame, after_expose);
        let held_frame = state.build_frame().unwrap();
        let held = state.renderer.prepare(&held_frame).unwrap();
        eprintln!(
            "native pressure fixture targets={}",
            state.renderer.usage().targets
        );
        state.bridge.edit_widget(state.root_id, |mut widget| {
            widget.set_transform(reel_ui::kurbo::Affine::scale(0.7))
        });
        state.invalidate();
        state.render().unwrap();
        assert_eq!(state.demand.model, Model::Waiting);
        assert_eq!(state.bridge.accepted_key().unwrap().frame_id(), accepted);
        assert_eq!(accepted_transform(&state), reel_ui::kurbo::Affine::IDENTITY);
        let refused_frame = state.frame;
        // No completion and no new input: OS redraws cannot repeatedly compile.
        for _ in 0..10 {
            state.render().unwrap();
        }
        assert_eq!(state.frame, refused_frame);
        let id = state.renderer.submit(held).unwrap(); // offscreen: never accepts its geometry
        state
            .renderer
            .gpu()
            .device
            .poll(wgpu::PollType::Wait)
            .unwrap();
        assert_eq!(state.bridge.accepted_key().unwrap().frame_id(), accepted);
        assert!(state.renderer.usage().targets > 0);
        state.render().unwrap(); // observes completion and retries latest state, without input
        assert_eq!(state.demand.model, Model::Idle);
        assert!(state.bridge.accepted_key().unwrap().frame_id() > accepted);
        assert_eq!(
            accepted_transform(&state),
            reel_ui::kurbo::Affine::scale(0.7)
        );
        state
            .renderer
            .gpu()
            .device
            .poll(wgpu::PollType::Wait)
            .unwrap();
        state.poll_completions().unwrap();
        // R01 can freeze this finite f64 transform; GPU conversion cannot represent it.
        state.bridge.edit_widget(state.root_id, |mut widget| {
            widget.set_transform(reel_ui::kurbo::Affine::scale(1e100))
        });
        state.invalidate();
        state.redraw_requested().unwrap();
        assert_eq!(state.demand.model, Model::Blocked);
        let failed_count = state.frame;
        let accepted = state.bridge.accepted_key().unwrap().frame_id();
        state.redraw_requested().unwrap(); // repaint exact accepted frame; no bad-model compile
        assert!(
            state.renderer.usage().targets > 0,
            "expose must actually submit the frozen accepted frame"
        );
        assert_eq!(state.frame, failed_count);
        assert_eq!(state.bridge.accepted_key().unwrap().frame_id(), accepted);
        assert_eq!(
            accepted_transform(&state),
            reel_ui::kurbo::Affine::scale(0.7)
        );
        state
            .renderer
            .gpu()
            .device
            .poll(wgpu::PollType::Wait)
            .unwrap();
        state.poll_completions().unwrap();
        for _ in 0..10 {
            state.render().unwrap();
        }
        assert_eq!(state.frame, failed_count);
        assert_eq!(state.demand.model, Model::Blocked);
        state.bridge.edit_widget(state.root_id, |mut widget| {
            widget.set_transform(reel_ui::kurbo::Affine::scale(0.7))
        });
        state.invalidate();
        state.redraw_requested().unwrap();
        assert_eq!(state.demand.model, Model::Idle);
        assert!(!state.renderer.poll().unwrap().contains(&id)); // completion is delivered once

        state
            .renderer
            .gpu()
            .device
            .poll(wgpu::PollType::Wait)
            .unwrap();
        state.poll_completions().unwrap();
        let old_frame = state.build_frame().unwrap();
        let old = state.renderer.prepare(&old_frame).unwrap();
        let old_notice = presentation_notice(
            &old_frame,
            SurfaceEpoch(state.epoch),
            Ok(PresentOutcome::PresentAccepted(old_frame.frame_id())),
        );
        let old_epoch = state.epoch;
        // Stale configuration must never survive recovery negotiation.
        state.config.format = wgpu::TextureFormat::Rgba8Unorm;
        state.renderer.gpu().device.destroy();
        let _ = state.renderer.gpu().device.poll(wgpu::PollType::Wait);
        state.recover_device().unwrap();
        assert_eq!(state.renderer.gpu().generation(), DeviceGeneration(2));
        assert_eq!(state.epoch, old_epoch + 1);
        assert!(state.config.format.is_srgb());
        assert!(state.bridge.accepted_key().is_none());
        assert!(state.accepted_frame.is_none());
        assert_eq!(
            state.bridge.resolve(old_notice),
            reel_ui::bridge::NoticeResult::StaleOrForeign
        );
        assert!(matches!(
            state.renderer.submit(old),
            Err(GpuError::DeviceLost)
        ));
        state.render().unwrap();
        assert_eq!(
            accepted_transform(&state),
            reel_ui::kurbo::Affine::scale(0.7)
        );
        let generation = state.renderer.gpu().generation();
        let epoch = state.epoch;
        state.suspend();
        assert!(state.is_suspended());
        assert!(state.bridge.accepted_key().is_none());
        assert!(state.accepted_frame.is_none());
        state.resume().unwrap();
        assert_eq!(state.renderer.gpu().generation(), generation);
        assert_eq!(state.epoch, epoch + 1);
        state.render().unwrap();
        assert!(state.bridge.accepted_key().is_some());
        state.resize(winit::dpi::PhysicalSize::new(0, 0)).unwrap();
        let frame = state.frame;
        state.render().unwrap();
        assert_eq!(state.frame, frame);
        assert!(state.bridge.accepted_key().is_none());
        assert!(state.accepted_frame.is_none());
        drop(state);

        // Permanent refusals stop preparation even when unrelated GPU work completes.
        for limits in [
            GpuLimits {
                target_bytes: single_targets * 3 / 4,
                ..Default::default()
            },
            GpuLimits {
                resource_bytes: 1,
                ..Default::default()
            },
        ] {
            let mut refused = State::with_limits(event_loop, 1, limits).unwrap();
            refused.render().unwrap();
            assert_eq!(refused.demand.model, Model::Blocked);
            assert!(refused.bridge.accepted_key().is_none());
            let count = refused.frame;
            for _ in 0..10 {
                refused.redraw_requested().unwrap();
            }
            assert_eq!(refused.frame, count);
            assert_eq!(refused.renderer.usage().targets, 0);
        }
        eprintln!(
            "native retry, accepted geometry, permanent refusal, generation/epoch recovery and shutdown passed"
        );
    }
    fn session_reveal(state: &State) -> usize {
        match state
            .session
            .as_ref()
            .unwrap()
            .ui()
            .session()
            .view()
            .interaction
        {
            Some(reel_engine::InteractionView::Dialogue(dialogue)) => {
                dialogue.lines()[0].reveal().revealed_count
            }
            _ => 0,
        }
    }
    fn session_click(state: &mut State, x: f64, y: f64) {
        state.pointer = reel_ui::dpi::PhysicalPosition::new(x, y);
        for kind in [PointerKind::Move, PointerKind::Down, PointerKind::Up] {
            state.pointer_event(kind).unwrap();
        }
    }
    fn dark_dialogue_pixels(state: &mut State) -> usize {
        let frame = state.build_frame().unwrap();
        let prepared = state.renderer.prepare(&frame).unwrap();
        let pixels = state.renderer.readback(prepared).unwrap();
        let width = state.size.width as usize;
        (280..365)
            .flat_map(|y| (45..585).map(move |x| y * width + x))
            .filter(|index| {
                let p = pixels[*index];
                p[0] < 0.3 && p[1] < 0.3 && p[2] < 0.3 && p[3] > 0.9
            })
            .count()
    }
    fn exercise_session(event_loop: &ActiveEventLoop) {
        let mut state = State::new_session(event_loop, 1).unwrap();
        let identity = state.session.as_ref().unwrap().ui().session().identity();
        session_click(&mut state, 100., 300.);
        assert_eq!(
            session_reveal(&state),
            0,
            "no input before first actual presentation"
        );
        state.render().unwrap();
        assert!(state.bridge.accepted_key().is_some());
        let hidden_pixels = dark_dialogue_pixels(&mut state);
        assert_eq!(
            hidden_pixels, 0,
            "hidden full-text layout paints no dialogue glyph pixels"
        );
        state.next_reveal = Some(Instant::now());
        state.tick_reveal().unwrap();
        assert_eq!(session_reveal(&state), 1);
        assert_eq!(
            state
                .session
                .as_ref()
                .unwrap()
                .ui()
                .session()
                .view()
                .history
                .len(),
            1
        );
        state.resize(state.window.inner_size()).unwrap();
        assert!(state.reveal_deadline().is_some());
        assert_eq!(session_reveal(&state), 1);
        assert!(state.bridge.accepted_key().is_none());
        state.render().unwrap();
        state.renderer.gpu().device.destroy();
        let _ = state.renderer.gpu().device.poll(wgpu::PollType::Wait);
        state.recover_device().unwrap();
        assert_eq!(session_reveal(&state), 1);
        assert!(state.reveal_deadline().is_some());
        assert!(state.bridge.accepted_key().is_none());
        state.render().unwrap();
        state.suspend();
        assert!(state.reveal_deadline().is_none());
        assert!(state.bridge.accepted_key().is_none());
        state.resume().unwrap();
        assert!(state.reveal_deadline().is_some());
        assert_eq!(session_reveal(&state), 1);
        assert_eq!(
            state.session.as_ref().unwrap().ui().session().identity(),
            identity
        );
        state.render().unwrap();
        // Holding a real pointer gesture freezes the timed reveal revision.
        state.pointer = reel_ui::dpi::PhysicalPosition::new(100., 300.);
        state.pointer_event(PointerKind::Move).unwrap();
        state.pointer_event(PointerKind::Down).unwrap();
        state.next_reveal = Some(Instant::now());
        state.tick_reveal().unwrap();
        assert_eq!(session_reveal(&state), 1);
        state.pointer_event(PointerKind::Up).unwrap();
        assert!(session_reveal(&state) > 1);
        assert!(state.reveal_deadline().is_none());
        state.render().unwrap();
        let shown_pixels = dark_dialogue_pixels(&mut state);
        assert!(
            shown_pixels > 100,
            "real Parley glyphs must produce software GPU pixels"
        );
        eprintln!(
            "software GPU dialogue glyph coverage: hidden={hidden_pixels}, full={shown_pixels}"
        );
        session_click(&mut state, 100., 300.); // drain first completion, request second line
        assert_eq!(state.session.as_ref().unwrap().diagnostic_step(), 1);
        assert_eq!(session_reveal(&state), 0);
        session_click(&mut state, 100., 300.); // still accepted old first-line frame
        assert_eq!(session_reveal(&state), 0);
        state.render().unwrap();
        session_click(&mut state, 100., 300.); // reveal second line
        state.render().unwrap();
        session_click(&mut state, 100., 300.); // install choice
        assert_eq!(state.session.as_ref().unwrap().diagnostic_step(), 2);
        state.render().unwrap();
        session_click(&mut state, 100., 320.); // actual second Button
        assert_eq!(state.session.as_ref().unwrap().diagnostic_step(), 3);
        let session = state.session.as_ref().unwrap().ui().session();
        assert_eq!(session.status(), reel_engine::SessionStatus::Ended);
        assert_eq!(session.view().history.len(), 2);
        assert!(session.view().interaction.is_none());
        state.render().unwrap();
        drop(state);
        let mut cancelled = State::new_session(event_loop, 1).unwrap();
        cancelled.render().unwrap();
        assert!(cancelled.session.as_mut().unwrap().ui_mut().cancel());
        assert!(!cancelled.session.as_mut().unwrap().ui_mut().cancel());
        cancelled
            .session
            .as_mut()
            .unwrap()
            .after_input(&mut cancelled.bridge, cancelled.root_id)
            .unwrap();
        assert_eq!(
            cancelled.session.as_ref().unwrap().ui().session().status(),
            reel_engine::SessionStatus::Cancelled
        );
        assert_eq!(
            cancelled
                .session
                .as_ref()
                .unwrap()
                .ui()
                .session()
                .view()
                .history
                .len(),
            1
        );
        eprintln!(
            "real Rust host session/window: presented input, stable reveal timer, choice, end/cancel, resize/device/suspend preservation passed; no Aside execution"
        );
    }
    #[test]
    #[ignore = "requires Linux X11 display, a graphics adapter and REEL_SESSION_FONT"]
    fn actual_session_window_host_contract() {
        struct Harness;
        impl ApplicationHandler for Harness {
            fn resumed(&mut self, event_loop: &ActiveEventLoop) {
                exercise_session(event_loop);
                event_loop.exit();
            }
            fn window_event(
                &mut self,
                _: &ActiveEventLoop,
                _: winit::window::WindowId,
                _: winit::event::WindowEvent,
            ) {
            }
        }
        let event_loop = EventLoop::builder()
            .with_x11()
            .with_any_thread(true)
            .build()
            .unwrap();
        event_loop.run_app(&mut Harness).unwrap();
    }
    #[test]
    #[ignore = "requires Linux X11 display and actual graphics adapter"]
    fn actual_window_retry_and_recovery_contract() {
        struct Harness;
        impl ApplicationHandler for Harness {
            fn resumed(&mut self, event_loop: &ActiveEventLoop) {
                exercise(event_loop);
                event_loop.exit();
            }
            fn window_event(
                &mut self,
                _: &ActiveEventLoop,
                _: winit::window::WindowId,
                _: winit::event::WindowEvent,
            ) {
            }
        }
        let event_loop = EventLoop::builder()
            .with_x11()
            .with_any_thread(true)
            .build()
            .unwrap();
        event_loop.run_app(&mut Harness).unwrap();
    }
}
