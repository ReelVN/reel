use super::*;
use crate::common::effect_plan::{context, map_storage};
use reel_ui::{
    frame::FrameEffect,
    kurbo::{Affine, Point, Rect},
    resources::ResourceHandle,
};
#[path = "analysis.rs"]
mod analysis;

fn abi(
    logical: [u32; 2],
    storage: [u32; 2],
    output: [u32; 2],
    transform: Affine,
) -> [[f32; 4]; crate::effect::CONTEXT_SLOTS] {
    let effect = FrameEffect {
        definition: [0; 32],
        shader: ResourceHandle {
            registry: 1,
            slot: 1,
            generation: 1,
        },
        values: Vec::new(),
        images: Vec::new(),
        bounds: Rect::new(0., 0., logical[0] as f64, logical[1] as f64),
        coverage_invert: false,
        resolution_scale: 1.,
    };
    let mut context = context(&effect, transform, output, logical).unwrap();
    map_storage(&mut context, logical, storage);
    context
}

fn dot(row: [f32; 4], p: [f32; 2], fused: bool) -> f32 {
    if fused {
        row[0].mul_add(p[0], row[1].mul_add(p[1], row[2]))
    } else {
        row[0] * p[0] + row[1] * p[1] + row[2]
    }
}

// Independent execution of the source helper, including both clamps and the
// actual physical repeat-addressed linear neighbors. No planning math is reused.
fn coordinate(
    context: &[[f32; 4]; crate::effect::CONTEXT_SLOTS],
    p: [f32; 2],
    axis: usize,
    tap: i32,
    fused: bool,
) -> Option<[f32; 2]> {
    let mut uv = [dot(context[0], p, fused), dot(context[1], p, fused)];
    if uv.into_iter().any(|n| !(0. ..=1.).contains(&n)) {
        return None;
    }
    uv[axis] += tap as f32 / context[4][axis + 2];
    uv = uv.map(|v| v.clamp(0., 1.));
    Some(std::array::from_fn(|a| {
        let half = 0.5 / context[5][a + 2];
        dot(context[a + 2], uv, fused).clamp(
            context[6][a] + half,
            context[6][a] + context[6][a + 2] - half,
        )
    }))
}

fn neighbors(q: [f32; 2], extent: [u32; 2]) -> [(u32, u32); 4] {
    let low = [0, 1].map(|a| (q[a] * extent[a] as f32 - 0.5).floor() as i32);
    std::array::from_fn(|n| {
        (
            (low[0] + (n % 2) as i32).rem_euclid(extent[0] as i32) as u32,
            (low[1] + (n / 2) as i32).rem_euclid(extent[1] as i32) as u32,
        )
    })
}

#[test]
fn row_union_is_bounded_disjoint_and_keeps_original_pixel_edges() {
    let bounds = Scissor {
        x: 7,
        y: 5,
        width: 159,
        height: 99,
    };
    let full = Cover::rectangle(bounds, [173, 111]).unwrap();
    assert_eq!(full.rectangles().collect::<Vec<_>>(), [bounds]);
    assert_eq!(full.tile_count(), 24);
    let mut cover = Cover::empty(bounds, [173, 111]).unwrap();
    cover.insert(Scissor {
        x: 32,
        y: 32,
        width: 64,
        height: 64,
    });
    cover.insert(Scissor {
        x: 64,
        y: 64,
        width: 64,
        height: 32,
    });
    assert_eq!(cover.tile_count(), 5);
    assert_eq!(cover.draw_count(), 2);
    let mut visits = [[0_u8; 173]; 111];
    for rectangle in cover.rectangles() {
        assert!(rectangle.tile_exact(EDGE, cover.extent));
        for y in rectangle.y..rectangle.y + rectangle.height {
            for x in rectangle.x..rectangle.x + rectangle.width {
                visits[y as usize][x as usize] += 1;
            }
        }
    }
    for y in 0..111 {
        for x in 0..173 {
            assert_eq!(visits[y][x], u8::from(cover.contains(x as u32, y as u32)));
        }
    }
    assert!(Cover::rectangle(bounds, [4097, 111]).is_none());
    assert!(
        Cover::rectangle(
            Scissor {
                x: u32::MAX,
                width: 2,
                ..bounds
            },
            [173, 111]
        )
        .is_none()
    );
    assert_eq!(
        Cover::rectangle(Scissor::full([0, 0]), [173, 111])
            .unwrap()
            .tile_count(),
        0
    );
}

#[test]
fn rotated_demand_removes_tiles_while_enclosing_every_sampled_neighbor() {
    let extent = [512, 512];
    let transform =
        Affine::translate((256., 256.)) * Affine::rotate(0.61) * Affine::translate((-256., -256.));
    let context = abi(extent, extent, extent, transform);
    let bounds = Scissor {
        x: 128,
        y: 128,
        width: 256,
        height: 256,
    };
    let consumer = Cover::rectangle(bounds, extent).unwrap();
    let footprint = EffectSourceFootprint {
        logical_halo: [30, 0, 30, 0],
    };
    let original = super::super::required(&context, footprint, bounds, extent).unwrap();
    let plan = backward(&context, footprint, &consumer, extent, original).unwrap();
    assert_eq!(plan.queries, consumer.tile_count());
    assert!(plan.cover.tile_count() < Cover::rectangle(original, extent).unwrap().tile_count());
    assert!(plan.cover.draw_count() <= ROWS);
    for fused in [false, true] {
        for y in (bounds.y..bounds.y + bounds.height).step_by(11) {
            for x in (bounds.x..bounds.x + bounds.width).step_by(13) {
                for tap in -30..=30 {
                    let q = coordinate(&context, [x as f32 + 0.5, y as f32 + 0.5], 0, tap, fused)
                        .unwrap();
                    for (sx, sy) in neighbors(q, extent) {
                        assert!(
                            plan.cover.contains(sx, sy),
                            "missing ({sx},{sy}) from ({x},{y}) tap={tap}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn padding_mirrors_and_physical_repeat_edges_keep_the_original_read_contract() {
    let logical = [509, 287];
    let storage = [544, 320];
    for transform in [
        Affine::IDENTITY,
        Affine::translate((509., 0.)) * Affine::scale_non_uniform(-1., 1.),
        Affine::translate((60., -20.)) * Affine::new([1., 0.13, -0.2, 1., 0., 0.]),
    ] {
        let context = abi(logical, storage, logical, transform);
        for bounds in [
            Scissor {
                x: 0,
                y: 0,
                width: 33,
                height: 35,
            },
            Scissor {
                x: 475,
                y: 250,
                width: 34,
                height: 37,
            },
        ] {
            let consumer = Cover::rectangle(bounds, logical).unwrap();
            for axis in 0..2 {
                let mut halo = [0; 4];
                halo[axis] = 30;
                halo[axis + 2] = 30;
                let plan = backward(
                    &context,
                    EffectSourceFootprint { logical_halo: halo },
                    &consumer,
                    storage,
                    Scissor::full(logical),
                )
                .unwrap();
                for fused in [false, true] {
                    for y in (bounds.y..bounds.y + bounds.height).step_by(3) {
                        for x in (bounds.x..bounds.x + bounds.width).step_by(3) {
                            for tap in -30..=30 {
                                let Some(q) = coordinate(
                                    &context,
                                    [x as f32 + 0.5, y as f32 + 0.5],
                                    axis,
                                    tap,
                                    fused,
                                ) else {
                                    continue;
                                };
                                for (sx, sy) in neighbors(q, storage) {
                                    // Existing initialization still owns physical padding.
                                    if sx < logical[0] && sy < logical[1] {
                                        assert!(plan.cover.contains(sx, sy));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn sample(bytes: &[u8], q: [f32; 2], extent: [u32; 2]) -> f32 {
    let p = [0, 1].map(|a| q[a] * extent[a] as f32 - 0.5);
    let t = p.map(|p| p - p.floor());
    let [a, b, c, d] =
        neighbors(q, extent).map(|(x, y)| bytes[(y * extent[0] + x) as usize] as f32 / 255.);
    (a * (1. - t[0]) + b * t[0]) * (1. - t[1]) + (c * (1. - t[0]) + d * t[0]) * t[1]
}

fn mean(
    bytes: &[u8],
    context: &[[f32; 4]; crate::effect::CONTEXT_SLOTS],
    extent: [u32; 2],
    p: [f32; 2],
    axis: usize,
) -> u8 {
    let mut sum = 0_f32;
    for tap in -30..=30 {
        sum += sample(
            bytes,
            coordinate(context, p, axis, tap, false).unwrap(),
            extent,
        );
    }
    ((sum / 61.).clamp(0., 1.) * 255. + 0.5).floor() as u8
}

#[test]
fn two_large_kernel_stages_consume_identical_pixels_with_poison_in_unneeded_tiles() {
    let logical = [192, 192];
    let extent = [224, 224];
    let output = [256, 256];
    let h = abi(logical, extent, logical, Affine::IDENTITY);
    let transform =
        Affine::translate((128., 128.)) * Affine::rotate(0.47) * Affine::translate((-96., -96.));
    let v = abi(logical, extent, output, transform);
    let bounds = Scissor {
        x: 108,
        y: 116,
        width: 17,
        height: 13,
    };
    let consumer = Cover::rectangle(bounds, output).unwrap();
    let needed_h = backward(
        &v,
        EffectSourceFootprint {
            logical_halo: [0, 30, 0, 30],
        },
        &consumer,
        extent,
        Scissor::full(logical),
    )
    .unwrap();
    let needed_raw = backward(
        &h,
        EffectSourceFootprint {
            logical_halo: [30, 0, 30, 0],
        },
        &needed_h.cover,
        extent,
        Scissor::full(logical),
    )
    .unwrap();
    assert_eq!(needed_raw.queries as usize, needed_h.cover.draw_count());
    assert!(
        needed_h.cover.tile_count()
            < Cover::rectangle(Scissor::full(logical), extent)
                .unwrap()
                .tile_count()
    );
    let mut raw = vec![0_u8; (extent[0] * extent[1]) as usize];
    for y in 0..logical[1] {
        for x in 0..logical[0] {
            raw[(y * extent[0] + x) as usize] = ((x * 13 + y * 37 + x * y) % 256) as u8;
        }
    }
    let mut restricted_raw = raw.clone();
    for y in 0..logical[1] {
        for x in 0..logical[0] {
            if !needed_raw.cover.contains(x, y) {
                restricted_raw[(y * extent[0] + x) as usize] = 251;
            }
        }
    }
    let mut full_h = vec![0; raw.len()];
    let mut partial_h = full_h.clone();
    for y in 0..logical[1] {
        for x in 0..logical[0] {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let index = (y * extent[0] + x) as usize;
            full_h[index] = mean(&raw, &h, extent, p, 0);
            partial_h[index] = if needed_h.cover.contains(x, y) {
                mean(&restricted_raw, &h, extent, p, 0)
            } else {
                239
            };
        }
    }
    for y in bounds.y..bounds.y + bounds.height {
        for x in bounds.x..bounds.x + bounds.width {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            assert_eq!(
                mean(&full_h, &v, extent, p, 1),
                mean(&partial_h, &v, extent, p, 1)
            );
        }
    }
    // Reusing the backing with a different consumer does not inherit old rows.
    let next = Cover::rectangle(
        Scissor {
            x: 4,
            y: 8,
            width: 16,
            height: 16,
        },
        output,
    )
    .unwrap();
    assert_ne!(next.bounds(), consumer.bounds());
    assert!(!next.contains(110, 120));
    assert_eq!(next.tile_count(), 1);
    assert!(transform * Point::new(96., 96.) == Point::new(128., 128.));
}

/// Read the ordinary fixed-frame retained archive after capture, without a GPU.
/// Keep this executable with an archive when its descriptor serialization evolves.
#[test]
#[ignore = "requires REEL_ROI_ARCHIVE from the formal fixed-frame capture"]
fn saved_frame_tile_demand() {
    use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
    use reel_ui::{
        frame::{BuildLimits, FrameOp, SavedRetainedScene, StepBudget},
        resources::ResourceView,
    };
    use std::io::Write;
    let input =
        std::path::PathBuf::from(std::env::var_os("REEL_ROI_ARCHIVE").expect("REEL_ROI_ARCHIVE"));
    let bytes = std::fs::read(&input).unwrap();
    let (version, logical, physical, saved): (u32, [f64; 2], [u32; 2], SavedRetainedScene) =
        postcard::from_bytes(&bytes).unwrap();
    assert_eq!(version, 1);
    assert_eq!(
        saved.assets().count(),
        0,
        "the analysis archive keeps its immutable inline resources"
    );
    let pool = BudgetPool::new(
        BudgetCategory::Snapshot,
        QueueLimits {
            max_requests: 16384,
            max_bytes: 256 << 20,
        },
    );
    let mut restore = saved
        .restore_begin(
            BuildLimits {
                max_bytes: 256 << 20,
            },
            &pool,
        )
        .unwrap();
    let budget = StepBudget {
        max_commands: 4096,
        max_units: 1 << 20,
        max_bytes: 128 << 20,
    };
    for n in 0..10000 {
        let progress = restore.step(budget).unwrap();
        assert!(progress.missing_asset.is_none());
        if progress.build.complete {
            break;
        }
        assert!(n < 9999, "archive restore did not complete");
    }
    let restored = restore.finish().unwrap();
    let drawing = restored.drawing();
    let output = std::env::var_os("REEL_ROI_REPORT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| input.with_extension("roi.tsv"));
    let mut report = std::io::BufWriter::new(std::fs::File::create(&output).unwrap());
    writeln!(report, "frame\tversion={version}\tlogical={logical:?}\tphysical={physical:?}\tcommands={}\tresources={}", drawing.commands().len(), drawing.resources().leases().len()).unwrap();
    let scale = Affine::scale_non_uniform(
        physical[0] as f64 / logical[0],
        physical[1] as f64 / logical[1],
    );
    let mut depth = 0;
    for (index, command) in drawing.commands().iter().enumerate() {
        match &command.op {
            FrameOp::PushGroup(group) => {
                if let Some(effect) = group.effect.as_ref().or(group.mask_effect.as_ref()) {
                    let id: String = effect
                        .definition
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    let layout = crate::common::effect_plan::source_layout(
                        effect,
                        group.effect_to_target,
                        scale,
                        4096,
                        command.origin.at,
                    )
                    .unwrap();
                    let descriptor = drawing
                        .resources()
                        .leases()
                        .iter()
                        .find_map(|lease| {
                            if lease.handle() != effect.shader {
                                return None;
                            }
                            match lease.view() {
                                ResourceView::EffectShader { descriptor, .. } => Some(descriptor),
                                _ => None,
                            }
                        })
                        .unwrap();
                    writeln!(report, "effect\tpush={index}\tdepth={depth}\towner={}\tdefinition={id}\tbounds={:?}\tto_frame={:?}\traster={:?}\tto_source={:?}\tfootprint={:?}\tblend={:?}",
                        group.owner.to_raw(), effect.bounds, group.effect_to_target.as_coeffs(), layout.size, layout.frame_to_source.as_coeffs(), descriptor.source_footprint, group.composite).unwrap();
                } else {
                    writeln!(report, "group\tpush={index}\tdepth={depth}\towner={}\tto_frame={:?}\tink={:?}\tblend={:?}\tclip={:?}", group.owner.to_raw(), group.effect_to_target.as_coeffs(), group.ink_bounds, group.composite, group.clip).unwrap();
                }
                depth += 1;
            }
            FrameOp::PopGroup => {
                depth -= 1;
                writeln!(report, "pop\tindex={index}\tdepth={depth}").unwrap();
            }
            other => {
                writeln!(
                    report,
                    "command\tindex={index}\tdepth={depth}\top={other:?}"
                )
                .unwrap();
            }
        }
    }
    report.flush().unwrap();
    eprintln!("ROI archive metadata: {}", output.display());
}
