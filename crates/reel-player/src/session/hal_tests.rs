use super::tests::{mount, mouse, session};
use crate::{
    aside_session::AsideSessionUi,
    hal::{DisplayMetrics, PlatformHost, WindowControl},
    player::{Player, PlayerOptions},
    presentation::Presentation,
    story_runtime::StoryRuntime,
};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    clock::{Clock, MonoTime, UnixMillis},
    identity::ResourceVersion,
    result::Rejected,
    window::{WindowConfiguration, WindowInfo, WindowSize},
};
use reel_engine::aside::{SliceBudget, VmLimits};
use reel_render::*;
use reel_ui::{
    frame::*,
    resources::{ColorEncoding, ResourceSnapshot},
};
mod preparation;
mod redraw;

struct Host {
    presents: usize,
    redraws: usize,
    deadline: Option<MonoTime>,
    exited: bool,
    metrics: DisplayMetrics,
    now: MonoTime,
    visible: bool,
    title: String,
}
impl Default for Host {
    fn default() -> Self {
        Self {
            presents: 0,
            redraws: 0,
            deadline: None,
            exited: false,
            metrics: DisplayMetrics {
                physical_size: [640, 420],
                scale_factor: 1.,
            },
            now: MonoTime(0),
            visible: false,
            title: String::new(),
        }
    }
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
impl Clock for Host {
    fn monotonic_now(&self) -> MonoTime {
        self.now
    }
    fn wall_time(&self) -> Option<UnixMillis> {
        None
    }
}
impl WindowControl for Host {
    fn window_info(&mut self) -> WindowInfo {
        let size = WindowSize {
            width: f64::from(self.metrics.physical_size[0]) / self.metrics.scale_factor,
            height: f64::from(self.metrics.physical_size[1]) / self.metrics.scale_factor,
        };
        WindowInfo {
            size,
            windowed_size: size,
            scale_factor: self.metrics.scale_factor,
            ..Default::default()
        }
    }
    fn configure_window(&mut self, _: WindowConfiguration) -> Result<bool, String> {
        Err("display configuration is unsupported by this test host".into())
    }
    fn window_resized(&mut self) -> bool {
        true
    }
    fn move_pointer(&mut self, _: [f64; 2]) -> Result<bool, String> {
        Ok(false)
    }
}
impl crate::hal::Host for Host {
    fn display_metrics(&self) -> DisplayMetrics {
        self.metrics
    }
    fn set_title(&mut self, title: &str) {
        self.title = title.into();
    }
    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }
    fn wake_callback(&self) -> std::sync::Arc<dyn Fn() + Send + Sync> {
        std::sync::Arc::new(|| {})
    }
    fn ui_signal_handler(&self) -> impl FnMut(reel_ui::app::RenderRootSignal) + 'static {
        |_| {}
    }
}
struct Backend {
    inner: RecordingBackend,
    refusal: Option<RenderError>,
    polls: usize,
    resources_available: bool,
    configurations: Vec<SurfaceInfo>,
    preparing: bool,
    frames: Vec<reel_core::identity::FrameId>,
    cancelled: Vec<reel_core::identity::FrameId>,
}
impl RenderBackend for Backend {
    type Error = RenderError;
    fn capabilities(&self) -> RenderCaps {
        self.inner.capabilities()
    }
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), RenderError> {
        self.inner.configure(info)?;
        self.configurations.push(info);
        Ok(())
    }
    fn upload(&mut self, batch: UploadBatch) -> Result<UploadId, Rejected<UploadBatch>> {
        self.inner.upload(batch)
    }
    fn render_present(
        &mut self,
        frame: &RenderFrame,
        before: impl FnOnce(),
    ) -> Result<PresentOutcome, RenderError> {
        self.frames.push(frame.frame_id());
        if std::mem::take(&mut self.preparing) {
            return Ok(PresentOutcome::Preparing);
        }
        if let Some(error) = self.refusal.take() {
            return Err(error);
        }
        self.inner.render_present(frame, before)
    }
    fn cancel_preparation(&mut self, frame: &RenderFrame) {
        self.cancelled.push(frame.frame_id());
    }
    fn retire(&mut self, resources: &[ResourceVersion]) {
        self.inner.retire(resources);
    }
    fn poll(&mut self, out: &mut Vec<RenderEvent>, limit: usize) -> Result<(), RenderError> {
        self.polls += 1;
        if limit != 0 && std::mem::take(&mut self.resources_available) {
            out.push(RenderEvent::ResourcesAvailable);
            self.inner.poll(out, limit - 1)
        } else {
            self.inner.poll(out, limit)
        }
    }
    fn has_pending(&self) -> bool {
        self.resources_available || self.inner.has_pending()
    }
    fn suspend(&mut self) {
        self.inner.suspend();
    }
}
impl capture::CaptureBackend for Backend {
    type Error = RenderError;
    type Capture = ();
    fn capture(
        &mut self,
        _: &RenderFrame,
        _: [u32; 2],
        _: &BudgetPool,
        _: reel_core::wake::WakeSignal,
    ) -> Result<(), RenderError> {
        Err(RenderError::ResourcesUnavailable)
    }
    fn poll_capture(
        &mut self,
        _: &mut (),
        _: u64,
    ) -> Result<Option<capture::CapturedPixels>, RenderError> {
        Err(RenderError::ResourcesUnavailable)
    }
}
fn settle(story: &mut StoryRuntime, ns: u64) {
    for _ in 0..10_000 {
        story.tick(SliceBudget::default(), || ns).unwrap();
        if story
            .wake_deadline(MonoTime(ns))
            .is_none_or(|next| next > MonoTime(ns))
        {
            return;
        }
    }
    panic!("story work did not settle");
}

#[test]
fn borrowed_device_survives_player_font_frame_retirement_and_remount() {
    use crate::assets::{AssetOptions, Assets, ImageExecutor};
    use reel_core::{
        identity::{AssetKey, ContentVersion, Scope},
        result::Terminal,
        test_support::{MemoryAsset, MemoryAssetIo},
    };
    use std::{cell::RefCell, rc::Rc};
    const FONT: &[u8] = include_bytes!("../../tests/fixtures/font-weight/400.ttf");
    let limits = QueueLimits {
        max_requests: 16,
        max_bytes: 8 << 20,
    };
    let pool = BudgetPool::new(BudgetCategory::CpuInput, limits);
    let mut reads = None;
    let assets = Assets::with_adapters(AssetOptions::default(), |wake| {
        let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
        for path in ["body.ttf", "fallback.ttf"] {
            io.insert_asset(
                ResourceVersion {
                    key: AssetKey(path.into()),
                    version: ContentVersion([1; 32]),
                },
                MemoryAsset {
                    bytes: OwnedBytes::new(FONT.to_vec(), pool.reserve(FONT.len() as u64).unwrap())
                        .unwrap(),
                    media_type: None,
                },
            )
            .unwrap();
        }
        let io = Rc::new(RefCell::new(io));
        reads = Some(io.clone());
        (
            Box::new(crate::images::tests::ControlledIo(io)),
            Box::new(ImageExecutor::new(limits, wake)),
        )
    });
    let reads = reads.unwrap();
    let mut driver = AsideSessionUi::from_source(
        session(),
        r#"fn main() {
            game.configure(title: "Portable lifecycle", fonts: ["body.ttf", "fallback.ttf"]);
            stage.replace(button(width: 180, height: 60, on_click: |event| {
                await run(tween(event.target, x: 100, duration: 1));
            }));
        }"#,
        "portable_lifecycle",
        1,
        "main",
        "en",
        VmLimits::default(),
    )
    .unwrap()
    .with_assets(assets);
    for _ in 0..1000 {
        driver.tick(SliceBudget::default(), 256, || 0).unwrap();
        if driver.adapter().stage().root().is_some() && !driver.has_runnable_tasks() {
            break;
        }
    }
    assert_eq!(
        driver.adapter().game_settings().fonts,
        ["body.ttf", "fallback.ttf"]
    );
    assert!(driver.adapter().stage().root().is_some());
    let limits = ConsumerLimits {
        max_uploads: 1,
        max_resources: 4,
        max_frames: 2,
        max_events: 16,
        max_upload_bytes: 1 << 20,
        max_retained_bytes: 1 << 20,
        max_metadata_bytes: 8192,
        max_fixed_bytes: 16384,
    };
    let mut backend = Backend {
        inner: RecordingBackend::new(limits).unwrap(),
        refusal: None,
        polls: 0,
        resources_available: false,
        configurations: Vec::new(),
        preparing: false,
        frames: Vec::new(),
        cancelled: Vec::new(),
    };
    let mut host = Host::default();
    let mut player = Player::new(
        driver,
        &mut backend,
        &mut host,
        &PlayerOptions {
            logical_size: Some([640, 420]),
            canvas_size: Some([640, 420]),
            ..Default::default()
        },
    )
    .unwrap();
    for _ in 0..64 {
        player.tick(&mut host).unwrap();
    }
    assert!(player.settings_pending());
    assert_eq!(
        player.font_requests.len(),
        2,
        "prepare both fonts before waiting"
    );
    assert!(
        !player.needs_redraw(),
        "pending font IO must not spin redraws"
    );
    assert!(!player.session.as_ref().unwrap().needs_tick());
    for kind in ["move", "down", "up"] {
        player.dispatch_pointer(mouse(kind, (50., 30.))).unwrap();
    }
    assert!(
        !player.session.as_ref().unwrap().needs_tick(),
        "public input cannot wake author callbacks while fonts are pending"
    );
    player.render(&mut host).unwrap();
    assert!(!host.visible);
    assert_eq!(host.presents, 0);
    for _ in 0..1000 {
        while reads.borrow_mut().complete_next_read() {}
        player.tick(&mut host).unwrap();
        if !player.settings_pending() {
            break;
        }
    }
    assert!(!player.settings_pending(), "completed font must be applied");
    assert_eq!(player.active_fonts, ["body.ttf", "fallback.ttf"]);
    assert!(player.font_requests.is_empty());
    assert!(
        player.needs_redraw(),
        "completed inputs restore draw eligibility"
    );
    assert_eq!(host.title, "Portable lifecycle");

    // The recording consumer requires explicit upload completion, just as its
    // presentation contract requires explicit completion of submitted frames.
    let items: Vec<_> = player
        .resources
        .iter()
        .map(|font| {
            let reel_ui::resources::ResourceView::Font(data) = font.view() else {
                panic!("font resource");
            };
            let bytes = OwnedBytes::new(
                data.as_ref().to_vec(),
                pool.reserve(data.len() as u64).unwrap(),
            )
            .unwrap();
            UploadItem {
                resource: font.clone(),
                payload: UploadPayload::Font { bytes },
            }
        })
        .collect();
    let storage = pool
        .reserve((items.capacity() * size_of::<UploadItem>()) as u64)
        .unwrap();
    let upload = player
        .backend
        .upload(UploadBatch {
            items,
            scope: Scope::Process,
            storage,
        })
        .unwrap();
    assert!(
        player
            .backend
            .inner
            .complete_upload(upload, Terminal::Completed(()))
    );
    player.render(&mut host).unwrap();
    assert!(host.visible);
    assert_eq!(host.presents, 1);
    let accepted = player.presentation.bridge.accepted_key().unwrap().clone();
    let first = player.backend.inner.last_submission().unwrap();
    assert!(player.backend.inner.complete_render(first));
    player.poll_completions().unwrap();
    let configurations = player.backend.configurations.len();
    let metrics = host.metrics;
    player.resize(&mut host, metrics).unwrap();
    assert_eq!(player.backend.configurations.len(), configurations);
    assert_eq!(player.presentation.bridge.accepted_key(), Some(&accepted));
    assert!(!player.needs_redraw());
    // Unbound analogue input still reaches the shared input path, without
    // rebuilding an unchanged accepted frame on each controller sample.
    for index in 0..120 {
        use reel_ui::{core::TextEvent, input::*};
        player
            .text_event(TextEvent::Input(InputEvent::Value {
                device: InputDeviceId::Gamepad(1),
                control: Control::Axis(GamepadAxis::LeftX),
                value: if index % 2 == 0 { 0.01 } else { -0.01 },
            }))
            .unwrap();
        player.tick(&mut host).unwrap();
        assert!(!player.needs_redraw(), "unbound sample {index} repainted");
    }
    for kind in ["move", "down", "up"] {
        player.dispatch_pointer(mouse(kind, (50., 30.))).unwrap();
    }
    for _ in 0..64 {
        player.tick(&mut host).unwrap();
    }
    // Accept the queued click feedback before another visual sample takes the
    // next candidate slot. Its completion releases the recording consumer too.
    player.render(&mut host).unwrap();
    assert!(
        player
            .backend
            .inner
            .complete_render(player.backend.inner.last_submission().unwrap())
    );
    player.poll_completions().unwrap();
    host.now = MonoTime(250_000_000);
    player.tick(&mut host).unwrap();
    let x = |player: &mut Player<&mut Backend>| {
        player
            .story_mut()
            .unwrap()
            .driver()
            .adapter()
            .stage()
            .nodes()
            .next()
            .unwrap()
            .props
            .x
    };
    assert_eq!(x(&mut player), 25.);
    player.backend.preparing = true;
    player.render(&mut host).unwrap();
    assert!(player.presentation.is_preparing());
    let preparing = *player.backend.frames.last().unwrap();
    player.resize(&mut host, metrics).unwrap();
    assert!(player.presentation.is_preparing());
    assert!(player.backend.cancelled.is_empty());
    player.suspend(&mut host).unwrap();
    assert_eq!(player.backend.cancelled, [preparing]);
    assert!(!player.presentation.is_preparing());
    assert!(player.presentation.bridge.accepted_key().is_none());
    let configurations = player.backend.configurations.len();
    host.metrics = DisplayMetrics {
        physical_size: [800, 600],
        scale_factor: 1.25,
    };
    let metrics = host.metrics;
    player.resize(&mut host, metrics).unwrap();
    assert_eq!(player.backend.configurations.len(), configurations);
    let suspended_epoch = player.presentation.surface_info().surface_epoch;
    player.resize(&mut host, metrics).unwrap();
    assert_eq!(
        player.presentation.surface_info().surface_epoch,
        suspended_epoch
    );
    assert!(player.presentation.is_suspended());
    player.render(&mut host).unwrap();
    assert_eq!(host.presents, 2);
    assert!(player.presentation.bridge.accepted_key().is_none());
    host.now = MonoTime(10_250_000_000);
    player.resume(&mut host).unwrap();
    assert_eq!(player.backend.configurations.len(), configurations + 1);
    let restored = player.backend.configurations.last().unwrap();
    assert_eq!(restored.physical_size, [800, 600]);
    assert_eq!(restored.scale_factor, 1.25);
    player.tick(&mut host).unwrap();
    assert_eq!(x(&mut player), 25., "suspension freezes animation time");
    player.render(&mut host).unwrap();
    assert_eq!(host.presents, 3);
    assert_ne!(
        player.presentation.bridge.accepted_key().unwrap(),
        &accepted
    );
    host.now = MonoTime(10_500_000_000);
    player.tick(&mut host).unwrap();
    assert_eq!(x(&mut player), 50.);

    let frame_pool = player.pool.clone();
    let fonts: Vec<_> = player
        .resources
        .iter()
        .map(|font| font.downgrade())
        .collect();
    let font_versions: Vec<_> = player
        .resources
        .iter()
        .map(|font| font.version().clone())
        .collect();
    let last = player.backend.inner.last_submission().unwrap();
    player.backend.preparing = true;
    player.render(&mut host).unwrap();
    assert!(player.presentation.is_preparing());
    let closing_candidate = *player.backend.frames.last().unwrap();
    player.close().unwrap();
    assert_eq!(player.backend.cancelled, [preparing, closing_candidate]);
    for _ in 0..1000 {
        if player.close_complete() {
            break;
        }
        player.tick(&mut host).unwrap();
    }
    assert!(player.close_complete());
    drop(player);
    assert!(
        frame_pool.usage().bytes > 0,
        "submitted frame retains its resources"
    );
    assert!(backend.inner.complete_render(last));
    backend.retire(&font_versions);
    backend.poll(&mut Vec::new(), 64).unwrap();
    assert_eq!(frame_pool.usage().bytes, 0);
    assert!(
        fonts.iter().all(|font| font.upgrade().is_none()),
        "departed player's font owners are released"
    );
    // Like GXM reset_session, clear the departed session's surface and caches
    // while preserving the device wrapper. A new player owns a fresh epoch domain.
    let reset_session = |backend: &mut Backend| {
        assert!(!backend.has_pending());
        assert_eq!(backend.inner.resident_count(), 0);
        assert_eq!(backend.inner.inflight_count(), 0);
        backend.inner = RecordingBackend::new(limits).unwrap();
    };
    reset_session(&mut backend);

    // Exercise the actual library -> game -> library session variants, including
    // a game assembly error. Every player borrows the same still-live device.
    for (library, fail) in [(true, false), (false, true), (true, false)] {
        let configurations = backend.configurations.len();
        let session = if library {
            crate::player::session::PlayerSession::Launcher(Box::new(
                crate::launcher::Launcher::new(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
                    Assets::directory(Default::default()),
                    None,
                    reel_core::wake::WakeSignal::new(|| {}),
                )
                .unwrap(),
            ))
        } else {
            crate::player::session::PlayerSession::aside(
                AsideSessionUi::from_source(
                    session(),
                    "fn main() {}",
                    "replacement",
                    1,
                    "main",
                    "en",
                    VmLimits::default(),
                )
                .unwrap(),
            )
        };
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let mut player = Player::mount(session, &mut backend, &mut host, &Default::default())?;
            if fail {
                return Err("session assembly failed".into());
            }
            player.close()?;
            Ok(())
        })();
        assert_eq!(result.is_err(), fail, "library={library}: {result:?}");
        assert_eq!(backend.configurations.len(), configurations + 1);
        assert!(!backend.has_pending());
        reset_session(&mut backend);
    }
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
    let (bridge, root) = mount(story.driver().ui());
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
        resources_available: false,
        configurations: Vec::new(),
        preparing: false,
        frames: Vec::new(),
        cancelled: Vec::new(),
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
    let mut stamp = None;
    let mut render = |story: &mut StoryRuntime,
                      view: &mut Presentation,
                      backend: &mut Backend,
                      host: &mut Host| {
        story
            .driver_mut()
            .unwrap()
            .sync(&mut view.bridge, root)
            .unwrap();
        let result = view
            .render(backend, host, |bridge, id, info| {
                let Some((source, receipt)) = story.driver().record(bridge, root, id).unwrap()
                else {
                    return Ok(None);
                };
                stamp = Some(receipt);
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
                Ok(Some(build.finish().unwrap()))
            })
            .unwrap();
        if result.accepted {
            story
                .driver_mut()
                .unwrap()
                .presented(&view.bridge, stamp.as_ref().unwrap())
                .unwrap();
        }
        result
    };
    let click = |story: &mut StoryRuntime, view: &mut Presentation| {
        for kind in ["move", "down", "up"] {
            story
                .driver_mut()
                .unwrap()
                .handle_pointer(&mut view.bridge, mouse(kind, (50., 30.)))
                .unwrap();
        }
        story.wake_input();
        settle(story, 0);
        story
            .driver_mut()
            .unwrap()
            .sync(&mut view.bridge, root)
            .unwrap();
    };
    click(&mut story, &mut view);
    assert_eq!(
        story
            .driver()
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
    let presents = host.presents;
    view.invalidate();
    let pending = view
        .render(&mut backend, &mut host, |_, _, _| Ok(None))
        .unwrap();
    assert!(!pending.accepted && pending.refused.is_none());
    assert_eq!(
        host.presents, presents,
        "unfinished content never reaches the backend"
    );
    assert_eq!(view.bridge.pending_count(), 0);
    assert_eq!(view.bridge.accepted_key().unwrap().frame_id(), accepted);
    assert!(!view.needs_redraw());
    view.render(&mut backend, &mut host, |_, _, _| {
        panic!("preparation waits for the VM")
    })
    .unwrap();
    view.invalidate();
    click(&mut story, &mut view);
    assert_eq!(
        story
            .driver()
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
    story
        .driver_mut()
        .unwrap()
        .sync(&mut view.bridge, root)
        .unwrap();
    assert_eq!(
        story
            .driver()
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
    let pressured = render(&mut story, &mut view, &mut backend, &mut host);
    assert!(!pressured.accepted && pressured.refused.is_none());
    assert_eq!(view.bridge.pending_count(), 1);
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
    backend.refusal = Some(RenderError::Busy);
    backend.resources_available = true;
    let pressured = render(&mut story, &mut view, &mut backend, &mut host);
    assert!(!pressured.accepted && pressured.refused.is_none());
    assert!(!view.needs_redraw());
    let presents = host.presents;
    events.clear();
    view.poll(&mut backend, &mut events, 0).unwrap();
    assert!(!view.needs_redraw());
    view.poll(&mut backend, &mut events, 1).unwrap();
    assert!(matches!(
        events.as_slice(),
        [RenderEvent::ResourcesAvailable]
    ));
    assert!(view.needs_redraw());
    assert_eq!(view.bridge.accepted_key().unwrap().frame_id(), accepted);
    assert_eq!(
        host.presents, presents,
        "upload completion does not present or accept input"
    );
    assert!(render(&mut story, &mut view, &mut backend, &mut host).accepted);
    assert!(
        backend
            .inner
            .complete_render(backend.inner.last_submission().unwrap())
    );
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

    story.suspend(250_000_000).unwrap();
    view.suspend();
    backend.suspend();
    story.driver_mut().unwrap().clear_presented().unwrap();
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
    story
        .driver_mut()
        .unwrap()
        .sync(&mut view.bridge, root)
        .unwrap();
    assert_eq!(
        story
            .driver()
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
    story
        .driver_mut()
        .unwrap()
        .sync(&mut view.bridge, root)
        .unwrap();
    let node = story.driver().adapter().stage().nodes().next().unwrap();
    assert_eq!(node.props.y, 100.);
    assert!(!node.props.enabled);

    story.suspend(11_000_000_000).unwrap();
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
