//! Accepted CPU frames using the production stage, asset and text layout path.
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    identity::{FrameId, SurfaceEpoch},
};
use reel_player::{
    aside_session::AsideSessionUi,
    assets::{AssetKind, LoadId},
};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{
        BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
    },
    core::{DefaultProperties, Widget, WidgetId, WindowEvent},
    dpi::PhysicalSize,
    frame::{BuildLimits, FrameBuild, FrameInfo, StepBudget},
    resources::{ResourceData, ResourceLease, ResourceSnapshot},
};
use std::{collections::BTreeMap, sync::Arc};

pub(super) struct Presentation {
    pub bridge: PresentationBridge,
    pub root: WidgetId,
    pool: BudgetPool,
    size: Option<[u32; 2]>,
    fonts: BTreeMap<String, ResourceLease>,
    requests: BTreeMap<String, LoadId>,
    active_fonts: Vec<String>,
    pub frames: u64,
}

impl Presentation {
    pub fn fonts_ready(&self) -> bool {
        self.requests.is_empty()
    }
    pub fn new(driver: &AsideSessionUi) -> Result<Self, String> {
        let widget = driver.widget().prepare();
        let root = widget.id();
        let tree = RenderRoot::new(
            widget,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(640, 420),
                scale_factor: 1.,
                default_font: None,
            },
        );
        let bridge = PresentationBridge::new(
            tree,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .map_err(|error| format!("presentation bridge: {error:?}"))?;
        Ok(Self {
            bridge,
            root,
            pool: BudgetPool::new(
                BudgetCategory::CpuOutput,
                QueueLimits {
                    max_requests: 256,
                    max_bytes: 128 << 20,
                },
            ),
            size: None,
            fonts: BTreeMap::new(),
            requests: BTreeMap::new(),
            active_fonts: Vec::new(),
            frames: 0,
        })
    }

    fn configure(&mut self, driver: &AsideSessionUi) -> Result<bool, String> {
        let settings = driver.adapter().game_settings();
        let size = settings.canvas_size.unwrap_or([640, 420]);
        if self.size != Some(size) {
            self.bridge
                .handle_window_event(WindowEvent::Resize(PhysicalSize::new(size[0], size[1])));
            self.size = Some(size);
        }
        if self.active_fonts == settings.fonts {
            return Ok(true);
        }
        let assets = driver.ui().assets();
        self.requests.retain(|path, id| {
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
            let id = *self
                .requests
                .entry(path.clone())
                .or_insert_with(|| assets.request(path.clone(), AssetKind::Font));
            let Some(font) = assets
                .poll(id)
                .map_err(|error| format!("font {path}: {error}"))?
            else {
                return Ok(false);
            };
            self.requests.remove(path);
            self.fonts.insert(path.clone(), font);
        }
        let data = settings
            .fonts
            .iter()
            .map(|path| match self.fonts[path].data() {
                ResourceData::Font(font) => font.clone(),
                _ => unreachable!("font request returns font resource"),
            })
            .collect::<Vec<_>>();
        if !self.bridge.set_default_fonts(&data) {
            return Err("configured fonts contain no usable font".into());
        }
        self.active_fonts.clone_from(&settings.fonts);
        Ok(true)
    }

    /// Return true only after the current projection was accepted.
    pub fn present(&mut self, driver: &mut AsideSessionUi, frame: u64) -> Result<bool, String> {
        if !self.configure(driver)? {
            return Ok(false);
        }
        driver
            .sync(&mut self.bridge, self.root)
            .map_err(|error| error.to_string())?;
        let Some((source, receipt)) = driver
            .record(&mut self.bridge, self.root, FrameId(frame))
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        let key = source.key();
        let result = (|| {
            let leases = self
                .fonts
                .values()
                .chain(driver.ui().image_resources())
                .chain(driver.transition_resources())
                .cloned()
                .map(Ok)
                .collect::<Vec<_>>();
            let resources = ResourceSnapshot::capture(leases.into_iter(), &self.pool)
                .map_err(|error| format!("frame resources: {error:?}"))?;
            let size = self.size.expect("presentation is configured");
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
            .map_err(|error| format!("frame build: {error:?}"))?;
            build
                .set_transitions(receipt.frame_transitions().to_vec())
                .map_err(|error| format!("frame transitions: {error:?}"))?;
            let mut budget = StepBudget {
                max_commands: 64,
                max_units: 8192,
                max_bytes: 64 << 10,
            };
            loop {
                let progress = build
                    .step(budget)
                    .map_err(|error| format!("frame build: {error:?}"))?;
                if progress.complete {
                    break;
                }
                budget.max_units = budget.max_units.max(progress.minimum_units);
                budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
            }
            build
                .finish()
                .map_err(|error| format!("frame finish: {error:?}"))
        })();
        let drawing = match result {
            Ok(drawing) => drawing,
            Err(error) => {
                self.bridge.resolve(PresentationNotice::new(
                    key,
                    SurfaceEpoch(1),
                    PresentationOutcome::Skipped,
                ));
                return Err(error);
            }
        };
        if self.bridge.resolve(PresentationNotice::new(
            key,
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(frame)),
        )) != NoticeResult::Accepted
        {
            return Err("CPU frame acceptance was rejected".into());
        }
        driver
            .presented_frame(&self.bridge, &receipt, &drawing)
            .map_err(|error| error.to_string())?;
        self.frames += 1;
        Ok(true)
    }
}
