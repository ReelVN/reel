//! Compare smaller intermediate backing with the unchanged full logical viewport.
use super::{Blend, FragmentOutput, Result, Target, TextureFilter, log, native};

fn program(context: &native::Context) -> Result<native::Program> {
    Ok(context.create_program(native::ProgramDescriptor {
        output: FragmentOutput::Patched,
        vertex: native::ShaderCode::GxpBinary(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/present-color-vertex.gxp"
        ))),
        fragment: native::ShaderCode::GxpBinary(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/present-color-region.gxp"
        ))),
        bindings: &[
            native::AttributeBinding {
                location: 0,
                name: "a_position",
            },
            native::AttributeBinding {
                location: 1,
                name: "a_uv",
            },
        ],
        stride: 4,
        attributes: &super::attributes(),
        instance: None,
        clip_uniform: Some("reel_clip_rect"),
    })?)
}

#[expect(
    clippy::too_many_arguments,
    reason = "one original viewport and its cropped source view"
)]
fn stage(
    context: &native::Context,
    target: &native::Framebuffer,
    source: &native::Texture,
    program: &native::Program,
    vertices: &native::VertexBuffer,
    logical: [u32; 2],
    demand: [u32; 2],
    scale: [f32; 2],
    filter: TextureFilter,
) -> Result {
    context.clear(Target::Framebuffer(target), [0.; 4])?;
    draw_region(
        context,
        target,
        source,
        program,
        vertices,
        logical,
        [0, 0, demand[0], demand[1]],
        scale,
        filter,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one unchanged raster, texture and sampled rectangle"
)]
fn draw_region(
    context: &native::Context,
    target: &native::Framebuffer,
    source: &native::Texture,
    program: &native::Program,
    vertices: &native::VertexBuffer,
    logical: [u32; 2],
    region: [u32; 4],
    scale: [f32; 2],
    filter: TextureFilter,
) -> Result {
    let image = program.uniform("u_image").ok_or("missing sampler")?;
    let mapping = program.uniform("u_scale").ok_or("missing source scale")?;
    context.draw_filtered(
        native::Draw {
            target: Target::Framebuffer(target),
            size: logical,
            program,
            vertices,
            instances: None,
            indices: None,
            range: 0..3,
            textures: &[source],
            uniforms: &[
                native::UniformValue::Int(&image, 0),
                native::UniformValue::Vec2(&mapping, scale),
            ],
            blend: Blend::Replace,
            scissor: Some(region),
        },
        filter,
    )?;
    Ok(())
}

fn write_regions(
    context: &native::Context,
    program: &native::Program,
    source: &native::Texture,
    producer: &native::VertexBuffer,
) -> Result {
    let mut cases = 0;
    for (logical, storage) in [([181, 117], [193, 131]), ([237, 177], [257, 193])] {
        let framebuffer = |size| {
            let texture = context.create_target_texture_with_storage(
                size,
                native::TargetFormat::Rgba8,
                native::TargetStorage::Linear,
            )?;
            context.framebuffer(&texture)
        };
        let full = framebuffer(storage)?;
        let reused = framebuffer(storage)?;
        let expected = framebuffer(logical)?;
        let actual = framebuffer(logical)?;
        // Fractional taps at the logical right/bottom edge read transparent
        // physical padding. Both paths retain exactly these uploaded vertices.
        let [du, dv] = [0.65 / logical[0] as f32, 0.35 / logical[1] as f32];
        let consumer = context.vertex_buffer(
            &[-1., 1., du, dv, 3., 1., 2. + du, dv, -1., -3., du, 2. + dv],
            4,
            &super::attributes(),
        )?;
        let edge = [logical[0] - 29, logical[1] - 21, 29, 21];
        let edge_origin = [(edge[0] - 2) / 32 * 32, (edge[1] - 2) / 32 * 32];
        for filter in [TextureFilter::Nearest, TextureFilter::Linear] {
            for (draw, initialize) in [
                ([65, 49, 40, 28], [64, 32, 64, 64]),
                (
                    edge,
                    [
                        edge_origin[0],
                        edge_origin[1],
                        storage[0] - edge_origin[0],
                        storage[1] - edge_origin[1],
                    ],
                ),
            ] {
                // Alternate dirty complete writes and two different windows on
                // the same owner; a window must not leak into its next use.
                for empty in [false, true] {
                    context.clear(Target::Framebuffer(&reused), [0.875, 0.625, 0.375, 0.75])?;
                    context.clear(Target::Framebuffer(&full), [0.; 4])?;
                    let window = reused.with_write_region(Some(initialize))?;
                    context.clear(Target::Framebuffer(&window), [0.; 4])?;
                    if !empty {
                        for target in [&full, &window] {
                            draw_region(
                                context, target, source, program, producer, logical, draw, [1.; 2],
                                filter,
                            )?;
                        }
                    }
                    let scale = [0, 1].map(|axis| logical[axis] as f32 / storage[axis] as f32);
                    for (target, input) in
                        [(&expected, full.texture()), (&actual, window.texture())]
                    {
                        context.clear(Target::Framebuffer(target), [0.; 4])?;
                        draw_region(
                            context, target, input, program, &consumer, logical, draw, scale,
                            filter,
                        )?;
                    }
                    context.finish()?;
                    let reference = read_region(context, &expected, logical)?;
                    let pixels = read_region(context, &actual, logical)?;
                    let mut changed = 0;
                    let mut maximum = 0;
                    let mut visible = false;
                    for y in draw[1]..draw[1] + draw[3] {
                        for x in draw[0]..draw[0] + draw[2] {
                            let index = ((y * logical[0] + x) * 4) as usize;
                            visible |= reference[index + 3] != 0;
                            for channel in 0..4 {
                                let error =
                                    reference[index + channel].abs_diff(pixels[index + channel]);
                                changed += usize::from(error != 0);
                                maximum = maximum.max(error);
                            }
                        }
                    }
                    log(format_args!(
                        "EFFECT WRITE REGION case={cases} logical={logical:?} storage={storage:?} draw={draw:?} initialize={initialize:?} filter={filter:?} empty={empty} changed_bytes={changed} max_error={maximum} tolerance=0"
                    ));
                    if changed != 0 || visible == empty {
                        return Err(
                            "per-use source initialization differs at consumed pixels".into()
                        );
                    }
                    cases += 1;
                }
            }
        }
    }
    log(format_args!("EFFECT WRITE REGION PASS cases={cases}"));
    Ok(())
}

fn read_region(
    context: &native::Context,
    target: &native::Framebuffer,
    region: [u32; 2],
) -> Result<Vec<u8>> {
    let size = target.size();
    let mut full = vec![0; (size[0] * size[1] * 4) as usize];
    context.read_rgba(target, 0, &mut full)?;
    let mut cropped = Vec::with_capacity((region[0] * region[1] * 4) as usize);
    for row in full
        .chunks_exact(size[0] as usize * 4)
        .take(region[1] as usize)
    {
        cropped.extend_from_slice(&row[..region[0] as usize * 4]);
    }
    Ok(cropped)
}

fn reshaped_storage(
    context: &native::Context,
    program: &native::Program,
    source: &native::Texture,
    vertices: &native::VertexBuffer,
) -> Result {
    let framebuffer = |size| {
        let texture = context.create_target_texture_with_storage(
            size,
            native::TargetFormat::Rgba8,
            native::TargetStorage::Linear,
        )?;
        context.framebuffer(&texture)
    };
    let old = framebuffer([32, 16])?;
    let reader = framebuffer([32, 16])?;
    let expected = framebuffer([16, 32])?;
    for (reshape, filter) in [false, true].into_iter().flat_map(|reshape| {
        [TextureFilter::Nearest, TextureFilter::Linear].map(|filter| (reshape, filter))
    }) {
        log(format_args!(
            "EFFECT RESHAPE phase=start reshape={reshape} filter={filter:?}"
        ));
        context.clear(Target::Framebuffer(&old), [1., 0., 0., 1.])?;
        stage(
            context,
            &reader,
            old.texture(),
            program,
            vertices,
            [32, 16],
            [32, 16],
            [1.; 2],
            filter,
        )?;
        // Keep the previous descriptor and its queued reader alive while the
        // new immutable descriptor changes row stride over the same allocation.
        let before = context.memory_usage()?.pixels;
        let view = if reshape {
            context.reshape_target_texture(old.texture(), [16, 32])?
        } else {
            context.create_target_texture_with_storage(
                [16, 32],
                native::TargetFormat::Rgba8,
                native::TargetStorage::Linear,
            )?
        };
        let actual = context.framebuffer(&view)?;
        let after = context.memory_usage()?.pixels;
        if old.size() != [32, 16]
            || view.size() != [16, 32]
            || view.allocated_bytes() != old.texture().allocated_bytes()
            || reshape
                && (before.used_bytes != after.used_bytes
                    || before.block_allocations != after.block_allocations
                    || context.supports_texture_copy(old.texture(), [0; 2], &view, [0; 2], [16; 2]))
        {
            return Err("reshape changed allocation ownership or the old descriptor".into());
        }
        if reshape
            && draw_region(
                context,
                &actual,
                old.texture(),
                program,
                vertices,
                [16, 32],
                [0, 0, 16, 32],
                [1.; 2],
                filter,
            )
            .is_ok()
        {
            return Err("reshaped aliases allowed same-scene texture feedback".into());
        }
        for target in [&actual, &expected] {
            stage(
                context,
                target,
                source,
                program,
                vertices,
                [16, 32],
                [16, 32],
                [1.; 2],
                filter,
            )?;
        }
        context.finish()?;
        let prior = read_region(context, &reader, [32, 16])?;
        let reference = read_region(context, &expected, [16, 32])?;
        let pixels = read_region(context, &actual, [16, 32])?;
        let changed = reference
            .iter()
            .zip(&pixels)
            .filter(|(a, b)| a != b)
            .count();
        let prior_changed = prior
            .chunks_exact(4)
            .filter(|pixel| *pixel != [255, 0, 0, 255])
            .count();
        log(format_args!(
            "EFFECT RESHAPE reshape={reshape} filter={filter:?} changed_bytes={changed} prior_changed_pixels={prior_changed} allocation_bytes={} tolerance=0",
            view.allocated_bytes(),
        ));
        if changed != 0 || prior_changed != 0 {
            return Err("reshaped storage changed pixels or a previously queued reader".into());
        }
    }
    log("EFFECT RESHAPE PASS cases=4");
    Ok(())
}

pub(super) fn run(context: &native::Context) -> Result {
    log("EFFECT REGION setup");
    context.finish()?;
    let program = program(context)?;
    let source_size = [29, 19];
    let source = context.create_target_texture(source_size, native::TargetFormat::Rgba8)?;
    let pixels: Vec<_> = (0..source_size[0] * source_size[1])
        .flat_map(|index| {
            let alpha = (31 + index % 225) as u8;
            [
                (index * 17 % u32::from(alpha)) as u8,
                (index * 37 % u32::from(alpha)) as u8,
                (index * 71 % u32::from(alpha)) as u8,
                alpha,
            ]
        })
        .collect();
    context.upload(
        &source,
        [0; 2],
        source_size,
        &pixels,
        source_size[0] as usize * 4,
    )?;
    let vertices = context.vertex_buffer(
        &[-1., 1., 0., 0., 3., 1., 2., 0., -1., -3., 0., 2.],
        4,
        &super::attributes(),
    )?;
    let mut cases = 0;
    for (logical, backing) in [
        ([1920, 400], [960, 200]),
        ([2048, 1024], [512, 256]),
        ([2052, 796], [513, 199]),
    ] {
        let cropped = (0..4)
            .map(|_| {
                let texture =
                    context.create_target_texture(backing, native::TargetFormat::Rgba8)?;
                context.framebuffer(&texture)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let ordinary = (0..4)
            .map(|_| {
                let texture =
                    context.create_target_texture(logical, native::TargetFormat::Rgba8)?;
                context.framebuffer(&texture)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for filter in [TextureFilter::Nearest, TextureFilter::Linear] {
            log(format_args!(
                "EFFECT REGION case={cases} phase=ordinary logical={logical:?} backing={backing:?} filter={filter:?}"
            ));
            context.finish()?;
            let start = context.memory_usage()?.scene_submissions;
            for index in 0..4 {
                let input = if index == 0 {
                    &source
                } else {
                    ordinary[index - 1].texture()
                };
                stage(
                    context,
                    &ordinary[index],
                    input,
                    &program,
                    &vertices,
                    logical,
                    backing.map(|size| size - index as u32 * 2),
                    [1.; 2],
                    filter,
                )?;
            }
            context.finish()?;
            let ordinary_scenes = context.memory_usage()?.scene_submissions - start;
            let expected = ordinary
                .iter()
                .map(|target| read_region(context, target, backing))
                .collect::<Result<Vec<_>>>()?;
            if !expected[0]
                .chunks_exact(4)
                .any(|pixel| pixel[3] > 0 && pixel[3] < 255)
            {
                return Err("ordinary viewport did not render the translucent source".into());
            }
            log(format_args!("EFFECT REGION case={cases} phase=cropped"));
            let start = context.memory_usage()?.scene_submissions;
            for index in 0..4 {
                let input = if index == 0 {
                    &source
                } else {
                    cropped[index - 1].texture()
                };
                let scale = if index == 0 {
                    [1.; 2]
                } else {
                    [0, 1].map(|axis| (logical[axis] / backing[axis]) as f32)
                };
                stage(
                    context,
                    &cropped[index],
                    input,
                    &program,
                    &vertices,
                    logical,
                    backing.map(|size| size - index as u32 * 2),
                    scale,
                    filter,
                )?;
            }
            let early_scenes = context.memory_usage()?.scene_submissions - start;
            context.finish()?;
            let cropped_scenes = context.memory_usage()?.scene_submissions - start;
            let mut changed = 0;
            let mut maximum = 0;
            for (index, reference) in expected.iter().enumerate() {
                let actual = read_region(context, &cropped[index], backing)?;
                for (&a, &b) in reference.iter().zip(&actual) {
                    let error = a.abs_diff(b);
                    changed += usize::from(error != 0);
                    maximum = maximum.max(error);
                }
            }
            log(format_args!(
                "EFFECT REGION case={cases} logical={logical:?} backing={backing:?} filter={filter:?} changed_bytes={changed} max_error={maximum} ordinary_scenes={ordinary_scenes} cropped_scenes={cropped_scenes} early_scenes={early_scenes} tolerance=0"
            ));
            if changed != 0 || ordinary_scenes != 4 || cropped_scenes != 4 {
                return Err(
                    "cropped feedback differs from the original viewport or scene order".into(),
                );
            }
            cases += 1;
        }
        context.finish()?;
    }
    log(format_args!("EFFECT REGION PASS cases={cases}"));
    write_regions(context, &program, &source, &vertices)?;
    reshaped_storage(context, &program, &source, &vertices)?;
    Ok(())
}
