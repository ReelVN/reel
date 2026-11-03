//! Same-revision accepted-frame inputs for the ignored local-raster experiment.
use reel_core::budget::BudgetPool;
use reel_ui::{
    frame::{BuildLimits, FrameDraw, FrameImage, FrameOp, SavedRetainedScene, StepBudget},
    imaging::{Composite, record::Geometry},
    kurbo::{Affine, Rect},
    peniko::{Brush, Fill, ImageSampler},
    resources::ResourceLease,
};

// Deserialize the current reel-player::performance::frame_archive::FrameArchive
// envelope here: reel-render cannot depend on its player consumer. This test
// reads the same revision's artifact and does not define another archive writer.
#[derive(serde::Deserialize)]
struct FrameArchive {
    version: u32,
    logical_size: [f64; 2],
    physical_size: [u32; 2],
    scene: SavedRetainedScene,
}

pub(super) struct ImageDraw {
    pub lease: ResourceLease,
    pub bounds: Rect,
    pub transform: Affine,
    pub brush_transform: Option<Affine>,
    pub image: FrameImage,
    pub sampler: ImageSampler,
    pub composite: Composite,
    pub fill_rule: Fill,
    pub enclosing_effect: Option<(Rect, Affine)>,
    pub logical_size: [f64; 2],
    pub physical_size: [u32; 2],
}

/// Recover the selected image's actual resource, without decoding or replacing
/// its texture layout or alpha-coverage metadata with a synthetic RGBA image.
pub(super) fn load(pool: &BudgetPool, limits: BuildLimits) -> Result<ImageDraw, String> {
    let path = std::env::var_os("REEL_FRAME_ARCHIVE")
        .ok_or("set REEL_FRAME_ARCHIVE to a same-revision frame.postcard")?;
    let draw_index: usize = std::env::var("REEL_FRAME_DRAW_INDEX")
        .map_err(|error| format!("read REEL_FRAME_DRAW_INDEX: {error}"))?
        .parse()
        .map_err(|error| format!("parse REEL_FRAME_DRAW_INDEX: {error}"))?;
    let path = std::path::Path::new(&path);
    let bytes = std::fs::read(path)
        .map_err(|error| format!("read frame archive {}: {error}", path.display()))?;
    let archive: FrameArchive = postcard::from_bytes(&bytes)
        .map_err(|error| format!("decode frame archive {}: {error}", path.display()))?;
    if archive.version != 1 {
        return Err(format!(
            "expected FrameArchive version 1, got {}",
            archive.version
        ));
    }
    let mut restore = archive
        .scene
        .restore_begin(limits, pool)
        .map_err(|error| format!("begin restoring frame archive: {error:?}"))?;
    let mut budget = StepBudget {
        max_commands: 1024,
        max_units: 4096,
        max_bytes: 1 << 20,
    };
    loop {
        let progress = restore
            .step(budget)
            .map_err(|error| format!("restore frame archive: {error:?}"))?;
        if let Some(index) = progress.missing_asset {
            return Err(format!("frame archive requires non-inline asset {index}"));
        }
        if progress.build.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.build.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.build.minimum_bytes);
    }
    let restored = restore
        .finish()
        .map_err(|error| format!("finish restoring frame archive: {error:?}"))?;
    let drawing = restored.drawing();
    let command = drawing
        .commands()
        .get(draw_index)
        .ok_or_else(|| format!("frame has no command at index {draw_index}"))?;
    let FrameOp::Draw(FrameDraw::Fill {
        transform,
        fill_rule,
        brush: Brush::Image(brush),
        brush_transform,
        shape: Geometry::Rect(bounds),
        composite,
    }) = &command.op
    else {
        return Err(format!(
            "command {draw_index} is not a rectangular image fill"
        ));
    };
    let lease = drawing
        .resources()
        .leases()
        .iter()
        .find(|lease| lease.handle() == brush.image.resource)
        .ok_or_else(|| format!("command {draw_index} has no image resource lease"))?
        .clone();
    let mut groups = Vec::new();
    for command in &drawing.commands()[..draw_index] {
        match &command.op {
            FrameOp::PushGroup(group) => groups.push(group),
            FrameOp::PopGroup => {
                groups.pop();
            }
            _ => {}
        }
    }
    let enclosing_effect = groups.iter().rev().find_map(|group| {
        group
            .effect
            .as_ref()
            .map(|effect| (effect.bounds, group.effect_to_target))
    });
    Ok(ImageDraw {
        lease,
        bounds: *bounds,
        transform: *transform,
        brush_transform: *brush_transform,
        image: brush.image,
        sampler: brush.sampler,
        composite: *composite,
        fill_rule: *fill_rule,
        enclosing_effect,
        logical_size: archive.logical_size,
        physical_size: archive.physical_size,
    })
}
