use super::*;
use crate::{presentation::PresentationResult, session::RecordedProjection};
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_ui::core::WidgetId;

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
                .driver_mut()
                .unwrap()
                .presented(&self.view.bridge, self.receipt.as_ref().unwrap())
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
    f.complete();
    f.backend.refusal = Some(RenderError::Busy);
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
