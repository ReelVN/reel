//! Compare current-pixel blending with the sampled, isolated-clip path on device.
use super::{DeviceBackend, Error, Result, color_capture::capture, log, r01_fixture};
use reel_core::identity::FrameId;
use reel_ui::{
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::{
        Composite,
        record::{Clip, Draw, Geometry, Group, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{BlendMode, Brush, Color, Compose, Mix},
};

const SIZE: [u32; 2] = [128, 128];
const COLORS: [[u8; 4]; 16] = [
    [0, 0, 0, 0],
    [255, 255, 255, 0],
    [0, 0, 0, 255],
    [255, 255, 255, 255],
    [255, 0, 0, 1],
    [0, 255, 0, 127],
    [0, 0, 255, 128],
    [255, 128, 0, 254],
    [1, 2, 3, 255],
    [10, 11, 12, 255],
    [63, 127, 191, 63],
    [127, 128, 129, 128],
    [250, 251, 252, 253],
    [128, 64, 32, 160],
    [32, 128, 224, 240],
    [254, 1, 127, 16],
];

fn error(value: impl std::fmt::Debug) -> Error {
    format!("{value:?}").into()
}

fn fill(scene: &mut Scene, bounds: Rect, rgba: [u8; 4]) {
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])),
        brush_transform: None,
        shape: Geometry::Rect(bounds),
        composite: Default::default(),
    });
}

fn frame(blend: BlendMode, alpha: f32, sampled: bool, id: u64) -> Result<RenderFrame> {
    let mut scene = Scene::new();
    for (row, backdrop) in COLORS.into_iter().enumerate() {
        for col in 0..16 {
            fill(
                &mut scene,
                Rect::new(
                    col as f64 * 8.,
                    row as f64 * 8.,
                    col as f64 * 8. + 8.,
                    row as f64 * 8. + 8.,
                ),
                backdrop,
            );
        }
    }
    scene.push_group(Group {
        // The identity clip keeps the existing sampled destination path while
        // covering every pixel. Both paths retain the same isolated source.
        clip: sampled.then_some(Clip::Fill {
            transform: Affine::IDENTITY,
            shape: Geometry::Rect(Rect::new(0., 0., 128., 128.)),
            fill_rule: Default::default(),
        }),
        composite: Composite { alpha, blend },
        ..Default::default()
    });
    for row in 0..16 {
        for (col, source) in COLORS.into_iter().enumerate() {
            let x = col as f64 * 8.;
            let y = row as f64 * 8.;
            fill(
                &mut scene,
                Rect::new(x + 1., y + 1., x + 7., y + 7.),
                source,
            );
            // Overlap forces a real source isolation and checks its quantization.
            fill(
                &mut scene,
                Rect::new(x + 4., y + 3., x + 8., y + 8.),
                source,
            );
        }
    }
    scene.pop_group();
    // A later translucent draw checks continuation of the parent surface.
    fill(
        &mut scene,
        Rect::new(1., 61., 127., 65.),
        [180, 70, 210, 93],
    );
    build(scene, id)
}

fn build(scene: Scene, id: u64) -> Result<RenderFrame> {
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let pool = r01_fixture::pool();
    let mut build = FrameBuild::new(
        bridge.record(FrameId(id)).map_err(error)?,
        r01_fixture::empty_snapshot(&pool),
        FrameInfo {
            logical_size: SIZE.map(f64::from),
            physical_size: SIZE,
        },
        r01_fixture::limits(),
        &pool,
    )
    .map_err(error)?;
    while !build
        .step(r01_fixture::step_budget())
        .map_err(error)?
        .complete
    {}
    build.finish().map_err(error)
}

fn scheduled_frame(separate_scopes: bool, id: u64) -> Result<RenderFrame> {
    let mut scene = Scene::new();
    fill(
        &mut scene,
        Rect::new(0., 0., 128., 128.),
        [40, 90, 160, 255],
    );
    for (index, blend) in [Mix::Multiply, Mix::Screen, Mix::Overlay, Mix::Difference]
        .into_iter()
        .enumerate()
    {
        let x = 8. + 17. * index as f64;
        let y = 12. + 11. * index as f64;
        if separate_scopes {
            scene.push_clip(Clip::Fill {
                transform: Affine::IDENTITY,
                shape: Geometry::Rect(Rect::new(0., 0., 128., 128.)),
                fill_rule: Default::default(),
            });
        }
        scene.push_group(Group {
            composite: Composite {
                alpha: 0.75,
                blend: blend.into(),
            },
            ..Default::default()
        });
        fill(
            &mut scene,
            Rect::new(x, y, x + 48., y + 48.),
            COLORS[10 + index],
        );
        fill(
            &mut scene,
            Rect::new(x + 12., y + 8., x + 52., y + 44.),
            COLORS[6 + index],
        );
        scene.pop_group();
        if separate_scopes {
            scene.pop_clip();
        }
    }
    build(scene, id)
}

fn scheduled_colors(renderer: &mut DeviceBackend) -> Result {
    let previous = reel_render::profiling::shared_source_atlas();
    let result = scheduled_color_cases(renderer);
    reel_render::profiling::set_shared_source_atlas(previous);
    result
}

fn scheduled_color_cases(renderer: &mut DeviceBackend) -> Result {
    let reference = capture(renderer, scheduled_frame(true, 1002)?, SIZE)?;
    let mut failed = false;
    for shared in [false, true] {
        reel_render::profiling::set_shared_source_atlas(shared);
        let before = reel_core::profile::Sample::snapshot();
        let direct = capture(
            renderer,
            scheduled_frame(false, 1003 + u64::from(shared))?,
            SIZE,
        )?;
        let batch_calls = reel_core::profile::Sample::snapshot().delta(before).calls
            [reel_core::profile::Phase::CompositeBatch as usize];
        let changed = direct
            .iter()
            .zip(&reference)
            .filter(|(a, b)| a != b)
            .count();
        let max_error = direct
            .iter()
            .zip(&reference)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        log(format_args!(
            "BATCH COLOR shared={shared} batch_calls={batch_calls} changed_bytes={changed} max_error={max_error}"
        ));
        if batch_calls == 0 || changed != 0 {
            std::fs::write(
                super::directory().join(format!("batch-shared-{shared}.rgba")),
                direct,
            )?;
            std::fs::write(super::directory().join("batch-reference.rgba"), &reference)?;
            failed = true;
        }
    }
    if failed {
        Err("scheduled blend pixels differ or the batch was not exercised".into())
    } else {
        Ok(())
    }
}

pub(super) fn run(renderer: &mut DeviceBackend) -> Result {
    scheduled_colors(renderer)?;
    super::image_color::run(renderer)?;
    let modes = [
        BlendMode {
            mix: Mix::Normal,
            compose: Compose::PlusLighter,
        },
        Mix::Multiply.into(),
        Mix::Screen.into(),
        Mix::Overlay.into(),
        Mix::HardLight.into(),
        Mix::ColorDodge.into(),
        Mix::Lighten.into(),
        Mix::Difference.into(),
        Mix::SoftLight.into(),
    ];
    let mut cases = 0;
    let mut changed = 0usize;
    let mut max_error = 0;
    for mode in modes {
        for alpha in [0., 0.1, 0.5, 0.9, 1.] {
            let direct = capture(renderer, frame(mode, alpha, false, cases * 2 + 1)?, SIZE)?;
            let sampled = capture(renderer, frame(mode, alpha, true, cases * 2 + 2)?, SIZE)?;
            if direct.len() != sampled.len() {
                return Err("blend readback size mismatch".into());
            }
            let mut local_changed = 0;
            let mut local_max = 0;
            for (a, b) in direct.iter().zip(&sampled) {
                let distance = a.abs_diff(*b);
                local_max = local_max.max(distance);
                local_changed += usize::from(distance != 0);
            }
            changed += local_changed;
            max_error = max_error.max(local_max);
            log(format_args!(
                "BLEND SAMPLE mode={mode:?} alpha={alpha} changed={local_changed} max_error={local_max}"
            ));
            cases += 1;
        }
    }
    log(format_args!(
        "BLEND COLOR cases={cases} pixels={} changed_bytes={changed} max_error={max_error}",
        cases * u64::from(SIZE[0] * SIZE[1])
    ));
    if changed != 0 {
        return Err("blend paths differ; inspect numerical report".into());
    }
    Ok(())
}
