use crate::diagnostic::Diagnostic;
#[path = "window_control.rs"]
mod control;
use crate::platform::WindowControl;
use crate::session::{ProjectionStamp, SessionHostDiagnostic};
use crate::{
    aside_session::AsideSessionUi, shell::StoryWindowOptions, window_session::WindowSession,
};
use crate::{
    platform::{PlatformHost, winit::WinitHost},
    presentation::Presentation,
};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    clock::Clock,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion, SurfaceEpoch},
};
use reel_render::{
    AlphaMode, RenderBackend, RenderEvent, SurfaceInfo,
    gpu::{DeviceGeneration, Gpu, GpuError, GpuLimits, WgpuBackend},
};
use reel_ui::bridge::{BridgeLimits, PresentationBridge};
use reel_ui::core::pointer::PointerButtons;
use reel_ui::frame::{BuildLimits, FrameBuild, FrameInfo, RenderFrame, StepBudget};
use reel_ui::resources::ResourceRegistry;
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    core::*,
    dpi::PhysicalSize,
};
use std::{sync::Arc, time::Instant};
use winit::{event_loop::ActiveEventLoop, window::Window};

type Error = Box<dyn std::error::Error>;
// Surface is declared first and explicitly dropped before Window on shutdown/suspend.
pub(crate) struct State {
    pub(crate) backend: WgpuBackend,
    instance: wgpu::Instance,
    pub(crate) window: Arc<Window>,
    pub(crate) host: WinitHost,
    pub(crate) presentation: Presentation,
    size: PhysicalSize<u32>,
    pool: BudgetPool,
    resources: Vec<reel_ui::resources::ResourceLease>,
    fonts: std::collections::BTreeMap<String, usize>,
    active_fonts: Vec<String>,
    font_requests: std::collections::BTreeMap<String, crate::assets::LoadId>,
    canvas_id: Option<WidgetId>,
    game: Option<Arc<reel_engine::aside::GameSettings>>,
    fit_initial_canvas: bool,
    epoch: u64,
    pub(crate) pointer: reel_ui::dpi::PhysicalPosition<f64>,
    pub(crate) modifiers: winit::keyboard::ModifiersState,
    pointer_buttons: PointerButtons,
    touch: crate::platform::winit::TouchInput,
    input: reel_ui::input::NavigationInput,
    input_origin: Instant,
    input_focused: bool,
    input_events: Vec<reel_ui::input::NavigationEvent>,
    input_pending: bool,
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    gamepads: Option<crate::platform::gamepad::GamepadSource>,
    pub(crate) root_id: WidgetId,
    pub(crate) zoom: f64,
    pub(crate) session: Option<WindowSession>,
    recorded_projection: Option<ProjectionStamp>,
    closing: bool,
    first_frame_pending: bool,
    completions: Vec<RenderEvent<GpuError>>,
    captures: crate::capture::CaptureService<reel_render::gpu::GpuCapture>,
    window_state: Arc<reel_core::window::WindowInfo>,
    window_request: Option<reel_engine::aside::window::WindowRequestId>,
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
        let root = if let Some(root) = &options.save_directory {
            root.clone()
        } else {
            #[cfg(target_os = "windows")]
            let data = std::env::var_os("LOCALAPPDATA")
                .map(std::path::PathBuf::from)
                .ok_or("LOCALAPPDATA is unavailable; set StoryWindowOptions.save_directory")?;
            #[cfg(target_os = "macos")]
            let data = std::env::var_os("HOME")
                .map(|home| std::path::PathBuf::from(home).join("Library/Application Support"))
                .ok_or("HOME is unavailable; set StoryWindowOptions.save_directory")?;
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME")
                        .map(|home| std::path::PathBuf::from(home).join(".local/share"))
                })
                .ok_or(
                    "user data directory is unavailable; set StoryWindowOptions.save_directory",
                )?;
            crate::saves::game_save_directory(
                &data,
                &options.assets.root.canonicalize()?.to_string_lossy(),
            )
        };
        let save_bytes = options.save_bytes;
        let assets = driver.ui().assets();
        let audio = options.audio.clone();
        let mut state = Self::with_mode(
            event_loop,
            generation,
            GpuLimits::default(),
            Some(WindowSession::aside(driver)),
            Some(options),
        )?;
        let window = state.window.clone();
        let wake = reel_core::wake::WakeSignal::new(move || window.request_redraw());
        let service = crate::saves::SaveService::new(
            Arc::new(crate::saves::DirectorySaves::new(root, save_bytes)),
            reel_core::budget::QueueLimits {
                max_requests: 1,
                max_bytes: save_bytes,
            },
            wake.clone(),
        );
        if let Some(WindowSession::Aside { story: runtime, .. }) = &mut state.session {
            runtime.set_frame_capture_enabled(true);
            runtime.set_window_control_enabled(true);
            runtime.set_saves(crate::saves::RuntimeSaves::new(service, save_bytes));
            runtime.set_audio_factory(move || {
                Box::new(crate::audio::AudioPlayer::desktop(
                    assets.clone(),
                    audio.clone(),
                    wake.clone(),
                ))
            });
        }
        Ok(state)
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
        let first_frame_pending = matches!(session, Some(WindowSession::Aside { .. }));
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_visible(!first_frame_pending)
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
                        let [width, height] = options.as_ref().map_or([640, 420], |options| {
                            options
                                .logical_size
                                .or(options.canvas_size)
                                .unwrap_or([1280, 720])
                        });
                        winit::dpi::LogicalSize::new(width, height)
                    }),
            )?,
        );
        let instance = wgpu::Instance::default();
        if let Some(session) = &mut session {
            let window = Arc::downgrade(&window);
            session.ui_mut()?.set_resource_waker(Arc::new(move || {
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
        let info = SurfaceInfo {
            physical_size: [size.width, size.height],
            scale_factor: window.scale_factor(),
            surface_epoch: SurfaceEpoch(1),
            color_encoding: reel_ui::resources::ColorEncoding::Srgb,
            alpha_mode: AlphaMode::Opaque,
        };
        eprintln!(
            "adapter={:?}; quality={:?}",
            gpu.adapter.get_info(),
            gpu.quality([size.width.max(1), size.height.max(1)])?
        );
        let mut backend = WgpuBackend::new(gpu, surface);
        backend.configure(info)?;
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: u32::MAX,
                max_bytes: 64 << 20,
            },
        );
        let font_path = if options.is_some() {
            None
        } else if session.is_some() {
            Some(std::path::PathBuf::from(std::env::var_os("REEL_SESSION_FONT").or_else(|| std::env::var_os("REEL_DIAGNOSTIC_FONT"))
                .ok_or("set REEL_SESSION_FONT to a TTF/OTF file for the Rust session host diagnostic")?))
        } else {
            std::env::var_os("REEL_DIAGNOSTIC_FONT").map(std::path::PathBuf::from)
        };
        let prepared_resources = u32::from(session.is_none());
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
            resources.push(lease.clone());
            Some(image.clone())
        } else {
            None
        };
        let mut fonts = std::collections::BTreeMap::new();
        let font = if let Some(path) = &font_path {
            let lease = crate::fonts::load(path, &pool)?;
            let reel_ui::resources::ResourceData::Font(font) = lease.data() else {
                unreachable!()
            };
            let font = font.clone();
            fonts.insert(path.to_string_lossy().into_owned(), resources.len());
            resources.push(lease);
            Some(font)
        } else {
            if options.is_none() {
                eprintln!(
                    "Set REEL_DIAGNOSTIC_FONT to a TTF/OTF file to include real Parley text."
                );
            }
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
        let (widget, canvas_id) = if let Some(options) = &options {
            let canvas = if let Some(size) = options.canvas_size {
                reel_ui::widgets::Canvas::new(widget, size)
            } else {
                reel_ui::widgets::Canvas::viewport(widget)
            }
            .prepare();
            let id = canvas.id();
            (canvas.erased(), Some(id))
        } else {
            (widget, None)
        };
        let host = WinitHost::new(window.clone());
        let root = RenderRoot::new(
            widget,
            host.ui_signal_handler(),
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
        let game = session
            .as_ref()
            .and_then(WindowSession::game_settings)
            .cloned();
        let mut state = Self {
            captures: {
                let window = window.clone();
                crate::capture::CaptureService::new(
                    options.as_ref().map_or_else(
                        || crate::assets::AssetOptions::default().input_bytes,
                        |options| options.assets.input_bytes,
                    ),
                    reel_core::wake::WakeSignal::new(move || window.request_redraw()),
                )
            },
            backend,
            instance,
            host,
            window,
            presentation: Presentation::new(bridge, info),
            size: PhysicalSize::new(size.width, size.height),
            pool,
            resources,
            fonts,
            active_fonts: font_path
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            font_requests: Default::default(),
            canvas_id,
            game,
            fit_initial_canvas: options.as_ref().is_some_and(|options| {
                options.logical_size.is_none() && options.canvas_size.is_none()
            }),
            epoch: 1,
            pointer: Default::default(),
            modifiers: Default::default(),
            pointer_buttons: Default::default(),
            touch: Default::default(),
            input: Default::default(),
            input_origin: Instant::now(),
            input_focused: true,
            input_events: Vec::with_capacity(6),
            input_pending: false,
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
            gamepads: None,
            root_id,
            zoom: 1.,
            session,
            recorded_projection: None,
            closing: false,
            first_frame_pending,
            completions: Vec::new(),
            window_state: Arc::new(Default::default()),
            window_request: None,
        };
        state.publish_window_info()?;
        state.start_gamepads();

        Ok(state)
    }
    pub(crate) fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) -> Result<(), Error> {
        self.invalidate_surface(size)?;
        if self.backend.surface().is_some() {
            self.backend.configure(self.presentation.surface_info())?;
        }
        let complete = self.host.window_resized();
        self.publish_window_info()?;
        if complete {
            self.complete_window_change(Ok(self.window_state.clone()))?;
        }
        self.window.request_redraw();
        Ok(())
    }
    fn invalidate_surface(&mut self, size: winit::dpi::PhysicalSize<u32>) -> Result<(), Error> {
        self.size = PhysicalSize::new(size.width, size.height);
        self.epoch = self.epoch.checked_add(1).ok_or("surface epoch exhausted")?;
        self.presentation.replace_surface(SurfaceInfo {
            physical_size: [size.width, size.height],
            scale_factor: self.window.scale_factor(),
            surface_epoch: SurfaceEpoch(self.epoch),
            ..self.presentation.surface_info()
        });
        // Invalidates candidates even if fresh negotiation subsequently fails.
        if let Some(session) = &mut self.session {
            session.ui_mut()?.clear_presented();
            session.sync(&mut self.presentation.bridge, self.root_id)?;
        }
        Ok(())
    }
    pub(crate) fn invalidate(&mut self) {
        self.presentation.invalidate();
        if self.presentation.needs_redraw() {
            self.host.request_redraw();
        }
    }
    pub(crate) fn is_diagnostic(&self) -> bool {
        !matches!(self.session, Some(WindowSession::Aside { .. }))
    }
    pub(crate) fn story_deadline(&self) -> Option<Instant> {
        if self.captures.busy() {
            return Some(Instant::now() + std::time::Duration::from_millis(8));
        }
        self.session
            .as_ref()
            .filter(|session| session.needs_tick())
            .and_then(|session| Instant::now().checked_add(session.tick_interval()))
    }
    pub(crate) fn tick_story(&mut self) -> Result<(), Error> {
        self.maintain_window_requests()?;
        self.maintain_captures()?;
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        if !session.needs_tick() && !session.ui().images_need_poll() {
            return Ok(());
        }
        let host = &self.host;
        let now = || host.monotonic_now().0;
        let budget = reel_engine::aside::SliceBudget {
            deadline_ns: Some(now().saturating_add(2_000_000)),
            ..Default::default()
        };
        let before = session.ui().projection_stamp();
        session.tick(budget, now)?;
        if !self.closing && session.exit_requested() {
            return self.close();
        }
        self.maintain_captures()?;
        let settings_changed = if self.closing {
            false
        } else {
            self.apply_game_settings()?
        };
        self.maintain_window_requests()?;
        if !self.closing {
            let session = self.session.as_mut().unwrap();
            session.check_fonts()?;
            if self.settings_pending() {
                return Ok(());
            }
            let session = self.session.as_mut().unwrap();
            session.sync(&mut self.presentation.bridge, self.root_id)?;
            if settings_changed || before != session.ui().projection_stamp() {
                self.invalidate();
            }
        }
        Ok(())
    }
    fn maintain_captures(&mut self) -> Result<(), Error> {
        if let Some(WindowSession::Aside { story, .. }) = &mut self.session
            && !story.capturing()
        {
            let changed = self.captures.drive(
                &mut self.backend,
                story.driver_mut()?.adapter_mut(),
                self.presentation.accepted_frame.as_ref(),
            )?;
            if changed {
                story.wake_input();
            }
        }
        Ok(())
    }
    fn settings_pending(&self) -> bool {
        self.session
            .as_ref()
            .and_then(WindowSession::game_settings)
            .is_some_and(|settings| {
                self.game
                    .as_ref()
                    .is_none_or(|applied| !Arc::ptr_eq(applied, settings))
            })
    }
    fn apply_game_settings(&mut self) -> Result<bool, Error> {
        let Some(settings) = self
            .session
            .as_ref()
            .and_then(WindowSession::game_settings)
            .filter(|settings| {
                self.game
                    .as_ref()
                    .is_none_or(|old| !Arc::ptr_eq(old, settings))
            })
            .cloned()
        else {
            return Ok(false);
        };
        self.session.as_mut().unwrap().ui_mut()?.clear_presented();
        let paths = settings.fonts.clone();
        let assets = self.session.as_ref().unwrap().ui().assets();
        self.font_requests.retain(|path, id| {
            if paths.contains(path) {
                true
            } else {
                assets.cancel(*id);
                false
            }
        });
        if paths != self.active_fonts {
            let mut data = Vec::with_capacity(paths.len());
            for path in &paths {
                let index = if let Some(&index) = self.fonts.get(path) {
                    index
                } else {
                    let request = *self.font_requests.entry(path.clone()).or_insert_with(|| {
                        assets.request(path.clone(), crate::assets::AssetKind::Font)
                    });
                    let lease = assets.poll(request).map_err(|e| {
                        format!(
                            "{}: font {}: {e}",
                            settings.source.as_deref().unwrap_or("game.configure"),
                            path
                        )
                    })?;
                    let Some(lease) = lease else {
                        return Ok(false);
                    };
                    self.font_requests.remove(path);
                    let index = self.resources.len();
                    self.resources.push(lease);
                    self.fonts.insert(path.clone(), index);
                    index
                };
                let reel_ui::resources::ResourceData::Font(font) = self.resources[index].data()
                else {
                    unreachable!()
                };
                data.push(font.clone());
            }
            if !self.presentation.bridge.set_default_fonts(&data) {
                return Err(format!(
                    "{}: configured font files contain no usable font",
                    settings.source.as_deref().unwrap_or("game.configure")
                )
                .into());
            }
            self.active_fonts = paths;
        }
        self.window.set_title(&settings.title);
        if self.fit_initial_canvas
            && let Some([width, height]) = settings.canvas_size
        {
            self.fit_initial_canvas = false;
            if let Some(size) = self
                .window
                .request_inner_size(winit::dpi::LogicalSize::new(width, height))
            {
                self.resize(size)?;
            }
        }
        if let Some(id) = self.canvas_id {
            self.presentation.bridge.edit_widget(id, |mut widget| {
                let mut canvas = widget.downcast::<reel_ui::widgets::Canvas>();
                reel_ui::widgets::Canvas::set_size(&mut canvas, settings.canvas_size);
            });
        }
        self.game = Some(settings);
        Ok(true)
    }
    pub(crate) fn close(&mut self) -> Result<(), Error> {
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        {
            self.gamepads = None;
        }
        self.input.cancel(|_| {});
        self.closing = true;
        self.presentation.suspend();
        if let Some(session) = &mut self.session {
            session.ui_mut()?.close_resources();
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
    pub(crate) fn exit_requested(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(WindowSession::exit_requested)
    }
    pub(crate) fn poll_completions(&mut self) -> Result<(), GpuError> {
        self.completions.clear();
        self.presentation
            .poll(&mut self.backend, &mut self.completions, 64)?;
        Ok(())
    }
    pub(crate) fn render(&mut self) -> Result<(), Error> {
        // Candidate metadata layout is never eligible for paint/presentation.
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.ui().images_pending())
        {
            return Ok(());
        }
        // Startup may span several VM slices before main supplies the font files.
        if !self.is_diagnostic() && self.active_fonts.is_empty() {
            return Ok(());
        }
        if self.first_frame_pending
            && (self.settings_pending()
                || !self
                    .session
                    .as_ref()
                    .is_some_and(WindowSession::initial_frame_ready))
        {
            return Ok(());
        }
        self.poll_completions()?;
        let Self {
            presentation,
            backend,
            host,
            pool,
            resources,
            session,
            recorded_projection,
            ..
        } = self;
        let result = presentation.render(backend, host, |bridge, id, info| {
            build_frame(
                bridge,
                id,
                info,
                pool,
                resources,
                session.as_ref(),
                recorded_projection,
            )
        });
        let result = match result {
            Err(error)
                if matches!(
                    error.downcast_ref::<GpuError>(),
                    Some(GpuError::Surface(
                        reel_render::gpu::SurfaceFailure::Lost
                            | reel_render::gpu::SurfaceFailure::Outdated
                    ))
                ) =>
            {
                if matches!(
                    error.downcast_ref::<GpuError>(),
                    Some(GpuError::Surface(reel_render::gpu::SurfaceFailure::Lost))
                ) {
                    self.backend
                        .resume(self.instance.create_surface(self.window.clone())?);
                }
                self.resize(self.window.inner_size())?;
                return Ok(());
            }
            result => result?,
        };
        if result.accepted
            && !self.settings_pending()
            && let (Some(session), Some(stamp)) = (&mut self.session, self.recorded_projection)
        {
            session.presented(stamp)?;
        }
        if result.accepted && self.first_frame_pending {
            self.first_frame_pending = false;
            self.window.set_visible(true);
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
            eprintln!("frame refused; keeping last accepted view: {error}");
        }
        Ok(())
    }
    pub(crate) fn redraw_requested(&mut self) -> Result<(), Error> {
        self.tick_story()?;
        if self.closing {
            return Ok(());
        }
        self.presentation.exposed();
        self.render()
    }
    /// Hidden native windows need not receive redraw events. The event loop
    /// still advances startup using VM deadlines and resource completion wakes.
    pub(crate) fn prepare_initial_frame(&mut self) -> Result<(), Error> {
        if self.first_frame_pending && !self.closing {
            self.render()?;
        }
        Ok(())
    }
    pub(crate) fn recover_device(&mut self) -> Result<(), Error> {
        if let Err(error) = self.backend.renderer().gpu().check() {
            eprintln!("recovering graphics device: {error}");
        }
        self.pointer_event(PointerKind::Cancel)?;
        let size = self.window.inner_size();
        self.invalidate_surface(size)?;
        let generation = self
            .backend
            .renderer()
            .gpu()
            .generation()
            .0
            .checked_add(1)
            .ok_or("device generation exhausted")?;
        self.backend.suspend();
        let surface = self.instance.create_surface(self.window.clone())?;
        let gpu = pollster::block_on(Gpu::request(
            &self.instance,
            Some(&surface),
            DeviceGeneration(generation),
            self.backend.renderer().gpu().limits(),
        ))?;
        let mut backend = WgpuBackend::new(gpu, surface).with_vsync(self.backend.vsync_enabled());
        backend.configure(self.presentation.surface_info())?;
        // Retire the old surface and device together, including zero-sized views.
        self.backend = backend;
        self.publish_window_info()?;
        self.window.request_redraw();
        eprintln!(
            "device recovered generation={generation}, epoch={}",
            self.epoch
        );
        Ok(())
    }
    pub(crate) fn suspend(&mut self) -> Result<(), Error> {
        self.stop_gamepads();
        self.touch.clear();
        self.input.cancel(|_| {});
        self.presentation
            .bridge
            .handle_text_event(TextEvent::WindowFocusChange(false));
        self.presentation.bridge.cancel_pointer_inputs();
        let _ = self.pointer_event(PointerKind::Cancel);
        self.presentation.suspend();
        self.backend.suspend();
        if let Some(session) = &mut self.session {
            session.ui_mut()?.clear_presented();
            session.suspend(self.host.monotonic_now().0)?;
        }
        Ok(())
    }
    pub(crate) fn is_suspended(&self) -> bool {
        self.backend.surface().is_none()
    }
    pub(crate) fn resume(&mut self) -> Result<(), Error> {
        self.backend
            .resume(self.instance.create_surface(self.window.clone())?);
        if self.backend.renderer().gpu().check().is_err() {
            self.recover_device()?;
        } else {
            self.resize(self.window.inner_size())?;
        }
        if let Some(session) = &mut self.session {
            session.resume(self.host.monotonic_now().0);
        }
        self.text_event(TextEvent::WindowFocusChange(self.input_focused))?;
        Ok(())
    }
    pub(crate) fn text_event(&mut self, event: TextEvent) -> Result<Handled, Error> {
        let event = self.host.clipboard_paste(&event).unwrap_or(event);
        if let TextEvent::WindowFocusChange(focus) = &event {
            self.input_focused = *focus;
            if *focus && !self.is_suspended() {
                self.start_gamepads();
            }
            if !focus {
                self.stop_gamepads();
                self.input.cancel(|_| {});
            }
        }
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
        if lost_focus {
            self.touch.clear();
        }
        if self.settings_pending() && !cancelling {
            return Ok(Handled::Yes);
        }
        let handled = if let Some(session) = &mut self.session {
            let handled = session.handle_text(&mut self.presentation.bridge, event)?;
            session.after_input(&mut self.presentation.bridge, self.root_id)?;
            handled
        } else {
            self.presentation.bridge.handle_text_event(event)
        };
        self.invalidate();
        Ok(handled)
    }
    pub(crate) fn pointer_event(&mut self, kind: PointerKind) -> Result<(), Error> {
        if self.settings_pending() && !matches!(kind, PointerKind::Cancel) {
            return Ok(());
        }
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
            PointerKind::Scroll(delta) => PointerEvent::Scroll(PointerScrollEvent {
                pointer,
                state,
                delta,
            }),
        };
        self.dispatch_pointer(event)
    }
    pub(crate) fn touch_event(&mut self, touch: winit::event::Touch) -> Result<(), Error> {
        if self.is_suspended() || self.settings_pending() {
            return Ok(());
        }
        let modifiers = ui_events_winit::keyboard::from_winit_modifier_state(self.modifiers);
        if let Some(event) = self
            .touch
            .event(touch, self.window.scale_factor(), modifiers)
        {
            self.dispatch_pointer(event)?;
        }
        Ok(())
    }
    fn dispatch_pointer(&mut self, event: PointerEvent) -> Result<(), Error> {
        let released = matches!(event, PointerEvent::Up(_));
        if let Some(session) = &mut self.session {
            session.handle_pointer(&mut self.presentation.bridge, event)?;
            if released {
                session.after_input(&mut self.presentation.bridge, self.root_id)?;
            }
        } else {
            self.presentation.bridge.handle_pointer_event(event)?;
        }
        self.invalidate();
        Ok(())
    }
    fn start_gamepads(&mut self) {
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        if self.gamepads.is_none() {
            let window = Arc::downgrade(&self.window);
            match crate::platform::gamepad::GamepadSource::new(
                std::num::NonZeroUsize::new(256).unwrap(),
                move || {
                    if let Some(window) = window.upgrade() {
                        window.request_redraw();
                    }
                },
            ) {
                Ok(source) => self.gamepads = Some(source),
                Err(error) => eprintln!("gamepad backend unavailable: {error}"),
            }
        }
    }
    fn stop_gamepads(&mut self) {
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        {
            self.gamepads = None;
        }
        self.input_pending = false;
    }
    pub(crate) fn input_deadline(&self) -> Option<Instant> {
        if self.input_pending {
            Some(Instant::now())
        } else {
            self.input
                .next_deadline()
                .and_then(|when| self.input_origin.checked_add(when))
        }
    }
    pub(crate) fn tick_gamepads(&mut self) -> Result<(), Error> {
        let now = self.input_origin.elapsed();
        self.input_pending = false;
        // A bounded drain prevents a noisy device from monopolizing a frame.
        // Remaining events keep the window awake via the backend's notifications.
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        for index in 0..256 {
            let Some(source) = &self.gamepads else {
                break;
            };
            let event = match source.try_event() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    eprintln!("gamepad backend stopped: {error}");
                    self.gamepads = None;
                    self.input.cancel(|_| {});
                    self.text_event(TextEvent::Input(reel_ui::input::InputEvent::Cancel))?;
                    self.text_event(TextEvent::Navigation(reel_ui::input::NavigationEvent {
                        action: reel_ui::input::NavigationAction::Confirm,
                        phase: reel_ui::input::ActionPhase::Cancelled,
                        value: 0.,
                    }))?;
                    break;
                }
            };
            if self.input_focused {
                let claimed = self
                    .text_event(TextEvent::Input(event.clone()))?
                    .is_handled();
                self.input_events.clear();
                if claimed && matches!(event, reel_ui::input::InputEvent::Value { .. }) {
                    self.input.cancel(|e| self.input_events.push(e));
                } else {
                    self.input
                        .update(&event, now, |e| self.input_events.push(e))?;
                }
                for i in 0..self.input_events.len() {
                    self.text_event(TextEvent::Navigation(self.input_events[i]))?;
                }
            }
            self.input_pending = index == 255;
        }
        self.input_events.clear();
        self.input.tick(now, |event| self.input_events.push(event));
        for i in 0..self.input_events.len() {
            self.text_event(TextEvent::Navigation(self.input_events[i]))?;
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub(crate) enum PointerKind {
    Move,
    Down(PointerButton),
    Up(PointerButton),
    Cancel,
    Scroll(ScrollDelta),
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(session) = &mut self.session {
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
fn build_frame(
    bridge: &mut PresentationBridge,
    id: FrameId,
    info: SurfaceInfo,
    pool: &BudgetPool,
    resources: &[reel_ui::resources::ResourceLease],
    session: Option<&WindowSession>,
    recorded_projection: &mut Option<ProjectionStamp>,
) -> Result<RenderFrame, Error> {
    let source = bridge.record(id).map_err(|e| format!("record: {e:?}"))?;
    *recorded_projection = session.map(|session| session.ui().projection_stamp());
    let images = session.map_or(&[][..], |session| session.ui().image_resources());
    let snapshot = reel_ui::resources::ResourceSnapshot::capture(
        (0..resources.len() + images.len()).map(|index| {
            if index < resources.len() {
                Ok(resources[index].clone())
            } else {
                Ok(images[index - resources.len()].clone())
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

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
impl State {
    fn build_frame(&mut self) -> Result<RenderFrame, Error> {
        self.presentation.frame += 1;
        let info = self.presentation.surface_info();
        build_frame(
            &mut self.presentation.bridge,
            FrameId(self.presentation.frame),
            info,
            &self.pool,
            &self.resources,
            self.session.as_ref(),
            &mut self.recorded_projection,
        )
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod tests {
    use super::*;
    use crate::demand::Model;
    use reel_render::{PresentOutcome, presentation_notice};
    use reel_ui::app::InputGeometrySource;
    #[cfg(target_os = "windows")]
    use winit::platform::windows::EventLoopBuilderExtWindows;
    #[cfg(target_os = "linux")]
    use winit::platform::x11::EventLoopBuilderExtX11;
    use winit::{application::ApplicationHandler, event_loop::EventLoop};

    fn accepted_transform(state: &State) -> reel_ui::kurbo::Affine {
        let InputGeometrySource::Presented(Some(view)) =
            state.presentation.bridge.root().input_geometry_source()
        else {
            panic!("input requires an actually accepted surface frame");
        };
        view.get(state.root_id).unwrap().transform
    }
    fn exercise(event_loop: &ActiveEventLoop) {
        // Derive pressure from the current profile rather than assuming 4x support.
        let mut probe = State::with_limits(
            event_loop,
            1,
            GpuLimits {
                target_bytes: 512 << 20,
                ..Default::default()
            },
        )
        .unwrap();
        let probe_frame = probe.build_frame().unwrap();
        let prepared = probe.backend.renderer_mut().prepare(&probe_frame).unwrap();
        let single_targets = probe.backend.renderer_mut().usage().targets;
        let minimum_single_target = u64::from(probe.size.width) * u64::from(probe.size.height) * 4;
        let second = probe.backend.renderer().prepare(&probe_frame).unwrap();
        let first_id = probe.backend.renderer_mut().submit(prepared).unwrap();
        let second_id = probe.backend.renderer_mut().submit(second).unwrap();
        probe
            .backend
            .renderer()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let mut events = Vec::new();
        probe.backend.poll(&mut events, 0).unwrap();
        assert!(events.is_empty());
        assert!(probe.backend.has_pending());
        probe.backend.poll(&mut events, 1).unwrap();
        assert_eq!(events.len(), 1);
        assert!(probe.backend.has_pending());
        probe.backend.poll(&mut events, 1).unwrap();
        assert_eq!(events.len(), 2);
        assert!(!probe.backend.has_pending());
        for (event, id) in events.iter().zip([first_id, second_id]) {
            assert!(
                matches!(event, RenderEvent::RenderFinished { id: actual, frame }
                if actual.0 == id && *frame == probe_frame.frame_id())
            );
        }
        assert!(
            probe.presentation.bridge.accepted_key().is_none(),
            "offscreen completion never accepts input"
        );
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
        assert!(state.presentation.bridge.accepted_key().is_some());
        state
            .backend
            .renderer_mut()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        state.poll_completions().unwrap();
        let before_expose = state.presentation.frame;
        state.redraw_requested().unwrap();
        assert!(state.presentation.frame > before_expose);
        let accepted = state.presentation.bridge.accepted_key().unwrap().frame_id();
        assert_eq!(state.presentation.demand.model, Model::Idle);
        state
            .backend
            .renderer_mut()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        state.poll_completions().unwrap();
        // Successful idle never creates a timer-driven model preparation.
        let after_expose = state.presentation.frame;
        state.render().unwrap();
        assert_eq!(state.presentation.frame, after_expose);
        let held_frame = state.build_frame().unwrap();
        let held = state.backend.renderer_mut().prepare(&held_frame).unwrap();
        eprintln!(
            "native pressure fixture targets={}",
            state.backend.renderer_mut().usage().targets
        );
        state
            .presentation
            .bridge
            .edit_widget(state.root_id, |mut widget| {
                widget.set_transform(reel_ui::kurbo::Affine::scale(0.7))
            });
        state.invalidate();
        state.render().unwrap();
        assert_eq!(state.presentation.demand.model, Model::Waiting);
        assert_eq!(
            state.presentation.bridge.accepted_key().unwrap().frame_id(),
            accepted
        );
        assert_eq!(accepted_transform(&state), reel_ui::kurbo::Affine::IDENTITY);
        let refused_frame = state.presentation.frame;
        // No completion and no new input: OS redraws cannot repeatedly compile.
        for _ in 0..10 {
            state.render().unwrap();
        }
        assert_eq!(state.presentation.frame, refused_frame);
        let id = state.backend.renderer_mut().submit(held).unwrap(); // offscreen: never accepts its geometry
        state
            .backend
            .renderer_mut()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        assert_eq!(
            state.presentation.bridge.accepted_key().unwrap().frame_id(),
            accepted
        );
        assert!(state.backend.renderer_mut().usage().targets > 0);
        state.render().unwrap(); // observes completion and retries latest state, without input
        assert_eq!(state.presentation.demand.model, Model::Idle);
        assert!(state.presentation.bridge.accepted_key().unwrap().frame_id() > accepted);
        assert_eq!(
            accepted_transform(&state),
            reel_ui::kurbo::Affine::scale(0.7)
        );
        state
            .backend
            .renderer_mut()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        state.poll_completions().unwrap();
        // R01 can freeze this finite f64 transform; GPU conversion cannot represent it.
        state
            .presentation
            .bridge
            .edit_widget(state.root_id, |mut widget| {
                widget.set_transform(reel_ui::kurbo::Affine::scale(1e100))
            });
        state.invalidate();
        state.redraw_requested().unwrap();
        assert_eq!(state.presentation.demand.model, Model::Blocked);
        let failed_count = state.presentation.frame;
        let accepted = state.presentation.bridge.accepted_key().unwrap().frame_id();
        state.redraw_requested().unwrap(); // repaint exact accepted frame; no bad-model compile
        assert!(
            state.backend.renderer_mut().usage().targets > 0,
            "expose must actually submit the frozen accepted frame"
        );
        assert_eq!(state.presentation.frame, failed_count);
        assert_eq!(
            state.presentation.bridge.accepted_key().unwrap().frame_id(),
            accepted
        );
        assert_eq!(
            accepted_transform(&state),
            reel_ui::kurbo::Affine::scale(0.7)
        );
        state
            .backend
            .renderer_mut()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        state.poll_completions().unwrap();
        for _ in 0..10 {
            state.render().unwrap();
        }
        assert_eq!(state.presentation.frame, failed_count);
        assert_eq!(state.presentation.demand.model, Model::Blocked);
        state
            .presentation
            .bridge
            .edit_widget(state.root_id, |mut widget| {
                widget.set_transform(reel_ui::kurbo::Affine::scale(0.7))
            });
        state.invalidate();
        state.redraw_requested().unwrap();
        assert_eq!(state.presentation.demand.model, Model::Idle);
        assert!(!state.backend.renderer_mut().poll().unwrap().contains(&id)); // completion is delivered once

        state
            .backend
            .renderer_mut()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        state.poll_completions().unwrap();
        let old_frame = state.build_frame().unwrap();
        let old = state.backend.renderer_mut().prepare(&old_frame).unwrap();
        let old_notice = presentation_notice(
            &old_frame,
            SurfaceEpoch(state.epoch),
            Ok::<_, reel_render::RenderError>(PresentOutcome::PresentAccepted(
                old_frame.frame_id(),
            )),
        );
        let old_epoch = state.epoch;
        // Recovery negotiates a fresh configuration for the new device generation.
        let acquired = match state.backend.surface().unwrap().get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            other => panic!("expected an acquired surface frame: {other:?}"),
        };
        state.backend.renderer_mut().gpu().device.destroy();
        let _ = state
            .backend
            .renderer_mut()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely());
        // Regression: wgpu 26 panicked in Surface::discard_texture here,
        // preventing the device recovery below from ever running.
        drop(acquired);
        let Err(GpuError::DeviceLost(loss)) = state.backend.renderer().gpu().check() else {
            panic!("surface cleanup must preserve the original device loss");
        };
        assert_eq!(loss.reason, wgpu::DeviceLostReason::Destroyed);
        state.recover_device().unwrap();
        assert_eq!(
            state.backend.renderer_mut().gpu().generation(),
            DeviceGeneration(2)
        );
        assert_eq!(state.epoch, old_epoch + 1);
        assert!(state.backend.configuration().unwrap().format.is_srgb());
        assert!(state.presentation.bridge.accepted_key().is_none());
        assert!(state.presentation.accepted_frame.is_none());
        assert_eq!(
            state.presentation.bridge.resolve(old_notice),
            reel_ui::bridge::NoticeResult::StaleOrForeign
        );
        assert!(matches!(
            state.backend.renderer_mut().submit(old),
            Err(GpuError::WrongDevice)
        ));
        state.render().unwrap();
        assert_eq!(
            accepted_transform(&state),
            reel_ui::kurbo::Affine::scale(0.7)
        );
        let generation = state.backend.renderer_mut().gpu().generation();
        let epoch = state.epoch;
        state.suspend().unwrap();
        assert!(state.is_suspended());
        assert!(state.presentation.bridge.accepted_key().is_none());
        assert!(state.presentation.accepted_frame.is_none());
        state.resume().unwrap();
        assert_eq!(state.backend.renderer_mut().gpu().generation(), generation);
        assert_eq!(state.epoch, epoch + 1);
        state.render().unwrap();
        assert!(state.presentation.bridge.accepted_key().is_some());
        state.resize(winit::dpi::PhysicalSize::new(0, 0)).unwrap();
        let frame = state.presentation.frame;
        state.backend.renderer().gpu().device.destroy();
        state
            .invalidate_surface(winit::dpi::PhysicalSize::new(0, 0))
            .unwrap();
        state.backend.suspend();
        let surface = state.instance.create_surface(state.window.clone()).unwrap();
        let gpu = pollster::block_on(Gpu::request(
            &state.instance,
            Some(&surface),
            DeviceGeneration(3),
            state.backend.renderer().gpu().limits(),
        ))
        .unwrap();
        let mut backend = WgpuBackend::new(gpu, surface);
        backend
            .configure(state.presentation.surface_info())
            .unwrap();
        state.backend = backend;
        state.render().unwrap();
        assert_eq!(state.presentation.frame, frame);
        assert!(state.presentation.bridge.accepted_key().is_none());
        assert!(state.presentation.accepted_frame.is_none());
        state.resize(state.window.inner_size()).unwrap();
        state.render().unwrap();
        assert!(state.presentation.bridge.accepted_key().is_some());
        state
            .backend
            .renderer()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        state.poll_completions().unwrap();
        let frame = state.build_frame().unwrap();
        let device = state.backend.renderer().gpu().device.clone();
        // Loss after queue submission but before presentation must not report
        // PresentAccepted or panic in Queue::present / SurfaceTexture::drop.
        let result = state.backend.render_present(&frame, || {
            device.destroy();
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        });
        assert!(matches!(result, Err(GpuError::DeviceLost(_))));
        state.recover_device().unwrap();
        state.render().unwrap();
        assert!(state.presentation.bridge.accepted_key().is_some());
        drop(state);

        // Permanent refusals stop preparation even when unrelated GPU work completes.
        for limits in [
            GpuLimits {
                target_bytes: minimum_single_target,
                ..Default::default()
            },
            GpuLimits {
                resource_bytes: 1,
                ..Default::default()
            },
        ] {
            let mut refused = State::with_limits(event_loop, 1, limits).unwrap();
            refused.render().unwrap();
            assert_eq!(refused.presentation.demand.model, Model::Blocked);
            assert!(refused.presentation.bridge.accepted_key().is_none());
            let count = refused.presentation.frame;
            for _ in 0..10 {
                refused.redraw_requested().unwrap();
            }
            assert_eq!(refused.presentation.frame, count);
            assert_eq!(refused.backend.renderer_mut().usage().targets, 0);
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
        let scale = state.window.scale_factor();
        state.pointer = reel_ui::dpi::PhysicalPosition::new(x * scale, y * scale);
        for kind in [
            PointerKind::Move,
            PointerKind::Down(PointerButton::Primary),
            PointerKind::Up(PointerButton::Primary),
        ] {
            state.pointer_event(kind).unwrap();
        }
    }
    fn dark_dialogue_pixels(state: &mut State) -> usize {
        state
            .backend
            .renderer()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        state.poll_completions().unwrap();
        let frame = state.build_frame().unwrap();
        let prepared = state.backend.renderer_mut().prepare(&frame).unwrap();
        let pixels = state.backend.renderer_mut().readback(prepared).unwrap();
        let width = state.size.width as usize;
        let px = |value: f64| (value * state.window.scale_factor()) as usize;
        let left = px(45.);
        let right = px(585.);
        (px(280.)..px(365.))
            .flat_map(|y| (left..right).map(move |x| y * width + x))
            .filter(|index| {
                let p = pixels[*index];
                p[0] < 0.3 && p[1] < 0.3 && p[2] < 0.3 && p[3] > 0.9
            })
            .count()
    }
    fn present_session(state: &mut State) {
        // The fixture explicitly drives completion boundaries; immediate hardware
        // completion is not guaranteed by a render attempt or an offscreen readback.
        state
            .backend
            .renderer()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        state.poll_completions().unwrap();
        state.render().unwrap();
        state
            .backend
            .renderer()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        state.poll_completions().unwrap();
    }
    fn advance_session_text(state: &mut State) {
        let session = state.session.as_mut().unwrap();
        session.ui_mut().unwrap().pause_dialogue();
        session.ui_mut().unwrap().advance_dialogue(0, 30.).unwrap();
        session
            .ui_mut()
            .unwrap()
            .advance_dialogue(33_333_334, 30.)
            .unwrap();
        session
            .sync(&mut state.presentation.bridge, state.root_id)
            .unwrap();
        state.invalidate();
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
        present_session(&mut state);
        assert!(state.presentation.bridge.accepted_key().is_some());
        let hidden_pixels = dark_dialogue_pixels(&mut state);
        assert_eq!(
            hidden_pixels, 0,
            "hidden full-text layout paints no dialogue glyph pixels"
        );
        advance_session_text(&mut state);
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
        assert!(state.story_deadline().is_none());
        assert_eq!(session_reveal(&state), 1);
        assert!(state.presentation.bridge.accepted_key().is_none());
        present_session(&mut state);
        state.backend.renderer_mut().gpu().device.destroy();
        let _ = state
            .backend
            .renderer_mut()
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely());
        state.recover_device().unwrap();
        assert_eq!(session_reveal(&state), 1);
        assert!(state.story_deadline().is_none());
        assert!(state.presentation.bridge.accepted_key().is_none());
        present_session(&mut state);
        state.suspend().unwrap();
        assert!(state.story_deadline().is_none());
        assert!(state.presentation.bridge.accepted_key().is_none());
        state.resume().unwrap();
        assert!(state.story_deadline().is_none());
        assert_eq!(session_reveal(&state), 1);
        assert_eq!(
            state.session.as_ref().unwrap().ui().session().identity(),
            identity
        );
        present_session(&mut state);
        // Holding a real pointer gesture freezes the timed reveal revision.
        let scale = state.window.scale_factor();
        state.pointer = reel_ui::dpi::PhysicalPosition::new(100. * scale, 300. * scale);
        state.pointer_event(PointerKind::Move).unwrap();
        state
            .pointer_event(PointerKind::Down(PointerButton::Primary))
            .unwrap();
        advance_session_text(&mut state);
        assert_eq!(session_reveal(&state), 1);
        state
            .pointer_event(PointerKind::Up(PointerButton::Primary))
            .unwrap();
        assert!(session_reveal(&state) > 1);
        assert!(state.story_deadline().is_none());
        present_session(&mut state);
        let shown_pixels = dark_dialogue_pixels(&mut state);
        assert!(
            shown_pixels > 100,
            "real Parley glyphs must produce GPU pixels"
        );
        eprintln!("GPU dialogue glyph coverage: hidden={hidden_pixels}, full={shown_pixels}");
        session_click(&mut state, 100., 300.); // drain first completion, request second line
        assert_eq!(state.session.as_ref().unwrap().diagnostic_step(), 1);
        assert_eq!(session_reveal(&state), 0);
        session_click(&mut state, 100., 300.); // still accepted old first-line frame
        assert_eq!(session_reveal(&state), 0);
        present_session(&mut state);
        session_click(&mut state, 100., 300.); // reveal second line
        present_session(&mut state);
        session_click(&mut state, 100., 300.); // install choice
        assert_eq!(state.session.as_ref().unwrap().diagnostic_step(), 2);
        present_session(&mut state);
        session_click(&mut state, 100., 320.); // actual second Button
        assert_eq!(state.session.as_ref().unwrap().diagnostic_step(), 3);
        let session = state.session.as_ref().unwrap().ui().session();
        assert_eq!(session.status(), reel_engine::SessionStatus::Ended);
        assert_eq!(session.view().history.len(), 2);
        assert!(session.view().interaction.is_none());
        present_session(&mut state);
        drop(state);
        let mut cancelled = State::new_session(event_loop, 1).unwrap();
        present_session(&mut cancelled);
        assert!(
            cancelled
                .session
                .as_mut()
                .unwrap()
                .ui_mut()
                .unwrap()
                .cancel()
        );
        assert!(
            !cancelled
                .session
                .as_mut()
                .unwrap()
                .ui_mut()
                .unwrap()
                .cancel()
        );
        cancelled
            .session
            .as_mut()
            .unwrap()
            .after_input(&mut cancelled.presentation.bridge, cancelled.root_id)
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
    #[ignore = "requires a native display, a graphics adapter and REEL_SESSION_FONT"]
    fn actual_session_window_host_contract() {
        struct Harness;
        impl ApplicationHandler for Harness {
            fn resumed(&mut self, event_loop: &ActiveEventLoop) {
                exercise_session(event_loop);
                exercise_author_resources(event_loop, None);
                exercise_author_resources(event_loop, Some([720, 480]));
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
        #[cfg(target_os = "linux")]
        let event_loop = EventLoop::builder()
            .with_x11()
            .with_any_thread(true)
            .build()
            .unwrap();
        #[cfg(target_os = "windows")]
        let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
        event_loop.run_app(&mut Harness).unwrap();
    }
    fn exercise_author_resources(event_loop: &ActiveEventLoop, initial_size: Option<[u32; 2]>) {
        use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
        let font = std::env::var("REEL_SESSION_FONT")
            .unwrap()
            .replace('\\', "/");
        let alternate = {
            let (parent, name) = font.rsplit_once('/').unwrap();
            format!("{parent}/./{name}")
        };
        let source = format!(
            r#"
fn main() {{
    game.configure(width: 640, height: 400, fonts: [{font:?}]);
    let picture = image("assets/panels.png");
    stage.replace(picture);
    await stage.presented(owner: picture);
    narrator("A");
    game.configure(width: 800, height: 600, fonts: [{alternate:?}]);
    picture.source = "assets/sun.jpg";
    narrator("B");
    game.exit();
    throw "continued after shutdown";
}}
"#
        );
        let options = StoryWindowOptions {
            logical_size: initial_size,
            assets: crate::assets::AssetOptions {
                root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
                ..Default::default()
            },
            ..Default::default()
        };
        let driver = AsideSessionUi::from_source(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            &source,
            "main",
            1,
            "main",
            "en",
            VmLimits::default(),
        )
        .unwrap()
        .with_asset_options(options.assets.clone());
        let mut state = State::new_story(event_loop, 1, driver, options).unwrap();
        assert!(state.first_frame_pending);
        #[cfg(target_os = "windows")]
        assert_eq!(state.window.is_visible(), Some(false));
        state.prepare_initial_frame().unwrap();
        assert!(state.presentation.bridge.accepted_key().is_none());
        assert!(
            state.first_frame_pending,
            "startup without scene/fonts is not a frame"
        );
        let assets = state.session.as_ref().unwrap().ui().assets();
        let (send, receive) = std::sync::mpsc::channel();
        assets.set_waker(Arc::new(move || {
            let _ = send.send(());
        }));
        let settle = |state: &mut State| {
            let deadline = Instant::now() + std::time::Duration::from_secs(15);
            loop {
                state.tick_story().unwrap();
                if !state.settings_pending()
                    && !state.session.as_ref().unwrap().ui().images_pending()
                    && !state.session.as_ref().unwrap().needs_tick()
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "author resources failed to settle"
                );
                if !state.session.as_ref().unwrap().needs_tick() {
                    receive
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .expect("author resources must wake window");
                }
            }
        };
        settle(&mut state);
        assert_eq!(state.active_fonts, std::slice::from_ref(&font));
        #[cfg(target_os = "windows")]
        {
            let [width, height] = initial_size.unwrap_or([640, 400]);
            assert_eq!(
                state.window.inner_size(),
                winit::dpi::LogicalSize::new(width, height)
                    .to_physical(state.window.scale_factor()),
                "the first canvas sets the default client size; explicit window sizes take precedence"
            );
            let requested = winit::dpi::PhysicalSize::new(850, 625);
            let _ = state.window.request_inner_size(requested);
            let deadline = Instant::now() + std::time::Duration::from_secs(2);
            while state.window.inner_size() != requested && Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(state.window.inner_size(), requested);
            state.resize(requested).unwrap();
        }
        let player_size = state.window.inner_size();
        assert!(
            state
                .session
                .as_ref()
                .unwrap()
                .ui()
                .session()
                .view()
                .interaction
                .is_none()
        );
        assert!(state.first_frame_pending);
        #[cfg(target_os = "windows")]
        assert_eq!(state.window.is_visible(), Some(false));
        state.prepare_initial_frame().unwrap();
        assert!(
            !state.first_frame_pending,
            "startup presents without a native redraw event"
        );
        assert!(state.presentation.bridge.accepted_key().is_some());
        #[cfg(target_os = "windows")]
        assert_eq!(state.window.is_visible(), Some(true));
        settle(&mut state);
        present_session(&mut state);
        let old_frame = state.build_frame().unwrap();
        let old_pixels = state.session.as_ref().unwrap().ui().image_resources()[0].clone();
        let session = state.session.as_mut().unwrap();
        let id = session.ui().session().view().interaction.unwrap().id();
        let owner = session.ui().session().root_owner();
        for _ in 0..2 {
            session
                .ui_mut()
                .unwrap()
                .session_mut()
                .handle_local_input(owner, reel_engine::InputAction::Advance(id))
                .unwrap();
        }
        session
            .after_input(&mut state.presentation.bridge, state.root_id)
            .unwrap();
        // Run the VM to the second wait, then deliberately leave IO completions unpolled.
        while state.session.as_ref().unwrap().needs_tick() {
            state
                .session
                .as_mut()
                .unwrap()
                .tick(Default::default(), || 0)
                .unwrap();
        }
        assert!(state.settings_pending());
        assert!(!state.apply_game_settings().unwrap());
        assert_eq!(state.active_fonts, [font]);
        assert_eq!(
            state.session.as_ref().unwrap().ui().image_resources()[0].handle(),
            old_pixels.handle()
        );
        let reveal = session_reveal(&state);
        session_click(&mut state, 100., 300.);
        advance_session_text(&mut state);
        assert_eq!(
            session_reveal(&state),
            reveal,
            "old frame cannot consume input while fonts load"
        );
        settle(&mut state);
        assert_eq!(state.active_fonts, [alternate]);
        assert_eq!(
            state.window.inner_size(),
            player_size,
            "later configuration preserves the player's window size"
        );
        let lease = &state.session.as_ref().unwrap().ui().image_resources()[0];
        assert_eq!(lease.version().key.0, "assets/sun.jpg");
        assert!(old_pixels.is_retired());
        present_session(&mut state);
        let frame = state.build_frame().unwrap();
        let prepared = state.backend.renderer_mut().prepare(&frame).unwrap();
        eprintln!(
            "author window size={:?} quality={:?} usage={:?}",
            prepared.size(),
            prepared.quality(),
            state.backend.renderer().usage()
        );
        let pixels = state.backend.renderer_mut().readback(prepared).unwrap();
        assert_eq!(
            pixels.len(),
            state.size.width as usize * state.size.height as usize
        );
        assert!(!old_frame.resources().leases().is_empty());
        advance_session_text(&mut state);
        present_session(&mut state);
        session_click(&mut state, 100., 300.);
        let shutdown_deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !state.close_complete() {
            state.tick_story().unwrap();
            assert!(
                Instant::now() < shutdown_deadline,
                "author shutdown did not finish"
            );
        }
        assert!(state.exit_requested());
        eprintln!(
            "author font/image IO, atomic replacement, input pause, GPU readback and close passed"
        );
    }
    #[test]
    #[ignore = "requires a native display and actual graphics adapter"]
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
        #[cfg(target_os = "linux")]
        let event_loop = EventLoop::builder()
            .with_x11()
            .with_any_thread(true)
            .build()
            .unwrap();
        #[cfg(target_os = "windows")]
        let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
        event_loop.run_app(&mut Harness).unwrap();
    }
}
