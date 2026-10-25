//! Vita host assembly using the shared player, asset loader and media transport.
//!
//! Build on Linux/WSL with nightly Rust, `rust-src` and VitaSDK on `PATH`:
//!
//! ```sh
//! cargo +nightly build -Z build-std=std,panic_unwind --locked --release \
//!     -p reel-player --bin reel-vita --no-default-features --features vita \
//!     --target armv7-sony-vita-newlibeabihf
//! ```
//!
//! Set the target's `CC`, `CXX` and `AR` to the VitaSDK cross tools. The render
//! build compiles GLSL to embedded SGX binaries with the pinned host compiler.
//! Convert the ELF using `vita-elf-create` and `vita-make-fself`; the runtime
//! directory contains `eboot.bin`, `sce_sys/param.sfo` and the four matching PVR
//! modules under `module/`. `reel export --runtime` adds the game distribution.
use crate::{
    hal::{DisplayMetrics, Host, PlatformHost, WindowControl},
    player::{Player, PlayerOptions},
};
use pvr_psp2_sys::platform::{
    self as native,
    power::{PowerEvents, PowerKind},
    wake::Wake,
};
use reel_core::{
    budget::QueueLimits,
    clock::{Clock, MonoTime, UnixMillis},
    wake::WakeSignal,
    window::{WindowCapabilities, WindowConfiguration, WindowInfo, WindowPlatform, WindowSize},
};
use reel_render::gles2::{GlesBackend, GlesOptions};
use reel_ui::app::RenderRootSignal;
use std::{
    cell::{Cell, RefCell},
    path::Path,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
mod input;
type Error = Box<dyn std::error::Error>;

#[derive(Default)]
struct Signals {
    redraw: Cell<bool>,
    exit: Cell<bool>,
    visible: Cell<bool>,
    title: RefCell<String>,
}
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
        self.signals.redraw.set(true);
        let _ = self.wake.signal();
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
        move |signal| match signal {
            RenderRootSignal::RequestRedraw | RenderRootSignal::RequestAnimFrame => {
                signals.redraw.set(true);
                let _ = wake.signal();
            }
            RenderRootSignal::Exit => {
                signals.exit.set(true);
                let _ = wake.signal();
            }
            RenderRootSignal::SetTitle(title) => {
                *signals.title.borrow_mut() = title;
            }
            _ => {}
        }
    }
}

/// Load the same relocatable ASB distribution used by desktop exports. Persistent
/// data follows the installed title identity and survives rebuilt code packages.
pub fn run_package(package: &Path, save_root: &Path, module_directory: &Path) -> Result<(), Error> {
    let loaded = crate::distribution::load(package, None)?;
    let title_id = native::title_id()?;
    let mut options = PlayerOptions::default();
    options.assets.root = loaded.loose.clone().unwrap_or_else(|| loaded.root.clone());
    options.assets.resident_bytes = 24 << 20;
    options.assets.input_bytes = 4 << 20;
    options.assets.decode_bytes = 12 << 20;
    options.asset_packages = Some(loaded.packages);
    options.asset_overlay = loaded.loose;
    options.resource_target = "vita".into();
    options.catalogs = loaded.catalogs.into();
    options.save_directory = Some(save_root.join(title_id));
    options.save_bytes = 8 << 20;
    options.frame_bytes = 4 << 20;
    options.media.pcm_bytes = 8 << 20;
    options.media.static_pcm_threshold_bytes = 256 << 10;
    options.media.stream_window_frames = 4096;
    options.media.queue_bytes = 1 << 20;
    options.media.mixer.max_sounds = 16;
    options.media.video.decoded_bytes = 24 << 20;
    options.media.video.queued_pictures = 2;
    options.media.video.queued_audio_frames = 4096;
    options.media.video_decoder = Some(Arc::new(reel_media::video::VitaVideoDecoderFactory));
    let story = crate::distribution::story_from_package(loaded.package)?;
    run_story(story, options, module_directory)
}

/// Run an already compiled story with native controllers and power callbacks.
pub fn run_story(
    story: reel_engine::aside::source::CompiledStory,
    options: PlayerOptions,
    module_directory: &Path,
) -> Result<(), Error> {
    use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
    let wake = Wake::new()?;
    let mut power = PowerEvents::new(wake.clone())?;
    let mut controls = native::input::Input::new()?;
    let mut gles = GlesOptions::default();
    gles.module_directory = module_directory
        .to_str()
        .ok_or("Vita module directory must be UTF-8")?
        .into();
    gles.vsync = true;
    let backend = GlesBackend::new(gles)?;
    let mut host = VitaHost::new(backend.output_size(), wake.clone());
    let assets = crate::assets::Assets::packaged_for_target(
        options.assets.clone(),
        options.asset_packages.clone().unwrap_or_default(),
        options.asset_overlay.clone(),
        "vita".into(),
    )
    .map_err(|error| format!("Vita resources: {error:?}"))?;
    let notify = host.wake_callback();
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
    let mut limits = VmLimits::default();
    limits.heap.max_bytes = 24 << 20;
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
    let mut player = Player::new(driver, backend, &mut host, &options)?;
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
    let mut input = input::Input::default();
    let mut suspended = false;
    let mut closing = false;
    let result = (|| -> Result<(), Error> {
        loop {
            while let Some(event) = power.poll()? {
                // Keep the actual notification timestamp, even when a callback
                // was queued immediately before the kernel suspended this thread.
                host.event_time = Some(MonoTime(event.monotonic_micros.saturating_mul(1000)));
                match event.kind {
                    PowerKind::Suspending if !suspended => {
                        input.cancel(&mut player)?;
                        player.suspend(&mut host)?;
                        suspended = true;
                    }
                    PowerKind::Resumed if suspended => {
                        player.resume(&mut host)?;
                        suspended = false;
                    }
                    _ => {}
                }
                host.event_time = None;
            }
            if host.signals.exit.get() || player.exit_requested() {
                if !closing {
                    player.close()?;
                    closing = true;
                }
            }
            if closing {
                player.tick(&mut host)?;
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
            let poll_deadline = MonoTime(now.0.saturating_add(16_000_000));
            input.sample(controls.sample()?, now, host.size, &mut player)?;
            player.poll_completions()?;
            player.tick(&mut host)?;
            player.prepare_initial_frame(&mut host)?;
            if host.signals.redraw.replace(false) || player.needs_redraw() {
                player.render(&mut host)?;
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
            wake.wait(Some(timeout))?;
        }
        Ok(())
    })();
    // Always close shared services before the graphics context and native wake
    // registrations disappear, including error exits from input or rendering.
    host.event_time = None;
    let cleanup = (|| -> Result<(), Error> {
        if !closing {
            player.close()?;
        }
        while !player.close_complete() {
            player.tick(&mut host)?;
            player.poll_completions()?;
            wake.wait(Some(Duration::from_millis(1)))?;
        }
        Ok(())
    })();
    // Preserve the event/render failure when closing also reports an error.
    result.and(cleanup)
}
