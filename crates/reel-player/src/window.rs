use crate::diagnostic::Diagnostic;
use masonry_core::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    core::*,
    dpi::PhysicalSize,
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
use reel_stage::{
    BridgeLimits, BuildLimits, FrameBuild, FrameInfo, PresentationBridge, ResourceRegistry,
    StepBudget,
};
use std::sync::Arc;
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
    resources: Vec<reel_stage::ResourceHandle>,
    epoch: u64,
    frame: u64,
    pub(crate) pointer: masonry_core::dpi::PhysicalPosition<f64>,
    pub(crate) dirty: bool,
    pub(crate) root_id: WidgetId,
    pub(crate) zoom: f64,
}
impl State {
    pub(crate) fn new(event_loop: &ActiveEventLoop, generation: u64) -> Result<Self, Error> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Reel renderer diagnostic — drag shapes; no Aside session")
                    .with_inner_size(winit::dpi::LogicalSize::new(640., 420.)),
            )?,
        );
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone())?;
        let gpu = pollster::block_on(Gpu::request(
            &instance,
            Some(&surface),
            DeviceGeneration(generation),
            GpuLimits::default(),
        ))?;
        let capabilities = surface.get_capabilities(&gpu.adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .ok_or("no sRGB surface")?;
        if !capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::Opaque)
        {
            return Err("diagnostic requires an opaque surface".into());
        }
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
        };
        let output = SurfacePass::new(&gpu.device, format)?;
        if size.width != 0 && size.height != 0 {
            surface.configure(&gpu.device, &config);
        }
        eprintln!(
            "adapter={:?}; quality={:?}",
            gpu.adapter.get_info(),
            gpu.quality([config.width, config.height])?
        );
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 64,
                max_bytes: 64 << 20,
            },
        );
        let mut registry =
            ResourceRegistry::new(2, 64 << 10).map_err(|e| format!("registry: {e:?}"))?;
        let version = |name: &str| ResourceVersion {
            key: AssetKey(name.into()),
            version: ContentVersion([1; 32]),
        };
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
                version("diagnostic/checker-v1"),
                bytes,
                [2, 2],
                masonry_core::peniko::ImageFormat::Rgba8,
                masonry_core::peniko::ImageAlphaType::Alpha,
                reel_stage::ColorEncoding::Srgb,
            )
            .map_err(|e| format!("image: {e:?}"))?;
        let lease = registry
            .acquire(image_handle)
            .map_err(|e| format!("image lease: {e:?}"))?;
        let reel_stage::ResourceData::Image { image, .. } = lease.data() else {
            unreachable!()
        };
        let mut resources = vec![image_handle];
        let font_path = std::env::var_os("REEL_DIAGNOSTIC_FONT");
        let font = if let Some(path) = font_path {
            let bytes = std::fs::read(path)?;
            let ticket = pool
                .reserve(bytes.capacity() as u64)
                .map_err(|e| format!("font quota: {e:?}"))?;
            let bytes = OwnedBytes::new(bytes, ticket).map_err(|e| format!("font bytes: {e:?}"))?;
            let handle = registry
                .register_font(version("diagnostic/font-v1"), bytes, 0)
                .map_err(|e| format!("font: {e:?}"))?;
            resources.push(handle);
            let lease = registry
                .acquire(handle)
                .map_err(|e| format!("font lease: {e:?}"))?;
            let reel_stage::ResourceData::Font(font) = lease.data() else {
                unreachable!()
            };
            Some(font.data.clone())
        } else {
            eprintln!("Set REEL_DIAGNOSTIC_FONT to a TTF/OTF file to include real Parley text.");
            None
        };
        let widget = Diagnostic::new(image.clone(), font.is_some()).prepare();
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
                test_font: font,
            },
        );
        let bridge = PresentationBridge::new(
            root,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: masonry_core::app::GeometryLimits {
                    max_nodes: 1024,
                    max_bytes: 2 << 20,
                    max_path: 64,
                },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 16 << 20,
            },
        )
        .map_err(|e| format!("bridge: {e:?}"))?;
        Ok(Self {
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
            pointer: Default::default(),
            dirty: true,
            root_id,
            zoom: 1.,
        })
    }
    pub(crate) fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) -> Result<(), Error> {
        self.size = PhysicalSize::new(size.width, size.height);
        self.epoch = self.epoch.checked_add(1).ok_or("surface epoch exhausted")?;
        self.bridge.replace_surface(SurfaceEpoch(self.epoch));
        self.bridge
            .handle_window_event(masonry_core::core::WindowEvent::Rescale(
                self.window.scale_factor(),
            ));
        self.bridge
            .handle_window_event(masonry_core::core::WindowEvent::Resize(self.size));
        if size.width > 0 && size.height > 0 {
            self.renderer.gpu().quality([size.width, size.height])?;
            self.config.width = size.width;
            self.config.height = size.height;
            if let Some(surface) = &self.surface {
                surface.configure(&self.renderer.gpu().device, &self.config);
            }
        }
        self.dirty = true;
        self.window.request_redraw();
        Ok(())
    }
    pub(crate) fn render(&mut self) -> Result<(), Error> {
        self.renderer.poll()?;
        if self.size.width == 0 || self.size.height == 0 {
            self.dirty = false;
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
                self.dirty = true;
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        self.frame = self
            .frame
            .checked_add(1)
            .ok_or("frame identity exhausted")?;
        let source = self
            .bridge
            .record(FrameId(self.frame))
            .map_err(|e| format!("record: {e:?}"))?;
        let snapshot = self
            .registry
            .freeze(&self.resources, &self.pool)
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
            BuildLimits {
                max_fragments: 1024,
                max_commands: 4096,
                max_masks: 64,
                max_depth: 32,
                max_bytes: 8 << 20,
                max_total_units: 4_000_000,
            },
            &self.pool,
        )
        .map_err(|e| format!("build: {e:?}"))?;
        while !build
            .step(StepBudget {
                max_commands: 64,
                max_units: 8192,
                max_bytes: 64 << 10,
            })
            .map_err(|e| format!("compile: {e:?}"))?
            .complete
        {}
        let frame = build.finish().map_err(|e| format!("finish: {e:?}"))?;
        let mut prepared = match self.renderer.prepare(&frame) {
            Ok(p) => p,
            Err(GpuError::Busy) => {
                self.dirty = true;
                return Ok(());
            }
            Err(
                error @ (GpuError::Budget { .. }
                | GpuError::Unsupported { .. }
                | GpuError::Geometry { .. }),
            ) => {
                eprintln!("frame refused; keeping last accepted view: {error}");
                self.dirty = false;
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
        let notice = presentation_notice(
            &frame,
            SurfaceEpoch(self.epoch),
            Ok(PresentOutcome::PresentAccepted(frame.frame_id())),
        );
        let accepted = self.bridge.resolve(notice);
        if std::env::var_os("REEL_DIAGNOSTIC_TRACE").is_some() {
            eprintln!(
                "present frame={} epoch={} result={accepted:?}",
                self.frame, self.epoch
            );
        }
        self.dirty = false;
        Ok(())
    }
    pub(crate) fn recover_device(&mut self) -> Result<(), Error> {
        self.pointer_event(PointerKind::Cancel);
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
            GpuLimits::default(),
        ))?;
        self.output = SurfacePass::new(&gpu.device, self.config.format)?;
        self.renderer = Renderer::new(gpu);
        self.resize(self.window.inner_size())?;
        eprintln!(
            "device recovered generation={generation}, epoch={}",
            self.epoch
        );
        Ok(())
    }
    pub(crate) fn suspend(&mut self) {
        self.pointer_event(PointerKind::Cancel);
        self.bridge.clear();
        let _ = self.renderer.gpu().device.poll(wgpu::PollType::Wait);
        let _ = self.renderer.poll();
        self.surface = None;
        self.dirty = false;
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
    pub(crate) fn pointer_event(&mut self, kind: PointerKind) {
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
        self.bridge.handle_pointer_event(event);
        self.dirty = true;
        self.window.request_redraw();
    }
}
pub(crate) enum PointerKind {
    Move,
    Down,
    Up,
    Cancel,
}

impl Drop for State {
    fn drop(&mut self) {
        self.bridge.clear();
        let _ = self.renderer.gpu().device.poll(wgpu::PollType::Wait);
        let _ = self.renderer.poll();
        self.surface = None;
    }
}
