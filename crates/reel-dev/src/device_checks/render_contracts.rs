//! Bounded submission, completion ordering and in-flight resource retirement.
use super::{DeviceBackend, DeviceError, Result, SIZE, check, scenes};
use reel_core::identity::FrameId;
use reel_render::{AlphaMode, PresentOutcome, RenderBackend, RenderEvent, SurfaceInfo};
use reel_ui::resources::ColorEncoding;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const CASES: &[&str] = &[
    "empty",
    "solid",
    "rgba",
    "bc1",
    "bc3",
    "bc1_region",
    "bc3_region",
    "regional_instances",
    "text",
    "text_plain",
    "text_styled",
    "offscreen",
    "mask_chain",
    "mask_threshold",
    "mask_dag",
    "mask_static",
    "resources",
];

fn poll(
    renderer: &mut DeviceBackend,
    pending: &mut VecDeque<FrameId>,
    completed: &mut usize,
) -> Result {
    let mut events = Vec::with_capacity(32);
    renderer.poll(&mut events, 32)?;
    for event in events {
        match event {
            RenderEvent::RenderFinished { frame, .. } => {
                if pending.pop_front() != Some(frame) {
                    return Err("unexpected or out-of-order frame completion".into());
                }
                *completed += 1;
            }
            RenderEvent::DeviceLost(error) => return Err(error.into()),
            _ => {}
        }
    }
    Ok(())
}

fn await_capacity(
    renderer: &mut DeviceBackend,
    pending: &mut VecDeque<FrameId>,
    completed: &mut usize,
    capacity: usize,
) -> Result {
    let deadline = Instant::now() + Duration::from_secs(10);
    while pending.len() >= capacity {
        poll(renderer, pending, completed)?;
        if pending.len() >= capacity {
            if Instant::now() >= deadline {
                return Err("frame completion timeout".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

fn case(renderer: &mut DeviceBackend, name: &str, flights: usize) -> Result {
    renderer.reset_session()?;
    let mut assets = scenes::Assets::new()?;
    let mut pending = VecDeque::with_capacity(flights);
    let mut completed = 0;
    let frames = if name.starts_with("text") {
        scenes::TEXT_REVEAL_FRAMES + 3
    } else {
        8
    };
    for index in 0..frames {
        await_capacity(renderer, &mut pending, &mut completed, flights)?;
        let frame = assets.frame(name, index)?;
        if index == 0 {
            renderer.configure(SurfaceInfo {
                physical_size: SIZE,
                scale_factor: 1.,
                surface_epoch: frame.surface_epoch(),
                color_encoding: ColorEncoding::Srgb,
                alpha_mode: AlphaMode::Opaque,
            })?;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match renderer.render_present(&frame, || {}) {
                Ok(PresentOutcome::PresentAccepted(id)) if id == frame.frame_id() => {
                    pending.push_back(id);
                    break;
                }
                Ok(outcome) => {
                    return Err(format!("{name}: unexpected acceptance {outcome:?}").into());
                }
                Err(DeviceError::Busy) => {
                    poll(renderer, &mut pending, &mut completed)?;
                    if Instant::now() >= deadline {
                        return Err("frame submission timeout".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error.into()),
            }
        }
        if name == "resources" {
            // Submitted frames retain their images after logical retirement.
            renderer.retire(&assets.versions);
            drop(frame);
            assets.retire();
        }
    }
    await_capacity(renderer, &mut pending, &mut completed, 1)?;
    if completed != frames {
        return Err("missing frame completion".into());
    }
    if name == "regional_instances" {
        for index in [0, 37, 84] {
            let pixels = super::color_capture::capture(renderer, assets.frame(name, index)?, SIZE)?;
            scenes::check_regional_pixels(&pixels)?;
        }
    }
    assets.retire();
    renderer.reset_session()?;
    let usage = renderer.usage();
    if renderer.has_pending()
        || usage.targets != 0
        || usage.cached_images != 0
        || usage.retired_images != 0
        || usage.retired_image_readers != 0
        || usage.cached_geometry != 0
    {
        return Err(format!("session resources remained after drain/reset: {usage:?}").into());
    }
    if assets.usage().bytes != 0 {
        return Err("retired fixture pixels remained after session reset".into());
    }
    Ok(())
}

pub(super) fn run(renderer: &mut DeviceBackend, selected: Option<&str>) -> Result {
    for name in CASES
        .iter()
        .copied()
        .filter(|name| selected.is_none_or(|s| s == *name))
    {
        // Two queued frames exercise retirement while a previous reader is live.
        check(name, || case(renderer, name, 2))?;
    }
    Ok(())
}
