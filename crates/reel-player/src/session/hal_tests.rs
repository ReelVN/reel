use super::tests::{mount, mouse, session};
use crate::{
    aside_session::AsideSessionUi, platform::PlatformHost, presentation::Presentation,
    story_runtime::StoryRuntime,
};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    clock::MonoTime,
    identity::ResourceVersion,
    result::Rejected,
};
use reel_engine::aside::{SliceBudget, VmLimits};
use reel_render::*;
use reel_ui::{
    frame::*,
    resources::{ColorEncoding, ResourceSnapshot},
};

#[derive(Default)]
struct Host {
    presents: usize,
    redraws: usize,
    deadline: Option<MonoTime>,
    exited: bool,
}
impl PlatformHost for Host {
    fn request_redraw(&mut self) {
        self.redraws += 1;
    }
    fn before_present(&mut self) {
        self.presents += 1;
    }
    fn set_wake_deadline(&mut self, when: Option<MonoTime>) {
        self.deadline = when;
    }
    fn request_exit(&mut self) {
        self.exited = true;
    }
}
struct Backend {
    inner: RecordingBackend,
    refusal: Option<RenderError>,
    polls: usize,
}
impl RenderBackend for Backend {
    type Error = RenderError;
    fn capabilities(&self) -> RenderCaps {
        self.inner.capabilities()
    }
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), RenderError> {
        self.inner.configure(info)
    }
    fn upload(&mut self, batch: UploadBatch) -> Result<UploadId, Rejected<UploadBatch>> {
        self.inner.upload(batch)
    }
    fn render_present(
        &mut self,
        frame: &RenderFrame,
        before: impl FnOnce(),
    ) -> Result<PresentOutcome, RenderError> {
        if let Some(error) = self.refusal.take() {
            return Err(error);
        }
        self.inner.render_present(frame, before)
    }
    fn retire(&mut self, resources: &[ResourceVersion]) {
        self.inner.retire(resources);
    }
    fn poll(&mut self, out: &mut Vec<RenderEvent>, limit: usize) -> Result<(), RenderError> {
        self.polls += 1;
        self.inner.poll(out, limit)
    }
    fn has_pending(&self) -> bool {
        self.inner.has_pending()
    }
    fn suspend(&mut self) {
        self.inner.suspend();
    }
}
fn settle(story: &mut StoryRuntime, ns: u64) {
    for _ in 0..10_000 {
        story.tick(SliceBudget::default(), || ns).unwrap();
        if !story.needs_tick() || story.tick_interval().as_millis() == 16 {
            return;
        }
    }
    panic!("story work did not settle");
}

#[test]
fn portable_host_runs_story_with_real_frames_input_refusal_suspend_and_close() {
    let driver = AsideSessionUi::from_source(
        session(),
        r#"fn main() {
        stage.replace(button(width: 180, height: 60, on_click: |event| {
            event.target.x += 10;
            await run(tween(event.target, y: 100, duration: 1));
            event.target.enabled = false;
        }));
    }"#,
        "hal_story",
        1,
        "main",
        "en",
        VmLimits::default(),
    )
    .unwrap();
    let mut story = StoryRuntime::new(driver);
    settle(&mut story, 0);
    let (bridge, root) = mount(story.driver.ui());
    let info = SurfaceInfo {
        physical_size: [640, 420],
        scale_factor: 1.,
        surface_epoch: reel_core::identity::SurfaceEpoch(1),
        color_encoding: ColorEncoding::Srgb,
        alpha_mode: AlphaMode::Opaque,
    };
    let mut view = Presentation::new(bridge, info);
    let mut host = Host::default();
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
    };
    backend.configure(info).unwrap();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: u32::MAX,
            max_bytes: 32 << 20,
        },
    );
    let mut events = Vec::new();
    view.poll(&mut backend, &mut events, 8).unwrap();
    assert_eq!(backend.polls, 0, "idle must not poll the device");
    let render = |story: &mut StoryRuntime,
                  view: &mut Presentation,
                  backend: &mut Backend,
                  host: &mut Host| {
        story.driver.sync(&mut view.bridge, root).unwrap();
        let mut stamp = None;
        let result = view
            .render(backend, host, |bridge, id, info| {
                let source = bridge.record(id).unwrap();
                stamp = Some(story.driver.projection_stamp());
                let mut build = FrameBuild::new(
                    source,
                    ResourceSnapshot::capture(std::iter::empty(), &pool).unwrap(),
                    FrameInfo {
                        logical_size: [640., 420.],
                        physical_size: info.physical_size,
                    },
                    BuildLimits { max_bytes: 8 << 20 },
                    &pool,
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
                Ok(build.finish().unwrap())
            })
            .unwrap();
        if result.accepted {
            story.driver.presented(stamp.unwrap()).unwrap();
        }
        result
    };
    let click = |story: &mut StoryRuntime, view: &mut Presentation| {
        for kind in ["move", "down", "up"] {
            story
                .driver
                .handle_pointer(&mut view.bridge, mouse(kind, (50., 30.)))
                .unwrap();
        }
        story.wake_input();
        settle(story, 0);
        story.driver.sync(&mut view.bridge, root).unwrap();
    };
    click(&mut story, &mut view);
    assert_eq!(
        story
            .driver
            .adapter()
            .stage()
            .nodes()
            .next()
            .unwrap()
            .props
            .x,
        0.
    );
    assert!(render(&mut story, &mut view, &mut backend, &mut host).accepted);
    let accepted = view.bridge.accepted_key().unwrap().frame_id();
    click(&mut story, &mut view);
    assert_eq!(
        story
            .driver
            .adapter()
            .stage()
            .nodes()
            .next()
            .unwrap()
            .props
            .x,
        10.
    );
    settle(&mut story, 250_000_000);
    story.driver.sync(&mut view.bridge, root).unwrap();
    assert_eq!(
        story
            .driver
            .adapter()
            .stage()
            .nodes()
            .next()
            .unwrap()
            .props
            .y,
        25.
    );
    view.invalidate();
    assert!(
        render(&mut story, &mut view, &mut backend, &mut host)
            .refused
            .is_some()
    );
    assert_eq!(view.bridge.pending_count(), 0);
    assert_eq!(view.bridge.accepted_key().unwrap().frame_id(), accepted);
    for _ in 0..32 {
        view.exposed();
        view.render(&mut backend, &mut host, |_, _, _| {
            panic!("pressure cannot rebuild before completion")
        })
        .unwrap();
    }
    assert!(
        backend
            .inner
            .complete_render(backend.inner.last_submission().unwrap())
    );
    view.poll(&mut backend, &mut events, 0).unwrap();
    assert!(events.is_empty());
    assert!(!view.needs_redraw());
    view.poll(&mut backend, &mut events, 1).unwrap();
    assert_eq!(events.len(), 1);
    assert!(render(&mut story, &mut view, &mut backend, &mut host).accepted);
    assert!(
        backend
            .inner
            .complete_render(backend.inner.last_submission().unwrap())
    );
    events.clear();
    view.poll(&mut backend, &mut events, 1).unwrap();
    let accepted = view.bridge.accepted_key().unwrap().frame_id();
    view.invalidate();
    backend.refusal = Some(RenderError::ResourcesUnavailable);
    assert!(
        render(&mut story, &mut view, &mut backend, &mut host)
            .refused
            .is_some()
    );
    let presents = host.presents;
    view.render(&mut backend, &mut host, |_, _, _| {
        panic!("repaint must reuse accepted frame")
    })
    .unwrap();
    assert_eq!(host.presents, presents + 1);
    assert_eq!(view.bridge.accepted_key().unwrap().frame_id(), accepted);
    assert_eq!(view.bridge.pending_count(), 0);

    story.suspend(250_000_000);
    view.suspend();
    backend.suspend();
    story.driver.clear_presented().unwrap();
    assert!(!story.needs_tick());
    story
        .tick(SliceBudget::default(), || panic!("suspended host sleeps"))
        .unwrap();
    assert!(view.bridge.accepted_key().is_none());
    let resumed = SurfaceInfo {
        surface_epoch: reel_core::identity::SurfaceEpoch(2),
        ..info
    };
    assert!(view.replace_surface(resumed));
    assert!(
        !view.replace_surface(info),
        "stale lifecycle event cannot restore input"
    );
    backend.configure(resumed).unwrap();
    story.resume(10_250_000_000);
    settle(&mut story, 10_250_000_000);
    story.driver.sync(&mut view.bridge, root).unwrap();
    assert_eq!(
        story
            .driver
            .adapter()
            .stage()
            .nodes()
            .next()
            .unwrap()
            .props
            .y,
        25.
    );
    assert!(
        backend
            .inner
            .complete_render(backend.inner.last_submission().unwrap())
    );
    events.clear();
    view.poll(&mut backend, &mut events, 1).unwrap();
    assert!(render(&mut story, &mut view, &mut backend, &mut host).accepted);
    settle(&mut story, 11_000_000_000);
    story.driver.sync(&mut view.bridge, root).unwrap();
    let node = story.driver.adapter().stage().nodes().next().unwrap();
    assert_eq!(node.props.y, 100.);
    assert!(!node.props.enabled);

    story.suspend(11_000_000_000);
    view.suspend();
    backend.suspend();
    story.close().unwrap();
    let mut now = 11_000_000_000;
    for _ in 0..1000 {
        if story.close_complete() {
            break;
        }
        let deadline = now + 20_000;
        story
            .tick(
                SliceBudget {
                    deadline_ns: Some(deadline),
                    ..Default::default()
                },
                || {
                    now += 1000;
                    now
                },
            )
            .unwrap();
    }
    assert!(
        story.close_complete(),
        "close drains roots while suspended using real deadlines"
    );
    host.request_exit();
    assert!(host.exited);
    assert!(
        backend
            .inner
            .complete_render(backend.inner.last_submission().unwrap())
    );
    events.clear();
    view.poll(&mut backend, &mut events, 1).unwrap();
    assert_eq!(
        pool.usage().bytes,
        0,
        "last frame and input snapshots retire after completion"
    );
}
