//! Original ABI sample coordinates, logical-size halos and authored-domain costs.
use super::*;
use reel_ui::{core::Widget, frame::RecordLocation, kurbo::Rect, resources::ResourceHandle};

fn effect(width: f64, height: f64) -> FrameEffect {
    FrameEffect {
        definition: [0; 32],
        shader: ResourceHandle {
            registry: 1,
            slot: 1,
            generation: 1,
        },
        values: Vec::new(),
        images: Vec::new(),
        bounds: Rect::new(0., 0., width, height),
        coverage_invert: false,
    }
}
fn dot32(row: [f32; 4], p: [f32; 2], fused: bool) -> f32 {
    if fused {
        row[0].mul_add(p[0], row[1].mul_add(p[1], row[2]))
    } else {
        row[0] * p[0] + row[1] * p[1] + row[2]
    }
}

#[test]
fn linear_sample_neighbors_stay_inside_the_backward_region_on_the_original_grid() {
    let size = [512, 288];
    let effect = effect(128., 96.);
    let transform = Affine::translate((20., 30.)) * Affine::rotate(0.17) * Affine::scale(2.);
    let p = transform * reel_ui::kurbo::Point::new(64., 48.);
    let output = Scissor {
        x: p.x.floor() as u32 - 6,
        y: p.y.floor() as u32 - 4,
        width: 12,
        height: 8,
    };
    for storage in [size, [544, 320]] {
        let mut context = context(&effect, transform, [400, 300], size).unwrap();
        map_storage(&mut context, size, storage);
        for axis in 0..2 {
            let mut halo = [0; 4];
            halo[axis] = 30;
            halo[axis + 2] = 30;
            let region = required(
                &context,
                EffectSourceFootprint { logical_halo: halo },
                output,
                size,
            )
            .unwrap();
            assert!(region.width * region.height < size[0] * size[1]);
            for fused in [false, true] {
                for y in output.y..output.y + output.height {
                    for x in output.x..output.x + output.width {
                        let p = [x as f32 + 0.5, y as f32 + 0.5];
                        let uv = [dot32(context[0], p, fused), dot32(context[1], p, fused)];
                        for tap in -30..=30 {
                            let mut sampled = uv;
                            sampled[axis] += tap as f32 / context[4][axis + 2];
                            sampled = sampled.map(|v| v.clamp(0., 1.));
                            for a in 0..2 {
                                let h = 0.5 / context[5][a + 2];
                                let q = dot32(context[a + 2], sampled, fused).clamp(
                                    context[6][a] + h,
                                    context[6][a] + context[6][a + 2] - h,
                                );
                                let texel = q * storage[a] as f32 - 0.5;
                                for neighbor in [texel.floor() as i32, texel.floor() as i32 + 1] {
                                    if !(0..size[a] as i32).contains(&neighbor) {
                                        continue;
                                    }
                                    let lo = if a == 0 { region.x } else { region.y };
                                    let count = if a == 0 { region.width } else { region.height };
                                    assert!(
                                        (lo..lo + count).contains(&(neighbor as u32)),
                                        "axis={axis} tap={tap} sample={neighbor} region={region:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        // The same original Size remains in both shader languages; neither the
        // source raster extent nor this reduced scissor replaces logical Size.
        assert_eq!(context[4][2..], [128., 96.]);
        assert_eq!(context[5][2..], storage.map(|v| v as f32));
    }
}

#[test]
fn ill_conditioned_abi_coordinates_retain_the_full_source() {
    let effect = effect(128., 96.);
    let mut context = context(&effect, Affine::IDENTITY, [128, 96], [128, 96]).unwrap();
    context[0] = [1e8, -1e8, 0., 0.];
    assert!(
        required(
            &context,
            EffectSourceFootprint {
                logical_halo: [30, 0, 30, 0]
            },
            Scissor {
                x: 20,
                y: 20,
                width: 32,
                height: 32
            },
            [128, 96]
        )
        .is_none()
    );
}

#[test]
fn authored_large_rotating_box_chain_reports_real_domain_fragment_areas() {
    // MahoyoProbe assets.aside: im07l39 is 1464². components/layer.aside adds
    // maximum_blur(30)+2 on both sides, then H61/V61/H3/V3. These are the
    // rune_sweep.aside layer_02 authored transform poses, not device frame rates.
    let effect = effect(1528., 1528.);
    let owner = reel_ui::widgets::Label::new("").prepare().id();
    let at = RecordLocation {
        fragment: 0,
        command: 0,
        mask_depth: 0,
        owner,
    };
    // Production Canvas preserves aspect ratio and centers the 1024x576 canvas.
    let pixel_scale = Affine::translate((0., 2.)) * Affine::scale(960. / 1024.);
    // These tracks use as_one=quad_out over their original linear curve keys.
    for seconds in [0f64, 1.5, 3., 4.5, 6.] {
        let time = (1. - (1. - seconds / 6.).powi(2)) * 6.;
        let x = if time <= 3. {
            1437. + (588. - 1437.) * time / 3.
        } else {
            588. + (-542. - 588.) * (time - 3.) / 3.
        };
        let y = -53. + 29. * time / 6.;
        let degrees = 360. * time / 6.;
        let to_output = pixel_scale
            * Affine::translate((x, y))
            * Affine::scale_non_uniform(2., 0.65)
            * Affine::rotate(-degrees.to_radians())
            * Affine::translate((-764., -764.));
        let layout = source_layout(&effect, to_output, Affine::IDENTITY, 4096, at).unwrap();
        let local = layout.frame_to_source * to_output;
        let mut output = Scissor::full([960, 544]);
        let mut after = Vec::new();
        // Reverse demand: outer V3 -> H3 -> V61 -> H61 -> image source.
        for (index, halo) in [[0, 1, 0, 1], [1, 0, 1, 0], [0, 30, 0, 30], [30, 0, 30, 0]]
            .into_iter()
            .enumerate()
        {
            let output_size = if index == 0 { [960, 544] } else { layout.size };
            let transform = if index == 0 { to_output } else { local };
            let context = context(&effect, transform, output_size, layout.size).unwrap();
            output = required(
                &context,
                EffectSourceFootprint { logical_halo: halo },
                output,
                layout.size,
            )
            .unwrap();
            after.push(u64::from(output.width) * u64::from(output.height));
        }
        let full = u64::from(layout.size[0]) * u64::from(layout.size[1]);
        assert!(
            after[1] + after[2] < full * 2,
            "both 61tap outputs must save work"
        );
        eprintln!(
            "authored box t={seconds} pose=({x},{y},{degrees}) source={:?} H61_pixels={full}->{} V61_pixels={full}->{} image_pixels={full}->{}",
            layout.size, after[2], after[1], after[3]
        );
    }
}
