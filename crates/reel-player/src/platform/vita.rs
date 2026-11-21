//! Vita host assembly using the shared player, asset loader and media transport.
//!
//! Build with nightly Rust, `rust-src` and VitaSDK on `PATH`:
//!
//! ```sh
//! cargo +nightly vita build eboot --locked --release \
//!     -p reel-player --bin reel-vita --no-default-features --features vita
//! ```
//!
//! Set the target's `CC`, `CXX` and `AR` to the VitaSDK cross tools. The render
//! build compiles Cg to embedded GXP programs with the official SDK tool selected
//! by `REEL_PSP2CGC` or `SCE_PSP2_SDK_DIR`.
//! `cargo-vita` builds the executable reused by the standalone game distributor.
//! Graphics uses the system GXM API.
use super::vita_signals::Signals;
use crate::{
    hal::{DisplayMetrics, Host, PlatformHost, WindowControl},
    player::{Player, PlayerOptions},
};
use reel_core::{
    budget::{BudgetCategory, BudgetGroup, BudgetPool, QueueLimits},
    clock::{Clock, MonoTime, UnixMillis},
    wake::WakeSignal,
    window::{WindowCapabilities, WindowConfiguration, WindowInfo, WindowPlatform, WindowSize},
};
use reel_render::gxm::{GxmBackend, GxmOptions};
use reel_ui::app::RenderRootSignal;
use std::{path::Path, rc::Rc, sync::Arc, time::Duration};
use vita_native::app::{
    self as native,
    power::{PowerEvents, PowerKind},
    wake::Wake,
};
#[cfg(feature = "performance")]
mod frame_probe;
mod input;
#[cfg(feature = "performance")]
mod profiling;
type Error = Box<dyn std::error::Error>;

/// Fixed-display host. Native controller, wake and power objects stay in the
/// platform adapter; author code sees the ordinary stage and window capabilities.
pub struct VitaHost {
    size: [u32; 2],
    wake: Arc<Wake>,
    signals: Rc<Signals>,
    deadline: Option<MonoTime>,
    event_time: Option<MonoTime>,
    vsync: bool,
}
impl VitaHost {
    fn new(size: [u32; 2], wake: Arc<Wake>) -> Self {
        Self {
            size,
            wake,
            signals: Rc::new(Signals::default()),
            deadline: None,
            event_time: None,
            vsync: true,
        }
    }
}
impl Clock for VitaHost {
    fn monotonic_now(&self) -> MonoTime {
        self.event_time
            .unwrap_or_else(|| MonoTime(native::time::monotonic_micros().saturating_mul(1000)))
    }
    fn wall_time(&self) -> Option<UnixMillis> {
        native::time::unix_millis().ok().map(UnixMillis)
    }
}
impl PlatformHost for VitaHost {
    fn request_redraw(&mut self) {
        self.signals.request_redraw();
    }
    fn set_wake_deadline(&mut self, when: Option<MonoTime>) {
        self.deadline = when;
    }
    fn before_present(&mut self) {}
    fn request_exit(&mut self) {
        self.signals.exit.set(true);
        let _ = self.wake.signal();
    }
}
impl WindowControl for VitaHost {
    fn window_info(&mut self) -> WindowInfo {
        let size = WindowSize {
            width: f64::from(self.size[0]),
            height: f64::from(self.size[1]),
        };
        WindowInfo {
            platform: WindowPlatform::Vita,
            size,
            windowed_size: size,
            scale_factor: 1.,
            fullscreen: true,
            vsync: self.vsync,
            refresh_rate_millihertz: Some(60_000),
            capabilities: WindowCapabilities::default(),
            resolutions: Arc::from([size]),
        }
    }
    fn configure_window(&mut self, configuration: WindowConfiguration) -> Result<bool, String> {
        if configuration
            .width
            .is_some_and(|width| width != f64::from(self.size[0]))
            || configuration
                .height
                .is_some_and(|height| height != f64::from(self.size[1]))
            || configuration.fullscreen == Some(false)
            || configuration.vsync.is_some_and(|value| value != self.vsync)
        {
            return Err("Vita uses its fixed fullscreen display configuration".into());
        }
        Ok(true)
    }
    fn window_resized(&mut self) -> bool {
        true
    }
    fn move_pointer(&mut self, _: [f64; 2]) -> Result<bool, String> {
        Ok(false)
    }
}
impl Host for VitaHost {
    fn display_metrics(&self) -> DisplayMetrics {
        DisplayMetrics {
            physical_size: self.size,
            scale_factor: 1.,
        }
    }
    fn set_title(&mut self, title: &str) {
        #[cfg(feature = "performance")]
        if reel_core::profile::WallSnapshot::is_active() {
            eprintln!("[reel-probe] title={title}");
        }
        *self.signals.title.borrow_mut() = title.into();
    }
    fn set_visible(&mut self, visible: bool) {
        self.signals.visible.set(visible);
    }
    fn wake_callback(&self) -> Arc<dyn Fn() + Send + Sync> {
        let wake = self.wake.clone();
        Arc::new(move || {
            let _ = wake.signal();
        })
    }
    fn ui_signal_handler(&self) -> impl FnMut(RenderRootSignal) + 'static {
        let signals = self.signals.clone();
        let wake = self.wake.clone();
        move |signal| {
            signals.receive(signal, || {
                let _ = wake.signal();
            });
        }
    }
}

/// One native event loop and graphics device shared by successive sessions.
/// Session resources retire at each boundary; device programs survive until
/// `run` closes the application. The GXM device is initialized once per run.
pub struct Runtime {
    native: NativeLoop,
    backend: GxmBackend,
}

impl Runtime {
    /// Create the native device, run the application, and close it explicitly.
    /// Shutdown errors retain the application's original error when both fail.
    pub fn run<T>(application: impl FnOnce(&mut Self) -> Result<T, Error>) -> Result<T, Error> {
        let (native, backend) = NativeLoop::new()?;
        #[cfg(feature = "vm-profiling")]
        crate::set_vm_profiling_enabled(false)?;
        let mut runtime = Self { native, backend };
        let result = application(&mut runtime);
        let Self {
            mut native,
            backend,
        } = runtime;
        let result = close_backend(result, backend);
        #[cfg(feature = "performance")]
        if let Err(error) = native.finish_performance(&result) {
            return result.and(Err(error));
        }
        #[cfg(not(feature = "performance"))]
        let _ = &mut native;
        result
    }

    /// Borrow the application's renderer for native drawing. A subsequent
    /// player session drains submitted work and retires these session resources.
    pub fn renderer(&mut self) -> &mut GxmBackend {
        &mut self.backend
    }

    /// Measure the next ordinary package session using a validated developer
    /// request. Its optional accepted-frame capture closes after replay.
    #[cfg(feature = "performance")]
    pub fn set_performance(&mut self, request: crate::performance::Request) -> Result<(), Error> {
        request.validate()?;
        if self.native.performance.is_some()
            || self.native.recording.is_some()
            || self.native.completed_performance.is_some()
        {
            return Err("this runtime already owns a performance request".into());
        }
        self.native.performance = Some(request);
        Ok(())
    }

    /// Load a relocatable ASB distribution using the retained device. The host
    /// supplies this work's persistent-data directory independently of title ID.
    pub fn run_package(&mut self, package: &Path, save_directory: &Path) -> Result<(), Error> {
        self.backend.reset_session()?;
        self.native.begin_session(&self.backend);
        #[cfg(feature = "performance")]
        self.native.start_performance()?;
        let result = run_package_on(package, save_directory, &mut self.native, &mut self.backend);
        let result = reset_after_session(&mut self.backend, &result).and(result);
        #[cfg(feature = "performance")]
        self.native.complete_performance(&result);
        result
    }

    /// Run a compiled story with native controllers and power callbacks.
    pub fn run_story(
        &mut self,
        story: reel_engine::aside::source::CompiledStory,
        options: PlayerOptions,
    ) -> Result<(), Error> {
        self.backend.reset_session()?;
        self.native.begin_session(&self.backend);
        let result = run_story_on(story, options, &mut self.native, &mut self.backend);
        reset_after_session(&mut self.backend, &result)?;
        result
    }
}

fn run_package_on(
    package: &Path,
    save_directory: &Path,
    native: &mut NativeLoop,
    backend: &mut GxmBackend,
) -> Result<(), Error> {
    let loading = reel_core::profile::Scope::new(reel_core::profile::Phase::Load);
    let loaded = crate::distribution::load(package, None)?;
    let mut options = PlayerOptions::default();
    options.assets.root = loaded.loose.clone().unwrap_or_else(|| loaded.root.clone());
    // Font payloads move from input to resident ownership without copying. Keep
    // complete Unicode fonts viable alongside images and dynamic player input.
    options.assets.resident_bytes = 64 << 20;
    options.assets.input_bytes = crate::artwork::VITA_INPUT_BYTES;
    options.assets.decode_bytes = crate::artwork::VITA_DECODE_BYTES;
    options.asset_packages = Some(loaded.packages);
    options.asset_overlay = loaded.loose;
    options.resource_target = "vita".into();
    options.catalogs = loaded.catalogs.into();
    options.save_directory = Some(save_directory.into());
    options.save_bytes = 8 << 20;
    options.frame_bytes = 4 << 20;
    options.media.pcm_bytes = 8 << 20;
    options.media.static_pcm_threshold_bytes = 256 << 10;
    options.media.stream_window_frames = 4096;
    options.media.queue_bytes = 1 << 20;
    options.media.mixer.max_sounds = 16;
    // Includes AvPlayer's codec buffers, owned NV12 output and PCM. The 960x544
    // hardware profile reaches the old 24 MiB limit before its output pool fills.
    options.media.video.decoded_bytes = 32 << 20;
    options.media.video.queued_pictures = 2;
    options.media.video.queued_audio_frames = 4096;
    options.media.video_decoder = Some(Arc::new(reel_media::video::VitaVideoDecoderFactory {
        contiguous_budget: Some(native.video_contiguous.clone()),
        ..Default::default()
    }));
    let story = crate::distribution::story_from_package(loaded.package)?;
    drop(loading);
    run_story_on(story, options, native, backend)
}

fn run_story_on(
    story: reel_engine::aside::source::CompiledStory,
    mut options: PlayerOptions,
    native: &mut NativeLoop,
    backend: &mut GxmBackend,
) -> Result<(), Error> {
    use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
    let mounting = reel_core::profile::Scope::new(reel_core::profile::Phase::Mount);
    let assets = options
        .open_assets()
        .map_err(|error| format!("Vita resources: {error:?}"))?;
    let notify = native.host.wake_callback();
    let signal = WakeSignal::new(move || notify());
    let media_options = options.media.clone();
    let make_media = {
        let assets = assets.clone();
        let signal = signal.clone();
        move || {
            Box::new(crate::media::MediaPlayer::new(
                assets.clone(),
                media_options.clone(),
                signal.clone(),
                |wake| {
                    reel_media::VitaOutput::new(wake)
                        .map(|output| Box::new(output) as Box<dyn reel_media::AudioOutput>)
                        .map_err(|e| e.to_string())
                },
            )) as Box<dyn reel_engine::media::MediaBackend>
        }
    };
    // Code must leave at least 24 MiB for mutable state. The ledger admits
    // actual allocations up to the shared 80 MiB VM allowance; it does not
    // preallocate the allowance or cap a larger live UI at that minimum.
    let visual_plan_bytes = story
        .visual_plan
        .as_ref()
        .map_or(0, |plan| plan.as_bytes().len() as u64);
    let bytes = story.program.execution_capacity_bytes() as u64 + visual_plan_bytes + (24 << 20);
    if bytes > 80 << 20 {
        return Err(format!(
            "VM needs {} MiB for code and working memory; Vita allows 80 MiB",
            bytes.div_ceil(1 << 20)
        )
        .into());
    }
    let mut limits = VmLimits::default();
    limits.heap.max_bytes = (80 << 20) - visual_plan_bytes;
    limits.slots.initial_capacity = 4096;
    limits.slots.max_capacity = 65_536;
    limits.max_tasks = 256;
    let driver = crate::aside_session::AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1))?,
        story,
        &options.locale,
        limits,
    )?
    .with_asset_context(assets, options.catalogs.clone())
    .with_media(make_media())?;
    let mut player = Player::new(driver, backend, &mut native.host, &options)?;
    if let Some(runtime) = player.story_mut() {
        let root = options
            .save_directory
            .clone()
            .ok_or("Vita save directory is required")?;
        let service = crate::saves::SaveService::new(
            Arc::new(crate::saves::DirectorySaves::new(root, options.save_bytes)),
            QueueLimits {
                max_requests: 1,
                max_bytes: options.save_bytes,
            },
            signal,
        );
        runtime.set_saves(crate::saves::RuntimeSaves::new(service, options.save_bytes));
        runtime.set_media_factory(make_media);
    }
    drop(mounting);
    native.drive(&mut player)
}

fn reset_after_session<T>(
    backend: &mut GxmBackend,
    result: &Result<T, Error>,
) -> Result<(), Error> {
    backend.reset_session().map_err(|failure| {
        let prior = result
            .as_ref()
            .err()
            .map(|error| format!("{error}\n"))
            .unwrap_or_default();
        format!("{prior}Vita graphics session cleanup: {failure}").into()
    })
}

fn close_backend<T>(result: Result<T, Error>, backend: GxmBackend) -> Result<T, Error> {
    match (result, backend.close()) {
        (result, Ok(())) => result,
        (Ok(_), Err(failure)) => Err(format!("Vita graphics shutdown: {failure}").into()),
        (Err(error), Err(failure)) => {
            Err(format!("{error}\nVita graphics shutdown: {failure}").into())
        }
    }
}

struct NativeLoop {
    video_contiguous: BudgetPool,
    host: VitaHost,
    wake: Arc<Wake>,
    power: PowerEvents,
    controls: native::input::Input,
    #[cfg(feature = "performance")]
    frame_probe: Option<frame_probe::FrameProbe>,
    #[cfg(feature = "performance")]
    performance: Option<crate::performance::Request>,
    #[cfg(feature = "performance")]
    recording: Option<crate::performance::recording::Recording>,
    #[cfg(feature = "performance")]
    completed_performance: Option<(crate::performance::recording::Recording, String, String)>,
}

impl NativeLoop {
    #[cfg(feature = "performance")]
    fn start_performance(&mut self) -> Result<(), Error> {
        let Some(request) = self.performance.take() else {
            return Ok(());
        };
        #[cfg(feature = "thread-profiling")]
        profiling::install_thread_clock();
        self.frame_probe = request
            .frame
            .clone()
            .map(|frame| frame_probe::FrameProbe::new(frame, request.output.join("frozen.tsv")));
        self.recording = Some(crate::performance::recording::Recording::start(request)?);
        Ok(())
    }

    #[cfg(feature = "performance")]
    fn complete_performance(&mut self, result: &Result<(), Error>) {
        if let Some(mut recording) = self.recording.take() {
            recording.stop();
            let outcome = result
                .as_ref()
                .err()
                .map(ToString::to_string)
                .unwrap_or_else(|| "closed".into());
            self.completed_performance =
                Some((recording, self.host.signals.title.borrow().clone(), outcome));
        }
    }

    #[cfg(feature = "performance")]
    fn finish_performance<T>(&mut self, result: &Result<T, Error>) -> Result<(), Error> {
        if let Some((mut recording, title, outcome)) = self.completed_performance.take() {
            let outcome = result
                .as_ref()
                .err()
                .map(ToString::to_string)
                .unwrap_or(outcome);
            recording.finish(&title, &outcome)?;
        }
        Ok(())
    }
    fn begin_session(&mut self, backend: &GxmBackend) {
        #[cfg(feature = "performance")]
        let _ = reel_core::profile::Sample::take();
        // Signals and deadlines belong to the departed player, unlike native input,
        // power callbacks and the wake object retained by the application.
        self.host = VitaHost::new(backend.output_size(), self.wake.clone());
    }

    fn new() -> Result<(Self, GxmBackend), Error> {
        let wake = Wake::new()?;
        let power = PowerEvents::new(wake.clone())?;
        let controls = native::input::Input::new()?;
        // Existing host-service stacks already reduce free MAIN. Configure before
        // graphics or session/media workers own additional managed blocks.
        vita_native::allocator::configure_main_budget()?;
        let free = native::memory::free_memory()?;
        let contiguous_capacity = free.contiguous_bytes;
        let video_contiguous = BudgetGroup::new(contiguous_capacity).pool(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: u32::MAX,
                max_bytes: contiguous_capacity,
            },
        );
        let display_wake = wake.clone();
        let backend = GxmBackend::new(
            GxmOptions::default(),
            Some(Arc::new(move || {
                let _ = display_wake.signal();
            })),
        )?;
        let host = VitaHost::new(backend.output_size(), wake.clone());
        Ok((
            Self {
                video_contiguous,
                host,
                wake,
                power,
                controls,
                #[cfg(feature = "performance")]
                frame_probe: None,
                #[cfg(feature = "performance")]
                performance: None,
                #[cfg(feature = "performance")]
                recording: None,
                #[cfg(feature = "performance")]
                completed_performance: None,
            },
            backend,
        ))
    }

    fn drive(&mut self, player: &mut Player<&mut GxmBackend>) -> Result<(), Error> {
        let Self {
            host,
            wake,
            power,
            controls,
            video_contiguous,
            #[cfg(feature = "performance")]
            frame_probe,
            #[cfg(feature = "performance")]
            recording,
            #[cfg(feature = "performance")]
                performance: _,
            #[cfg(feature = "performance")]
                completed_performance: _,
        } = self;
        #[cfg(feature = "performance")]
        let mut frame_probe = frame_probe.take();
        #[cfg(not(feature = "performance"))]
        let _ = video_contiguous;
        // Carry held input through the boundary until release. A launch-confirm press
        // must not become a second confirmation in the newly mounted game.
        let mut input = input::Input::default();
        let mut text = super::vita_text::TextInput::<native::ime::Keyboard>::default();
        let mut suspended = false;
        let mut closing = false;
        #[cfg(feature = "performance")]
        let mut next_memory_sample = MonoTime(0);
        #[cfg(feature = "performance")]
        let mut waits = profiling::Waits::default();
        let result = (|| -> Result<(), Error> {
            input = input::Input::new(controls.sample()?);
            loop {
                while let Some(event) = power.poll()? {
                    // Keep the actual notification timestamp, even when a callback
                    // was queued immediately before the kernel suspended this thread.
                    host.event_time = Some(MonoTime(event.monotonic_micros.saturating_mul(1000)));
                    match event.kind {
                        PowerKind::Suspending if !suspended => {
                            input.cancel(player)?;
                            text.cancel(|event| player.text_event(event).map(|_| ()))?;
                            player.suspend(host)?;
                            suspended = true;
                        }
                        PowerKind::Resumed if suspended => {
                            player.resume(host)?;
                            player.text_event(reel_ui::core::TextEvent::WindowFocusChange(true))?;
                            suspended = false;
                        }
                        _ => {}
                    }
                    host.event_time = None;
                }
                if (host.signals.exit.get() || player.exit_requested()) && !closing {
                    text.cancel(|event| player.text_event(event).map(|_| ()))?;
                    player.close()?;
                    closing = true;
                }
                if closing {
                    player.tick(host)?;
                    player.poll_completions()?;
                    if player.close_complete() {
                        break;
                    }
                    wake.wait(Some(Duration::from_millis(1)))?;
                    continue;
                }
                if suspended {
                    wake.wait(None)?;
                    continue;
                }
                let now = host.monotonic_now();
                #[cfg(feature = "performance")]
                let probe_started = recording.as_ref().map(|_| std::time::Instant::now());
                let poll_deadline = MonoTime(now.0.saturating_add(16_000_000));
                let sample = {
                    let _timing =
                        reel_core::profile::Scope::new(reel_core::profile::Phase::ControlPoll);
                    controls.sample()?
                };
                let keyboard_was_active = text.active();
                {
                    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Input);
                    if !keyboard_was_active {
                        input.sample(sample, now, host.size, player)?;
                    }
                    text.update(&host.signals.text, |event| {
                        player.text_event(event).map(|_| ())
                    })?;
                    if keyboard_was_active || text.active() {
                        if !keyboard_was_active {
                            input.cancel(player)?;
                        }
                        // Native keyboard owns held controls until their release;
                        // closing it cannot leak its confirm/touch into the story.
                        input.suppress(sample);
                    }
                }
                player.poll_completions()?;
                player.tick(host)?;
                player.prepare_initial_frame(host)?;
                if host.signals.take_redraw() || player.needs_redraw() {
                    player.render(host)?;
                }
                #[cfg(feature = "performance")]
                let probe_presented = if let (Some(recording), Some(probe_started)) =
                    (recording.as_mut(), probe_started)
                {
                    let usage = player.backend.usage();
                    let presented = recording.record(
                        probe_started,
                        player.story_mut().as_deref(),
                        usage.total(),
                        &host.signals.title.borrow(),
                    );
                    if presented && reel_core::profile::WallSnapshot::is_active() {
                        profiling::record_submission(recording, player.backend);
                    }
                    if now >= next_memory_sample {
                        next_memory_sample = MonoTime(now.0.saturating_add(1_000_000_000));
                        profiling::record_device(
                            recording,
                            player.backend,
                            video_contiguous,
                            &waits,
                        );
                    }
                    presented
                } else {
                    false
                };
                // Accepting a retained frame can expose a newer model after its
                // UI signal was consumed. Service eligible work before sleeping;
                // preparation under GPU pressure remains blocked until completion.
                #[cfg(feature = "performance")]
                if probe_presented && let Some(probe) = frame_probe.as_mut() {
                    let title = host.signals.title.borrow().clone();
                    if let Some(frame) = player.presentation.accepted_frame.as_ref()
                        && probe.should_capture(&title, frame.frame_id().0)
                    {
                        let frame = frame.clone();
                        let assets = player
                            .session
                            .as_ref()
                            .expect("active performance session")
                            .ui()
                            .assets();
                        probe.run(player.backend, &frame, &assets)?;
                        host.signals.exit.set(true);
                        continue;
                    }
                }
                if host.signals.has_redraw() || player.needs_redraw() {
                    continue;
                }
                let now = host.monotonic_now();
                let deadline = player
                    .story_deadline(now)
                    .into_iter()
                    .chain(host.deadline)
                    .chain(input.deadline())
                    .chain([poll_deadline])
                    .min()
                    .expect("controller poll deadline");
                // Controller/touch sampling has no event notification API. Poll at a
                // bounded 60 Hz. Time spent in rendering/vsync already contributes
                // to this interval, so continuous animation receives no second cap.
                let timeout = Duration::from_nanos(deadline.0.saturating_sub(now.0));
                {
                    let _timing =
                        reel_core::profile::Scope::new(reel_core::profile::Phase::HostWait);
                    #[cfg(feature = "performance")]
                    let started = recording.as_ref().map(|_| std::time::Instant::now());
                    let _result = wake.wait(Some(timeout))?;
                    #[cfg(feature = "performance")]
                    if let Some(started) = started {
                        waits.record(timeout, started.elapsed(), _result);
                    }
                }
            }
            Ok(())
        })();
        #[cfg(feature = "performance")]
        let result = result.and_then(|()| match frame_probe.as_ref() {
            Some(probe) => probe.finish(),
            None => Ok(()),
        });
        #[cfg(feature = "performance")]
        if let Some(recording) = recording.as_mut() {
            profiling::record_device(recording, player.backend, video_contiguous, &waits);
        }
        // Always close shared services before the graphics context and native wake
        // registrations disappear, including error exits from input or rendering.
        host.event_time = None;
        let keyboard_cleanup = text.cancel(|event| player.text_event(event).map(|_| ()));
        let cleanup = (|| -> Result<(), Error> {
            if !closing {
                player.close()?;
            }
            while !player.close_complete() {
                player.tick(host)?;
                player.poll_completions()?;
                wake.wait(Some(Duration::from_millis(1)))?;
            }
            Ok(())
        })();
        // Preserve the event/render failure when closing also reports an error.
        let result = result.and(keyboard_cleanup).and(cleanup);
        result
    }
}
