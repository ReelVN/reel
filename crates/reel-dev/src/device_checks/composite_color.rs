//! Paired device readback for rectangular composition and retained transitions.
use super::{DeviceBackend, Error, Result, color_capture::capture, log, r01_fixture};
use reel_core::identity::FrameId;
use reel_ui::{
    core::PaintImageMask,
    frame::{FrameBuild, FrameInfo, FrameTransition, RenderFrame, RetainBuild, RetainedScene},
    imaging::{
        Composite,
        record::{Clip, Draw, Geometry, Group, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, Color, Mix},
    stage::{MaskOp, MaskProgram},
};

// Both dimensions leave a partial hardware tile and padded texture storage.
const SIZE: [u32; 2] = [137, 93];

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

fn scene(clip: usize, alpha: f32, screen: bool, alternate: bool) -> Scene {
    let mut scene = Scene::new();
    fill(
        &mut scene,
        Rect::new(0., 0., 137., 93.),
        [24, 100, 180, 211],
    );
    let (bounds, transform) = match clip {
        0 => (Rect::new(0., 0., 137., 93.), Affine::IDENTITY),
        1 => (Rect::new(32., 32., 128., 93.), Affine::IDENTITY),
        2 => (Rect::new(7., 9., 133., 87.), Affine::IDENTITY),
        3 => (Rect::new(7.2, 9.7, 132.8, 86.3), Affine::IDENTITY),
        _ => (Rect::new(7., 9., 123., 77.), Affine::rotate(0.09)),
    };
    scene.push_group(Group {
        clip: Some(Clip::Fill {
            transform,
            shape: Geometry::Rect(bounds),
            fill_rule: Default::default(),
        }),
        ..Default::default()
    });
    scene.push_group(Group {
        composite: Composite {
            alpha,
            blend: if screen {
                Mix::Screen.into()
            } else {
                Default::default()
            },
        },
        ..Default::default()
    });
    for row in 0..7 {
        for column in 0..11 {
            let x = f64::from(column * 13);
            let y = f64::from(row * 15);
            let colors = if alternate {
                [20 + column as u8 * 17, 235, 30 + row as u8 * 31, 173]
            } else {
                [240, 15 + row as u8 * 31, 20 + column as u8 * 17, 119]
            };
            fill(&mut scene, Rect::new(x, y, x + 13., y + 15.), colors);
            fill(
                &mut scene,
                Rect::new(x + 5., y + 4., x + 11., y + 13.),
                colors,
            );
        }
    }
    scene.pop_group();
    scene.pop_group();
    fill(
        &mut scene,
        Rect::new(2., 45., 136., 49.),
        [180, 70, 210, 93],
    );
    scene
}

fn frame(scene: Scene, id: u64, transitions: Vec<FrameTransition>) -> Result<RenderFrame> {
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
    build.set_transitions(transitions).map_err(error)?;
    while !build
        .step(r01_fixture::step_budget())
        .map_err(error)?
        .complete
    {}
    build.finish().map_err(error)
}

fn retain(frame: &RenderFrame) -> Result<RetainedScene> {
    let pool = r01_fixture::pool();
    let mut build = RetainBuild::new(frame, None, r01_fixture::limits(), &pool).map_err(error)?;
    while !build
        .step(r01_fixture::step_budget())
        .map_err(error)?
        .complete
    {}
    build.finish().map_err(error)
}

fn cropped_groups(renderer: &mut DeviceBackend, reference: bool) -> Result {
    // Finite overlapping children force an isolated source smaller than the
    // viewport. Repeated extents also exercise pooled physical target padding.
    let mut case = 0;
    let mut maximum = 0;
    let directory = super::directory().join("composite-color-v1");
    for offset in [0., 11., 23.] {
        for alpha in [0.1, 0.5] {
            let a = Rect::new(7. + offset, 9., 65. + offset, 37.);
            let b = Rect::new(32. + offset, 21., 113. + offset, 76.);
            let mut scene = Scene::new();
            fill(&mut scene, Rect::new(0., 0., 137., 93.), [0, 0, 0, 255]);
            scene.push_group(Group {
                composite: Composite {
                    alpha,
                    ..Default::default()
                },
                ..Default::default()
            });
            fill(&mut scene, a, [255; 4]);
            fill(&mut scene, b, [255; 4]);
            scene.pop_group();
            let pixels = capture(renderer, frame(scene, 10_000 + case, vec![])?, SIZE)?;
            let path = directory.join(format!("crop-{case}.rgba"));
            if reference {
                std::fs::write(path, &pixels)?;
            } else {
                let expected = std::fs::read(path)?;
                if expected.len() != pixels.len() {
                    return Err("cropped group reference size mismatch".into());
                }
                for (actual, expected) in pixels.iter().zip(expected) {
                    maximum = maximum.max(actual.abs_diff(expected));
                }
            }
            for y in 0..SIZE[1] {
                for x in 0..SIZE[0] {
                    let point = (f64::from(x) + 0.5, f64::from(y) + 0.5);
                    // The device reference covers every edge sample, including
                    // the existing linear-filter rounding at isolated boundaries.
                    // Uniform interiors and distant exterior pixels also have
                    // an independent analytic oracle.
                    let inside = a.inset(-1.).contains(point) || b.inset(-1.).contains(point);
                    let outside = !a.inset(1.).contains(point) && !b.inset(1.).contains(point);
                    let expected = if inside {
                        (alpha * 255.).round() as u8
                    } else {
                        0
                    };
                    let at = ((y * SIZE[0] + x) * 4) as usize;
                    let actual = &pixels[at..at + 4];
                    let tolerance = u8::from(inside);
                    if actual[3] != 255
                        || ((inside || outside)
                            && actual[..3].iter().any(|v| v.abs_diff(expected) > tolerance))
                    {
                        return Err(format!("cropped group case={case} pixel=({x},{y}) actual={actual:?} expected_rgb={expected}").into());
                    }
                }
            }
            case += 1;
        }
    }
    if maximum > 1 {
        return Err(format!("cropped group reference max_error={maximum} exceeds one byte").into());
    }
    log(format_args!(
        "COMPOSITE CROP cases={case} reference={reference} max_error={maximum} tolerance=1 interior_tolerance=1 exterior_error=0"
    ));
    Ok(())
}

pub(super) fn run(renderer: &mut DeviceBackend, reference: bool) -> Result {
    let directory = super::directory().join("composite-color-v1");
    if reference {
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("source.txt"),
            env!("REEL_DEVICE_CHECKS_REVISION"),
        )?;
    } else {
        log(format_args!(
            "COMPOSITE reference_source={}",
            std::fs::read_to_string(directory.join("source.txt"))?
        ));
    }
    let mut cases = 0u64;
    let mut changed = 0usize;
    let mut maximum = 0u8;
    let mut compare = |frame| -> Result {
        let actual = capture(renderer, frame, SIZE)?;
        let path = directory.join(format!("{cases}.rgba"));
        let mut local_max = 0;
        let mut local_changed = 0;
        if reference {
            std::fs::write(path, &actual)?;
        } else {
            let expected = std::fs::read(path)?;
            if expected.len() != actual.len() {
                return Err("composite reference size mismatch".into());
            }
            for (a, b) in actual.iter().zip(expected) {
                let distance = a.abs_diff(b);
                local_max = local_max.max(distance);
                local_changed += usize::from(distance != 0);
            }
        }
        log(format_args!(
            "COMPOSITE SAMPLE case={cases} changed={local_changed} max_error={local_max}"
        ));
        maximum = maximum.max(local_max);
        changed += local_changed;
        cases += 1;
        Ok(())
    };
    let mut id = 1;
    for clip in 0..5 {
        for alpha in [0.1, 0.5, 1.] {
            for screen in [false, true] {
                compare(frame(scene(clip, alpha, screen, false), id, vec![])?)?;
                id += 1;
            }
        }
    }
    let previous = retain(&frame(scene(2, 0.7, false, false), id, vec![])?)?;
    id += 1;
    let mask = PaintImageMask {
        program: MaskProgram::new(vec![MaskOp::Constant(0.25_f32.to_bits())], vec![])
            .map_err(error)?,
        images: std::sync::Arc::from([]),
        bounds: Rect::new(0., 0., 137., 93.),
        parameters: [0.5, 0.06, 0., 0., 0.],
    };
    for masked in [false, true] {
        for progress in [0., 0.1, 0.5, 0.9, 1.] {
            compare(frame(
                scene(4, 0.7, false, true),
                id,
                vec![FrameTransition {
                    root: None,
                    previous: previous.clone(),
                    progress,
                    mask: masked.then(|| mask.clone()),
                }],
            )?)?;
            id += 1;
        }
    }
    log(format_args!(
        "COMPOSITE COLOR reference={reference} cases={cases} pixels={} changed_bytes={changed} max_error={maximum} tolerance=1",
        cases * u64::from(SIZE[0] * SIZE[1])
    ));
    if maximum > 1 {
        return Err("composite output differs from device reference beyond one byte".into());
    }
    cropped_groups(renderer, reference)?;
    super::mask_color::run(renderer, reference)?;
    Ok(())
}
