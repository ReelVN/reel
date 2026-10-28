//! Temporary device experiment; delegates drawing to production Reel/native code.
#![allow(dead_code)]
#[path = "../../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[path = "../../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;
#[path = "scenes.rs"]
mod scenes;
#[path = "timing.rs"]
mod timing;

use reel_render::gxm::{GxmBackend as DeviceBackend, GxmError as DeviceError};
use reel_render::{AlphaMode, PresentOutcome, RenderBackend, RenderEvent, SurfaceInfo};
use reel_ui::resources::ColorEncoding;
use std::{
    collections::VecDeque,
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use vita_native::graphics as native;

type Error = Box<dyn std::error::Error>;
type Result<T = ()> = std::result::Result<T, Error>;
const SIZE: [u32; 2] = [960, 544];
const CASES: &[&str] = &[
    "clear_swap",
    "fence",
    "rgba",
    "bc1",
    "bc3",
    "bc1_region",
    "bc3_region",
    "text",
    "offscreen",
    "mask_chain",
    "mask_threshold",
    "mask_dag",
    "mask_static",
    "resources",
];

fn log(message: impl std::fmt::Display) {
    eprintln!("[reel-gpu-probe] {message}");
}

fn call<T>(label: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    log(format_args!("{label} BEFORE"));
    let start = Instant::now();
    let result = f();
    match &result {
        Ok(_) => log(format_args!(
            "{label} AFTER us={}",
            start.elapsed().as_micros()
        )),
        Err(error) => log(format_args!("{label} ERROR {error}")),
    }
    result
}

fn directory() -> PathBuf {
    PathBuf::from("ux0:/data/reel-probe")
}

fn marker(case: &str, state: &str) -> Result {
    // Disk work occurs only at case boundaries. A frozen start survives reboot.
    let directory = directory();
    std::fs::create_dir_all(&directory)?;
    let line = format!("case={case} state={state}\n");
    reel_storage::publish(&directory.join("status.txt"), true, |file| {
        file.write_all(line.as_bytes())
    })?;
    let mut history = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("cases.log"))?;
    history.write_all(line.as_bytes())?;
    history.flush()?;
    drop(history);
    Ok(())
}

fn checkpoint(enabled: bool, case: &str, frame: usize, step: &str) -> Result {
    if enabled {
        let line = format!("case={case} frame={frame} step={step}\n");
        reel_storage::publish(&directory().join("checkpoint.txt"), true, |file| {
            file.write_all(line.as_bytes())
        })?;
        log(line.trim_end());
    }
    Ok(())
}

struct Sampler {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Sampler {
    fn start(trace: bool) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new()
            .name("gpu-probe".into())
            .stack_size(64 << 10)
            .spawn(move || {
                // Reuse production fixed atomic phases; this thread never touches GPU state.
                use reel_core::profile::{NAMES, Sample};
                let start = Instant::now();
                let mut previous = Sample::snapshot();
                while !stopped.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(1));
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    let current = Sample::snapshot();
                    let delta = current.delta(previous);
                    previous = current;
                    let active: Vec<_> = Sample::active()
                        .into_iter()
                        .enumerate()
                        .filter_map(|(i, v)| v.map(|v| format!("{}:{}ms", NAMES[i], v / 1000)))
                        .collect();
                    let work: Vec<_> = NAMES
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| delta.calls[*i] != 0)
                        .map(|(i, n)| format!("{n}:{}/{}", delta.micros[i], delta.calls[i]))
                        .collect();
                    let line = format!(
                        "sample t_ms={} active={} work={}",
                        start.elapsed().as_millis(),
                        active.join(","),
                        work.join(",")
                    );
                    log(&line);
                    if trace
                        && let Err(error) =
                            reel_storage::publish(&directory().join("active.txt"), true, |file| {
                                file.write_all(line.as_bytes())
                            })
                    {
                        log(format_args!("sample publication failed: {error}"));
                        break;
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn native_case(case: &str, frames: usize) -> Result {
    let context = call("native.context", || {
        let context = reel_render::gxm::create_context(&Default::default(), None, None)?;
        Ok(context)
    })?;
    log(format_args!(
        "driver={:?} caps={:?}",
        context.driver_info()?,
        context.capabilities()
    ));
    for index in 0..frames {
        let label = format!("case={case} frame={index}");
        call(&format!("{label} clear"), || {
            Ok(context.clear(
                native::Target::Screen,
                [0.1, (index % 8) as f32 / 16., 0.3, 1.],
            )?)
        })?;
        let fence = if case == "fence" {
            let fence = call(&format!("{label} fence.create"), || Ok(context.fence()?))?;
            call(&format!("{label} flush"), || Ok(context.flush()?))?;
            Some(fence)
        } else {
            None
        };
        call(&format!("{label} present"), || Ok(context.present()?))?;
        if let Some(fence) = fence {
            call(&format!("{label} fence.poll_until_complete"), || {
                let start = Instant::now();
                while !fence.poll()? {
                    if start.elapsed() > Duration::from_secs(10) {
                        return Err("fence completion timeout".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            })?;
            call(&format!("{label} fence.drop"), || {
                drop(fence);
                Ok(())
            })?;
        }
    }
    call("native.finish", || Ok(context.finish()?))?;
    call("native.close", || {
        context.close().map_err(|(_, error)| error.into())
    })
}

fn poll_frames(
    renderer: &mut DeviceBackend,
    pending: &mut VecDeque<reel_core::identity::FrameId>,
    events: &mut Vec<RenderEvent<DeviceError>>,
    completed: &mut usize,
) -> Result {
    events.clear();
    renderer.poll(events, 32)?;
    for event in events.drain(..) {
        match event {
            RenderEvent::RenderFinished { frame, .. } => {
                if pending.pop_front() != Some(frame) {
                    return Err("completion order".into());
                }
                *completed += 1;
            }
            RenderEvent::DeviceLost(error) => return Err(error.into()),
            _ => {}
        }
    }
    Ok(())
}

fn renderer_case(
    renderer: &mut DeviceBackend,
    case: &str,
    frames: usize,
    trace: bool,
    vsync: bool,
    flights: usize,
) -> Result {
    renderer.set_vsync(vsync)?;
    let mut assets = scenes::Assets::new()?;
    let mut pending = VecDeque::with_capacity(flights);
    let mut events = Vec::with_capacity(32);
    let mut measurements = timing::Frames::new(frames);
    let mut peak_pending = 0;
    let mut completed = 0;
    let mut polls = 0;
    let mut geometry_uploads = reel_core::profile::GeometryUploads::snapshot();
    let mut warm_uploads = reel_core::profile::GeometryUploads::default();
    let warm_after = if case == "text" {
        scenes::TEXT_REVEAL_FRAMES
    } else {
        8
    };
    let total = Instant::now();
    for index in 0..frames {
        let started = Instant::now();
        let mut waiting = Duration::ZERO;
        while pending.len() >= flights {
            let wait = Instant::now();
            poll_frames(renderer, &mut pending, &mut events, &mut completed)?;
            polls += 1;
            if pending.len() >= flights {
                if started.elapsed() > Duration::from_secs(10) {
                    return Err("flight timeout".into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            waiting += wait.elapsed();
        }
        checkpoint(trace, case, index, "build")?;
        let build_start = Instant::now();
        let frame = assets.frame(case, index)?;
        if index == 0 {
            renderer.configure(SurfaceInfo {
                physical_size: SIZE,
                scale_factor: 1.,
                surface_epoch: frame.surface_epoch(),
                color_encoding: ColorEncoding::Srgb,
                alpha_mode: AlphaMode::Opaque,
            })?;
        }
        let built = Instant::now();
        checkpoint(trace, case, index, "render")?;
        let (submit, present) = loop {
            let start = Instant::now();
            let mut present_start = start;
            let mut checkpoint_error = None;
            let outcome = renderer.render_present(&frame, || {
                present_start = Instant::now();
                checkpoint_error = checkpoint(trace, case, index, "present").err();
            });
            if let Some(error) = checkpoint_error {
                return Err(error);
            }
            match outcome {
                Ok(PresentOutcome::PresentAccepted(id)) => {
                    if id != frame.frame_id() {
                        return Err("frame identity".into());
                    }
                    pending.push_back(id);
                    peak_pending = peak_pending.max(pending.len());
                    break (present_start.duration_since(start), present_start.elapsed());
                }
                Ok(outcome) => return Err(format!("{case}: unexpected {outcome:?}").into()),
                Err(DeviceError::Busy) => {
                    poll_frames(renderer, &mut pending, &mut events, &mut completed)?;
                    polls += 1;
                    if started.elapsed() > Duration::from_secs(10) {
                        return Err("submission timeout".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                    waiting += start.elapsed();
                }
                Err(error) => return Err(error.into()),
            }
        };
        if case == "resources" {
            renderer.retire(&assets.versions);
            drop(frame);
            assets.retire();
        }
        checkpoint(trace, case, index, "submitted")?;
        let uploads = reel_core::profile::GeometryUploads::snapshot();
        if index >= warm_after {
            let delta = uploads.delta(geometry_uploads);
            warm_uploads.calls += delta.calls;
            warm_uploads.bytes += delta.bytes;
        }
        geometry_uploads = uploads;
        measurements.push([
            built.duration_since(build_start),
            submit,
            present,
            waiting,
            started.elapsed(),
        ]);
    }
    let drain = Instant::now();
    while !pending.is_empty() {
        poll_frames(renderer, &mut pending, &mut events, &mut completed)?;
        polls += 1;
        if !pending.is_empty() {
            if drain.elapsed() > Duration::from_secs(10) {
                return Err("drain timeout".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let elapsed = total.elapsed();
    if completed != frames {
        return Err("missing completions".into());
    }
    if trace {
        log(format_args!(
            "CASE {case} diagnostic checkpoints enabled; timing excluded"
        ));
    } else {
        measurements.report(
            &format!("{case}-vsync{}-flights{flights}", u8::from(vsync)),
            warm_after,
        )?;
    }
    log(format_args!(
        "CASE {case} geometry_upload_warm_frames={} calls={} bytes={}",
        frames.saturating_sub(warm_after),
        warm_uploads.calls,
        warm_uploads.bytes
    ));
    log(format_args!(
        "CASE {case} vsync={vsync} submitted={frames} completed={completed} flights_peak={peak_pending}/{flights} polls={polls} elapsed_us={} drain_us={} accounted_usage={:?} asset_usage={:?}",
        elapsed.as_micros(),
        drain.elapsed().as_micros(),
        renderer.usage(),
        assets.usage()
    ));
    Ok(())
}

fn render_cases(
    renderer: &mut DeviceBackend,
    selected: &str,
    frames: usize,
    trace: bool,
    vsync: bool,
    flights: usize,
) -> Result {
    let (vsync, flights) = if selected == "matrix" {
        (true, 1)
    } else {
        (vsync, flights)
    };
    for case in CASES.iter().copied().filter(|case| {
        (matches!(selected, "all" | "render" | "matrix")
            && !matches!(*case, "clear_swap" | "fence"))
            || *case == selected
    }) {
        let label = format!("{case}-vsync{}-flights{flights}", u8::from(vsync));
        marker(&label, "START")?;
        log(format_args!("CASE {label} START"));
        let before = reel_core::profile::Sample::snapshot();
        checkpoint(trace, case, 0, "reset_session")?;
        renderer.reset_session()?;
        let result = renderer_case(renderer, case, frames, trace, vsync, flights);
        marker(&label, if result.is_ok() { "PASS" } else { "ERROR" })?;
        let delta = reel_core::profile::Sample::snapshot().delta(before);
        for (index, name) in reel_core::profile::NAMES.iter().enumerate() {
            if delta.calls[index] != 0 {
                log(format_args!(
                    "CASE {label} phase={name} us={} calls={}",
                    delta.micros[index], delta.calls[index]
                ));
            }
        }
        result?;
        log(format_args!("CASE {label} PASS"));
    }
    if selected == "matrix" {
        // Compare identical workloads; baseline cases already measured vsync/one flight.
        for (vsync, flights) in [(true, 2), (false, 1), (false, 2)] {
            for case in ["offscreen", "mask_static", "text"] {
                render_cases(renderer, case, frames, trace, vsync, flights)?;
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "vita")]
fn package_case(runtime: &mut reel_player::platform::vita::Runtime, case: &str) -> Result {
    use reel_player::saves::{DirectorySaves, SaveStorage};
    marker(case, "START")?;
    log(format_args!(
        "MEMORY {case} start heap={:?} free={:?}",
        vita_native::allocator::stats(),
        vita_native::app::memory::free_memory()
    ));
    runtime.renderer().set_vsync(true)?;
    let save_directory = directory().join(case);
    let saves = DirectorySaves::new(&save_directory, 8 << 20);
    saves
        .delete_data("probe-completed")
        .map_err(|error| error.to_string())?;
    let result = runtime
        .run_package(
            &std::path::Path::new("app0:/").join(case).join("game.asb"),
            &save_directory,
        )
        .and_then(|()| {
            if saves
                .read_data("probe-completed")
                .map_err(|error| error.to_string())?
                .is_none()
            {
                return Err(format!("{case} ended before completion").into());
            }
            Ok(())
        });
    log(format_args!(
        "MEMORY {case} end heap={:?} free={:?}",
        vita_native::allocator::stats(),
        vita_native::app::memory::free_memory()
    ));
    marker(case, if result.is_ok() { "PASS" } else { "ERROR" })?;
    result
}

fn run() -> Result {
    let mut selected = "authored".to_owned();
    let mut frames = 120;
    let mut trace = false;
    let mut vsync = true;
    let mut flights = 1;
    let mut tokens: Vec<String> = std::fs::read_to_string(directory().join("case.txt"))
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    tokens.extend(std::env::args().skip(1));
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "trace" | "--trace" => trace = true,
            "--case" => {
                index += 1;
                selected = tokens.get(index).ok_or("missing --case value")?.clone();
            }
            "--frames" => {
                index += 1;
                frames = tokens.get(index).ok_or("missing --frames value")?.parse()?;
            }
            "vsync=on" => vsync = true,
            "vsync=off" => vsync = false,
            value if value.starts_with("flights=") => flights = value[8..].parse()?,
            value if value.starts_with("frames=") => frames = value[7..].parse()?,
            value => selected = value.to_owned(),
        }
        index += 1;
    }
    if !(1..=2).contains(&flights) {
        return Err("flights must be 1 or 2".into());
    }
    if !(1..=10_000).contains(&frames) {
        return Err("frames must be in 1..10000".into());
    }
    if !matches!(
        selected.as_str(),
        "authored" | "all" | "render" | "matrix" | "media"
    ) && !CASES.contains(&selected.as_str())
    {
        return Err(format!(
            "unknown case {selected}; available: authored,all,render,matrix,media,{}",
            CASES.join(",")
        )
        .into());
    }
    log(format_args!(
        "BEGIN title=REELP0002 case={selected} frames={frames} trace={trace} vsync={vsync} flights={flights} source={} dirty={} no_readback=true",
        env!("REEL_PROBE_REVISION"),
        env!("REEL_PROBE_DIRTY")
    ));
    // Package sessions use the player's sampler, including VM task locations.
    let _sampler = (selected != "authored")
        .then(|| Sampler::start(trace))
        .transpose()?;
    if matches!(selected.as_str(), "clear_swap" | "fence") {
        marker(&selected, "START")?;
        let result = native_case(&selected, frames);
        marker(&selected, if result.is_ok() { "PASS" } else { "ERROR" })?;
        result?;
    } else {
        reel_player::platform::vita::Runtime::run(|runtime| {
            if selected == "authored" {
                return package_case(runtime, "authored");
            }
            render_cases(runtime.renderer(), &selected, frames, trace, vsync, flights)?;
            if selected == "all" {
                package_case(runtime, "authored")?;
            } else if matches!(selected.as_str(), "matrix" | "media") {
                package_case(runtime, "media")?;
            }
            Ok(())
        })?;
    }
    log("ALL SELECTED CASES PASS");
    Ok(())
}

pub(super) fn main() {
    std::panic::set_hook(Box::new(|info| log(format_args!("PANIC {info}"))));
    if let Err(error) = run() {
        log(format_args!("FAILED {error}"));
        std::process::exit(1);
    }
}
