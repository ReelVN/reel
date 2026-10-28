use super::*;
use crate::{presentation::PresentationResult, session::RecordedProjection};
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_ui::core::WidgetId;

mod redraw_wake;

struct Fixture {
    story: StoryRuntime,
    view: Presentation,
    backend: Backend,
    host: Host,
    root: WidgetId,
    pool: BudgetPool,
    receipt: Option<RecordedProjection>,
    builds: usize,
}

impl Fixture {
    fn new() -> Self {
        let driver = AsideSessionUi::from_source(
            session(),
            r#"fn main() {
                let node = button(width: 80, height: 40);
                stage.replace(node);
                await stage.presented();
                await run(tween(node, x: 200, duration: 1));
            }"#,
            "preparation",
            1,
            "main",
            "en",
            VmLimits::default(),
        )
        .unwrap();
        let mut story = StoryRuntime::new(driver);
        settle(&mut story, 0);
        let (bridge, root) = mount(story.driver().ui());
        let info = SurfaceInfo {
            physical_size: [640, 420],
            scale_factor: 1.,
            surface_epoch: SurfaceEpoch(1),
            color_encoding: ColorEncoding::Srgb,
            alpha_mode: AlphaMode::Opaque,
        };
        let mut backend = Backend {
            inner: RecordingBackend::new(ConsumerLimits {
                max_uploads: 1,
                max_resources: 1,
                max_frames: 1,
                max_events: 4,
                max_upload_bytes: 1024,
                max_retained_bytes: 1024,
                max_metadata_bytes: 8192,
                max_fixed_bytes: 16384,
            })
            .unwrap(),
            refusal: None,
            polls: 0,
            resources_available: false,
            configurations: Vec::new(),
            preparing: false,
            frames: Vec::new(),
            cancelled: Vec::new(),
        };
        backend.configure(info).unwrap();
        Self {
            story,
            root,
            backend,
            view: Presentation::new(bridge, info),
            host: Host::default(),
            receipt: None,
            builds: 0,
            pool: BudgetPool::new(
                BudgetCategory::CpuOutput,
                QueueLimits {
                    max_requests: u32::MAX,
                    max_bytes: 32 << 20,
                },
            ),
        }
    }

    fn sync(&mut self, ns: u64) {
        self.host.now = MonoTime(ns);
        settle(&mut self.story, ns);
        self.story
            .driver_mut()
            .unwrap()
            .sync(&mut self.view.bridge, self.root)
            .unwrap();
        self.view.invalidate();
    }

    fn render(&mut self) -> PresentationResult {
        let result = self
            .view
            .render(&mut self.backend, &mut self.host, |bridge, id, info| {
                self.builds += 1;
                let Some((source, receipt)) = self.story.driver().record(bridge, self.root, id)?
                else {
                    return Ok(None);
                };
                self.receipt = Some(receipt);
                let mut build = FrameBuild::new(
                    source,
                    ResourceSnapshot::capture(std::iter::empty(), &self.pool).unwrap(),
                    FrameInfo {
                        logical_size: [640., 420.],
                        physical_size: info.physical_size,
                    },
                    BuildLimits { max_bytes: 8 << 20 },
                    &self.pool,
                )
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
                Ok(Some(build.finish().unwrap()))
            })
            .unwrap();
        if result.accepted {
            self.story
                .presented_frame(
                    &self.view.bridge,
                    self.receipt.as_ref().unwrap(),
                    self.view.accepted_frame.as_ref().unwrap(),
                    self.host.now.0,
                )
                .unwrap();
        }
        result
    }

    fn complete(&mut self) {
        if let Some(id) = self.backend.inner.last_submission() {
            self.backend.inner.complete_render(id);
        }
        self.backend.resources_available = true;
        self.view
            .poll(&mut self.backend, &mut Vec::new(), 8)
            .unwrap();
    }
}

#[test]
fn gpu_preparation_keeps_candidate_until_completion_then_publishes_newer_model() {
    let mut f = Fixture::new();
    f.sync(0);
    assert!(f.render().accepted);
    f.complete();
    let first = f.view.bridge.accepted_key().unwrap().clone();
    f.sync(0);
    f.sync(250_000_000);
    assert_eq!(
        f.story
            .driver()
            .adapter()
            .stage()
            .nodes()
            .next()
            .unwrap()
            .props
            .x,
        50.
    );
    f.backend.preparing = true;
    assert!(!f.render().accepted);
    let prepared = f.receipt.as_ref().unwrap().key();
    assert_ne!(prepared.frame_id(), first.frame_id());
    assert_eq!(f.view.bridge.accepted_key(), Some(&first));
    assert_eq!(f.view.bridge.pending_count(), 1);
    assert!(f.view.is_preparing());
    let builds = f.builds;
    for ns in [500_000_000, 750_000_000] {
        f.sync(ns);
        for _ in 0..8 {
            f.view.exposed();
            assert!(!f.view.needs_redraw());
            assert!(!f.render().accepted);
        }
    }
    assert_eq!(f.builds, builds);
    assert_eq!(
        f.story
            .driver()
            .adapter()
            .stage()
            .nodes()
            .next()
            .unwrap()
            .props
            .x,
        150.
    );
    f.complete();
    f.backend.refusal = Some(RenderError::Busy);
    f.backend.resources_available = true;
    let pressured = f.render();
    assert!(!pressured.accepted && pressured.refused.is_none());
    assert!(f.view.is_preparing());
    assert_eq!(f.view.bridge.pending_count(), 1);
    f.complete();
    assert!(f.render().accepted);
    assert_eq!(f.view.bridge.accepted_key(), Some(&prepared));
    assert_eq!(f.builds, builds);
    assert!(!f.view.is_preparing());
    assert!(
        f.view.needs_redraw(),
        "pending model change survives acceptance"
    );
    assert_eq!(&f.backend.frames[1..], &[prepared.frame_id(); 3]);
    f.complete();
    assert!(f.render().accepted);
    assert!(f.view.bridge.accepted_key().unwrap().frame_id() > prepared.frame_id());
    assert_eq!(f.builds, builds + 1);
    assert_eq!(f.view.bridge.pending_count(), 0);
    assert!(f.backend.cancelled.is_empty());
}

#[test]
fn busy_frame_is_built_once_and_new_animation_state_follows_its_acceptance() {
    let mut f = Fixture::new();
    f.sync(0);
    assert!(f.render().accepted);
    // Leave the accepted submission in flight so the one-slot backend returns Busy.
    let accepted = f.view.bridge.accepted_key().unwrap().clone();
    f.sync(0);
    f.sync(250_000_000);
    let before = f.builds;
    let refused = f.render();
    assert!(!refused.accepted && refused.refused.is_none());
    let candidate = f.receipt.as_ref().unwrap().key();
    assert!(f.view.is_preparing());
    assert_eq!(f.view.bridge.accepted_key(), Some(&accepted));
    assert_eq!(f.view.bridge.pending_count(), 1);
    assert_eq!(f.builds, before + 1);
    for ns in [500_000_000, 750_000_000] {
        f.sync(ns);
        for _ in 0..8 {
            assert!(!f.view.needs_redraw());
            assert!(!f.render().accepted);
        }
    }
    assert_eq!(
        f.builds,
        before + 1,
        "busy polls must not rebuild the candidate"
    );
    assert_eq!(
        f.story
            .driver()
            .adapter()
            .stage()
            .nodes()
            .next()
            .unwrap()
            .props
            .x,
        150.
    );
    f.complete();
    assert!(f.render().accepted);
    assert_eq!(f.view.bridge.accepted_key(), Some(&candidate));
    assert_eq!(f.builds, before + 1, "completion resumes the same frame");
    assert!(f.view.needs_redraw(), "later animation must remain pending");
    f.complete();
    assert!(f.render().accepted);
    assert_eq!(f.builds, before + 2);
    assert!(f.view.bridge.accepted_key().unwrap().frame_id() > candidate.frame_id());
    assert_eq!(f.view.bridge.pending_count(), 0);
    eprintln!("busy candidate: 16 waits, 1 initial build, 0 retry builds, 1 updated build");
}

#[test]
fn busy_without_a_pending_completion_reports_refusal_and_retires_candidate() {
    let mut f = Fixture::new();
    f.sync(0);
    assert!(f.render().accepted);
    f.complete();
    let accepted = f.view.bridge.accepted_key().unwrap().clone();
    f.sync(250_000_000);
    f.backend.refusal = Some(RenderError::Busy);
    let result = f.render();
    assert!(result.refused.is_some());
    assert!(!f.view.is_preparing());
    assert_eq!(f.view.bridge.pending_count(), 0);
    assert_eq!(f.view.bridge.accepted_key(), Some(&accepted));
}
#[test]
fn busy_candidate_cancelled_on_surface_change_cannot_publish_geometry() {
    let mut f = Fixture::new();
    f.sync(0);
    assert!(f.render().accepted);
    f.sync(250_000_000);
    assert!(!f.render().accepted);
    assert!(f.view.is_preparing());
    let candidate = f.receipt.as_ref().unwrap().key();
    f.view.cancel_preparation(&mut f.backend);
    let info = SurfaceInfo {
        surface_epoch: SurfaceEpoch(2),
        ..f.view.surface_info()
    };
    assert!(f.view.replace_surface(info));
    f.backend.configure(info).unwrap();
    f.complete();
    assert_eq!(f.backend.cancelled, [candidate.frame_id()]);
    assert_eq!(f.view.bridge.pending_count(), 0);
    assert_eq!(
        f.view
            .bridge
            .resolve(reel_ui::bridge::PresentationNotice::new(
                candidate.clone(),
                candidate.surface_epoch(),
                reel_ui::bridge::PresentationOutcome::Accepted(candidate.frame_id()),
            )),
        reel_ui::bridge::NoticeResult::StaleOrForeign
    );
    assert!(f.view.bridge.accepted_key().is_none());
}
#[test]
fn preparing_accepted_repaint_waits_for_gpu_despite_new_model_changes() {
    let mut f = Fixture::new();
    f.sync(0);
    assert!(f.render().accepted);
    f.complete();
    let accepted = f.view.bridge.accepted_key().unwrap().clone();
    f.sync(0);
    f.backend.refusal = Some(RenderError::ResourcesUnavailable);
    assert!(f.render().refused.is_some());
    assert_eq!(f.view.bridge.pending_count(), 0);
    f.backend.preparing = true;
    assert!(!f.render().accepted);
    assert!(f.view.is_preparing());
    assert_eq!(f.backend.frames.last(), Some(&accepted.frame_id()));
    let attempts = f.backend.frames.len();
    f.sync(750_000_000);
    f.view.exposed();
    assert!(!f.view.needs_redraw());
    assert!(!f.render().accepted);
    assert_eq!(f.backend.frames.len(), attempts);
    f.complete();
    assert!(
        !f.render().accepted,
        "repainting cannot accept new input geometry"
    );
    assert!(!f.view.is_preparing());
    assert_eq!(f.view.bridge.accepted_key(), Some(&accepted));
    assert!(f.view.needs_redraw());
    f.complete();
    assert!(f.render().accepted);
    assert!(f.view.bridge.accepted_key().unwrap().frame_id() > accepted.frame_id());
}

#[test]
fn cancelled_gpu_preparation_cannot_publish_input_after_surface_change_or_suspend() {
    for suspend in [false, true] {
        let mut f = Fixture::new();
        f.sync(0);
        f.backend.preparing = true;
        assert!(!f.render().accepted);
        let candidate = f.receipt.as_ref().unwrap().key();
        f.view.cancel_preparation(&mut f.backend);
        assert_eq!(f.backend.cancelled, [candidate.frame_id()]);
        assert_eq!(f.view.bridge.pending_count(), 0);
        assert!(!f.view.is_preparing());
        if suspend {
            f.view.suspend();
            f.backend.suspend();
        } else {
            let info = SurfaceInfo {
                surface_epoch: SurfaceEpoch(2),
                ..f.view.surface_info()
            };
            assert!(f.view.replace_surface(info));
            f.backend.configure(info).unwrap();
        }
        f.complete();
        assert_eq!(
            f.view
                .bridge
                .resolve(reel_ui::bridge::PresentationNotice::new(
                    candidate.clone(),
                    candidate.surface_epoch(),
                    reel_ui::bridge::PresentationOutcome::Accepted(candidate.frame_id()),
                )),
            reel_ui::bridge::NoticeResult::StaleOrForeign
        );
        assert!(f.view.bridge.accepted_key().is_none());
        assert_eq!(f.host.presents, 0);
        if suspend {
            assert!(!f.view.needs_redraw());
        } else {
            assert!(f.render().accepted);
            assert_eq!(
                f.view.bridge.accepted_key().unwrap().surface_epoch(),
                SurfaceEpoch(2)
            );
            assert_eq!(f.backend.frames, [FrameId(1), FrameId(2)]);
        }
    }
}

#[derive(Debug)]
enum PixelTestError {
    Missing(PixelDemand),
    Other(RenderError),
}
impl std::fmt::Display for PixelTestError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "{self:?}")
    }
}
impl std::error::Error for PixelTestError {}
impl BackendError for PixelTestError {
    fn kind(&self) -> FailureKind {
        match self {
            Self::Other(error) => error.kind(),
            Self::Missing(_) => FailureKind::Refused,
        }
    }
    fn into_pixel_demand(self) -> Result<PixelDemand, Self> {
        match self {
            Self::Missing(demand) => Ok(demand),
            error => Err(error),
        }
    }
}
struct PixelBackend<'a> {
    inner: &'a mut Backend,
    demand: Option<PixelDemand>,
    busy_once: bool,
}
impl RenderBackend for PixelBackend<'_> {
    type Error = PixelTestError;
    fn capabilities(&self) -> RenderCaps {
        self.inner.capabilities()
    }
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), Self::Error> {
        self.inner.configure(info).map_err(PixelTestError::Other)
    }
    fn upload(&mut self, batch: UploadBatch) -> Result<UploadId, Rejected<UploadBatch>> {
        self.inner.upload(batch)
    }
    fn render_present(
        &mut self,
        frame: &RenderFrame,
        before: impl FnOnce(),
    ) -> Result<PresentOutcome, Self::Error> {
        if std::mem::take(&mut self.busy_once) {
            return Err(PixelTestError::Other(RenderError::Busy));
        }
        if let Some(demand) = self.demand.take() {
            return Err(PixelTestError::Missing(demand));
        }
        before();
        Ok(PresentOutcome::PresentAccepted(frame.frame_id()))
    }
    fn cancel_preparation(&mut self, frame: &RenderFrame) {
        self.inner.cancel_preparation(frame);
    }
    fn retire(&mut self, versions: &[ResourceVersion]) {
        self.inner.retire(versions);
    }
    fn poll(
        &mut self,
        out: &mut Vec<RenderEvent<Self::Error>>,
        limit: usize,
    ) -> Result<(), Self::Error> {
        if limit != 0 && std::mem::take(&mut self.inner.resources_available) {
            out.push(RenderEvent::ResourcesAvailable);
        }
        Ok(())
    }
    fn has_pending(&self) -> bool {
        self.inner.has_pending()
    }
    fn suspend(&mut self) {
        self.inner.suspend();
    }
}

#[test]
fn pixel_demand_retains_the_exact_candidate_and_accepted_geometry() {
    use reel_core::identity::{AssetKey, ContentVersion};
    use reel_ui::{
        peniko::{ImageAlphaType, ImageFormat},
        resources::{ResourceLease, ResourceRegistry},
    };
    let mut f = Fixture::new();
    f.sync(0);
    assert!(f.render().accepted);
    let accepted = f.view.bridge.accepted_key().unwrap().clone();
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let h = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("restore-test".into()),
                version: ContentVersion([1; 32]),
            },
            OwnedBytes::new(vec![255; 4], f.pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let source = registry.acquire(h).unwrap();
    source.permit_pixel_reload();
    assert!(source.discard_uploaded_pixels());
    let resources = vec![source.clone()];
    let ticket = f
        .pool
        .reserve((resources.capacity() * size_of::<ResourceLease>()) as u64)
        .unwrap();
    let demand = PixelDemand::new(resources, ticket).unwrap();
    let mut backend = PixelBackend {
        inner: &mut f.backend,
        demand: Some(demand),
        busy_once: true,
    };
    f.view.invalidate();
    let pending = f
        .view
        .render(&mut backend, &mut f.host, |bridge, id, info| {
            let recorded = bridge.record(id).unwrap();
            let mut build = FrameBuild::new(
                recorded,
                ResourceSnapshot::capture(std::iter::once(Ok(source.clone())), &f.pool).unwrap(),
                FrameInfo {
                    logical_size: [640., 420.],
                    physical_size: info.physical_size,
                },
                BuildLimits { max_bytes: 8 << 20 },
                &f.pool,
            )
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
            Ok(Some(build.finish().unwrap()))
        })
        .unwrap();
    assert!(pending.pixels.is_none());
    let key = f.view.preparing_key().unwrap();
    assert_eq!(f.view.bridge.accepted_key(), Some(&accepted));
    if let Some(id) = backend.inner.inner.last_submission() {
        backend.inner.inner.complete_render(id);
    }
    backend.inner.resources_available = true;
    f.view.poll(&mut backend, &mut Vec::new(), 8).unwrap();
    let missing = f
        .view
        .render(&mut backend, &mut f.host, |_, _, _| {
            panic!("busy candidate must not be rebuilt before pixel demand")
        })
        .unwrap();
    let (pixel_key, demand) = missing.pixels.unwrap();
    assert_eq!(pixel_key, key);
    assert_eq!(demand.resources()[0].handle(), h);
    assert_eq!(f.view.bridge.accepted_key(), Some(&accepted));
    assert!(!f.view.needs_redraw());
    drop(demand);
    assert!(f.view.pixels_ready(&key));
    let result = f
        .view
        .render(&mut backend, &mut f.host, |_, _, _| {
            panic!("candidate must not be rebuilt")
        })
        .unwrap();
    assert!(result.accepted);
    assert_eq!(f.view.bridge.accepted_key(), Some(&key));
}
