use super::diagnostic::Diagnostic;
use crate::hal::{Host, PlatformHost};
#[cfg(test)]
use crate::player::build_frame;
use crate::player::{PlayerOptions, PointerKind};
use crate::session::SessionHostDiagnostic;
use crate::{aside_session::AsideSessionUi, player::session::PlayerSession};
use crate::{platform::winit::WinitHost, presentation::Presentation};
#[cfg(test)]
use reel_core::identity::FrameId;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    clock::Clock,
    identity::{AssetKey, ContentVersion, ResourceVersion, SurfaceEpoch},
    wake::WakeSignal,
};
#[cfg(test)]
use reel_render::RenderEvent;
use reel_render::{
    AlphaMode, RenderBackend, SurfaceInfo,
    gpu::{DeviceGeneration, Gpu, GpuError, GpuLimits, WgpuBackend},
};
use reel_ui::bridge::{BridgeLimits, PresentationBridge};
#[cfg(test)]
use reel_ui::frame::RenderFrame;
use reel_ui::resources::ResourceRegistry;
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    core::*,
    dpi::PhysicalSize,
};
use std::{sync::Arc, time::Instant};
use winit::{event_loop::ActiveEventLoop, window::Window};

type Error = Box<dyn std::error::Error>;
#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod scroll_cost;

/// Desktop windows allow 1 GiB each for GPU resources and offscreen targets,
/// 64 MiB for transient rendering data and 64 MiB per image upload preparation.
/// Explicit limits bypass this selection. Reservations follow actual usage.
fn default_gpu_limits() -> GpuLimits {
    let mut limits = GpuLimits::default();
    if cfg!(any(
        target_os = "windows",
        target_os = "linux",
        target_os = "macos"
    )) {
        limits.resource_bytes = 1 << 30;
        limits.target_bytes = 1 << 30;
        limits.transient_bytes = 64 << 20;
        limits.image_upload_bytes_per_prepare = 64 << 20;
    }
    limits
}

// Surface is declared first and explicitly dropped before Window on shutdown/suspend.
pub(crate) struct State {
    pub(crate) player: crate::player::Player<WgpuBackend>,
    instance: wgpu::Instance,
    pub(crate) window: Arc<Window>,
    pub(crate) host: WinitHost,
    pub(crate) modifiers: winit::keyboard::ModifiersState,
    touch: crate::platform::winit::TouchInput,
    input: reel_ui::input::NavigationInput,
    input_origin: Instant,
    input_focused: bool,
    input_events: Vec<reel_ui::input::NavigationEvent>,
    input_pending: bool,
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    gamepads: Option<crate::platform::gamepad::GamepadSource>,
    pub(crate) zoom: f64,
}
impl State {
    fn assemble(
        player: crate::player::Player<WgpuBackend>,
        instance: wgpu::Instance,
        window: Arc<Window>,
        host: WinitHost,
    ) -> Self {
        let mut state = Self {
            player,
            instance,
            host,
            window,
            modifiers: Default::default(),
            touch: Default::default(),
            input: Default::default(),
            input_origin: Instant::now(),
            input_focused: true,
            input_events: Vec::with_capacity(6),
            input_pending: false,
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
            gamepads: None,
            zoom: 1.,
        };
        state.start_gamepads();

        state
    }
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        wake: WakeSignal,
        generation: u64,
    ) -> Result<Self, Error> {
        Self::with_limits(event_loop, wake, generation, default_gpu_limits())
    }
    pub(crate) fn new_session(
        event_loop: &ActiveEventLoop,
        wake: WakeSignal,
        generation: u64,
    ) -> Result<Self, Error> {
        Self::with_mode(
            event_loop,
            wake,
            generation,
            default_gpu_limits(),
            Some(PlayerSession::Diagnostic(Box::new(
                SessionHostDiagnostic::new()?,
            ))),
            None,
        )
    }
    pub(crate) fn new_story(
        event_loop: &ActiveEventLoop,
        wake: WakeSignal,
        generation: u64,
        driver: AsideSessionUi,
        options: PlayerOptions,
    ) -> Result<Self, Error> {
        let root = if let Some(root) = &options.save_directory {
            root.clone()
        } else {
            #[cfg(target_os = "windows")]
            let data = std::env::var_os("LOCALAPPDATA")
                .map(std::path::PathBuf::from)
                .ok_or("LOCALAPPDATA is unavailable; set PlayerOptions.save_directory")?;
            #[cfg(target_os = "macos")]
            let data = std::env::var_os("HOME")
                .map(|home| std::path::PathBuf::from(home).join("Library/Application Support"))
                .ok_or("HOME is unavailable; set PlayerOptions.save_directory")?;
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME")
                        .map(|home| std::path::PathBuf::from(home).join(".local/share"))
                })
                .ok_or("user data directory is unavailable; set PlayerOptions.save_directory")?;
            crate::saves::default_save_directory(&data, &options.assets.root)?
        };
        let save_bytes = options.save_bytes;
        let media_wake = wake.clone();
        let media = crate::media::desktop_media(
            driver.ui().assets(),
            options.media.clone(),
            WakeSignal::new(move || media_wake.wake()),
        );
        let driver = driver.with_media(media())?;
        let mut state = Self::with_mode(
            event_loop,
            wake.clone(),
            generation,
            default_gpu_limits(),
            Some(PlayerSession::aside(driver)),
            Some(options),
        )?;
        let service = crate::saves::SaveService::new(
            Arc::new(crate::saves::DirectorySaves::new(root, save_bytes)),
            reel_core::budget::QueueLimits {
                max_requests: 1,
                max_bytes: save_bytes,
            },
            WakeSignal::new(move || wake.wake()),
        );
        if let Some(PlayerSession::Aside { story: runtime, .. }) = &mut state.player.session {
            runtime.set_saves(crate::saves::RuntimeSaves::new(service, save_bytes));
            runtime.set_media_factory(media);
        }
        Ok(state)
    }

    fn with_limits(
        event_loop: &ActiveEventLoop,
        wake: WakeSignal,
        generation: u64,
        limits: GpuLimits,
    ) -> Result<Self, Error> {
        Self::with_mode(event_loop, wake, generation, limits, None, None)
    }
    fn with_mode(
        event_loop: &ActiveEventLoop,
        wake: WakeSignal,
        generation: u64,
        limits: GpuLimits,
        mut session: Option<PlayerSession>,
        options: Option<PlayerOptions>,
    ) -> Result<Self, Error> {
        let first_frame_pending = matches!(session, Some(PlayerSession::Aside { .. }));
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
                        PhysicalSize::new(width, height)
                    }),
            )?,
        );
        let instance = wgpu::Instance::default();
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
        let mut host = WinitHost::new(window.clone()).with_event_wake(wake);
        if let Some(options) = options {
            let player = crate::player::Player::mount(
                session.expect("story session supplied with player options"),
                backend,
                &mut host,
                &options,
            )?;
            return Ok(Self::assemble(player, instance, window, host));
        }
        if let Some(session) = &mut session {
            session.ui_mut()?.set_resource_waker(host.wake_callback());
        }
        backend.configure(info)?;
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: u32::MAX,
                max_bytes: 64 << 20,
            },
        );
        let font_path = if session.is_some() {
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
            let descriptor = lease.image_descriptor().expect("diagnostic image");
            let pixels = lease.read_pixels().expect("diagnostic pixels");
            let image = reel_ui::peniko::ImageData {
                data: pixels.blob(),
                format: descriptor.format,
                alpha_type: descriptor.alpha_type,
                width: descriptor.width,
                height: descriptor.height,
            };
            resources.push(lease);
            Some(image)
        } else {
            None
        };
        let mut fonts = std::collections::BTreeMap::new();
        let font = if let Some(path) = &font_path {
            let lease = crate::fonts::load(path, &pool)?;
            let reel_ui::resources::ResourceView::Font(font) = lease.view() else {
                unreachable!()
            };
            let font = font.clone();
            fonts.insert(path.to_string_lossy().into_owned(), resources.len());
            resources.push(lease);
            Some(font)
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
        let canvas_id = None;
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
            .and_then(PlayerSession::game_settings)
            .cloned();
        let player = crate::player::Player {
            captures: {
                let wake = host.wake_callback();
                crate::capture::CaptureService::new(
                    crate::assets::AssetOptions::default().input_bytes,
                    crate::player::PlayerOptions::default().capture_readback_bytes_per_poll,
                    WakeSignal::new(move || wake()),
                )
            },
            backend,
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
            fit_initial_canvas: false,
            epoch: 1,
            pointer: Default::default(),
            pointer_buttons: Default::default(),
            root_id,
            session,
            recorded_projection: None,
            closing: false,
            first_frame_pending,
            ui_animation: Default::default(),
            completions: Vec::new(),
            #[cfg(feature = "performance")]
            desktop_trace: None,
            pixel_recovery: Default::default(),
            window_state: Arc::new(Default::default()),
            window_request: None,
            input_hints: Default::default(),
        };
        Ok(Self::assemble(player, instance, window, host))
    }
    pub(crate) fn recover_device(&mut self) -> Result<(), Error> {
        if let Err(error) = self.player.backend.renderer().gpu().check() {
            eprintln!("recovering graphics device: {error}");
        }
        self.pointer_event(PointerKind::Cancel)?;
        let size = self.window.inner_size();
        self.invalidate_surface(size)?;
        let generation = self
            .player
            .backend
            .renderer()
            .gpu()
            .generation()
            .0
            .checked_add(1)
            .ok_or("device generation exhausted")?;
        self.player.backend.suspend();
        let surface = self.instance.create_surface(self.window.clone())?;
        let gpu = pollster::block_on(Gpu::request(
            &self.instance,
            Some(&surface),
            DeviceGeneration(generation),
            self.player.backend.renderer().gpu().limits(),
        ))?;
        let mut backend =
            WgpuBackend::new(gpu, surface).with_vsync(self.player.backend.vsync_enabled());
        backend.configure(self.player.presentation.surface_info())?;
        // Retire the old surface and device together, including zero-sized views.
        self.player.backend = backend;
        self.publish_window_info()?;
        self.window.request_redraw();
        eprintln!(
            "device recovered generation={generation}, epoch={}",
            self.player.epoch
        );
        Ok(())
    }
    pub(crate) fn is_suspended(&self) -> bool {
        self.player.backend.surface().is_none()
    }
    pub(crate) fn touch_event(&mut self, touch: winit::event::Touch) -> Result<(), Error> {
        if self.is_suspended() {
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
    fn start_gamepads(&mut self) {
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        if self.gamepads.is_none() {
            let wake = self.host.wake_callback();
            match crate::platform::gamepad::GamepadSource::new(
                std::num::NonZeroUsize::new(256).unwrap(),
                move || wake(),
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
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::ControlPoll);
        let now = self.input_origin.elapsed();
        self.input_pending = false;
        // A bounded drain prevents a noisy device from monopolizing a frame.
        // An immediate deadline continues a full batch on the next loop turn.
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

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
impl State {
    fn build_frame(&mut self) -> Result<RenderFrame, Error> {
        self.player.presentation.frame += 1;
        let info = self.player.presentation.surface_info();
        build_frame(
            &mut self.player.presentation.bridge,
            self.player.root_id,
            FrameId(self.player.presentation.frame),
            info,
            &self.player.pool,
            &self.player.resources,
            self.player.session.as_ref(),
            &mut self.player.recorded_projection,
        )
        .map(|frame| frame.expect("test frame is ready"))
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod tests;

impl State {
    fn publish_window_info(&mut self) -> Result<(), Error> {
        self.player.publish_window_info(&mut self.host)
    }
    pub(crate) fn monitor_changed(&mut self) -> Result<(), Error> {
        self.publish_window_info()
    }
    pub(crate) fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) -> Result<(), Error> {
        let metrics = crate::hal::DisplayMetrics {
            physical_size: [size.width, size.height],
            scale_factor: self.window.scale_factor(),
        };
        self.player.resize(&mut self.host, metrics)
    }
    fn invalidate_surface(&mut self, size: winit::dpi::PhysicalSize<u32>) -> Result<(), Error> {
        self.player.invalidate_surface(crate::hal::DisplayMetrics {
            physical_size: [size.width, size.height],
            scale_factor: self.window.scale_factor(),
        })
    }
    pub(crate) fn invalidate(&mut self) {
        self.player.invalidate();
        if self.player.needs_redraw() {
            self.host.request_redraw();
        }
    }
    pub(crate) fn is_diagnostic(&self) -> bool {
        self.player.is_diagnostic()
    }
    pub(crate) fn story_deadline(&self) -> Option<(Instant, crate::story_runtime::WakeSource)> {
        let now = self.host.monotonic_now();
        let (deadline, source) = self.player.story_wake(now)?;
        let at = Instant::now().checked_add(std::time::Duration::from_nanos(
            deadline.0.saturating_sub(now.0),
        ))?;
        Some((at, source))
    }
    pub(crate) fn tick_story(&mut self) -> Result<(), Error> {
        self.player.tick(&mut self.host)
    }
    #[cfg(test)]
    fn apply_game_settings(&mut self) -> Result<bool, Error> {
        self.player.apply_game_settings(&mut self.host)
    }
    pub(crate) fn close(&mut self) -> Result<(), Error> {
        self.stop_gamepads();
        self.input.cancel(|_| {});
        self.player.close()
    }
    pub(crate) fn close_complete(&self) -> bool {
        self.player.close_complete()
    }
    pub(crate) fn exit_requested(&self) -> bool {
        self.player.exit_requested()
    }
    pub(crate) fn poll_completions(&mut self) -> Result<(), GpuError> {
        self.player.poll_completions()
    }
    pub(crate) fn render(&mut self) -> Result<(), Error> {
        match self.player.render(&mut self.host) {
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
                    self.player
                        .backend
                        .resume(self.instance.create_surface(self.window.clone())?);
                }
                // A lost or outdated surface must be replaced even at equal geometry.
                let size = self.window.inner_size();
                self.invalidate_surface(size)?;
                self.player
                    .backend
                    .configure(self.player.presentation.surface_info())?;
                self.resize(size)
            }
            result => result,
        }
    }
    pub(crate) fn redraw_requested(&mut self) -> Result<(), Error> {
        self.player.prepare_redraw(&mut self.host)?;
        if self.player.closing {
            return Ok(());
        }
        self.render()
    }
    pub(crate) fn prepare_initial_frame(&mut self) -> Result<(), Error> {
        if self.player.first_frame_pending && !self.player.closing {
            self.render()?;
        }
        Ok(())
    }
    pub(crate) fn suspend(&mut self) -> Result<(), Error> {
        self.stop_gamepads();
        self.touch.clear();
        self.input.cancel(|_| {});
        self.player.suspend(&mut self.host)
    }
    pub(crate) fn resume(&mut self) -> Result<(), Error> {
        self.player
            .backend
            .resume(self.instance.create_surface(self.window.clone())?);
        if self.player.backend.renderer().gpu().check().is_err() {
            self.recover_device()?;
        }
        self.player.resume(&mut self.host)?;
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
                self.touch.clear();
            }
        }
        self.player.text_event(event)
    }
    pub(crate) fn pointer_event(&mut self, kind: PointerKind) -> Result<(), Error> {
        self.player.pointer_event(kind)
    }
    fn dispatch_pointer(&mut self, event: PointerEvent) -> Result<(), Error> {
        self.player.dispatch_pointer(event)
    }
}
