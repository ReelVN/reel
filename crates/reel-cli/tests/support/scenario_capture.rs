//! Opt-in GPU presentation/capture for the existing project scenario host.
//! Uses the production frame compiler, renderer and asynchronous PNG service.
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    identity::{FrameId, SurfaceEpoch},
    wake::WakeSignal,
};
use reel_player::{
    aside_session::AsideSessionUi,
    assets::{AssetKind, LoadId},
    capture::CaptureService,
};
use reel_render::{
    capture::{CaptureBackend, CapturedPixels},
    gpu::{DeviceGeneration, Gpu, GpuCapture, GpuError, GpuLimits, Renderer},
};
use reel_ui::{
    bridge::{NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome},
    core::{WidgetId, WindowEvent},
    dpi::PhysicalSize,
    frame::{BuildLimits, FrameBuild, FrameInfo, RenderFrame, StepBudget},
    resources::{ResourceData, ResourceLease, ResourceSnapshot},
};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Default)]
pub struct CaptureStats {
    pub count: u64,
    pub size: [u32; 2],
    pub center: [u8; 4],
    pub right: [u8; 4],
    pub centers: Vec<[u8; 4]>,
}

// Renderer already implements the offscreen readback. The window adapter's
// CaptureBackend additionally owns a native surface, which this host does not need.
struct Offscreen {
    renderer: Renderer,
    stats: CaptureStats,
    capture_dir: Option<PathBuf>,
}
impl CaptureBackend for Offscreen {
    type Error = GpuError;
    type Capture = GpuCapture;

    fn capture(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        output: &BudgetPool,
        wake: WakeSignal,
    ) -> Result<GpuCapture, GpuError> {
        self.renderer.capture(frame, size, output, wake)
    }

    fn poll_capture(
        &mut self,
        capture: &mut GpuCapture,
    ) -> Result<Option<CapturedPixels>, GpuError> {
        let pixels = self.renderer.poll_capture(capture)?;
        if let Some(pixels) = &pixels {
            let [width, height] = pixels.size;
            let sample = |x: u32| {
                let start = ((height / 2 * width + x) * 4) as usize;
                pixels.rgba.as_slice()[start..start + 4].try_into().unwrap()
            };
            self.stats.count += 1;
            self.stats.size = pixels.size;
            self.stats.center = sample(width / 2);
            self.stats.right = sample(width * 7 / 8);
            self.stats.centers.push(self.stats.center);
            if let Some(directory) = &self.capture_dir {
                let path = directory.join(format!("capture-{:03}.png", self.stats.count));
                image::save_buffer(
                    &path,
                    pixels.rgba.as_slice(),
                    width,
                    height,
                    image::ColorType::Rgba8,
                )
                .unwrap_or_else(|error| panic!("scenario capture {}: {error}", path.display()));
                eprintln!("scenario capture PNG: {}", path.display());
            }
        }
        Ok(pixels)
    }
}

pub struct ScenarioCapture {
    backend: Option<Offscreen>,
    captures: CaptureService<GpuCapture>,
    pool: BudgetPool,
    accepted: Option<RenderFrame>,
    size: Option<[u32; 2]>,
    fonts: BTreeMap<String, ResourceLease>,
    font_requests: BTreeMap<String, LoadId>,
    active_fonts: Vec<String>,
}
impl ScenarioCapture {
    pub fn new(kind: &str) -> Self {
        let capture_dir = std::env::var_os("REEL_SCENARIO_CAPTURE_DIR").map(|directory| {
            let directory = PathBuf::from(directory).join(kind);
            std::fs::create_dir_all(&directory).unwrap_or_else(|error| {
                panic!("scenario capture {}: {error}", directory.display())
            });
            directory
        });
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            DeviceGeneration(1),
            GpuLimits::default(),
        ))
        .expect("REEL_SCENARIO_GPU_CAPTURE requires an available GPU adapter");
        eprintln!("scenario GPU: {:?}", gpu.adapter.get_info());
        Self::assemble(Some(Offscreen {
            renderer: Renderer::new(gpu),
            stats: CaptureStats::default(),
            capture_dir,
        }))
    }
    pub fn cpu() -> Self {
        Self::assemble(None)
    }
    fn assemble(backend: Option<Offscreen>) -> Self {
        Self {
            backend,
            captures: CaptureService::new(128 << 20, WakeSignal::new(|| {})),
            pool: BudgetPool::new(
                BudgetCategory::CpuOutput,
                QueueLimits {
                    max_requests: 256,
                    max_bytes: 128 << 20,
                },
            ),
            accepted: None,
            size: None,
            fonts: BTreeMap::new(),
            font_requests: BTreeMap::new(),
            active_fonts: Vec::new(),
        }
    }

    pub fn stats(&self) -> CaptureStats {
        self.backend
            .as_ref()
            .map_or_else(CaptureStats::default, |backend| backend.stats.clone())
    }

    pub fn new_session(&mut self) {
        self.captures = CaptureService::new(128 << 20, WakeSignal::new(|| {}));
        self.accepted = None;
        self.size = None;
        self.fonts.clear();
        self.font_requests.clear();
        self.active_fonts.clear();
    }

    pub fn drive(&mut self, driver: &mut AsideSessionUi) -> bool {
        let Some(backend) = &mut self.backend else {
            return false;
        };
        backend.renderer.poll().unwrap();
        // Drain prior submissions before starting a readback; the production
        // capture service treats a rejected admission as the request's result.
        if self.captures.busy() || !backend.renderer.has_pending() {
            self.captures
                .drive(backend, driver, self.accepted.as_ref())
                .unwrap()
        } else {
            false
        }
    }

    fn configure(&mut self, driver: &AsideSessionUi, bridge: &mut PresentationBridge) -> bool {
        let settings = driver.adapter().game_settings();
        let size = settings.canvas_size.unwrap_or([640, 420]);
        if self.size != Some(size) {
            bridge.handle_window_event(WindowEvent::Resize(PhysicalSize::new(size[0], size[1])));
            self.size = Some(size);
        }
        if self.active_fonts == settings.fonts {
            return true;
        }
        let assets = driver.ui().assets();
        self.font_requests.retain(|path, id| {
            if settings.fonts.contains(path) {
                true
            } else {
                assets.cancel(*id);
                false
            }
        });
        self.fonts.retain(|path, _| settings.fonts.contains(path));
        for path in &settings.fonts {
            if self.fonts.contains_key(path) {
                continue;
            }
            let request = *self
                .font_requests
                .entry(path.clone())
                .or_insert_with(|| assets.request(path.clone(), AssetKind::Font));
            let Some(font) = assets
                .poll(request)
                .unwrap_or_else(|error| panic!("font {path}: {error}"))
            else {
                return false;
            };
            self.font_requests.remove(path);
            self.fonts.insert(path.clone(), font);
        }
        let data = settings
            .fonts
            .iter()
            .map(|path| {
                let ResourceData::Font(font) = self.fonts[path].data() else {
                    unreachable!()
                };
                font.clone()
            })
            .collect::<Vec<_>>();
        assert!(
            bridge.set_default_fonts(&data),
            "configured fonts contain no usable font"
        );
        self.active_fonts.clone_from(&settings.fonts);
        true
    }

    pub fn present(
        &mut self,
        driver: &mut AsideSessionUi,
        bridge: &mut PresentationBridge,
        root: WidgetId,
        frame: u64,
    ) {
        if self.captures.busy() || !self.configure(driver, bridge) {
            return;
        }
        driver.sync(bridge, root).unwrap();
        let Some((source, receipt)) = driver.record(bridge, root, FrameId(frame)).unwrap() else {
            return;
        };
        let key = source.key();
        if self.backend.is_none() && !receipt.needs_transition_drawing() {
            assert_eq!(
                bridge.resolve(PresentationNotice::new(
                    key,
                    SurfaceEpoch(1),
                    PresentationOutcome::Accepted(FrameId(frame)),
                )),
                NoticeResult::Accepted
            );
            driver.presented(bridge, &receipt).unwrap();
            return;
        }
        let leases = self
            .fonts
            .values()
            .chain(driver.ui().image_resources())
            .chain(driver.transition_resources())
            .cloned()
            .map(Ok)
            .collect::<Vec<_>>();
        let resources = ResourceSnapshot::capture(leases.into_iter(), &self.pool).unwrap();
        let size = self.size.unwrap();
        let mut build = FrameBuild::new(
            source,
            resources,
            FrameInfo {
                logical_size: size.map(f64::from),
                physical_size: size,
            },
            BuildLimits { max_bytes: 8 << 20 },
            &self.pool,
        )
        .unwrap();
        build
            .set_transitions(receipt.frame_transitions().to_vec())
            .unwrap();
        let mut budget = StepBudget {
            max_commands: 64,
            max_units: 8192,
            max_bytes: 64 << 10,
        };
        loop {
            let progress = build.step(budget).unwrap();
            if progress.complete {
                break;
            }
            budget.max_units = budget.max_units.max(progress.minimum_units);
            budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
        }
        let candidate = build.finish().unwrap();
        if let Some(backend) = &mut self.backend {
            let prepared = match backend.renderer.prepare(&candidate) {
                Ok(prepared) => prepared,
                Err(error) if error.is_retryable() => {
                    bridge.resolve(PresentationNotice::new(
                        key,
                        SurfaceEpoch(1),
                        PresentationOutcome::Skipped,
                    ));
                    return;
                }
                Err(error) => panic!("scenario GPU frame: {error}"),
            };
            backend.renderer.submit(prepared).unwrap();
        }
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                key,
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(frame)),
            )),
            NoticeResult::Accepted
        );
        driver
            .presented_frame(
                bridge,
                &receipt,
                &candidate,
                driver.adapter().logical_time(),
            )
            .unwrap();
        self.accepted = Some(candidate);
    }
}
