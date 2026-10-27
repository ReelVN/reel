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
    gpu::{DeviceGeneration, Gpu, GpuCapture, GpuError, GpuLimits, Quality, Renderer},
};
use reel_ui::{
    bridge::{NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome},
    core::{DefaultProperties, Widget, WidgetId, WindowEvent},
    dpi::PhysicalSize,
    frame::{BuildLimits, FrameBuild, FrameInfo, RenderFrame, StepBudget},
    resources::{ResourceData, ResourceLease, ResourceSnapshot},
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

#[derive(Clone, Debug, Default)]
pub struct CaptureStats {
    pub count: u64,
    pub size: [u32; 2],
    pub center: [u8; 4],
    pub right: [u8; 4],
    pub centers: Vec<[u8; 4]>,
}

// Command structure and scalar domains only; resource leases and image payloads
// never enter this diagnostic artifact.
fn dump_topology(frame: &RenderFrame, error: &GpuError, title: &str) {
    use reel_ui::frame::FrameOp;
    use std::io::Write;
    let Some(directory) = std::env::var_os("REEL_SCENARIO_FAILURE_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("rejected-topology.txt");
    let mut output = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    writeln!(
        output,
        "title={title:?} error={error:?} frame={:?}",
        frame.info()
    )
    .unwrap();
    let mut pending = vec![("current".to_string(), frame.drawing())];
    let mut seen = std::collections::HashSet::new();
    while let Some((name, drawing)) = pending.pop() {
        if !seen.insert(std::ptr::from_ref(drawing)) {
            continue;
        }
        let effect = |effect: &reel_ui::frame::FrameEffect| {
            let output = drawing.resources().leases().iter().find_map(|lease| {
                if lease.handle() == effect.shader
                    && let ResourceData::EffectShader { descriptor, .. } = lease.data()
                {
                    Some(descriptor.output)
                } else {
                    None
                }
            });
            (
                output,
                effect.definition,
                effect.bounds,
                effect.shader,
                effect.coverage_invert,
            )
        };
        writeln!(
            output,
            "drawing={name} commands={}",
            drawing.commands().len()
        )
        .unwrap();
        for (label, commands) in std::iter::once(("body".to_string(), drawing.commands())).chain(
            drawing
                .masks()
                .iter()
                .map(|(id, mask)| (format!("mask-{id}"), mask.commands.as_slice())),
        ) {
            writeln!(output, "stream={label}").unwrap();
            for (index, command) in commands.iter().enumerate() {
                write!(output, "{index} {:?} ", command.origin.at).unwrap();
                match &command.op {
                    FrameOp::PushGroup(group) => writeln!(output,
                        "PushGroup ink={:?} transform={:?} effect={:?} mask_effect={:?} alpha={} blend={:?} invert={} clip={:?} mask={:?} image_mask={:?} filters={:?}",
                        group.ink_bounds, group.effect_to_target, group.effect.as_ref().map(effect),
                        group.mask_effect.as_ref().map(effect), group.composite.alpha, group.composite.blend,
                        group.invert, group.clip, group.mask, group.image_mask.as_ref().map(|mask| mask.bounds), group.filters),
                    FrameOp::PushClip(clip) => writeln!(output, "PushClip {clip:?}"),
                    FrameOp::PopClip => writeln!(output, "PopClip"),
                    FrameOp::PopGroup => writeln!(output, "PopGroup"),
                    FrameOp::PushContext(_) => writeln!(output, "PushContext"),
                    FrameOp::PopContext => writeln!(output, "PopContext"),
                    FrameOp::Draw(_) => writeln!(output, "Draw"),
                    FrameOp::External(_) => writeln!(output, "External"),
                }.unwrap();
            }
        }
        for (index, transition) in drawing.transitions().iter().enumerate() {
            writeln!(output, "transition {index} commands={:?} progress={} parent_to_window={:?} previous_to_parent={:?}",
                transition.commands, transition.progress, transition.parent_to_window, transition.previous.to_parent()).unwrap();
            pending.push((
                format!("{name}/previous-{index}"),
                transition.previous.drawing(),
            ));
        }
    }
    output.flush().unwrap();
    eprintln!("scenario rejected command topology: {}", path.display());
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
    capture_enabled: bool,
    captures: CaptureService<GpuCapture>,
    pool: BudgetPool,
    accepted: Option<RenderFrame>,
    size: Option<[u32; 2]>,
    fonts: BTreeMap<String, ResourceLease>,
    font_requests: BTreeMap<String, LoadId>,
    active_fonts: Vec<String>,
    physical_size: Option<[u32; 2]>,
    canvas_id: Option<WidgetId>,
    canvas_size: Option<[u32; 2]>,
    accepted_frames: u64,
    rejected_frames: u64,
    last_rejection: Option<String>,
    last_quality: Option<Quality>,
}
impl ScenarioCapture {
    pub fn new(kind: &str, capture: bool) -> Self {
        let capture_dir = capture
            .then(|| std::env::var_os("REEL_SCENARIO_CAPTURE_DIR"))
            .flatten()
            .map(|directory| {
                let directory = PathBuf::from(directory).join(kind);
                std::fs::create_dir_all(&directory).unwrap_or_else(|error| {
                    panic!("scenario capture {}: {error}", directory.display())
                });
                directory
            });
        let mut limits = GpuLimits::default();
        if let Ok(value) = std::env::var("REEL_SCENARIO_TARGET_BYTES") {
            limits.target_bytes = value
                .parse::<u64>()
                .ok()
                .filter(|&value| value != 0)
                .expect("REEL_SCENARIO_TARGET_BYTES must be a positive byte count");
        }
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            DeviceGeneration(1),
            limits,
        ))
        .expect("GPU scenario presentation requires an available GPU adapter");
        eprintln!("scenario GPU: {:?}", gpu.adapter.get_info());
        eprintln!("scenario GPU target budget: {} bytes", limits.target_bytes);
        Self::assemble(
            Some(Offscreen {
                renderer: Renderer::new(gpu),
                stats: CaptureStats::default(),
                capture_dir,
            }),
            capture,
        )
    }
    pub fn cpu() -> Self {
        Self::assemble(None, false)
    }
    fn assemble(backend: Option<Offscreen>, capture_enabled: bool) -> Self {
        Self {
            backend,
            capture_enabled,
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
            physical_size: std::env::var("REEL_SCENARIO_PHYSICAL_SIZE")
                .ok()
                .map(|value| {
                    let (width, height) = value
                        .split_once('x')
                        .expect("REEL_SCENARIO_PHYSICAL_SIZE expects WIDTHxHEIGHT");
                    [width, height].map(|axis| {
                        axis.parse::<u32>()
                            .ok()
                            .filter(|&axis| axis != 0)
                            .expect("scenario physical dimensions must be positive u32 values")
                    })
                }),
            canvas_id: None,
            canvas_size: None,
            accepted_frames: 0,
            rejected_frames: 0,
            last_rejection: None,
            last_quality: None,
        }
    }

    pub fn bridge(&mut self, driver: &AsideSessionUi) -> (PresentationBridge, WidgetId) {
        let Some([width, height]) = self.physical_size else {
            return super::support::bridge(driver);
        };
        use reel_ui::{
            app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
            bridge::BridgeLimits,
            widgets::Canvas,
        };
        let widget = driver.widget().prepare();
        let root_id = widget.id();
        let canvas = Canvas::viewport(widget).prepare();
        self.canvas_id = Some(canvas.id());
        let root = RenderRoot::new(
            canvas,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(width, height),
                scale_factor: 1.,
                default_font: None,
            },
        );
        let bridge = PresentationBridge::new(
            root,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .unwrap();
        (bridge, root_id)
    }

    pub fn finish(&mut self) {
        if let Some(backend) = &mut self.backend {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while backend.renderer.has_pending() {
                backend.renderer.poll().unwrap();
                assert!(
                    std::time::Instant::now() < deadline,
                    "scenario GPU completion stalled"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            eprintln!(
                "scenario presentation: accepted={} rejected={} last_rejection={:?} usage={:?}",
                self.accepted_frames,
                self.rejected_frames,
                self.last_rejection,
                backend.renderer.usage()
            );
        }
    }

    pub fn stats(&self) -> CaptureStats {
        self.backend
            .as_ref()
            .map_or_else(CaptureStats::default, |backend| backend.stats.clone())
    }

    pub fn window_point(
        &self,
        bridge: &PresentationBridge,
        root: WidgetId,
        point: [f64; 2],
    ) -> [f64; 2] {
        let geometry = bridge
            .accepted_key()
            .and_then(|key| bridge.accepted_geometry(key))
            .expect("scenario input requires accepted geometry");
        let transform = geometry
            .get(root)
            .expect("accepted session widget")
            .window_transform;
        let point = transform * reel_ui::kurbo::Point::new(point[0], point[1]);
        [point.x, point.y]
    }

    pub fn new_session(&mut self) {
        self.captures = CaptureService::new(128 << 20, WakeSignal::new(|| {}));
        self.accepted = None;
        self.size = None;
        self.fonts.clear();
        self.font_requests.clear();
        self.active_fonts.clear();
        self.canvas_id = None;
        self.canvas_size = None;
    }

    pub fn drive(&mut self, driver: &mut AsideSessionUi) -> bool {
        let Some(backend) = &mut self.backend else {
            return false;
        };
        backend.renderer.poll().unwrap();
        // Drain prior submissions before starting a readback; the production
        // capture service treats a rejected admission as the request's result.
        if self.capture_enabled && (self.captures.busy() || !backend.renderer.has_pending()) {
            self.captures
                .drive(backend, driver, self.accepted.as_ref())
                .unwrap()
        } else {
            false
        }
    }

    fn configure(&mut self, driver: &AsideSessionUi, bridge: &mut PresentationBridge) -> bool {
        let settings = driver.adapter().game_settings();
        let canvas_size = settings.canvas_size.unwrap_or([640, 420]);
        if self.canvas_size != Some(canvas_size) {
            if let Some(id) = self.canvas_id {
                bridge.edit_widget(id, |mut widget| {
                    reel_ui::widgets::Canvas::set_size(
                        &mut widget.downcast::<reel_ui::widgets::Canvas>(),
                        Some(canvas_size),
                    );
                });
                eprintln!(
                    "scenario canvas={canvas_size:?} physical={:?}",
                    self.physical_size
                );
            }
            self.canvas_size = Some(canvas_size);
        }
        let size = self.physical_size.unwrap_or(canvas_size);
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
                    self.rejected_frames += 1;
                    let rejection = error.to_string();
                    if self.last_rejection.as_ref() != Some(&rejection) {
                        dump_topology(&candidate, &error, &driver.adapter().game_settings().title);
                        eprintln!(
                            "scenario GPU rejected frame={frame} title={:?} size={size:?}: {error}; usage={:?}",
                            driver.adapter().game_settings().title,
                            backend.renderer.usage()
                        );
                        self.last_rejection = Some(rejection);
                    }
                    bridge.resolve(PresentationNotice::new(
                        key,
                        SurfaceEpoch(1),
                        PresentationOutcome::Skipped,
                    ));
                    return;
                }
                Err(error) => {
                    dump_topology(&candidate, &error, &driver.adapter().game_settings().title);
                    panic!(
                        "scenario GPU frame={frame} title={:?} size={size:?}: {error}; usage={:?}",
                        driver.adapter().game_settings().title,
                        backend.renderer.usage()
                    )
                }
            };
            let quality = prepared.quality();
            if self.last_quality != Some(quality) {
                eprintln!(
                    "scenario GPU frame={frame} title={:?} quality={quality:?} usage={:?}",
                    driver.adapter().game_settings().title,
                    backend.renderer.usage()
                );
                self.last_quality = Some(quality);
            }
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
        self.accepted_frames += 1;
    }
}
