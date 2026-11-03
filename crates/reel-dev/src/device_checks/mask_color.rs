//! Device reference images for sampled mask composition, including padded targets.
use super::{DeviceBackend, Error, Result, color_capture::capture, log, r01_fixture};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    core::PaintImageMask,
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::record::{Clip, Draw, Geometry, Group, Scene},
    kurbo::{Affine, Rect},
    peniko::{Brush, Color, ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceRegistry},
    stage::{MaskChannel, MaskOp, MaskProgram},
};
use std::sync::Arc;

const KINDS: [&str; 7] = ["threshold", "chain_a", "chain_g", "aa", "ag", "ga", "gg"];

fn error(value: impl std::fmt::Debug) -> Error {
    format!("{value:?}").into()
}

fn program(kind: usize) -> Result<Arc<MaskProgram>> {
    if kind == 0 {
        return Ok(MaskProgram::image("mask-color-0".into()));
    }
    let channel = |gray| {
        if gray {
            MaskChannel::Gray
        } else {
            MaskChannel::Alpha
        }
    };
    let mut ops = vec![
        MaskOp::Constant(0),
        MaskOp::Constant(1_f32.to_bits()),
        MaskOp::Parameter(2),
        MaskOp::Sample {
            image: 0,
            channel: channel(matches!(kind, 2 | 5 | 6)),
            coordinates: [2, 0, 1, 1],
            repeat: 1,
        },
        MaskOp::Constant(0.75_f32.to_bits()),
        MaskOp::Multiply(3, 4),
    ];
    let mut images = vec!["mask-color-0".into()];
    if kind >= 3 {
        ops.push(MaskOp::Sample {
            image: 1,
            channel: channel(matches!(kind, 4 | 6)),
            coordinates: [0, 2, 1, 1],
            repeat: 2,
        });
        ops.push(MaskOp::Multiply(5, 6));
        images.push("mask-color-1".into());
    }
    MaskProgram::new(ops, images).map_err(error)
}

fn frame(size: [u32; 2], kind: usize, clip: usize, id: u64) -> Result<RenderFrame> {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 4096,
            max_bytes: 64 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(8, 1 << 20).map_err(error)?;
    let mut handles = Vec::new();
    let mut images = Vec::new();
    for image in 0..if kind >= 3 { 2 } else { 1 } {
        let mut pixels = Vec::new();
        for y in 0..8_u8 {
            for x in 0..8_u8 {
                // Independent RGB and alpha gradients distinguish AA/AG/GA/GG.
                pixels.extend_from_slice(&[
                    13 + x * 29,
                    9 + y * 31,
                    17 + ((x + y + image * 3) % 8) * 27,
                    31 + ((x * 3 + y + image * 5) % 8) * 32,
                ]);
            }
        }
        let owned = OwnedBytes::new(pixels, pool.reserve(8 * 8 * 4).map_err(error)?)
            .map_err(|e| error(e.reason))?;
        let handle = registry
            .register_image(
                ResourceVersion {
                    key: AssetKey(format!("mask-color-{image}")),
                    version: ContentVersion([image + 1; 32]),
                },
                owned,
                [8, 8],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Srgb,
            )
            .map_err(error)?;
        images.push(registry.acquire(handle).map_err(error)?);
        handles.push(handle);
    }
    let [w, h] = size.map(f64::from);
    let full = Rect::new(0., 0., w, h);
    let bounds = if size == [1, 1] {
        full
    } else {
        match clip {
            0 => full,
            1 => Rect::new(3., 5., w - 4., h - 3.),
            _ => Rect::new(-3.2, 4.3, w - 1.3, h + 2.6),
        }
    };
    let mut scene = Scene::new();
    scene.push_group(Group {
        clip: Some(Clip::Fill {
            transform: Affine::IDENTITY,
            shape: Geometry::Rect(bounds),
            fill_rule: Default::default(),
        }),
        ..Default::default()
    });
    for (bounds, color) in [
        (full, Color::from_rgba8(240, 53, 19, 173)),
        (
            Rect::new(w * 0.3, h * 0.2, w, h * 0.8),
            Color::from_rgba8(31, 97, 221, 119),
        ),
    ] {
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(color),
            brush_transform: None,
            shape: Geometry::Rect(bounds),
            composite: Default::default(),
        });
    }
    scene.pop_group();
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    widget.opacity = [1., 0.5, 0.1][clip];
    widget.mask = Some(PaintImageMask {
        program: program(kind)?,
        images: images.into(),
        bounds: full,
        parameters: [0.53, 0.12, -0.17, 0., if clip == 2 { 1. } else { 0. }],
    });
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(id)).map_err(error)?,
        registry.freeze(&handles, &pool).map_err(error)?,
        FrameInfo {
            logical_size: [w, h],
            physical_size: size,
        },
        r01_fixture::limits(),
        &pool,
    )
    .map_err(error)?;
    let mut budget = r01_fixture::step_budget();
    loop {
        let progress = build.step(budget).map_err(error)?;
        if progress.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
    build.finish().map_err(error)
}

pub(super) fn run(renderer: &mut DeviceBackend, reference: bool) -> Result {
    let directory = super::directory().join("mask-color-v1");
    if reference {
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("source.txt"),
            env!("REEL_DEVICE_CHECKS_REVISION"),
        )?;
    } else {
        log(format_args!(
            "MASK COLOR reference_source={}",
            std::fs::read_to_string(directory.join("source.txt"))?
        ));
    }
    let mut cases = 0;
    let mut maximum = 0;
    let mut pixels = 0;
    for size in [[1, 1], [31, 33], [137, 93]] {
        for (kind, name) in KINDS.into_iter().enumerate() {
            for clip in 0..3 {
                let actual = capture(renderer, frame(size, kind, clip, 20_000 + cases)?, size)?;
                let path = directory.join(format!("{cases}.rgba"));
                let mut local_max = 0;
                if reference {
                    std::fs::write(path, &actual)?;
                } else {
                    let expected = std::fs::read(path)?;
                    if actual.len() != expected.len() {
                        return Err(format!("mask reference size mismatch case={cases}").into());
                    }
                    for (a, b) in actual.iter().zip(expected) {
                        local_max = local_max.max(a.abs_diff(b));
                    }
                }
                maximum = maximum.max(local_max);
                pixels += size[0] * size[1];
                log(format_args!(
                    "MASK SAMPLE case={cases} kind={name} size={}x{} clip={clip} max_error={local_max}",
                    size[0], size[1]
                ));
                cases += 1;
            }
        }
    }
    log(format_args!(
        "MASK COLOR reference={reference} cases={cases} pixels={pixels} max_error={maximum} tolerance=1"
    ));
    if maximum > 1 {
        return Err("sampled mask differs from device reference beyond one byte".into());
    }
    Ok(())
}
