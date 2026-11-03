//! Hardware rendering assertions through the production backend and native API.
mod blend_color;
mod color_capture;
mod composite_color;
#[path = "../../../reel-ui/tests/support/fixture.rs"]
#[allow(dead_code)]
mod fixture;
mod image_color;
mod mask_color;
mod present_color;
#[path = "../../../reel-ui/tests/support/r01_fixture.rs"]
#[allow(dead_code)]
mod r01_fixture;
mod render_contracts;
mod scenes;
mod video_color;

use crate::options::{Check, Options};
use clap::Parser;
use reel_render::RenderBackend;
use reel_render::gxm::{GxmBackend as DeviceBackend, GxmError as DeviceError};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vita_native::graphics as native;

type Error = Box<dyn std::error::Error>;
type Result<T = ()> = std::result::Result<T, Error>;
const SIZE: [u32; 2] = [960, 544];

fn log(message: impl std::fmt::Display) {
    eprintln!("[reel-device-checks] {message}");
}

fn directory() -> PathBuf {
    PathBuf::from("ux0:/data/reel-device-checks")
}

fn check(name: &str, run: impl FnOnce() -> Result) -> Result {
    log(format_args!("case={name} state=START"));
    let result = run();
    log(format_args!(
        "case={name} state={}",
        if result.is_ok() { "PASS" } else { "ERROR" }
    ));
    result
}

fn finish(result: Result, closed: Result) -> Result {
    match result {
        Ok(()) => closed,
        Err(error) => {
            if let Err(close) = closed {
                log(format_args!("close error={close}"));
            }
            Err(error)
        }
    }
}

fn native_case(fenced: bool) -> Result {
    let context = reel_render::gxm::create_context(&Default::default(), None)?;
    let result = (|| {
        for index in 0..3 {
            context.clear(native::Target::Screen, [0.1, index as f32 / 16., 0.3, 1.])?;
            let fence = if fenced {
                let fence = context.fence()?;
                context.flush()?;
                Some(fence)
            } else {
                None
            };
            context.present()?;
            if let Some(fence) = fence {
                let started = Instant::now();
                while !fence.poll()? {
                    if started.elapsed() > Duration::from_secs(10) {
                        return Err("fence completion timeout".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        context.finish()?;
        Ok(())
    })();
    let closed = context.close().map_err(|(_, error)| error.into());
    finish(result, closed)
}

fn renderer_checks(selected: Check) -> Result {
    let mut renderer = DeviceBackend::new(Default::default(), None)?;
    renderer.set_vsync(true)?;
    let result = (|| match selected {
        Check::All => {
            check("video_color", || video_color::run(&mut renderer))?;
            renderer.reset_session()?;
            check("blend_color", || blend_color::run(&mut renderer))?;
            renderer.reset_session()?;
            render_contracts::run(&mut renderer, None)
        }
        Check::VideoColor => video_color::run(&mut renderer),
        Check::BlendColor => blend_color::run(&mut renderer),
        Check::CompositeColor | Check::CompositeReference => {
            composite_color::run(&mut renderer, selected == Check::CompositeReference)
        }
        Check::Render => render_contracts::run(&mut renderer, None),
        case => render_contracts::run(&mut renderer, Some(&case.name())),
    })();
    let closed = renderer.close().map_err(Into::into);
    finish(result, closed)
}

fn run() -> Result {
    let mut tokens: Vec<String> = match std::fs::read_to_string(directory().join("case.txt")) {
        Ok(value) => value.split_whitespace().map(str::to_owned).collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    tokens.extend(std::env::args().skip(1));
    let options =
        Options::try_parse_from(std::iter::once("reel-device-checks".to_owned()).chain(tokens))?;
    std::fs::create_dir_all(directory())?;
    vita_native::allocator::configure_main_budget()?;
    log(format_args!(
        "BEGIN title=REELP0002 case={} source={} dirty={}",
        options.case.name(),
        env!("REEL_DEVICE_CHECKS_REVISION"),
        env!("REEL_DEVICE_CHECKS_DIRTY")
    ));
    match options.case {
        Check::All => {
            check("fence", || native_case(true))?;
            check("present_color", present_color::run)?;
            check("effect_region", present_color::effect_regions)?;
            renderer_checks(Check::All)?;
        }
        Check::ClearSwap => check("clear_swap", || native_case(false))?,
        Check::Fence => check("fence", || native_case(true))?,
        Check::PresentColor => check("present_color", present_color::run)?,
        Check::PaintRegion => check("paint_region", present_color::paint_regions)?,
        Check::EffectRegion => check("effect_region", present_color::effect_regions)?,
        case => check(&case.name(), || renderer_checks(case))?,
    }
    log("ALL SELECTED CHECKS PASS");
    Ok(())
}

pub(super) fn main() {
    std::panic::set_hook(Box::new(|info| log(format_args!("PANIC {info}"))));
    if let Err(error) = run() {
        log(format_args!("FAILED {error}"));
        std::process::exit(1);
    }
}
