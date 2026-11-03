//! Same-build accepted-frame artifacts for offline renderer analysis.
#[cfg(target_os = "vita")]
use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
use reel_ui::frame::SavedRetainedScene;
#[cfg(target_os = "vita")]
use reel_ui::frame::{BuildLimits, FrameDrawing, RenderFrame, RetainBuild, StepBudget};
#[cfg(target_os = "vita")]
use std::{
    io,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

/// Dimensions and a self-contained immutable drawing, encoded with postcard.
/// Resource pixels are embedded; decoding uses the matching engine revision.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct FrameArchive {
    /// Archive envelope version, currently 1.
    pub version: u32,
    /// Author-facing surface size before physical scaling.
    pub logical_size: [f64; 2],
    /// Physical render target dimensions.
    pub physical_size: [u32; 2],
    /// Existing UI archive representation, including transitions and resources.
    pub scene: SavedRetainedScene,
}

/// Stream an accepted frame after all timed repetitions have completed.
/// Retention and archive metadata have a combined 32 MiB limit; immutable pixel
/// storage keeps its existing leases and is streamed through a 64 KiB buffer.
/// Released CPU pixels reload through the session's versioned asset service and
/// its existing resource budgets, outside the measurement intervals.
#[cfg(target_os = "vita")]
pub(crate) fn save(
    frame: &RenderFrame,
    assets: &crate::assets::Assets,
    path: &Path,
) -> io::Result<()> {
    let pool = BudgetPool::new(
        BudgetCategory::Snapshot,
        QueueLimits {
            max_requests: 1024,
            max_bytes: 32 << 20,
        },
    );
    let limits = BuildLimits {
        max_bytes: 16 << 20,
    };
    let error = |e| io::Error::other(format!("retain measured frame: {e:?}"));
    let mut build = RetainBuild::new(frame, None, limits, &pool).map_err(error)?;
    let mut budget = StepBudget {
        max_commands: 1024,
        max_units: 4096,
        max_bytes: 1 << 20,
    };
    loop {
        let progress = build.step(budget).map_err(error)?;
        if progress.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
    let retained = build.finish().map_err(error)?;
    recover_pixels(
        retained.drawing(),
        assets,
        Instant::now() + Duration::from_secs(30),
    )?;
    let archive = FrameArchive {
        version: 1,
        logical_size: frame.info().logical_size,
        physical_size: frame.info().physical_size,
        scene: retained
            .snapshot(|_| None)
            .map_err(|e| io::Error::other(format!("snapshot measured frame: {e:?}")))?,
    };
    let mut writer = io::BufWriter::with_capacity(64 * 1024, std::fs::File::create(path)?);
    postcard::to_io(&archive, &mut writer).map_err(io::Error::other)?;
    writer.flush()
}

#[cfg(target_os = "vita")]
fn recover_pixels(
    drawing: &FrameDrawing,
    assets: &crate::assets::Assets,
    deadline: Instant,
) -> io::Result<()> {
    for lease in drawing.resources().leases() {
        let lease = match lease.view() {
            reel_ui::resources::ResourceView::AtlasView { backing, .. } => backing,
            _ => lease,
        };
        if lease.pixel_byte_len().is_none() || lease.pixels_resident() {
            continue;
        }
        let origin = assets.retained_asset(lease).ok_or_else(|| {
            io::Error::other(format!(
                "frame pixels have no reloadable origin: {:?}",
                lease.version()
            ))
        })?;
        let load = assets.request_version(origin.version, crate::assets::AssetKind::Image);
        let result = (|| {
            loop {
                if let Some(replacement) = assets.poll(load).map_err(io::Error::other)? {
                    return lease.restore_pixels(&replacement).map_err(|e| {
                        io::Error::other(format!(
                            "restore frame pixels {:?}: {e:?}",
                            lease.version()
                        ))
                    });
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("reload frame pixels {:?}", lease.version()),
                    ));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        })();
        if result.is_err() {
            assets.cancel(load);
        }
        result?;
    }
    for transition in drawing.transitions() {
        recover_pixels(transition.previous.drawing(), assets, deadline)?;
    }
    Ok(())
}
